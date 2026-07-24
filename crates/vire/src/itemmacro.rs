//! Hygienic **item macros** — `macro name(P: type, n: ident, e: expr) { <items> }`
//! invoked as `name!(args)`, expanding to declarations.
//!
//! Phase 3c of the compile-time programming layer (see TODO.md). Designed so the
//! classic C-preprocessor hazards cannot occur:
//!   * **AST-level, never textual** — arguments are AST nodes substituted into AST
//!     positions; there is no token pasting or re-lexing.
//!   * **Kind-checked parameters** — every parameter declares whether it is a
//!     `type`, an `ident`, or an `expr`, and the expander rejects an argument of
//!     the wrong kind (you cannot splice an expression where a type belongs).
//!   * **Hygiene** — a binding introduced *inside* the macro body is gensym-renamed
//!     per expansion, so it can never capture or be captured by the call site.
//!   * **Type-checked after expansion** — expansion runs before inference, so the
//!     generated `fn`/`type` goes through the full checker like hand-written code.
//!
//! Deliberately limited: arguments for `type`/`ident` parameters must be bare
//! names (no `List[Int]` type arguments yet); nested item-macro invocations inside
//! a macro body are not re-expanded; generated top-level names are the caller's
//! responsibility (a collision is caught downstream, never silently merged).

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::expand::{collect_binders_block, subst_block};
use crate::infer::expr_span;

const S: Span = Span(0, 0);
/// Upper bound on fixpoint rounds (nested/recursive item-macro expansion).
const ROUND_LIMIT: u32 = 64;

/// Expand every `name!(...)` invocation into the macro's substituted items and
/// drop the macro definitions. Returns diagnostics (unknown macro, arity, kind
/// mismatch) carrying a source span — kind errors point at the offending
/// argument, the rest at the invocation site.
pub fn expand_item_macros(m: &mut Module) -> Vec<Diag> {
    // Nothing to do unless the module defines or invokes an item macro.
    if !m.items.iter().any(|it| matches!(it, Item::ItemMacro { .. } | Item::MacroInvoke { .. })) {
        return Vec::new();
    }
    let mut defs: HashMap<String, (Vec<MacroParam>, Vec<Item>)> = HashMap::new();
    for it in &m.items {
        if let Item::ItemMacro { name, params, items, .. } = it {
            defs.insert(name.clone(), (params.clone(), items.clone()));
        }
    }

    let mut errs = Vec::new();
    let had_defs = !defs.is_empty();
    let mut counter: u32 = 0;

    // Fixpoint: a macro body may itself invoke item macros, so re-expand until no
    // invocation remains. Each round expands one level; a round that does not
    // reduce the invocation count (or a hard round cap) breaks a diverging macro.
    let mut round = 0;
    loop {
        if !m.items.iter().any(|it| matches!(it, Item::MacroInvoke { .. })) {
            break;
        }
        round += 1;
        if round > ROUND_LIMIT {
            errs.push(Diag::error("item macro expansion: recursion limit reached (diverging macro?)", S));
            m.items.retain(|it| !matches!(it, Item::MacroInvoke { .. }));
            break;
        }
        let old = std::mem::take(&mut m.items);
        let mut out: Vec<Item> = Vec::with_capacity(old.len());
        for it in old {
            match it {
                Item::ItemMacro { .. } => {} // drop the definition
                Item::MacroInvoke { name, args, span } => match defs.get(&name) {
                    None => errs.push(Diag::error(&format!("unknown item macro `{name}!`"), span)),
                    Some((params, items)) => match expand_one(&name, params, items, &args, span, &mut counter) {
                        Ok(mut gen) => out.append(&mut gen),
                        Err(e) => errs.push(e),
                    },
                },
                other => out.push(other),
            }
        }
        m.items = out;
    }

    // Safety net: item macros make top-level name collisions easy (two invocations,
    // or the no-token-pasting limitation). Report duplicate `fn`/`type` names with a
    // clear front-end diagnostic instead of a late, cryptic LLVM "invalid
    // redefinition". (Only when macros actually defined items.)
    if !had_defs {
        return errs;
    }
    // Report at one representative occurrence's span so the duplicate is clickable.
    let mut fn_names: HashMap<String, (u32, Span)> = HashMap::new();
    let mut ty_names: HashMap<String, (u32, Span)> = HashMap::new();
    for it in &m.items {
        match it {
            Item::Fn(f) => {
                let e = fn_names.entry(f.sig.name.clone()).or_insert((0, f.sig.span));
                e.0 += 1;
            }
            Item::Type(t) => {
                let e = ty_names.entry(t.name.clone()).or_insert((0, t.span));
                e.0 += 1;
            }
            _ => {}
        }
    }
    let mut dups: Vec<(String, Span)> = fn_names
        .into_iter()
        .filter(|(_, (n, _))| *n > 1)
        .map(|(name, (n, sp))| (format!("duplicate function `{name}` defined {n} times (item-macro name collision — names need to be distinct)"), sp))
        .chain(
            ty_names
                .into_iter()
                .filter(|(_, (n, _))| *n > 1)
                .map(|(name, (n, sp))| (format!("duplicate type `{name}` defined {n} times (item-macro name collision — names need to be distinct)"), sp)),
        )
        .collect();
    dups.sort();
    errs.extend(dups.into_iter().map(|(msg, sp)| Diag::error(&msg, sp)));
    errs
}

