//! Shallow Self-Recursive Inlining (AST→AST, VOR der Typinferenz).
//!
//! Kleine, tail-förmige, selbst-rekursive Funktionen werden 1–2 Ebenen in sich
//! selbst inline-expandiert: jeder Selbstaufruf `f(args)` im Rumpf wird durch den
//! (param-substituierten, hygienisierten) Rumpf ersetzt. Der übrige Rekursions-
//! Boden bleibt erhalten (Terminierung unverändert).
//!
//! Zwei Effekte (siehe sprache/REKURSION-INLINING.md):
//!  1. **Call-Overhead-Halbierung** — jeder inline-expandierte Frame berechnet
//!     mehrere Ebenen ohne echten `call`. Gilt für JEDE Rekursion.
//!  2. **Branching-Reduktion** — legt überlappende Teilaufrufe frei (fib(n-1) und
//!     fib(n-2) rufen BEIDE fib(n-3)); LLVM-CSE mergt die identischen reinen Calls
//!     → der Branching-Faktor sinkt. Das ist der große Gewinn (fib 0,08→0,005 s),
//!     GRATIS, weil LLVM reine Funktionen als `readnone` führt.
//!
//! Das ersetzt genau g++s flaches Rekursions-Inlining (das LLVM per Default NICHT
//! macht) und übertrifft es via CSE.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::expand::{collect_binders_block, subst_block};

/// Wie viele Ebenen selbst-inline. 2 = jeder Frame deckt 3 Rekursionsebenen ab
/// (Rumpf ×(#Selbstaufrufe)^2 Größe — nur für KLEINE Funktionen, s. MAX_NODES).
const DEPTH: u32 = 2;
/// Größenobergrenze (AST-Knoten) einer Kandidaten-Funktion — gegen Code-Bloat.
const MAX_NODES: usize = 48;

pub fn inline_recursion(m: &mut Module) {
    // Kandidaten: kleine, self-rekursive, tail-förmige (kein `return`) Funktionen.
    let mut cands: HashMap<String, (Vec<String>, Block)> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if let Some(body) = &f.body {
                let params: Vec<String> = f.sig.params.iter().map(|p| p.name.clone()).collect();
                if is_self_recursive(body, &f.sig.name) && !block_has_return(body) && node_count_block(body) <= MAX_NODES {
                    cands.insert(f.sig.name.clone(), (params, body.clone()));
                }
            }
        }
    }
    if cands.is_empty() {
        return;
    }
    let mut counter: u32 = 0;
    for it in &mut m.items {
        if let Item::Fn(f) = it {
            if let Some((params, orig)) = cands.get(&f.sig.name) {
                let body = f.body.as_mut().unwrap();
                for _ in 0..DEPTH {
                    inline_calls_block(body, &f.sig.name, params, orig, &mut counter);
                }
            }
        }
    }
}

// --- Kandidaten-Prüfung -----------------------------------------------------

fn is_self_recursive(b: &Block, name: &str) -> bool {
    let mut found = false;
    visit_calls_block(b, &mut |n| {
        if n == name {
            found = true;
        }
    });
    found
}

fn visit_calls_block(b: &Block, f: &mut impl FnMut(&str)) {
    for s in &b.stmts {
        visit_calls_stmt(s, f);
    }
    if let Some(t) = &b.tail {
        visit_calls_expr(t, f);
    }
}
fn visit_calls_stmt(s: &Stmt, f: &mut impl FnMut(&str)) {
    match s {
        Stmt::Let { value: Some(e), .. } | Stmt::Expr(e) | Stmt::Return(Some(e), _) => visit_calls_expr(e, f),
        Stmt::Assign { target, value, .. } => {
            visit_calls_expr(target, f);
            visit_calls_expr(value, f);
        }
        Stmt::While { cond, body, .. } => {
            visit_calls_expr(cond, f);
            visit_calls_block(body, f);
        }
        Stmt::For { iter, body, .. } => {
            visit_calls_expr(iter, f);
            visit_calls_block(body, f);
        }
        _ => {}
    }
}
fn visit_calls_expr(e: &Expr, f: &mut impl FnMut(&str)) {
    if let Expr::Call { callee, args, .. } = e {
        if let Expr::Ident(n, _) = callee.as_ref() {
            f(n);
        }
        visit_calls_expr(callee, f);
        args.iter().for_each(|a| visit_calls_expr(a, f));
        return;
    }
    for_each_subexpr(e, &mut |s| visit_calls_expr(s, f));
    for_each_subblock(e, &mut |b| visit_calls_block(b, f));
}

