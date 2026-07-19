//! `spawn f(arg)` desugaring — Vire's safe-by-construction thread primitive.
//!
//! Each `spawn f(arg)` is rewritten to a call of a generated per-worker C shim
//! that hands `f` and its argument to the runtime `jrt_spawn` (a function-pointer
//! thread model, distinct from the Java Runnable/vtable path). The call yields an
//! opaque thread handle (a `$Thread` ref, immortal → RC-safe) that `join(h)`
//! awaits, returning the worker's result.
//!
//! Safe by construction — the Send check: the worker's parameter must be a
//! scalar (moved/copied per thread) or a `Sync` type (`Atomic`/`Mutex`, shared
//! safely). Sharing a bare mutable object across threads is a compile error, so a
//! data race cannot be written. (Deadlock-freedom is NOT guaranteed — see
//! REFERENCE §10.)
//!
//! The generated shim is compiler glue, not user `unsafe`, so it is emitted with
//! abi `"c-glue"`: compiled as C but exempt from the CSolver verification gate and
//! the `vire audit` trust boundary (it contains only a function-pointer handoff to
//! a trusted runtime entry point).

use crate::ast::*;
use crate::diag::Span;
use std::collections::{HashMap, HashSet};

/// Rewrite every `spawn f(arg)` and append the generated shims. Returns
/// (diagnostics, worker_names). A non-empty worker list means the program uses
/// threads (link with `-DFASTLLVM_THREADS -pthread`) and each worker must be kept
/// as a reachability root (it is called only from its C shim, invisible to RTA).
pub fn desugar_spawn(m: &mut Module) -> (Vec<String>, Vec<String>) {
    // Worker parameter type per top-level function (for the C signature + Send check).
    let mut param_ty: HashMap<String, Option<Type>> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            let first = f.sig.params.first().and_then(|p| p.ty.clone());
            param_ty.insert(f.sig.name.clone(), first);
        }
    }
    let mut ctx = Ctx { param_ty, shims: HashSet::new(), generated: Vec::new(), errs: Vec::new() };
    for item in &mut m.items {
        if let Item::Fn(f) = item {
            if let Some(body) = &mut f.body {
                ctx.block(body);
            }
        }
    }
    m.items.extend(std::mem::take(&mut ctx.generated));
    let workers: Vec<String> = ctx.shims.into_iter().collect();
    (ctx.errs, workers)
}

struct Ctx {
    param_ty: HashMap<String, Option<Type>>,
    /// Worker names that already have a generated `__spawn_<f>` shim (dedupe).
    shims: HashSet<String>,
    generated: Vec<Item>,
    errs: Vec<String>,
}

/// The C type + whether the argument is a reference (pointer) for a Vire type at
/// the spawn boundary. `None` type ⇒ default integer.
fn boundary_cty(t: Option<&Type>) -> Result<&'static str, String> {
    match t.map(|t| t.name.as_str()) {
        // Scalars: copied into the thread, no sharing.
        Some("Int") | Some("I64") | Some("U64") | Some("Bool") | Some("I32") | None => Ok("long"),
        // Sync reference types: shared safely (atomic ops / lock).
        Some("Atomic") | Some("Mutex") => Ok("void*"),
        // Anything else (a bare record/list/string) would share unsynchronized
        // mutable state across threads → refused (the Send check).
        Some(other) => Err(format!(
            "cannot send `{other}` across a `spawn` boundary — a shared mutable value is not \
             thread-safe. Pass a scalar (copied per thread) or a `Sync` type (`Atomic`/`Mutex`)."
        )),
    }
}

