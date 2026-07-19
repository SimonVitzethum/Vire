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

    // 1. Build the const environment in source order (forward references to an
    //    earlier const resolve; a later one is not yet visible).
    let mut env: HashMap<String, CVal> = HashMap::new();
    for it in &m.items {
        if let Item::Const { name, value, .. } = it {
            match ceval(value, &env) {
                Some(v) => {
                    env.insert(name.clone(), v);
                }
                None => errs.push(format!("const `{name}`: initializer is not a compile-time constant")),
            }
        }
    }

    // 2. Rewrite every function/method body.
    let mut w = Walker { consts: &env, scopes: Vec::new() };
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

/// Constant-fold an expression against the const environment. Handles literals,
/// const-name lookup, unary/binary arithmetic/comparison/logic, and `comptime`.
/// Anything else → `None` (not compile-time constant).
fn ceval(e: &Expr, env: &HashMap<String, CVal>) -> Option<CVal> {
    match e {
        Expr::Int(v, _) => Some(CVal::Int(*v as i64)),
        Expr::Float(v, _) => Some(CVal::Float(*v)),
        Expr::Bool(v, _) => Some(CVal::Bool(*v)),
        Expr::Ident(n, _) => env.get(n).copied(),
        Expr::Comptime { inner, .. } => ceval(inner, env),
        Expr::Unary { op, rhs, .. } => {
            let r = ceval(rhs, env)?;
            match (op, r) {
                (UnOp::Neg, CVal::Int(i)) => Some(CVal::Int(i.wrapping_neg())),
                (UnOp::Neg, CVal::Float(f)) => Some(CVal::Float(-f)),
                (UnOp::Not, CVal::Bool(b)) => Some(CVal::Bool(!b)),
                _ => None,
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            let a = ceval(lhs, env)?;
            let b = ceval(rhs, env)?;
            match (a, b) {
                (CVal::Int(x), CVal::Int(y)) => int_binop(*op, x, y),
                (CVal::Float(x), CVal::Float(y)) => float_binop(*op, x, y),
                (CVal::Bool(x), CVal::Bool(y)) => bool_binop(*op, x, y),
                _ => None,
            }
        }
        _ => None,
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
            let repl = if let Expr::Comptime { inner, span } = &*e {
                let span = *span;
                if let Expr::If { cond, then, elifs, els, .. } = inner.as_ref() {
                    fold_comptime_if(cond, then, elifs, els, self.consts)
                } else {
                    ceval(inner, self.consts).map(|v| lit(v, span))
                }
            } else {
                None
            };
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
    env: &HashMap<String, CVal>,
) -> Option<Expr> {
    match ceval(cond, env) {
        Some(CVal::Bool(true)) => Some(Expr::Block(then.clone())),
        Some(CVal::Bool(false)) => {
            for (ec, eb) in elifs {
                match ceval(ec, env) {
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
