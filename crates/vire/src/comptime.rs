//! Compile-time evaluation pass — runs *after* inference, on the AST.
//!
//! This is Phase 2 of the compile-time programming layer (see TODO.md): comptime
//! folding moves out of lowering into a dedicated source-to-source pass positioned
//! after `infer_module`, so it can (now and increasingly) consult the typed AST /
//! type graph instead of being fused with codegen.
//!
//! What it does today:
//!   * collects module-level `const` declarations into a compile-time environment
//!     (each may reference earlier consts), then **drops** the `const` items;
//!   * substitutes a `const`-named identifier with its literal value — respecting
//!     lexical scope, so a local/parameter of the same name shadows the const;
//!   * folds `comptime <expr>` to a literal and `comptime if C { A } else { B }` to
//!     the taken branch (dropping the untaken one).
//!
//! Best-effort and non-regressive: anything it cannot evaluate (e.g. a `comptime`
//! that references a not-yet-bound value generic `N`) is left untouched for the
//! existing handling in lowering. Only successful evaluations rewrite the AST.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::Span;

/// A compile-time scalar value (mirrors `lower.rs`'s `CVal`, plus an environment).
#[derive(Clone, Copy)]
enum CVal {
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Evaluate `const` declarations, then fold comptime/const references across all
/// bodies. Returns diagnostics for consts whose initializer is not constant.
pub fn eval_comptime(m: &mut Module) -> Vec<String> {
    let mut errs = Vec::new();

    // 0. Registry of pure functions callable at compile time (`comptime f(args)`).
    //    Any non-generic function with a body is a candidate; whether it is
    //    *actually* evaluable comptime is decided per call (a body that touches
    //    runtime-only operations simply fails to evaluate and is deferred).
    let mut funcs: HashMap<String, FnDef> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if f.sig.generics.is_empty() && f.body.is_some() {
                funcs.insert(f.sig.name.clone(), f.clone());
            }
        }
    }

    // 1. Build the const environment in source order (forward references to an
    //    earlier const resolve; a later one is not yet visible).
    let mut env: HashMap<String, CVal> = HashMap::new();
    for it in &m.items {
        if let Item::Const { name, value, .. } = it {
            let mut ip = Interp::new(&env, &funcs);
            match ip.eval(value) {
                Some(v) => {
                    env.insert(name.clone(), v);
                }
                None => {
                    if let Some(e) = ip.overflow {
                        errs.push(format!("const `{name}`: {e}"));
                    } else {
                        errs.push(format!("const `{name}`: initializer is not a compile-time constant"));
                    }
                }
            }
        }
    }

    // 2. Rewrite every function/method body.
    let mut w = Walker { consts: &env, funcs: &funcs, errs: &mut errs, scopes: Vec::new() };
    for it in m.items.iter_mut() {
        match it {
            Item::Fn(f) => w.fn_def(f),
            Item::Type(t) => {
                for md in &mut t.methods {
                    w.fn_def(md);
                }
            }
            Item::Impl(im) => {
                for md in &mut im.methods {
                    w.fn_def(md);
                }
            }
            Item::Trait(tr) => {
                for md in &mut tr.methods {
                    w.fn_def(md);
                }
            }
            _ => {}
        }
    }

    // 3. The consts are fully inlined now — drop the declarations.
    m.items.retain(|it| !matches!(it, Item::Const { .. }));
    errs
}

/// A budget-limited compile-time interpreter over the scalar value domain. It
/// evaluates comptime expressions with a real environment: literals, const/local
/// lookup, arithmetic/logic, `comptime` blocks with `let`/assignment/`if`/`for`/
/// `while`, and calls to pure module functions (`comptime f(x)`). Anything it
/// cannot execute at compile time (a runtime-only op, unbound name, reference
/// type) → `None`, and the caller defers the node to lowering. A blown step or
/// recursion budget sets `overflow` (a hard error).
struct Interp<'a> {
    consts: &'a HashMap<String, CVal>,
    funcs: &'a HashMap<String, FnDef>,
    /// Mutable local bindings, innermost scope last. A function call swaps in a
    /// fresh stack (lexical isolation — a callee sees only its params + consts).
    scopes: Vec<HashMap<String, CVal>>,
    steps: usize,
    depth: usize,
    overflow: Option<String>,
}

/// Upper bound on evaluation steps (loop iterations / statements) — halts an
/// accidental infinite comptime loop instead of hanging the compiler.
const STEP_LIMIT: usize = 2_000_000;
/// Upper bound on comptime call recursion depth.
const DEPTH_LIMIT: usize = 256;

