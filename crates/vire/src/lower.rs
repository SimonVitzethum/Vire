//! Lowering Vire AST → `crates/ir` (SSA-like, no slot reuse).
//! Covers the M2 core: functions, arithmetic, control flow (if/while/
//! for-over-Range), `print`, calls to own functions. Generics/traits/
//! closures/capsule follow (FRONTEND-PLAN F5–F8).

use std::collections::{BTreeSet, HashMap};

use fastllvm_ir::{ArrKind, BasicBlock, BinOp as IB, Block, ExternDecl, Function, Local, Operand, Program, Rvalue, Statement, Terminator, Ty};

use crate::ast::*;

/// Frozen dynamic-ABI version (the former DYNAMIC-VIRE-PLAN.md (removed in cleanup; see git history) §3). Bumped on any change to the
/// object header / descriptor / dispatch-slot layout a loaded module depends on. `1` = the
/// initial reflection substrate (P0). Exposed to programs via `abi_version()`.
const VIRE_ABI_VERSION: i64 = 1;

/// Field layout of a user type: (field name, IR type, ref target class).
type Layout = Vec<(String, Ty, Option<String>)>;

/// Variant of a sum type: (sum type name, tag, fields as (flattened name, type, ref class)).
type VariantInfo = (String, i64, Vec<(String, Ty, Option<String>)>);

/// Info about a generic function for monomorphization at call sites.
#[derive(Clone)]
struct GInfo {
    /// Type parameter names, e.g. `["T"]` for `fn f[T](…)`.
    tparams: Vec<String>,
    /// Per generic parameter: is it a comptime VALUE param (`[comptime N: Int]`)?
    /// Parallel to `tparams`. Value params bind to a literal, not a type.
    comptime: Vec<bool>,
    /// Parameter type annotations (with T placeholders), for binding the type arguments.
    param_tys: Vec<Option<Type>>,
    /// Return annotation (with T).
    ret: Option<Type>,
}

/// Symbol name of a monomorph. instance: `f$Int$Point`.
fn mono_sym(name: &str, targs: &[String]) -> String {
    format!("{name}${}", targs.join("$"))
}

/// Concrete type name of an argument (for type argument binding).
fn concrete_tyname(ty: Ty, class: Option<&String>) -> String {
    match ty {
        Ty::F64 => "Float".into(),
        Ty::F32 => "F32".into(),
        Ty::I32 => "I32".into(),
        Ty::Ref => class.cloned().unwrap_or_else(|| "Str".into()),
        _ => "Int".into(),
    }
}

/// Replaces type parameter names in a `Type` with concrete types.
fn subst_type(t: &Type, bind: &HashMap<String, String>) -> Type {
    let name = bind.get(&t.name).cloned().unwrap_or_else(|| t.name.clone());
    Type { name, args: t.args.iter().map(|a| subst_type(a, bind)).collect(), borrowed: t.borrowed, span: t.span }
}

/// Clones a generic FnDef and substitutes the type parameters in the signature +
/// body annotations (Let/Cast). The rest of the body goes through inference.
fn subst_fndef(f: &FnDef, bind: &HashMap<String, String>) -> FnDef {
    let mut nf = f.clone();
    nf.sig.generics = vec![]; // instance is no longer generic
    for p in &mut nf.sig.params {
        if let Some(t) = &p.ty {
            p.ty = Some(subst_type(t, bind));
        }
    }
    if let Some(t) = &nf.sig.ret {
        nf.sig.ret = Some(subst_type(t, bind));
    }
    if let Some(b) = &mut nf.body {
        subst_block(b, bind);
    }
    nf
}

fn subst_block(b: &mut crate::ast::Block, bind: &HashMap<String, String>) {
    for s in &mut b.stmts {
        subst_stmt(s, bind);
    }
    // Tail expressions rarely contain type annotations; casts within them via subst_expr.
    if let Some(t) = &mut b.tail {
        subst_expr(t, bind);
    }
}

fn subst_stmt(s: &mut Stmt, bind: &HashMap<String, String>) {
    match s {
        Stmt::Let { value: Some(v), .. } => subst_expr(v, bind),
        Stmt::Expr(e) | Stmt::Return(Some(e), _) => subst_expr(e, bind),
        Stmt::Assign { target, value, .. } => {
            subst_expr(target, bind);
            subst_expr(value, bind);
        }
        Stmt::While { cond, body, .. } => {
            subst_expr(cond, bind);
            subst_block(body, bind);
        }
        Stmt::For { iter, body, .. } => {
            subst_expr(iter, bind);
            subst_block(body, bind);
        }
        _ => {}
    }
}

fn subst_expr(e: &mut Expr, bind: &HashMap<String, String>) {
    match e {
        // A comptime value parameter (`N`) bound to a literal → inline the literal.
        // Value bindings are numeric strings; type bindings (e.g. "Int") never
        // appear in value position, so this only fires for value generics.
        Expr::Ident(n, sp) => {
            if let Some(v) = bind.get(n).and_then(|s| s.parse::<i128>().ok()) {
                *e = Expr::Int(v, *sp);
            }
        }
        Expr::Cast { ty, inner, .. } => {
            *ty = subst_type(ty, bind);
            subst_expr(inner, bind);
        }
        Expr::Binary { lhs, rhs, .. } => {
            subst_expr(lhs, bind);
            subst_expr(rhs, bind);
        }
        Expr::Unary { rhs, .. } => subst_expr(rhs, bind),
        Expr::Call { callee, args, .. } => {
            subst_expr(callee, bind);
            for a in args {
                subst_expr(a, bind);
            }
        }
        Expr::TurboCall { targs, args, .. } => {
            for t in targs {
                subst_expr(t, bind);
            }
            for a in args {
                subst_expr(a, bind);
            }
        }
        Expr::Index { base, index, .. } => {
            subst_expr(base, bind);
            subst_expr(index, bind);
        }
        Expr::Range { start, end, .. } => {
            subst_expr(start, bind);
            subst_expr(end, bind);
        }
        Expr::If { cond, then, elifs, els, .. } => {
            subst_expr(cond, bind);
            subst_block(then, bind);
            for (c, b) in elifs {
                subst_expr(c, bind);
                subst_block(b, bind);
            }
            if let Some(b) = els {
                subst_block(b, bind);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            subst_expr(scrutinee, bind);
            for (_, g, b) in arms {
                if let Some(g) = g {
                    subst_expr(g, bind);
                }
                subst_expr(b, bind);
            }
        }
        Expr::List(xs, _) => xs.iter_mut().for_each(|x| subst_expr(x, bind)),
        Expr::MapLit(kvs, _) => {
            for (k, v) in kvs {
                subst_expr(k, bind);
                subst_expr(v, bind);
            }
        }
        Expr::Block(b) => subst_block(b, bind),
        Expr::Field { base, .. } | Expr::Try { inner: base, .. } | Expr::Comptime { inner: base, .. } => subst_expr(base, bind),
        _ => {}
    }
}

/// Built-in FFI signatures. Reserved for explicit `extern` decls.
fn builtin_ffi_sigs() -> Vec<(&'static str, Vec<Ty>, Ty)> {
    vec![]
}

/// All methods (type-inline + `impl` blocks) as (class name, method).
fn collect_methods(m: &Module) -> Vec<(String, &FnDef)> {
    let mut out = Vec::new();
    for it in &m.items {
        match it {
            Item::Type(t) => out.extend(t.methods.iter().map(|meth| (t.name.clone(), meth))),
            Item::Impl(im) => out.extend(im.methods.iter().map(|meth| (im.for_type.name.clone(), meth))),
            _ => {}
        }
    }
    out
}

/// Element source for an iterator adapter (`fold`/`map`/`filter`/…): either a
/// numeric range counted `start..end` or a `$List` iterated by index.
enum IterSrc {
    Range { start: Operand, end: Operand, incl: bool },
    List(Operand),
}

/// Call signature of a function: parameter types, return type, return class
/// (for object returns the class name — for field access on the result).
struct Sig {
    params: Vec<Ty>,
    ret: Ty,
    ret_class: Option<String>,
    /// For an array return (`-> Array[T]` / `farray`): the element kind, so `f()[i]`
    /// lowers to a real array access (not "unknown array").
    ret_arr: Option<ArrKind>,
    /// For a ref-array return (`-> Array[Node]`): the element object class, so
    /// `f()[i].field` resolves and the result threads on like a local ref array.
    ret_arr_class: Option<String>,
    /// Per-parameter `Ptr` shape (parallel to `params`): true where the declared
    /// annotation is `Ptr`. Only meaningful for `extern` callees (see E421).
    ptr_params: Vec<bool>,
    /// True where the declared return annotation is `Ptr` (extern callees).
    ptr_ret: bool,
    /// True for `extern "C"` declarations (C-ABI boundary: Ptr provenance applies).
    is_extern: bool,
}

/// A type annotation that may cross the C-ABI boundary (`extern "C"` params and
/// returns, mirroring the `vire bindgen` mapping in main.rs): scalars + the
/// opaque `Ptr` handle. Anything else (Str, arrays, user objects, Result/Option)
/// has no C layout — passing it would hand a heap-object pointer to C, so it is
/// rejected (E421), never silently passed. `Unit` is allowed as a return (void).
fn extern_mappable(name: &str) -> bool {
    matches!(
        name,
        "Int" | "I64" | "U64" | "I32" | "U32" | "F64" | "Float" | "F32" | "Bool" | "Char"
            | "Byte" | "U8" | "I8" | "U16" | "I16" | "Short" | "Ptr" | "Unit"
    )
}

/// A declared `syscall` item (the former language/the former HW-SAFE.md (removed in cleanup; see git history) (removed in cleanup; see git history) §2): OS number + per-param
/// shape. `Int` passes directly; `Buf` (`Array[Byte]`) passes the data pointer
/// (the length stays an explicit user argument — no hidden pairing).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SysParamKind {
    Int,
    Buf,
}

#[derive(Clone)]
struct SyscallInfo {
    num: u64,
    params: Vec<SysParamKind>,
}
/// A bare `Result` annotation (no type arguments) is E-error — it would resolve
/// to the reserved (empty) `Result` class with no layout. `Result[T, E]` (both
/// given) is fine; anything else with that name is the user's own business.
fn check_result_ann(t: &Option<Type>, errs: &mut Vec<String>, wh: &str) {
    if let Some(tp) = t {
        if tp.name == "Result" && tp.args.len() != 2 {
            errs.push(format!("error[E100]: `{wh}` uses bare `Result` — give both type arguments (`Result[T, E]`)"));
        }
    }
}
/// Element kind (and, for a user object element, class) of an array return type
/// (`Array[Elem]` / `array` / `farray`) — else `(None, None)`.
fn arr_ret(t: Option<&Type>) -> (Option<ArrKind>, Option<String>) {
    let Some(t) = t else { return (None, None) };
    if t.name == "farray" {
        return (Some(ArrKind::Double), None);
    }
    if t.name == "Array" || t.name == "array" {
        return match t.args.first().and_then(|a| class_of(Some(a))) {
            Some(ec) => (Some(ArrKind::Ref), Some(ec)),
            None => (Some(t.args.first().map(|a| arrkind_of_name(&a.name)).unwrap_or(ArrKind::Long)), None),
        };
    }
    (None, None)
}

/// Array-return kind/class for a call into a GENERIC instantiation. A generic body
/// `fn f[T](…) -> Array[T] { array(n) }` allocates an UNTYPED (scalar-vtable) array —
/// `array(n)` inside the body has no visibility of `T` — so a REF element `T` (e.g.
/// `Array[Node]`) would leak (the array's drop never releases the ref slots). Only
/// SCALAR element returns are sound here; a ref element is left unattributed, so the
/// caller's `f()[i]` is a loud "unknown array" error rather than a silent leak. (A
/// non-generic factory that writes `mut a: Array[Node] = array(n)` builds a real ref
/// array and is handled by `arr_ret`.)
fn arr_ret_generic(t: Option<&Type>) -> (Option<ArrKind>, Option<String>) {
    match arr_ret(t) {
        (Some(ArrKind::Ref), _) => (None, None),
        other => other,
    }
}

/// `Rc` gate scan (AGENTS.md §2.5, E300). The name `Rc` is reserved: any mention
/// (type, definition, binding, constructor call, pattern, import) outside a gated
/// kernel module (`kernel` marker + `use kernel::Rc`) is a loud error — never a
/// silent scalar/class fallback. Inside a gated module each `Rc[T]` is validated
/// (exactly one argument, a plain user object type — no nesting, no scalars,
/// no arrays, no Option/Result) and its instance class `Rc$T` is collected for
/// layout aliasing.
fn rc_gate_msg(where_: &str) -> String {
    format!("error[E300]: `Rc` in {where_} is only available in kernel modules — add a top-level `kernel` marker plus `use kernel::Rc`, or restructure without shared ownership (AGENTS.md §2.5)")
}

/// Scalar-ish names that can never be an `Rc` payload (they need no sharing).
fn rc_scalar_name(n: &str) -> bool {
    matches!(
        n,
        "Int" | "I64" | "U64" | "I32" | "U32" | "F64" | "Float" | "F32" | "Bool" | "Char"
            | "Byte" | "U8" | "I8" | "U16" | "I16" | "Short" | "Unit" | "Ptr" | "Str"
            | "Array" | "array" | "farray" | "Mmio" | "Ref" | "Option" | "Result"
    )
}

fn rc_scan_type(
    t: &Type,
    where_: &str,
    allowed: bool,
    types: &HashMap<String, Layout>,
    tparams: &[String],
    needs: &mut Vec<(String, String)>,
    errs: &mut Vec<String>,
) {
    if t.name != "Rc" {
        for a in &t.args {
            rc_scan_type(a, where_, allowed, types, tparams, needs, errs);
        }
        return;
    }
    if !allowed {
        errs.push(rc_gate_msg(where_));
        for a in &t.args {
            rc_scan_type(a, where_, allowed, types, tparams, needs, errs);
        }
        return;
    }
    if t.args.len() != 1 {
        errs.push(format!("error[E100]: `Rc` in {where_} needs exactly one type argument (`Rc[T]`)"));
        return;
    }
    let inner = &t.args[0];
    if inner.name == "Rc" {
        errs.push(format!("error[E300]: nested `Rc[Rc[T]]` in {where_} is not meaningful — flatten to a single `Rc[T]` (AGENTS.md §2.5)"));
        return;
    }
    if !inner.args.is_empty() {
        errs.push(format!("error[E300]: `Rc[{}]` in {where_} needs a plain object type (generic instances are not supported) (AGENTS.md §2.5)", inner.name));
        return;
    }
    if rc_scalar_name(&inner.name) {
        errs.push(format!("error[E300]: `Rc[{}]` in {where_} needs an object type — scalars need no sharing, arrays are already owned (AGENTS.md §2.5)", inner.name));
        return;
    }
    if tparams.contains(&inner.name) {
        errs.push(format!("error[E300]: `Rc[{}]` in {where_} over a generic parameter is not supported yet (AGENTS.md §2.5)", inner.name));
        return;
    }
    if !types.contains_key(&inner.name) {
        errs.push(format!("error[E300]: `Rc[{}]` in {where_} refers to unknown type `{}` (AGENTS.md §2.5)", inner.name, inner.name));
        return;
    }
    needs.push((format!("Rc${}", inner.name), inner.name.clone()));
}

fn rc_scan_sig(
    sig: &FnSig,
    what: &str,
    allowed: bool,
    types: &HashMap<String, Layout>,
    tparams: &[String],
    needs: &mut Vec<(String, String)>,
    errs: &mut Vec<String>,
) {
    if sig.name == "Rc" {
        errs.push("error[E300]: `Rc` is a reserved name (the kernel shared-ownership type) — rename the function (AGENTS.md §2.5)".into());
    }
    for p in &sig.params {
        if p.name == "Rc" {
            errs.push(format!("error[E300]: `Rc` is a reserved name — rename parameter `Rc` of `{what}` (AGENTS.md §2.5)"));
        }
        if let Some(t) = p.ty.as_ref() {
            rc_scan_type(t, &format!("parameter `{}` of `{what}`", p.name), allowed, types, tparams, needs, errs);
        }
    }
    if let Some(t) = sig.ret.as_ref() {
        rc_scan_type(t, &format!("return type of `{what}`"), allowed, types, tparams, needs, errs);
    }
}

fn rc_scan_block(
    b: &Block2,
    where_: &str,
    allowed: bool,
    types: &HashMap<String, Layout>,
    tparams: &[String],
    needs: &mut Vec<(String, String)>,
    errs: &mut Vec<String>,
) {
    for s in &b.stmts {
        rc_scan_stmt(s, where_, allowed, types, tparams, needs, errs);
    }
    if let Some(t) = b.tail.as_deref() {
        rc_scan_expr(t, where_, allowed, types, tparams, needs, errs);
    }
}

fn rc_scan_stmt(
    s: &Stmt,
    where_: &str,
    allowed: bool,
    types: &HashMap<String, Layout>,
    tparams: &[String],
    needs: &mut Vec<(String, String)>,
    errs: &mut Vec<String>,
) {
    match s {
        Stmt::Let { name, ty, value, .. } => {
            if name == "Rc" {
                errs.push(format!("error[E300]: `Rc` is a reserved name — rename the binding in `{where_}` (AGENTS.md §2.5)"));
            }
            if let Some(t) = ty.as_ref() {
                rc_scan_type(t, &format!("annotation of `{name}` in `{where_}`"), allowed, types, tparams, needs, errs);
            }
            if let Some(v) = value.as_ref() {
                rc_scan_expr(v, where_, allowed, types, tparams, needs, errs);
            }
        }
        Stmt::Assign { target, value, .. } => {
            rc_scan_expr(target, where_, allowed, types, tparams, needs, errs);
            rc_scan_expr(value, where_, allowed, types, tparams, needs, errs);
        }
        Stmt::Expr(e) => rc_scan_expr(e, where_, allowed, types, tparams, needs, errs),
        Stmt::Return(e, _) => {
            if let Some(e) = e.as_ref() {
                rc_scan_expr(e, where_, allowed, types, tparams, needs, errs);
            }
        }
        Stmt::While { cond, body, .. } => {
            rc_scan_expr(cond, where_, allowed, types, tparams, needs, errs);
            rc_scan_block(body, where_, allowed, types, tparams, needs, errs);
        }
        Stmt::For { pat, iter, body, .. } => {
            rc_scan_pattern(pat, where_, errs);
            rc_scan_expr(iter, where_, allowed, types, tparams, needs, errs);
            rc_scan_block(body, where_, allowed, types, tparams, needs, errs);
        }
        Stmt::Break(_) | Stmt::Continue(_) => {}
    }
}

fn rc_scan_pattern(pat: &Pattern, where_: &str, errs: &mut Vec<String>) {
    match pat {
        Pattern::Bind(n, _) if n == "Rc" => {
            errs.push(format!("error[E300]: `Rc` is a reserved name — rename the binding in `{where_}` (AGENTS.md §2.5)"));
        }
        Pattern::Ctor { name, args, .. } if name == "Rc" => {
            errs.push(format!("error[E300]: `Rc(..)` pattern in `{where_}` — `Rc` values are built via `mut r: Rc[T] = T(..)`, not patterns (AGENTS.md §2.5)"));
            for a in args {
                rc_scan_pattern(a, where_, errs);
            }
        }
        Pattern::Ctor { args, .. } => {
            for a in args {
                rc_scan_pattern(a, where_, errs);
            }
        }
        Pattern::Tuple(ps, _) | Pattern::Or(ps, _) => {
            for a in ps {
                rc_scan_pattern(a, where_, errs);
            }
        }
        _ => {}
    }
}