fn block_has_return(b: &Block) -> bool {
    b.stmts.iter().any(stmt_has_return) || b.tail.as_deref().map(expr_has_return).unwrap_or(false)
}
fn stmt_has_return(s: &Stmt) -> bool {
    match s {
        Stmt::Return(..) => true,
        Stmt::While { body, .. } | Stmt::For { body, .. } => block_has_return(body),
        Stmt::Let { value: Some(e), .. } | Stmt::Expr(e) => expr_has_return(e),
        Stmt::Assign { target, value, .. } => expr_has_return(target) || expr_has_return(value),
        _ => false,
    }
}
fn expr_has_return(e: &Expr) -> bool {
    let mut r = false;
    for_each_subexpr(e, &mut |s| r |= expr_has_return(s));
    for_each_subblock(e, &mut |b| r |= block_has_return(b));
    r
}

fn node_count_block(b: &Block) -> usize {
    let mut n = b.stmts.len() + 1;
    for s in &b.stmts {
        if let Stmt::Let { value: Some(e), .. } | Stmt::Expr(e) | Stmt::Return(Some(e), _) = s {
            n += node_count_expr(e);
        }
    }
    if let Some(t) = &b.tail {
        n += node_count_expr(t);
    }
    n
}
fn node_count_expr(e: &Expr) -> usize {
    let mut n = 1;
    for_each_subexpr(e, &mut |s| n += node_count_expr(s));
    for_each_subblock(e, &mut |b| n += node_count_block(b));
    n
}

/// „Rein genug", um als Argument dupliziert/direkt substituiert zu werden
/// (keine Aufrufe/Seiteneffekte). Nur so bleibt der Selbstaufruf sicher inline-bar.
fn is_pure_arg(e: &Expr) -> bool {
    match e {
        Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Char(..) | Expr::Ident(..) | Expr::SelfExpr(..) => true,
        Expr::Unary { rhs, .. } => is_pure_arg(rhs),
        Expr::Binary { lhs, rhs, .. } => is_pure_arg(lhs) && is_pure_arg(rhs),
        Expr::Cast { inner, .. } => is_pure_arg(inner),
        _ => false,
    }
}

// --- Inline-Ersetzung (eine Ebene) ------------------------------------------

fn inline_calls_block(b: &mut Block, name: &str, params: &[String], orig: &Block, counter: &mut u32) {
    for s in &mut b.stmts {
        inline_calls_stmt(s, name, params, orig, counter);
    }
    if let Some(t) = &mut b.tail {
        inline_calls_expr(t, name, params, orig, counter);
    }
}
fn inline_calls_stmt(s: &mut Stmt, name: &str, params: &[String], orig: &Block, counter: &mut u32) {
    match s {
        Stmt::Let { value: Some(e), .. } | Stmt::Expr(e) | Stmt::Return(Some(e), _) => inline_calls_expr(e, name, params, orig, counter),
        Stmt::Assign { target, value, .. } => {
            inline_calls_expr(target, name, params, orig, counter);
            inline_calls_expr(value, name, params, orig, counter);
        }
        Stmt::While { cond, body, .. } => {
            inline_calls_expr(cond, name, params, orig, counter);
            inline_calls_block(body, name, params, orig, counter);
        }
        Stmt::For { iter, body, .. } => {
            inline_calls_expr(iter, name, params, orig, counter);
            inline_calls_block(body, name, params, orig, counter);
        }
        _ => {}
    }
}
/// Kinder ZUERST (eine Ebene pro Durchlauf — den frisch eingesetzten Rumpf NICHT
/// erneut betreten), dann ggf. diesen Selbstaufruf ersetzen.
fn inline_calls_expr(e: &mut Expr, name: &str, params: &[String], orig: &Block, counter: &mut u32) {
    for_each_subexpr_mut(e, &mut |s| inline_calls_expr(s, name, params, orig, counter));
    for_each_subblock_mut(e, &mut |b| inline_calls_block(b, name, params, orig, counter));
    let repl = if let Expr::Call { callee, args, .. } = e {
        if matches!(callee.as_ref(), Expr::Ident(n, _) if n == name) && args.len() == params.len() && args.iter().all(is_pure_arg) {
            // pmap: param → Argument-Ausdruck (direkt → LLVM sieht identische reine
            // Subaufrufe und CSE-t sie). rename: Rumpf-lokale Binder gensym.
            let pmap: HashMap<String, Expr> = params.iter().cloned().zip(args.iter().cloned()).collect();
            let id = *counter;
            *counter += 1;
            let mut locals = HashSet::new();
            collect_binders_block(orig, &mut locals);
            let rename: HashMap<String, String> = locals.iter().map(|l| (l.clone(), format!("{l}$ri{id}"))).collect();
            let mut b = orig.clone();
            subst_block(&mut b, &pmap, &rename);
            Some(Expr::Block(b))
        } else {
            None
        }
    } else {
        None
    };
    if let Some(new_e) = repl {
        *e = new_e;
    }
}