impl<'a> Interp<'a> {
    fn new(consts: &'a HashMap<String, CVal>, funcs: &'a HashMap<String, FnDef>) -> Self {
        Interp { consts, funcs, scopes: vec![HashMap::new()], steps: 0, depth: 0, overflow: None }
    }

    fn tick(&mut self) -> Option<()> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            self.overflow = Some("comptime evaluation exceeded the step budget (possible infinite loop)".into());
            return None;
        }
        if self.overflow.is_some() {
            return None;
        }
        Some(())
    }

    fn lookup(&self, n: &str) -> Option<CVal> {
        self.scopes.iter().rev().find_map(|s| s.get(n).copied()).or_else(|| self.consts.get(n).copied())
    }
    fn assign(&mut self, n: &str, v: CVal) -> Option<()> {
        for s in self.scopes.iter_mut().rev() {
            if s.contains_key(n) {
                s.insert(n.to_string(), v);
                return Some(());
            }
        }
        None // assignment to an unknown/runtime name → not comptime
    }
    fn bind(&mut self, n: &str, v: CVal) {
        self.scopes.last_mut().unwrap().insert(n.to_string(), v);
    }

    fn eval(&mut self, e: &Expr) -> Option<CVal> {
        self.tick()?;
        match e {
            Expr::Int(v, _) => Some(CVal::Int(*v as i64)),
            Expr::Float(v, _) => Some(CVal::Float(*v)),
            Expr::Bool(v, _) => Some(CVal::Bool(*v)),
            Expr::Ident(n, _) => self.lookup(n),
            Expr::Comptime { inner, .. } => self.eval(inner),
            Expr::Block(b) => self.eval_block(b),
            Expr::If { cond, then, elifs, els, .. } => {
                if let CVal::Bool(c) = self.eval(cond)? {
                    if c {
                        return self.eval_block(then);
                    }
                }
                for (ec, eb) in elifs {
                    if let CVal::Bool(true) = self.eval(ec)? {
                        return self.eval_block(eb);
                    }
                }
                match els {
                    Some(b) => self.eval_block(b),
                    None => None,
                }
            }
            Expr::Unary { op, rhs, .. } => {
                let r = self.eval(rhs)?;
                match (op, r) {
                    (UnOp::Neg, CVal::Int(i)) => Some(CVal::Int(i.wrapping_neg())),
                    (UnOp::Neg, CVal::Float(f)) => Some(CVal::Float(-f)),
                    (UnOp::Not, CVal::Bool(b)) => Some(CVal::Bool(!b)),
                    _ => None,
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let a = self.eval(lhs)?;
                let b = self.eval(rhs)?;
                match (a, b) {
                    (CVal::Int(x), CVal::Int(y)) => int_binop(*op, x, y),
                    (CVal::Float(x), CVal::Float(y)) => float_binop(*op, x, y),
                    (CVal::Bool(x), CVal::Bool(y)) => bool_binop(*op, x, y),
                    _ => None,
                }
            }
            Expr::Call { callee, args, .. } => {
                let name = match callee.as_ref() {
                    Expr::Ident(n, _) => n,
                    _ => return None,
                };
                let argv: Vec<CVal> = args.iter().map(|a| self.eval(a)).collect::<Option<_>>()?;
                let f = self.funcs.get(name)?.clone();
                self.call_fn(&f, &argv)
            }
            _ => None,
        }
    }

    /// Evaluate a block for its value: run its statements (side-effecting the
    /// scope), then its tail. A block with no tail has no comptime value → `None`.
    fn eval_block(&mut self, b: &Block) -> Option<CVal> {
        self.scopes.push(HashMap::new());
        let ok = self.exec_stmts(&b.stmts);
        let v = if ok.is_some() { b.tail.as_ref().and_then(|t| self.eval(t)) } else { None };
        self.scopes.pop();
        v
    }

    fn exec_stmts(&mut self, stmts: &[Stmt]) -> Option<()> {
        for s in stmts {
            self.exec_stmt(s)?;
        }
        Some(())
    }

    fn exec_stmt(&mut self, s: &Stmt) -> Option<()> {
        self.tick()?;
        match s {
            Stmt::Let { name, value, .. } => {
                let v = self.eval(value.as_ref()?)?;
                // `mut x = e` that re-assigns an existing binding vs. a new one:
                // if already bound in scope, update; else introduce.
                if self.assign(name, v).is_none() {
                    self.bind(name, v);
                }
                Some(())
            }
            Stmt::Assign { target, op, value, .. } => {
                let n = match target {
                    Expr::Ident(n, _) => n,
                    _ => return None,
                };
                let rhs = self.eval(value)?;
                let v = match op {
                    None => rhs,
                    Some(o) => {
                        let cur = self.lookup(n)?;
                        match (cur, rhs) {
                            (CVal::Int(x), CVal::Int(y)) => int_binop(*o, x, y)?,
                            (CVal::Float(x), CVal::Float(y)) => float_binop(*o, x, y)?,
                            (CVal::Bool(x), CVal::Bool(y)) => bool_binop(*o, x, y)?,
                            _ => return None,
                        }
                    }
                };
                self.assign(n, v)
            }
            Stmt::Expr(e) => self.eval(e).map(|_| ()),
            Stmt::While { cond, body, .. } => {
                while let CVal::Bool(true) = self.eval(cond)? {
                    self.tick()?;
                    self.scopes.push(HashMap::new());
                    let r = self.exec_stmts(&body.stmts);
                    self.scopes.pop();
                    r?;
                }
                Some(())
            }
            Stmt::For { pat, iter, body, .. } => {
                // Comptime `for i in a..b { … }`: execute the body once per value.
                let (start, end, incl) = match iter {
                    Expr::Range { start, end, inclusive, .. } => (self.eval(start)?, self.eval(end)?, *inclusive),
                    _ => return None,
                };
                let (s, e) = match (start, end) {
                    (CVal::Int(s), CVal::Int(e)) => (s, e),
                    _ => return None,
                };
                let name = match pat {
                    Pattern::Bind(n, _) => Some(n.clone()),
                    Pattern::Wildcard(_) => None,
                    _ => return None,
                };
                let last = if incl { e } else { e - 1 };
                let mut i = s;
                while i <= last {
                    self.tick()?;
                    self.scopes.push(HashMap::new());
                    if let Some(n) = &name {
                        self.bind(n, CVal::Int(i));
                    }
                    let r = self.exec_stmts(&body.stmts);
                    self.scopes.pop();
                    r?;
                    i += 1;
                }
                Some(())
            }
            // return/break/continue are not modelled → not comptime-evaluable.
            Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_) => None,
        }
    }

    fn call_fn(&mut self, f: &FnDef, args: &[CVal]) -> Option<CVal> {
        if self.depth >= DEPTH_LIMIT {
            self.overflow = Some("comptime call recursion limit exceeded".into());
            return None;
        }
        let body = f.body.as_ref()?;
        if f.sig.params.len() != args.len() {
            return None;
        }
        self.depth += 1;
        // Lexical isolation: the callee sees only its parameters + consts/funcs,
        // never the caller's locals. Swap the scope stack, restore on return.
        let saved = std::mem::take(&mut self.scopes);
        let mut frame = HashMap::new();
        for (p, v) in f.sig.params.iter().zip(args) {
            frame.insert(p.name.clone(), *v);
        }
        self.scopes.push(frame);
        let r = self.eval_block(body);
        self.scopes = saved;
        self.depth -= 1;
        r
    }
}

