//! Leichte, ganzprogrammweite Typinferenz (F5-Kern, monomorph).
//!
//! Zweck bis zur vollen HM/bidirektionalen Inferenz: **un-annotierte
//! Parametertypen ausfüllen**, damit z.B. Float-Funktionen ohne `: Float` korrekt
//! absenken. Arbeitet über Union-Find auf dem skalaren Typgitter
//! (I64/F64/I32/Ref/Void); alles Höhere (Generics/Traits/Referenztypen von
//! Nutzertypen) bleibt späteren Stufen und wird hier konservativ als `Ref`/offen
//! behandelt. Best-effort: bei Konflikten wird nicht abgebrochen, der betroffene
//! Parameter bleibt un-annotiert (die Absenkung defaultet dann zu I64).
//!
//! Ergebnis: `infer_module` **mutiert** den AST und schreibt konkrete Typen in
//! bislang `None`-Parameter. Die Absenkung (`lower`) liest sie unverändert.

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

/// Union-Find über Typvariablen; konkrete Typen sind Blätter.
struct Unifier {
    parent: Vec<T>, // parent[v] für Var(v); nicht-Var = gebundener konkreter Typ
}

impl Unifier {
    fn new() -> Self {
        Unifier { parent: Vec::new() }
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
                return cur; // freie Variable
            }
            cur = p;
        }
        cur
    }
    /// Vereinigt zwei Typen. Bei Konflikt (zwei verschiedene konkrete Typen)
    /// passiert nichts (best-effort) — der Aufrufer verlässt sich nicht darauf.
    fn unify(&mut self, a: T, b: T) {
        let (ra, rb) = (self.resolve(a), self.resolve(b));
        if ra == rb {
            return;
        }
        match (ra, rb) {
            (T::Var(v), other) | (other, T::Var(v)) => {
                self.parent[v as usize] = other;
            }
            _ => { /* Konflikt zweier konkreter Typen: ignorieren */ }
        }
    }
}

/// Globale Signatur einer Funktion: Typvariablen der Parameter + Rückgabe.
struct Sig {
    params: Vec<T>,
    ret: T,
}

pub fn infer_module(m: &mut Module) {
    let mut u = Unifier::new();
    // 1. Globale Signaturvariablen anlegen (annotiert → konkret, sonst frisch).
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
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
    }
    // 2. Rümpfe durchlaufen, Constraints sammeln.
    for it in &m.items {
        if let Item::Fn(f) = it {
            if let Some(body) = &f.body {
                let sig = &sigs[&f.sig.name];
                let mut cx = Ctx {
                    u: &mut u,
                    sigs: &sigs,
                    scopes: vec![HashMap::new()],
                    ret: sig.ret,
                };
                for (p, pv) in f.sig.params.iter().zip(&sig.params) {
                    cx.bind(&p.name, *pv);
                }
                cx.infer_block(body, true);
            }
        }
    }
    // 3. Aufgelöste Parameter- UND Rückgabetypen zurückschreiben (nur bisher
    //    un-annotierte). `main` bleibt außen vor — bleibt in der Absenkung Void.
    let resolved: HashMap<String, (Vec<T>, T)> = sigs
        .iter()
        .map(|(n, s)| (n.clone(), (s.params.iter().map(|t| u.resolve(*t)).collect(), u.resolve(s.ret))))
        .collect();
    for it in m.items.iter_mut() {
        if let Item::Fn(f) = it {
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

struct Ctx<'a> {
    u: &'a mut Unifier,
    sigs: &'a HashMap<String, Sig>,
    scopes: Vec<HashMap<String, T>>,
    ret: T,
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
                        self.u.unify(existing, vt); // Zuweisung, kein neues Binding
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
                // `for i in a..b`: i:I64, Range-Enden I64.
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
                    self.bind(n, T::I64);
                }
                self.infer_block(body, false);
                self.scopes.pop();
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    fn infer_expr(&mut self, e: &Expr) -> T {
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
                self.u.unify(l, r);
                if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                    T::I32
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
                    if let Some(sig) = self.sigs.get(n) {
                        for (at, pt) in arg_ts.iter().zip(&sig.params) {
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
            _ => self.u.fresh(),
        }
    }
}

/// Annotationsname → skalarer Typ (analog `lower::ty_of`, aber im T-Gitter).
/// Unbekannte/Nutzertypen → `Ref` (konservativ). None → None (frei).
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

/// T → Annotationsname für die Rückschrift (nur konkrete skalare Typen).
fn concrete_name(t: T) -> Option<&'static str> {
    match t {
        T::I64 => Some("Int"),
        T::F64 => Some("Float"),
        T::I32 => Some("I32"),
        T::Ref => Some("Str"),
        T::Void => Some("Unit"),
        T::Var(_) => None,
    }
}
