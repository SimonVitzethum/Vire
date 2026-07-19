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
        // Derivation reads the fields; only non-generic product types today.
        if !t.generics.is_empty() {
            errs.push(format!("@derive on generic type `{}` is not yet supported", t.name));
            continue;
        }
        if !t.variants.is_empty() {
            errs.push(format!("@derive on sum type `{}` is not yet supported (product types only)", t.name));
            continue;
        }
        for (d, _span) in wanted {
            let (mname, gen): (&str, fn(&TypeDef) -> FnDef) = match d.as_str() {
                "Eq" => ("eq", derive_eq),
                "Show" => ("show", derive_show),
                _ => {
                    errs.push(format!("unknown derive `{d}` on `{}` (supported: Eq, Show)", t.name));
                    continue;
                }
            };
            // An explicit method of the same name wins (user override).
            if t.methods.iter().any(|md| md.sig.name == mname) {
                continue;
            }
            let md = gen(t);
            t.methods.push(md);
        }
    }
    errs
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
fn self_field(name: &str) -> Expr {
    field(Expr::SelfExpr(S), name)
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