fn int_binop(op: BinOp, x: i64, y: i64) -> Option<CVal> {
    use BinOp::*;
    Some(match op {
        Add => CVal::Int(x.wrapping_add(y)),
        Sub => CVal::Int(x.wrapping_sub(y)),
        Mul => CVal::Int(x.wrapping_mul(y)),
        AddWrap => CVal::Int(x.wrapping_add(y)),
        SubWrap => CVal::Int(x.wrapping_sub(y)),
        MulWrap => CVal::Int(x.wrapping_mul(y)),
        Div => CVal::Int(x.checked_div(y)?),
        Rem => CVal::Int(x.checked_rem(y)?),
        BitAnd => CVal::Int(x & y),
        BitOr => CVal::Int(x | y),
        BitXor => CVal::Int(x ^ y),
        Shl => CVal::Int(x.wrapping_shl(y as u32)),
        Shr => CVal::Int(x.wrapping_shr(y as u32)),
        Eq => CVal::Bool(x == y),
        Ne => CVal::Bool(x != y),
        Lt => CVal::Bool(x < y),
        Le => CVal::Bool(x <= y),
        Gt => CVal::Bool(x > y),
        Ge => CVal::Bool(x >= y),
        And | Or => return None,
    })
}

fn float_binop(op: BinOp, x: f64, y: f64) -> Option<CVal> {
    use BinOp::*;
    Some(match op {
        Add => CVal::Float(x + y),
        Sub => CVal::Float(x - y),
        Mul => CVal::Float(x * y),
        Div => CVal::Float(x / y),
        Eq => CVal::Bool(x == y),
        Ne => CVal::Bool(x != y),
        Lt => CVal::Bool(x < y),
        Le => CVal::Bool(x <= y),
        Gt => CVal::Bool(x > y),
        Ge => CVal::Bool(x >= y),
        _ => return None,
    })
}

