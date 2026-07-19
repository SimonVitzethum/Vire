//! `@derive(...)` — reflection-driven method generation.
//!
//! Phase 3b of the compile-time programming layer (see TODO.md): read a type's
//! structure and synthesize methods from it, instead of hand-writing boilerplate.
//! This is source-to-source — each derived trait produces an ordinary method
//! `FnDef` injected into the type, which then goes through inference and lowering
//! like any hand-written method. No runtime reflection (AOT).
//!
//! Supported today: `Eq` (structural equality) and `Show` (a `T(f, …)` string),
//! for non-generic product (struct) types. A method the user already wrote is
//! left untouched (an explicit definition overrides the derive).

use crate::ast::*;
use crate::diag::Span;

const S: Span = Span(0, 0);

/// Expand `@derive(...)` on every type. Returns diagnostics for unknown derives
/// or unsupported targets.
pub fn derive_expand(m: &mut Module) -> Vec<String> {
    let mut errs = Vec::new();
    for it in m.items.iter_mut() {
        let Item::Type(t) = it else { continue };
        if t.attrs.is_empty() {
            continue;
        }
        // Collect the requested derives (from every `@derive(...)` attribute).
        let mut wanted: Vec<(String, Span)> = Vec::new();
        for a in &t.attrs {
            if a.name == "derive" {
                for d in &a.args {
                    wanted.push((d.clone(), a.span));
                }
            } else {
                errs.push(format!("unknown attribute `@{}` (only `@derive` is supported)", a.name));
            }
        }
        if wanted.is_empty() {
            continue;
        }
        // Derivation reads the structure; generic types (fields reference `T`) are
        // not yet supported for either shape.
        if !t.generics.is_empty() {
            errs.push(format!("@derive on generic type `{}` is not yet supported", t.name));
            continue;
        }
        let is_sum = !t.variants.is_empty();
        for (d, _span) in wanted {
            let mname = match d.as_str() {
                "Eq" => "eq",
                "Show" => "show",
                "Ord" => "cmp",
                "Hash" => "hash",
                "Json" => "to_json",
                _ => {
                    errs.push(format!("unknown derive `{d}` on `{}` (supported: Eq, Show, Ord, Hash, Json)", t.name));
                    continue;
                }
            };
            // An explicit method of the same name wins (user override).
            if t.methods.iter().any(|md| md.sig.name == mname) {
                continue;
            }
            let md: Option<FnDef> = match (d.as_str(), is_sum) {
                ("Eq", false) => Some(derive_eq(t)),
                ("Show", false) => Some(derive_show(t)),
                ("Json", false) => unwrap_or_err(derive_json(t), &mut errs),
                ("Ord", false) => unwrap_or_err(derive_ord(t), &mut errs),
                ("Hash", false) => unwrap_or_err(derive_hash(t), &mut errs),
                ("Eq", true) => Some(derive_eq_sum(t)),
                ("Show", true) => Some(derive_show_sum(t)),
                ("Json", true) => unwrap_or_err(derive_json_sum(t), &mut errs),
                ("Hash", true) => unwrap_or_err(derive_hash_sum(t), &mut errs),
                ("Ord", true) => unwrap_or_err(derive_ord_sum(t), &mut errs),
                _ => unreachable!(),
            };
            match md {
                Some(m) => t.methods.push(m),
                None => continue, // an error was already recorded
            }
        }
    }
    errs
}

/// Fold a `Result<FnDef, String>` into an `Option<FnDef>`, recording the error.
fn unwrap_or_err(r: Result<FnDef, String>, errs: &mut Vec<String>) -> Option<FnDef> {
    match r {
        Ok(m) => Some(m),
        Err(e) => {
            errs.push(e);
            None
        }
    }
}

// --- AST construction helpers ------------------------------------------------