fn expand_one(name: &str, params: &[MacroParam], items: &[Item], args: &[Expr], inv_span: Span, counter: &mut u32) -> Result<Vec<Item>, Diag> {
    if params.len() != args.len() {
        return Err(Diag::error(&format!("item macro `{name}!`: expected {} argument(s), {} given", params.len(), args.len()), inv_span));
    }

    // Kind-check each argument against its parameter's declared kind, building the
    // substitution maps. This is the safety gate: a wrong-kind argument is a hard
    // error, never a blind splice.
    let mut tmap: HashMap<String, Type> = HashMap::new(); // type param → Type
    let mut imap: HashMap<String, String> = HashMap::new(); // ident param → name
    let mut pmap: HashMap<String, Expr> = HashMap::new(); // expr subst (idents + exprs + blocks)
    let mut patmap: HashMap<String, Pattern> = HashMap::new(); // pat param → Pattern
    for (p, a) in params.iter().zip(args) {
        match p.kind {
            ParamKind::Type => match expr_to_type(a) {
                Some(ty) => {
                    // Also expose the type argument to nested invocations (as the
                    // original expression) via the expression map.
                    pmap.insert(p.name.clone(), a.clone());
                    tmap.insert(p.name.clone(), ty);
                }
                None => return Err(Diag::error(&format!("item macro `{name}!`: argument for `{}: type` must be a type (name or `T[Arg]`)", p.name), expr_span(a))),
            },
            ParamKind::Ident => match a {
                Expr::Ident(n, sp) => {
                    imap.insert(p.name.clone(), n.clone());
                    // Carry the argument's real span so a later type error on the
                    // substituted name points back at the call site, not 0:0.
                    pmap.insert(p.name.clone(), Expr::Ident(n.clone(), *sp));
                }
                _ => return Err(Diag::error(&format!("item macro `{name}!`: argument for `{}: ident` must be an identifier", p.name), expr_span(a))),
            },
            // A block argument must be spelled with braces — kind-checked so it
            // cannot silently accept a bare expression. Spliced like an expr.
            ParamKind::Block => match a {
                Expr::Block(_) => {
                    pmap.insert(p.name.clone(), a.clone());
                }
                _ => return Err(Diag::error(&format!("item macro `{name}!`: argument for `{}: block` must be a `{{ … }}` block", p.name), expr_span(a))),
            },
            // A pattern argument is spliced into pattern positions (`match`/`for`).
            ParamKind::Pat => match expr_to_pattern(a) {
                Some(pat) => {
                    patmap.insert(p.name.clone(), pat);
                }
                None => return Err(Diag::error(&format!("item macro `{name}!`: argument for `{}: pat` must be a pattern (`_`, a literal, a binding, or `Ctor(...)`)", p.name), expr_span(a))),
            },
            ParamKind::Expr => {
                pmap.insert(p.name.clone(), a.clone());
            }
        }
    }

    let id = *counter;
    *counter += 1;
    let mut gen = Vec::with_capacity(items.len());
    for it in items {
        let mut c = it.clone();
        subst_item(&mut c, &tmap, &imap, &pmap, &patmap, id);
        gen.push(c);
    }
    Ok(gen)
}

/// Reinterpret an expression argument as a pattern: `_`, an int/str/bool literal,
/// a binding (lowercase ident), a nullary/`Ctor(args)` constructor, a dotted
/// `Type.Variant`, or a tuple. Returns `None` for anything not a valid pattern.
fn expr_to_pattern(e: &Expr) -> Option<Pattern> {
    match e {
        Expr::Ident(n, sp) if n == "_" => Some(Pattern::Wildcard(*sp)),
        Expr::Ident(n, sp) => {
            if n.contains('.') || n.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                Some(Pattern::Ctor { name: n.clone(), args: vec![], span: *sp })
            } else {
                Some(Pattern::Bind(n.clone(), *sp))
            }
        }
        Expr::Int(v, sp) => Some(Pattern::Int(*v, *sp)),
        Expr::Str(s, sp) => Some(Pattern::Str(s.clone(), *sp)),
        Expr::Bool(b, sp) => Some(Pattern::Bool(*b, *sp)),
        // `Type.Variant` (no fields) parses as a field access.
        Expr::Field { base, name, span } => {
            let base = expr_to_pattern(base)?;
            let bn = match base {
                Pattern::Ctor { name, .. } | Pattern::Bind(name, _) => name,
                _ => return None,
            };
            Some(Pattern::Ctor { name: format!("{bn}.{name}"), args: vec![], span: *span })
        }
        // `Ctor(p, …)` or `Type.Variant(p, …)`.
        Expr::Call { callee, args, span } => {
            let cn = match callee.as_ref() {
                Expr::Ident(n, _) => n.clone(),
                Expr::Field { .. } => match expr_to_pattern(callee)? {
                    Pattern::Ctor { name, .. } => name,
                    _ => return None,
                },
                _ => return None,
            };
            let pargs = args.iter().map(expr_to_pattern).collect::<Option<Vec<_>>>()?;
            Some(Pattern::Ctor { name: cn, args: pargs, span: *span })
        }
        _ => None,
    }
}