fn bool_binop(op: BinOp, x: bool, y: bool) -> Option<CVal> {
    use BinOp::*;
    Some(match op {
        And => CVal::Bool(x && y),
        Or => CVal::Bool(x || y),
        Eq => CVal::Bool(x == y),
        Ne => CVal::Bool(x != y),
        _ => return None,
    })
}

fn lit(v: CVal, span: Span) -> Expr {
    match v {
        CVal::Int(i) => Expr::Int(i as i128, span),
        CVal::Float(f) => Expr::Float(f, span),
        CVal::Bool(b) => Expr::Bool(b, span),
    }
}

/// Scope-aware AST walker performing const substitution and comptime folding.
struct Walker<'a> {
    consts: &'a HashMap<String, CVal>,
    funcs: &'a HashMap<String, FnDef>,
    errs: &'a mut Vec<String>,
    /// Stack of lexically bound names (params, lets, loop/lambda/match binders) —
    /// a binding shadows a const of the same name.
    scopes: Vec<HashSet<String>>,
}

impl Walker<'_> {
    fn bound(&self, n: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(n))
    }
    fn bind(&mut self, n: &str) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(n.to_string());
        }
    }

    fn fn_def(&mut self, f: &mut FnDef) {
        self.scopes.push(HashSet::new());
        for p in &f.sig.params {
            self.bind(&p.name);
        }
        if let Some(b) = &mut f.body {
            self.block(b);
        }
        self.scopes.pop();
    }

    fn block(&mut self, b: &mut Block) {
        self.scopes.push(HashSet::new());
        for s in &mut b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &mut b.tail {
            self.expr(t);
        }
        self.scopes.pop();
    }

    fn stmt(&mut self, s: &mut Stmt) {
        match s {
            Stmt::Let { name, value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
                self.bind(name); // binds AFTER its initializer
            }
            Stmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return(e, _) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::For { pat, iter, body, .. } => {
                self.expr(iter);
                self.scopes.push(HashSet::new());
                bind_pattern(pat, self);
                self.block(body);
                self.scopes.pop();
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        // Const-name substitution (unless shadowed).
        if let Expr::Ident(n, span) = e {
            if !self.bound(n) {
                if let Some(v) = self.consts.get(n) {
                    *e = lit(*v, *span);
                }
            }
            return;
        }

        // Recurse into children first (so nested consts/comptime resolve).
        self.walk_children(e);

        // Fold a comptime node once its interior is resolved.
        if matches!(e, Expr::Comptime { .. }) {
            let mut ip = Interp::new(self.consts, self.funcs);
            let repl = if let Expr::Comptime { inner, span } = &*e {
                let span = *span;
                // `comptime if`: select the taken branch as a whole block (keeping
                // runtime statements), rather than evaluating it to a scalar.
                if let Expr::If { cond, then, elifs, els, .. } = inner.as_ref() {
                    fold_comptime_if(cond, then, elifs, els, &mut ip)
                } else if let Expr::Call { callee, args, .. } = inner.as_ref() {
                    // `comptime assert(cond[, "message"])` — a compile-time check: the
                    // condition is evaluated now; a false/zero result is a compile error.
                    // Folds to a no-op literal (zero runtime cost) either way.
                    if matches!(callee.as_ref(), Expr::Ident(n, _) if n == "assert") {
                        match args.first().and_then(|a| ip.eval(a)) {
                            Some(CVal::Bool(true)) => {}
                            Some(CVal::Int(i)) if i != 0 => {}
                            Some(CVal::Bool(false)) | Some(CVal::Int(_)) => {
                                let msg = args
                                    .get(1)
                                    .and_then(|a| if let Expr::Str(s, _) = a { Some(s.as_str()) } else { None })
                                    .unwrap_or("comptime assertion failed");
                                self.errs.push(format!("comptime assert failed: {msg}"));
                            }
                            Some(CVal::Float(_)) => {
                                self.errs.push("comptime assert: condition must be Bool/Int, not Float".into());
                            }
                            None => {
                                self.errs.push("comptime assert: condition is not a compile-time constant".into());
                            }
                        }
                        Some(Expr::Bool(true, span))
                    } else {
                        ip.eval(inner).map(|v| lit(v, span))
                    }
                } else {
                    ip.eval(inner).map(|v| lit(v, span))
                }
            } else {
                None
            };
            if let Some(e2) = ip.overflow.take() {
                self.errs.push(e2);
            }
            if let Some(r) = repl {
                *e = r;
            }
        }
    }

    /// Recurse into an expression's sub-expressions/blocks, tracking scopes for
    /// the binding forms (lambda, comprehension, match arms, capsule).
    fn walk_children(&mut self, e: &mut Expr) {
        match e {
            Expr::Int(..)
            | Expr::Float(..)
            | Expr::Str(..)
            | Expr::Char(..)
            | Expr::Bool(..)
            | Expr::Ident(..)
            | Expr::SelfExpr(..) => {}
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
            Expr::TurboCall { targs, args, .. } => {
                for t in targs {
                    self.expr(t);
                }
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
                for (ec, eb) in elifs {
                    self.expr(ec);
                    self.block(eb);
                }
                if let Some(b) = els {
                    self.block(b);
                }
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.expr(scrutinee);
                for (pat, guard, body) in arms {
                    self.scopes.push(HashSet::new());
                    bind_pattern(pat, self);
                    if let Some(g) = guard {
                        self.expr(g);
                    }
                    self.expr(body);
                    self.scopes.pop();
                }
            }
            Expr::Block(b) => self.block(b),
            Expr::Lambda { params, body, .. } => {
                self.scopes.push(HashSet::new());
                for p in params.iter() {
                    self.bind(p);
                }
                self.expr(body);
                self.scopes.pop();
            }
            Expr::List(xs, _) => {
                for x in xs {
                    self.expr(x);
                }
            }
            Expr::Comprehension { elem, var, iter, cond, .. } => {
                self.expr(iter);
                self.scopes.push(HashSet::new());
                self.bind(var);
                if let Some(c) = cond {
                    self.expr(c);
                }
                self.expr(elem);
                self.scopes.pop();
            }
            Expr::MapLit(pairs, _) => {
                for (k, v) in pairs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => self.expr(inner),
            Expr::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }
            Expr::Capsule { inputs, body, .. } => {
                self.scopes.push(HashSet::new());
                for (n, _) in inputs.iter() {
                    self.bind(n);
                }
                self.block(body);
                self.scopes.pop();
            }
            Expr::Spawn { call, .. } => self.expr(call),
        }
    }
}

/// `comptime if`: select the taken branch when the condition is a compile-time
/// bool, returning it as a block expression (the untaken branches are dropped and
/// never lowered). `None` if the condition is not a compile-time constant → the
/// node is left for lowering.
fn fold_comptime_if(
    cond: &Expr,
    then: &Block,
    elifs: &[(Expr, Block)],
    els: &Option<Block>,
    ip: &mut Interp,
) -> Option<Expr> {
    match ip.eval(cond) {
        Some(CVal::Bool(true)) => Some(Expr::Block(then.clone())),
        Some(CVal::Bool(false)) => {
            for (ec, eb) in elifs {
                match ip.eval(ec) {
                    Some(CVal::Bool(true)) => return Some(Expr::Block(eb.clone())),
                    Some(CVal::Bool(false)) => continue,
                    _ => return None, // an elif condition is not constant → defer
                }
            }
            match els {
                Some(b) => Some(Expr::Block(b.clone())),
                // No branch taken and no else → a unit (empty block), matching the
                // lowering behavior (`comptime if false {…}` with no else is Void).
                None => Some(Expr::Block(Block { stmts: vec![], tail: None, span: then.span })),
            }
        }
        _ => None,
    }
}

/// Add a pattern's bound names to the current scope (so they shadow consts).
fn bind_pattern(p: &Pattern, w: &mut Walker) {
    match p {
        Pattern::Bind(n, _) => w.bind(n),
        Pattern::Ctor { args, .. } | Pattern::Tuple(args, _) | Pattern::Or(args, _) => {
            for a in args {
                bind_pattern(a, w);
            }
        }
        Pattern::Wildcard(_) | Pattern::Int(..) | Pattern::Str(..) | Pattern::Bool(..) => {}
    }
}