fn ty_ref(n: &str) -> Type {
    Type { name: n.into(), args: vec![], borrowed: false, span: S }
}
fn ident(n: &str) -> Expr {
    Expr::Ident(n.into(), S)
}
fn field(base: Expr, name: &str) -> Expr {
    Expr::Field { base: Box::new(base), name: name.into(), span: S }
}
fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
    Expr::Binary { op, lhs: Box::new(l), rhs: Box::new(r), span: S }
}
fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call { callee: Box::new(ident(name)), args, span: S }
}
fn str_lit(s: &str) -> Expr {
    Expr::Str(s.into(), S)
}
fn int_lit(v: i128) -> Expr {
    Expr::Int(v, S)
}
fn self_field(name: &str) -> Expr {
    field(Expr::SelfExpr(S), name)
}
fn other_field(name: &str) -> Expr {
    field(ident("other"), name)
}
fn cast_int(e: Expr) -> Expr {
    Expr::Cast { inner: Box::new(e), ty: ty_ref("Int"), span: S }
}
/// A method call `recv.name(args)`.
fn method_call(recv: Expr, name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call { callee: Box::new(field(recv, name)), args, span: S }
}
/// `if cond { then_e } else { else_e }` as an expression.
fn if_expr(cond: Expr, then_e: Expr, else_e: Expr) -> Expr {
    let blk = |e: Expr| Block { stmts: vec![], tail: Some(Box::new(e)), span: S };
    Expr::If { cond: Box::new(cond), then: blk(then_e), elifs: vec![], els: Some(blk(else_e)), span: S }
}

/// Scalar/Str field kinds relevant to structural derivation. `Other` (a nested
/// user type) is not derivable for `Ord`/`Hash` without recursion → rejected.
#[derive(PartialEq)]
enum FKind {
    Int,
    Float,
    Bool,
    Str,
    Other,
}
fn fkind(ty: &Type) -> FKind {
    match ty.name.as_str() {
        "Int" | "I64" | "I32" | "U32" | "U64" => FKind::Int,
        "Float" | "F64" | "F32" => FKind::Float,
        "Bool" => FKind::Bool,
        "Str" => FKind::Str,
        _ => FKind::Other,
    }
}
fn method(name: &str, params: Vec<Param>, ret: Type, tail: Expr) -> FnDef {
    FnDef {
        sig: FnSig { name: name.into(), generics: vec![], params, ret: Some(ret), span: S },
        body: Some(Block { stmts: vec![], tail: Some(Box::new(tail)), span: S }),
        is_pub: false,
    }
}
fn param(name: &str, ty: Option<Type>) -> Param {
    Param { name: name.into(), ty, default: None }
}

// --- Derivations -------------------------------------------------------------

/// `fn eq(self, other: T) -> Bool { self.f1 == other.f1 && … }` (empty → `true`).
fn derive_eq(t: &TypeDef) -> FnDef {
    let mut cond: Option<Expr> = None;
    for f in &t.fields {
        let eq = bin(BinOp::Eq, self_field(&f.name), field(ident("other"), &f.name));
        cond = Some(match cond {
            None => eq,
            Some(c) => bin(BinOp::And, c, eq),
        });
    }
    let tail = cond.unwrap_or(Expr::Bool(true, S));
    method("eq", vec![param("self", None), param("other", Some(ty_ref(&t.name)))], ty_ref("Bool"), tail)
}

/// `fn show(self) -> Str { "T(" + str(self.f1) + ", " + … + ")" }`.
fn derive_show(t: &TypeDef) -> FnDef {
    let mut expr = str_lit(&format!("{}(", t.name));
    for (i, f) in t.fields.iter().enumerate() {
        if i > 0 {
            expr = bin(BinOp::Add, expr, str_lit(", "));
        }
        expr = bin(BinOp::Add, expr, call("str", vec![self_field(&f.name)]));
    }
    expr = bin(BinOp::Add, expr, str_lit(")"));
    method("show", vec![param("self", None)], ty_ref("Str"), expr)
}

/// `@derive(Ord)`/`@derive(Hash)` need per-field ordering/hashing, which is only
/// defined here for scalar and `Str` fields — a nested user-type field would need
/// its own derive (recursion), not yet supported.
fn require_scalar(t: &TypeDef, what: &str) -> Result<(), String> {
    for f in &t.fields {
        if fkind(&f.ty) == FKind::Other {
            return Err(format!(
                "@derive({what}) on `{}`: field `{}: {}` is not a scalar/Str type (nested derive not yet supported)",
                t.name, f.name, f.ty.name
            ));
        }
    }
    Ok(())
}

