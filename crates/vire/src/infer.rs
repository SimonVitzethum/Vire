//! Lightweight, whole-program type inference (F5 core, monomorphic).
//!
//! Purpose until full HM/bidirectional inference: **fill in un-annotated
//! parameter types**, so that e.g. float functions without `: Float` lower
//! correctly. Works via union-find over the scalar type lattice
//! (I64/F64/I32/Ref/Void); everything higher (generics/traits/reference types of
//! user types) is left to later stages and is treated conservatively here as
//! `Ref`/open. Best-effort: on conflicts it does not abort, the affected
//! parameter stays un-annotated (lowering then defaults to I64).
//!
//! Result: `infer_module` **mutates** the AST and writes concrete types into
//! previously `None` parameters. Lowering (`lower`) reads them unchanged.

use std::collections::HashMap;

use crate::ast::*;
use crate::diag::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum T {
    I64,
    F64,
    I32,
    Ref,
    Void,
    Var(u32),
}

/// Public, resolved type of an expression — the value stored in the typed-AST
/// side-table. `Unknown` is a type variable that stayed free (nothing constrained
/// it): honest about what inference could not determine, rather than defaulting.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InferTy {
    Int,
    Float,
    Bool,
    Ref,
    Unit,
    Unknown,
}

impl InferTy {
    pub fn name(self) -> &'static str {
        match self {
            InferTy::Int => "Int",
            InferTy::Float => "Float",
            InferTy::Bool => "Bool",
            InferTy::Ref => "Ref",
            InferTy::Unit => "Unit",
            InferTy::Unknown => "?",
        }
    }
    fn of(t: T) -> Self {
        match t {
            T::I64 => InferTy::Int,
            T::F64 => InferTy::Float,
            T::I32 => InferTy::Bool,
            T::Ref => InferTy::Ref,
            T::Void => InferTy::Unit,
            T::Var(_) => InferTy::Unknown,
        }
    }
}

/// The typed AST: inferred type of every expression, keyed by its source span.
/// AST nodes have no identity, so the span (byte range) is the key. This is the
/// side-table Phase 1 produces — the persisted per-expression type view that
/// comptime/reflection/macros consume.
pub type ExprTypes = HashMap<Span, InferTy>;

/// Union-find over type variables; concrete types are leaves.
struct Unifier {
    parent: Vec<T>, // parent[v] for Var(v); non-Var = bound concrete type
    /// Collisions of two concrete types — these are mistyped programs that
    /// MUST be reported (otherwise silently miscompiled to the I64 default).
    conflicts: Vec<(T, T)>,
}

impl Unifier {
    fn new() -> Self {
        Unifier { parent: Vec::new(), conflicts: Vec::new() }
    }
    fn fresh(&mut self) -> T {
        let id = self.parent.len() as u32;
        self.parent.push(T::Var(id));
        T::Var(id)
    }
    fn resolve(&self, t: T) -> T {
        let mut cur = t;
        while let T::Var(v) = cur {
            let p = self.parent[v as usize];
            if p == T::Var(v) {
                return cur; // free variable
            }
            cur = p;
        }
        cur
    }
    /// Unifies two types. On conflict (two different concrete types)
    /// nothing happens (best-effort) — the caller does not rely on it.
    fn unify(&mut self, a: T, b: T) {
        let (ra, rb) = (self.resolve(a), self.resolve(b));
        if ra == rb {
            return;
        }
        match (ra, rb) {
            (T::Var(v), other) | (other, T::Var(v)) => {
                self.parent[v as usize] = other;
            }
            // Conflict of two concrete types: do NOT swallow silently — record and
            // report. The default fallback (I64) must not quietly wave through a
            // mistyped program.
            _ => self.conflicts.push((ra, rb)),
        }
    }
}

/// Global signature of a function: type variables of the parameters + return.
struct Sig {
    params: Vec<T>,
    ret: T,
}

/// Infers parameter/return types and writes them into the AST. Returns
/// conflict diagnostics (mistyped programs) — these MUST be reported, because
/// the default fallback (I64) would otherwise silently miscompile them.
pub fn infer_module(m: &mut Module) -> Vec<String> {
    infer_module_typed(m).0
}

