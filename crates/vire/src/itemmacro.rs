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
use crate::diag::Span;
use crate::expand::{collect_binders_block, subst_block};

const S: Span = Span(0, 0);

/// Expand every `name!(...)` invocation into the macro's substituted items and
/// drop the macro definitions. Returns diagnostics (unknown macro, arity, kind
/// mismatch).
pub fn expand_item_macros(m: &mut Module) -> Vec<String> {
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
    let old = std::mem::take(&mut m.items);
    let mut out: Vec<Item> = Vec::with_capacity(old.len());
    for it in old {
        match it {
            Item::ItemMacro { .. } => {} // drop the definition
            Item::MacroInvoke { name, args, .. } => match defs.get(&name) {
                None => errs.push(format!("unknown item macro `{name}!`")),
                Some((params, items)) => match expand_one(&name, params, items, &args, &mut counter) {
                    Ok(mut gen) => out.append(&mut gen),
                    Err(e) => errs.push(e),
                },
            },
            other => out.push(other),
        }
    }
    m.items = out;

    // Safety net: item macros make top-level name collisions easy (two invocations,
    // or the no-token-pasting limitation). Report duplicate `fn`/`type` names with a
    // clear front-end diagnostic instead of a late, cryptic LLVM "invalid
    // redefinition". (Only when macros actually defined items.)
    if !had_defs {
        return errs;
    }
    let mut fn_names: HashMap<String, u32> = HashMap::new();
    let mut ty_names: HashMap<String, u32> = HashMap::new();
    for it in &m.items {
        match it {
            Item::Fn(f) => *fn_names.entry(f.sig.name.clone()).or_insert(0) += 1,
            Item::Type(t) => *ty_names.entry(t.name.clone()).or_insert(0) += 1,
            _ => {}
        }
    }
    let mut dups: Vec<String> = fn_names
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(name, n)| format!("duplicate function `{name}` defined {n} times (item-macro name collision — names need to be distinct)"))
        .chain(
            ty_names
                .into_iter()
                .filter(|(_, n)| *n > 1)
                .map(|(name, n)| format!("duplicate type `{name}` defined {n} times (item-macro name collision — names need to be distinct)")),
        )
        .collect();
    dups.sort();
    errs.extend(dups);
    errs
}

fn expand_one(name: &str, params: &[MacroParam], items: &[Item], args: &[Expr], counter: &mut u32) -> Result<Vec<Item>, String> {
    if params.len() != args.len() {
        return Err(format!("item macro `{name}!`: expected {} argument(s), {} given", params.len(), args.len()));
    }

    // Kind-check each argument against its parameter's declared kind, building the
    // three substitution maps. This is the safety gate: a wrong-kind argument is a
    // hard error, never a blind splice.
    let mut tmap: HashMap<String, Type> = HashMap::new(); // type param → Type
    let mut imap: HashMap<String, String> = HashMap::new(); // ident param → name
    let mut pmap: HashMap<String, Expr> = HashMap::new(); // expr subst (idents + exprs)
    for (p, a) in params.iter().zip(args) {
        match p.kind {
            ParamKind::Type => match a {
                Expr::Ident(n, sp) => {
                    tmap.insert(p.name.clone(), Type { name: n.clone(), args: vec![], borrowed: false, span: *sp });
                }
                _ => return Err(format!("item macro `{name}!`: argument for `{}: type` must be a type name", p.name)),
            },
            ParamKind::Ident => match a {
                Expr::Ident(n, _) => {
                    imap.insert(p.name.clone(), n.clone());
                    pmap.insert(p.name.clone(), Expr::Ident(n.clone(), S));
                }
                _ => return Err(format!("item macro `{name}!`: argument for `{}: ident` must be an identifier", p.name)),
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
        subst_item(&mut c, &tmap, &imap, &pmap, id);
        gen.push(c);
    }
    Ok(gen)
}

// --- Substitution over declarations ------------------------------------------

fn subst_item(it: &mut Item, tmap: &HashMap<String, Type>, imap: &HashMap<String, String>, pmap: &HashMap<String, Expr>, id: u32) {
    match it {
        Item::Fn(f) => subst_fn(f, tmap, imap, pmap, id),
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
                subst_fn(md, tmap, imap, pmap, id);
            }
        }
        Item::Impl(im) => {
            if let Some(tn) = &mut im.trait_name {
                rename_name(tn, imap);
            }
            subst_type(&mut im.for_type, tmap, imap);
            for md in &mut im.methods {
                subst_fn(md, tmap, imap, pmap, id);
            }
        }
        Item::Const { value, .. } => {
            // A macro-body const references only params (no local binders) → no
            // hygiene rename needed; substitute parameters in its initializer.
            subst_block_expr(value, pmap);
        }
        _ => {}
    }
}

fn subst_fn(f: &mut FnDef, tmap: &HashMap<String, Type>, imap: &HashMap<String, String>, pmap: &HashMap<String, Expr>, id: u32) {
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
        subst_block(body, pmap, &rename);
    }
}

/// Substitute parameters in a standalone expression (no local binders).
fn subst_block_expr(e: &mut Expr, pmap: &HashMap<String, Expr>) {
    crate::expand::subst_expr(e, pmap, &HashMap::new());
}

fn rename_name(name: &mut String, imap: &HashMap<String, String>) {
    if let Some(n) = imap.get(name) {
        *name = n.clone();
    }
}

/// Substitute a type parameter (whole node) or rename via an ident parameter.
fn subst_type(t: &mut Type, tmap: &HashMap<String, Type>, imap: &HashMap<String, String>) {
    if let Some(rt) = tmap.get(&t.name) {
        let borrowed = t.borrowed || rt.borrowed;
        *t = rt.clone();
        t.borrowed = borrowed;
        return; // the argument type is a bare name; nothing to recurse into
    }
    if let Some(n) = imap.get(&t.name) {
        t.name = n.clone();
    }
    for a in &mut t.args {
        subst_type(a, tmap, imap);
    }
}