/// `fn cmp(self, other: T) -> Int { … }` — lexicographic ordering, returning
/// -1/0/1. Numeric/bool fields compare with `<`/`>`; `Str` fields via `compareTo`.
fn derive_ord(t: &TypeDef) -> Result<FnDef, String> {
    require_scalar(t, "Ord")?;
    let body = cmp_chain(&t.fields, 0);
    Ok(method("cmp", vec![param("self", None), param("other", Some(ty_ref(&t.name)))], ty_ref("Int"), body))
}

fn cmp_chain(fields: &[Field], i: usize) -> Expr {
    if i >= fields.len() {
        return int_lit(0); // all fields equal
    }
    let f = &fields[i];
    let (lt, gt) = if fkind(&f.ty) == FKind::Str {
        // `self.f.compareTo(other.f)` is <0 / 0 / >0.
        let cmp = || method_call(self_field(&f.name), "compareTo", vec![other_field(&f.name)]);
        (bin(BinOp::Lt, cmp(), int_lit(0)), bin(BinOp::Gt, cmp(), int_lit(0)))
    } else {
        (bin(BinOp::Lt, self_field(&f.name), other_field(&f.name)), bin(BinOp::Gt, self_field(&f.name), other_field(&f.name)))
    };
    if_expr(lt, int_lit(-1), if_expr(gt, int_lit(1), cmp_chain(fields, i + 1)))
}

/// `fn hash(self) -> Int { ((7 * 31 + h(f0)) * 31 + h(f1)) … }` — the classic
/// 31-multiplier combiner. Per-field hash: an Int/Bool/Float field is folded in
/// by value (Bool/Float cast to Int), a `Str` field via `hashCode()`.
fn derive_hash(t: &TypeDef) -> Result<FnDef, String> {
    require_scalar(t, "Hash")?;
    let mut acc = int_lit(7);
    for f in &t.fields {
        acc = bin(BinOp::Add, bin(BinOp::Mul, acc, int_lit(31)), field_hash_of(self_field(&f.name), &f.ty));
    }
    Ok(method("hash", vec![param("self", None)], ty_ref("Int"), acc))
}

/// Hash contribution of one field value `e` of type `ty`.
fn field_hash_of(e: Expr, ty: &Type) -> Expr {
    match fkind(ty) {
        FKind::Int => e,
        FKind::Bool | FKind::Float => cast_int(e),
        FKind::Str => method_call(e, "hashCode", vec![]),
        FKind::Other => unreachable!("rejected by require_scalar"),
    }
}

/// JSON rendering of one field value `e` of type `ty`: numbers bare, `Bool` as
/// true/false, `Str` quoted. (No escaping yet — see TODO.)
fn json_value(e: Expr, ty: &Type) -> Expr {
    match fkind(ty) {
        FKind::Bool => if_expr(e, str_lit("true"), str_lit("false")),
        FKind::Str => bin(BinOp::Add, bin(BinOp::Add, str_lit("\""), e), str_lit("\"")),
        _ => call("str", vec![e]), // Int / Float bare; Other rejected earlier
    }
}

/// `fn to_json(self) -> Str { "{" + "\"f\": " + <json f> + … + "}" }`.
fn derive_json(t: &TypeDef) -> Result<FnDef, String> {
    require_scalar(t, "Json")?;
    let mut e = str_lit("{");
    for (i, f) in t.fields.iter().enumerate() {
        if i > 0 {
            e = bin(BinOp::Add, e, str_lit(", "));
        }
        e = bin(BinOp::Add, e, str_lit(&format!("\"{}\": ", f.name)));
        e = bin(BinOp::Add, e, json_value(self_field(&f.name), &f.ty));
    }
    e = bin(BinOp::Add, e, str_lit("}"));
    Ok(method("to_json", vec![param("self", None)], ty_ref("Str"), e))
}