/// Like `infer_module`, but also returns the typed-AST side-table: the resolved
/// type of every expression, keyed by source span. This is the Phase-1 foundation
/// for the compile-time programming layer.
pub fn infer_module_typed(m: &mut Module) -> (Vec<String>, ExprTypes) {
    let mut u = Unifier::new();
    // Per-expression type record (type variables; resolved to concrete at the end).
    let mut rec: HashMap<Span, T> = HashMap::new();
    // `@gpu` kernel names (param 0 = injected thread index, dropped at call sites).
    let gpu_fns: std::collections::HashSet<String> = m
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Fn(f) if f.attrs.iter().any(|a| a.name == "gpu") => Some(f.sig.name.clone()),
            _ => None,
        })
        .collect();
    // 1. Create global signature variables (annotated → concrete, otherwise fresh).
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            // Do NOT type-check generic functions monomorphically — their body is
            // polymorphic (T is not a concrete type). Each monomorph. instance is
            // created during lowering; calls `id(42)` stay unconstrained here.
            if !f.sig.generics.is_empty() {
                continue;
            }
            let params = f
                .sig
                .params
                .iter()
                .map(|p| match ann_ty(p.ty.as_ref()) {
                    Some(t) => t,
                    None => u.fresh(),
                })
                .collect();
            let ret = match ann_ty(f.sig.ret.as_ref()) {
                Some(t) => t,
                None => u.fresh(),
            };
            sigs.insert(f.sig.name.clone(), Sig { params, ret });
        }
        // extern "C" signatures are fully annotated (C-ABI) → concrete types,
        // so that calls (`sqrt(x)`) constrain their arguments correctly.
        if let Item::Extern { items, .. } = it {
            for s in items {
                let params = s.params.iter().map(|p| ann_ty(p.ty.as_ref()).unwrap_or(T::I64)).collect();
                let ret = ann_ty(s.ret.as_ref()).unwrap_or(T::Void);
                sigs.insert(s.name.clone(), Sig { params, ret });
            }
        }
    }
    // 2. Traverse bodies, collect constraints.
    for it in &m.items {
        if let Item::Fn(f) = it {
            if !f.sig.generics.is_empty() {
                continue; // generic → do not infer monomorphically
            }
            // `@vertex`/`@fragment` are SPIR-V shaders compiled by shader.rs, not
            // host code — their bodies use shader-only forms (`vecN`, vector math)
            // that the host inference doesn't model, so skip them here.
            if f.attrs.iter().any(|a| matches!(a.name.as_str(), "vertex" | "fragment" | "mesh" | "task" | "compute" | "gpuvk")) {
                continue;
            }
            if let Some(body) = &f.body {
                let sig = &sigs[&f.sig.name];
                let mut cx = Ctx {
                    u: &mut u,
                    sigs: &sigs,
                    gpu: &gpu_fns,
                    scopes: vec![HashMap::new()],
                    ret: sig.ret,
                    types: &mut rec,
                };
                for (p, pv) in f.sig.params.iter().zip(&sig.params) {
                    cx.bind(&p.name, *pv);
                }
                cx.infer_block(body, true);
            }
        }
    }
    // 3. Write back resolved parameter AND return types (only previously
    //    un-annotated ones). `main` is left out — stays Void in lowering.
    let resolved: HashMap<String, (Vec<T>, T)> = sigs
        .iter()
        .map(|(n, s)| (n.clone(), (s.params.iter().map(|t| u.resolve(*t)).collect(), u.resolve(s.ret))))
        .collect();
    for it in m.items.iter_mut() {
        if let Item::Fn(f) = it {
            if f.sig.generics.is_empty() {
            let (rs, ret) = &resolved[&f.sig.name];
            for (p, t) in f.sig.params.iter_mut().zip(rs) {
                if p.ty.is_none() {
                    if let Some(name) = concrete_name(*t) {
                        p.ty = Some(Type { name: name.into(), args: vec![], borrowed: false, span: Span(0, 0) });
                    }
                }
            }
            if f.sig.ret.is_none() && f.sig.name != "main" {
                if let Some(name) = concrete_name(*ret) {
                    f.sig.ret = Some(Type { name: name.into(), args: vec![], borrowed: false, span: Span(0, 0) });
                }
            }
            }
        }
    }
    // 4. Report type conflicts (deduplicated). Best-effort inference, but a
    //    detected conflict is a real type error, not noise.
    let mut msgs: Vec<String> = u
        .conflicts
        .iter()
        .map(|(a, b)| format!("type conflict: {} vs {} (inference)", ty_name(*a), ty_name(*b)))
        .collect();
    msgs.sort();
    msgs.dedup();

    // Resolve the recorded type variables to concrete types → the typed AST.
    let exprtypes: ExprTypes = rec.iter().map(|(s, t)| (*s, InferTy::of(u.resolve(*t)))).collect();
    (msgs, exprtypes)
}

