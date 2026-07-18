//! Hygienische Ausdrucks-Makros: AST→AST-Expansion VOR der Typinferenz.
//!
//! `macro name(p, …) = <expr>` wird an jeder Aufrufstelle `name(args)` durch den
//! Rumpf ersetzt — Parameter werden durch die Argument-Teilbäume substituiert,
//! makro-lokale Bindungen (`mut`/Lambda-Parameter/`for`-Variablen/Muster-Binder)
//! werden pro Expansion gensym-umbenannt. Dadurch:
//!   * kann eine makro-eingeführte Bindung KEINEN Bezeichner eines Arguments
//!     einfangen (Hygiene nach unten), und
//!   * behalten Argument-Ausdrücke ihre Bedeutung an der Aufrufstelle (Hygiene
//!     nach oben — sie werden unverändert eingesetzt).
//!
//! Bewusst begrenzt (ehrlich): ein makro-lokaler Name wird im ganzen Rumpf
//! konsistent umbenannt (nicht per disjunktem Scope); Makros expandieren zu
//! Ausdrücken (kein item-erzeugendes Makro); Rekursions-/Tiefenlimit gegen
//! divergierende Makros.

use std::collections::{HashMap, HashSet};

use crate::ast::*;

const DEPTH_LIMIT: u32 = 64;

struct Expander {
    macros: HashMap<String, (Vec<String>, Expr)>,
    counter: u32,
    depth: u32,
    errs: Vec<String>,
}

/// Setzt alle Makros im Modul ein und entfernt die Makro-Definitionen. Fehler
/// (Aritätskonflikt, Rekursionslimit) werden gesammelt zurückgegeben.
pub fn expand_macros(m: &mut Module) -> Result<(), Vec<String>> {
    let mut macros: HashMap<String, (Vec<String>, Expr)> = HashMap::new();
    for it in &m.items {
        if let Item::Macro { name, params, body, .. } = it {
            macros.insert(name.clone(), (params.clone(), body.clone()));
        }
    }
    if macros.is_empty() {
        return Ok(());
    }
    let mut ex = Expander { macros, counter: 0, depth: 0, errs: Vec::new() };
    for it in &mut m.items {
        match it {
            Item::Fn(f) => {
                if let Some(b) = &mut f.body {
                    ex.block(b);
                }
            }
            Item::Type(t) => {
                for meth in &mut t.methods {
                    if let Some(b) = &mut meth.body {
                        ex.block(b);
                    }
                }
            }
            Item::Impl(i) => {
                for meth in &mut i.methods {
                    if let Some(b) = &mut meth.body {
                        ex.block(b);
                    }
                }
            }
            Item::Const { value, .. } => ex.expr(value),
            _ => {}
        }
    }
    m.items.retain(|it| !matches!(it, Item::Macro { .. }));
    if ex.errs.is_empty() {
        Ok(())
    } else {
        Err(ex.errs)
    }
}