// --- Sum types (match on the variant tag) ------------------------------------

/// A constructor pattern `Name(p0, …, p{n-1})` binding `{prefix}0..{prefix}{n-1}`.
fn ctor_pat(name: &str, n: usize, prefix: &str) -> Pattern {
    Pattern::Ctor { name: name.into(), args: (0..n).map(|k| Pattern::Bind(format!("{prefix}{k}"), S)).collect(), span: S }
}

/// `fn show(self) -> Str { match self { Variant(a…) -> "Variant(" + str(a) + …, … } }`.
fn derive_show_sum(t: &TypeDef) -> FnDef {
    let arms = t
        .variants
        .iter()
        .map(|v| {
            let n = v.fields.len();
            let pat = ctor_pat(&v.name, n, "_a");
            let body = if n == 0 {
                str_lit(&v.name) // dataless → just the name
            } else {
                let mut e = str_lit(&format!("{}(", v.name));
                for k in 0..n {
                    if k > 0 {
                        e = bin(BinOp::Add, e, str_lit(", "));
                    }
                    e = bin(BinOp::Add, e, call("str", vec![ident(&format!("_a{k}"))]));
                }
                bin(BinOp::Add, e, str_lit(")"))
            };
            (pat, None, body)
        })
        .collect();
    let body = Expr::Match { scrutinee: Box::new(Expr::SelfExpr(S)), arms, span: S };
    method("show", vec![param("self", None)], ty_ref("Str"), body)
}

/// `fn eq(self, other: T) -> Bool { match self { V(a…) -> match other { V(b…) ->
/// a==b && …, _ -> false }, … } }` — same variant with equal payloads, else false.
fn derive_eq_sum(t: &TypeDef) -> FnDef {
    let arms = t
        .variants
        .iter()
        .map(|v| {
            let n = v.fields.len();
            let payload_eq = if n == 0 {
                Expr::Bool(true, S)
            } else {
                let mut c: Option<Expr> = None;
                for k in 0..n {
                    let eq = bin(BinOp::Eq, ident(&format!("_a{k}")), ident(&format!("_b{k}")));
                    c = Some(match c {
                        None => eq,
                        Some(p) => bin(BinOp::And, p, eq),
                    });
                }
                c.unwrap()
            };
            let inner = Expr::Match {
                scrutinee: Box::new(ident("other")),
                arms: vec![(ctor_pat(&v.name, n, "_b"), None, payload_eq), (Pattern::Wildcard(S), None, Expr::Bool(false, S))],
                span: S,
            };
            (ctor_pat(&v.name, n, "_a"), None, inner)
        })
        .collect();
    let body = Expr::Match { scrutinee: Box::new(Expr::SelfExpr(S)), arms, span: S };
    method("eq", vec![param("self", None), param("other", Some(ty_ref(&t.name)))], ty_ref("Bool"), body)
}

/// Ord/Hash/Json over a sum type need per-variant-field ordering/hashing/rendering,
/// defined here only for scalar and `Str` payloads.
fn require_scalar_sum(t: &TypeDef, what: &str) -> Result<(), String> {
    for v in &t.variants {
        for f in &v.fields {
            if fkind(&f.ty) == FKind::Other {
                return Err(format!(
                    "@derive({what}) on `{}`: variant `{}` field of type `{}` is not scalar/Str (nested derive not yet supported)",
                    t.name, v.name, f.ty.name
                ));
            }
        }
    }
    Ok(())
}

/// `fn hash(self) -> Int { match self { V(a…) -> ((17*31 + ord)*31 + h(a)…), … } }`
/// — folds the variant's declaration ordinal, then each payload field.
fn derive_hash_sum(t: &TypeDef) -> Result<FnDef, String> {
    require_scalar_sum(t, "Hash")?;
    let arms = t
        .variants
        .iter()
        .enumerate()
        .map(|(ord, v)| {
            let n = v.fields.len();
            let mut acc = bin(BinOp::Add, bin(BinOp::Mul, int_lit(17), int_lit(31)), int_lit(ord as i128));
            for k in 0..n {
                acc = bin(BinOp::Add, bin(BinOp::Mul, acc, int_lit(31)), field_hash_of(ident(&format!("_a{k}")), &v.fields[k].ty));
            }
            (ctor_pat(&v.name, n, "_a"), None, acc)
        })
        .collect();
    let body = Expr::Match { scrutinee: Box::new(Expr::SelfExpr(S)), arms, span: S };
    Ok(method("hash", vec![param("self", None)], ty_ref("Int"), body))
}