/// Source span of an expression node (the side-table key).
pub(crate) fn expr_span(e: &Expr) -> Span {
    match e {
        Expr::Int(_, s)
        | Expr::Float(_, s)
        | Expr::Str(_, s)
        | Expr::Char(_, s)
        | Expr::Bool(_, s)
        | Expr::Ident(_, s)
        | Expr::SelfExpr(s)
        | Expr::List(_, s)
        | Expr::MapLit(_, s) => *s,
        Expr::Unary { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Call { span, .. }
        | Expr::TurboCall { span, .. }
        | Expr::Field { span, .. }
        | Expr::Index { span, .. }
        | Expr::If { span, .. }
        | Expr::Match { span, .. }
        | Expr::Lambda { span, .. }
        | Expr::Comprehension { span, .. }
        | Expr::Try { span, .. }
        | Expr::Cast { span, .. }
        | Expr::Comptime { span, .. }
        | Expr::Range { span, .. }
        | Expr::Capsule { span, .. }
        | Expr::Spawn { span, .. } => *span,
        Expr::Block(b) => b.span,
    }
}

fn ty_name(t: T) -> &'static str {
    match t {
        T::I64 => "Int",
        T::F64 => "Float",
        T::I32 => "I32/Bool",
        T::Ref => "object/ref",
        T::Void => "Unit",
        T::Var(_) => "?",
    }
}

struct Ctx<'a> {
    u: &'a mut Unifier,
    sigs: &'a HashMap<String, Sig>,
    /// Names of `@gpu` kernels: at a call site their parameter 0 (the injected
    /// thread index) is not passed by the caller, so the call's args unify with
    /// `sig.params[1..]`. The body still binds all params (index included).
    gpu: &'a std::collections::HashSet<String>,
    scopes: Vec<HashMap<String, T>>,
    ret: T,
    /// Typed-AST side-table being populated: span → (unresolved) type variable.
    types: &'a mut HashMap<Span, T>,
}