impl Ctx {
    fn block(&mut self, b: &mut Block) {
        for s in &mut b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &mut b.tail {
            self.expr(t);
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        match s {
            Stmt::Let { value: Some(e), .. } => self.expr(e),
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return(Some(e), _) => self.expr(e),
            Stmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::For { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        self.children(e);
        if let Expr::Spawn { call, span } = e {
            if let Some(rep) = self.build(call, *span) {
                *e = rep;
            }
        }
    }

    fn children(&mut self, e: &mut Expr) {
        match e {
            Expr::Unary { rhs, .. } => self.expr(rhs),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Call { callee, args, .. } => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Field { base, .. } => self.expr(base),
            Expr::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            Expr::If { cond, then, elifs, els, .. } => {
                self.expr(cond);
                self.block(then);
                for (c, b) in elifs {
                    self.expr(c);
                    self.block(b);
                }
                if let Some(b) = els {
                    self.block(b);
                }
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.expr(scrutinee);
                for (_, g, b) in arms {
                    if let Some(g) = g {
                        self.expr(g);
                    }
                    self.expr(b);
                }
            }
            Expr::Block(b) => self.block(b),
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::List(xs, _) => xs.iter_mut().for_each(|x| self.expr(x)),
            Expr::MapLit(kvs, _) => {
                for (k, v) in kvs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => self.expr(inner),
            Expr::Spawn { call, .. } => self.expr(call),
            _ => {}
        }
    }

    /// Rewrite `spawn f(arg)` → `__spawn_f(arg)`, generating the C shim + extern
    /// on first sight of worker `f`.
    fn build(&mut self, call: &Expr, span: Span) -> Option<Expr> {
        let (fname, arg) = match call {
            Expr::Call { callee, args, .. } => match callee.as_ref() {
                Expr::Ident(n, _) if args.len() == 1 => (n.clone(), args[0].clone()),
                Expr::Ident(_, _) => {
                    self.errs.push("spawn: the worker call must take exactly one argument (a scalar or an Atomic/Mutex)".into());
                    return None;
                }
                _ => {
                    self.errs.push("spawn: expected `spawn worker(arg)` with a named worker function".into());
                    return None;
                }
            },
            _ => {
                self.errs.push("spawn: expected a function call `spawn worker(arg)`".into());
                return None;
            }
        };
        let pty = match self.param_ty.get(&fname) {
            Some(t) => t.clone(),
            None => {
                self.errs.push(format!("spawn: `{fname}` is not a top-level function"));
                return None;
            }
        };
        let cty = match boundary_cty(pty.as_ref()) {
            Ok(c) => c,
            Err(e) => {
                self.errs.push(e);
                return None;
            }
        };
        let shim = format!("__spawn_{fname}");
        if self.shims.insert(fname.clone()) {
            // extern int64_t f(<cty>); the shim casts f to the runtime worker type
            // and hands it to jrt_spawn. void* out = the thread handle.
            let code = format!(
                "// generated glue for `spawn {fname}(..)` — trusted runtime handoff\n\
                 #include <stdint.h>\n\
                 extern void *jrt_spawn(int64_t (*fn)(void *), void *arg);\n\
                 extern int64_t {fname}({cty});\n\
                 void *{shim}({cty} a) {{\n\
                 \x20   return jrt_spawn((int64_t (*)(void *)){fname}, (void *)(intptr_t)a);\n\
                 }}\n"
            );
            self.generated.push(Item::Native { abi: "c-glue".into(), code, links: vec![], span });
            // extern "C" __spawn_f(a: <pty>) -> $Thread   ($Thread ⇒ Ty::Ref handle)
            let ty = |name: &str| Type { name: name.to_string(), args: vec![], borrowed: false, span };
            let param = Param { name: "a".into(), ty: Some(pty.clone().unwrap_or_else(|| ty("Int"))), default: None };
            let sig = FnSig { name: shim.clone(), generics: vec![], params: vec![param], ret: Some(ty("$Thread")), span };
            self.generated.push(Item::Extern { abi: "C".into(), items: vec![sig], links: vec![], header: None, span });
        }
        Some(Expr::Call { callee: Box::new(Expr::Ident(shim, span)), args: vec![arg], span })
    }
}
