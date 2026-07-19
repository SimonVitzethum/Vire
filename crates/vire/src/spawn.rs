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
    // All parameter types per top-level function (for the C signature + Send check).
    let mut params: HashMap<String, Vec<Option<Type>>> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            params.insert(f.sig.name.clone(), f.sig.params.iter().map(|p| p.ty.clone()).collect());
        }
    }
    let mut ctx = Ctx { params, roots: HashSet::new(), gen: HashSet::new(), generated: Vec::new(), errs: Vec::new() };
    for item in &mut m.items {
        if let Item::Fn(f) = item {
            if let Some(body) = &mut f.body {
                ctx.block(body);
            }
        }
    }
    m.items.extend(std::mem::take(&mut ctx.generated));
    let workers: Vec<String> = ctx.roots.into_iter().collect();
    (ctx.errs, workers)
}

struct Ctx {
    params: HashMap<String, Vec<Option<Type>>>,
    /// Worker fn names invoked through generated glue → kept as RTA roots.
    roots: HashSet<String>,
    /// Generated shim names, for one-per-worker deduplication.
    gen: HashSet<String>,
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
        match e {
            Expr::Spawn { call, span } => {
                if let Some(rep) = self.build(call, *span) {
                    *e = rep;
                }
            }
            // `parallel_for(n, shared, worker)` — a normal call, desugared to a shim
            // that fork/joins n threads over the runtime `jrt_parallel_for`.
            Expr::Call { callee, args, span } => {
                if let Expr::Ident(n, _) = callee.as_ref() {
                    if n == "parallel_for" {
                        if let Some(rep) = self.build_parallel_for(args, *span) {
                            *e = rep;
                        }
                    }
                }
            }
            _ => {}
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

    /// Rewrite `spawn f(a, b, …)` into a call of a generated shim, generating the
    /// C glue + extern on first sight of worker `f`. One argument → a direct
    /// `__spawn_f(a)`; several → `__spawn_f(__envpack_f(a, b, …))`, where the args
    /// are boxed into an immortal env buffer that the worker unpacks.
    fn build(&mut self, call: &Expr, span: Span) -> Option<Expr> {
        let (fname, cargs) = match call {
            Expr::Call { callee, args, .. } => match callee.as_ref() {
                Expr::Ident(n, _) if !args.is_empty() => (n.clone(), args.clone()),
                Expr::Ident(_, _) => {
                    self.errs.push("spawn: the worker call needs at least one argument (a scalar or an Atomic/Mutex)".into());
                    return None;
                }
                _ => {
                    self.errs.push("spawn: expected `spawn worker(args…)` with a named worker function".into());
                    return None;
                }
            },
            _ => {
                self.errs.push("spawn: expected a function call `spawn worker(args…)`".into());
                return None;
            }
        };
        let ptys = match self.params.get(&fname) {
            Some(t) => t.clone(),
            None => {
                self.errs.push(format!("spawn: `{fname}` is not a top-level function"));
                return None;
            }
        };
        if ptys.len() != cargs.len() {
            self.errs.push(format!("spawn: `{fname}` takes {} argument(s) but {} were given", ptys.len(), cargs.len()));
            return None;
        }
        // Send check + C type per parameter.
        let mut ctys: Vec<&'static str> = Vec::with_capacity(ptys.len());
        for pt in &ptys {
            match boundary_cty(pt.as_ref()) {
                Ok(c) => ctys.push(c),
                Err(e) => {
                    self.errs.push(e);
                    return None;
                }
            }
        }
        let ty = |name: &str| Type { name: name.to_string(), args: vec![], borrowed: false, span };
        let shim = format!("__spawn_{fname}");
        self.roots.insert(fname.clone());
        let first = self.gen.insert(shim.clone());
        if cargs.len() == 1 {
            let cty = ctys[0];
            if first {
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
                let param = Param { name: "a".into(), ty: Some(ptys[0].clone().unwrap_or_else(|| ty("Int"))), default: None };
                let sig = FnSig { name: shim.clone(), generics: vec![], params: vec![param], ret: Some(ty("$Thread")), span };
                self.generated.push(Item::Extern { abi: "C".into(), items: vec![sig], links: vec![], header: None, span });
            }
            return Some(Expr::Call { callee: Box::new(Expr::Ident(shim, span)), args: cargs, span });
        }
        // Multi-argument: pack into an env buffer, unpack in the worker trampoline.
        let n = cargs.len();
        let pack = format!("__envpack_{fname}");
        if first {
            let sig_params = ctys.iter().enumerate().map(|(i, c)| format!("{c} a{i}")).collect::<Vec<_>>().join(", ");
            let call_params = ctys.iter().enumerate().map(|(i, c)| cast_from_slot(c, i)).collect::<Vec<_>>().join(", ");
            let stores = ctys.iter().enumerate().map(|(i, c)| format!("    s[{i}] = {};", cast_to_slot(c, &format!("a{i}")))).collect::<Vec<_>>().join("\n");
            let code = format!(
                "// generated glue for `spawn {fname}(..)` — trusted runtime handoff\n\
                 #include <stdint.h>\n\
                 extern void *jrt_spawn(int64_t (*fn)(void *), void *arg);\n\
                 extern void *jrt_env_new(int64_t n);\n\
                 extern int64_t {fname}({sig});\n\
                 static int64_t __run_{fname}(void *env) {{\n\
                 \x20   int64_t *s = (int64_t *)((char *)env + 16);\n\
                 \x20   return {fname}({call});\n\
                 }}\n\
                 void *{pack}({sig_params}) {{\n\
                 \x20   void *e = jrt_env_new({n});\n\
                 \x20   int64_t *s = (int64_t *)((char *)e + 16);\n\
                 {stores}\n\
                 \x20   return e;\n\
                 }}\n\
                 void *{shim}(void *env) {{ return jrt_spawn(__run_{fname}, env); }}\n",
                sig = ctys.join(", "),
                call = call_params,
            );
            self.generated.push(Item::Native { abi: "c-glue".into(), code, links: vec![], span });
            // extern __envpack_f(a0: T0, …) -> $Env   and   __spawn_f(e: $Env) -> $Thread
            let pack_params = ptys.iter().enumerate().map(|(i, pt)| Param { name: format!("a{i}"), ty: Some(pt.clone().unwrap_or_else(|| ty("Int"))), default: None }).collect();
            let pack_sig = FnSig { name: pack.clone(), generics: vec![], params: pack_params, ret: Some(ty("$Env")), span };
            self.generated.push(Item::Extern { abi: "C".into(), items: vec![pack_sig], links: vec![], header: None, span });
            let spawn_sig = FnSig { name: shim.clone(), generics: vec![], params: vec![Param { name: "e".into(), ty: Some(ty("$Env")), default: None }], ret: Some(ty("$Thread")), span };
            self.generated.push(Item::Extern { abi: "C".into(), items: vec![spawn_sig], links: vec![], header: None, span });
        }
        let env = Expr::Call { callee: Box::new(Expr::Ident(pack, span)), args: cargs, span };
        Some(Expr::Call { callee: Box::new(Expr::Ident(shim, span)), args: vec![env], span })
    }

    /// `parallel_for(n, shared, worker)` → `__pfor_worker(n, shared)`. Forks `n`
    /// threads running `worker(i, shared)` for i in 0..n and joins them all
    /// (`jrt_parallel_for`). `worker` is a bare function name; `shared` must be a
    /// Sync type (`Atomic`/`Mutex`).
    fn build_parallel_for(&mut self, args: &[Expr], span: Span) -> Option<Expr> {
        if args.len() != 3 {
            self.errs.push("parallel_for: expected `parallel_for(count, shared, worker)`".into());
            return None;
        }
        let worker = match &args[2] {
            Expr::Ident(n, _) => n.clone(),
            _ => {
                self.errs.push("parallel_for: the third argument must be a worker function name".into());
                return None;
            }
        };
        let ptys = match self.params.get(&worker) {
            Some(t) => t.clone(),
            None => {
                self.errs.push(format!("parallel_for: `{worker}` is not a top-level function"));
                return None;
            }
        };
        if ptys.len() != 2 {
            self.errs.push(format!("parallel_for: `{worker}` must take (index: Int, shared) — found {} parameter(s)", ptys.len()));
            return None;
        }
        // index is a scalar; shared must be a Sync type.
        if let Err(e) = boundary_cty(ptys[0].as_ref()) {
            self.errs.push(e);
            return None;
        }
        let shared_cty = match boundary_cty(ptys[1].as_ref()) {
            Ok(c) => c,
            Err(e) => {
                self.errs.push(e);
                return None;
            }
        };
        let ty = |name: &str| Type { name: name.to_string(), args: vec![], borrowed: false, span };
        let shim = format!("__pfor_{worker}");
        self.roots.insert(worker.clone());
        if self.gen.insert(shim.clone()) {
            let code = format!(
                "// generated glue for `parallel_for(.., {worker})` — trusted runtime handoff\n\
                 #include <stdint.h>\n\
                 extern void jrt_parallel_for(int64_t n, void *shared, int64_t (*fn)(int64_t, void *));\n\
                 extern int64_t {worker}(int64_t, {shared_cty});\n\
                 int64_t {shim}(int64_t n, void *shared) {{\n\
                 \x20   jrt_parallel_for(n, shared, (int64_t (*)(int64_t, void *)){worker});\n\
                 \x20   return 0;\n\
                 }}\n"
            );
            self.generated.push(Item::Native { abi: "c-glue".into(), code, links: vec![], span });
            let params = vec![
                Param { name: "n".into(), ty: Some(ty("Int")), default: None },
                Param { name: "shared".into(), ty: Some(ptys[1].clone().unwrap_or_else(|| ty("Int"))), default: None },
            ];
            let sig = FnSig { name: shim.clone(), generics: vec![], params, ret: Some(ty("Int")), span };
            self.generated.push(Item::Extern { abi: "C".into(), items: vec![sig], links: vec![], header: None, span });
        }
        Some(Expr::Call { callee: Box::new(Expr::Ident(shim, span)), args: vec![args[0].clone(), args[1].clone()], span })
    }
}

/// C expression storing arg `name` (of C type `cty`) into an int64 env slot.
fn cast_to_slot(cty: &str, name: &str) -> String {
    if cty == "void*" {
        format!("(int64_t)(intptr_t){name}")
    } else {
        format!("(int64_t){name}")
    }
}

/// C expression reading env slot `i` back as C type `cty`.
fn cast_from_slot(cty: &str, i: usize) -> String {
    if cty == "void*" {
        format!("(void *)(intptr_t)s[{i}]")
    } else {
        format!("(long)s[{i}]")
    }
}