fn rc_scan_expr(
    e: &Expr,
    where_: &str,
    allowed: bool,
    types: &HashMap<String, Layout>,
    tparams: &[String],
    needs: &mut Vec<(String, String)>,
    errs: &mut Vec<String>,
) {
    match e {
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Str(..)
        | Expr::Char(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::SelfExpr(..) => {}
        Expr::Unary { rhs, .. } => rc_scan_expr(rhs, where_, allowed, types, tparams, needs, errs),
        Expr::Binary { lhs, rhs, .. } => {
            rc_scan_expr(lhs, where_, allowed, types, tparams, needs, errs);
            rc_scan_expr(rhs, where_, allowed, types, tparams, needs, errs);
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(n, _) = callee.as_ref() {
                if n == "Rc" {
                    if allowed {
                        errs.push(format!("error[E300]: `Rc(..)` constructor call in `{where_}` is not supported — build via `mut r: Rc[T] = T(..)` (AGENTS.md §2.5)"));
                    } else {
                        errs.push(rc_gate_msg(&format!("call in `{where_}`")));
                    }
                }
            } else {
                rc_scan_expr(callee, where_, allowed, types, tparams, needs, errs);
            }
            for a in args {
                rc_scan_expr(a, where_, allowed, types, tparams, needs, errs);
            }
        }
        Expr::TurboCall { targs, args, .. } => {
            for t in targs {
                if let Expr::Ident(n, _) = t {
                    if n == "Rc" {
                        errs.push(rc_gate_msg(&format!("type argument in `{where_}`")));
                    }
                } else {
                    rc_scan_expr(t, where_, allowed, types, tparams, needs, errs);
                }
            }
            for a in args {
                rc_scan_expr(a, where_, allowed, types, tparams, needs, errs);
            }
        }
        Expr::Field { base, .. } => rc_scan_expr(base, where_, allowed, types, tparams, needs, errs),
        Expr::Index { base, index, .. } => {
            rc_scan_expr(base, where_, allowed, types, tparams, needs, errs);
            rc_scan_expr(index, where_, allowed, types, tparams, needs, errs);
        }
        Expr::If { cond, then, elifs, els, .. } => {
            rc_scan_expr(cond, where_, allowed, types, tparams, needs, errs);
            rc_scan_block(then, where_, allowed, types, tparams, needs, errs);
            for (c, b) in elifs {
                rc_scan_expr(c, where_, allowed, types, tparams, needs, errs);
                rc_scan_block(b, where_, allowed, types, tparams, needs, errs);
            }
            if let Some(b) = els.as_ref() {
                rc_scan_block(b, where_, allowed, types, tparams, needs, errs);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            rc_scan_expr(scrutinee, where_, allowed, types, tparams, needs, errs);
            for (p, g, b) in arms {
                rc_scan_pattern(p, where_, errs);
                if let Some(g) = g.as_ref() {
                    rc_scan_expr(g, where_, allowed, types, tparams, needs, errs);
                }
                rc_scan_expr(b, where_, allowed, types, tparams, needs, errs);
            }
        }
        Expr::Block(b) => rc_scan_block(b, where_, allowed, types, tparams, needs, errs),
        Expr::Lambda { params, body, .. } => {
            for p in params {
                if p == "Rc" {
                    errs.push(format!("error[E300]: `Rc` is a reserved name — rename the parameter in `{where_}` (AGENTS.md §2.5)"));
                }
            }
            rc_scan_expr(body, where_, allowed, types, tparams, needs, errs);
        }
        Expr::List(xs, _) => {
            for x in xs {
                rc_scan_expr(x, where_, allowed, types, tparams, needs, errs);
            }
        }
        Expr::Comprehension { elem, var, iter, cond, .. } => {
            if var == "Rc" {
                errs.push(format!("error[E300]: `Rc` is a reserved name — rename the variable in `{where_}` (AGENTS.md §2.5)"));
            }
            rc_scan_expr(elem, where_, allowed, types, tparams, needs, errs);
            rc_scan_expr(iter, where_, allowed, types, tparams, needs, errs);
            if let Some(c) = cond.as_deref() {
                rc_scan_expr(c, where_, allowed, types, tparams, needs, errs);
            }
        }
        Expr::MapLit(kvs, _) => {
            for (k, v) in kvs {
                rc_scan_expr(k, where_, allowed, types, tparams, needs, errs);
                rc_scan_expr(v, where_, allowed, types, tparams, needs, errs);
            }
        }
        Expr::Try { inner, .. } | Expr::Comptime { inner, .. } => {
            rc_scan_expr(inner, where_, allowed, types, tparams, needs, errs)
        }
        Expr::Cast { inner, ty, .. } => {
            rc_scan_expr(inner, where_, allowed, types, tparams, needs, errs);
            rc_scan_type(ty, &format!("cast in `{where_}`"), allowed, types, tparams, needs, errs);
        }
        Expr::Range { start, end, .. } => {
            rc_scan_expr(start, where_, allowed, types, tparams, needs, errs);
            rc_scan_expr(end, where_, allowed, types, tparams, needs, errs);
        }
        Expr::Capsule { inputs, body, .. } => {
            for (n, _) in inputs {
                if n == "Rc" {
                    errs.push(format!("error[E300]: `Rc` is a reserved name — rename the input in `{where_}` (AGENTS.md §2.5)"));
                }
            }
            rc_scan_block(body, where_, allowed, types, tparams, needs, errs);
        }
        Expr::Region { max, body, .. } => {
            rc_scan_expr(max, where_, allowed, types, tparams, needs, errs);
            rc_scan_block(body, where_, allowed, types, tparams, needs, errs);
        }
        Expr::Spawn { call, .. } => rc_scan_expr(call, where_, allowed, types, tparams, needs, errs),
    }
}

fn rc_scan_fndef(
    f: &FnDef,
    what: &str,
    allowed: bool,
    types: &HashMap<String, Layout>,
    outer_tparams: &[String],
    needs: &mut Vec<(String, String)>,
    errs: &mut Vec<String>,
) {
    let mut tparams: Vec<String> = outer_tparams.to_vec();
    for g in &f.sig.generics {
        tparams.push(g.name.clone());
        if let Some(t) = g.ty.as_ref() {
            rc_scan_type(t, &format!("generic bound of `{what}`"), allowed, types, &tparams, needs, errs);
        }
    }
    rc_scan_sig(&f.sig, what, allowed, types, &tparams, needs, errs);
    if let Some(b) = f.body.as_ref() {
        rc_scan_block(b, what, allowed, types, &tparams, needs, errs);
    }
}

fn rc_scan_item(
    it: &Item,
    allowed: bool,
    types: &HashMap<String, Layout>,
    needs: &mut Vec<(String, String)>,
    errs: &mut Vec<String>,
) {
    match it {
        Item::Fn(f) => rc_scan_fndef(f, &format!("fn {}", f.sig.name), allowed, types, &[], needs, errs),
        Item::Type(t) => {
            if t.name == "Rc" {
                errs.push("error[E300]: `Rc` is a reserved name (the kernel shared-ownership type) — rename the type (AGENTS.md §2.5)".into());
            }
            if t.name == "Rc" {
                return;
            }
            let mut tparams: Vec<String> = t.generics.iter().map(|g| g.name.clone()).collect();
            for g in &t.generics {
                if let Some(ty) = g.ty.as_ref() {
                    rc_scan_type(ty, &format!("generic bound of `type {}`", t.name), allowed, types, &tparams, needs, errs);
                }
            }
            for fld in &t.fields {
                rc_scan_type(&fld.ty, &format!("field `{}` of `type {}`", fld.name, t.name), allowed, types, &tparams, needs, errs);
            }
            for v in &t.variants {
                for fld in &v.fields {
                    rc_scan_type(&fld.ty, &format!("variant `{}` of `type {}`", v.name, t.name), allowed, types, &tparams, needs, errs);
                }
            }
            for m in &t.methods {
                if m.sig.name == "Rc" {
                    errs.push(format!("error[E300]: `Rc` is a reserved name — rename method `Rc` of `type {}` (AGENTS.md §2.5)", t.name));
                }
                let mut mtparams = tparams.clone();
                for g in &m.sig.generics {
                    mtparams.push(g.name.clone());
                }
                rc_scan_sig(&m.sig, &format!("{}.{}", t.name, m.sig.name), allowed, types, &mtparams, needs, errs);
                if let Some(b) = m.body.as_ref() {
                    rc_scan_block(b, &format!("{}.{}", t.name, m.sig.name), allowed, types, &mtparams, needs, errs);
                }
            }
        }
        Item::Trait(tr) => {
            if tr.name == "Rc" {
                errs.push("error[E300]: `Rc` is a reserved name (the kernel shared-ownership type) — rename the trait (AGENTS.md §2.5)".into());
            }
            for m in &tr.methods {
                rc_scan_fndef(m, &format!("trait {}.{}", tr.name, m.sig.name), allowed, types, &tr.generics.iter().map(|g| g.name.clone()).collect::<Vec<_>>(), needs, errs);
            }
        }
        Item::Impl(im) => {
            if im.trait_name.as_deref() == Some("Rc") {
                errs.push("error[E300]: `Rc` is a reserved name — cannot implement trait `Rc` (AGENTS.md §2.5)".into());
            }
            rc_scan_type(&im.for_type, &format!("impl target"), allowed, types, &[], needs, errs);
            for m in &im.methods {
                rc_scan_fndef(m, &format!("impl {}.{}", im.for_type.name, m.sig.name), allowed, types, &[], needs, errs);
            }
        }
        Item::Const { value, .. } => {
            rc_scan_expr(value, "const", allowed, types, &[], needs, errs);
        }
        Item::Use { path, .. } => {
            if path.len() == 2 && path[0] == "kernel" && path[1] == "Rc" {
                return;
            }
            if path.iter().any(|s| s == "Rc") {
                errs.push(rc_gate_msg(&format!("import `{}`", path.join("::"))));
            }
        }
        Item::Extern { items, .. } => {
            for s in items {
                rc_scan_sig(s, &format!("extern {}", s.name), allowed, types, &[], needs, errs);
            }
        }
        Item::Syscall { params, ret, name, .. } => {
            for p in params {
                if let Some(t) = p.ty.as_ref() {
                    rc_scan_type(t, &format!("parameter of `syscall {name}`"), allowed, types, &[], needs, errs);
                }
            }
            if let Some(t) = ret.as_ref() {
                rc_scan_type(t, &format!("return of `syscall {name}`"), allowed, types, &[], needs, errs);
            }
        }
        Item::Macro { body, .. } => {
            rc_scan_expr(body, "macro", allowed, types, &[], needs, errs);
        }
        Item::ItemMacro { items, .. } => {
            for sub in items {
                rc_scan_item(sub, allowed, types, needs, errs);
            }
        }
        Item::MacroInvoke { args, .. } => {
            for a in args {
                rc_scan_expr(a, "macro invocation", allowed, types, &[], needs, errs);
            }
        }
        Item::Native { .. } | Item::Cxx { .. } | Item::Kernel { .. } => {}
    }
}

pub fn lower_module(m: &Module) -> Result<Program, Vec<String>> {
    lower_module_src(m, "", &[])
}

/// Byte offset → 1-based source line (for debug info); 0 if unknown.
fn line_of(line_starts: &[usize], byte: usize) -> u32 {
    if line_starts.is_empty() {
        return 0;
    }
    line_starts.partition_point(|&s| s <= byte) as u32
}

/// `hw`: declared hardware regions (name → (base, len)) from `--hw-profile`
/// (the former language/the former HW-SAFE.md (removed in cleanup; see git history) (removed in cleanup; see git history) §2), resolved by `Mmio.bind("name")`.
pub fn lower_module_src(m: &Module, src: &str, hw: &[(String, u64, u64)]) -> Result<Program, Vec<String>> {
    let line_starts: Vec<usize> = if src.is_empty() {
        Vec::new()
    } else {
        std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect()
    };
    let ls = &line_starts[..];
    let mut prog = Program::default();
    let mut errs = Vec::new();
    // The backend emits volatile accesses + descriptors from this table.
    prog.hw_regions = hw.iter().cloned().collect();
    // Declared syscalls (name → number + param shapes), filled by the E100/E421
    // pre-pass below and consulted at every call site + in `sigs`.
    let mut syscalls: HashMap<String, SyscallInfo> = HashMap::new();
    // Declared hardware regions (name → (base, len)) for `Mmio.bind("name")`.
    let hw_regions: HashMap<String, (u64, u64)> =
        hw.iter().map(|(n, b, l)| (n.clone(), (*b, *l))).collect();

    // E100 (AGENTS.md §4, fully typed): after inference every `fn` parameter and
    // return type must be known. Anything still un-annotated here was NOT inferred
    // (inference only ever fills in, never defaults) — silently defaulting to I64
    // would miscompile instead of rejecting. `main`'s return stays Void by design;
    // `self` is always a Ref; generic fns are exempt (monomorphized per call site,
    // their `T` params bind at instantiation).
    for it in &m.items {
        if let Item::Fn(f) = it {
            if !f.sig.generics.is_empty() {
                continue;
            }
            // Higher-order templates are never lowered standalone (inlined at
            // call sites, params bound there) — nothing to check here.
            if is_higher_order(f) {
                continue;
            }
            for p in &f.sig.params {
                if p.ty.is_none() {
                    errs.push(format!(
                        "error[E100]: cannot infer type of parameter `{}` in `fn {}` — annotate it (e.g. `{}: Int`)",
                        p.name, f.sig.name, p.name
                    ));
                }
                check_result_ann(&p.ty, &mut errs, &format!("parameter `{}` of `fn {}`", p.name, f.sig.name));
            }
            if f.sig.ret.is_none() && f.sig.name != "main" {
                errs.push(format!(
                    "error[E100]: cannot infer return type of `fn {}` — annotate it (e.g. `-> Int`)",
                    f.sig.name
                ));
            }
            check_result_ann(&f.sig.ret, &mut errs, &format!("return type of `fn {}`", f.sig.name));
        }
        if let Item::Extern { items, .. } = it {
            for s in items {
                for p in &s.params {
                    if p.ty.is_none() {
                        errs.push(format!(
                            "error[E100]: `extern` parameter `{}` in `{}` must be annotated (C-ABI needs the type)",
                            p.name, s.name
                        ));
                    }
                    check_result_ann(&p.ty, &mut errs, &format!("parameter `{}` of `extern {}`", p.name, s.name));
                }
                if s.ret.is_none() {
                    errs.push(format!(
                        "error[E100]: `extern` function `{}` must have an annotated return type",
                        s.name
                    ));
                }
                check_result_ann(&s.ret, &mut errs, &format!("return type of `extern {}`", s.name));
            }
        }
        // `syscall NAME(params) -> Ret = NUM` (the former language/the former HW-SAFE.md (removed in cleanup; see git history) (removed in cleanup; see git history) §2): declarations
        // are always fully typed (E100) and narrowly shaped (E421): at most 6 params
        // (the kernel ABI), each `Int` or `Array[Byte]`, return `Int`.
        if let Item::Syscall { name, params, ret, num, span: _ } = it {
            let mut kinds = Vec::new();
            let mut bad = false;
            for p in params {
                match p.ty.as_ref().map(|t| t.name.as_str()) {
                    None => {
                        errs.push(format!(
                            "error[E100]: `syscall` parameter `{}` in `{}` must be annotated (`Int` or `Array[Byte]`)",
                            p.name, name
                        ));
                        bad = true;
                    }
                    Some("Int" | "I64" | "U64" | "I32" | "U32") => kinds.push(SysParamKind::Int),
                    Some("Array" | "array") if p.ty.as_ref().and_then(|t| t.args.first()).is_some_and(|a| matches!(a.name.as_str(), "Byte" | "U8")) => {
                        kinds.push(SysParamKind::Buf)
                    }
                    Some(other) => {
                        errs.push(format!(
                            "error[E421]: `syscall` parameter `{}` in `{}` has type `{other}` — only `Int` and `Array[Byte]` cross into a syscall (no objects, no strings; use `.bytes()`)",
                            p.name, name
                        ));
                        bad = true;
                    }
                }
            }
            if params.len() > 6 {
                errs.push(format!(
                    "error[E421]: `syscall {name}` has {} parameters — the kernel ABI carries at most 6",
                    params.len()
                ));
                bad = true;
            }
            match ret.as_ref().map(|t| t.name.as_str()) {
                Some("Int" | "I64" | "U64" | "I32" | "U32") => {}
                _ => {
                    errs.push(format!(
                        "error[E421]: `syscall {name}` must declare `-> Int` (a syscall returns a machine word)"
                    ));
                    bad = true;
                }
            }
            if !bad {
                if let Some(prev) = syscalls.get(name) {
                    errs.push(format!(
                        "error[E421]: duplicate `syscall {name}` (previous number {})",
                        prev.num
                    ));
                } else {
                    syscalls.insert(name.clone(), SyscallInfo { num: *num, params: kinds });
                }
            }
        }
    }

    // Product types → classes. Sum types → ONE tagged class: field `__tag`
    // (I64) + all variant fields flattened (`Variant_field`). Match dispatches
    // via `__tag`. (Space = sum of all variants; simple, fits the flat
    // class model. A more compact union follows later.)
    let mut types: HashMap<String, Layout> = HashMap::new();
    // (type name, field name) → element kind, for array-typed struct fields.
    let mut field_arr: HashMap<(String, String), ArrKind> = HashMap::new();
    let mut variants: HashMap<String, VariantInfo> = HashMap::new();
    // Generic product types (`type Box[T] { value: T }`): do NOT register
    // directly as a class (fields reference T). Instead monomorphized per used
    // type argument combination (`Box$Float`) — like generic
    // functions. Name → (type parameters, fields).
    let mut generic_ptypes: HashMap<String, (Vec<String>, Vec<Field>)> = HashMap::new();
    // Generic sum types (`type Maybe[T] { Some2(T) | Nothing }` + the built-in
    // Option[T]): monomorphized per type argument (`Option$Float`), so that Float
    // payloads are type-correct (no i64 erasure). Name → (type parameter, [(variant,
    // [(field name, type name)])]). Single-parameter; Result stays i64-erased.
    let mut generic_stypes: HashMap<String, (Vec<String>, Vec<(String, Vec<(String, String)>)>)> = HashMap::new();
    let mut variant_owner_g: HashMap<String, String> = HashMap::new();
    // `weak f: T` fields, keyed `Type.field` (the former DYNAMIC-VIRE-PLAN.md (removed in cleanup; see git history) §6). A side table
    // instead of a 4th element in `Layout`, which threads through two dozen signatures
    // and means nothing to any of them: only `FieldInfo` construction reads this.
    let mut weak_fields: BTreeSet<String> = BTreeSet::new();
    for it in &m.items {
        if let Item::Type(t) = it {
            // `weak` is meaningful only on a product type's ref field. On a sum-type
            // payload or a generic type parameter it would need per-variant / per-
            // instantiation ownership tracking that step 2b does not build — reject it
            // instead of silently ignoring the modifier (a silently-owning "weak" field
            // would make the acyclicity gate lie).
            for f in t.fields.iter().filter(|f| f.weak) {
                if !t.generics.is_empty() {
                    errs.push(format!("`weak` is not supported on a generic type ({}.{})", t.name, f.name));
                } else if ty_of(Some(&f.ty)) != Ty::Ref {
                    errs.push(format!("`weak {}: {}` — only a reference field can be weak", f.name, f.ty.name));
                } else {
                    weak_fields.insert(format!("{}.{}", t.name, f.name));
                }
            }
            for v in &t.variants {
                for f in v.fields.iter().filter(|f| f.weak) {
                    errs.push(format!("`weak` is not supported on a sum-type payload ({}.{})", v.name, f.name));
                }
            }
            if !t.generics.is_empty() && t.variants.is_empty() {
                generic_ptypes.insert(
                    t.name.clone(),
                    (t.generics.iter().map(|g| g.name.clone()).collect(), t.fields.clone()),
                );
                continue;
            }
            if !t.generics.is_empty() {
                // Generic sum type.
                let tparams: Vec<String> = t.generics.iter().map(|g| g.name.clone()).collect();
                let variants_g: Vec<(String, Vec<(String, String)>)> = t
                    .variants
                    .iter()
                    .map(|v| {
                        let vf = v
                            .fields
                            .iter()
                            .enumerate()
                            .map(|(i, f)| {
                                let fname = if f.name.is_empty() { format!("{}_{i}", v.name) } else { format!("{}_{}", v.name, f.name) };
                                (fname, f.ty.name.clone())
                            })
                            .collect();
                        variant_owner_g.insert(v.name.clone(), t.name.clone());
                        (v.name.clone(), vf)
                    })
                    .collect();
                generic_stypes.insert(t.name.clone(), (tparams, variants_g));
                continue;
            }
            if t.variants.is_empty() {
                let layout: Layout = t
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), ty_of(Some(&f.ty)), class_of(Some(&f.ty))))
                    .collect();
                // Record array-typed fields' element kind, so `x.field[i]` in the
                // body knows how to index it (a plain GetField only yields a `Ref`).
                for f in &t.fields {
                    if let Some(k) = field_arrkind(&f.ty) {
                        field_arr.insert((t.name.clone(), f.name.clone()), k);
                    }
                }
                types.insert(t.name.clone(), layout);
            } else {
                let mut layout: Layout = vec![("__tag".into(), Ty::I64, None)];
                for (tag, v) in t.variants.iter().enumerate() {
                    let vfields: Vec<(String, Ty, Option<String>)> = v
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(i, f)| {
                            let fname = if f.name.is_empty() { format!("{}_{i}", v.name) } else { format!("{}_{}", v.name, f.name) };
                            (fname, ty_of(Some(&f.ty)), class_of(Some(&f.ty)))
                        })
                        .collect();
                    layout.extend(vfields.iter().cloned());
                    variants.insert(v.name.clone(), (t.name.clone(), tag as i64, vfields));
                }
                types.insert(t.name.clone(), layout);
            }
        }
    }
    // Built-in sum types Option/Result (if not defined by the user).
    // Payload is currently i64-wide (Int/pointer); typed/Float payloads
    // need generic types (next step).
    if !types.contains_key("Option") {
        types.insert("Option".into(), vec![("__tag".into(), Ty::I64, None), ("Some_value".into(), Ty::I64, None)]);
        variants.insert("Some".into(), ("Option".into(), 0, vec![("Some_value".into(), Ty::I64, None)]));
        variants.insert("None".into(), ("Option".into(), 1, vec![]));
    }
    if !types.contains_key("Result") {
        // Erased Result is gone (see generic_stypes below): a bare `Result`
        // annotation with no type arguments is rejected where it is used
        // (E-error, never a dangling class). The name stays reserved here so
        // user types cannot claim it.
        types.insert("Result".into(), vec![]);
    }
    // Built-in generic Option[T]: `Some(x)` is monomorphized type-correctly
    // (`Option$Float` carries F64), `None` stays over the erased Option (only __tag).
    if !generic_stypes.contains_key("Option") {
        generic_stypes.insert(
            "Option".into(),
            (vec!["T".into()], vec![("Some".into(), vec![("Some_value".into(), "T".into())]), ("None".into(), vec![])]),
        );
        variant_owner_g.entry("Some".into()).or_insert("Option".into());
        variant_owner_g.entry("None".into()).or_insert("Option".into());
    }
    // Built-in generic Result[T, E]: `Ok(x)`/`Err(e)` monomorphize to `Result$T$E`
    // with fully typed payload slots (AGENTS.md §4) — no i64 erasure anywhere.
    // There is deliberately NO erased fallback: mixed erased/mono layouts diverge
    // for Ref payloads (memory-unsafe under RC), so every site resolves (T, E)
    // from payload, annotation, or enclosing signature, or fails loudly.
    if !generic_stypes.contains_key("Result") {
        generic_stypes.insert(
            "Result".into(),
            (
                vec!["T".into(), "E".into()],
                vec![
                    ("Ok".into(), vec![("Ok_value".into(), "T".into())]),
                    ("Err".into(), vec![("Err_error".into(), "E".into())]),
                ],
            ),
        );
        variant_owner_g.entry("Ok".into()).or_insert("Result".into());
        variant_owner_g.entry("Err".into()).or_insert("Result".into());
    }
    // `Rc[T]` KERNEL GATE (AGENTS.md §2.5, E300): the name `Rc` is reserved.
    // Userspace use is a loud error, never a silent I64 (bare `Rc` would
    // otherwise fall through `ty_of`/`class_of` into a bogus scalar/class).
    // A kernel module (`kernel` marker + `use kernel::Rc`) may use `Rc[T]`
    // with a plain object type T; the instance class `Rc$T` aliases T's layout
    // (same fields, same drop/trace) but forces sharing in the solver.
    let is_kernel = m.items.iter().any(|it| matches!(it, Item::Kernel { .. }));
    let imported_rc = m.items.iter().any(|it| matches!(it, Item::Use { path, .. }
        if path.len() == 2 && path[0] == "kernel" && path[1] == "Rc"));
    let rc_allowed = is_kernel && imported_rc;
    {
        let mut rc_needs: Vec<(String, String)> = Vec::new();
        for it in &m.items {
            rc_scan_item(it, rc_allowed, &types, &mut rc_needs, &mut errs);
        }
        // Register layout aliases (deduped): `Rc$T` shares T's fields, and
        // `weak` back-edges carry over (a weak field stays non-owning).
        rc_needs.sort();
        rc_needs.dedup();
        for (mangled, inner) in &rc_needs {
            match types.get(inner) {
                Some(layout) => {
                    let layout = layout.clone();
                    for (fname, _, _) in &layout {
                        let key = format!("{inner}.{fname}");
                        if weak_fields.contains(&key) {
                            weak_fields.insert(format!("{mangled}.{fname}"));
                        }
                    }
                    types.insert(mangled.clone(), layout);
                }
                None => {
                    errs.push(format!("error[E300]: `Rc[{inner}]` refers to unknown type `{inner}` (AGENTS.md §2.5)"));
                }
            }
        }
    }
    // TRAIT OBJECTS (dynamic dispatch): traits are registered as interfaces,
    // `impl Trait for Typ` adds the methods into the type vtable at
    // consistent global slots (the backend's existing interface/CallVirtual
    // machinery). Trait → [(method, descriptor, param types incl. self, return)].
    let mut trait_methods: HashMap<String, Vec<(String, String, Vec<Ty>, Ty)>> = HashMap::new();
    for it in &m.items {
        if let Item::Trait(tr) = it {
            let ms = tr
                .methods
                .iter()
                .map(|meth| {
                    let params: Vec<Ty> = meth.sig.params.iter().map(|p| if p.name == "self" { Ty::Ref } else { ty_of(p.ty.as_ref()) }).collect();
                    (meth.sig.name.clone(), method_desc(&params), params, guess_ret_ty(meth))
                })
                .collect();
            trait_methods.insert(tr.name.clone(), ms);
        }
    }
    // Type → implemented traits (from `impl Trait for Typ`).
    let mut type_traits: HashMap<String, Vec<String>> = HashMap::new();
    for it in &m.items {
        if let Item::Impl(im) = it {
            if let Some(tn) = &im.trait_name {
                if trait_methods.contains_key(tn) {
                    type_traits.entry(im.for_type.name.clone()).or_default().push(tn.clone());
                }
            }
        }
    }
    // Register ClassInfo per type (user + built-in).
    let mut all_type_names: Vec<String> = m
        .items
        .iter()
        .filter_map(|it| if let Item::Type(t) = it { Some(t.name.clone()) } else { None })
        .filter(|n| types.contains_key(n))
        .collect();
    for bi in ["Option", "Result"] {
        if types.contains_key(bi) && !all_type_names.iter().any(|n| n == bi) {
            all_type_names.push(bi.into());
        }
    }
    // Kernel `Rc$T` aliases get their own ClassInfo (vtable/drop/trace identical
    // to T — same fields — so field access, method calls and drops resolve; the
    // `Rc$` marker only forces sharing in the solver, never a new layout).
    // Implemented-trait slots carry over too (the vtable entries point at the
    // shared `T.method` bodies), so trait dispatch works through Rc handles.
    {
        let mut rcs: Vec<String> = types.keys().filter(|k| k.starts_with("Rc$")).cloned().collect();
        rcs.sort();
        for r in rcs {
            if !all_type_names.iter().any(|n| n == &r) {
                all_type_names.push(r.clone());
            }
            let inner = r.strip_prefix("Rc$").unwrap_or(&r);
            if let Some(ifaces) = type_traits.get(inner).cloned() {
                type_traits.entry(r).or_insert(ifaces);
            }
        }
    }
    for tname in &all_type_names {
        let fields = types[tname]
            .iter()
            .map(|(n, ty, rt)| fastllvm_ir::FieldInfo {
                name: n.clone(),
                ty: *ty,
                ref_target: rt.clone(),
                weak: weak_fields.contains(&format!("{tname}.{n}")),
            })
            .collect();
        // Implemented traits → interfaces + the impl methods into the vtable
        // (mangled = `Typ.methode`, as collect_methods lowers them).
        let ifaces = type_traits.get(tname).cloned().unwrap_or_default();
        let mut methods = Vec::new();
        for tn in &ifaces {
            if let Some(tms) = trait_methods.get(tn) {
                for (mn, d, _, _) in tms {
                    methods.push(fastllvm_ir::MethodInfo {
                        name: mn.clone(),
                        desc: d.clone(),
                        is_static: false,
                        has_body: true,
                        mangled: format!("{tname}.{mn}"),
                    });
                }
            }
        }
        prog.classes.push(fastllvm_ir::ClassInfo {
            name: tname.clone(),
            super_name: Some("java/lang/Object".to_string()),
            is_interface: false,
            interfaces: ifaces,
            fields,
            static_fields: vec![],
            methods,
            has_clinit: false,
        });
    }
    // Traits as interface ClassInfos (abstract methods → global vtable slots).
    for (tname, ms) in &trait_methods {
        let methods = ms
            .iter()
            .map(|(mn, d, _, _)| fastllvm_ir::MethodInfo { name: mn.clone(), desc: d.clone(), is_static: false, has_body: false, mangled: String::new() })
            .collect();
        prog.classes.push(fastllvm_ir::ClassInfo {
            name: tname.clone(),
            super_name: Some("java/lang/Object".to_string()),
            is_interface: true,
            interfaces: vec![],
            fields: vec![],
            static_fields: vec![],
            methods,
            has_clinit: false,
        });
    }

    // Signature table (name → (param types, return type, return class)) for calls.
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            let ps = f.sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
            let (ra, rac) = arr_ret(f.sig.ret.as_ref());
            // `Ptr` shapes for user functions too: a `fn set(p: Ptr)` body trusts
            // `p`, so the call site is checked exactly like the C boundary (E421).
            // (The body itself is proven at its own `-> Ptr` returns.)
            let ptr_params = f.sig.params.iter().map(|p| p.ty.as_ref().is_some_and(|t| t.name == "Ptr")).collect();
            let ptr_ret = f.sig.ret.as_ref().is_some_and(|t| t.name == "Ptr");
            sigs.insert(f.sig.name.clone(), Sig { params: ps, ret: guess_ret_ty(f), ret_class: class_of_ann(f.sig.ret.as_ref(), &generic_ptypes, &generic_stypes), ret_arr: ra, ret_arr_class: rac, ptr_params, ptr_ret, is_extern: false });
        }
        // `cxx { … }` is REMOVED (AGENTS.md §1/§3, E410): an embedded C++ body is
        // a blind escape (it never passes the verifier — only C/asm do). C++
        // interop lives outside the language: `extern "C" header` + the C++ TU
        // linked via `--obj`/`-l` (C-ABI, like `cpp_call.vr`).
        if let Item::Cxx { .. } = it {
            errs.push("error[E410]: `cxx { … }` is removed — embedded C++ is a blind escape (AGENTS.md §1/§3). Put the C++ in its own TU behind `extern \"C\"` wrappers and link it via `--obj`/`-l` with an `extern \"C\"` declaration".into());
        }
        // extern "C" { fn name(...) -> T }: C-ABI function, directly under its
        // name (no mangling). Calls resolve through this; the backend
        // declares the called-but-undefined function, clang links it
        // (libc/libm/-lstdc++ / linked objects).
        // E421 (AGENTS.md §3): the mapping is CHECKED — only scalars + opaque
        // `Ptr` cross the boundary (the same set `vire bindgen` generates).
        // Str/arrays/user objects have no C layout; passing one would hand a
        // heap-object pointer to C — rejected, never silently passed.
        // Compiler-generated glue (`C-glue`: the `spawn` shims, `@c` captures)
        // is an internal handoff with internal shapes — it registers sigs but
        // skips the user-boundary mapping check.
        if let Item::Extern { items, abi, .. } = it {
            let checked = abi == "C";
            for sig in items {
                if checked {
                    for p in &sig.params {
                        let tn = p.ty.as_ref().map(|t| t.name.as_str()).unwrap_or("?");
                        if !extern_mappable(tn) {
                            errs.push(format!("error[E421]: `extern` parameter `{}` in `{}` has type `{tn}` — only scalars and opaque `Ptr` cross the C ABI (strings via `cstr(s)`, byte buffers via `bview(p, len)` + length; objects never)", p.name, sig.name));
                        }
                    }
                    if let Some(rt) = sig.ret.as_ref() {
                        if !extern_mappable(&rt.name) {
                            errs.push(format!("error[E421]: `extern` function `{}` returns `{}` — only scalars and opaque `Ptr` cross the C ABI", sig.name, rt.name));
                        }
                    }
                }
                let ps: Vec<Ty> = sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
                let (ra, rac) = arr_ret(sig.ret.as_ref());
                let ptr_params: Vec<bool> = sig.params.iter().map(|p| p.ty.as_ref().map(|t| t.name == "Ptr").unwrap_or(false)).collect();
                let ptr_ret = sig.ret.as_ref().map(|t| t.name == "Ptr").unwrap_or(false);
                let ret = ret_ty(sig);
                sigs.insert(sig.name.clone(), Sig { params: ps.clone(), ret, ret_class: class_of(sig.ret.as_ref()), ret_arr: ra, ret_arr_class: rac, ptr_params: ptr_params.clone(), ptr_ret, is_extern: true });
                // Record the C-ABI boundary in the IR (checked types as-is;
                // backends filter: LLVM links, Gabbro maps scalar-only).
                if checked {
                    prog.extern_fns.push(ExternDecl { name: sig.name.clone(), params: ps, ret, ptr_params: ptr_params.clone(), ptr_ret });
                }
            }
        }
        // Declared syscalls resolve like functions (same arity/type checking at
        // call sites); the call itself lowers to `syscall(NUM, …)` (see lower_call).
        // A name clash with a `fn` is a loud error, not a shadowing.
        if let Item::Syscall { name, .. } = it {
            if sigs.contains_key(name) {
                errs.push(format!("error[E421]: `syscall {name}` clashes with a function of the same name"));
            } else if let Some(info) = syscalls.get(name) {
                let ps = info.params.iter().map(|k| match k {
                    SysParamKind::Int => Ty::I64,
                    SysParamKind::Buf => Ty::Ref,
                }).collect();
                sigs.insert(name.clone(), Sig { params: ps, ret: Ty::I64, ret_class: None, ret_arr: None, ret_arr_class: None, ptr_params: vec![], ptr_ret: false, is_extern: false });
            }
        }
    }
    // Built-in Python bridge: always register the signatures, so that `py_import`
    // & co. are callable from pure Vire WITHOUT an `extern` block (the lowering
    // emits calls, the backend declares, the driver links the bridge).
    for (name, params, ret) in builtin_ffi_sigs() {
        sigs.entry(name.to_string()).or_insert(Sig { params, ret, ret_class: None, ret_arr: None, ret_arr_class: None, ptr_params: vec![], ptr_ret: false, is_extern: false });
    }
    // Methods (type-inline + impl blocks) → symbol `Class.method`, self = Ref.
    let methods = collect_methods(m);
    // Trait/impl coherence: a type must not define the same method name more than
    // once (across inline methods and `impl` blocks). Both would mangle to
    // `Type.method` and one definition would silently shadow the other — reject it
    // instead, so overlapping/duplicate impls are a compile error, not a surprise.
    {
        let mut counts: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
        for (class, meth) in &methods {
            *counts.entry((class.clone(), meth.sig.name.clone())).or_insert(0) += 1;
        }
        let mut dups: Vec<(String, String, usize)> =
            counts.into_iter().filter(|(_, n)| *n > 1).map(|((c, m), n)| (c, m, n)).collect();
        dups.sort();
        for (class, mname, n) in dups {
            errs.push(format!(
                "coherence: method `{mname}` is defined {n} times for type `{class}` (conflicting or overlapping impls)"
            ));
        }
    }
    for (class, meth) in &methods {
        let ps = meth
            .sig
            .params
            .iter()
            .map(|p| if p.name == "self" { Ty::Ref } else { ty_of(p.ty.as_ref()) })
            .collect();
        let sym = format!("{class}.{}", meth.sig.name);
        let (ra, rac) = arr_ret(meth.sig.ret.as_ref());
        let ptr_params = meth.sig.params.iter().map(|p| if p.name == "self" { false } else { p.ty.as_ref().is_some_and(|t| t.name == "Ptr") }).collect();
        let ptr_ret = meth.sig.ret.as_ref().is_some_and(|t| t.name == "Ptr");
        sigs.insert(sym, Sig { params: ps, ret: guess_ret_ty(meth), ret_class: class_of_ann(meth.sig.ret.as_ref(), &generic_ptypes, &generic_stypes), ret_arr: ra, ret_arr_class: rac, ptr_params, ptr_ret, is_extern: false });
    }
    // Collect generic functions (do NOT lower directly — one monomorph.
    // instance per call type argument). Trait bounds are parsed, but not yet
    // resolved (trait solving/coherence is the open hard half).
    let mut generics: HashMap<String, GInfo> = HashMap::new();
    let mut generic_defs: HashMap<String, FnDef> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if !f.sig.generics.is_empty() {
                generics.insert(
                    f.sig.name.clone(),
                    GInfo {
                        tparams: f.sig.generics.iter().map(|g| g.name.clone()).collect(),
                        comptime: f.sig.generics.iter().map(|g| g.is_comptime).collect(),
                        param_tys: f.sig.params.iter().map(|p| p.ty.clone()).collect(),
                        ret: f.sig.ret.clone(),
                    },
                );
                generic_defs.insert(f.sig.name.clone(), f.clone());
            }
        }
    }
    // Non-generic function ASTs for higher-order inlining.
    let mut fn_defs: HashMap<String, FnDef> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if f.sig.generics.is_empty() && f.body.is_some() {
                fn_defs.insert(f.sig.name.clone(), f.clone());
            }
        }
    }
    // Dynamic-fn / open-fn seams (the former DYNAMIC-VIRE-PLAN.md (removed in cleanup; see git history) P1): the slot index is the position
    // in this SORTED list — deterministic, and the single source of truth that both the
    // backend (`@jrt_dynslot`) and `install_override` compute from `fn_defs`, so they agree.
    {
        let mut dl: Vec<String> = fn_defs
            .values()
            .filter(|fd| fd.attrs.iter().any(|a| a.name == "__dynamic"))
            .map(|fd| fd.sig.name.clone())
            .collect();
        dl.sort();
        prog.dyn_fns = dl;
        // `@patchable fn` — a direct-call function with a NOP entry sled the runtime patcher
        // can redirect (hot-update, the former DYNAMIC-VIRE-PATCH.md (removed in cleanup; see git history) B1). Distinct from `dynamic fn`:
        // not slot-dispatched. Sorted for a deterministic `@jrt_patchtab` order.
        let mut pl: Vec<String> = fn_defs
            .values()
            .filter(|fd| fd.attrs.iter().any(|a| a.name == "patchable"))
            .map(|fd| fd.sig.name.clone())
            .collect();
        pl.sort();
        prog.patch_fns = pl;
    }
    // Pre-pass: instantiate annotation-driven generic instances (`-> Option[Float]`,
    // `b: Box[Int]`) module-wide, so that call sites/matches see the
    // concrete instance (layout + variants) — even in functions that only
    // use the type but do not annotate it.
    let mut shared_inst: HashMap<String, Layout> = HashMap::new();
    let mut shared_svars: HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>> = HashMap::new();
    {
        let mut seed = |t: &Type| {
            if t.args.is_empty() {
                return;
            }
            let targs: Vec<String> = t.args.iter().map(|a| a.name.clone()).collect();
            if let Some((tps, vars)) = generic_stypes.get(&t.name) {
                inst_stype(&t.name, tps, vars, &targs, &mut shared_inst, &mut shared_svars);
            } else if let Some((tps, fields)) = generic_ptypes.get(&t.name) {
                inst_ptype(&t.name, tps, fields, &targs, &mut shared_inst);
            }
        };
        for it in &m.items {
            if let Item::Fn(f) = it {
                for p in &f.sig.params {
                    if let Some(t) = &p.ty {
                        seed(t);
                    }
                }
                if let Some(t) = &f.sig.ret {
                    seed(t);
                }
            }
        }
        for (_, meth) in &methods {
            for p in &meth.sig.params {
                if let Some(t) = &p.ty {
                    seed(t);
                }
            }
            if let Some(t) = &meth.sig.ret {
                seed(t);
            }
        }
    }
    let mut mono_queue: Vec<(String, Vec<String>)> = Vec::new();
    // Instantiated generic types (mangled class → layout), collected from all
    // functions and afterwards registered as classes for the backend.
    let mut all_insts: HashMap<String, Layout> = HashMap::new();
    let mut str_index: HashMap<String, u32> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if !f.sig.generics.is_empty() {
                continue; // generic → only instantiated on demand
            }
            // REMOVED (AGENTS.md §1): GPU/Vulkan stages are gone. Loud error, not silent skip.
            if let Some(a) = f.attrs.iter().find(|a| matches!(a.name.as_str(), "gpu" | "vertex" | "fragment" | "mesh" | "task" | "compute" | "gpuvk")) {
                errs.push(format!("removed: `@{}` is gone — GPU/Vulkan is out per AGENTS.md §1 (pure CPU project)", a.name));
                continue;
            }
            if is_higher_order(f) {
                continue; // higher-order template → only inline (defunctionalization)
            }
            match lower_fn(f, &sigs, &syscalls, &hw_regions, &types, &field_arr, &variants, &generics, &trait_methods, &fn_defs, &generic_ptypes, &generic_stypes, &variant_owner_g, &shared_inst, &shared_svars, &mut prog.strings, &mut str_index, None, None, line_of(ls, f.sig.span.0), ls) {
                Ok((func, mono, insts, names)) => {
                    prog.debug_local_names.insert(func.name.clone(), names);
                    prog.functions.push(func);
                    mono_queue.extend(mono);
                    all_insts.extend(insts);
                }
                Err(mut e) => errs.append(&mut e),
            }
        }
        // trait/const/use: skipped here (trait dispatch open)
    }
    for (class, meth) in &methods {
        let sym = format!("{class}.{}", meth.sig.name);
        match lower_fn(meth, &sigs, &syscalls, &hw_regions, &types, &field_arr, &variants, &generics, &trait_methods, &fn_defs, &generic_ptypes, &generic_stypes, &variant_owner_g, &shared_inst, &shared_svars, &mut prog.strings, &mut str_index, Some(class), Some(&sym), line_of(ls, meth.sig.span.0), ls) {
            Ok((func, mono, insts, names)) => {
                prog.debug_local_names.insert(func.name.clone(), names);
                prog.functions.push(func);
                mono_queue.extend(mono);
                all_insts.extend(insts);
            }
            Err(mut e) => errs.append(&mut e),
        }
    }
    // Monomorphization worklist: substitute + lower each requested instance
    // (may request further instances), until fixpoint.
    let mut mono_done: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some((gname, targs)) = mono_queue.pop() {
        let sym = mono_sym(&gname, &targs);
        if !mono_done.insert(sym.clone()) {
            continue;
        }
        let Some(gdef) = generic_defs.get(&gname) else { continue };
        // Enforce declared trait bounds (`[T: Shape]`): every type argument must
        // implement each bound of its parameter. Checked here, at the concrete
        // instantiation, with a precise message pointing at the boundary — rather
        // than letting it surface downstream as a cryptic "type has no method X".
        // Only user (nominal) types can carry impls, so a primitive/instance name
        // absent from `type_traits` fails a non-empty bound. Skip lowering the
        // ill-typed instance (the build already fails on the error) to avoid a
        // duplicate downstream diagnostic.
        let mut bound_violation = false;
        for (gp, concrete) in gdef.sig.generics.iter().zip(targs.iter()) {
            for bound in &gp.bounds {
                let satisfied = type_traits.get(concrete).is_some_and(|ts| ts.iter().any(|t| t == bound));
                if !satisfied {
                    errs.push(format!(
                        "trait bound not satisfied: `{gname}` requires `{}: {}`, but the type argument `{concrete}` does not implement `{bound}`",
                        gp.name,
                        gp.bounds.join(" + "),
                    ));
                    bound_violation = true;
                }
            }
        }
        if bound_violation {
            continue;
        }
        let bind: HashMap<String, String> = gdef.sig.generics.iter().map(|g| g.name.clone()).zip(targs.iter().cloned()).collect();
        let inst = subst_fndef(gdef, &bind);
        // Register instance signature (for recursion/mutual calls).
        let ps = inst.sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
        let (ra, rac) = arr_ret(inst.sig.ret.as_ref());
        let ptr_params = inst.sig.params.iter().map(|p| p.ty.as_ref().is_some_and(|t| t.name == "Ptr")).collect();
        let ptr_ret = inst.sig.ret.as_ref().is_some_and(|t| t.name == "Ptr");
        sigs.insert(sym.clone(), Sig { params: ps, ret: guess_ret_ty(&inst), ret_class: class_of_ann(inst.sig.ret.as_ref(), &generic_ptypes, &generic_stypes), ret_arr: ra, ret_arr_class: rac, ptr_params, ptr_ret, is_extern: false });
        match lower_fn(&inst, &sigs, &syscalls, &hw_regions, &types, &field_arr, &variants, &generics, &trait_methods, &fn_defs, &generic_ptypes, &generic_stypes, &variant_owner_g, &shared_inst, &shared_svars, &mut prog.strings, &mut str_index, None, Some(&sym), line_of(ls, inst.sig.span.0), ls) {
            Ok((func, mono, insts, names)) => {
                prog.debug_local_names.insert(func.name.clone(), names);
                prog.functions.push(func);
                mono_queue.extend(mono);
                all_insts.extend(insts);
            }
            Err(mut e) => errs.append(&mut e),
        }
    }
    // Register instantiated generic types as classes (backend layout).
    // Merge annotation-driven (shared) + payload-driven (all_insts).
    for (k, v) in &shared_inst {
        all_insts.entry(k.clone()).or_insert_with(|| v.clone());
    }
    for (mangled, layout) in &all_insts {
        let fields = layout
            .iter()
            .map(|(n, ty, rt)| fastllvm_ir::FieldInfo { name: n.clone(), ty: *ty, ref_target: rt.clone(), weak: false })
            .collect();
        prog.classes.push(fastllvm_ir::ClassInfo {
            name: mangled.clone(),
            super_name: Some("java/lang/Object".to_string()),
            is_interface: false,
            interfaces: vec![],
            fields,
            static_fields: vec![],
            methods: vec![],
            has_clinit: false,
        });
    }
    if errs.is_empty() {
        // E300 enforcement lives in main.rs (post-solver-pass program — the same
        // IR the ownership report and the WARN preview see). Judging the fresh
        // lowering would reject programs whose News the escape analysis provably
        // promotes (false positives against AGENTS.md §2.1).
        Ok(prog)
    } else {
        Err(errs)
    }
}

fn ty_of(t: Option<&Type>) -> Ty {
    match t.map(|t| t.name.as_str()) {
        Some("Float") | Some("F64") => Ty::F64,
        Some("F32") => Ty::F32,
        Some("Bool") => Ty::I32,
        Some("Str") => Ty::Ref,
        Some("I32") | Some("U32") => Ty::I32,
        Some("Int") | Some("I64") | Some("U64") => Ty::I64,
        // `Ptr` = opaque raw pointer (FFI): i64-wide, NO RC (not a Vire object).
        Some("Ptr") => Ty::I64,
        Some("Unit") | None => Ty::I64, // default integer when nothing is given
        // Everything else is a (user) reference type: object on the heap.
        Some(_) => Ty::Ref,
    }
}

/// `mut r: Rc[T] = …` annotation → `(Rc$T, T)` when T resolves to an object
/// class (defensive: the gate scan already rejected malformed uses with errors;
/// None here simply skips relabeling and the scan's errors abort the build).
fn rc_mangled_of_ann(ann: &Option<Type>) -> Option<(String, String)> {
    let t = ann.as_ref()?;
    if t.name != "Rc" || t.args.len() != 1 {
        return None;
    }
    let c = class_of(Some(&t.args[0]))?;
    Some((format!("Rc${c}"), c))
}

/// Class name of a reference type annotation (for GetField/New), otherwise None.
fn class_of(t: Option<&Type>) -> Option<String> {
    let t = t?;
    let name = t.name.as_str();
    // `Rc[T]` (kernel only, gated in the pre-pass) resolves to the ownership
    // marker class `Rc$T` — same layout as `T`, but the solver forces sharing
    // (AGENTS.md §2.5). Malformed uses (caught by the gate scan with errors)
    // fall back to None here so lowering cannot invent a bogus class.
    if name == "Rc" {
        let inner = t.args.first()?;
        let c = class_of(Some(inner))?;
        return Some(format!("Rc${c}"));
    }
    match name {
        // `Ref` = an object reference of unknown class (the inference write-back name):
        // no class attached, exactly like `Str` — so a method call falls through to the
        // string-method / unknown-type path, never a bogus class named "Ref".
        "Float" | "F64" | "F32" | "Bool" | "Str" | "Ref" | "I32" | "U32" | "Int" | "I64" | "U64" | "Unit" | "Ptr"
        // Sub-word element types are scalars, not classes: `Array[Byte]` is a U8
        // array, not a ref array (a missing name here misreads the slots as ptr).
        | "Byte" | "U8" | "I8" | "Char" | "U16" | "I16" | "Short" => None,
        // `Mmio` is an opaque compiler handle (bound to a `--hw-profile` region),
        // not a heap class — field resolution must not invent a class `Mmio`.
        "Mmio" => None,
        _ => Some(name.to_string()),
    }
}

/// Class of an annotation taking generic types into account: `Option[Float]` →
/// `Option$Float` (the monomorphized instance), otherwise like `class_of`. For
/// return classes in signatures, so that call sites see the concrete instance.
fn class_of_ann(
    t: Option<&Type>,
    gp: &HashMap<String, (Vec<String>, Vec<Field>)>,
    gs: &HashMap<String, (Vec<String>, Vec<(String, Vec<(String, String)>)>)>,
) -> Option<String> {
    let t = t?;
    if !t.args.is_empty() && (gp.contains_key(&t.name) || gs.contains_key(&t.name)) {
        let args = t.args.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join("$");
        return Some(format!("{}${args}", t.name));
    }
    class_of(Some(t))
}

/// Instantiates a generic product type `base[targs]` → mangled class
/// `base$targs`; stores the substituted layout in `layouts`.
fn inst_ptype(base: &str, tparams: &[String], fields: &[Field], targs: &[String], layouts: &mut HashMap<String, Layout>) -> String {
    let mangled = format!("{base}${}", targs.join("$"));
    if !layouts.contains_key(&mangled) {
        let tmap: HashMap<String, String> = tparams.iter().cloned().zip(targs.iter().cloned()).collect();
        let layout: Layout = fields
            .iter()
            .map(|f| {
                let cn = tmap.get(&f.ty.name).cloned().unwrap_or_else(|| f.ty.name.clone());
                let t = Type { name: cn, args: vec![], borrowed: false, span: f.ty.span };
                (f.name.clone(), ty_of(Some(&t)), class_of(Some(&t)))
            })
            .collect();
        layouts.insert(mangled.clone(), layout);
    }
    mangled
}

/// Instantiates a generic sum type `sum[targs]` → mangled class.
/// Fills `layouts` (class layout) + `svars` (variant → (tag, field layout)).
fn inst_stype(
    sum: &str,
    tparams: &[String],
    variants: &[(String, Vec<(String, String)>)],
    targs: &[String],
    layouts: &mut HashMap<String, Layout>,
    svars: &mut HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>>,
) -> String {
    let mangled = format!("{sum}${}", targs.join("$"));
    if layouts.contains_key(&mangled) {
        return mangled;
    }
    let tmap: HashMap<String, String> = tparams.iter().cloned().zip(targs.iter().cloned()).collect();
    let mut layout: Layout = vec![("__tag".into(), Ty::I64, None)];
    let mut vmap: HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)> = HashMap::new();
    for (tag, (vname, vfields)) in variants.iter().enumerate() {
        let vf: Vec<(String, Ty, Option<String>)> = vfields
            .iter()
            .map(|(fname, tyname)| {
                let cn = tmap.get(tyname).cloned().unwrap_or_else(|| tyname.clone());
                let t = Type { name: cn, args: vec![], borrowed: false, span: crate::diag::Span(0, 0) };
                (fname.clone(), ty_of(Some(&t)), class_of(Some(&t)))
            })
            .collect();
        layout.extend(vf.iter().cloned());
        vmap.insert(vname.clone(), (tag as i64, vf));
    }
    layouts.insert(mangled.clone(), layout);
    svars.insert(mangled.clone(), vmap);
    mangled
}

/// Synthetic method descriptor (for consistent vtable slot assignment
/// between trait call and impl). `self` (first param) is omitted.
fn method_desc(params: &[Ty]) -> String {
    let mut s = String::from("(");
    for t in params.iter().skip(1) {
        s.push(ty_code(*t));
    }
    s.push(')');
    s
}
fn ty_code(t: Ty) -> char {
    match t {
        Ty::F64 | Ty::F32 => 'D',
        Ty::Ref => 'L',
        Ty::I32 => 'I',
        _ => 'J',
    }
}

fn ret_ty(sig: &FnSig) -> Ty {
    match &sig.ret {
        None => Ty::Void,
        Some(t) if t.name == "Unit" => Ty::Void,
        Some(t) => ty_of(Some(t)),
    }
}

/// Return type of a function — until type inference (F5) exists.
/// With `-> T` annotation: exact. Without: estimated structurally from the tail
/// expression (no tail → Void). Used for call sites AND the function itself,
/// so that both agree.
fn guess_ret_ty(f: &FnDef) -> Ty {
    // `main` is the entry point — always Void, regardless of whether the last line
    // was parsed as a (Void) expression like `print(x)` as tail.
    if f.sig.name == "main" {
        return Ty::Void;
    }
    if f.sig.ret.is_some() {
        return ret_ty(&f.sig);
    }
    match f.body.as_ref().and_then(|b| b.tail.as_ref()) {
        Some(t) => guess_expr_ty(t),
        None => Ty::Void,
    }
}