// --- generische Kind-Traversierung ------------------------------------------

fn for_each_subexpr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    match e {
        Expr::Unary { rhs, .. } => f(rhs),
        Expr::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        Expr::Call { callee, args, .. } => {
            f(callee);
            args.iter().for_each(f);
        }
        Expr::Field { base, .. } => f(base),
        Expr::Index { base, index, .. } => {
            f(base);
            f(index);
        }
        Expr::If { cond, .. } => f(cond),
        Expr::Match { scrutinee, arms, .. } => {
            f(scrutinee);
            for (_, g, b) in arms {
                if let Some(g) = g {
                    f(g);
                }
                f(b);
            }
        }
        Expr::Lambda { body, .. } => f(body),
        Expr::List(xs, _) => xs.iter().for_each(f),
        Expr::Comprehension { elem, iter, cond, .. } => {
            f(elem);
            f(iter);
            if let Some(c) = cond {
                f(c);
            }
        }
        Expr::MapLit(kvs, _) => kvs.iter().for_each(|(k, v)| {
            f(k);
            f(v);
        }),
        Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => f(inner),
        Expr::Range { start, end, .. } => {
            f(start);
            f(end);
        }
        _ => {}
    }
}
fn for_each_subblock(e: &Expr, f: &mut impl FnMut(&Block)) {
    match e {
        Expr::If { then, elifs, els, .. } => {
            f(then);
            for (_, b) in elifs {
                f(b);
            }
            if let Some(b) = els {
                f(b);
            }
        }
        Expr::Block(b) | Expr::Capsule { body: b, .. } => f(b),
        _ => {}
    }
}
fn for_each_subexpr_mut(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    match e {
        Expr::Unary { rhs, .. } => f(rhs),
        Expr::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        Expr::Call { callee, args, .. } => {
            f(callee);
            args.iter_mut().for_each(f);
        }
        Expr::Field { base, .. } => f(base),
        Expr::Index { base, index, .. } => {
            f(base);
            f(index);
        }
        Expr::If { cond, .. } => f(cond),
        Expr::Match { scrutinee, arms, .. } => {
            f(scrutinee);
            for (_, g, b) in arms {
                if let Some(g) = g {
                    f(g);
                }
                f(b);
            }
        }
        Expr::Lambda { body, .. } => f(body),
        Expr::List(xs, _) => xs.iter_mut().for_each(f),
        Expr::Comprehension { elem, iter, cond, .. } => {
            f(elem);
            f(iter);
            if let Some(c) = cond {
                f(c);
            }
        }
        Expr::MapLit(kvs, _) => kvs.iter_mut().for_each(|(k, v)| {
            f(k);
            f(v);
        }),
        Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => f(inner),
        Expr::Range { start, end, .. } => {
            f(start);
            f(end);
        }
        _ => {}
    }
}
fn for_each_subblock_mut(e: &mut Expr, f: &mut impl FnMut(&mut Block)) {
    match e {
        Expr::If { then, elifs, els, .. } => {
            f(then);
            for (_, b) in elifs {
                f(b);
            }
            if let Some(b) = els {
                f(b);
            }
        }
        Expr::Block(b) | Expr::Capsule { body: b, .. } => f(b),
        _ => {}
    }
}