/// Reinterpret an expression argument as a type: a bare name `Foo`, or a single-
/// argument generic application `Foo[Arg]` (which parses as an index). Returns
/// `None` for anything that is not a valid type spelling.
fn expr_to_type(e: &Expr) -> Option<Type> {
    match e {
        Expr::Ident(n, sp) => Some(Type { name: n.clone(), args: vec![], borrowed: false, span: *sp }),
        Expr::Index { base, index, span } => {
            let base = expr_to_type(base)?;
            let arg = expr_to_type(index)?;
            Some(Type { name: base.name, args: vec![arg], borrowed: false, span: *span })
        }
        _ => None,
    }
}

// --- Substitution over declarations ------------------------------------------

fn subst_item(it: &mut Item, tmap: &HashMap<String, Type>, imap: &HashMap<String, String>, pmap: &HashMap<String, Expr>, patmap: &HashMap<String, Pattern>, id: u32) {
    match it {
        Item::Fn(f) => subst_fn(f, tmap, imap, pmap, patmap, id),
        Item::Type(t) => {
            rename_name(&mut t.name, imap);
            for fld in &mut t.fields {
                subst_type(&mut fld.ty, tmap, imap);
            }
            for v in &mut t.variants {
                for fld in &mut v.fields {
                    subst_type(&mut fld.ty, tmap, imap);
                }
            }
            for md in &mut t.methods {
                subst_fn(md, tmap, imap, pmap, patmap, id);
            }
        }
        Item::Impl(im) => {
            if let Some(tn) = &mut im.trait_name {
                rename_name(tn, imap);
            }
            subst_type(&mut im.for_type, tmap, imap);
            for md in &mut im.methods {
                subst_fn(md, tmap, imap, pmap, patmap, id);
            }
        }
        Item::Const { value, .. } => {
            // A macro-body const references only params (no local binders) → no
            // hygiene rename needed; substitute parameters in its initializer.
            subst_block_expr(value, pmap, patmap);
        }
        // A nested `other!(args)` invocation inside a macro body: substitute the
        // outer macro's parameters into its arguments, so it re-expands correctly
        // in the next fixpoint round.
        Item::MacroInvoke { args, .. } => {
            for a in args {
                subst_block_expr(a, pmap, patmap);
            }
        }
        _ => {}
    }
}

fn subst_fn(f: &mut FnDef, tmap: &HashMap<String, Type>, imap: &HashMap<String, String>, pmap: &HashMap<String, Expr>, patmap: &HashMap<String, Pattern>, id: u32) {
    rename_name(&mut f.sig.name, imap);
    for p in &mut f.sig.params {
        if let Some(ty) = &mut p.ty {
            subst_type(ty, tmap, imap);
        }
    }
    if let Some(ret) = &mut f.sig.ret {
        subst_type(ret, tmap, imap);
    }
    if let Some(body) = &mut f.body {
        // Hygiene: rename the body's own bindings so they cannot collide with or
        // capture names from the call site.
        let mut binders: HashSet<String> = HashSet::new();
        collect_binders_block(body, &mut binders);
        let rename: HashMap<String, String> = binders.iter().map(|b| (b.clone(), format!("{b}$m{id}"))).collect();
        subst_block(body, pmap, patmap, &rename);
    }
}

/// Substitute parameters in a standalone expression (no local binders).
fn subst_block_expr(e: &mut Expr, pmap: &HashMap<String, Expr>, patmap: &HashMap<String, Pattern>) {
    crate::expand::subst_expr(e, pmap, patmap, &HashMap::new());
}

/// Resolve an ident/type/method *name*: first the `##` token-paste sentinel
/// (each fragment mapped through the ident params, else kept literal), then a
/// bare ident-parameter rename.
fn rename_name(name: &mut String, imap: &HashMap<String, String>) {
    if name.contains('\u{1}') {
        *name = name
            .split('\u{1}')
            .map(|frag| imap.get(frag).cloned().unwrap_or_else(|| frag.to_string()))
            .collect::<String>();
        return;
    }
    if let Some(n) = imap.get(name) {
        *name = n.clone();
    }
}

/// Substitute a type parameter (whole node), resolve a `##` pasted type name, or
/// rename via an ident parameter.
fn subst_type(t: &mut Type, tmap: &HashMap<String, Type>, imap: &HashMap<String, String>) {
    if let Some(rt) = tmap.get(&t.name) {
        let borrowed = t.borrowed || rt.borrowed;
        *t = rt.clone();
        t.borrowed = borrowed;
        return; // the argument type is a bare name; nothing to recurse into
    }
    // A pasted type reference (`Base ## Box`) — resolve each fragment through the
    // ident params, else keep literal, exactly like a defined name.
    rename_name(&mut t.name, imap);
    for a in &mut t.args {
        subst_type(a, tmap, imap);
    }
}