/// Rough, annotation-free type estimate of an expression (only literals/structure).
/// Idents/calls without context → I64 (default integer). Replaces real inference.
fn guess_expr_ty(e: &Expr) -> Ty {
    match e {
        // `print(...)` returns Void (intrinsic).
        Expr::Call { callee, .. } if matches!(callee.as_ref(), Expr::Ident(n, _) if n == "print") => Ty::Void,
        Expr::Float(..) => Ty::F64,
        Expr::Bool(..) => Ty::I32,
        Expr::Str(..) => Ty::Ref,
        Expr::Int(..) => Ty::I64,
        Expr::Unary { rhs, .. } => guess_expr_ty(rhs),
        Expr::If { then, els, .. } => {
            let t = then.tail.as_ref().map(|e| guess_expr_ty(e)).unwrap_or(Ty::Void);
            if t != Ty::Void {
                t
            } else {
                els.as_ref().and_then(|b| b.tail.as_ref()).map(|e| guess_expr_ty(e)).unwrap_or(Ty::Void)
            }
        }
        Expr::Block(b) => b.tail.as_ref().map(|e| guess_expr_ty(e)).unwrap_or(Ty::Void),
        Expr::Binary { op, lhs, rhs, .. } => {
            if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                Ty::I32
            } else if guess_expr_ty(lhs) == Ty::F64 || guess_expr_ty(rhs) == Ty::F64 {
                Ty::F64
            } else {
                Ty::I64
            }
        }
        _ => Ty::I64,
    }
}

struct FnLower<'a> {
    locals: Vec<Ty>,
    /// Source name of each local (parallel to `locals`; `None` for temporaries).
    /// Collected for debug builds → `DILocalVariable`/`#dbg_declare`.
    local_names: Vec<Option<String>>,
    blocks: Vec<BasicBlock>,
    cur: usize,
    scopes: Vec<HashMap<String, (Local, Ty)>>,
    sigs: &'a HashMap<String, Sig>,
    /// Declared syscalls (name → number + param shapes) for `syscall(NUM, …)`
    /// lowering at call sites (the former language/the former HW-SAFE.md (removed in cleanup; see git history) (removed in cleanup; see git history) §2).
    syscalls: &'a HashMap<String, SyscallInfo>,
    /// User type layouts (name → fields) for New/GetField.
    types: &'a HashMap<String, Layout>,
    /// (type, field) → element kind for array-typed struct fields, so
    /// `x.field[i]` can lower to a real bounds-checked array access.
    field_arr: &'a HashMap<(String, String), ArrKind>,
    /// Variant registry (variant name → info) for construction + match.
    variants: &'a HashMap<String, VariantInfo>,
    /// Generic functions (name → info) for call monomorphization.
    generics: &'a HashMap<String, GInfo>,
    /// Traits (name → methods) for dynamic dispatch (trait objects): a
    /// method call on a trait-typed receiver becomes `CallVirtual`.
    trait_methods: &'a HashMap<String, Vec<(String, String, Vec<Ty>, Ty)>>,
    /// Function ASTs (name → def) for higher-order inlining: when a lambda
    /// is passed, the called function expands inline at that spot
    /// (defunctionalization — direct, specialized code instead of a function pointer).
    fn_defs: &'a HashMap<String, FnDef>,
    /// Stack of the currently inline-expanded functions (recursion guard).
    inlining: Vec<String>,
    /// Generic product types (name → (type parameters, fields)) for `Box(x)`.
    generic_ptypes: &'a HashMap<String, (Vec<String>, Vec<Field>)>,
    /// Generic sum types (name → (type parameters, variants)) for `Some(x)`.
    generic_stypes: &'a HashMap<String, (Vec<String>, Vec<(String, Vec<(String, String)>)>)>,
    /// Variant → generic sum type (`Some` → `Option`) for construction/match.
    variant_owner_g: &'a HashMap<String, String>,
    /// Module-wide generic instances (annotation-driven): layout + variants.
    shared_inst: &'a HashMap<String, Layout>,
    shared_svars: &'a HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>>,
    /// Instantiated generic types of this function (mangled name → layout).
    /// Construction + annotated parameters fill this; the module registers
    /// the classes for the backend from it.
    local_inst: HashMap<String, Layout>,
    /// Payload-driven sum instances of this function (variant registry).
    local_svars: HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>>,
    /// Requested monomorph. instances: (generic name, concrete type arguments).
    mono: Vec<(String, Vec<String>)>,
    /// Class of a ref local (object local index → class name) for field access.
    local_class: HashMap<u32, String>,
    /// Element kind of an array/list local (for index/len/for-over-list).
    local_arr: HashMap<u32, ArrKind>,
    /// For a REF-element array local (`mut x: Array[Node] = array(n)`): the object
    /// class of its elements, so `x[i].field` resolves and `x[i] = e` is class-checked.
    local_arr_class: HashMap<u32, String>,
    /// Set while lowering a `mut x: Array[Cls] = …` RHS: the expected element object
    /// class, consumed once by the `array(n)` builtin to build a ref array of `Cls`.
    arr_hint: Option<String>,
    /// Expected `(T, E)` for an `Ok(x)`/`Err(e)` under construction, from the
    /// nearest annotation: a `mut r: Result[T, E] = …` let-annotation (one-shot,
    /// like `arr_hint`) or the enclosing function's `-> Result[T, E]` return
    /// type. Fills type parameters the payload cannot constrain (usually E for
    /// `Ok`, T for `Err`); anything still free is E-error, never silent `Int`.
    sum_hint: Option<(String, String)>,
    expected_result: Option<(String, String)>,
    /// MMIO handles (local → (base address, region length)) from `Mmio.bind("name")`
    /// (the former language/the former HW-SAFE.md (removed in cleanup; see git history) (removed in cleanup; see git history) §2). The handle value is an immortal descriptor object
    /// (plain Ref local, RC no-ops via refcount -1); the table drives `readN`/`writeN`
    /// lowering to volatile accesses at base+offset. Propagated through copies.
    mmio: HashMap<u32, (String, u64, u64)>,
    /// Opaque-pointer provenance (AGENTS.md §3, E421): locals proven to hold a `Ptr`
    /// that came from a checked source (`cstr`, `bview`-checked input, an extern call
    /// returning `Ptr`, a `Ptr` parameter, or the null literal `0`) — never a forged
    /// integer. Only these (plus literal `0`) may flow into `Ptr` sinks (extern
    /// params, `bview`, `as Ptr`). Propagated through copies; cleared on doubt.
    ptr_locals: std::collections::HashSet<u32>,
    /// True when this function's declared return annotation is `Ptr`: `return e`
    /// and the tail value must then be Ptr-proven (E421), so a forged integer
    /// cannot leave through a `Ptr` return either.
    ret_is_ptr: bool,
    /// Explicit-`region` nesting depth. While > 0, inferred loop-arenas do NOT
    /// fire (their unbounded push would shadow the declared bounded pool —
    /// the explicit region's own admission check subsumes the loop check).
    in_region: u32,
    /// Declared hardware regions (name → (base, len)) from `--hw-profile`, for
    /// resolving `Mmio.bind("name")` (E400 on unknown names).
    hw_regions: &'a HashMap<String, (u64, u64)>,
    /// Lambda locals: `mut f = x -> …` → (parameter, body). The call `f(a)` is
    /// inline-expanded at that spot (capturing closures in the same scope for free).
    local_lambda: HashMap<u32, (Vec<String>, Expr)>,
    /// Shared string literal pool (Program::strings); `intern` returns indices.
    strings: &'a mut Vec<String>,
    /// O(1) index into the string pool (literal → index) — otherwise `intern`
    /// would be a linear search = O(n²) compile time with many literals (scaling for large prog.).
    str_idx: &'a mut HashMap<String, u32>,
    errs: Vec<String>,
    /// Target blocks of the enclosing loops: (continue → header, break → exit).
    loops: Vec<(Block, Block)>,
    /// Source line-start offsets (for debug `DebugLine` markers); empty = no debug.
    line_starts: &'a [usize],
    /// Line of the last emitted `DebugLine` marker (0 = none), to avoid repeats.
    last_dbg_line: u32,
    /// Mangled name of the function being lowered (the innermost DebugLine frame).
    fn_name: String,
    /// Active `with log.span(…)` context: a compile-time stack of field-lists.
    /// Every `log.*` inside prepends the flattened fields; push/pop are markers
    /// consumed at lowering, so a span costs nothing at runtime.
    log_span: Vec<Vec<Expr>>,
}