/// `fn cmp(self, other: T) -> Int { … }` for a sum type: variants order by
/// declaration ordinal (an earlier variant is smaller); within the same variant,
/// payloads compare lexicographically. A nested match on `other` gives the
/// ordinal comparison exactly (no separate ordinal accessor needed).
fn derive_ord_sum(t: &TypeDef) -> Result<FnDef, String> {
    require_scalar_sum(t, "Ord")?;
    let outer = t
        .variants
        .iter()
        .enumerate()
        .map(|(i, vi)| {
            let inner = t
                .variants
                .iter()
                .enumerate()
                .map(|(j, vj)| {
                    let body = if j < i {
                        int_lit(1) // self's variant is declared later → greater
                    } else if j > i {
                        int_lit(-1) // self's variant is declared earlier → smaller
                    } else {
                        cmp_chain_vars(&vi.fields, 0) // same variant → compare payloads
                    };
                    (ctor_pat(&vj.name, vj.fields.len(), "_b"), None, body)
                })
                .collect();
            let inner_match = Expr::Match { scrutinee: Box::new(ident("other")), arms: inner, span: S };
            (ctor_pat(&vi.name, vi.fields.len(), "_a"), None, inner_match)
        })
        .collect();
    let body = Expr::Match { scrutinee: Box::new(Expr::SelfExpr(S)), arms: outer, span: S };
    Ok(method("cmp", vec![param("self", None), param("other", Some(ty_ref(&t.name)))], ty_ref("Int"), body))
}

/// Lexicographic comparison of bound payload variables `_a{k}` vs `_b{k}`.
fn cmp_chain_vars(fields: &[Field], i: usize) -> Expr {
    if i >= fields.len() {
        return int_lit(0);
    }
    let (a, b) = (ident(&format!("_a{i}")), ident(&format!("_b{i}")));
    let (lt, gt) = if fkind(&fields[i].ty) == FKind::Str {
        let c = || method_call(ident(&format!("_a{i}")), "compareTo", vec![ident(&format!("_b{i}"))]);
        (bin(BinOp::Lt, c(), int_lit(0)), bin(BinOp::Gt, c(), int_lit(0)))
    } else {
        (bin(BinOp::Lt, a.clone(), b.clone()), bin(BinOp::Gt, a, b))
    };
    if_expr(lt, int_lit(-1), if_expr(gt, int_lit(1), cmp_chain_vars(fields, i + 1)))
}

/// `fn to_json(self) -> Str { match self { V(a…) -> "{\"V\": [json a, …]}", dataless
/// -> "\"V\"", … } }`.
fn derive_json_sum(t: &TypeDef) -> Result<FnDef, String> {
    require_scalar_sum(t, "Json")?;
    let arms = t
        .variants
        .iter()
        .map(|v| {
            let n = v.fields.len();
            let body = if n == 0 {
                str_lit(&format!("\"{}\"", v.name))
            } else {
                let mut e = str_lit(&format!("{{\"{}\": [", v.name));
                for k in 0..n {
                    if k > 0 {
                        e = bin(BinOp::Add, e, str_lit(", "));
                    }
                    e = bin(BinOp::Add, e, json_value(ident(&format!("_a{k}")), &v.fields[k].ty));
                }
                bin(BinOp::Add, e, str_lit("]}"))
            };
            (ctor_pat(&v.name, n, "_a"), None, body)
        })
        .collect();
    let body = Expr::Match { scrutinee: Box::new(Expr::SelfExpr(S)), arms, span: S };
    Ok(method("to_json", vec![param("self", None)], ty_ref("Str"), body))
}