impl<'a> Ctx<'a> {
    fn bind(&mut self, name: &str, t: T) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), t);
    }
    fn lookup(&self, name: &str) -> Option<T> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn infer_block(&mut self, b: &Block, tail_is_ret: bool) -> T {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            self.infer_stmt(s);
        }
        let t = match &b.tail {
            Some(e) => {
                let te = self.infer_expr(e);
                if tail_is_ret {
                    let (rt, ret) = (te, self.ret);
                    self.u.unify(rt, ret);
                }
                te
            }
            None => T::Void,
        };
        self.scopes.pop();
        t
    }

    fn infer_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { mutable, name, value, .. } => {
                let vt = value.as_ref().map(|v| self.infer_expr(v)).unwrap_or_else(|| self.u.fresh());
                if !mutable {
                    if let Some(existing) = self.lookup(name) {
                        self.u.unify(existing, vt); // assignment, not a new binding
                        return;
                    }
                }
                let slot = self.u.fresh();
                self.u.unify(slot, vt);
                self.bind(name, slot);
            }
            Stmt::Assign { target, value, .. } => {
                let vt = self.infer_expr(value);
                if let Expr::Ident(n, _) = target {
                    if let Some(t) = self.lookup(n) {
                        self.u.unify(t, vt);
                    }
                }
            }
            Stmt::Expr(e) => {
                self.infer_expr(e);
            }
            Stmt::Return(e, _) => {
                if let Some(e) = e {
                    let te = self.infer_expr(e);
                    let ret = self.ret;
                    self.u.unify(te, ret);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.infer_expr(cond);
                self.infer_block(body, false);
            }
            Stmt::For { pat, iter, body, .. } => {
                // `for i in a..b`: i:I64, range ends I64. `for x in liste`: the
                // element type is unknown to the inference (without array type) → fresh
                // variable, so that the usage constrains it (no I64 coercion).
                let is_range = matches!(iter, Expr::Range { .. });
                if let Expr::Range { start, end, .. } = iter {
                    let s = self.infer_expr(start);
                    let e = self.infer_expr(end);
                    self.u.unify(s, T::I64);
                    self.u.unify(e, T::I64);
                } else {
                    self.infer_expr(iter);
                }
                self.scopes.push(HashMap::new());
                if let Pattern::Bind(n, _) = pat {
                    let t = if is_range { T::I64 } else { self.u.fresh() };
                    self.bind(n, t);
                }
                self.infer_block(body, false);
                self.scopes.pop();
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    /// Infer an expression's type AND record it in the typed-AST side-table,
    /// keyed by the node's span.
    fn infer_expr(&mut self, e: &Expr) -> T {
        let t = self.infer_expr_inner(e);
        self.types.insert(expr_span(e), t);
        t
    }

    fn infer_expr_inner(&mut self, e: &Expr) -> T {
        match e {
            Expr::Int(..) => T::I64,
            Expr::Float(..) => T::F64,
            Expr::Bool(..) => T::I32,
            Expr::Str(..) => T::Ref,
            Expr::Char(..) => T::I32,
            Expr::Ident(n, _) => self.lookup(n).unwrap_or_else(|| self.u.fresh()),
            Expr::Unary { op, rhs, .. } => {
                let r = self.infer_expr(rhs);
                match op {
                    UnOp::Neg => r,
                    UnOp::Not => T::I32,
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let l = self.infer_expr(lhs);
                let r = self.infer_expr(rhs);
                // `+` with a Ref/String side = string concatenation → do NOT
                // unify (numbers become strings at runtime), result Ref.
                if matches!(op, BinOp::Add) {
                    let (rl, rr) = (self.u.resolve(l), self.u.resolve(r));
                    if rl == T::Ref || rr == T::Ref {
                        return T::Ref;
                    }
                }
                // Numeric promotion: a genuinely-mixed Int/Float operation is NOT a
                // conflict — the Int side promotes to Float (matching the `sitofp` the
                // lowerer inserts), and the result is Float. Only skip the unify when both
                // sides are already concrete I64 and F64; otherwise unify as before so a
                // free type variable still propagates (`i * 1.0` fixes `i` to Int, not Float).
                let (rl, rr) = (self.u.resolve(l), self.u.resolve(r));
                let mixed_num = (rl == T::I64 && rr == T::F64) || (rl == T::F64 && rr == T::I64);
                if !mixed_num {
                    self.u.unify(l, r);
                }
                if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                    T::I32
                } else if mixed_num {
                    T::F64
                } else {
                    l
                }
            }
            Expr::Call { callee, args, .. } => {
                let arg_ts: Vec<T> = args.iter().map(|a| self.infer_expr(a)).collect();
                if let Expr::Ident(n, _) = callee.as_ref() {
                    if n == "print" {
                        return T::Void;
                    }
                    // `@gpu` device intrinsics have fixed return types — teach them to
                    // inference so one used as a kernel's tail (e.g. `gpu_sync()`)
                    // doesn't look like a returned value (see lower::gpu_intrinsic_*).
                    match n.as_str() {
                        "gpu_sync" => return T::Void,
                        "gpu_sqrt" | "gpu_fabs" | "gpu_floor" | "gpu_ceil" | "gpu_fmin"
                        | "gpu_fmax" | "sqrt" | "sin" | "cos" | "floor" => return T::F64,
                        "gpu_gid" | "gpu_gsize" | "gpu_tid" | "gpu_bid" | "gpu_bdim"
                        | "gpu_gdim" | "gpu_atomic_add" | "gpu_shfl_down"
                        | "gpu_warp_reduce_add" | "vk_triangle" | "vk_frame_bg" | "vk_window" | "vk_bench" | "vk_textured" | "vk_two_pass" | "vk_chain" | "vk_blend2" | "vk_frame" | "vk_window_mesh" | "vk_texture_draw" | "vk_draw_handle"
                        | "vk_mesh" | "vk_mesh_c" | "vk_mesh_shader" | "vk_draw" | "vk_draw_tex" | "vk_draw_tex2" | "vk_draw_buf" | "vk_draw_tex_buf" | "vk_render_ppm" | "vk_render3d" | "vk_resolution" | "vk_pipeline_depth" | "vk_motion" | "vk_gpu_count" | "vk_gpu_list" | "vk_gpu_select" | "vk_jitter" | "vk_depth"
                        | "vk_render_res" | "vk_display_res" | "vk_upscale"
                        | "vk_mesh_scene" | "vk_mesh_scene_cull"
                        | "vk_mesh_built" | "vk_built_color" | "gpuvk_run" => return T::I64,
                        // Returns an RC-bound GPU texture handle (a Vire object).
                        "vk_texture_new" | "vk_buffer_new" | "vk_session" => return T::Ref,
                        "vk_buffer_get" => return T::F64,
                        _ => {}
                    }
                    if let Some(sig) = self.sigs.get(n) {
                        // `@gpu` kernels: skip param 0 (the injected thread index) —
                        // the caller passes only params 1.. .
                        let skip = if self.gpu.contains(n) { 1 } else { 0 };
                        for (at, pt) in arg_ts.iter().zip(sig.params.iter().skip(skip)) {
                            self.u.unify(*at, *pt);
                        }
                        return sig.ret;
                    }
                }
                self.u.fresh()
            }
            Expr::If { cond, then, elifs, els, .. } => {
                self.infer_expr(cond);
                let t = self.infer_block(then, false);
                for (ec, eb) in elifs {
                    self.infer_expr(ec);
                    let bt = self.infer_block(eb, false);
                    self.u.unify(t, bt);
                }
                if let Some(e) = els {
                    let et = self.infer_block(e, false);
                    self.u.unify(t, et);
                }
                t
            }
            Expr::Block(b) => self.infer_block(b, false),
            // `x as T` → target type (argument free, it is converted).
            Expr::Cast { inner, ty, .. } => {
                self.infer_expr(inner);
                ann_ty(Some(ty)).unwrap_or(T::I64)
            }
            // Comprehension: bind the variable freshly (element type unknown to
            // the inference without array type), infer elem/cond. Result = fresh (array).
            Expr::Comprehension { var, iter, elem, cond, .. } => {
                self.infer_expr(iter);
                self.scopes.push(std::collections::HashMap::new());
                let v = self.u.fresh();
                self.bind(var, v);
                if let Some(c) = cond {
                    self.infer_expr(c);
                }
                self.infer_expr(elem);
                self.scopes.pop();
                self.u.fresh()
            }
            _ => self.u.fresh(),
        }
    }
}

/// Annotation name → scalar type (analogous to `lower::ty_of`, but in the T lattice).
/// Unknown/user types → `Ref` (conservative). None → None (free).
fn ann_ty(t: Option<&Type>) -> Option<T> {
    let t = t?;
    Some(match t.name.as_str() {
        "Float" | "F64" => T::F64,
        "F32" => T::F64,
        "Bool" => T::I32,
        "Str" => T::Ref,
        "I32" | "U32" => T::I32,
        "Int" | "I64" | "U64" => T::I64,
        "Unit" => T::Void,
        _ => T::Ref,
    })
}

/// T → annotation name for the write-back (only concrete scalar types).
fn concrete_name(t: T) -> Option<&'static str> {
    match t {
        T::I64 => Some("Int"),
        T::F64 => Some("Float"),
        T::I32 => Some("I32"),
        // A reference of unknown class (inference collapses every object/Str to `Ref`).
        // Write back the honest neutral name `Ref` — NOT `Str`, which falsely labelled
        // every inferred object parameter a string. Both lower identically (`Ty::Ref`,
        // no class → string methods still dispatch on a genuine string), so this is a
        // truthfulness fix, but it stops `vire infer`/annotations from lying.
        T::Ref => Some("Ref"),
        T::Void => Some("Unit"),
        T::Var(_) => None,
    }
}