impl Expander {
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
            Stmt::Let { value, .. } => {
                if let Some(e) = value {
                    self.expr(e);
                }
            }
            Stmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return(v, _) => {
                if let Some(e) = v {
                    self.expr(e);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::For { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    /// Kinder zuerst expandieren (Argumente makro-frei machen), dann diese Stelle:
    /// ist sie ein Makro-Aufruf, durch den (hygienisierten) Rumpf ersetzen und das
    /// Ergebnis erneut expandieren (verschachtelte/rumpf-interne Makros).
    fn expr(&mut self, e: &mut Expr) {
        self.expr_children(e);
        let repl = if let Expr::Call { callee, args, span } = e {
            if let Expr::Ident(n, _) = callee.as_ref() {
                if let Some((params, body)) = self.macros.get(n).cloned() {
                    if params.len() != args.len() {
                        self.errs.push(format!("Makro `{n}`: {} Parameter erwartet, {} übergeben", params.len(), args.len()));
                        None
                    } else {
                        Some(self.instantiate(&params, args, &body, *span))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(mut new_e) = repl {
            if self.depth < DEPTH_LIMIT {
                self.depth += 1;
                self.expr(&mut new_e);
                self.depth -= 1;
            } else {
                self.errs.push("Makro-Expansion: Rekursionslimit erreicht (divergierendes Makro?)".into());
            }
            *e = new_e;
        }
    }

    fn expr_children(&mut self, e: &mut Expr) {
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
            Expr::List(xs, _) => {
                for x in xs {
                    self.expr(x);
                }
            }
            Expr::Comprehension { elem, iter, cond, .. } => {
                self.expr(elem);
                self.expr(iter);
                if let Some(c) = cond {
                    self.expr(c);
                }
            }
            Expr::MapLit(kvs, _) => {
                for (k, v) in kvs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => self.expr(inner),
            Expr::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }
            Expr::Capsule { body, .. } => self.block(body),
            _ => {}
        }
    }

    /// Rumpf klonen, makro-lokale Binder gensym-umbenennen, Parameter durch die
    /// Argument-Teilbäume ersetzen.
    fn instantiate(&mut self, params: &[String], args: &[Expr], body: &Expr, _span: crate::diag::Span) -> Expr {
        let mut b = body.clone();
        let id = self.counter;
        self.counter += 1;
        let mut locals: HashSet<String> = HashSet::new();
        collect_binders_expr(&b, &mut locals);
        for p in params {
            locals.remove(p); // Parameter werden substituiert, nicht umbenannt
        }
        let rename: HashMap<String, String> = locals.iter().map(|l| (l.clone(), format!("{l}$h{id}"))).collect();
        let pmap: HashMap<String, Expr> = params.iter().cloned().zip(args.iter().cloned()).collect();
        subst_expr(&mut b, &pmap, &rename);
        b
    }
}

// --- Binder-Sammlung (makro-lokale Namen) -----------------------------------

fn collect_binders_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Unary { rhs, .. } => collect_binders_expr(rhs, out),
        Expr::Binary { lhs, rhs, .. } => {
            collect_binders_expr(lhs, out);
            collect_binders_expr(rhs, out);
        }
        Expr::Call { callee, args, .. } => {
            collect_binders_expr(callee, out);
            args.iter().for_each(|a| collect_binders_expr(a, out));
        }
        Expr::Field { base, .. } => collect_binders_expr(base, out),
        Expr::Index { base, index, .. } => {
            collect_binders_expr(base, out);
            collect_binders_expr(index, out);
        }
        Expr::If { cond, then, elifs, els, .. } => {
            collect_binders_expr(cond, out);
            collect_binders_block(then, out);
            for (c, b) in elifs {
                collect_binders_expr(c, out);
                collect_binders_block(b, out);
            }
            if let Some(b) = els {
                collect_binders_block(b, out);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            collect_binders_expr(scrutinee, out);
            for (p, g, b) in arms {
                collect_binders_pat(p, out);
                if let Some(g) = g {
                    collect_binders_expr(g, out);
                }
                collect_binders_expr(b, out);
            }
        }
        Expr::Block(b) => collect_binders_block(b, out),
        Expr::Lambda { params, body, .. } => {
            params.iter().for_each(|p| {
                out.insert(p.clone());
            });
            collect_binders_expr(body, out);
        }
        Expr::List(xs, _) => xs.iter().for_each(|x| collect_binders_expr(x, out)),
        Expr::Comprehension { elem, var, iter, cond, .. } => {
            out.insert(var.clone());
            collect_binders_expr(elem, out);
            collect_binders_expr(iter, out);
            if let Some(c) = cond {
                collect_binders_expr(c, out);
            }
        }
        Expr::MapLit(kvs, _) => kvs.iter().for_each(|(k, v)| {
            collect_binders_expr(k, out);
            collect_binders_expr(v, out);
        }),
        Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => collect_binders_expr(inner, out),
        Expr::Range { start, end, .. } => {
            collect_binders_expr(start, out);
            collect_binders_expr(end, out);
        }
        Expr::Capsule { body, .. } => collect_binders_block(body, out),
        _ => {}
    }
}

fn collect_binders_block(b: &Block, out: &mut HashSet<String>) {
    for s in &b.stmts {
        match s {
            Stmt::Let { name, value, .. } => {
                out.insert(name.clone());
                if let Some(e) = value {
                    collect_binders_expr(e, out);
                }
            }
            Stmt::Assign { target, value, .. } => {
                collect_binders_expr(target, out);
                collect_binders_expr(value, out);
            }
            Stmt::Expr(e) => collect_binders_expr(e, out),
            Stmt::Return(v, _) => {
                if let Some(e) = v {
                    collect_binders_expr(e, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                collect_binders_expr(cond, out);
                collect_binders_block(body, out);
            }
            Stmt::For { pat, iter, body, .. } => {
                collect_binders_pat(pat, out);
                collect_binders_expr(iter, out);
                collect_binders_block(body, out);
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }
    if let Some(t) = &b.tail {
        collect_binders_expr(t, out);
    }
}

fn collect_binders_pat(p: &Pattern, out: &mut HashSet<String>) {
    match p {
        Pattern::Bind(n, _) => {
            out.insert(n.clone());
        }
        Pattern::Ctor { args, .. } | Pattern::Tuple(args, _) | Pattern::Or(args, _) => args.iter().for_each(|a| collect_binders_pat(a, out)),
        _ => {}
    }
}

// --- Substitution: Parameter → Argument, lokale Binder → frische Namen -------

fn subst_expr(e: &mut Expr, pmap: &HashMap<String, Expr>, rename: &HashMap<String, String>) {
    // Ident: Parameter-Ersetzung (ganzer Knoten) hat Vorrang, sonst Umbenennung.
    if let Expr::Ident(n, _) = e {
        if let Some(arg) = pmap.get(n) {
            *e = arg.clone();
            return;
        }
        if let Some(fresh) = rename.get(n) {
            *n = fresh.clone();
        }
        return;
    }
    match e {
        Expr::Unary { rhs, .. } => subst_expr(rhs, pmap, rename),
        Expr::Binary { lhs, rhs, .. } => {
            subst_expr(lhs, pmap, rename);
            subst_expr(rhs, pmap, rename);
        }
        Expr::Call { callee, args, .. } => {
            subst_expr(callee, pmap, rename);
            args.iter_mut().for_each(|a| subst_expr(a, pmap, rename));
        }
        Expr::Field { base, .. } => subst_expr(base, pmap, rename),
        Expr::Index { base, index, .. } => {
            subst_expr(base, pmap, rename);
            subst_expr(index, pmap, rename);
        }
        Expr::If { cond, then, elifs, els, .. } => {
            subst_expr(cond, pmap, rename);
            subst_block(then, pmap, rename);
            for (c, b) in elifs {
                subst_expr(c, pmap, rename);
                subst_block(b, pmap, rename);
            }
            if let Some(b) = els {
                subst_block(b, pmap, rename);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            subst_expr(scrutinee, pmap, rename);
            for (p, g, b) in arms {
                subst_pat(p, rename);
                if let Some(g) = g {
                    subst_expr(g, pmap, rename);
                }
                subst_expr(b, pmap, rename);
            }
        }
        Expr::Block(b) => subst_block(b, pmap, rename),
        Expr::Lambda { params, body, .. } => {
            for p in params.iter_mut() {
                if let Some(fresh) = rename.get(p) {
                    *p = fresh.clone();
                }
            }
            subst_expr(body, pmap, rename);
        }
        Expr::List(xs, _) => xs.iter_mut().for_each(|x| subst_expr(x, pmap, rename)),
        Expr::Comprehension { elem, var, iter, cond, .. } => {
            if let Some(fresh) = rename.get(var) {
                *var = fresh.clone();
            }
            subst_expr(elem, pmap, rename);
            subst_expr(iter, pmap, rename);
            if let Some(c) = cond {
                subst_expr(c, pmap, rename);
            }
        }
        Expr::MapLit(kvs, _) => kvs.iter_mut().for_each(|(k, v)| {
            subst_expr(k, pmap, rename);
            subst_expr(v, pmap, rename);
        }),
        Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => subst_expr(inner, pmap, rename),
        Expr::Range { start, end, .. } => {
            subst_expr(start, pmap, rename);
            subst_expr(end, pmap, rename);
        }
        Expr::Capsule { body, .. } => subst_block(body, pmap, rename),
        _ => {}
    }
}

fn subst_block(b: &mut Block, pmap: &HashMap<String, Expr>, rename: &HashMap<String, String>) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let { name, value, .. } => {
                if let Some(fresh) = rename.get(name) {
                    *name = fresh.clone();
                }
                if let Some(e) = value {
                    subst_expr(e, pmap, rename);
                }
            }
            Stmt::Assign { target, value, .. } => {
                subst_expr(target, pmap, rename);
                subst_expr(value, pmap, rename);
            }
            Stmt::Expr(e) => subst_expr(e, pmap, rename),
            Stmt::Return(v, _) => {
                if let Some(e) = v {
                    subst_expr(e, pmap, rename);
                }
            }
            Stmt::While { cond, body, .. } => {
                subst_expr(cond, pmap, rename);
                subst_block(body, pmap, rename);
            }
            Stmt::For { pat, iter, body, .. } => {
                subst_pat(pat, rename);
                subst_expr(iter, pmap, rename);
                subst_block(body, pmap, rename);
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }
    if let Some(t) = &mut b.tail {
        subst_expr(t, pmap, rename);
    }
}

fn subst_pat(p: &mut Pattern, rename: &HashMap<String, String>) {
    match p {
        Pattern::Bind(n, _) => {
            if let Some(fresh) = rename.get(n) {
                *n = fresh.clone();
            }
        }
        Pattern::Ctor { args, .. } | Pattern::Tuple(args, _) | Pattern::Or(args, _) => args.iter_mut().for_each(|a| subst_pat(a, rename)),
        _ => {}
    }
}