impl<'a> FnLower<'a> {
    fn new_local(&mut self, ty: Ty) -> Local {
        self.locals.push(ty);
        self.local_names.push(None);
        Local((self.locals.len() - 1) as u32)
    }
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.str_idx.get(s) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.str_idx.insert(s.to_string(), i);
        self.strings.push(s.to_string());
        i
    }
    fn new_block(&mut self) -> Block {
        self.blocks.push(BasicBlock { statements: vec![], terminator: Terminator::Return(None) });
        Block((self.blocks.len() - 1) as u32)
    }
    fn emit(&mut self, s: Statement) {
        self.blocks[self.cur].statements.push(s);
    }
    fn term(&mut self, blk: usize, t: Terminator) {
        self.blocks[blk].terminator = t;
    }
    fn lookup(&self, name: &str) -> Option<(Local, Ty)> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return Some(*v);
            }
        }
        None
    }
    fn bind(&mut self, name: &str, l: Local, t: Ty) {
        // Remember the source name (first binding wins; shadowing keeps the
        // outer name for debug display, which is close enough for inspection).
        if let Some(slot) = self.local_names.get_mut(l.0 as usize) {
            if slot.is_none() && !name.is_empty() {
                *slot = Some(name.to_string());
            }
        }
        self.scopes.last_mut().unwrap().insert(name.to_string(), (l, t));
    }
    /// Layout of a class — user type OR instantiated generic type.
    fn layout_of(&self, class: &str) -> Option<Layout> {
        self.types
            .get(class)
            .or_else(|| self.local_inst.get(class))
            .or_else(|| self.shared_inst.get(class))
            .cloned()
    }
    /// Variant info (tag, field layout) of a variant IN a concrete
    /// (possibly generic) instance class. None → not in this instance.
    fn variant_in(&self, class: &str, variant: &str) -> Option<(String, i64, Vec<(String, Ty, Option<String>)>)> {
        let m = self.local_svars.get(class).or_else(|| self.shared_svars.get(class))?;
        let (tag, vf) = m.get(variant)?;
        Some((class.to_string(), *tag, vf.clone()))
    }
    /// Concrete type name of a lowered value (for type argument inference of
    /// generic constructors): class of a ref, otherwise scalar name.
    fn ty_name(&self, op: &Operand, ty: Ty) -> String {
        // A string literal IS a `Str` (immortal JStr) — not an opaque `Ref`.
        // This lets `Some("x")`/`Err("msg")` infer `Option[Str]`/`E = Str`
        // without annotation; anything else Ref stays opaque (sound).
        if matches!(op, Operand::ConstStr(_)) {
            return "Str".into();
        }
        if let Some(c) = self.class_of_operand(op) {
            return c;
        }
        match ty {
            Ty::F64 => "Float",
            Ty::F32 => "F32",
            Ty::I32 => "I32",
            Ty::Ref => "Ref",
            _ => "Int",
        }
        .to_string()
    }
    /// Instantiates a generic product type `base[targs]` → mangled class
    /// `base$targs`. Stores the (substituted) layout once in `local_inst`.
    fn instantiate_ptype(&mut self, base: &str, tparams: &[String], fields: &[Field], targs: &[String]) -> String {
        inst_ptype(base, tparams, fields, targs, &mut self.local_inst)
    }
    /// Class of an operand, if it is a ref local with a known class.
    fn class_of_operand(&self, op: &Operand) -> Option<String> {
        match op {
            Operand::Copy(l) => self.local_class.get(&l.0).cloned(),
            _ => None,
        }
    }
    /// Array element kind of an operand, if it is an array/list local.
    fn arr_of_operand(&self, op: &Operand) -> Option<ArrKind> {
        match op {
            Operand::Copy(l) => self.local_arr.get(&l.0).copied(),
            _ => None,
        }
    }
    /// Element object class of a ref-element array operand (`x` where `x[i]` is an object).
    fn arr_class_of_operand(&self, op: &Operand) -> Option<String> {
        match op {
            Operand::Copy(l) => self.local_arr_class.get(&l.0).cloned(),
            _ => None,
        }
    }
    /// MMIO handle region of an operand, if it is a bound handle local.
    fn mmio_of_operand(&self, op: &Operand) -> Option<(String, u64, u64)> {
        match op {
            Operand::Copy(l) => self.mmio.get(&l.0).cloned(),
            _ => None,
        }
    }
    /// Opaque-pointer provenance (AGENTS.md §3, E421): true when the operand is
    /// proven to be a checked pointer — a `Ptr`-proven local, or the null
    /// constant (`0` / `null`). Any other integer (a variable, a non-zero
    /// literal, arithmetic) is NOT a pointer, even though both are i64-wide.
    fn ptr_of_operand(&self, op: &Operand) -> bool {
        match op {
            Operand::Copy(l) => self.ptr_locals.contains(&l.0),
            Operand::ConstI64(0) => true,
            Operand::ConstI32(0) => true,
            Operand::ConstNull => true,
            _ => false,
        }
    }
    /// Record/clear `Ptr` provenance on a fresh local from its source operand.
    fn track_ptr(&mut self, dest: u32, op: &Operand) {
        if self.ptr_of_operand(op) {
            self.ptr_locals.insert(dest);
        } else {
            self.ptr_locals.remove(&dest);
        }
    }
    /// `Mmio.bind("name")` → immortal descriptor handle (`GetStatic $mmio`).
    /// E400 if the name is not in `--hw-profile` or the argument is not a
    /// string literal (the region must be statically known — never computed).
    fn lower_mmio_bind(&mut self, args: &[Expr]) -> (Operand, Ty) {
        let name = match (args.len(), args.first()) {
            (1, Some(Expr::Str(s, _))) => s.clone(),
            _ => {
                self.errs.push("error[E400]: `Mmio.bind` takes one string literal naming a `--hw-profile` region (e.g. `Mmio.bind(\"uart0\")`)".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        let Some((base, len)) = self.hw_regions.get(&name).copied() else {
            let mut known: Vec<&String> = self.hw_regions.keys().collect();
            known.sort();
            let hint = if known.is_empty() {
                "no `--hw-profile` was given (every bind is E400 without one)".to_string()
            } else {
                format!("known regions: {}", known.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))
            };
            self.errs.push(format!("error[E400]: unknown hardware region `{name}` — {hint}"));
            return (Operand::ConstI64(0), Ty::I64);
        };
        let d = self.new_local(Ty::Ref);
        self.emit(Statement::GetStatic { dest: d, class: "$mmio".into(), field: name.clone() });
        self.mmio.insert(d.0, (name, base, len));
        (Operand::Copy(d), Ty::Ref)
    }
    /// `dev.readN(off)` / `dev.writeN(off, v)` (N = 8/16/32/64) on a bound
    /// handle → calls to generated per-site volatile accessors
    /// (`__vire_mmio_r32_<base>_<off>`, emitted by the backend — no inline asm,
    /// no runtime). The offset must be an integer literal with
    /// `off + width <= len` (E400 otherwise — the address stays static).
    fn lower_mmio_access(&mut self, obj: Operand, name: &str, args: &[Expr]) -> (Operand, Ty) {
        let (region, base, len) = self.mmio_of_operand(&obj).expect("mmio handle checked by caller");
        let (width, is_write) = match name {
            "read8" => (1u64, false),
            "read16" => (2, false),
            "read32" => (4, false),
            "read64" => (8, false),
            "write8" => (1, true),
            "write16" => (2, true),
            "write32" => (4, true),
            "write64" => (8, true),
            _ => {
                self.errs.push(format!("error[E400]: `{name}` is not an MMIO operation (use read8/16/32/64, write8/16/32/64)"));
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        let arity = if is_write { 2 } else { 1 };
        if args.len() != arity {
            self.errs.push(format!("error[E400]: `{name}` takes {arity} argument(s), got {}", args.len()));
            return (Operand::ConstI64(0), Ty::I64);
        }
        let off = match &args[0] {
            Expr::Int(v, _) if *v >= 0 => *v as u64,
            _ => {
                self.errs.push(format!("error[E400]: `{name}` offset must be a non-negative integer literal (the address stays static, region `{region}`)"));
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        if off + width > len {
            self.errs.push(format!(
                "error[E400]: `{name}` at offset {off} + {width} bytes exceeds region `{region}` (length {len})"
            ));
            return (Operand::ConstI64(0), Ty::I64);
        }
        let dir = if is_write { "w" } else { "r" };
        let sym = format!("__vire_mmio_{dir}{}_{base:x}_{off:x}", width * 8);
        if is_write {
            let (vop, vty) = self.lower_expr(&args[1]);
            let v = if vty == Ty::I32 {
                let w = self.new_local(Ty::I64);
                self.emit(Statement::Assign(w, Rvalue::Convert(vop)));
                Operand::Copy(w)
            } else {
                vop
            };
            self.emit(Statement::Call { dest: None, func: sym, args: vec![v] });
            return (Operand::ConstI64(0), Ty::Void);
        }
        let d = self.new_local(Ty::I64);
        self.emit(Statement::Call { dest: Some(d), func: sym, args: vec![] });
        (Operand::Copy(d), Ty::I64)
    }
    /// Bring an operand to i32 (array index/length are Java `int`).
    fn to_i32(&mut self, op: Operand) -> Operand {
        match op {
            Operand::ConstI64(v) => Operand::ConstI32(v as i32),
            Operand::ConstI32(_) => op,
            other => {
                let d = self.new_local(Ty::I32);
                self.emit(Statement::Assign(d, Rvalue::Convert(other)));
                Operand::Copy(d)
            }
        }
    }
    /// Operand → String (Ref). Ref stays; scalars via jrt_*_to_str.
    fn to_str(&mut self, op: Operand, ty: Ty) -> Operand {
        if ty == Ty::Ref {
            return op;
        }
        let func = match ty {
            Ty::F64 => "jrt_double_to_str",
            Ty::F32 => "jrt_float_to_str",
            Ty::I32 => "jrt_int_to_str",
            _ => "jrt_long_to_str",
        };
        let arg = if ty == Ty::I64 { op } else if matches!(ty, Ty::F64 | Ty::F32 | Ty::I32) { op } else { to_i64(op) };
        let d = self.new_local(Ty::Ref);
        self.emit(Statement::Call { dest: Some(d), func: func.into(), args: vec![arg] });
        Operand::Copy(d)
    }
    /// ArrayLen (i32) → i64 operand for Vire (Ints are i64).
    fn array_len_i64(&mut self, arr: Operand) -> Operand {
        let li32 = self.new_local(Ty::I32);
        self.emit(Statement::ArrayLen { dest: li32, arr });
        let l64 = self.new_local(Ty::I64);
        self.emit(Statement::Assign(l64, Rvalue::Convert(Operand::Copy(li32))));
        Operand::Copy(l64)
    }

    /// i32 operand sign-extended to i64 (for mixed int arithmetic with
    /// packed `I32` fields). Constants directly, otherwise a `Convert` (sext).
    fn widen_i32(&mut self, op: Operand) -> Operand {
        if let Operand::ConstI32(v) = op {
            return Operand::ConstI64(v as i64);
        }
        let d = self.new_local(Ty::I64);
        self.emit(Statement::Assign(d, Rvalue::Convert(op)));
        Operand::Copy(d)
    }

    /// Inline a lambda body at the current position: bind each parameter to a
    /// fresh local holding the corresponding argument, lower the body, return its
    /// value. Mirrors the `local_lambda` call path — no closure object is made.
    /// Build the printable expression for a `log.LEVEL(...)` call. With a literal
    /// message containing `{}` placeholders and matching extra args, it is a **structured
    /// field** log: `log.info("user={} ms={}", id, t)` → `"[INFO] user=" + str(id) + " ms="
    /// + str(t)`, interpolated at compile time (no parser change; positional args, so the
    /// disabled-call = zero-cost property is preserved). A placeholder/arg count mismatch
    /// is a compile error. A non-literal message is printed as-is (no tag). Returns `None`
    /// on error (a diagnostic was pushed).
    /// Compile-time prefix for the active `with log.span(…)` context, e.g.
    /// `[req=<id> user=<name>] `. `None` when no span is active. Field keys that
    /// are string literals print bare; values go through `str(…)`.
    fn span_prefix(&self, sp: crate::diag::Span) -> Option<Expr> {
        use crate::ast::BinOp;
        let fields: Vec<&Expr> = self.log_span.iter().flatten().collect();
        if fields.is_empty() {
            return None;
        }
        let add = |lhs: Expr, rhs: Expr| Expr::Binary { op: BinOp::Add, lhs: Box::new(lhs), rhs: Box::new(rhs), span: sp };
        let str_of = |e: &Expr| Expr::Call { callee: Box::new(Expr::Ident("str".into(), sp)), args: vec![e.clone()], span: sp };
        let mut e = Expr::Str("[".into(), sp);
        let mut i = 0;
        while i < fields.len() {
            if i > 0 {
                e = add(e, Expr::Str(" ".into(), sp));
            }
            let key = if let Expr::Str(k, _) = fields[i] { Expr::Str(k.clone(), sp) } else { str_of(fields[i]) };
            e = add(e, key);
            if i + 1 < fields.len() {
                e = add(e, Expr::Str("=".into(), sp));
                e = add(e, str_of(fields[i + 1]));
            }
            i += 2;
        }
        Some(add(e, Expr::Str("] ".into(), sp)))
    }

    fn build_log_message(&mut self, level: &str, tag: &str, msg: &Expr, extra: &[Expr], span: crate::diag::Span) -> Option<Expr> {
        use crate::ast::BinOp;
        let Expr::Str(s, sp) = msg else {
            // Non-literal message (e.g. a built string): emit it verbatim.
            if !extra.is_empty() {
                self.errs.push(format!("log.{level}: structured fields need a literal message with `{{}}`"));
                return None;
            }
            return Some(msg.clone());
        };
        let add = |lhs: Expr, rhs: Expr| Expr::Binary { op: BinOp::Add, lhs: Box::new(lhs), rhs: Box::new(rhs), span: *sp };
        let prefix = self.span_prefix(*sp);
        // Console sink: color the level tag when the build opted in (`--log-color`).
        let ctag = colorize_tag(level, tag);
        // `[LEVEL] ` + the span context (if any) + `rest`.
        let head = |rest: Expr| -> Expr {
            match &prefix {
                Some(pfx) => add(add(Expr::Str(ctag.clone(), *sp), pfx.clone()), rest),
                None => add(Expr::Str(ctag.clone(), *sp), rest),
            }
        };
        if !s.contains("{}") {
            if !extra.is_empty() {
                self.errs.push(format!("log.{level}: {} extra argument(s) but no `{{}}` in the message", extra.len()));
                return None;
            }
            // No span → keep the compile-time-concatenated literal (zero runtime cost).
            if prefix.is_none() {
                return Some(Expr::Str(format!("{ctag}{s}"), *sp));
            }
            return Some(head(Expr::Str(s.clone(), *sp)));
        }
        let segs: Vec<&str> = s.split("{}").collect();
        let nph = segs.len() - 1;
        if nph != extra.len() {
            self.errs.push(format!("log.{level}: {nph} `{{}}` placeholder(s) but {} argument(s)", extra.len()));
            return None;
        }
        // tag [+ span prefix] + seg0 + str(a0) + seg1 + str(a1) + …
        let mut e = head(Expr::Str(segs[0].to_string(), *sp));
        for (i, a) in extra.iter().enumerate() {
            let stra = Expr::Call { callee: Box::new(Expr::Ident("str".into(), *sp)), args: vec![a.clone()], span: *sp };
            e = add(e, stra);
            if !segs[i + 1].is_empty() {
                e = add(e, Expr::Str(segs[i + 1].to_string(), *sp));
            }
        }
        let _ = span;
        Some(e)
    }

    fn apply_lambda(&mut self, params: &[String], body: &Expr, args: &[(Operand, Ty)]) -> (Operand, Ty) {
        self.scopes.push(HashMap::new());
        for (p, (op, ty)) in params.iter().zip(args) {
            let d = self.new_local(*ty);
            if let Some(c) = self.class_of_operand(op) {
                self.local_class.insert(d.0, c);
            }
            self.emit(Statement::Assign(d, Rvalue::Use(op.clone())));
            self.bind(p, d, *ty);
        }
        let r = self.lower_expr(body);
        self.scopes.pop();
        r
    }

    /// Iterator adapters (`fold`/`sum`/`count`/`each`/`map`/`filter`) over a range
    /// or a `$List`. The lambda body is inlined per element into a generated
    /// counting loop — there is no closure object, so LLVM optimizes the fused
    /// loop like hand-written code. Returns `None` if `name` is not an adapter (the
    /// caller then falls through to ordinary method dispatch). Elements are `Int`
    /// (i64): a list stores i64 slots, a range counts i64.
    fn lower_iter_adapter(&mut self, src: IterSrc, name: &str, args: &[Expr]) -> Option<(Operand, Ty)> {
        if !matches!(name, "fold" | "sum" | "count" | "each" | "forEach" | "map" | "filter") {
            return None;
        }
        // Validate the lambda argument up-front (adapters that take one).
        let lam = |e: &Expr| -> Option<(Vec<String>, Expr)> {
            if let Expr::Lambda { params, body, .. } = e { Some((params.clone(), (**body).clone())) } else { None }
        };
        let needs_lambda = matches!(name, "each" | "forEach" | "map" | "filter");
        if needs_lambda && args.first().and_then(&lam).is_none() {
            self.errs.push(format!("`{name}` expects a lambda argument, e.g. `.{name}(x -> …)`"));
            return Some((Operand::ConstI64(0), Ty::I64));
        }
        if name == "fold" && (args.len() < 2 || lam(&args[1]).is_none()) {
            self.errs.push("`fold` expects `(init, (acc, x) -> …)`".into());
            return Some((Operand::ConstI64(0), Ty::I64));
        }

        // --- Pre-loop: bounds + accumulator in the current block ---
        let ivar = self.new_local(Ty::I64);
        let (init_i, end_op, incl) = match &src {
            IterSrc::Range { start, end, incl } => (to_i64(start.clone()), to_i64(end.clone()), *incl),
            IterSrc::List(obj) => {
                let len = self.new_local(Ty::I64);
                self.emit(Statement::Call { dest: Some(len), func: "vire_list_len".into(), args: vec![obj.clone()] });
                (Operand::ConstI64(0), Operand::Copy(len), false)
            }
        };
        self.emit(Statement::Assign(ivar, Rvalue::Use(init_i)));

        // fold → init; sum/count → 0; map/filter → new $List; each → no accumulator.
        let acc: Option<Local> = match name {
            "fold" => {
                let (initop, initty) = self.lower_expr(&args[0]);
                let a = self.new_local(initty);
                if let Some(c) = self.class_of_operand(&initop) { self.local_class.insert(a.0, c); }
                self.emit(Statement::Assign(a, Rvalue::Use(initop)));
                Some(a)
            }
            "sum" | "count" => {
                let a = self.new_local(Ty::I64);
                self.emit(Statement::Assign(a, Rvalue::Use(Operand::ConstI64(0))));
                Some(a)
            }
            "map" | "filter" => {
                let a = self.new_local(Ty::Ref);
                self.local_class.insert(a.0, "$List".into());
                self.emit(Statement::Call { dest: Some(a), func: "vire_list_new".into(), args: vec![] });
                Some(a)
            }
            _ => None,
        };
        let acc_ty = acc.map(|a| self.locals[a.0 as usize]);

        // --- Loop skeleton (mirrors the numeric `for` lowering) ---
        let header = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Goto(header));
        self.cur = header.0 as usize;
        let cond = self.new_local(Ty::I32);
        let cmp = if incl { IB::CmpLe } else { IB::CmpLt };
        self.emit(Statement::Assign(cond, Rvalue::Binary(cmp, Operand::Copy(ivar), end_op)));
        let bodyb = self.new_block();
        let latch = self.new_block();
        let exit = self.new_block();
        self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(cond), then_blk: bodyb, else_blk: exit });

        // --- Body: element + adapter action ---
        self.cur = bodyb.0 as usize;
        let elem: Operand = match &src {
            IterSrc::Range { .. } => Operand::Copy(ivar),
            IterSrc::List(obj) => {
                let e = self.new_local(Ty::I64);
                self.emit(Statement::Call { dest: Some(e), func: "vire_list_get".into(), args: vec![obj.clone(), Operand::Copy(ivar)] });
                Operand::Copy(e)
            }
        };
        match name {
            "sum" => {
                let a = acc.unwrap();
                self.emit(Statement::Assign(a, Rvalue::Binary(IB::Add, Operand::Copy(a), elem)));
            }
            "count" => {
                let a = acc.unwrap();
                self.emit(Statement::Assign(a, Rvalue::Binary(IB::Add, Operand::Copy(a), Operand::ConstI64(1))));
            }
            "each" | "forEach" => {
                let (params, body) = lam(&args[0]).unwrap();
                self.apply_lambda(&params, &body, &[(elem, Ty::I64)]);
            }
            "fold" => {
                let a = acc.unwrap();
                let at = acc_ty.unwrap();
                let (params, body) = lam(&args[1]).unwrap();
                let (r, _) = self.apply_lambda(&params, &body, &[(Operand::Copy(a), at), (elem, Ty::I64)]);
                // apply_lambda may have advanced self.cur (control flow in the body);
                // the assignment lands in whatever block the value is live in.
                let rr = if at == Ty::I64 { to_i64(r) } else { r };
                self.emit(Statement::Assign(a, Rvalue::Use(rr)));
            }
            "map" => {
                let a = acc.unwrap();
                let (params, body) = lam(&args[0]).unwrap();
                let (r, _) = self.apply_lambda(&params, &body, &[(elem, Ty::I64)]);
                self.emit(Statement::Call { dest: None, func: "vire_list_push".into(), args: vec![Operand::Copy(a), to_i64(r)] });
            }
            "filter" => {
                let a = acc.unwrap();
                let (params, body) = lam(&args[0]).unwrap();
                // Hold the element in a stable local across the predicate's blocks.
                let el = self.new_local(Ty::I64);
                self.emit(Statement::Assign(el, Rvalue::Use(elem)));
                let (pred, _) = self.apply_lambda(&params, &body, &[(Operand::Copy(el), Ty::I64)]);
                let c = self.new_local(Ty::I32);
                self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpNe, to_i64(pred), Operand::ConstI64(0))));
                let push_blk = self.new_block();
                let after = self.new_block();
                self.term(self.cur, Terminator::Branch { cond: Operand::Copy(c), then_blk: push_blk, else_blk: after });
                self.cur = push_blk.0 as usize;
                self.emit(Statement::Call { dest: None, func: "vire_list_push".into(), args: vec![Operand::Copy(a), Operand::Copy(el)] });
                self.term(push_blk.0 as usize, Terminator::Goto(after));
                self.cur = after.0 as usize;
            }
            _ => unreachable!(),
        }
        // Body (which may now end in a later block) → latch → header.
        let bend = self.cur;
        self.term(bend, Terminator::Goto(latch));
        self.cur = latch.0 as usize;
        self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
        self.term(latch.0 as usize, Terminator::Goto(header));
        self.cur = exit.0 as usize;

        Some(match acc {
            Some(a) => (Operand::Copy(a), self.locals[a.0 as usize]),
            None => (Operand::ConstI64(0), Ty::Void),
        })
    }

    fn lower_block(&mut self, b: &Block2) {
        let _ = self.lower_block_val(b); // Void context: tail value discarded
    }

    /// Name whose call creates a heap object (constructor of a user/
    /// generic type, variant, or collection builtin).
    fn is_alloc_name(&self, n: &str) -> bool {
        self.types.contains_key(n)
            || self.variants.contains_key(n)
            || self.generic_ptypes.contains_key(n)
            || self.variant_owner_g.contains_key(n)
            || matches!(n, "list" | "map" | "set" | "array" | "farray" | "barray" | "str_from" | "find_byte" | "bview" | "bview_len")
    }

    /// AUTOMATIC LOOP ARENA (escape→arena). A `while` iteration whose
    /// allocations provably do NOT leave the iteration is placed into a
    /// per-iteration bump arena (like an automatic capsule): no
    /// malloc/free per node, en-bloc release at the end of the iteration. Hits the
    /// ONLY measured gap (allocator; btree 2.57× ceiling). Conservative —
    /// any uncertainty ⇒ do not promote (no arena ⇒ no unsoundness).
    ///
    /// Safe if the body (transitively over user callees):
    ///  - allocates (otherwise no benefit),
    ///  - writes NO field/index a *reference* (mutating an existing object with a
    ///    ref could store an arena reference to the outside; constructors do NOT
    ///    count as field writes — they are calls on fresh objects; a scalar store
    ///    can never leak an arena pointer),
    ///  - calls NO mutator method (push/put/set/pop/add/insert),
    ///  - performs NO `return`/`break`/`continue` that leaves the ARENA ITERATION
    ///    (a control-flow exit that skips the en-bloc `jrt_arena_pop`),
    ///  - only calls user functions + constructors (no extern/builtin/lambda —
    ///    could capture a reference),
    ///  - assigns no ref to an outer (cross-iteration) variable.
    ///
    /// INTERPROCEDURAL PRECISION. The badness check recurses transitively through
    /// user callees. Two context flags decide whether a control-flow statement
    /// escapes the arena, so a callee's own `return`/`break`/`continue` does NOT
    /// disqualify the arena — it returns to a caller that is still *inside* the
    /// arena's dynamic extent (the arena is a thread-local `arena_top`, so every
    /// allocation the iteration transitively performs lands in it and is freed en
    /// bloc at the pop). This is what lets a factory→consume pattern (e.g. an AST
    /// `parse()` handed to an `eval()` that uses `return`) run allocation-free:
    ///  - `in_callee`: we are inside a called function's body, not the loop's own
    ///    function. A `return` here is fine (control comes back within the arena);
    ///    a `return` in the loop's own function bypasses the pop → forbidden.
    ///    Outer-variable rebinding is only meaningful in the loop's own function
    ///    (a callee cannot reach the loop function's locals — Vire has no closures
    ///    capturing mutable outer state, and no mutable globals).
    ///  - `in_loop`: we are inside a *nested* loop within the loop's own function.
    ///    A `break`/`continue` there targets the nested loop, not our arena loop,
    ///    so it does not skip our pop; at the arena-loop level it does → forbidden.
    /// A ref-storing field mutation `obj.f = ref` is forbidden in callees and,
    /// in the loop's own function, allowed ONLY when both `obj` and the stored `ref`
    /// are provably iteration-fresh (both allocated into — and freed with — this
    /// iteration's arena): the store then keeps an arena ref inside an arena object,
    /// so nothing outlives the pop. See `loop_fresh_locals` / `expr_is_fresh`.
    /// A ref store whose base is not proven fresh, or any ref store inside a callee,
    /// stays forbidden (the one way an arena object could outlive the arena).
    fn while_arena_safe(&self, body: &Block2) -> bool {
        let mut outer: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in &self.scopes {
            for k in s.keys() {
                outer.insert(k.clone());
            }
        }
        // Iteration-fresh locals — the domain in which a ref-storing field mutation
        // can be admitted soundly. Seed empty (no fresh params in the loop's own
        // function); callee descents re-seed with the callee's fresh parameters.
        let fresh = self.region_fresh_locals(body, &std::collections::HashSet::new(), None);
        let mut seen = std::collections::HashSet::new();
        if self.region_bad_block(body, false, false, None, &outer, &mut seen, &fresh) {
            return false;
        }
        seen.clear();
        self.region_allocates_block(body, &mut seen)
    }

    /// Names that are provably iteration-fresh in a body: every assignment to them
    /// comes from an arena-local source (a constructor / a call whose arguments are all
    /// fresh / a field-read of a fresh object / `null` / a scalar / a string concat,
    /// which allocates into the active arena). `seed` are names fresh on entry — empty
    /// for the loop's own function, the callee's fresh ref parameters for a callee body
    /// (their arguments were proven fresh at the call site). `cur_fn` is the scope for
    /// scalarness. A greatest fixpoint: assume all seeded+assigned names fresh, then
    /// drop any whose right-hand side is not fresh until stable. Non-fresh ground
    /// sources (an opaque call, an uninitialised `mut`) propagate and remove names
    /// transitively. Sound direction: a name stays fresh only if EVERY reaching
    /// definition is fresh (a not-reassigned seed param stays fresh).
    fn region_fresh_locals(&self, body: &Block2, seed: &std::collections::HashSet<String>, cur_fn: Option<&FnDef>) -> std::collections::HashSet<String> {
        let mut assigns: Vec<(String, Option<&Expr>)> = Vec::new();
        self.collect_fresh_assigns_block(body, &mut assigns);
        let mut fresh: std::collections::HashSet<String> = seed.iter().cloned().collect();
        fresh.extend(assigns.iter().map(|(n, _)| n.clone()));
        loop {
            let mut changed = false;
            for (name, rhs) in &assigns {
                if fresh.contains(name) {
                    let ok = rhs.map(|e| self.expr_is_fresh(e, &fresh, cur_fn)).unwrap_or(false);
                    if !ok {
                        fresh.remove(name);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        fresh
    }

    fn collect_fresh_assigns_block<'e>(&self, b: &'e Block2, out: &mut Vec<(String, Option<&'e Expr>)>) {
        for s in &b.stmts {
            self.collect_fresh_assigns_stmt(s, out);
        }
        if let Some(e) = b.tail.as_deref() {
            self.collect_fresh_assigns_expr(e, out);
        }
    }

    fn collect_fresh_assigns_stmt<'e>(&self, s: &'e Stmt, out: &mut Vec<(String, Option<&'e Expr>)>) {
        match s {
            // `mut x = e` / `x = e` (rebind): the target binds to `e`. An uninitialised
            // `mut x` (value None) records None → treated as non-fresh.
            Stmt::Let { name, value, .. } => out.push((name.clone(), value.as_ref())),
            // A plain `name = e` rebinds; a compound `name op= e` (op = Some) is recorded
            // as None (non-fresh: op= on a ref is a string concat, and the base is scalar
            // in every other case — either way we do not depend on it being fresh).
            Stmt::Assign { target: Expr::Ident(n, _), op, value, .. } => {
                out.push((n.clone(), if op.is_none() { Some(value) } else { None }));
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => self.collect_fresh_assigns_block(body, out),
            Stmt::Expr(e) | Stmt::Return(Some(e), _) => self.collect_fresh_assigns_expr(e, out),
            _ => {}
        }
    }

    fn collect_fresh_assigns_expr<'e>(&self, e: &'e Expr, out: &mut Vec<(String, Option<&'e Expr>)>) {
        match e {
            Expr::If { then, elifs, els, .. } => {
                self.collect_fresh_assigns_block(then, out);
                for (_, b) in elifs {
                    self.collect_fresh_assigns_block(b, out);
                }
                if let Some(b) = els {
                    self.collect_fresh_assigns_block(b, out);
                }
            }
            Expr::Match { arms, .. } => {
                for (_, _, b) in arms {
                    self.collect_fresh_assigns_expr(b, out);
                }
            }
            Expr::Block(b) => self.collect_fresh_assigns_block(b, out),
            _ => {}
        }
    }

    /// Is `e` an iteration-fresh (arena-local) value, or a scalar/null that can never
    /// dangle? Used both for the mutation base/value check and inside the freshness
    /// fixpoint. The invariant it maintains: an arena object's ref fields only ever
    /// hold fresh refs, `null`, or immortal literals — so a field-read of a fresh base
    /// is itself fresh, and a constructor/call yields fresh memory when its arguments
    /// are all fresh (a callee that returns an argument returns a fresh one).
    fn expr_is_fresh(&self, e: &Expr, fresh: &std::collections::HashSet<String>, cur_fn: Option<&FnDef>) -> bool {
        match e {
            // Scalars cannot be a dangling ref; a string literal is immortal (never
            // freed, so no dangle); `a + b` allocates into the active arena (or is
            // scalar arithmetic) → arena-local either way.
            Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Char(..) | Expr::Str(..)
            | Expr::Unary { .. } | Expr::Binary { .. } => true,
            Expr::Ident(n, _) if n == "null" && self.lookup(n).is_none() => true,
            // A fresh local/param, or any scalar-typed name (a scalar cannot dangle).
            // `cur_fn` selects the scope for scalarness: the loop's own function (None →
            // live scope) or a callee (Some(fd) → its parameter annotations).
            Expr::Ident(n, _) => fresh.contains(n) || !self.region_name_is_ref(n, cur_fn),
            Expr::Field { base, .. } => self.expr_is_fresh(base, fresh, cur_fn),
            // A slot read `a[i]` of a fresh array is fresh: inside a promoted arena every
            // ref store into `a` had a fresh value (the index-store admission above), so
            // the array's ref slots only hold fresh refs — reading one yields a fresh ref.
            // (Scalar reads are trivially non-dangling.) Symmetric with the field-read case.
            Expr::Index { base, .. } => self.expr_is_fresh(base, fresh, cur_fn),
            // Constructor / known function: the result is arena-local as long as every
            // argument is fresh (a callee that returns an argument returns a fresh one).
            // Relies on Vire having NO global/static ref state (0 GetStatic/PutStatic
            // emitted, consts are comptime scalars): a function can only return a fresh
            // allocation, one of its arguments, or an immortal literal — never a
            // pre-existing non-arena ref. If mutable global refs are ever added, this
            // branch must also require the callee to be proven not to return one.
            Expr::Call { callee, args, .. } => match callee.as_ref() {
                Expr::Ident(n, _) if self.is_alloc_name(n) || self.fn_defs.contains_key(n) => {
                    args.iter().all(|a| self.expr_is_fresh(a, fresh, cur_fn))
                }
                _ => false,
            },
            _ => false,
        }
    }

    // `cur_fn`: the function whose body we are traversing — `None` for the loop's
    // own function (resolve names via the live scope), `Some(fd)` inside a callee
    // (resolve names via the callee's parameter annotations; a callee-local name is
    // unknown → conservatively a ref). Used so `p[0] = p[0] + 1` on an `Array[Int]`
    // parameter is seen as the scalar store it is, not a possible ref store.
    fn region_bad_block(&self, b: &Block2, in_callee: bool, in_loop: bool, cur_fn: Option<&FnDef>, outer: &std::collections::HashSet<String>, seen: &mut std::collections::HashSet<String>, fresh: &std::collections::HashSet<String>) -> bool {
        b.stmts.iter().any(|s| self.region_bad_stmt(s, in_callee, in_loop, cur_fn, outer, seen, fresh))
            || b.tail.as_deref().map(|e| self.region_bad_expr(e, in_callee, in_loop, cur_fn, outer, seen, fresh)).unwrap_or(false)
    }

    fn region_bad_stmt(&self, s: &Stmt, in_callee: bool, in_loop: bool, cur_fn: Option<&FnDef>, outer: &std::collections::HashSet<String>, seen: &mut std::collections::HashSet<String>, fresh: &std::collections::HashSet<String>) -> bool {
        match s {
            // A `return` in the loop's OWN function bypasses the en-bloc pop (and
            // may hand an arena ref to the caller) → forbidden. A `return` inside a
            // callee returns to a caller still within the arena's dynamic extent →
            // safe (the returned value's fate is re-checked at the call site).
            Stmt::Return(..) => !in_callee,
            // `break`/`continue` skip our pop only when they target the arena loop
            // itself: i.e. in the loop's own function and not inside a nested loop.
            Stmt::Break(_) | Stmt::Continue(_) => !in_callee && !in_loop,
            // `name = expr` is a Let (rebinding) in the Vire AST. If it re-binds an
            // OUTER variable (declared before the loop) with a ref, the ref escapes
            // beyond the iteration → forbidden. New body-local names
            // (not in `outer`) are harmless (they die with the iteration). Only
            // meaningful in the loop's own function — a callee cannot reach the loop
            // function's locals (no closures / mutable globals).
            Stmt::Let { name, value, .. } => {
                // Rebinding an OUTER var with a ref escapes the iteration — unless the
                // outer var is scalar-typed (no ref fits) or the value is provably scalar.
                let outer_is_ref = self.lookup(name).map(|(_, t)| t == Ty::Ref).unwrap_or(true);
                let escapes = !in_callee
                    && outer.contains(name)
                    && outer_is_ref
                    && value.as_ref().map(|v| self.expr_may_be_ref(v, cur_fn)).unwrap_or(false);
                escapes || value.as_ref().map(|e| self.region_bad_expr(e, in_callee, in_loop, cur_fn, outer, seen, fresh)).unwrap_or(false)
            }
            Stmt::Assign { target, value, .. } => {
                let target_bad = match target {
                    // Index mutation `a[i] = v` can only make an arena object outlive the
                    // arena if it stores a REFERENCE. Two independent proofs of "no ref
                    // stored", either sufficient: (a) the array's element kind is a scalar
                    // — a ref cannot be stored into an `Array[Int]` slot at all (the type
                    // checker forbids it), regardless of `v`; or (b) `v` is provably scalar.
                    // Both are base-insensitive → also fire inside callees (the one
                    // relaxation-proof leak route: a heap ref stored into a long-lived
                    // container). `region_index_scalar`/`expr_may_be_ref` are conservative.
                    // THIRD proof (the freshness relaxation, symmetric with the field case
                    // below): `a[i] = ref` is admitted when the array `a` AND the stored
                    // `ref` are both provably iteration-fresh — then both live in this
                    // iteration's arena and are freed en bloc at the pop, so the slot never
                    // holds a ref that outlives the arena and no non-arena ref is captured.
                    Expr::Index { base, .. } => {
                        !self.region_index_scalar(base, cur_fn)
                            && self.expr_may_be_ref(value, cur_fn)
                            && !(self.expr_is_fresh(base, fresh, cur_fn)
                                && self.expr_is_fresh(value, fresh, cur_fn))
                    }
                    // Field mutation `obj.f = v`. A scalar `v` cannot leak a ref. A ref `v`
                    // is admitted when the mutated object `base` AND the stored value are
                    // both provably iteration-fresh: then both live in this iteration's
                    // arena and are freed together at the pop, so nothing outlives it (no
                    // dangle) and no non-arena ref is captured (no missing release → no
                    // leak). This holds in the loop's own function (`fresh` = the loop's
                    // fresh locals) and inside a callee whose arguments were proven fresh
                    // at the call site (`fresh` = the callee's fresh params + locals);
                    // in a non-fresh-args callee descent `fresh` is empty, so nothing is
                    // admitted. `in_callee` no longer gates it — the `fresh` set does.
                    Expr::Field { base, .. } => {
                        self.expr_may_be_ref(value, cur_fn)
                            && !(self.expr_is_fresh(base, fresh, cur_fn)
                                && self.expr_is_fresh(value, fresh, cur_fn))
                    }
                    // Ref to an outer variable (compound `x op= e`) → escapes. Safe if the
                    // outer variable is scalar-typed (a ref cannot be stored into an I64/F64
                    // slot) or the written value is provably scalar. Loop-function only.
                    Expr::Ident(n, _) => {
                        !in_callee
                            && outer.contains(n)
                            && self.lookup(n).map(|(_, t)| t == Ty::Ref).unwrap_or(true)
                            && self.expr_may_be_ref(value, cur_fn)
                    }
                    _ => false,
                };
                target_bad || self.region_bad_expr(value, in_callee, in_loop, cur_fn, outer, seen, fresh)
            }
            Stmt::Expr(e) => self.region_bad_expr(e, in_callee, in_loop, cur_fn, outer, seen, fresh),
            // A nested loop within the SAME function: its body runs at `in_loop = true`
            // so a `break`/`continue` there targets it, not our arena loop.
            Stmt::While { cond, body, .. } => self.region_bad_expr(cond, in_callee, in_loop, cur_fn, outer, seen, fresh) || self.region_bad_block(body, in_callee, true, cur_fn, outer, seen, fresh),
            Stmt::For { iter, body, .. } => self.region_bad_expr(iter, in_callee, in_loop, cur_fn, outer, seen, fresh) || self.region_bad_block(body, in_callee, true, cur_fn, outer, seen, fresh),
        }
    }

    fn region_bad_expr(&self, e: &Expr, in_callee: bool, in_loop: bool, cur_fn: Option<&FnDef>, outer: &std::collections::HashSet<String>, seen: &mut std::collections::HashSet<String>, fresh: &std::collections::HashSet<String>) -> bool {
        match e {
            Expr::Call { callee, args, .. } => {
                let callee_bad = match callee.as_ref() {
                    Expr::Ident(n, _) => {
                        // A `dynamic fn` / `open fn` is an extension SEAM (the former DYNAMIC-VIRE-PLAN.md (removed in cleanup; see git history)
                        // §2/§6): a runtime override could store its args or escape, so it is a
                        // hard black box to region inference — treat it as opaque, exactly like
                        // an extern. This is what makes the arena decline at a seam (the sealed-
                        // by-default performance/soundness boundary). Sealed callees are still
                        // descended into transparently below.
                        if self.fn_defs.get(n).is_some_and(|fd| fd.attrs.iter().any(|a| a.name == "__dynamic")) {
                            true
                        } else if let Some(fd) = self.fn_defs.get(n) {
                            // Are ALL arguments fresh at THIS call site? Then the callee's
                            // ref parameters are arena-local, and it may mutate their graphs
                            // (a fresh ref stored into a fresh param stays in the arena).
                            // Only when the arg/param arity matches (no defaults in play).
                            let args_fresh = args.len() == fd.sig.params.len()
                                && args.iter().all(|a| self.expr_is_fresh(a, fresh, cur_fn));
                            // Key `seen` by (callee, args_fresh): the fresh and conservative
                            // descents are distinct checks, and each terminates recursion.
                            let key = format!("{n}#{}", args_fresh as u8);
                            if seen.insert(key) {
                                match &fd.body {
                                    // Descend into the callee: `in_callee = true` (its
                                    // return/break/continue no longer disqualify the arena),
                                    // `in_loop = false`, `cur_fn = Some(fd)` (names via ITS
                                    // params). `fresh` = the callee's fresh params + locals
                                    // when the args were proven fresh, else empty (ref-field
                                    // mutation then stays conservatively forbidden there).
                                    Some(b) => {
                                        let callee_fresh = if args_fresh {
                                            let seed: std::collections::HashSet<String> = fd
                                                .sig
                                                .params
                                                .iter()
                                                .filter(|p| ty_of(p.ty.as_ref()) == Ty::Ref)
                                                .map(|p| p.name.clone())
                                                .collect();
                                            self.region_fresh_locals(b, &seed, Some(fd))
                                        } else {
                                            std::collections::HashSet::new()
                                        };
                                        self.region_bad_block(b, true, false, Some(fd), outer, seen, &callee_fresh)
                                    }
                                    None => true, // only signature → opaque
                                }
                            } else {
                                false // already being checked (recursion) → ok for the cycle
                            }
                        } else if self.is_alloc_name(n) {
                            false // constructor of a fresh object — allowed
                        } else {
                            true // builtin/extern/unknown → conservatively opaque
                        }
                    }
                    // Mutator method on a (possibly outer) object → could store.
                    _ => true,
                };
                callee_bad || args.iter().any(|a| self.region_bad_expr(a, in_callee, in_loop, cur_fn, outer, seen, fresh))
            }
            Expr::Unary { rhs, .. } => self.region_bad_expr(rhs, in_callee, in_loop, cur_fn, outer, seen, fresh),
            Expr::Binary { lhs, rhs, .. } => self.region_bad_expr(lhs, in_callee, in_loop, cur_fn, outer, seen, fresh) || self.region_bad_expr(rhs, in_callee, in_loop, cur_fn, outer, seen, fresh),
            Expr::Field { base, .. } => self.region_bad_expr(base, in_callee, in_loop, cur_fn, outer, seen, fresh),
            Expr::Index { base, index, .. } => self.region_bad_expr(base, in_callee, in_loop, cur_fn, outer, seen, fresh) || self.region_bad_expr(index, in_callee, in_loop, cur_fn, outer, seen, fresh),
            Expr::If { cond, then, elifs, els, .. } => {
                self.region_bad_expr(cond, in_callee, in_loop, cur_fn, outer, seen, fresh)
                    || self.region_bad_block(then, in_callee, in_loop, cur_fn, outer, seen, fresh)
                    || elifs.iter().any(|(c, b)| self.region_bad_expr(c, in_callee, in_loop, cur_fn, outer, seen, fresh) || self.region_bad_block(b, in_callee, in_loop, cur_fn, outer, seen, fresh))
                    || els.as_ref().map(|b| self.region_bad_block(b, in_callee, in_loop, cur_fn, outer, seen, fresh)).unwrap_or(false)
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.region_bad_expr(scrutinee, in_callee, in_loop, cur_fn, outer, seen, fresh)
                    || arms.iter().any(|(_, g, b)| g.as_ref().map(|g| self.region_bad_expr(g, in_callee, in_loop, cur_fn, outer, seen, fresh)).unwrap_or(false) || self.region_bad_expr(b, in_callee, in_loop, cur_fn, outer, seen, fresh))
            }
            Expr::Block(b) => self.region_bad_block(b, in_callee, in_loop, cur_fn, outer, seen, fresh),
            Expr::List(xs, _) => xs.iter().any(|x| self.region_bad_expr(x, in_callee, in_loop, cur_fn, outer, seen, fresh)),
            Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => self.region_bad_expr(inner, in_callee, in_loop, cur_fn, outer, seen, fresh),
            Expr::Range { start, end, .. } => self.region_bad_expr(start, in_callee, in_loop, cur_fn, outer, seen, fresh) || self.region_bad_expr(end, in_callee, in_loop, cur_fn, outer, seen, fresh),
            // Lambda/Comprehension/MapLit: conservatively opaque (could capture/store outside).
            Expr::Lambda { .. } | Expr::Comprehension { .. } | Expr::MapLit(..) => true,
            // Capsule: precise, not opaque. Inputs are deep-COPIED (read-only —
            // never a store into region objects), so only the body can be bad,
            // checked with the same outer/fresh sets as a nested block (a body
            // store of an outer ref into a region object, or a boundary-crossing
            // return/break/continue, is still rejected; the arena-contained
            // mutations of the input copies are admitted).
            Expr::Capsule { body, .. } => self.region_bad_block(body, in_callee, in_loop, cur_fn, outer, seen, fresh),
            _ => false,
        }
    }

    fn region_allocates_block(&self, b: &Block2, seen: &mut std::collections::HashSet<String>) -> bool {
        b.stmts.iter().any(|s| self.region_allocates_stmt(s, seen)) || b.tail.as_deref().map(|e| self.region_allocates_expr(e, seen)).unwrap_or(false)
    }
    fn region_allocates_stmt(&self, s: &Stmt, seen: &mut std::collections::HashSet<String>) -> bool {
        match s {
            Stmt::Let { value, .. } => value.as_ref().map(|e| self.region_allocates_expr(e, seen)).unwrap_or(false),
            Stmt::Assign { target, value, .. } => self.region_allocates_expr(target, seen) || self.region_allocates_expr(value, seen),
            Stmt::Expr(e) | Stmt::Return(Some(e), _) => self.region_allocates_expr(e, seen),
            Stmt::While { cond, body, .. } => self.region_allocates_expr(cond, seen) || self.region_allocates_block(body, seen),
            Stmt::For { iter, body, .. } => self.region_allocates_expr(iter, seen) || self.region_allocates_block(body, seen),
            _ => false,
        }
    }
    fn region_allocates_expr(&self, e: &Expr, seen: &mut std::collections::HashSet<String>) -> bool {
        match e {
            Expr::Call { callee, args, .. } => {
                let ca = match callee.as_ref() {
                    Expr::Ident(n, _) => {
                        if self.is_alloc_name(n) {
                            true
                        } else if let Some(fd) = self.fn_defs.get(n) {
                            if seen.insert(n.clone()) {
                                fd.body.as_ref().map(|b| self.region_allocates_block(b, seen)).unwrap_or(false)
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                ca || args.iter().any(|a| self.region_allocates_expr(a, seen))
            }
            Expr::List(..) | Expr::MapLit(..) | Expr::Comprehension { .. } => true,
            Expr::Unary { rhs, .. } => self.region_allocates_expr(rhs, seen),
            Expr::Binary { lhs, rhs, .. } => self.region_allocates_expr(lhs, seen) || self.region_allocates_expr(rhs, seen),
            Expr::Field { base, .. } => self.region_allocates_expr(base, seen),
            Expr::Index { base, index, .. } => self.region_allocates_expr(base, seen) || self.region_allocates_expr(index, seen),
            Expr::If { cond, then, elifs, els, .. } => {
                self.region_allocates_expr(cond, seen)
                    || self.region_allocates_block(then, seen)
                    || elifs.iter().any(|(c, b)| self.region_allocates_expr(c, seen) || self.region_allocates_block(b, seen))
                    || els.as_ref().map(|b| self.region_allocates_block(b, seen)).unwrap_or(false)
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.region_allocates_expr(scrutinee, seen) || arms.iter().any(|(_, g, b)| g.as_ref().map(|g| self.region_allocates_expr(g, seen)).unwrap_or(false) || self.region_allocates_expr(b, seen))
            }
            Expr::Block(b) => self.region_allocates_block(b, seen),
            Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => self.region_allocates_expr(inner, seen),
            _ => false,
        }
    }

    /// Conservative: can the expression yield a reference? (For the arena escape
    /// check — "could this stored value be an arena pointer?".) Only obviously
    /// scalar → false. `cur_fn` names the function whose body this expression lives
    /// in, so names resolve against the right scope (see `region_bad_block`).
    fn expr_may_be_ref(&self, e: &Expr, cur_fn: Option<&FnDef>) -> bool {
        match e {
            Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Char(..) | Expr::Unary { .. } => false,
            Expr::Binary { op, lhs, rhs, .. } => {
                // String `+` yields Ref; other arithmetic/comparison/logic = scalar.
                matches!(op, BinOp::Add) && (self.expr_may_be_ref(lhs, cur_fn) || self.expr_may_be_ref(rhs, cur_fn))
            }
            Expr::Ident(n, _) => self.region_name_is_ref(n, cur_fn),
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::Ident(n, _) => self.is_alloc_name(n) || self.sigs.get(n).map(|s| s.ret == Ty::Ref).unwrap_or(true),
                _ => true,
            },
            // Indexing an array of scalar (non-ref) elements is definitely a scalar;
            // anything we cannot prove scalar stays conservatively a ref.
            Expr::Index { base, .. } => !self.region_index_scalar(base, cur_fn),
            _ => true,
        }
    }

    /// Is `name` a reference-typed binding in the current region-check context?
    /// Loop's own function (`cur_fn = None`) → live scope; callee → its parameter
    /// annotations (a callee-local / unknown name → conservatively a ref). Sound
    /// direction: `true` when unsure (blocks promotion, never allows a leak).
    fn region_name_is_ref(&self, name: &str, cur_fn: Option<&FnDef>) -> bool {
        match cur_fn {
            None => self.lookup(name).map(|(_, t)| t == Ty::Ref).unwrap_or(true),
            Some(fd) => match fd.sig.params.iter().find(|p| p.name == name) {
                Some(p) => ty_of(p.ty.as_ref()) == Ty::Ref,
                None => true,
            },
        }
    }

    /// Is `base` provably an array whose element kind is a scalar (non-ref)? Then
    /// `base[i]` is definitely not a reference. Resolved from the live `local_arr`
    /// (loop's own function) or the callee's `Array[T]` parameter annotation. Only
    /// `ArrKind::Ref` is a reference element; everything else is scalar. Conservative
    /// `false` (not-provably-scalar) whenever the base is not a resolvable array.
    fn region_index_scalar(&self, base: &Expr, cur_fn: Option<&FnDef>) -> bool {
        let Expr::Ident(n, _) = base else { return false };
        match cur_fn {
            None => self
                .lookup(n)
                .and_then(|(l, _)| self.local_arr.get(&l.0))
                .map(|k| *k != ArrKind::Ref)
                .unwrap_or(false),
            Some(fd) => fd
                .sig
                .params
                .iter()
                .find(|p| &p.name == n)
                .and_then(|p| p.ty.as_ref())
                .filter(|t| t.name == "Array" || t.name == "array")
                .map(|t| {
                    // Scalar element ONLY for a recognised primitive kind. A user object
                    // class maps to `arrkind_of_name` = Long (its default) but is a REF
                    // element — treating it as scalar here would wrongly admit a ref store
                    // in a callee (unsound). So require both: not Ref-kind AND not a user
                    // product/sum type.
                    let elem = t.args.first().map(|a| a.name.as_str()).unwrap_or("Int");
                    arrkind_of_name(elem) != ArrKind::Ref
                        && !self.types.contains_key(elem)
                        && !self.variants.contains_key(elem)
                        && !self.generic_ptypes.contains_key(elem)
                })
                .unwrap_or(false),
        }
    }

    /// Like `lower_block`, but yields the tail value (for if/block expressions).
    /// Without tail → (_, Void).
    fn lower_block_val(&mut self, b: &Block2) -> (Operand, Ty) {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            self.lower_stmt(s);
        }
        let v = match &b.tail {
            Some(t) => {
                self.mark_line(expr_span(t));
                self.lower_expr(t)
            }
            None => (Operand::ConstI64(0), Ty::Void),
        };
        self.scopes.pop();
        v
    }

    /// Debug: emit a `DebugLine` marker for `span`'s source line (only in debug
    /// builds, and only when the line changed). The backend turns it into a
    /// `!DILocation` for the instructions that follow.
    fn mark_line(&mut self, span: crate::diag::Span) {
        if self.line_starts.is_empty() {
            return;
        }
        let line = line_of(self.line_starts, span.0);
        if line != 0 && line != self.last_dbg_line {
            self.last_dbg_line = line;
            let frame = vec![(self.fn_name.clone(), line)];
            self.emit(Statement::DebugLine(frame));
        }
    }

    fn lower_stmt(&mut self, s: &Stmt) {
        self.mark_line(stmt_span(s));
        match s {
            Stmt::Let { mutable, name, ty: ann, value, .. } => {
                // An explicit `: Type` annotation names the object class the RHS may
                // not carry (the inference escape hatch); used as a fallback below.
                // Generic annotations (`Option[T]`, `Result[T, E]`) resolve to the
                // monomorphized instance (`Option$Float`, `Result$Str$Str`); a bare
                // `Result` (no arguments) is E-error, never a dangling class.
                // An `Array[Cls]` annotation is NOT an object class for the local itself
                // (the local is the array) — it declares the element class, handled via
                // `arr_hint` below, so exclude it here.
                let ann_class = ann
                    .as_ref()
                    .filter(|t| t.name != "Array" && t.name != "array")
                    .and_then(|t| {
                        if t.name == "Result" && t.args.len() != 2 {
                            self.errs.push("error[E100]: `Result` needs both type arguments (`x: Result[T, E]`)".into());
                            None
                        } else {
                            class_of_ann(Some(t), &self.generic_ptypes, &self.generic_stypes)
                        }
                    });
                // `mut r: Result[T, E] = Ok(x)/Err(e)` — the annotation fixes what the
                // payload leaves free (one-shot hint, like `arr_hint`).
                let sum_hint = ann.as_ref().and_then(|t| {
                    if t.name == "Result" && t.args.len() == 2 {
                        Some((t.args[0].name.clone(), t.args[1].name.clone()))
                    } else {
                        None
                    }
                });
                // `mut x: Array[Node] = array(n)` — the annotation's element type is a
                // user object class → build a REF array of that class. Passed to the
                // `array(n)` builtin through `arr_hint` (consumed once).
                let arr_hint = ann
                    .as_ref()
                    .filter(|t| t.name == "Array" || t.name == "array")
                    .and_then(|t| t.args.first())
                    .and_then(|a| class_of(Some(a)));
                // `mut f = x -> …`: remember lambda (the call is inline-expanded).
                if let Some(Expr::Lambda { params, body, .. }) = value {
                    let l = self.new_local(Ty::I64);
                    self.local_lambda.insert(l.0, (params.clone(), (**body).clone()));
                    self.bind(name, l, Ty::I64);
                    return;
                }
                // Binding-vs-assignment (F3 replacement until Resolve exists): `mut x = …`
                // always binds anew; a plain `x = …` on an already
                // visible name is an assignment, not shadowing.
                if !mutable {
                    if let Some((l, _)) = self.lookup(name) {
                        let (op, _) = match value {
                            Some(v) => self.lower_expr(v),
                            None => (Operand::ConstI64(0), Ty::I64),
                        };
                        // Update object class on reassignment (traversal
                        // `cur = cur.next` must keep knowing cur as a Node).
                        if let Some(c) = self.class_of_operand(&op) {
                            self.local_class.insert(l.0, c);
                        }
                        // `Ptr` provenance follows the value: a reassigned integer
                        // clears it (doubt removes the mark, never keeps a stale one).
                        self.track_ptr(l.0, &op);
                        self.emit(Statement::Assign(l, Rvalue::Use(op)));
                        return;
                    }
                }
                self.arr_hint = arr_hint;
                self.sum_hint = sum_hint;
                // Kernel `Rc[T]` annotation: `New`s of T built by this RHS are
                // relabeled to the ownership marker class `Rc$T` below (same
                // layout — only the solver treats them as forced-shared).
                let rc_base = self.locals.len();
                let (op, ty) = match value {
                    Some(v) => self.lower_expr(v),
                    None => (Operand::ConstI64(0), Ty::I64),
                };
                self.arr_hint = None;
                self.sum_hint = None;
                // Relabel direct `T(..)` constructions under `mut r: Rc[T]` to
                // `Rc$T` (range-scoped: only News this RHS built, only matching
                // inner class — nested other-class News keep their class). The
                // New-dest's table entry is relabeled too, so later field/method
                // resolution sees `Rc$T` (not the ctor name). Field ops on the
                // relabeled roots (and their in-range copies) are relabeled as
                // well, so ownership sniffing (`Rc$` parents own via counting)
                // and `weak` lookup see one consistent class.
                let rc_ann = rc_mangled_of_ann(ann);
                if let Some((mangled, inner)) = rc_ann.clone() {
                    let mut relabeled: Vec<u32> = Vec::new();
                    for b in &mut self.blocks {
                        for st in &mut b.statements {
                            if let Statement::New { dest, class } = st {
                                if dest.0 as usize >= rc_base && class.as_str() == inner {
                                    class.clone_from(&mangled);
                                    relabeled.push(dest.0);
                                }
                            }
                        }
                    }
                    // Copies of relabeled roots (same range) + their field ops.
                    let mut grown = true;
                    while grown {
                        grown = false;
                        for b in &mut self.blocks {
                            for st in &mut b.statements {
                                match st {
                                    Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) => {
                                        if relabeled.contains(&s.0) && !relabeled.contains(&d.0) {
                                            relabeled.push(d.0);
                                            grown = true;
                                        }
                                    }
                                    Statement::PutField { obj, class, .. } => {
                                        if let Operand::Copy(o) = obj {
                                            if relabeled.contains(&o.0) && class.as_str() == inner {
                                                class.clone_from(&mangled);
                                            }
                                        }
                                    }
                                    Statement::GetField { obj, class, .. } => {
                                        if let Operand::Copy(o) = obj {
                                            if relabeled.contains(&o.0) && class.as_str() == inner {
                                                class.clone_from(&mangled);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    for d in relabeled {
                        self.local_class.insert(d, mangled.clone());
                    }
                }
                let l = self.new_local(ty);
                // Pass the object class resp. array element kind on to the new local.
                // The explicit `: Type` annotation is the fallback when the RHS operand
                // carries no class (the inference escape hatch). A `Rc[T]`
                // annotation OVERRIDES (never falls back): the bound value is
                // claimed as `Rc$T` — or rejected on a genuine mismatch (a `Leaf`
                // object is not a `Rc[Node]`, layouts would diverge).
                if let Some((mangled, inner)) = rc_ann {
                    match self.class_of_operand(&op) {
                        Some(c) if c == mangled || c == inner => {
                            self.local_class.insert(l.0, mangled);
                        }
                        Some(c) => {
                            self.errs.push(format!("error[E300]: `{name}: Rc[{inner}]` is bound to a `{c}` value — construct the declared type (`{inner}(..)`) or drop the annotation (AGENTS.md §2.5)"));
                        }
                        None => {
                            self.local_class.insert(l.0, mangled);
                        }
                    }
                } else if let Some(c) = self.class_of_operand(&op).or(ann_class) {
                    self.local_class.insert(l.0, c);
                }
                if let Some(k) = self.arr_of_operand(&op) {
                    self.local_arr.insert(l.0, k);
                }
                // Propagate the ref-array element class (`x[i]` is then a known object).
                if let Some(c) = self.arr_class_of_operand(&op) {
                    self.local_arr_class.insert(l.0, c);
                }
                // Propagate a bound MMIO handle (`mut u2 = uart` stays a handle).
                if let Some(m) = self.mmio_of_operand(&op) {
                    self.mmio.insert(l.0, m);
                }
                // `Ptr` provenance (AGENTS.md §3, E421): a `mut x: Ptr = v`
                // annotation DEMANDS a proven pointer — an integer RHS is
                // int→ptr, rejected, never silently relabeled. Without the
                // annotation, provenance propagates from the value (so
                // `mut p = cstr(s)` threads on as a pointer).
                if ann.as_ref().is_some_and(|t| t.name == "Ptr") {
                    if self.ptr_of_operand(&op) {
                        self.ptr_locals.insert(l.0);
                    } else {
                        self.errs.push(format!("error[E421]: `{name}: Ptr` is bound to a non-pointer value — a number is never an address (AGENTS.md §3). Bind a `Ptr` source (`cstr`, an extern call returning `Ptr`, a `Ptr` parameter) or the null literal `0`"));
                    }
                } else {
                    self.track_ptr(l.0, &op);
                }
                self.emit(Statement::Assign(l, Rvalue::Use(op)));
                self.bind(name, l, ty);
            }
            Stmt::Assign { target, op, value, .. } => match target {
                Expr::Ident(name, _) => {
                    if let Some((l, _ty)) = self.lookup(name) {
                        let (rhs, _) = self.lower_expr(value);
                        if op.is_none() {
                            if let Some(c) = self.class_of_operand(&rhs) {
                                self.local_class.insert(l.0, c);
                            }
                            if let Some(m) = self.mmio_of_operand(&rhs) {
                                self.mmio.insert(l.0, m);
                            }
                            self.track_ptr(l.0, &rhs);
                        }
                        let rv = match op {
                            None => Rvalue::Use(rhs),
                            Some(o) => Rvalue::Binary(map_op(*o), Operand::Copy(l), rhs),
                        };
                        self.emit(Statement::Assign(l, rv));
                    } else {
                        self.errs.push(format!("unknown variable: {name}"));
                    }
                }
                // Field mutation `p.x = v` resp. `p.x op= v` → (Get)+Binary+PutField.
                Expr::Field { base, name, .. } => {
                    let (obj, _) = self.lower_expr(base);
                    let class = match self.class_of_operand(&obj) {
                        Some(c) => c,
                        None => {
                            self.errs.push(format!("field assignment `.{name}`: type of the object unknown (annotate it)"));
                            return;
                        }
                    };
                    let fty = match self.layout_of(&class).and_then(|l| l.into_iter().find(|(n, ..)| n == name)) {
                        Some((_, ty, _)) => ty,
                        None => {
                            self.errs.push(format!("`{class}` has no field `{name}`"));
                            return;
                        }
                    };
                    let (mut v, _) = self.lower_expr(value);
                    if let Some(o) = op {
                        // compound: read old value, combine.
                        let cur = self.new_local(fty);
                        self.emit(Statement::GetField { dest: cur, obj: obj.clone(), class: class.clone(), field: name.clone() });
                        let d = self.new_local(fty);
                        self.emit(Statement::Assign(d, Rvalue::Binary(map_op(*o), Operand::Copy(cur), v)));
                        v = Operand::Copy(d);
                    }
                    if fty == Ty::I64 {
                        v = to_i64(v);
                    }
                    self.emit(Statement::PutField { obj, class, field: name.clone(), value: v });
                }
                // Index assignment `xs[i] = v` (array or growable list).
                Expr::Index { base, index, .. } => {
                    let (arr, _) = self.lower_expr(base);
                    let (idx, _) = self.lower_expr(index);
                    let (mut v, vt) = self.lower_expr(value);
                    if self.class_of_operand(&arr).as_deref() == Some("$List") {
                        self.emit(Statement::Call { dest: None, func: "vire_list_set".into(), args: vec![arr, to_i64(idx), to_i64(v)] });
                    } else if let Some(kind) = self.arr_of_operand(&arr) {
                        if kind == ArrKind::Ref {
                            // `x[i] = e` into a ref array of `Cls`: the stored value must be
                            // a `Cls` reference (or `null`). A class mismatch is a loud error
                            // — never a wrong-offset store — so a later `x[i].field` read
                            // (which trusts the declared element class) is always sound.
                            match (self.arr_class_of_operand(&arr), self.class_of_operand(&v)) {
                                (Some(ec), Some(vc)) if ec != vc => {
                                    self.errs.push(format!("index assignment: array element is `{ec}`, stored value is `{vc}`"));
                                }
                                // A ref value with no known class (e.g. a bare `null`,
                                // class Ref/Str) is admitted — null stores no object; a
                                // classless ref cannot claim a conflicting layout.
                                _ => {}
                            }
                        } else if (kind == ArrKind::Double || kind == ArrKind::Float) && (vt == Ty::I32 || vt == Ty::I64) {
                            // `xs[i] = <int>` into a float array → widen int to the
                            // element's float value type (i2d/i2f), else the store's
                            // value/element types would mismatch.
                            let d = self.new_local(kind.value_ty());
                            self.emit(Statement::Assign(d, Rvalue::Convert(v)));
                            v = Operand::Copy(d);
                        } else if kind.value_ty() == Ty::I64 && vt != Ty::I64 {
                            // Long AND U8 (byte) arrays carry their element as i64
                            // in the IR. Widen the stored value (e.g. a byte from
                            // `65 + i`, char_at, or arithmetic) so the store's
                            // value/element widths agree.
                            v = to_i64(v);
                        }
                        let idx32 = self.to_i32(idx);
                        self.emit(Statement::ArrayStore { arr, index: idx32, value: v, kind, checked: true });
                    } else {
                        self.errs.push("index assignment: not an array/list".into());
                    }
                }
                _ => {
                    self.errs.push("assignment target M2: only variables and fields".into());
                }
            },
            Stmt::Expr(e) => {
                self.lower_expr(e);
            }
            Stmt::Return(e, _) => {
                let t = match e {
                    Some(e) => {
                        let (op, _) = self.lower_expr(e);
                        // E421: a `-> Ptr` function returns only proven pointers.
                        if self.ret_is_ptr && !self.ptr_of_operand(&op) {
                            self.errs.push("error[E421]: `return` in a `-> Ptr` function with a non-pointer value — a number is never an address (AGENTS.md §3)".into());
                        }
                        Terminator::Return(Some(op))
                    }
                    None => Terminator::Return(None),
                };
                let cur = self.cur;
                self.term(cur, t);
                // Rest becomes a new (unreachable) block
                let nb = self.new_block();
                self.cur = nb.0 as usize;
            }
            Stmt::Break(_) => {
                match self.loops.last() {
                    Some((_, exit)) => {
                        let cur = self.cur;
                        self.term(cur, Terminator::Goto(*exit));
                        let nb = self.new_block();
                        self.cur = nb.0 as usize;
                    }
                    None => self.errs.push("`break` outside a loop".into()),
                }
            }
            Stmt::Continue(_) => {
                match self.loops.last() {
                    Some((header, _)) => {
                        let cur = self.cur;
                        self.term(cur, Terminator::Goto(*header));
                        let nb = self.new_block();
                        self.cur = nb.0 as usize;
                    }
                    None => self.errs.push("`continue` outside a loop".into()),
                }
            }
            Stmt::While { cond, body, .. } => {
                let header = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Goto(header));
                self.cur = header.0 as usize;
                let (c, _) = self.lower_expr(cond);
                let bodyb = self.new_block();
                let exit = self.new_block();
                self.term(header.0 as usize, Terminator::Branch { cond: c, then_blk: bodyb, else_blk: exit });
                self.cur = bodyb.0 as usize;
                // AUTO-ARENA (escape→arena): place provably non-escaping alloc.
                // into a per-iteration bump arena (no malloc/free per node).
                // No inferred arena inside an explicit `region` (its unbounded push
                // would shadow the declared bounded pool; the region's own check subsumes it).
                let arena = self.in_region == 0 && self.while_arena_safe(body);
                let body_locals_start = self.locals.len();
                if arena {
                    self.emit(Statement::Call { dest: None, func: "jrt_arena_push".into(), args: vec![] });
                }
                self.loops.push((header, exit));
                self.lower_block(body);
                self.loops.pop();
                if arena {
                    // Ref locals created in the body point into the arena. After the pop
                    // the memory is gone → null them BEFORE the pop, otherwise the
                    // function-end release (jrt_release) reads freed memory (UAF).
                    for idx in body_locals_start..self.locals.len() {
                        if self.locals[idx] == Ty::Ref {
                            self.emit(Statement::Assign(Local(idx as u32), Rvalue::Use(Operand::ConstNull)));
                        }
                    }
                    self.emit(Statement::Call { dest: None, func: "jrt_arena_pop".into(), args: vec![] });
                }
                let end = self.cur;
                self.term(end, Terminator::Goto(header));
                self.cur = exit.0 as usize;
            }
            Stmt::For { pat, iter, body, .. } => {
                let name = match pat {
                    Pattern::Bind(n, _) => n.clone(),
                    Pattern::Wildcard(_) => "_".into(),
                    _ => {
                        self.errs.push("for pattern: only `for x in …`".into());
                        return;
                    }
                };
                // `for x in liste` (non-Range) → iterate over the array:
                // i=0; while i<len { x = arr[i]; body; i++ }.
                if !matches!(iter, Expr::Range { .. }) {
                    let (arr, _) = self.lower_expr(iter);
                    // for over a growable list ($List) → vire_list_len/get.
                    if self.class_of_operand(&arr).as_deref() == Some("$List") {
                        let len = self.new_local(Ty::I64);
                        self.emit(Statement::Call { dest: Some(len), func: "vire_list_len".into(), args: vec![arr.clone()] });
                        let ivar = self.new_local(Ty::I64);
                        self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
                        let header = self.new_block();
                        let cur = self.cur;
                        self.term(cur, Terminator::Goto(header));
                        self.cur = header.0 as usize;
                        let c = self.new_local(Ty::I32);
                        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), Operand::Copy(len))));
                        let bodyb = self.new_block();
                        let latch = self.new_block();
                        let exit = self.new_block();
                        self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(c), then_blk: bodyb, else_blk: exit });
                        self.cur = bodyb.0 as usize;
                        let elem = self.new_local(Ty::I64);
                        self.emit(Statement::Call { dest: Some(elem), func: "vire_list_get".into(), args: vec![arr.clone(), Operand::Copy(ivar)] });
                        self.scopes.push(HashMap::new());
                        self.bind(&name, elem, Ty::I64);
                        self.loops.push((latch, exit));
                        self.lower_block(body);
                        self.loops.pop();
                        self.scopes.pop();
                        let end = self.cur;
                        self.term(end, Terminator::Goto(latch));
                        self.cur = latch.0 as usize;
                        self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                        self.term(latch.0 as usize, Terminator::Goto(header));
                        self.cur = exit.0 as usize;
                        return;
                    }
                    let kind = match self.arr_of_operand(&arr) {
                        Some(k) => k,
                        None => {
                            self.errs.push("for iterator: range `a..b` or a list".into());
                            return;
                        }
                    };
                    let vty = kind.value_ty();
                    let len = self.array_len_i64(arr.clone());
                    let ivar = self.new_local(Ty::I64);
                    self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
                    let header = self.new_block();
                    let cur = self.cur;
                    self.term(cur, Terminator::Goto(header));
                    self.cur = header.0 as usize;
                    let cond = self.new_local(Ty::I32);
                    self.emit(Statement::Assign(cond, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), len)));
                    let bodyb = self.new_block();
                    let latch = self.new_block();
                    let exit = self.new_block();
                    self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(cond), then_blk: bodyb, else_blk: exit });
                    self.cur = bodyb.0 as usize;
                    let elem = self.new_local(vty);
                    let elem_class = self.arr_class_of_operand(&arr);
                    let idx32 = self.to_i32(Operand::Copy(ivar));
                    self.emit(Statement::ArrayLoad { dest: elem, arr, index: idx32, kind, checked: true });
                    self.scopes.push(HashMap::new());
                    // `for x in refArray` — bind the element's object class so `x.field`
                    // resolves in the loop body (like `mut x = refArray[i]`).
                    if kind == ArrKind::Ref {
                        if let Some(c) = elem_class {
                            self.local_class.insert(elem.0, c);
                        }
                    }
                    self.bind(&name, elem, vty);
                    self.loops.push((latch, exit));
                    self.lower_block(body);
                    self.loops.pop();
                    self.scopes.pop();
                    let end = self.cur;
                    self.term(end, Terminator::Goto(latch));
                    self.cur = latch.0 as usize;
                    self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                    self.term(latch.0 as usize, Terminator::Goto(header));
                    self.cur = exit.0 as usize;
                    return;
                }
                let (start, end_op, incl) = match iter {
                    Expr::Range { start, end, inclusive, .. } => {
                        let (s, _) = self.lower_expr(start);
                        let (e, _) = self.lower_expr(end);
                        (s, e, *inclusive)
                    }
                    _ => unreachable!(),
                };
                let ivar = self.new_local(Ty::I64);
                self.emit(Statement::Assign(ivar, Rvalue::Use(to_i64(start))));
                let header = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Goto(header));
                self.cur = header.0 as usize;
                let cond = self.new_local(Ty::I32);
                let cmp = if incl { IB::CmpLe } else { IB::CmpLt };
                self.emit(Statement::Assign(cond, Rvalue::Binary(cmp, Operand::Copy(ivar), to_i64(end_op))));
                let bodyb = self.new_block();
                let latch = self.new_block(); // increment block: `continue` target
                let exit = self.new_block();
                self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(cond), then_blk: bodyb, else_blk: exit });
                self.cur = bodyb.0 as usize;
                // Bind the loop variable (always scalar I64) BEFORE the arena analysis so
                // `expr_may_be_ref` can see it — otherwise `a[i] = i` reads `i` as an
                // unknown (conservatively ref) and the scalar-store relaxation never fires.
                self.scopes.push(HashMap::new());
                self.bind(&name, ivar, Ty::I64);
                // AUTO-ARENA (escape→arena), same soundness gate as the `while` case:
                // a numeric `for` iteration whose allocations provably do not escape the
                // iteration runs in a per-iteration bump arena (no malloc/free per node).
                // `while_arena_safe` forbids body-level return/break/continue, so the
                // arena_pop below is never skipped.
                // No inferred arena inside an explicit `region` (its unbounded push
                // would shadow the declared bounded pool; the region's own check subsumes it).
                let arena = self.in_region == 0 && self.while_arena_safe(body);
                let body_locals_start = self.locals.len();
                if arena {
                    self.emit(Statement::Call { dest: None, func: "jrt_arena_push".into(), args: vec![] });
                }
                self.loops.push((latch, exit)); // continue → latch (not header!), otherwise no increment
                self.lower_block(body);
                self.loops.pop();
                self.scopes.pop();
                if arena {
                    // Ref locals created in the body point into the arena; null them
                    // before the pop so the function-end release does not read freed
                    // memory (UAF) — identical to the `while` arena.
                    for idx in body_locals_start..self.locals.len() {
                        if self.locals[idx] == Ty::Ref {
                            self.emit(Statement::Assign(Local(idx as u32), Rvalue::Use(Operand::ConstNull)));
                        }
                    }
                    self.emit(Statement::Call { dest: None, func: "jrt_arena_pop".into(), args: vec![] });
                }
                let end = self.cur;
                self.term(end, Terminator::Goto(latch));
                self.cur = latch.0 as usize;
                self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                self.term(latch.0 as usize, Terminator::Goto(header));
                self.cur = exit.0 as usize;
            }
        }
    }

    /// Returns (operand, type). Emits temporaries as needed.
    fn lower_expr(&mut self, e: &Expr) -> (Operand, Ty) {
        match e {
            Expr::Int(v, _) => (Operand::ConstI64(*v as i64), Ty::I64),
            Expr::Float(v, _) => (Operand::ConstF64(*v), Ty::F64),
            Expr::Bool(b, _) => (Operand::ConstI32(if *b { 1 } else { 0 }), Ty::I32),
            Expr::Str(s, _) => {
                let id = self.intern(s);
                (Operand::ConstStr(id), Ty::Ref)
            }
            // `null` — MEASUREMENT BOOTSTRAP (not the final language; that has no
            // null, but Option). Only needed to construct linked/cyclic graphs
            // and thereby enter the RC/collector path on Vire IR FOR THE FIRST
            // TIME (M0.1b-on-Vire). Will be replaced by Option[T].
            Expr::Ident(name, _) if name == "null" && self.lookup(name).is_none() => {
                (Operand::ConstNull, Ty::Ref)
            }
            // Nullary variant as expression: `Empty` → tagged instance.
            Expr::Ident(name, _) if self.variants.contains_key(name) && self.lookup(name).is_none() => {
                self.build_variant(name, &[])
            }
            Expr::Ident(name, _) => match self.lookup(name) {
                Some((l, ty)) => (Operand::Copy(l), ty),
                None => {
                    self.errs.push(format!("unknown variable: {name}"));
                    (Operand::ConstI64(0), Ty::I64)
                }
            },
            // `self` = the receiver bound as a parameter.
            Expr::SelfExpr(_) => match self.lookup("self") {
                Some((l, ty)) => (Operand::Copy(l), ty),
                None => {
                    self.errs.push("`self` outside a method".into());
                    (Operand::ConstI64(0), Ty::I64)
                }
            },
            Expr::Unary { op, rhs, .. } => {
                let (r, rt) = self.lower_expr(rhs);
                match op {
                    UnOp::Neg => {
                        let d = self.new_local(rt);
                        self.emit(Statement::Assign(d, Rvalue::Neg(r)));
                        (Operand::Copy(d), rt)
                    }
                    UnOp::Not => {
                        let d = self.new_local(Ty::I32);
                        self.emit(Statement::Assign(d, Rvalue::Binary(IB::CmpEq, r, Operand::ConstI32(0))));
                        (Operand::Copy(d), Ty::I32)
                    }
                }
            }
            Expr::Binary { .. } if const_eval(e).is_some() => {
                // General constant folding: `2 + 3`, `WIDTH * HEIGHT` etc. →
                // constant at compile time (not only under `comptime`).
                match const_eval(e).unwrap() {
                    CVal::Int(v) => (Operand::ConstI64(v), Ty::I64),
                    CVal::Float(v) => (Operand::ConstF64(v), Ty::F64),
                    CVal::Bool(b) => (Operand::ConstI32(if b { 1 } else { 0 }), Ty::I32),
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let (mut l, mut lt) = self.lower_expr(lhs);
                let (mut r, mut rt) = self.lower_expr(rhs);
                // String concatenation: `+` with at least one ref side → Concat,
                // numbers are automatically converted to strings (`"n=" + n`).
                if matches!(op, BinOp::Add) && (lt == Ty::Ref || rt == Ty::Ref) {
                    let ls = self.to_str(l, lt);
                    let rs = self.to_str(r, rt);
                    let d = self.new_local(Ty::Ref);
                    self.emit(Statement::Call { dest: Some(d), func: "jrt_str_concat".into(), args: vec![ls, rs] });
                    return (Operand::Copy(d), Ty::Ref);
                }
                // Align integer widths: if the expression mixes a narrow i32
                // (e.g. a packed `I32` field) with i64, the i32 is sign-
                // extended. Otherwise the backend would emit `add i64 %a, %i32` (type error).
                // Makes opt-in `I32` field packing fully usable (RAM savings).
                if lt == Ty::I32 && rt == Ty::I64 {
                    l = self.widen_i32(l);
                    lt = Ty::I64;
                } else if rt == Ty::I32 && lt == Ty::I64 {
                    r = self.widen_i32(r);
                    rt = Ty::I64;
                }
                // Numeric promotion: mixing Int with Float in arithmetic (or a comparison)
                // promotes the Int side to Float (i64 → f64, `sitofp`), so `i * 1.0`,
                // `i / n * pi`, `x < 2.0` work without an explicit `as Float`. Int/Int stays
                // integer arithmetic (integer division included) — only a genuinely-Float
                // operand triggers promotion.
                if lt == Ty::I64 && rt == Ty::F64 {
                    let f = self.new_local(Ty::F64);
                    self.emit(Statement::Assign(f, Rvalue::Convert(l)));
                    l = Operand::Copy(f);
                    lt = Ty::F64;
                } else if rt == Ty::I64 && lt == Ty::F64 {
                    let f = self.new_local(Ty::F64);
                    self.emit(Statement::Assign(f, Rvalue::Convert(r)));
                    r = Operand::Copy(f);
                    rt = Ty::F64;
                }
                let _ = rt;
                let is_cmp = matches!(
                    op,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                );
                let ty = if is_cmp { Ty::I32 } else { lt };
                let d = self.new_local(ty);
                self.emit(Statement::Assign(d, Rvalue::Binary(map_op(*op), l, r)));
                (Operand::Copy(d), ty)
            }
            Expr::Field { base, name, .. } => {
                let (obj, _) = self.lower_expr(base);
                let class = match self.class_of_operand(&obj) {
                    Some(c) => c,
                    None => {
                        self.errs.push(format!("field access `.{name}`: type of the object unknown (annotate it)"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let (fty, rtarget) = match self.layout_of(&class).and_then(|l| l.into_iter().find(|(n, ..)| n == name)) {
                    Some((_, ty, rt)) => (ty, rt.clone()),
                    None => {
                        self.errs.push(format!("`{class}` has no field `{name}`"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let d = self.new_local(fty);
                if let Some(rt) = rtarget {
                    self.local_class.insert(d.0, rt);
                }
                // Array-typed field → tag the result local with its element kind so
                // `x.field[i]` / `x.field[i] = v` / `x.field.len()` lower to real
                // bounds-checked accesses (a bare GetField only knows `Ref`).
                if let Some(k) = self.field_arr.get(&(class.clone(), name.clone())).copied() {
                    self.local_arr.insert(d.0, k);
                }
                self.emit(Statement::GetField { dest: d, obj, class, field: name.clone() });
                (Operand::Copy(d), fty)
            }
            Expr::Call { callee, args, .. } => self.lower_call(callee, args),
            Expr::TurboCall { callee, targs, args, .. } => self.lower_turbocall(callee, targs, args),
            Expr::If { cond, then, elifs, els, .. } => self.lower_if(cond, then, elifs, els),
            Expr::Match { scrutinee, arms, .. } => self.lower_match(scrutinee, arms),
            // `comptime <expr>` → compile-time folding of constant expressions.
            // `x as T` — numeric conversion (int↔float, widths).
            Expr::Cast { inner, ty, .. } => {
                let (op, from) = self.lower_expr(inner);
                // E421 (AGENTS.md §3): `x as Ptr` forges a pointer out of a
                // number unless `x` is already a proven pointer (or null).
                // Casts between other types are unaffected.
                if ty.name == "Ptr" && !self.ptr_of_operand(&op) {
                    self.errs.push("error[E421]: `as Ptr` on a non-pointer value — a number is never an address (AGENTS.md §3). Cast only a `Ptr` source or the null literal `0`".into());
                }
                let to = ty_of(Some(ty));
                if from == to {
                    return (op, to);
                }
                let d = self.new_local(to);
                self.emit(Statement::Assign(d, Rvalue::Convert(op)));
                (Operand::Copy(d), to)
            }
            // `comptime if COND { A } else { B }` — conditional compilation: fold
            // COND at compile time and lower ONLY the taken branch, dropping the
            // rest (so a branch may reference platform-specific / otherwise-invalid
            // code that is never compiled). Falls back to constant folding for
            // value expressions (`comptime 2 + 3`).
            Expr::Comptime { inner, .. } => {
                if let Expr::If { cond, then, elifs, els, .. } = inner.as_ref() {
                    let taken: Option<&Block2> = match const_eval(cond) {
                        Some(CVal::Bool(true)) => Some(then),
                        Some(CVal::Bool(false)) => {
                            let mut chosen = els.as_ref();
                            for (ec, eb) in elifs {
                                match const_eval(ec) {
                                    Some(CVal::Bool(true)) => {
                                        chosen = Some(eb);
                                        break;
                                    }
                                    Some(CVal::Bool(false)) => continue,
                                    _ => {
                                        chosen = None;
                                        break;
                                    }
                                }
                            }
                            chosen
                        }
                        _ => {
                            self.errs.push("comptime if: condition is not a compile-time constant bool".into());
                            None
                        }
                    };
                    return match taken {
                        Some(b) => self.lower_block_val(b),
                        None => (Operand::ConstI64(0), Ty::Void),
                    };
                }
                match const_eval(inner) {
                    Some(CVal::Int(v)) => (Operand::ConstI64(v), Ty::I64),
                    Some(CVal::Float(v)) => (Operand::ConstF64(v), Ty::F64),
                    Some(CVal::Bool(b)) => (Operand::ConstI32(if b { 1 } else { 0 }), Ty::I32),
                    None => {
                        self.errs.push("comptime: expression is not constant-foldable (only literals/arithmetic/comparisons)".into());
                        (Operand::ConstI64(0), Ty::I64)
                    }
                }
            }
            // `e?` — error propagation for Result: Ok(v) → v; Err(_) → return e.
            // (Desugared to match; the enclosing function must return Result.)
            Expr::Try { inner, .. } => {
                // `e?` over a TYPED `Result[A, B]`: Ok(v) → v (typed A); Err(e) →
                // return a reconstructed `Result[C, D]` (the enclosing signature)
                // carrying the same error payload. Requires the error types to
                // match EXACTLY (B == D) — else E-error (no silent cross-layout
                // pass-through; the Ok slot types may differ, the Err value is
                // rebuilt into the caller's layout).
                let (obj, _) = self.lower_expr(inner);
                let recv = self.class_of_operand(&obj).unwrap_or_default();
                let (ra, rb) = Self::parse_result_class(&recv);
                let (rc, rd) = match &self.expected_result {
                    Some((t, e)) => (t.clone(), e.clone()),
                    None => {
                        self.errs.push("error[E100]: `?` needs an enclosing `-> Result[T, E]` return type (nothing to propagate into)".into());
                        ("Int".into(), "Str".into())
                    }
                };
                if recv.is_empty() || ra.is_empty() {
                    self.errs.push("error[E100]: `?` needs a typed `Result[T, E]` value (annotate it)".into());
                }
                if !rb.is_empty() && rb != rd {
                    self.errs.push(format!("error[E100]: cannot propagate `?` from `Result[{ra}, {rb}]` into `Result[{rc}, {rd}]` — the error types differ (convert explicitly with `match`)"));
                }
                // Layouts for both sides (instantiated on demand, like construction).
                let caller = self.mono_result_class(&rc, &rd);
                let tag = self.new_local(Ty::I64);
                self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: recv.clone(), field: "__tag".into() });
                let is_ok = self.new_local(Ty::I32);
                self.emit(Statement::Assign(is_ok, Rvalue::Binary(IB::CmpEq, Operand::Copy(tag), Operand::ConstI64(0))));
                let okb = self.new_block();
                let errb = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Branch { cond: Operand::Copy(is_ok), then_blk: okb, else_blk: errb });
                // Err branch: rebuild the error in the CALLER's layout.
                self.cur = errb.0 as usize;
                let (err_ty, _) = self.result_payload_ty(&recv, "Err");
                let epay = self.new_local(err_ty);
                self.emit(Statement::GetField { dest: epay, obj: obj.clone(), class: recv.clone(), field: "Err_error".into() });
                let newres = self.new_local(Ty::Ref);
                self.local_class.insert(newres.0, caller.clone());
                self.emit(Statement::New { dest: newres, class: caller.clone() });
                self.emit(Statement::PutField { obj: Operand::Copy(newres), class: caller.clone(), field: "__tag".into(), value: Operand::ConstI64(1) });
                self.emit(Statement::PutField { obj: Operand::Copy(newres), class: caller.clone(), field: "Err_error".into(), value: Operand::Copy(epay) });
                self.term(self.cur, Terminator::Return(Some(Operand::Copy(newres))));
                // Ok branch: extract the typed value.
                self.cur = okb.0 as usize;
                let (ok_ty, ok_cls) = self.result_payload_ty(&recv, "Ok");
                let v = self.new_local(ok_ty);
                self.emit(Statement::GetField { dest: v, obj, class: recv.clone(), field: "Ok_value".into() });
                // Attach the payload's object class (so `n.field` resolves when A is a type).
                if let Some(c) = ok_cls {
                    self.local_class.insert(v.0, c);
                }
                (Operand::Copy(v), ok_ty)
            }
            // List literal `[a, b, c]` → NewArray + ArrayStore. Element kind from the
            // first element (homogeneous). Empty list → Long (default).
            Expr::List(elems, _) => {
                let lowered: Vec<(Operand, Ty)> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let kind = lowered.first().map(|(_, t)| arrkind_of(*t)).unwrap_or(ArrKind::Long);
                let arr = self.new_local(Ty::Ref);
                self.local_arr.insert(arr.0, kind);
                self.emit(Statement::NewArray { dest: arr, kind, len: Operand::ConstI32(elems.len() as i32) });
                for (i, (mut v, t)) in lowered.into_iter().enumerate() {
                    if kind == ArrKind::Long {
                        v = to_i64(v);
                    }
                    let _ = t;
                    self.emit(Statement::ArrayStore { arr: Operand::Copy(arr), index: Operand::ConstI32(i as i32), value: v, kind, checked: false });
                }
                (Operand::Copy(arr), Ty::Ref)
            }
            Expr::Comprehension { elem, var, iter, cond, .. } => self.lower_comprehension(elem, var, iter, cond.as_deref()),
            // Map literal `[k: v, …]` → map() + put per pair.
            Expr::MapLit(pairs, _) => {
                let m = self.new_local(Ty::Ref);
                self.local_class.insert(m.0, "$Map".into());
                self.emit(Statement::Call { dest: Some(m), func: "vire_map_new".into(), args: vec![] });
                for (k, v) in pairs {
                    let (ko, kt) = self.lower_expr(k);
                    let (vo, vt) = self.lower_expr(v);
                    let ko = if kt == Ty::Ref { ko } else { to_i64(ko) };
                    let vo = if vt == Ty::Ref { vo } else { to_i64(vo) };
                    self.emit(Statement::Call { dest: None, func: "vire_map_put".into(), args: vec![Operand::Copy(m), ko, vo] });
                }
                (Operand::Copy(m), Ty::Ref)
            }
            // Indexing `xs[i]` → ArrayLoad (bounds-checked) resp. vire_list_get.
            Expr::Index { base, index, .. } => {
                let (arr, _) = self.lower_expr(base);
                if self.class_of_operand(&arr).as_deref() == Some("$List") {
                    let (idx, _) = self.lower_expr(index);
                    let d = self.new_local(Ty::I64);
                    self.emit(Statement::Call { dest: Some(d), func: "vire_list_get".into(), args: vec![arr, to_i64(idx)] });
                    return (Operand::Copy(d), Ty::I64);
                }
                // `view[i]` — a bounds-checked unsigned byte load over the mmap bytes (the runtime
                // reads the view's (ptr, len) and checks i against len). Zero-copy; unsigned 0..255.
                if self.class_of_operand(&arr).as_deref() == Some("ByteView") {
                    let (idx, _) = self.lower_expr(index);
                    let d = self.new_local(Ty::I64);
                    self.emit(Statement::Call { dest: Some(d), func: "jrt_bview_get".into(), args: vec![arr, to_i64(idx)] });
                    return (Operand::Copy(d), Ty::I64);
                }
                let kind = match self.arr_of_operand(&arr) {
                    Some(k) => k,
                    None => {
                        self.errs.push("index `[]`: unknown array (annotate it)".into());
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let elem_class = self.arr_class_of_operand(&arr);
                let (idx, _) = self.lower_expr(index);
                let idx32 = self.to_i32(idx);
                let vty = kind.value_ty();
                let d = self.new_local(vty);
                self.emit(Statement::ArrayLoad { dest: d, arr, index: idx32, kind, checked: true });
                // A ref array of objects: the loaded element is a `Cls` — attach the
                // class so `x[i].field` / `mut n = x[i]; n.field` resolve.
                if kind == ArrKind::Ref {
                    if let Some(c) = elem_class {
                        self.local_class.insert(d.0, c);
                    }
                }
                (Operand::Copy(d), vty)
            }
            Expr::Block(b) => self.lower_block_val(b),
            // capsule (pure form, scalar-in/-out): the body runs in its
            // own arena. `jrt_arena_push` before the body routes all heap
            // allocations there (immortal → no RC/collector), `jrt_arena_pop`
            // afterwards releases the arena en bloc. ONLY scalar inputs/result
            // allowed (hard errors otherwise): values cannot alias, and no
            // object pointer survives the arena → isolation + fault containment without
            // deep copy. Object-in/-out (deep copy) remains open.
            Expr::Region { max, body, .. } => {
                // M3 explicit bounded pool (the former OWNERSHIP-STATIC.md (removed in cleanup; see git history) §2.4): `region(max)`
                // brackets the body in `jrt_region_push(max)`/`pop` — body
                // allocations land in the pool (immortal, no RC/collector) and are
                // freed en bloc; past `max` bytes the program aborts (refuse-on-
                // full, never silent). Admission reuses the loop-arena proof
                // (`region_bad_block` with the region's own outer/fresh sets —
                // `in_loop = false`, so boundary-crossing return/break/continue is
                // rejected and the pop always runs). On failure: E300, and the
                // body still lowers WITHOUT pooling (normal RC) so one error does
                // not cascade into unknown-variable noise.
                let mut outer = std::collections::HashSet::new();
                for s in &self.scopes {
                    for k in s.keys() {
                        outer.insert(k.clone());
                    }
                }
                let fresh = self.region_fresh_locals(body, &std::collections::HashSet::new(), None);
                let mut seen = std::collections::HashSet::new();
                let safe = !self.region_bad_block(body, false, false, None, &outer, &mut seen, &fresh);
                // The ceiling: any Int expression (evaluated at runtime — the byte
                // counter enforces it dynamically); must be an integer type.
                let (mop, mty) = self.lower_expr(max);
                let m64 = match mty {
                    Ty::I64 => mop,
                    Ty::I32 => {
                        let w = self.new_local(Ty::I64);
                        self.emit(Statement::Assign(w, Rvalue::Convert(mop)));
                        Operand::Copy(w)
                    }
                    _ => {
                        self.errs.push("error[E300]: `region(max)` ceiling must be an `Int` expression".into());
                        Operand::ConstI64(0)
                    }
                };
                if !safe {
                    self.errs.push("error[E300]: `region` body is not pool-safe — a reference would outlive the pool (store outer refs into region objects, return region refs, or cross the boundary with return/break/continue). Name a bigger contract (restructure), or move the graph to a kernel `Rc` (AGENTS.md §2.4)".into());
                    return self.lower_block_val(body);
                }
                self.emit(Statement::Call { dest: None, func: "jrt_region_push".into(), args: vec![m64] });
                self.in_region += 1;
                let body_locals_start = self.locals.len();
                let (val, ty) = self.lower_block_val(body);
                self.in_region -= 1;
                if ty == Ty::Ref {
                    self.errs.push("error[E300]: `region` body value must be scalar — references die with the pool (return plain values)".into());
                }
                // Ref locals created in the body point into the pool; null them
                // before the pop so the function-end release does not read freed
                // pool memory (UAF read) — identical to the loop-arena nulling.
                for idx in body_locals_start..self.locals.len() {
                    if self.locals[idx] == Ty::Ref {
                        self.emit(Statement::Assign(Local(idx as u32), Rvalue::Use(Operand::ConstNull)));
                    }
                }
                self.emit(Statement::Call { dest: None, func: "jrt_region_pop".into(), args: vec![] });
                (val, ty)
            }
            Expr::Capsule { inputs, body, .. } => {
                // Classify inputs. Scalars pass by value. A primitive-array input
                // takes the flat clone path; any other ref (struct/graph, string,
                // ref-array) takes the GENERAL deep-copy (jrt_deep_copy_arena, which
                // dispatches per object via vtable slot 3). Either way the body works
                // on an isolated copy → a body bug can't touch the caller's data.
                let mut array_inputs: Vec<(String, Local, ArrKind)> = Vec::new();
                let mut object_inputs: Vec<(String, Local)> = Vec::new();
                for (nm, _borrowed) in inputs {
                    if let Some((l, Ty::Ref)) = self.lookup(nm) {
                        // E422: MMIO handles are capabilities bound to a global
                        // descriptor (vtable slot 3 is null — no deep-copy). The
                        // handle is bound AS-IS (no copy: copying a capability is
                        // meaningless); only crossing it OUT is forbidden (below).
                        if self.mmio.get(&l.0).is_some() {
                            self.errs.push(format!("error[E422]: capsule input `{nm}` is an MMIO handle — handles cannot be deep-copied into/out of a capsule (use it inside, or pass plain values)"));
                            continue;
                        }
                        match self.local_arr.get(&l.0).copied() {
                            Some(kind) if kind != ArrKind::Ref => array_inputs.push((nm.clone(), l, kind)),
                            _ => object_inputs.push((nm.clone(), l)),
                        }
                    }
                }
                // `return` in the body would skip arena_pop (arena leak) →
                // forbid it. break/continue: the loop targets are saved and
                // cleared during the body (inner loops set their own).
                if body_has_return(body) {
                    self.errs.push("capsule: `return` in the body not allowed (would leak the arena) — use the block value".into());
                }
                self.emit(Statement::Call { dest: None, func: "jrt_arena_push".into(), args: vec![] });
                let saved_loops = std::mem::take(&mut self.loops);
                self.scopes.push(HashMap::new());
                let body_locals_start = self.locals.len(); // arena locals from here on
                // Deep-copy each primitive-array input INTO the arena (jrt_array_clone
                // routes to the arena while it is active) and shadow the name so the
                // body sees the isolated copy; the caller's original is untouched.
                for (nm, l, kind) in &array_inputs {
                    let copy = self.new_local(Ty::Ref);
                    self.emit(Statement::Call {
                        dest: Some(copy),
                        func: "jrt_array_clone".into(),
                        args: vec![Operand::Copy(*l), Operand::ConstI64(kind.size() as i64), Operand::ConstI32(0)],
                    });
                    self.local_arr.insert(copy.0, *kind);
                    self.bind(nm, copy, Ty::Ref);
                }
                // Struct/graph/string/ref-array inputs: general deep-copy into the
                // arena (jrt_deep_copy_arena dispatches per object via vtable slot 3;
                // cycles + sharing handled by the copymap). Preserve the local's
                // array-kind/class so the body accesses the copy the same way.
                for (nm, l) in &object_inputs {
                    let copy = self.new_local(Ty::Ref);
                    self.emit(Statement::Call {
                        dest: Some(copy),
                        func: "jrt_deep_copy_arena".into(),
                        args: vec![Operand::Copy(*l)],
                    });
                    if let Some(k) = self.local_arr.get(&l.0).copied() {
                        self.local_arr.insert(copy.0, k);
                    }
                    if let Some(c) = self.local_class.get(&l.0).cloned() {
                        self.local_class.insert(copy.0, c);
                    }
                    self.bind(nm, copy, Ty::Ref);
                }
                let (val, ty) = self.lower_block_val(body);
                self.scopes.pop();
                self.loops = saved_loops;
                // Result: a scalar is captured into a register (survives the pop); a
                // primitive-array result is DEEP-COPIED OUT to the RC heap so it
                // outlives the arena. Other object results still need deep-copy-out.
                let null_end = self.locals.len();
                let (result_op, result_ty): (Operand, Ty) = if ty == Ty::Ref {
                    // E422: an MMIO handle must not leave the capsule (no deep-copy).
                    if let Operand::Copy(rl) = &val {
                        if self.mmio.get(&rl.0).is_some() {
                            self.errs.push("error[E422]: capsule result is an MMIO handle — handles cannot be deep-copied out of a capsule (return plain values)".into());
                        }
                    }
                    match self.arr_of_operand(&val) {
                        Some(kind) if kind != ArrKind::Ref => {
                            let out = self.new_local(Ty::Ref);
                            self.emit(Statement::Call {
                                dest: Some(out),
                                func: "jrt_arena_export_array".into(),
                                args: vec![val.clone(), Operand::ConstI64(kind.size() as i64)],
                            });
                            self.local_arr.insert(out.0, kind);
                            (Operand::Copy(out), Ty::Ref)
                        }
                        // struct/graph/string/ref-array result → general deep-copy OUT
                        // to the RC heap (survives the pop). Vtable dispatch + copymap.
                        _ => {
                            let cls = self.class_of_operand(&val);
                            let out = self.new_local(Ty::Ref);
                            self.emit(Statement::Call {
                                dest: Some(out),
                                func: "jrt_deep_copy_heap".into(),
                                args: vec![val.clone()],
                            });
                            if let Some(k) = self.arr_of_operand(&val) {
                                self.local_arr.insert(out.0, k);
                            }
                            if let Some(c) = cls {
                                self.local_class.insert(out.0, c);
                            }
                            (Operand::Copy(out), Ty::Ref)
                        }
                    }
                } else {
                    let res = self.new_local(if ty == Ty::Void { Ty::I64 } else { ty });
                    if ty != Ty::Void {
                        self.emit(Statement::Assign(res, Rvalue::Use(val)));
                    }
                    (Operand::Copy(res), ty)
                };
                // All ref locals created inside the arena (input copies + body) point
                // into the arena. After the pop that memory is gone, but the backend
                // releases ref locals at function end (reads the header → UAF). Set them
                // to null BEFORE the pop → jrt_release(null) is a no-op. The exported
                // heap result (created after `null_end`) is spared — it must survive.
                for idx in body_locals_start..null_end {
                    if self.locals[idx] == Ty::Ref {
                        self.emit(Statement::Assign(Local(idx as u32), Rvalue::Use(Operand::ConstNull)));
                    }
                }
                self.emit(Statement::Call { dest: None, func: "jrt_arena_pop".into(), args: vec![] });
                if result_ty == Ty::Void {
                    (Operand::ConstI64(0), Ty::Void)
                } else {
                    (result_op, result_ty)
                }
            }
            Expr::Range { .. } => {
                self.errs.push("range only as a for iterator (M2)".into());
                (Operand::ConstI64(0), Ty::I64)
            }
            other => {
                self.errs.push(format!("expression M2 not yet lowered: {}", expr_kind(other)));
                (Operand::ConstI64(0), Ty::I64)
            }
        }
    }

    /// `f[T, N](args)` — a generic call with EXPLICIT arguments. Type params bind
    /// to the named type; comptime value params bind to the folded literal (value
    /// generics). Remaining type params are still inferred from the arguments.
    fn lower_turbocall(&mut self, callee: &str, targs: &[Expr], call_args: &[Expr]) -> (Operand, Ty) {
        let g = match self.generics.get(callee).cloned() {
            Some(g) => g,
            None => {
                self.errs.push(format!("turbofish `{callee}[..]`: `{callee}` is not a generic function"));
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        if targs.len() > g.tparams.len() {
            self.errs.push(format!("turbofish `{callee}`: {} generic arg(s) given but only {} declared", targs.len(), g.tparams.len()));
            return (Operand::ConstI64(0), Ty::I64);
        }
        let lowered: Vec<(Operand, Ty)> = call_args.iter().map(|a| self.lower_expr(a)).collect();
        let mut bind: HashMap<String, String> = HashMap::new();
        // Explicit positional generic args.
        for (i, ta) in targs.iter().enumerate() {
            let tp = g.tparams[i].clone();
            if g.comptime.get(i).copied().unwrap_or(false) {
                match const_eval(ta) {
                    Some(CVal::Int(v)) => {
                        bind.insert(tp, v.to_string());
                    }
                    _ => {
                        self.errs.push(format!("turbofish `{callee}[{tp}]`: value generic must be a comptime Int"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                }
            } else {
                match ta {
                    Expr::Ident(n, _) => {
                        bind.insert(tp, n.clone());
                    }
                    _ => {
                        self.errs.push(format!("turbofish `{callee}[{tp}]`: type argument must be a type name"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                }
            }
        }
        // Infer any type params not given explicitly, from the argument types.
        for (i, pty) in g.param_tys.iter().enumerate() {
            if let Some(t) = pty {
                if g.tparams.contains(&t.name) && !bind.contains_key(&t.name) {
                    if let Some((op, ty)) = lowered.get(i) {
                        let cls = self.class_of_operand(op);
                        bind.entry(t.name.clone()).or_insert_with(|| concrete_tyname(*ty, cls.as_ref()));
                    }
                }
            }
        }
        let targ_strs: Vec<String> = g.tparams.iter().map(|tp| bind.get(tp).cloned().unwrap_or_else(|| "Int".into())).collect();
        let sym = mono_sym(callee, &targ_strs);
        self.mono.push((callee.to_string(), targ_strs));
        let ret_sub = g.ret.as_ref().map(|t| subst_type(t, &bind));
        let ret = ret_sub.as_ref().map(|t| ty_of(Some(t))).unwrap_or(Ty::Void);
        let ret_class = ret_sub.as_ref().and_then(|t| class_of(Some(t)));
        let (ret_arr, ret_arr_class) = arr_ret_generic(ret_sub.as_ref());
        // E421: substituted `Ptr` parameters are checked like direct calls.
        for (i, pty) in g.param_tys.iter().enumerate() {
            if pty.as_ref().map(|t| subst_type(t, &bind)).is_some_and(|st| st.name == "Ptr") {
                if lowered.get(i).is_some_and(|(o, _)| !self.ptr_of_operand(o)) {
                    self.errs.push(format!("error[E421]: argument {} of `{callee}` is `Ptr` but the value is a non-pointer integer — a number is never an address (AGENTS.md §3)", i + 1));
                }
            }
        }
        let arg_ops: Vec<Operand> = lowered.into_iter().map(|(o, _)| o).collect();
        if ret == Ty::Void {
            self.emit(Statement::Call { dest: None, func: sym, args: arg_ops });
            return (Operand::ConstI64(0), Ty::Void);
        }
        let d = self.new_local(ret);
        if let Some(c) = ret_class {
            self.local_class.insert(d.0, c);
        }
        if ret_sub.as_ref().is_some_and(|t| t.name == "Ptr") {
            self.ptr_locals.insert(d.0);
        }
        if let Some(k) = ret_arr {
            self.local_arr.insert(d.0, k);
        }
        if let Some(c) = ret_arr_class {
            self.local_arr_class.insert(d.0, c);
        }
        self.emit(Statement::Call { dest: Some(d), func: sym, args: arg_ops });
        (Operand::Copy(d), ret)
    }

    fn lower_call(&mut self, callee: &Expr, args: &[Expr]) -> (Operand, Ty) {
        // `with log.span(…)` markers (compile-time only): push/pop the active
        // context field-list. They emit no code — the fields are read when a
        // `log.*` inside the span builds its message.
        if let Expr::Ident(n, _) = callee {
            if n == "__log_span_push" {
                self.log_span.push(args.to_vec());
                return (Operand::ConstI64(0), Ty::Void);
            }
            if n == "__log_span_pop" {
                self.log_span.pop();
                return (Operand::ConstI64(0), Ty::Void);
            }
        }
        // Method call `obj.method(args)` → direct call `Class.method(obj, args)`
        // (monomorphic, no virtual dispatch — Vire types are (still) flat).
        if let Expr::Field { base, name, span } = callee {
            // Feature 6 — `log.LEVEL(msg)` with a COMPILE-TIME level filter. Levels
            // below the threshold lower to nothing (zero instructions), exactly the
            // "disabled log calls cost nothing" property. Enabled levels prepend a
            // level tag to a literal message at compile time and print it.
            if let Expr::Ident(id, _) = base.as_ref() {
                if id == "log" {
                    // 0=debug 1=info 2=warn 3=error. Threshold is a BUILD-TIME choice
                    // (`FASTLLVM_LOG_LEVEL`/`--log-level`, default info): a level below it
                    // lowers to nothing (zero instructions).
                    let threshold = log_threshold();
                    let (level, tag) = match name.as_str() {
                        "debug" => (0, "[DEBUG] "),
                        "info" => (1, "[INFO] "),
                        "warn" => (2, "[WARN] "),
                        "error" => (3, "[ERROR] "),
                        _ => {
                            self.errs.push(format!("log has no level `{name}` (use debug/info/warn/error)"));
                            return (Operand::ConstI64(0), Ty::Void);
                        }
                    };
                    if level >= threshold {
                        if let Some(msg) = args.first() {
                            let printed = self.build_log_message(&name, tag, msg, &args[1..], *span);
                            if let Some(printed) = printed {
                                let call = Expr::Call {
                                    callee: Box::new(Expr::Ident("print".into(), *span)),
                                    args: vec![printed],
                                    span: *span,
                                };
                                self.lower_expr(&call);
                            }
                        }
                    }
                    // Below threshold: emit nothing at all.
                    return (Operand::ConstI64(0), Ty::Void);
                }
            }
            // Iterator adapters over a RANGE receiver. A range is not a value, so
            // match it syntactically before lowering the base as an expression.
            if let Expr::Range { start, end, inclusive, .. } = base.as_ref() {
                let (s, _) = self.lower_expr(start);
                let (e, _) = self.lower_expr(end);
                if let Some(r) = self.lower_iter_adapter(IterSrc::Range { start: s, end: e, incl: *inclusive }, &name, args) {
                    return r;
                }
                // Not an adapter → fall through (range-as-value is an error below).
            }
            // `Mmio.bind("name")` — a memory-mapped hardware handle, WITHOUT inline
            // asm and WITHOUT an integer address in user code (the former language/the former HW-SAFE.md (removed in cleanup; see git history) (removed in cleanup; see git history)
            // §2). The name resolves against `--hw-profile` (E400 if unknown); the
            // handle is an immortal descriptor object (plain Ref local — RC no-ops
            // via refcount -1, exactly like string literals). Intercepted BEFORE
            // the base lowers (bare `Mmio` is a type name, not a variable).
            if let Expr::Ident(id, _) = base.as_ref() {
                if id == "Mmio" && name == "bind" {
                    return self.lower_mmio_bind(args);
                }
            }
            let (obj, base_ty) = self.lower_expr(base);
            // `xs.len()` on an array → ArrayLen.
            if name == "len" && args.is_empty() && self.arr_of_operand(&obj).is_some() {
                let l = self.array_len_i64(obj);
                return (l, Ty::I64);
            }
            // MMIO register access `dev.readN(off)` / `dev.writeN(off, v)` on a
            // bound handle → volatile accesses at base+offset (checked E400).
            if self.mmio_of_operand(&obj).is_some() {
                return self.lower_mmio_access(obj, &name, args);
            }
            // Iterator adapters over a $List receiver (fold/map/filter/sum/…).
            if self.class_of_operand(&obj).as_deref() == Some("$List") {
                if let Some(r) = self.lower_iter_adapter(IterSrc::List(obj.clone()), &name, args) {
                    return r;
                }
            }
            // Methods on growing lists ($List) and maps ($Map).
            if let Some(sent) = self.class_of_operand(&obj) {
                // Go-style error context: `result.wrap(msg)`. On `Err` (tag 1) it prepends
                // "msg: " to the (string) error, keeping the message chain; on `Ok` it passes
                // the Result through unchanged. Requires a typed `Result[T, Str]`
                // receiver (E must be `Str` — a wrapped chain IS a string); anything
                // else (including untyped leftovers) is E-error, use `match`.
                if (sent == "Result" || sent.starts_with("Result$")) && name == "wrap" {
                    let (rt, re) = Self::parse_result_class(&sent);
                    if re != "Str" {
                        self.errs.push(format!("error[E100]: `.wrap(msg)` needs `E = Str` (receiver is `Result[{rt}, {re}]`) — a wrapped chain is a string; match typed errors explicitly"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                    if args.len() != 1 {
                        self.errs.push(format!("wrap(msg) takes one Str argument, got {}", args.len()));
                    }
                    let (msg, _) = args.first().map(|e| self.lower_expr(e)).unwrap_or((Operand::ConstNull, Ty::Ref));
                    let tag = self.new_local(Ty::I64);
                    self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: sent.clone(), field: "__tag".into() });
                    let is_err = self.new_local(Ty::I32);
                    self.emit(Statement::Assign(is_err, Rvalue::Binary(IB::CmpEq, Operand::Copy(tag), Operand::ConstI64(1))));
                    let errb = self.new_block();
                    let okb = self.new_block();
                    let merge = self.new_block();
                    let cur = self.cur;
                    self.term(cur, Terminator::Branch { cond: Operand::Copy(is_err), then_blk: errb, else_blk: okb });
                    let res = self.new_local(Ty::Ref);
                    self.local_class.insert(res.0, sent.clone());
                    // Err branch → Err(jrt_err_wrap(msg, old_error)) in the same layout.
                    self.cur = errb.0 as usize;
                    let olderr = self.new_local(Ty::Ref);
                    self.emit(Statement::GetField { dest: olderr, obj: obj.clone(), class: sent.clone(), field: "Err_error".into() });
                    let joined = self.new_local(Ty::Ref);
                    self.emit(Statement::Call { dest: Some(joined), func: "jrt_err_wrap".into(), args: vec![msg, Operand::Copy(olderr)] });
                    let newres = self.new_local(Ty::Ref);
                    self.local_class.insert(newres.0, sent.clone());
                    self.emit(Statement::New { dest: newres, class: sent.clone() });
                    self.emit(Statement::PutField { obj: Operand::Copy(newres), class: sent.clone(), field: "__tag".into(), value: Operand::ConstI64(1) });
                    self.emit(Statement::PutField { obj: Operand::Copy(newres), class: sent.clone(), field: "Err_error".into(), value: Operand::Copy(joined) });
                    self.emit(Statement::Assign(res, Rvalue::Use(Operand::Copy(newres))));
                    self.term(self.cur, Terminator::Goto(merge));
                    // Ok branch → the original Result, unchanged.
                    self.cur = okb.0 as usize;
                    self.emit(Statement::Assign(res, Rvalue::Use(obj.clone())));
                    self.term(self.cur, Terminator::Goto(merge));
                    self.cur = merge.0 as usize;
                    return (Operand::Copy(res), Ty::Ref);
                }
                // `result.error()` — the Err payload as a Str (`""` when Ok,
                // Go's `err.Error()`). Requires `E = Str` (the payload IS the
                // message — no conversion); anything else is E-error, use `match`.
                if (sent == "Result" || sent.starts_with("Result$")) && name == "error" {
                    let (rt, re) = Self::parse_result_class(&sent);
                    if re != "Str" {
                        self.errs.push(format!("error[E100]: `.error()` needs `E = Str` (receiver is `Result[{rt}, {re}]`) — match typed errors explicitly"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                    let tag = self.new_local(Ty::I64);
                    self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: sent.clone(), field: "__tag".into() });
                    let is_err = self.new_local(Ty::I32);
                    self.emit(Statement::Assign(is_err, Rvalue::Binary(IB::CmpEq, Operand::Copy(tag), Operand::ConstI64(1))));
                    let errb = self.new_block();
                    let okb = self.new_block();
                    let merge = self.new_block();
                    self.term(self.cur, Terminator::Branch { cond: Operand::Copy(is_err), then_blk: errb, else_blk: okb });
                    let res = self.new_local(Ty::Ref);
                    self.local_class.insert(res.0, "Str".into());
                    self.cur = errb.0 as usize;
                    let s = self.new_local(Ty::Ref);
                    self.emit(Statement::GetField { dest: s, obj: obj.clone(), class: sent.clone(), field: "Err_error".into() });
                    self.emit(Statement::Assign(res, Rvalue::Use(Operand::Copy(s))));
                    self.term(self.cur, Terminator::Goto(merge));
                    self.cur = okb.0 as usize;
                    let empty = self.intern("");
                    self.emit(Statement::Assign(res, Rvalue::Use(Operand::ConstStr(empty))));
                    self.term(self.cur, Terminator::Goto(merge));
                    self.cur = merge.0 as usize;
                    return (Operand::Copy(res), Ty::Ref);
                }
                // `$Atomic` = a locally-constructed `Atomic(..)`; `Atomic` = a value
                // arriving typed as such (e.g. a worker parameter `c: Atomic`).
                if sent == "$Atomic" || sent == "Atomic" {
                    // `a.fetch_add(d)` (returns the previous value), `a.load()`.
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "fetch_add" | "add" => ("jrt_atomic_add", Ty::I64),
                        "load" | "get" => ("jrt_atomic_get", Ty::I64),
                        _ => {
                            self.errs.push(format!("Atomic has no method `{name}` (fetch_add/load)"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Channel" || sent == "Channel" {
                    // `c.send(v)` enqueues; `c.recv()` blocks for the next value.
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "send" => ("jrt_chan_send", Ty::Void),
                        "recv" => ("jrt_chan_recv", Ty::I64),
                        _ => {
                            self.errs.push(format!("Channel has no method `{name}` (send/recv)"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Mutex" || sent == "Mutex" {
                    // `m.lock()` / `m.unlock()` around a critical section; `m.get()` /
                    // `m.set(v)` read/update the guarded cell.
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "lock" => ("jrt_mutex_lock", Ty::Void),
                        "unlock" => ("jrt_mutex_unlock", Ty::Void),
                        "get" => ("jrt_mutex_get", Ty::I64),
                        "set" => ("jrt_mutex_set", Ty::Void),
                        _ => {
                            self.errs.push(format!("Mutex has no method `{name}` (lock/unlock/get/set)"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$List" {
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "push" => ("vire_list_push", Ty::Void),
                        "pop" => ("vire_list_pop", Ty::I64),
                        "len" => ("vire_list_len", Ty::I64),
                        "get" => ("vire_list_get", Ty::I64),
                        "set" => ("vire_list_set", Ty::Void),
                        "contains" => ("vire_list_contains", Ty::I32),
                        "clear" => ("vire_list_clear", Ty::Void),
                        _ => {
                            self.errs.push(format!("List has no method `{name}`"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Map" {
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "put" => ("vire_map_put", Ty::Void),
                        "get" => ("vire_map_get", Ty::I64),
                        "has" => ("vire_map_has", Ty::I32),
                        "remove" => ("vire_map_remove", Ty::I32),
                        "len" => ("vire_map_len", Ty::I64),
                        _ => {
                            self.errs.push(format!("Map has no method `{name}`"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Set" {
                    // A hash set of Ints (backed by the map runtime). `add`/`remove`
                    // return void/bool, `contains` a bool, `len` the count.
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "add" => ("vire_set_add", Ty::Void),
                        "contains" => ("vire_set_contains", Ty::I64),
                        "remove" => ("vire_set_remove", Ty::I64),
                        "len" => ("vire_set_len", Ty::I64),
                        _ => {
                            self.errs.push(format!("Set has no method `{name}` (add/contains/remove/len)"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
            }
            // STRING methods. A string receiver is a bare `Ty::Ref` carrying no
            // sentinel/class (a literal, a concat/`str()` result, or a `Str`-typed
            // parameter — `class_of` returns `None`/`"Str"`). Route only KNOWN
            // method names to the `jrt_str_*` runtime; anything else falls through
            // to the "annotate the receiver" error so genuine unknown-type calls
            // still fail. Arg kinds: `'i'` = index → i32, else a string ref.
            if base_ty == Ty::Ref
                && self.arr_of_operand(&obj).is_none()
                && self.class_of_operand(&obj).as_deref().map_or(true, |c| c == "Str")
            {
                // (func, Vire result type, arg kinds). The `jrt_str_*` runtime
                // returns i32 for every scalar; an `Int`-typed result (`Ty::I64`)
                // is therefore widened from the i32 the call yields, whereas a
                // `Bool` result (`Ty::I32`) is used verbatim (Vire `Bool` = i32).
                let strm: Option<(&str, Ty, &[char])> = match name.as_str() {
                    "len" | "length" => Some(("jrt_str_length", Ty::I64, &[])),
                    "charAt" | "char_at" => Some(("jrt_str_char_at", Ty::I64, &['i'])),
                    "indexOf" | "index_of" => Some(("jrt_str_indexof", Ty::I64, &['r'])),
                    "compareTo" | "compare_to" => Some(("jrt_str_compareto", Ty::I64, &['r'])),
                    "hashCode" | "hash_code" => Some(("jrt_str_hashcode", Ty::I64, &[])),
                    "isEmpty" | "is_empty" => Some(("jrt_str_is_empty", Ty::I32, &[])),
                    "equals" => Some(("jrt_str_equals", Ty::I32, &['r'])),
                    "startsWith" | "starts_with" => Some(("jrt_str_startswith", Ty::I32, &['r'])),
                    "endsWith" | "ends_with" => Some(("jrt_str_endswith", Ty::I32, &['r'])),
                    "trim" => Some(("jrt_str_trim", Ty::Ref, &[])),
                    "lower" | "toLowerCase" | "to_lower" => Some(("jrt_str_lower", Ty::Ref, &[])),
                    "upper" | "toUpperCase" | "to_upper" => Some(("jrt_str_upper", Ty::Ref, &[])),
                    "jsonEscape" | "json_escape" => Some(("jrt_str_json_escape", Ty::Ref, &[])),
                    "substring" if args.len() == 1 => Some(("jrt_str_substring1", Ty::Ref, &['i'])),
                    "substring" => Some(("jrt_str_substring2", Ty::Ref, &['i', 'i'])),
                    _ => None,
                };
                if let Some((func, ret, kinds)) = strm {
                    let mut all = vec![obj];
                    for (i, e) in args.iter().enumerate() {
                        let (o, t) = self.lower_expr(e);
                        all.push(match kinds.get(i) {
                            Some('i') => self.to_i32(o),
                            _ if t == Ty::Ref => o,
                            _ => to_i64(o),
                        });
                    }
                    // Integer result: the call is i32, widen to Int (i64).
                    if ret == Ty::I64 {
                        let d = self.new_local(Ty::I32);
                        self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                        return (self.widen_i32(Operand::Copy(d)), Ty::I64);
                    }
                    let d = self.new_local(ret);
                    // A string-returning method yields another string (chainable).
                    if ret == Ty::Ref { self.local_class.insert(d.0, "Str".into()); }
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
            }
            let class = match self.class_of_operand(&obj) {
                Some(c) => c,
                None => {
                    self.errs.push(format!("method call `.{name}()`: type of the receiver unknown (annotate it)"));
                    return (Operand::ConstI64(0), Ty::I64);
                }
            };
            // Kernel `Rc$T` handles share T's implementation: method lookup
            // strips the ownership marker (the mangled `T.method` is shared).
            let lookup_class = class.strip_prefix("Rc$").unwrap_or(&class);
            // TRAIT OBJECT: if the receiver is trait-typed (`s: Show`), dispatch
            // dynamically via the vtable (CallVirtual) — the concrete type is only
            // known at runtime. Otherwise a static `Typ.methode` call.
            if let Some(tms) = self.trait_methods.get(&class) {
                if let Some((mn, desc, params, ret)) = tms.iter().find(|(n, ..)| n == name).cloned() {
                    let mut arg_ops = vec![obj];
                    for a in args {
                        arg_ops.push(self.lower_expr(a).0);
                    }
                    let dest = if ret == Ty::Void { None } else { Some(self.new_local(ret)) };
                    self.emit(Statement::CallVirtual { dest, class: class.clone(), name: mn, desc, params, ret, args: arg_ops });
                    return match dest {
                        Some(d) => (Operand::Copy(d), ret),
                        None => (Operand::ConstI64(0), Ty::Void),
                    };
                }
            }
            let sym = format!("{lookup_class}.{name}");
            let mut arg_ops = vec![obj];
            for a in args {
                arg_ops.push(self.lower_expr(a).0);
            }
            let (ret, ret_class, ret_arr, ret_arr_class) = self
                .sigs
                .get(&sym)
                .map(|s| (s.ret, s.ret_class.clone(), s.ret_arr, s.ret_arr_class.clone()))
                .unwrap_or_else(|| {
                    self.errs.push(format!("`{class}` has no method `{name}`"));
                    (Ty::I64, None, None, None)
                });
            if ret == Ty::Void {
                self.emit(Statement::Call { dest: None, func: sym, args: arg_ops });
                return (Operand::ConstI64(0), Ty::Void);
            }
            let d = self.new_local(ret);
            if let Some(c) = ret_class {
                self.local_class.insert(d.0, c);
            }
            if let Some(k) = ret_arr {
                self.local_arr.insert(d.0, k);
            }
            if let Some(c) = ret_arr_class {
                self.local_arr_class.insert(d.0, c);
            }
            self.emit(Statement::Call { dest: Some(d), func: sym, args: arg_ops });
            return (Operand::Copy(d), ret);
        }
        let name = match callee {
            Expr::Ident(n, _) => n.clone(),
            _ => {
                self.errs.push("call target M2: only named functions".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        // REMOVED (AGENTS.md §1): `@gpu`/`@vulkan` device intrinsics are gone.
        // A call to a `gpu_*`/`vk_*`/`gpuvk_*` name is a compile error (not a
        // link-time unresolved symbol), so old programs fail loudly.
        if name.starts_with("gpu_") || name.starts_with("vk_") || name.starts_with("gpuvk_") {
            self.errs.push(format!("removed: `{name}` was a GPU/Vulkan builtin — GPU/Vulkan is out per AGENTS.md §1 (pure CPU project)"));
            return (Operand::ConstI64(0), Ty::I64);
        }
        // Buffer-capture intrinsics for inline C/asm blocks: `@arraydata(a)` yields the
        // raw data pointer (past the 16-byte object header), `@arraylen(a)` the element
        // count. Together they pass a Vire array to a `native "c"` block as a proven
        // (ptr, len) pair (see cblock.rs / the former language/the former VERIFIED-C-ASM.md (removed in cleanup; see git history) (removed in cleanup; see git history)).
        if name == "@arraydata" {
            let a = self.lower_expr(&args[0]).0;
            let d = self.new_local(Ty::Ref);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_array_data".into(), args: vec![a] });
            return (Operand::Copy(d), Ty::Ref);
        }
        if name == "@arraylen" {
            let a = self.lower_expr(&args[0]).0;
            let l = self.array_len_i64(a);
            return (l, Ty::I64);
        }
        // Call of a lambda local `f(args)` → body inline (parameters bound).
        if let Some((l, _)) = self.lookup(&name) {
            if let Some((params, body)) = self.local_lambda.get(&l.0).cloned() {
                self.scopes.push(HashMap::new());
                for (p, arg) in params.iter().zip(args) {
                    let (op, ty) = self.lower_expr(arg);
                    let d = self.new_local(ty);
                    if let Some(c) = self.class_of_operand(&op) {
                        self.local_class.insert(d.0, c);
                    }
                    self.track_ptr(d.0, &op);
                    self.emit(Statement::Assign(d, Rvalue::Use(op)));
                    self.bind(p, d, ty);
                }
                let r = self.lower_expr(&body);
                self.scopes.pop();
                return r;
            }
        }
        // Data-carrying variant of a generic sum type (`Some(3.5)`) →
        // monomorphized type-correctly. Data-less variants (`None`) go through
        // the erased path (only __tag; payload type irrelevant).
        if self.variant_owner_g.contains_key(&name) && !args.is_empty() {
            return self.build_generic_variant(&name, args);
        }
        // Variant constructor of a sum type: `Circle(2.0)` → tagged instance.
        if self.variants.contains_key(&name) {
            return self.build_variant(&name, args);
        }
        // Constructor of a generic product type: `Box(x)` → infer the type arguments
        // from the argument types, instantiate `Box$Float`, New + PutField.
        if let Some((tparams, fields)) = self.generic_ptypes.get(&name).cloned() {
            let lowered: Vec<(Operand, Ty)> = args.iter().map(|a| self.lower_expr(a)).collect();
            // Derive the type parameter from the first field that is exactly `T`.
            let mut tmap: HashMap<String, String> = HashMap::new();
            for (i, f) in fields.iter().enumerate() {
                if tparams.iter().any(|tp| tp == &f.ty.name) {
                    if let Some((op, ty)) = lowered.get(i) {
                        tmap.entry(f.ty.name.clone()).or_insert_with(|| self.ty_name(op, *ty));
                    }
                }
            }
            let targs: Vec<String> = tparams.iter().map(|tp| tmap.get(tp).cloned().unwrap_or_else(|| "Int".into())).collect();
            let mangled = self.instantiate_ptype(&name, &tparams, &fields, &targs);
            let layout = self.local_inst[&mangled].clone();
            let obj = self.new_local(Ty::Ref);
            self.local_class.insert(obj.0, mangled.clone());
            self.emit(Statement::New { dest: obj, class: mangled.clone() });
            if lowered.len() != layout.len() {
                self.errs.push(format!("{name}: expected {} fields, {} given", layout.len(), lowered.len()));
            }
            for ((fname, fty, _), (mut v, _)) in layout.iter().zip(lowered) {
                if *fty == Ty::I64 {
                    v = to_i64(v);
                }
                self.emit(Statement::PutField { obj: Operand::Copy(obj), class: mangled.clone(), field: fname.clone(), value: v });
            }
            return (Operand::Copy(obj), Ty::Ref);
        }
        // Constructor of a user type: `Point(x, y)` → New + PutField per field
        // (field order = declaration order).
        if let Some(layout) = self.types.get(&name).cloned() {
            let obj = self.new_local(Ty::Ref);
            self.local_class.insert(obj.0, name.clone());
            self.emit(Statement::New { dest: obj, class: name.clone() });
            if args.len() != layout.len() {
                self.errs.push(format!("{name}: expected {} fields, {} given", layout.len(), args.len()));
            }
            for ((fname, fty, _), arg) in layout.iter().zip(args) {
                let (mut v, _) = self.lower_expr(arg);
                if *fty == Ty::I64 {
                    v = to_i64(v);
                }
                self.emit(Statement::PutField {
                    obj: Operand::Copy(obj),
                    class: name.clone(),
                    field: fname.clone(),
                    value: v,
                });
            }
            return (Operand::Copy(obj), Ty::Ref);
        }
        // Higher-order: if a lambda is passed, expand the called
        // function inline at this spot (parameters bound, lambda as
        // local_lambda). BEFORE the eager argument lowering — a lambda is not a
        // value and must not be lowered directly.
        if args.iter().any(|a| matches!(a, Expr::Lambda { .. })) {
            if let Some(fdef) = self.fn_defs.get(&name).cloned() {
                if fdef.body.is_some() {
                    return self.inline_higher_order(&name, &fdef, args);
                }
            }
        }
        // DEVIRT: if a CONCRETELY-typed object is passed to a trait parameter
        // (`run(a, …)` with `a: AddOp`, param `o: Op`), expand the function
        // inline → in the body `o` is concrete → `o.apply()` becomes a STATIC
        // call (no vtable/type check). This is g++'s devirtualization gain,
        // done cleanly in the closed-world solver. Only for a small body (bloat).
        if let Some(fdef) = self.fn_defs.get(&name).cloned() {
            if fdef.body.is_some() && self.devirt_inline_ok(&fdef, args) {
                return self.inline_higher_order(&name, &fdef, args);
            }
        }
        let lowered: Vec<(Operand, Ty)> = args.iter().map(|a| self.lower_expr(a)).collect();
        // `sqrt(x)` — floating-point square root. Lowers to `jrt_math_sqrt`, which the
        // backend emits as the LLVM intrinsic `@llvm.sqrt.f64` (a single `sqrtsd`).
        // The argument is coerced to `Float`.
        if name == "sqrt" && lowered.len() == 1 {
            let (a, at) = lowered.into_iter().next().unwrap();
            let af = if at == Ty::F64 {
                a
            } else {
                let f = self.new_local(Ty::F64);
                self.emit(Statement::Assign(f, Rvalue::Convert(a)));
                Operand::Copy(f)
            };
            let d = self.new_local(Ty::F64);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_math_sqrt".into(), args: vec![af] });
            return (Operand::Copy(d), Ty::F64);
        }
        // `sin(x)` / `cos(x)` / `floor(x)` — the backend emits the LLVM intrinsic
        // @llvm.sin/cos/floor.f64. The argument is coerced to Float; the result is Float.
        if (name == "sin" || name == "cos" || name == "floor") && lowered.len() == 1 {
            let (a, at) = lowered.into_iter().next().unwrap();
            let af = if at == Ty::F64 {
                a
            } else {
                let f = self.new_local(Ty::F64);
                self.emit(Statement::Assign(f, Rvalue::Convert(a)));
                Operand::Copy(f)
            };
            let func = match name.as_str() { "sin" => "jrt_math_sin", "cos" => "jrt_math_cos", _ => "jrt_math_floor" };
            let d = self.new_local(Ty::F64);
            self.emit(Statement::Call { dest: Some(d), func: func.into(), args: vec![af] });
            return (Operand::Copy(d), Ty::F64);
        }
        // Declared `syscall NAME(…)`: lowers to `syscall(NUM, args…)` via libc —
        // no inline asm, no user-written wrapper (the former language/the former HW-SAFE.md (removed in cleanup; see git history) (removed in cleanup; see git history) §2). `Int`
        // args pass directly (I32 widens); `Array[Byte]` args pass the data
        // pointer (the length stays an explicit user argument). At most 6 args
        // (checked at declaration); the backend declares libc `syscall`.
        if let Some(info) = self.syscalls.get(&name).cloned() {
            if lowered.len() != info.params.len() {
                self.errs.push(format!(
                    "error[E421]: `syscall {name}` takes {} arguments, got {}",
                    info.params.len(),
                    lowered.len()
                ));
                return (Operand::ConstI64(0), Ty::I64);
            }
            let mut call_args = vec![Operand::ConstI64(info.num as i64)];
            for ((op, ty), kind) in lowered.into_iter().zip(info.params.iter()) {
                match kind {
                    SysParamKind::Int => {
                        if ty == Ty::I32 {
                            let w = self.new_local(Ty::I64);
                            self.emit(Statement::Assign(w, Rvalue::Convert(op)));
                            call_args.push(Operand::Copy(w));
                        } else {
                            call_args.push(op);
                        }
                    }
                    SysParamKind::Buf => {
                        if self.arr_of_operand(&op).is_none() {
                            self.errs.push(format!(
                                "error[E421]: `syscall {name}` buffer argument must be an `Array[Byte]` (got a non-array value)"
                            ));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                        let ptr = self.new_local(Ty::Ref);
                        self.emit(Statement::Call { dest: Some(ptr), func: "jrt_array_data".into(), args: vec![op] });
                        call_args.push(Operand::Copy(ptr));
                    }
                }
            }
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "syscall".into(), args: call_args });
            return (Operand::Copy(d), Ty::I64);
        }
        // Raw port I/O + compiler fences WITHOUT inline asm (the former language/the former HW-SAFE.md (removed in cleanup; see git history) (removed in cleanup; see git history)
        // §2, prozedural statt OO: plain functions, no classes). `port_outb(p, v)`
        // / `port_inb(p)` lower to generated per-program helpers (`__vire_port_*`,
        // emitted by the driver into the program's own TU — the single justified
        // toolchain-side `asm`, never user code). `fence()` (+`_acq`/`_rel`) lower
        // to generated functions containing a single LLVM fence (no asm at all).
        if name == "port_outb" {
            if lowered.len() != 2 {
                self.errs.push(format!("error[E421]: `port_outb` takes 2 arguments (port, value), got {}", lowered.len()));
                return (Operand::ConstI64(0), Ty::I64);
            }
            let args64: Vec<Operand> = lowered
                .into_iter()
                .map(|(op, ty)| {
                    if ty == Ty::I32 {
                        let w = self.new_local(Ty::I64);
                        self.emit(Statement::Assign(w, Rvalue::Convert(op)));
                        Operand::Copy(w)
                    } else {
                        op
                    }
                })
                .collect();
            self.emit(Statement::Call { dest: None, func: "__vire_port_outb".into(), args: args64 });
            return (Operand::ConstI64(0), Ty::Void);
        }
        if name == "port_inb" {
            if lowered.len() != 1 {
                self.errs.push(format!("error[E421]: `port_inb` takes 1 argument (port), got {}", lowered.len()));
                return (Operand::ConstI64(0), Ty::I64);
            }
            let (op, ty) = lowered.into_iter().next().unwrap();
            let port = if ty == Ty::I32 {
                let w = self.new_local(Ty::I64);
                self.emit(Statement::Assign(w, Rvalue::Convert(op)));
                Operand::Copy(w)
            } else {
                op
            };
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "__vire_port_inb".into(), args: vec![port] });
            return (Operand::Copy(d), Ty::I64);
        }
        if name == "fence" || name == "fence_acq" || name == "fence_rel" {
            if !lowered.is_empty() {
                self.errs.push(format!("error[E421]: `{name}` takes no arguments, got {}", lowered.len()));
                return (Operand::ConstI64(0), Ty::I64);
            }
            let sym = match name.as_str() {
                "fence" => "__vire_fence",
                "fence_acq" => "__vire_fence_acq",
                _ => "__vire_fence_rel",
            };
            self.emit(Statement::Call { dest: None, func: sym.into(), args: vec![] });
            return (Operand::ConstI64(0), Ty::Void);
        }
        // `ctz(x)` / `popcount(x)` — bit intrinsics on Int (i64). The backend emits
        // @llvm.cttz.i64(x, i1 false) (so ctz(0) = 64, defined — no UB) and
        // @llvm.ctpop.i64(x), each a single instruction (tzcnt / popcnt on x86).
        // Guarded by `fn_defs`: a user's own `fn ctz` takes precedence over the builtin.
        if (name == "ctz" || name == "popcount") && lowered.len() == 1 && !self.fn_defs.contains_key(&name) {
            let (a, _at) = lowered.into_iter().next().unwrap();
            let ai = to_i64(a);
            let d = self.new_local(Ty::I64);
            let func = if name == "ctz" { "jrt_ctz" } else { "jrt_popcount" };
            self.emit(Statement::Call { dest: Some(d), func: func.into(), args: vec![ai] });
            return (Operand::Copy(d), Ty::I64);
        }
        // Sized typed arrays: `array(n)` (Int), `farray(n)` (Float), `barray(n)` (unsigned
        // byte, 0..255) — real bounds-checked/-elidable arrays (as opposed to the i64 list).
        // `barray` is the byte buffer for scanning file bytes in Vire (grep, binary I/O): one
        // byte per element (not 8), `a[i]` is a zero-extended `load i8`.
        if name == "array" || name == "farray" || name == "barray" {
            let n = lowered.into_iter().next().map(|(o, _)| o).unwrap_or(Operand::ConstI64(0));
            let len32 = self.to_i32(n);
            let arr = self.new_local(Ty::Ref);
            // `mut x: Array[Cls] = array(n)` — a REF array of objects: every slot holds a
            // `Cls` reference (default null). The element class is recorded so `x[i]`
            // resolves to `Cls` and `x[i] = e` is class-checked. `farray`/`barray` are
            // always scalar (float / byte) and ignore the hint.
            if name == "array" {
                if let Some(cls) = self.arr_hint.take() {
                    self.local_arr.insert(arr.0, ArrKind::Ref);
                    self.local_arr_class.insert(arr.0, cls);
                    self.emit(Statement::NewArray { dest: arr, kind: ArrKind::Ref, len: len32 });
                    return (Operand::Copy(arr), Ty::Ref);
                }
            }
            let kind = match name.as_str() { "farray" => ArrKind::Double, "barray" => ArrKind::U8, _ => ArrKind::Long };
            self.local_arr.insert(arr.0, kind);
            self.emit(Statement::NewArray { dest: arr, kind, len: len32 });
            return (Operand::Copy(arr), Ty::Ref);
        }
        // `str_from(a, start, len)` — build a Str from a byte array's [start, start+len) range,
        // OR from a ByteView's [start, start+len) mmap bytes (zero-copy). Bounds-clamped in the
        // runtime (sound). The inverse of scanning: emit a matched line.
        if name == "str_from" && args.len() == 3 {
            let arr = self.lower_expr(&args[0]).0;
            let s0 = self.lower_expr(&args[1]).0; let start = self.to_i32(s0);
            let l0 = self.lower_expr(&args[2]).0; let len = self.to_i32(l0);
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "Str".into());
            if self.class_of_operand(&arr).as_deref() == Some("ByteView") {
                // The view carries (ptr, len); the runtime reads both and clamps to len.
                self.emit(Statement::Call { dest: Some(d), func: "jrt_bview_str".into(), args: vec![arr, start, len] });
            } else {
                let ptr = self.new_local(Ty::Ref);
                self.emit(Statement::Call { dest: Some(ptr), func: "jrt_array_data".into(), args: vec![arr.clone()] });
                let alen = self.array_len_i64(arr);
                let alen32 = self.to_i32(alen);
                self.emit(Statement::Call { dest: Some(d), func: "jrt_str_from_bytes".into(), args: vec![Operand::Copy(ptr), alen32, start, len] });
            }
            return (Operand::Copy(d), Ty::Ref);
        }
        // `find_byte(a, from, byte)` — index of the first `byte` at/after `from`, or -1 (SIMD
        // memchr; sound, fast). Works over a byte array OR a ByteView (mmap bytes, zero-copy) —
        // the rare-byte prefilter that keeps a Vire byte scan at ripgrep speed.
        if name == "find_byte" && args.len() == 3 {
            let arr = self.lower_expr(&args[0]).0;
            let f0 = self.lower_expr(&args[1]).0; let from = self.to_i32(f0);
            let b0 = self.lower_expr(&args[2]).0; let byte = self.to_i32(b0);
            let d = self.new_local(Ty::I64);
            if self.class_of_operand(&arr).as_deref() == Some("ByteView") {
                self.emit(Statement::Call { dest: Some(d), func: "jrt_bview_find".into(), args: vec![arr, from, byte] });
            } else {
                let ptr = self.new_local(Ty::Ref);
                self.emit(Statement::Call { dest: Some(ptr), func: "jrt_array_data".into(), args: vec![arr.clone()] });
                let alen = self.array_len_i64(arr);
                let alen32 = self.to_i32(alen);
                self.emit(Statement::Call { dest: Some(d), func: "jrt_find_byte".into(), args: vec![Operand::Copy(ptr), alen32, from, byte] });
            }
            return (Operand::Copy(d), Ty::I64);
        }
        // `bview(ptr, len)` — a bounds-checked, ZERO-COPY window over an external byte region (an
        // mmap pointer from C). SAFE replacement for peek_u8/find_byte_ptr/str_from_ptr: `len` is
        // carried once here (the single FFI trust point, like `cstr`), and every later access —
        // `v[i]`, `find_byte(v,…)`, `str_from(v,…)` — is checked against it. Represented as a
        // 2-element i64 array `[ptr, len]` (a real GC-managed array; the mmap bytes are never
        // copied), tagged class "ByteView" so those ops dispatch to the checked runtime path.
        if name == "bview" && args.len() == 2 {
            let p0 = self.lower_expr(&args[0]).0; let p = to_i64(p0.clone());
            // E421: `bview` is the single trust point for external bytes — the
            // pointer must be proven (`cstr`, an extern `Ptr` result, …), never a
            // forged integer (the old `peek_u8`-class hole in a new shape).
            if !self.ptr_of_operand(&p0) {
                self.errs.push("error[E421]: `bview(ptr, len)` needs a proven `Ptr` (from `cstr`, an extern call returning `Ptr`, or the null literal) — a number is never an address (AGENTS.md §3)".into());
            }
            let l0 = self.lower_expr(&args[1]).0; let l = to_i64(l0);
            let arr = self.new_local(Ty::Ref);
            self.local_arr.insert(arr.0, ArrKind::Long);
            self.emit(Statement::NewArray { dest: arr, kind: ArrKind::Long, len: Operand::ConstI32(2) });
            self.emit(Statement::ArrayStore { arr: Operand::Copy(arr), index: Operand::ConstI32(0), value: p, kind: ArrKind::Long, checked: false });
            self.emit(Statement::ArrayStore { arr: Operand::Copy(arr), index: Operand::ConstI32(1), value: l, kind: ArrKind::Long, checked: false });
            // Overwrite the array-kind tag with the view class: indexing dispatches to the
            // checked byte load, not a raw i64 load of the [ptr, len] metadata.
            self.local_arr.remove(&arr.0);
            self.local_class.insert(arr.0, "ByteView".into());
            return (Operand::Copy(arr), Ty::Ref);
        }
        // `bview_len(v)` — the view's byte length (its loop bound).
        if name == "bview_len" && args.len() == 1 {
            let v = self.lower_expr(&args[0]).0;
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_bview_len".into(), args: vec![v] });
            return (Operand::Copy(d), Ty::I64);
        }
        // Collection builtins: `list()` (growing list), `map()` (Int→Int),
        // `set()` (Int hash set).
        if name == "list" || name == "map" || name == "set" {
            let (func, sentinel) = match name.as_str() {
                "list" => ("vire_list_new", "$List"),
                "map" => ("vire_map_new", "$Map"),
                _ => ("vire_set_new", "$Set"),
            };
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, sentinel.into());
            self.emit(Statement::Call { dest: Some(d), func: func.into(), args: vec![] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // `Atomic(v)` → shared atomic counter (a `$Atomic` ref; immortal, thread-safe).
        if name == "Atomic" {
            let init = lowered.into_iter().next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "$Atomic".into());
            self.emit(Statement::Call { dest: Some(d), func: "jrt_atomic_new".into(), args: vec![init] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // `Mutex(v)` → a lock-guarded cell (a `$Mutex` ref; immortal).
        if name == "Mutex" {
            let init = lowered.into_iter().next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "$Mutex".into());
            self.emit(Statement::Call { dest: Some(d), func: "jrt_mutex_new".into(), args: vec![init] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // `Channel()` → a thread-safe FIFO queue (a `$Channel` ref; immortal).
        if name == "Channel" {
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "$Channel".into());
            self.emit(Statement::Call { dest: Some(d), func: "jrt_chan_new".into(), args: vec![] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // `join(h)` → wait for the spawned thread, yield its result.
        if name == "join" {
            let h = lowered.into_iter().next().map(|(o, _)| o).unwrap_or(Operand::ConstNull);
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_join".into(), args: vec![h] });
            return (Operand::Copy(d), Ty::I64);
        }
        // Builtin `str(x)` → text representation (Ref).
        if name == "str" {
            let (op, ty) = lowered.into_iter().next().unwrap_or((Operand::ConstNull, Ty::Ref));
            return (self.to_str(op, ty), Ty::Ref);
        }
        // ── Reflection API (P0 of the dynamic-runtime plan, the former DYNAMIC-VIRE-PLAN.md (removed in cleanup; see git history) §3).
        // Static, typed introspection resolved at compile time from the type graph — no
        // runtime metadata table yet (that lands with dynamic loading, P1, when a loaded
        // type's descriptor must be read at runtime). In a sealed build a value's runtime
        // type equals its static class, so these are exact today.
        // `type_name(x)` → the class name of `x` as a Str (scalars → their type name).
        if name == "type_name" {
            let (arg, ty) = lowered.into_iter().next().unwrap_or((Operand::ConstNull, Ty::Ref));
            let nm = match self.class_of_operand(&arg) {
                Some(c) => c,
                None => match ty {
                    Ty::F64 | Ty::F32 => "Float",
                    Ty::Ref => "Ref",
                    _ => "Int",
                }
                .to_string(),
            };
            let id = self.intern(&nm);
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "Str".into());
            self.emit(Statement::Assign(d, Rvalue::Use(Operand::ConstStr(id))));
            return (Operand::Copy(d), Ty::Ref);
        }
        // `field_count(x)` → number of declared fields of `x`'s type (0 for a scalar /
        // classless ref). Excludes the synthetic sum-type `__tag` slot.
        if name == "field_count" {
            let (arg, _) = lowered.into_iter().next().unwrap_or((Operand::ConstNull, Ty::Ref));
            let n = self
                .class_of_operand(&arg)
                .and_then(|c| self.layout_of(&c))
                .map(|l| l.iter().filter(|(nm, ..)| nm != "__tag").count() as i64)
                .unwrap_or(0);
            return (Operand::ConstI64(n), Ty::I64);
        }
        // `abi_version()` → the frozen dynamic-ABI version the binary was built with; the
        // anchor a loaded module's manifest will be pinned against (the former DYNAMIC-VIRE-PLAN.md (removed in cleanup; see git history) §3).
        if name == "abi_version" {
            return (Operand::ConstI64(VIRE_ABI_VERSION), Ty::I64);
        }
        // ── Dynamic module loading (M1, scalar-only) — the former DYNAMIC-VIRE-PLAN.md (removed in cleanup; see git history) P1.
        // `load_module(path: Str) -> Int` loads a prebuilt native module (`vire
        // --emit-module`), checks its ABI version, and returns an opaque handle (0 =
        // failed / ABI mismatch). `module_call(handle, arg: Int) -> Int` calls its scalar
        // entry. Scalar-in/-out only — no object crosses the boundary, so no cross-module
        // RC/arena concern (the scalar-capsule discipline).
        if name == "load_module" {
            let (arg, _) = lowered.into_iter().next().unwrap_or((Operand::ConstNull, Ty::Ref));
            let cs = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(cs), func: "vire_cstr".into(), args: vec![arg] });
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_load_module".into(), args: vec![Operand::Copy(cs)] });
            return (Operand::Copy(d), Ty::I64);
        }
        if name == "module_call" {
            let mut it = lowered.into_iter();
            let h = it.next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let a = it.next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_module_call".into(), args: vec![h, a] });
            return (Operand::Copy(d), Ty::I64);
        }
        // ── Step 2b: the OBJECT boundary. `module_call_obj(handle, o: T) -> T` hands a
        // live object to the loaded module's `fn module_obj(o: T) -> T` and takes its
        // result back. Two things make this sound, and neither is checked here:
        //   * The LAYOUT of `T` is proven identical on both sides by the load-time
        //     verifier (`jrt_verify_module`) — a module whose `T` differs was already
        //     rejected at `load_module`, so `handle` cannot name one.
        //   * The COUNTS are real: the module links no runtime of its own, so its
        //     retain/release/alloc are the host's. Argument is borrowed (+0), result is
        //     owned (+1) — the ordinary Vire calling convention, which is why the result
        //     needs no special cleanup here.
        // The result's class is the ARGUMENT's class: `module_obj` is `T -> T` by
        // signature, so there is nothing else it could be, and this keeps `r.field`
        // resolving in the host without a second type channel across the boundary.
        if name == "module_call_obj" {
            let mut it = lowered.into_iter();
            let h = it.next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let (obj, _) = it.next().unwrap_or((Operand::ConstNull, Ty::Ref));
            let arg_cls = match &obj {
                Operand::Copy(l) => self.local_class.get(&l.0).cloned(),
                _ => None,
            };
            let d = self.new_local(Ty::Ref);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_module_call_obj".into(), args: vec![h, obj] });
            if let Some(c) = arg_cls {
                self.local_class.insert(d.0, c);
            }
            return (Operand::Copy(d), Ty::Ref);
        }
        // `unload_module(handle) -> Int` — release the load reference. The module's `.so` is
        // dlclose'd (code unmapped) once no dyn-slot references it either, so a stale module
        // is reclaimed (no leak) but never closed while a slot still points into it (no UAF).
        if name == "unload_module" {
            let h = lowered.into_iter().next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_unload_module".into(), args: vec![h] });
            return (Operand::Copy(d), Ty::I64);
        }
        // `install_override(handle: Int, "fn_name") -> Int` (1 = installed, 0 = symbol not
        // found): write a loaded module's `vire_override_<name>` into the host's runtime slot
        // for the `dynamic fn` named `fn_name`, so subsequent calls dispatch to it. The slot
        // index is the fn's SORTED position among the dynamic fns (matches `@jrt_dynslot`).
        if name == "install_override" {
            let handle = lowered.into_iter().next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let fname = match args.get(1) {
                Some(Expr::Str(s, _)) => s.clone(),
                _ => {
                    self.errs.push("install_override(handle, \"name\"): the 2nd argument must be a string literal naming a `dynamic fn`".into());
                    String::new()
                }
            };
            let mut dl: Vec<String> = self
                .fn_defs
                .values()
                .filter(|fd| fd.attrs.iter().any(|a| a.name == "__dynamic"))
                .map(|fd| fd.sig.name.clone())
                .collect();
            dl.sort();
            let slot = match dl.iter().position(|n| n == &fname) {
                Some(i) => i as i64,
                None => {
                    if !fname.is_empty() {
                        self.errs.push(format!("install_override: `{fname}` is not a `dynamic fn`/`open fn`"));
                    }
                    0
                }
            };
            let sym = self.intern(&format!("vire_override_{fname}"));
            let cs = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(cs), func: "vire_cstr".into(), args: vec![Operand::ConstStr(sym)] });
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_dyn_install".into(), args: vec![Operand::ConstI64(slot), handle, Operand::Copy(cs)] });
            return (Operand::Copy(d), Ty::I64);
        }
        // `patch("target", handle, "repl") -> Int` (1 = ok) — hot-update: redirect the
        // `@patchable fn` `target`'s entry to the loaded module's `repl` by rewriting its
        // reserved NOP sled with a `jmp` (the former DYNAMIC-VIRE-PATCH.md (removed in cleanup; see git history) B1). `unpatch("target")`
        // restores the original entry byte-identically. Both names are string literals.
        if name == "patch" || name == "unpatch" {
            let target = match args.first() {
                Some(Expr::Str(s, _)) => s.clone(),
                _ => {
                    self.errs.push(format!("{name}(\"target\", …): the target must be a string literal naming a `@patchable fn`"));
                    String::new()
                }
            };
            if !target.is_empty()
                && !self.fn_defs.get(&target).is_some_and(|fd| fd.attrs.iter().any(|a| a.name == "patchable"))
            {
                self.errs.push(format!("{name}: `{target}` is not a `@patchable fn`"));
            }
            let t_id = self.intern(&target);
            let ct = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(ct), func: "vire_cstr".into(), args: vec![Operand::ConstStr(t_id)] });
            let d = self.new_local(Ty::I64);
            if name == "unpatch" {
                self.emit(Statement::Call { dest: Some(d), func: "jrt_unpatch".into(), args: vec![Operand::Copy(ct)] });
            } else {
                let handle = lowered.into_iter().nth(1).map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
                let repl = match args.get(2) {
                    Some(Expr::Str(s, _)) => s.clone(),
                    _ => {
                        self.errs.push("patch(\"target\", handle, \"repl\"): the 3rd argument must be a string literal".into());
                        String::new()
                    }
                };
                let r_id = self.intern(&format!("vire_override_{repl}"));
                let cr = self.new_local(Ty::I64);
                self.emit(Statement::Call { dest: Some(cr), func: "vire_cstr".into(), args: vec![Operand::ConstStr(r_id)] });
                self.emit(Statement::Call { dest: Some(d), func: "jrt_patch".into(), args: vec![Operand::Copy(ct), handle, Operand::Copy(cr)] });
            }
            return (Operand::Copy(d), Ty::I64);
        }
        // `field_name(x, i)` / `field_type(x, i)` → the i-th declared field's name resp.
        // type name (as a Str). `i` must be a COMPILE-TIME CONSTANT (the layout is static;
        // a runtime index would need the descriptor table, P1). Exact from the type graph;
        // out-of-range or non-constant `i` is a loud compile error. (`size_of` is
        // deliberately NOT here: an exact instance size needs the backend's post-narrowing
        // struct layout, so it lands with the descriptor table in P1, not a guess now.)
        if name == "field_name" || name == "field_type" {
            let mut it = lowered.into_iter();
            let (arg, _) = it.next().unwrap_or((Operand::ConstNull, Ty::Ref));
            let i = match it.next().map(|(o, _)| o) {
                Some(Operand::ConstI64(i)) => i,
                Some(Operand::ConstI32(i)) => i as i64,
                _ => {
                    self.errs.push(format!("{name}(x, i): the index i must be a constant integer"));
                    0
                }
            };
            let fields: Vec<(String, Ty, Option<String>)> = self
                .class_of_operand(&arg)
                .and_then(|c| self.layout_of(&c))
                .map(|l| l.into_iter().filter(|(nm, ..)| nm != "__tag").collect())
                .unwrap_or_default();
            let s = if i >= 0 && (i as usize) < fields.len() {
                let (fname, fty, fclass) = &fields[i as usize];
                if name == "field_name" {
                    fname.clone()
                } else {
                    match fclass {
                        Some(c) => c.clone(),
                        None => match fty {
                            Ty::F64 | Ty::F32 => "Float",
                            Ty::Ref => "Ref",
                            _ => "Int",
                        }
                        .to_string(),
                    }
                }
            } else {
                self.errs.push(format!("{name}(x, {i}): field index out of range (type has {} fields)", fields.len()));
                String::new()
            };
            let id = self.intern(&s);
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "Str".into());
            self.emit(Statement::Assign(d, Rvalue::Use(Operand::ConstStr(id))));
            return (Operand::Copy(d), Ty::Ref);
        }
        // FFI builtin `cstr(s)` → a NUL-terminated `char*` (as `Ptr`/i64) for extern-C
        // functions expecting `const char*`. Requires a Str: a bare `Ty::Ref` that is not an
        // array and carries no non-`Str` class (a literal, a concat/`str()` result, or a
        // `Str`-typed param — same test the `jrt_str_*` method dispatch uses). Passing anything
        // else (an Int, an array, a classed object) to `vire_cstr(JStr*)` would be a wrong-type
        // pointer → garbage/crash, so reject it at compile time.
        if name == "cstr" {
            if lowered.len() != 1 {
                self.errs.push(format!("cstr(s) takes exactly one argument, got {}", lowered.len()));
            }
            let (arg, aty) = lowered.into_iter().next().unwrap_or((Operand::ConstNull, Ty::Ref));
            let is_str = aty == Ty::Ref
                && self.arr_of_operand(&arg).is_none()
                && self.class_of_operand(&arg).as_deref().map_or(true, |c| c == "Str");
            if !is_str {
                self.errs.push("cstr(s) expects a Str argument".into());
            }
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "vire_cstr".into(), args: vec![arg] });
            // `cstr` returns a checked pointer: provenance for `Ptr` sinks.
            self.ptr_locals.insert(d.0);
            return (Operand::Copy(d), Ty::I64);
        }
        // Intrinsic `print` — multi-argument: each argument on its own line.
        if name == "print" {
            if lowered.is_empty() {
                let empty = self.intern("");
                self.emit(Statement::Call { dest: None, func: "jrt_println_str".into(), args: vec![Operand::ConstStr(empty)] });
            }
            for (op, ty) in lowered {
                let func = match ty {
                    Ty::F64 | Ty::F32 => "jrt_println_double",
                    Ty::Ref => "jrt_println_str",
                    _ => "jrt_println_long",
                };
                let arg = if matches!(ty, Ty::F64 | Ty::F32 | Ty::Ref) { op } else { to_i64(op) };
                self.emit(Statement::Call { dest: None, func: func.to_string(), args: vec![arg] });
            }
            return (Operand::ConstI64(0), Ty::Void);
        }
        // Call of a generic function → bind the type arguments from the argument
        // types, request the monomorph instance `f$T…`, call on the instance.
        if let Some(g) = self.generics.get(&name).cloned() {
            let mut bind: HashMap<String, String> = HashMap::new();
            for (i, pty) in g.param_tys.iter().enumerate() {
                if let Some(t) = pty {
                    if g.tparams.contains(&t.name) {
                        if let Some((op, ty)) = lowered.get(i) {
                            let cls = self.class_of_operand(op);
                            bind.entry(t.name.clone()).or_insert_with(|| concrete_tyname(*ty, cls.as_ref()));
                        }
                    }
                }
            }
            let targs: Vec<String> = g.tparams.iter().map(|tp| bind.get(tp).cloned().unwrap_or_else(|| "Int".into())).collect();
            let sym = mono_sym(&name, &targs);
            self.mono.push((name.clone(), targs.clone()));
            let ret_sub = g.ret.as_ref().map(|t| subst_type(t, &bind));
            let ret = ret_sub.as_ref().map(|t| ty_of(Some(t))).unwrap_or(Ty::Void);
            let ret_class = ret_sub.as_ref().and_then(|t| class_of(Some(t)));
            let (ret_arr, ret_arr_class) = arr_ret_generic(ret_sub.as_ref());
            let arg_ops: Vec<Operand> = lowered.into_iter().map(|(o, _)| o).collect();
            if ret == Ty::Void {
                self.emit(Statement::Call { dest: None, func: sym, args: arg_ops });
                return (Operand::ConstI64(0), Ty::Void);
            }
            let d = self.new_local(ret);
            if let Some(c) = ret_class {
                self.local_class.insert(d.0, c);
            }
            if let Some(k) = ret_arr {
                self.local_arr.insert(d.0, k);
            }
            if let Some(c) = ret_arr_class {
                self.local_arr_class.insert(d.0, c);
            }
            self.emit(Statement::Call { dest: Some(d), func: sym, args: arg_ops });
            return (Operand::Copy(d), ret);
        }
        // Call of an own function
        let (ret, ret_class, ret_arr, ret_arr_class, ptr_params, ptr_ret, is_extern) = self
            .sigs
            .get(&name)
            .map(|s| (s.ret, s.ret_class.clone(), s.ret_arr, s.ret_arr_class.clone(), s.ptr_params.clone(), s.ptr_ret, s.is_extern))
            .unwrap_or((Ty::I64, None, None, None, vec![], false, false));
        // E421 (AGENTS.md §3): a `Ptr` parameter takes only proven pointers (a
        // `Ptr` value or the null literal `0`) — never a forged integer. This
        // holds at the C boundary AND at user functions (whose bodies trust a
        // `Ptr` parameter). The check runs on the lowered operands so copies
        // (`mut q = p`), `cstr` results and `0` pass, anything else fails loudly.
        if ptr_params.iter().any(|b| *b) {
            for (i, is_ptr) in ptr_params.iter().enumerate() {
                if *is_ptr {
                    let proven = lowered.get(i).map(|(o, _)| self.ptr_of_operand(o)).unwrap_or(true);
                    if !proven {
                        let what = if is_extern { format!("extern `{name}`") } else { format!("`{name}`") };
                        self.errs.push(format!("error[E421]: argument {} of {what} is `Ptr` but the value is a non-pointer integer — a number is never an address (AGENTS.md §3). Pass a `Ptr` source (`cstr`, an extern call returning `Ptr`, a `Ptr` parameter) or the null literal `0`", i + 1));
                    }
                }
            }
        }
        // Convenience: for `py_*` bridge functions, string arguments are
        // automatically turned into C strings (`cstr`), so that one can write
        // `py_import("math")` instead of `py_import(cstr("math"))`.
        let auto_cstr = name.starts_with("py_");
        let arg_ops: Vec<Operand> = lowered
            .into_iter()
            .map(|(o, t)| {
                if auto_cstr && t == Ty::Ref {
                    let d = self.new_local(Ty::I64);
                    self.emit(Statement::Call { dest: Some(d), func: "vire_cstr".into(), args: vec![o] });
                    Operand::Copy(d)
                } else {
                    o
                }
            })
            .collect();
        if ret == Ty::Void {
            self.emit(Statement::Call { dest: None, func: name, args: arg_ops });
            (Operand::ConstI64(0), Ty::Void)
        } else {
            let d = self.new_local(ret);
            if let Some(c) = ret_class {
                self.local_class.insert(d.0, c); // object return: remember the class
            }
            // An extern call returning `Ptr` yields a checked pointer: provenance
            // for `Ptr` sinks (so helpers can thread `Ptr` through, checked at
            // their own `-> Ptr` return).
            if ptr_ret {
                self.ptr_locals.insert(d.0);
            }
            // Array return: remember the element kind (+ class for a ref array) so
            // `f()[i]` / `f()[i].field` resolve like a local/param array.
            if let Some(k) = ret_arr {
                self.local_arr.insert(d.0, k);
            }
            if let Some(c) = ret_arr_class {
                self.local_arr_class.insert(d.0, c);
            }
            self.emit(Statement::Call { dest: Some(d), func: name, args: arg_ops });
            (Operand::Copy(d), ret)
        }
    }

    /// Expand a higher-order function inline: `apply(x -> x+1, 5)` →
    /// body of `apply` at the call site, `f` bound as local_lambda,
    /// value parameters as locals. Fully specialized (direct code, LLVM can
    /// inline further); the lambda's captures stay visible via the scope stack.
    /// Is a devirt inline expansion worthwhile? Yes, if a CONCRETELY-typed
    /// object goes to a trait parameter (then the method call in the body becomes
    /// static) AND the body is small enough (code-bloat bound).
    fn devirt_inline_ok(&self, fdef: &FnDef, args: &[Expr]) -> bool {
        let has_devirt = fdef.sig.params.iter().zip(args).any(|(p, a)| {
            let is_trait_param = p.ty.as_ref().map(|t| self.trait_methods.contains_key(&t.name)).unwrap_or(false);
            if !is_trait_param {
                return false;
            }
            if let Expr::Ident(n, _) = a {
                if let Some((l, _)) = self.lookup(n) {
                    if let Some(c) = self.local_class.get(&l.0) {
                        // concrete class (not a trait) → the call becomes static.
                        return !self.trait_methods.contains_key(c);
                    }
                }
            }
            false
        });
        has_devirt && fdef.body.as_ref().map(|b| b.stmts.len() <= 24).unwrap_or(false)
    }

    fn inline_higher_order(&mut self, name: &str, fdef: &FnDef, args: &[Expr]) -> (Operand, Ty) {
        let body = fdef.body.as_ref().unwrap();
        if self.inlining.iter().any(|n| n == name) {
            self.errs
                .push(format!("higher-order: recursive inlining of `{name}` not supported (a lambda is not a storable function pointer)"));
            return (Operand::ConstI64(0), Ty::I64);
        }
        if body_has_return(body) {
            self.errs.push(format!(
                "higher-order: `{name}` uses `return` — inline expansion needs an expression-shaped body (tail value instead of `return`)"
            ));
            return (Operand::ConstI64(0), Ty::I64);
        }
        self.inlining.push(name.to_string());
        self.scopes.push(HashMap::new());
        for (p, arg) in fdef.sig.params.iter().zip(args) {
            match arg {
                Expr::Lambda { params, body, .. } => {
                    let l = self.new_local(Ty::Ref);
                    self.local_lambda.insert(l.0, (params.clone(), (**body).clone()));
                    self.bind(&p.name, l, Ty::Ref);
                }
                _ => {
                    let (op, ty) = self.lower_expr(arg);
                    // E421: inlined calls check `Ptr` parameters like direct calls.
                    if p.ty.as_ref().is_some_and(|t| t.name == "Ptr") && !self.ptr_of_operand(&op) {
                        self.errs.push(format!("error[E421]: argument of `{name}` is `Ptr` but the value is a non-pointer integer — a number is never an address (AGENTS.md §3)"));
                    }
                    let d = self.new_local(ty);
                    if let Some(c) = self.class_of_operand(&op) {
                        self.local_class.insert(d.0, c);
                    }
                    if let Some(k) = self.arr_of_operand(&op) {
                        self.local_arr.insert(d.0, k);
                    }
                    self.track_ptr(d.0, &op);
                    self.emit(Statement::Assign(d, Rvalue::Use(op)));
                    self.bind(&p.name, d, ty);
                }
            }
        }
        let r = self.lower_block_val(body);
        self.scopes.pop();
        self.inlining.pop();
        r
    }

    /// List comprehension `[elem for var in src (if cond)]` → two-pass:
    /// count (with filter) → allocate result array → fill.
    fn lower_comprehension(&mut self, elem: &Expr, var: &str, iter: &Expr, cond: Option<&Expr>) -> (Operand, Ty) {
        let (src, _) = self.lower_expr(iter);
        let src_kind = match self.arr_of_operand(&src) {
            Some(k) => k,
            None => {
                self.errs.push("comprehension: source is not a list".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        let src_vty = src_kind.value_ty();
        // Element kind of the result: probe elem in a dead block.
        let elem_kind = {
            let saved = self.cur;
            let dead = self.new_block();
            self.cur = dead.0 as usize;
            self.scopes.push(HashMap::new());
            let pv = self.new_local(src_vty);
            self.bind(var, pv, src_vty);
            let (_, ety) = self.lower_expr(elem);
            self.scopes.pop();
            self.cur = saved;
            arrkind_of(ety)
        };
        // Pass 1: count.
        let count = self.new_local(Ty::I64);
        self.emit(Statement::Assign(count, Rvalue::Use(Operand::ConstI64(0))));
        self.comp_loop(src.clone(), src_kind, var, src_vty, cond, &mut |s, _elem_local| {
            s.emit(Statement::Assign(count, Rvalue::Binary(IB::Add, Operand::Copy(count), Operand::ConstI64(1))));
        });
        // Allocate the result array.
        let count32 = self.to_i32(Operand::Copy(count));
        let res = self.new_local(Ty::Ref);
        self.local_arr.insert(res.0, elem_kind);
        self.emit(Statement::NewArray { dest: res, kind: elem_kind, len: count32 });
        // Pass 2: fill (evaluate elem, write at position j).
        let j = self.new_local(Ty::I64);
        self.emit(Statement::Assign(j, Rvalue::Use(Operand::ConstI64(0))));
        let elem_c = elem.clone();
        self.comp_loop(src, src_kind, var, src_vty, cond, &mut |s, _elem_local| {
            let (mut v, _) = s.lower_expr(&elem_c);
            if elem_kind == ArrKind::Long {
                v = to_i64(v);
            }
            let j32 = s.to_i32(Operand::Copy(j));
            s.emit(Statement::ArrayStore { arr: Operand::Copy(res), index: j32, value: v, kind: elem_kind, checked: true });
            s.emit(Statement::Assign(j, Rvalue::Binary(IB::Add, Operand::Copy(j), Operand::ConstI64(1))));
        });
        (Operand::Copy(res), Ty::Ref)
    }

    /// Emits `for var in src { if cond { body } }` — loop scaffold for
    /// comprehensions. `body` is called in the body (after an optional cond filter).
    fn comp_loop(&mut self, src: Operand, kind: ArrKind, var: &str, vty: Ty, cond: Option<&Expr>, body: &mut dyn FnMut(&mut Self, Local)) {
        let len = self.array_len_i64(src.clone());
        let ivar = self.new_local(Ty::I64);
        self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
        let header = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Goto(header));
        self.cur = header.0 as usize;
        let c = self.new_local(Ty::I32);
        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), len)));
        let bodyb = self.new_block();
        let latch = self.new_block();
        let exit = self.new_block();
        self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(c), then_blk: bodyb, else_blk: exit });
        self.cur = bodyb.0 as usize;
        let elem = self.new_local(vty);
        let idx32 = self.to_i32(Operand::Copy(ivar));
        self.emit(Statement::ArrayLoad { dest: elem, arr: src, index: idx32, kind, checked: true });
        self.scopes.push(HashMap::new());
        self.bind(var, elem, vty);
        // optional filter
        if let Some(cnd) = cond {
            let (cv, _) = self.lower_expr(cnd);
            let keep = self.new_block();
            self.term(self.cur, Terminator::Branch { cond: cv, then_blk: keep, else_blk: latch });
            self.cur = keep.0 as usize;
        }
        body(self, elem);
        self.scopes.pop();
        self.term(self.cur, Terminator::Goto(latch));
        self.cur = latch.0 as usize;
        self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
        self.term(latch.0 as usize, Terminator::Goto(header));
        self.cur = exit.0 as usize;
    }

    /// Build a tagged variant: `New Sum`, `__tag = t`, fields from the arguments.
    fn build_variant(&mut self, vname: &str, args: &[Expr]) -> (Operand, Ty) {
        let (sum, tag, vfields) = self.variants.get(vname).cloned().unwrap();
        let obj = self.new_local(Ty::Ref);
        self.local_class.insert(obj.0, sum.clone());
        self.emit(Statement::New { dest: obj, class: sum.clone() });
        self.emit(Statement::PutField { obj: Operand::Copy(obj), class: sum.clone(), field: "__tag".into(), value: Operand::ConstI64(tag) });
        if args.len() != vfields.len() {
            self.errs.push(format!("variant `{vname}`: expected {} fields, {} given", vfields.len(), args.len()));
        }
        for ((fname, fty, _), arg) in vfields.iter().zip(args) {
            let (mut v, _) = self.lower_expr(arg);
            if *fty == Ty::I64 {
                v = to_i64(v);
            }
            self.emit(Statement::PutField { obj: Operand::Copy(obj), class: sum.clone(), field: fname.clone(), value: v });
        }
        (Operand::Copy(obj), Ty::Ref)
    }

    /// Build a data-carrying variant of a generic sum type (`Some(3.5)`) type-correctly:
    /// infer the type arguments from the payload types, instantiate `Option$Float`
    /// (F64 payload), tagged instance with the instance class.
    /// Split a monomorphized `Result$T$E` class into `(T, E)`; `("", "")` when
    /// the class is not a typed Result (erased leftovers, user types, garbage).
    fn parse_result_class(class: &str) -> (String, String) {
        match class.strip_prefix("Result$") {
            Some(rest) => {
                let mut it = rest.split('$');
                match (it.next(), it.next(), it.next()) {
                    (Some(t), Some(e), None) => (t.into(), e.into()),
                    _ => (String::new(), String::new()),
                }
            }
            None => (String::new(), String::new()),
        }
    }
    /// Ensure the mono `Result$T$E` layout exists (module-wide seeding covers
    /// signatures; this covers reconstructed classes like `?` targets);
    /// returns the class name.
    fn mono_result_class(&mut self, t: &str, e: &str) -> String {
        let (tparams, variants) = self.generic_stypes["Result"].clone();
        inst_stype("Result", &tparams, &variants, &[t.into(), e.into()], &mut self.local_inst, &mut self.local_svars)
    }
    /// `(payload Ty, payload object class)` of variant `Ok`/`Err` in a mono
    /// `Result$T$E` class (falls back to erased i64 shape on unknown classes —
    /// callers E-error first, so this is unreachable in valid programs).
    fn result_payload_ty(&self, class: &str, vname: &str) -> (Ty, Option<String>) {
        if let Some((_, _, vf)) = self.variant_in(class, vname) {
            if let Some((_, ty, cls)) = vf.first() {
                return (*ty, cls.clone());
            }
        }
        (Ty::I64, None)
    }
    fn build_generic_variant(&mut self, vname: &str, args: &[Expr]) -> (Operand, Ty) {
        let sum = self.variant_owner_g[vname].clone();
        let (tparams, variants) = self.generic_stypes[&sum].clone();
        // `Ok`/`Err` carry exactly one payload; anything else is E-error (a
        // dataless `Ok` has no erased fallback anymore — there is no layout
        // without a payload type).
        if (vname == "Ok" || vname == "Err") && args.len() != 1 {
            self.errs.push(format!("error[E100]: `{vname}` takes exactly one payload argument, got {}", args.len()));
            return (Operand::ConstI64(0), Ty::I64);
        }
        let vdef = variants.iter().find(|(n, _)| n == vname).cloned().unwrap_or((vname.into(), vec![]));
        let lowered: Vec<(Operand, Ty)> = args.iter().map(|a| self.lower_expr(a)).collect();
        // Derive the type parameters from the payload fields that are exactly `T`.
        let mut tmap: HashMap<String, String> = HashMap::new();
        for (i, (_, tyname)) in vdef.1.iter().enumerate() {
            if tparams.iter().any(|tp| tp == tyname) {
                if let Some((op, ty)) = lowered.get(i) {
                    tmap.entry(tyname.clone()).or_insert_with(|| self.ty_name(op, *ty));
                }
            }
        }
        // Anything the payload leaves free comes from the nearest annotation
        // (one-shot let-hint, else the enclosing `-> Result[T, E]`); what is
        // still free is E-error — never a silent `Int` (AGENTS.md §4). A mono
        // `Result$T$E` with a wrongly defaulted parameter would read a Ref
        // field as i64 (or vice versa) — memory-unsafe under RC.
        let mut targs: Vec<String> = Vec::new();
        for tp in &tparams {
            if let Some(got) = tmap.get(tp) {
                targs.push(got.clone());
            } else if sum == "Result" {
                let hinted = self
                    .sum_hint
                    .clone()
                    .and_then(|(t, e)| (tp == "T").then(|| t.clone()).or_else(|| (tp == "E").then(|| e.clone())))
                    .or_else(|| {
                        self.expected_result.clone().and_then(|(t, e)| {
                            (tp == "T").then(|| t.clone()).or_else(|| (tp == "E").then(|| e.clone()))
                        })
                    });
                match hinted {
                    Some(h) => targs.push(h),
                    None => {
                        self.errs.push(format!(
                            "error[E100]: cannot infer `{tp}` of `{vname}` — annotate it (e.g. `mut r: Result[Int, Str] = {vname}(…)`) or return it from a `-> Result[T, E]` function"
                        ));
                        targs.push("Int".into());
                    }
                }
            } else {
                targs.push(tmap.get(tp).cloned().unwrap_or_else(|| "Int".into()));
            }
        }
        let mangled = inst_stype(&sum, &tparams, &variants, &targs, &mut self.local_inst, &mut self.local_svars);
        let (_, tag, vfields) = self.variant_in(&mangled, vname).unwrap();
        let obj = self.new_local(Ty::Ref);
        self.local_class.insert(obj.0, mangled.clone());
        self.emit(Statement::New { dest: obj, class: mangled.clone() });
        self.emit(Statement::PutField { obj: Operand::Copy(obj), class: mangled.clone(), field: "__tag".into(), value: Operand::ConstI64(tag) });
        for ((fname, fty, _), (mut v, _)) in vfields.iter().zip(lowered) {
            if *fty == Ty::I64 {
                v = to_i64(v);
            }
            self.emit(Statement::PutField { obj: Operand::Copy(obj), class: mangled.clone(), field: fname.clone(), value: v });
        }
        (Operand::Copy(obj), Ty::Ref)
    }

    /// `match s { Variant(binds) -> body … _ -> body }` → dispatch via `__tag`,
    /// field extraction per arm, phi stand-in via a result local (like lower_if).
    fn lower_match(&mut self, scrut: &Expr, arms: &[(Pattern, Option<Expr>, Expr)]) -> (Operand, Ty) {
        let (obj, oty) = self.lower_expr(scrut);
        let class = self.class_of_operand(&obj);
        // Unfold or-patterns at arm level: `A | B -> body` → two arms.
        let mut flat: Vec<(Pattern, Option<Expr>, Expr)> = Vec::new();
        for (pat, guard, body) in arms {
            match pat {
                Pattern::Or(ps, _) => {
                    for p in ps {
                        flat.push((p.clone(), guard.clone(), body.clone()));
                    }
                }
                _ => flat.push((pat.clone(), guard.clone(), body.clone())),
            }
        }
        // Exhaustiveness check (compile time): non-exhaustive = HARD ERROR.
        self.check_exhaustive(&class, &flat);
        let merge = self.new_block();
        let mut ends: Vec<(usize, Operand, Ty)> = Vec::new();
        for (pat, guard, body) in &flat {
            let fail = self.new_block();
            self.scopes.push(HashMap::new());
            self.emit_pattern_test(obj.clone(), oty, class.clone(), pat, fail);
            // Guard after a successful pattern.
            if let Some(g) = guard {
                let (gc, _) = self.lower_expr(g);
                let cont = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Branch { cond: gc, then_blk: cont, else_blk: fail });
                self.cur = cont.0 as usize;
            }
            let (v, t) = self.lower_expr(body);
            self.scopes.pop();
            ends.push((self.cur, v, t));
            self.cur = fail.0 as usize; // the next arm begins in the fail block
        }
        let rty = ends.iter().map(|(_, _, t)| *t).find(|t| *t != Ty::Void).unwrap_or(Ty::Void);
        let res = if rty != Ty::Void { Some(self.new_local(rty)) } else { None };
        // SOUND class/array propagation (same rule as `lower_if`): the result inherits
        // a class ONLY if EVERY value-producing arm agrees. A single disagreeing or
        // unknown arm → leave it unknown, so `n.field` on a heterogeneous match errors
        // loudly instead of loading against one arm's layout. `match` over homogeneous
        // object arms (`0 => Node(..)  _ => Node(..)`) then resolves `.field`.
        if let Some(r) = res {
            let cls: Vec<Option<String>> = ends.iter().map(|(_, v, _)| self.class_of_operand(v)).collect();
            if let Some(Some(first)) = cls.first() {
                if cls.iter().all(|c| c.as_deref() == Some(first.as_str())) {
                    self.local_class.insert(r.0, first.clone());
                }
            }
            let ark: Vec<Option<ArrKind>> = ends.iter().map(|(_, v, _)| self.arr_of_operand(v)).collect();
            if let Some(Some(first)) = ark.first() {
                if ark.iter().all(|k| *k == Some(*first)) {
                    self.local_arr.insert(r.0, *first);
                }
            }
            // `Ptr` provenance: every value-producing arm must be proven.
            if ends.iter().all(|(_, v, _)| self.ptr_of_operand(v)) {
                self.ptr_locals.insert(r.0);
            }
        }
        for (end, v, _) in &ends {
            if let Some(r) = res {
                self.blocks[*end].statements.push(Statement::Assign(r, Rvalue::Use(v.clone())));
            }
            self.blocks[*end].terminator = Terminator::Goto(merge);
        }
        // Close the fallthrough (unreachable because exhaustively checked) type-correctly.
        let cur = self.cur;
        if let Some(r) = res {
            self.blocks[cur].statements.push(Statement::Assign(r, Rvalue::Use(zero_of(rty))));
        }
        self.term(cur, Terminator::Goto(merge));
        self.cur = merge.0 as usize;
        match res {
            Some(r) => (Operand::Copy(r), rty),
            None => (Operand::ConstI64(0), Ty::Void),
        }
    }

    /// Emits the tests for ONE pattern against `obj`. On non-match
    /// → `fail`; on a match `self.cur` continues (with bindings in
    /// scope). Recursive for nested patterns.
    fn emit_pattern_test(&mut self, obj: Operand, ty: Ty, class: Option<String>, pat: &Pattern, fail: Block) {
        match pat {
            Pattern::Wildcard(_) => {}
            Pattern::Bind(name, _) => {
                let l = match &obj {
                    Operand::Copy(l) => *l,
                    _ => {
                        let d = self.new_local(ty);
                        self.emit(Statement::Assign(d, Rvalue::Use(obj.clone())));
                        d
                    }
                };
                if let Some(c) = &class {
                    self.local_class.insert(l.0, c.clone());
                }
                self.bind(name, l, ty);
            }
            Pattern::Int(v, _) => self.emit_eq_test(obj, to_i64(Operand::ConstI64(*v as i64)), fail),
            Pattern::Bool(b, _) => self.emit_eq_test(obj, Operand::ConstI32(if *b { 1 } else { 0 }), fail),
            Pattern::Ctor { name, args, .. } => {
                // For a generic instance class (`Option$Float`): use the instance layout
                // (type-correct field extraction), otherwise the erased variant.
                let inst = class.as_deref().and_then(|c| self.variant_in(c, name));
                let (sum, vtag, vfields) = match inst.or_else(|| self.variants.get(name).cloned()) {
                    Some(v) => v,
                    None => {
                        self.errs.push(format!("unknown variant `{name}` in match"));
                        return;
                    }
                };
                let tag = self.new_local(Ty::I64);
                self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: sum.clone(), field: "__tag".into() });
                self.emit_eq_test(Operand::Copy(tag), Operand::ConstI64(vtag), fail);
                for (j, argpat) in args.iter().enumerate() {
                    if let Some((fname, fty, rt)) = vfields.get(j).cloned() {
                        let d = self.new_local(fty);
                        if let Some(rc) = &rt {
                            self.local_class.insert(d.0, rc.clone());
                        }
                        self.emit(Statement::GetField { dest: d, obj: obj.clone(), class: sum.clone(), field: fname });
                        self.emit_pattern_test(Operand::Copy(d), fty, rt, argpat, fail);
                    }
                }
            }
            Pattern::Or(ps, _) => {
                // Nested or: try in order; if one matches → continue.
                let matched = self.new_block();
                for p in ps {
                    let next = self.new_block();
                    self.emit_pattern_test(obj.clone(), ty, class.clone(), p, next);
                    let cur = self.cur;
                    self.term(cur, Terminator::Goto(matched));
                    self.cur = next.0 as usize;
                }
                let cur = self.cur;
                self.term(cur, Terminator::Goto(fail));
                self.cur = matched.0 as usize;
            }
            _ => self.errs.push("match pattern: tuple/string patterns not yet lowered".into()),
        }
    }

    /// `obj == val ? continue : fail`.
    fn emit_eq_test(&mut self, obj: Operand, val: Operand, fail: Block) {
        let c = self.new_local(Ty::I32);
        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpEq, obj, val)));
        let cont = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Branch { cond: Operand::Copy(c), then_blk: cont, else_blk: fail });
        self.cur = cont.0 as usize;
    }

    /// Compile-time exhaustiveness check. Sum type: all variants or `_`/Bind.
    /// Scalar/literal: a `_`/Bind branch required. Otherwise a hard error.
    fn check_exhaustive(&mut self, class: &Option<String>, arms: &[(Pattern, Option<Expr>, Expr)]) {
        let has_catchall = arms.iter().any(|(p, g, _)| g.is_none() && matches!(p, Pattern::Wildcard(_) | Pattern::Bind(..)));
        if has_catchall {
            return;
        }
        if let Some(sum) = class {
            // Generic instance class (`Option$Float`) → variants from the
            // instance registry; otherwise the erased variants of the sum type.
            let inst_vars = self.local_svars.get(sum).or_else(|| self.shared_svars.get(sum));
            let all: Vec<(String, i64)> = if let Some(m) = inst_vars {
                m.iter().map(|(n, (t, _))| (n.clone(), *t)).collect()
            } else {
                self.variants.iter().filter(|(_, (s, _, _))| s == sum).map(|(n, (_, t, _))| (n.clone(), *t)).collect()
            };
            if all.is_empty() {
                return; // not a sum type (e.g. product type) → no check
            }
            let tag_of = |name: &str| -> Option<i64> {
                if let Some(m) = inst_vars {
                    m.get(name).map(|(t, _)| *t)
                } else {
                    self.variants.get(name).map(|(_, t, _)| *t)
                }
            };
            let mut covered = std::collections::HashSet::new();
            for (p, g, _) in arms {
                if g.is_some() {
                    continue; // guard can fail → does not reliably cover
                }
                if let Pattern::Ctor { name, args, .. } = p {
                    // covers the tag only if all arguments are irrefutable
                    if args.iter().all(is_irrefutable) {
                        if let Some(t) = tag_of(name) {
                            covered.insert(t);
                        }
                    }
                }
            }
            let missing: Vec<&str> = all.iter().filter(|(_, t)| !covered.contains(t)).map(|(n, _)| n.as_str()).collect();
            if !missing.is_empty() {
                self.errs.push(format!("non-exhaustive `match`: missing {} (or `_` arm)", missing.join(", ")));
            }
        } else {
            self.errs.push("`match` over scalar/literal needs a `_` arm (non-exhaustive)".into());
        }
    }

    fn lower_if(&mut self, cond: &Expr, then: &Block2, elifs: &[(Expr, Block2)], els: &Option<Block2>) -> (Operand, Ty) {
        let (c, _) = self.lower_expr(cond);
        let thenb = self.new_block();
        let elseb = self.new_block();
        let merge = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Branch { cond: c, then_blk: thenb, else_blk: elseb });
        // then branch → value + end block (not yet terminated).
        self.cur = thenb.0 as usize;
        let (tv, tty) = self.lower_block_val(then);
        let te = self.cur;
        // else branch: further `elif`s recursively, else the `else` block, else no value.
        self.cur = elseb.0 as usize;
        let (ev, ety) = if !elifs.is_empty() {
            let (ec, eb) = &elifs[0];
            let rest: Vec<(Expr, Block2)> = elifs[1..].to_vec();
            self.lower_if(ec, eb, &rest, els)
        } else if let Some(e) = els {
            self.lower_block_val(e)
        } else {
            (Operand::ConstI64(0), Ty::Void)
        };
        let ee = self.cur;
        // Result type: the non-Void branch wins (both equal for a value-if).
        let rty = if tty != Ty::Void { tty } else { ety };
        if rty != Ty::Void {
            // Phi replacement: shared result local, assigned in both end blocks.
            let res = self.new_local(rty);
            // The result inherits the object class / array element kind of its
            // branches (both agree for a well-formed value-if). Without this,
            // `mut n = if c { a } else { b }` forgets that `n` is an object, so a
            // later `n.field` fails "type of the object unknown" — the same gap
            // that made a self-recursive object builder (after recursion-inlining
            // rewrites the tail self-call into an if/else) unusable.
            // SOUND propagation only: the result inherits a class/array-kind ONLY when
            // BOTH branches agree on it. If they disagree (`if c { Node(..) } else {
            // Leaf(..) }`), the runtime value is heterogeneous, so we must NOT pin a
            // single layout — leaving it unknown restores the loud, conservative
            // "type of the object unknown" error instead of a wrong-offset load. A
            // branch with no class (e.g. a bare `null`) is also treated as disagreement.
            match (self.class_of_operand(&tv), self.class_of_operand(&ev)) {
                (Some(a), Some(b)) if a == b => { self.local_class.insert(res.0, a); }
                _ => {}
            }
            match (self.arr_of_operand(&tv), self.arr_of_operand(&ev)) {
                (Some(a), Some(b)) if a == b => { self.local_arr.insert(res.0, a); }
                _ => {}
            }
            // `Ptr` provenance merges only when BOTH branches are proven (a null
            // literal counts as proven, so `if c { p } else { 0 }` stays a pointer;
            // a forged integer on either side clears it — doubt, not trust).
            if self.ptr_of_operand(&tv) && self.ptr_of_operand(&ev) {
                self.ptr_locals.insert(res.0);
            }
            self.blocks[te].statements.push(Statement::Assign(res, Rvalue::Use(tv)));
            self.blocks[ee].statements.push(Statement::Assign(res, Rvalue::Use(ev)));
            self.term(te, Terminator::Goto(merge));
            self.term(ee, Terminator::Goto(merge));
            self.cur = merge.0 as usize;
            (Operand::Copy(res), rty)
        } else {
            self.term(te, Terminator::Goto(merge));
            self.term(ee, Terminator::Goto(merge));
            self.cur = merge.0 as usize;
            (Operand::ConstI64(0), Ty::Void)
        }
    }
}

// The AST calls it Block; here an alias to avoid a name collision with ir::Block.
use crate::ast::Block as Block2;

#[allow(clippy::too_many_arguments)]
fn lower_fn(
    f: &FnDef,
    sigs: &HashMap<String, Sig>,
    syscalls: &HashMap<String, SyscallInfo>,
    hw_regions: &HashMap<String, (u64, u64)>,
    types: &HashMap<String, Layout>,
    field_arr: &HashMap<(String, String), ArrKind>,
    variants: &HashMap<String, VariantInfo>,
    generics: &HashMap<String, GInfo>,
    trait_methods: &HashMap<String, Vec<(String, String, Vec<Ty>, Ty)>>,
    fn_defs: &HashMap<String, FnDef>,
    generic_ptypes: &HashMap<String, (Vec<String>, Vec<Field>)>,
    generic_stypes: &HashMap<String, (Vec<String>, Vec<(String, Vec<(String, String)>)>)>,
    variant_owner_g: &HashMap<String, String>,
    shared_inst: &HashMap<String, Layout>,
    shared_svars: &HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>>,
    strings: &mut Vec<String>,
    str_idx: &mut HashMap<String, u32>,
    recv_class: Option<&str>,
    sym: Option<&str>,
    line: u32,
    line_starts: &[usize],
) -> Result<(Function, Vec<(String, Vec<String>)>, HashMap<String, Layout>, Vec<Option<String>>), Vec<String>> {
    let ret = guess_ret_ty(f);
    let name = match sym {
        Some(s) => s.to_string(),
        None if f.sig.name == "main" => "vire_main".to_string(),
        None => f.sig.name.clone(),
    };
    let mut fl = FnLower {
        locals: Vec::new(),
        local_names: Vec::new(),
        blocks: Vec::new(),
        cur: 0,
        scopes: vec![HashMap::new()],
        sigs,
        syscalls,
        field_arr,
        types,
        variants,
        generics,
        trait_methods,
        fn_defs,
        inlining: Vec::new(),
        generic_ptypes,
        generic_stypes,
        variant_owner_g,
        shared_inst,
        shared_svars,
        local_inst: HashMap::new(),
        local_svars: HashMap::new(),
        mono: Vec::new(),
        local_class: HashMap::new(),
        local_arr: HashMap::new(),
        local_arr_class: HashMap::new(),
        arr_hint: None,
        sum_hint: None,
        expected_result: None,
        mmio: HashMap::new(),
        ptr_locals: std::collections::HashSet::new(),
        ret_is_ptr: false,
        in_region: 0,
        hw_regions,
        local_lambda: HashMap::new(),
        strings,
        str_idx,
        errs: Vec::new(),
        loops: Vec::new(),
        line_starts,
        last_dbg_line: 0,
        fn_name: name.clone(),
        log_span: Vec::new(),
    };
    // Block 0
    fl.new_block();
    // Expected `(T, E)` for `Ok`/`Err` under construction, from the enclosing
    // return type (AGENTS.md §4): `-> Result[T, E]` fixes what payloads alone
    // leave free. A bare `-> Result` (no arguments) is E-error here, not a
    // dangling class.
    if let Some(t) = f.sig.ret.as_ref().filter(|t| t.name == "Result") {
        if t.args.len() == 2 {
            fl.expected_result = Some((t.args[0].name.clone(), t.args[1].name.clone()));
        } else {
            fl.errs.push("error[E100]: `-> Result` needs both type arguments (`-> Result[T, E]`)".into());
        }
    }
    // Parameter → Locals 0..n
    let mut param_tys = Vec::new();
    for p in &f.sig.params {
        // An array parameter (`a: Array[Int]` / `Array[Float]`): a `Ref` with a known
        // element kind, recorded in `local_arr` below so `a[i]`, `a[i] = v` and `a.len()`
        // in the body lower to real bounds-checked array accesses (previously a ref param
        // carried no ArrKind → "unknown array"). Lets array-taking helpers (e.g. a
        // recursive `qsort(a, lo, hi)`) be written directly instead of an explicit stack.
        let mut arr_kind: Option<ArrKind> = None;
        let mut arr_cls: Option<String> = None;
        // `self` receiver: Ref to the method class.
        let (t, cls) = if p.name == "self" {
            (Ty::Ref, recv_class.map(|c| c.to_string()))
        } else if let Some(pt) = p.ty.as_ref().filter(|pt| pt.name == "Array" || pt.name == "array") {
            // `a: Array[Node]` — an element type that is a user object class is a REF
            // array (arrkind_of_name defaults unknown names to Long, which would misread
            // the ref slots as i64). Record the element class so `a[i].field` resolves
            // and `a[i] = e` is class-checked in the body, exactly like a local ref array.
            // `class_of` only filters the INFERENCE type names, so it still answers "class"
            // for the element-kind spellings `Byte`/`U8`/`I8`/`Long`/`Double` — taking
            // those down the ref path made `a[i]` in an `Array[Byte]` callee load a Ref
            // where an integer was expected. Ask `arrkind_of_name` first: anything it
            // knows is a scalar element, never a class.
            match pt.args.first().filter(|a| !is_scalar_elem_name(&a.name)).and_then(|a| class_of(Some(a))) {
                Some(ec) => {
                    arr_kind = Some(ArrKind::Ref);
                    arr_cls = Some(ec);
                }
                None => arr_kind = Some(pt.args.first().map(|a| arrkind_of_name(&a.name)).unwrap_or(ArrKind::Long)),
            }
            (Ty::Ref, None)
        } else if p.ty.as_ref().is_some_and(|pt| pt.name == "farray") {
            // `a: farray` — the builtin float(f64) array (element = Double), so
            // `a[i]`/`a[i] = v`/`a.len()` lower to real float array accesses.
            arr_kind = Some(ArrKind::Double);
            (Ty::Ref, None)
        } else if let Some(pt) = p.ty.as_ref().filter(|pt| !pt.args.is_empty() && fl.generic_ptypes.contains_key(&pt.name)) {
            // Annotated generic type `b: Box[Int]` → instance `Box$Int`, so that
            // field accesses in the body find the concrete layout.
            let (tparams, fields) = fl.generic_ptypes[&pt.name].clone();
            let targs: Vec<String> = pt.args.iter().map(|a| a.name.clone()).collect();
            let mangled = fl.instantiate_ptype(&pt.name, &tparams, &fields, &targs);
            (Ty::Ref, Some(mangled))
        } else {
            (ty_of(p.ty.as_ref()), class_of(p.ty.as_ref()))
        };
        param_tys.push(t);
        let l = fl.new_local(t);
        if let Some(c) = cls {
            fl.local_class.insert(l.0, c);
        }
        if let Some(k) = arr_kind {
            fl.local_arr.insert(l.0, k);
        }
        if let Some(c) = arr_cls {
            fl.local_arr_class.insert(l.0, c);
        }
        // A `Ptr`-annotated parameter starts with provenance (the caller was
        // checked at its call site); any other parameter never does.
        if p.ty.as_ref().is_some_and(|pt| pt.name == "Ptr") {
            fl.ptr_locals.insert(l.0);
        }
        fl.bind(&p.name, l, t);
    }
    // A `-> Ptr` function must return proven pointers (checked at each return).
    fl.ret_is_ptr = f.sig.ret.as_ref().is_some_and(|t| t.name == "Ptr");
    if let Some(body) = &f.body {
        // Statements + tail (tail = return value, if ret != Void)
        fl.scopes.push(HashMap::new());
        for s in &body.stmts {
            fl.lower_stmt(s);
        }
        let term = if let Some(t) = &body.tail {
            fl.mark_line(expr_span(t));
            let (op, _) = fl.lower_expr(t);
            // E421: a `-> Ptr` tail value must be a proven pointer too.
            if fl.ret_is_ptr && ret != Ty::Void && !fl.ptr_of_operand(&op) {
                fl.errs.push("error[E421]: `-> Ptr` function with a non-pointer tail value — a number is never an address (AGENTS.md §3)".into());
            }
            if ret == Ty::Void { Terminator::Return(None) } else { Terminator::Return(Some(op)) }
        } else if ret == Ty::Void {
            Terminator::Return(None)
        } else {
            // No tail, but a typed return value: the value comes from a
            // `return` statement; this fallthrough block is unreachable. But it
            // must terminate type-correctly (otherwise `ret void` in an i64 function).
            Terminator::Return(Some(zero_of(ret)))
        };
        fl.scopes.pop();
        let cur = fl.cur;
        fl.term(cur, term);
    } else {
        let cur = fl.cur;
        fl.term(cur, Terminator::Return(None));
    }
    if !fl.errs.is_empty() {
        return Err(fl.errs);
    }
    let mono = fl.mono;
    let local_inst = fl.local_inst;
    let local_names = fl.local_names;
    Ok((
        Function {
            name,
            params: param_tys,
            ret,
            locals: fl.locals,
            blocks: fl.blocks,
            receiver_nonnull: false,
            line,
        },
        mono,
        local_inst,
        local_names,
    ))
}

/// Higher-order template? True if a parameter is used as a call target in the
/// body (`f(x)` with `f` = parameter). Such functions are ONLY
/// inline-expanded (defunctionalization) — never lowered standalone, because the
/// function parameter has no runtime value.
fn is_higher_order(f: &FnDef) -> bool {
    let params: std::collections::HashSet<&str> = f.sig.params.iter().map(|p| p.name.as_str()).collect();
    match &f.body {
        Some(b) => block_calls_param(b, &params),
        None => false,
    }
}
fn block_calls_param(b: &Block2, params: &std::collections::HashSet<&str>) -> bool {
    b.stmts.iter().any(|s| stmt_calls_param(s, params)) || b.tail.as_deref().map(|e| expr_calls_param(e, params)).unwrap_or(false)
}
fn stmt_calls_param(s: &Stmt, params: &std::collections::HashSet<&str>) -> bool {
    match s {
        Stmt::Let { value, .. } => value.as_ref().map(|e| expr_calls_param(e, params)).unwrap_or(false),
        Stmt::Assign { target, value, .. } => expr_calls_param(target, params) || expr_calls_param(value, params),
        Stmt::Expr(e) => expr_calls_param(e, params),
        Stmt::Return(v, _) => v.as_ref().map(|e| expr_calls_param(e, params)).unwrap_or(false),
        Stmt::While { cond, body, .. } => expr_calls_param(cond, params) || block_calls_param(body, params),
        Stmt::For { iter, body, .. } => expr_calls_param(iter, params) || block_calls_param(body, params),
        Stmt::Break(_) | Stmt::Continue(_) => false,
    }
}
fn expr_calls_param(e: &Expr, params: &std::collections::HashSet<&str>) -> bool {
    let sub = |e: &Expr| expr_calls_param(e, params);
    let blk = |b: &Block2| block_calls_param(b, params);
    match e {
        Expr::Call { callee, args, .. } => {
            (matches!(callee.as_ref(), Expr::Ident(n, _) if params.contains(n.as_str()))) || sub(callee) || args.iter().any(sub)
        }
        Expr::Unary { rhs, .. } => sub(rhs),
        Expr::Binary { lhs, rhs, .. } => sub(lhs) || sub(rhs),
        Expr::Field { base, .. } => sub(base),
        Expr::Index { base, index, .. } => sub(base) || sub(index),
        Expr::If { cond, then, elifs, els, .. } => {
            sub(cond) || blk(then) || elifs.iter().any(|(c, b)| sub(c) || blk(b)) || els.as_ref().map(blk).unwrap_or(false)
        }
        Expr::Match { scrutinee, arms, .. } => sub(scrutinee) || arms.iter().any(|(_, g, b)| g.as_ref().map(sub).unwrap_or(false) || sub(b)),
        Expr::Block(b) => blk(b),
        Expr::Lambda { body, .. } => sub(body),
        Expr::List(xs, _) => xs.iter().any(sub),
        Expr::Comprehension { elem, iter, cond, .. } => sub(elem) || sub(iter) || cond.as_deref().map(sub).unwrap_or(false),
        Expr::MapLit(kvs, _) => kvs.iter().any(|(k, v)| sub(k) || sub(v)),
        Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => sub(inner),
        Expr::Range { start, end, .. } => sub(start) || sub(end),
        Expr::Capsule { body, .. } => blk(body),
        _ => false,
    }
}

/// Does a block contain (recursively over nested blocks/loops/if) a
/// `return` statement? For the capsule check (return would skip arena_pop).
fn body_has_return(b: &Block2) -> bool {
    b.stmts.iter().any(stmt_has_return)
}
fn stmt_has_return(s: &Stmt) -> bool {
    match s {
        Stmt::Return(..) => true,
        Stmt::While { body, .. } | Stmt::For { body, .. } => body_has_return(body),
        _ => false,
    }
}

/// Compile-time constant value (for `comptime`).
enum CVal {
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Folds a constant expression at compile time. `None` = not constant.
fn const_eval(e: &Expr) -> Option<CVal> {
    Some(match e {
        Expr::Int(v, _) => CVal::Int(*v as i64),
        Expr::Float(v, _) => CVal::Float(*v),
        Expr::Bool(b, _) => CVal::Bool(*b),
        Expr::Comptime { inner, .. } => return const_eval(inner),
        Expr::Unary { op, rhs, .. } => match (op, const_eval(rhs)?) {
            (UnOp::Neg, CVal::Int(v)) => CVal::Int(-v),
            (UnOp::Neg, CVal::Float(v)) => CVal::Float(-v),
            (UnOp::Not, CVal::Bool(b)) => CVal::Bool(!b),
            _ => return None,
        },
        Expr::Binary { op, lhs, rhs, .. } => {
            let (l, r) = (const_eval(lhs)?, const_eval(rhs)?);
            match (l, r) {
                (CVal::Int(a), CVal::Int(b)) => match op {
                    BinOp::Add | BinOp::AddWrap => CVal::Int(a.wrapping_add(b)),
                    BinOp::Sub | BinOp::SubWrap => CVal::Int(a.wrapping_sub(b)),
                    BinOp::Mul | BinOp::MulWrap => CVal::Int(a.wrapping_mul(b)),
                    BinOp::Div if b != 0 => CVal::Int(a / b),
                    BinOp::Rem if b != 0 => CVal::Int(a % b),
                    BinOp::Eq => CVal::Bool(a == b),
                    BinOp::Ne => CVal::Bool(a != b),
                    BinOp::Lt => CVal::Bool(a < b),
                    BinOp::Le => CVal::Bool(a <= b),
                    BinOp::Gt => CVal::Bool(a > b),
                    BinOp::Ge => CVal::Bool(a >= b),
                    BinOp::BitAnd => CVal::Int(a & b),
                    BinOp::BitOr => CVal::Int(a | b),
                    BinOp::BitXor => CVal::Int(a ^ b),
                    BinOp::Shl => CVal::Int(a << b),
                    BinOp::Shr => CVal::Int(a >> b),
                    BinOp::UShr => CVal::Int(((a as u64) >> b) as i64),
                    _ => return None,
                },
                (CVal::Float(a), CVal::Float(b)) => match op {
                    BinOp::Add => CVal::Float(a + b),
                    BinOp::Sub => CVal::Float(a - b),
                    BinOp::Mul => CVal::Float(a * b),
                    BinOp::Div => CVal::Float(a / b),
                    BinOp::Lt => CVal::Bool(a < b),
                    BinOp::Le => CVal::Bool(a <= b),
                    BinOp::Gt => CVal::Bool(a > b),
                    BinOp::Ge => CVal::Bool(a >= b),
                    _ => return None,
                },
                (CVal::Bool(a), CVal::Bool(b)) => match op {
                    BinOp::And => CVal::Bool(a && b),
                    BinOp::Or => CVal::Bool(a || b),
                    BinOp::Eq => CVal::Bool(a == b),
                    BinOp::Ne => CVal::Bool(a != b),
                    _ => return None,
                },
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// Pattern that always matches (covers its slot completely).
fn is_irrefutable(p: &Pattern) -> bool {
    matches!(p, Pattern::Wildcard(_) | Pattern::Bind(..))
}

/// IR value type → array element kind.
fn arrkind_of(t: Ty) -> ArrKind {
    match t {
        Ty::F64 => ArrKind::Double,
        Ty::F32 => ArrKind::Float,
        Ty::I32 => ArrKind::Int,
        Ty::Ref => ArrKind::Ref,
        _ => ArrKind::Long,
    }
}

/// Build-time log threshold (`FASTLLVM_LOG_LEVEL`, set by `--log-level`): a level below
/// it lowers to nothing. debug=0 info=1 warn=2 error=3 off=4; default info.
fn log_threshold() -> i32 {
    match std::env::var("FASTLLVM_LOG_LEVEL").ok().as_deref() {
        Some("debug") => 0,
        Some("info") => 1,
        Some("warn") => 2,
        Some("error") => 3,
        Some("off") | Some("none") => 4,
        _ => 1,
    }
}

/// Console sink: colour the `[LEVEL] ` tag by severity when `--log-color`
/// (`FASTLLVM_LOG_COLOR`) is set — otherwise the tag is returned unchanged, so
/// the default output stays plain. ANSI: error=red, warn=yellow, info=green,
/// debug=dim. The trailing space stays outside the reset so alignment holds.
fn colorize_tag(level: &str, tag: &str) -> String {
    if std::env::var_os("FASTLLVM_LOG_COLOR").is_none() {
        return tag.to_string();
    }
    let code = match level {
        "error" => "31",
        "warn" => "33",
        "info" => "32",
        _ => "90",
    };
    format!("\x1b[{code}m{}\x1b[0m ", tag.trim_end())
}

/// Element-type name in an array parameter (`Array[Int]`) → array element kind.
/// `Int`/`Long` = i64 slots (like `array(n)`), `Float` = f64 (like `farray(n)`).
/// Element kind of an array-typed annotation (`array`/`Array[T]`/`farray`), else
/// `None`. Used to tag array struct fields so `x.field[i]` can index them.
fn field_arrkind(t: &Type) -> Option<ArrKind> {
    match t.name.as_str() {
        "array" | "Array" => Some(arrkind_of_name(t.args.first().map(|a| a.name.as_str()).unwrap_or("Int"))),
        "farray" => Some(ArrKind::Double),
        _ => None,
    }
}

/// Is `n` an element-KIND spelling (a scalar/ref array element), as opposed to a user
/// object class? Every name `arrkind_of_name` recognises explicitly, plus the `Int`/`I64`/
/// `Long` family that it folds into its default arm. Guards the param path in `lower_fn`:
/// `class_of` alone answers "class" for `Byte`/`U8`/`I8`/`Long`/`Double`, which would make
/// `fn f(a: Array[Byte])` a ref array and mistype every `a[i]` in the body.
fn is_scalar_elem_name(n: &str) -> bool {
    matches!(
        n,
        "Float" | "F64" | "Double" | "F32" | "I32" | "U32" | "Byte" | "U8" | "I8"
            | "Int" | "I64" | "U64" | "Long" | "Bool" | "Ref" | "Str"
    )
}

fn arrkind_of_name(n: &str) -> ArrKind {
    match n {
        "Float" | "F64" | "Double" => ArrKind::Double,
        "F32" => ArrKind::Float,
        "I32" | "U32" => ArrKind::Int,
        // 1-byte elements. `Byte`/`U8` are UNSIGNED (0..255, zero-extend on load) — the right
        // semantics for byte scanning (grep, binary I/O); `I8` is the signed variant.
        "Byte" | "U8" => ArrKind::U8,
        "I8" => ArrKind::Byte,
        "Ref" | "Str" => ArrKind::Ref,
        _ => ArrKind::Long, // Int / I64 / Long / default
    }
}

/// Type-correct null/default operand (for unreachable typed returns).
fn zero_of(t: Ty) -> Operand {
    match t {
        Ty::F64 => Operand::ConstF64(0.0),
        Ty::F32 => Operand::ConstF32(0.0),
        Ty::I32 => Operand::ConstI32(0),
        Ty::Ref => Operand::ConstNull,
        _ => Operand::ConstI64(0),
    }
}

fn map_op(o: BinOp) -> IB {
    match o {
        BinOp::Add | BinOp::AddWrap => IB::Add,
        BinOp::Sub | BinOp::SubWrap => IB::Sub,
        BinOp::Mul | BinOp::MulWrap => IB::Mul,
        BinOp::Div => IB::Div,
        BinOp::Rem => IB::Rem,
        BinOp::Eq => IB::CmpEq,
        BinOp::Ne => IB::CmpNe,
        BinOp::Lt => IB::CmpLt,
        BinOp::Le => IB::CmpLe,
        BinOp::Gt => IB::CmpGt,
        BinOp::Ge => IB::CmpGe,
        BinOp::And | BinOp::BitAnd => IB::And,
        BinOp::Or | BinOp::BitOr => IB::Or,
        BinOp::BitXor => IB::Xor,
        BinOp::Shl => IB::Shl,
        BinOp::Shr => IB::Shr,
        BinOp::UShr => IB::UShr,
    }
}

/// For comparisons/range with mixed const widths: use i32 constants as i64.
/// Best-effort source span of an expression (for debug line markers).
fn expr_span(e: &Expr) -> crate::diag::Span {
    use Expr::*;
    match e {
        Int(_, s) | Float(_, s) | Str(_, s) | Char(_, s) | Bool(_, s) | Ident(_, s) | SelfExpr(s) => *s,
        Unary { span, .. } | Binary { span, .. } | Call { span, .. } | TurboCall { span, .. }
        | Field { span, .. } | Index { span, .. } | If { span, .. } | Match { span, .. }
        | Cast { span, .. } | Try { span, .. } | Range { span, .. } | Lambda { span, .. }
        | List(_, span) | MapLit(_, span) | Comptime { span, .. } | Capsule { span, .. }
        | Spawn { span, .. } => *span,
        _ => crate::diag::Span(0, 0),
    }
}

/// Best-effort source span of a statement (for debug line markers).
fn stmt_span(s: &Stmt) -> crate::diag::Span {
    match s {
        Stmt::Let { span, .. } | Stmt::Assign { span, .. } | Stmt::While { span, .. } | Stmt::For { span, .. } => *span,
        Stmt::Return(_, span) | Stmt::Break(span) | Stmt::Continue(span) => *span,
        Stmt::Expr(e) => expr_span(e),
    }
}

fn to_i64(op: Operand) -> Operand {
    match op {
        Operand::ConstI32(v) => Operand::ConstI64(v as i64),
        other => other,
    }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Str(..) => "Str", Expr::Char(..) => "Char", Expr::SelfExpr(..) => "self",
        Expr::Field { .. } => "field access", Expr::Index { .. } => "Index",
        Expr::Match { .. } => "match", Expr::Lambda { .. } => "Lambda",
        Expr::List(..) => "list", Expr::Try { .. } => "?", Expr::Cast { .. } => "as",
        Expr::Comptime { .. } => "comptime", Expr::Capsule { .. } => "capsule",
        _ => "expression",
    }
}
