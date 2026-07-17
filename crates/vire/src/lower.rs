//! Absenkung Vire-AST → `crates/ir` (SSA-nah, keine Slot-Wiederverwendung).
//! Deckt den M2-Kern ab: Funktionen, Arithmetik, Kontrollfluss (if/while/
//! for-über-Range), `print`, Aufrufe eigener Funktionen. Generics/Traits/
//! Closures/capsule folgen (FRONTEND-PLAN F5–F8).

use std::collections::HashMap;

use fastllvm_ir::{BasicBlock, BinOp as IB, Block, Function, Local, Operand, Program, Rvalue, Statement, Terminator, Ty};

use crate::ast::*;

pub fn lower_module(m: &Module) -> Result<Program, Vec<String>> {
    let mut prog = Program::default();
    // Signatur-Tabelle (Name → (ParamTypen, RückgabeTyp)) für Aufrufe.
    let mut sigs: HashMap<String, (Vec<Ty>, Ty)> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            let ps = f.sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
            sigs.insert(f.sig.name.clone(), (ps, guess_ret_ty(f)));
        }
    }
    let mut errs = Vec::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            match lower_fn(f, &sigs, &mut prog.strings) {
                Ok(func) => prog.functions.push(func),
                Err(mut e) => errs.append(&mut e),
            }
        }
        // type/trait/impl/const/use/extern: M2+ — hier noch übersprungen
    }
    if errs.is_empty() {
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
        Some("Unit") | None => Ty::I64, // Default-Ganzzahl, wenn nichts steht
        Some(_) => Ty::I64,
    }
}

fn ret_ty(sig: &FnSig) -> Ty {
    match &sig.ret {
        None => Ty::Void,
        Some(t) if t.name == "Unit" => Ty::Void,
        Some(t) => ty_of(Some(t)),
    }
}

/// Rückgabetyp einer Funktion — bis Typinferenz (F5) steht.
/// Mit `-> T`-Annotation: exakt. Ohne: strukturell aus dem Tail-Ausdruck
/// geschätzt (kein Tail → Void). Wird für Aufrufstellen UND die Funktion selbst
/// benutzt, damit beide übereinstimmen.
fn guess_ret_ty(f: &FnDef) -> Ty {
    // `main` ist der Einstieg — immer Void, egal ob die letzte Zeile ein
    // (Void-)Ausdruck wie `print(x)` als Tail geparst wurde.
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

/// Grobe, annotationsfreie Typ-Schätzung eines Ausdrucks (nur Literale/Struktur).
/// Idents/Aufrufe ohne Kontext → I64 (Default-Ganzzahl). Ersetzt echte Inferenz.
fn guess_expr_ty(e: &Expr) -> Ty {
    match e {
        // `print(...)` gibt Void zurück (Intrinsic).
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
    blocks: Vec<BasicBlock>,
    cur: usize,
    scopes: Vec<HashMap<String, (Local, Ty)>>,
    sigs: &'a HashMap<String, (Vec<Ty>, Ty)>,
    /// Gemeinsamer String-Literal-Pool (Program::strings); `intern` gibt Indizes.
    strings: &'a mut Vec<String>,
    errs: Vec<String>,
    /// Ziel-Blöcke der umgebenden Schleifen: (continue → header, break → exit).
    loops: Vec<(Block, Block)>,
}

impl<'a> FnLower<'a> {
    fn new_local(&mut self, ty: Ty) -> Local {
        self.locals.push(ty);
        Local((self.locals.len() - 1) as u32)
    }
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return i as u32;
        }
        self.strings.push(s.to_string());
        (self.strings.len() - 1) as u32
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
        self.scopes.last_mut().unwrap().insert(name.to_string(), (l, t));
    }

    fn lower_block(&mut self, b: &Block2) {
        let _ = self.lower_block_val(b); // Void-Kontext: Tail-Wert verworfen
    }

    /// Wie `lower_block`, liefert aber den Tail-Wert (für if-/Block-Ausdrücke).
    /// Ohne Tail → (_, Void).
    fn lower_block_val(&mut self, b: &Block2) -> (Operand, Ty) {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            self.lower_stmt(s);
        }
        let v = match &b.tail {
            Some(t) => self.lower_expr(t),
            None => (Operand::ConstI64(0), Ty::Void),
        };
        self.scopes.pop();
        v
    }

    fn lower_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let { mutable, name, value, .. } => {
                // Binding-vs-Zuweisung (F3-Ersatz bis Resolve steht): `mut x = …`
                // bindet immer neu; ein schlichtes `x = …` auf einen bereits
                // sichtbaren Namen ist eine Zuweisung, kein Shadowing.
                if !mutable {
                    if let Some((l, _)) = self.lookup(name) {
                        let (op, _) = match value {
                            Some(v) => self.lower_expr(v),
                            None => (Operand::ConstI64(0), Ty::I64),
                        };
                        self.emit(Statement::Assign(l, Rvalue::Use(op)));
                        return;
                    }
                }
                let (op, ty) = match value {
                    Some(v) => self.lower_expr(v),
                    None => (Operand::ConstI64(0), Ty::I64),
                };
                let l = self.new_local(ty);
                self.emit(Statement::Assign(l, Rvalue::Use(op)));
                self.bind(name, l, ty);
            }
            Stmt::Assign { target, op, value, .. } => {
                if let Expr::Ident(name, _) = target {
                    if let Some((l, _ty)) = self.lookup(name) {
                        let (rhs, _) = self.lower_expr(value);
                        let rv = match op {
                            None => Rvalue::Use(rhs),
                            Some(o) => Rvalue::Binary(map_op(*o), Operand::Copy(l), rhs),
                        };
                        self.emit(Statement::Assign(l, rv));
                    } else {
                        self.errs.push(format!("unbekannte Variable: {name}"));
                    }
                } else {
                    self.errs.push("Zuweisungsziel M2: nur einfache Variablen".into());
                }
            }
            Stmt::Expr(e) => {
                self.lower_expr(e);
            }
            Stmt::Return(e, _) => {
                let t = match e {
                    Some(e) => {
                        let (op, _) = self.lower_expr(e);
                        Terminator::Return(Some(op))
                    }
                    None => Terminator::Return(None),
                };
                let cur = self.cur;
                self.term(cur, t);
                // Rest wird ein neuer (unerreichbarer) Block
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
                    None => self.errs.push("`break` außerhalb einer Schleife".into()),
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
                    None => self.errs.push("`continue` außerhalb einer Schleife".into()),
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
                self.loops.push((header, exit));
                self.lower_block(body);
                self.loops.pop();
                let end = self.cur;
                self.term(end, Terminator::Goto(header));
                self.cur = exit.0 as usize;
            }
            Stmt::For { pat, iter, body, .. } => {
                // Nur `for i in a..b` (Range) in M2.
                let name = match pat {
                    Pattern::Bind(n, _) => n.clone(),
                    Pattern::Wildcard(_) => "_".into(),
                    _ => {
                        self.errs.push("for-Muster M2: nur `for i in a..b`".into());
                        return;
                    }
                };
                let (start, end_op, incl) = match iter {
                    Expr::Range { start, end, inclusive, .. } => {
                        let (s, _) = self.lower_expr(start);
                        let (e, _) = self.lower_expr(end);
                        (s, e, *inclusive)
                    }
                    _ => {
                        self.errs.push("for-Iterator M2: nur Range `a..b`".into());
                        return;
                    }
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
                let latch = self.new_block(); // Inkrement-Block: `continue`-Ziel
                let exit = self.new_block();
                self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(cond), then_blk: bodyb, else_blk: exit });
                self.cur = bodyb.0 as usize;
                self.scopes.push(HashMap::new());
                self.bind(&name, ivar, Ty::I64);
                self.loops.push((latch, exit)); // continue → latch (nicht header!), sonst kein Inkrement
                self.lower_block(body);
                self.loops.pop();
                self.scopes.pop();
                let end = self.cur;
                self.term(end, Terminator::Goto(latch));
                self.cur = latch.0 as usize;
                self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                self.term(latch.0 as usize, Terminator::Goto(header));
                self.cur = exit.0 as usize;
            }
        }
    }

    /// Liefert (Operand, Typ). Emittiert bei Bedarf Temporäre.
    fn lower_expr(&mut self, e: &Expr) -> (Operand, Ty) {
        match e {
            Expr::Int(v, _) => (Operand::ConstI64(*v as i64), Ty::I64),
            Expr::Float(v, _) => (Operand::ConstF64(*v), Ty::F64),
            Expr::Bool(b, _) => (Operand::ConstI32(if *b { 1 } else { 0 }), Ty::I32),
            Expr::Str(s, _) => {
                let id = self.intern(s);
                (Operand::ConstStr(id), Ty::Ref)
            }
            Expr::Ident(name, _) => match self.lookup(name) {
                Some((l, ty)) => (Operand::Copy(l), ty),
                None => {
                    self.errs.push(format!("unbekannte Variable: {name}"));
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
            Expr::Binary { op, lhs, rhs, .. } => {
                let (l, lt) = self.lower_expr(lhs);
                let (r, _rt) = self.lower_expr(rhs);
                let is_cmp = matches!(
                    op,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                );
                let ty = if is_cmp { Ty::I32 } else { lt };
                let d = self.new_local(ty);
                self.emit(Statement::Assign(d, Rvalue::Binary(map_op(*op), l, r)));
                (Operand::Copy(d), ty)
            }
            Expr::Call { callee, args, .. } => self.lower_call(callee, args),
            Expr::If { cond, then, elifs, els, .. } => self.lower_if(cond, then, elifs, els),
            Expr::Block(b) => self.lower_block_val(b),
            Expr::Range { .. } => {
                self.errs.push("Range nur als for-Iterator (M2)".into());
                (Operand::ConstI64(0), Ty::I64)
            }
            other => {
                self.errs.push(format!("Ausdruck M2 noch nicht abgesenkt: {}", expr_kind(other)));
                (Operand::ConstI64(0), Ty::I64)
            }
        }
    }

    fn lower_call(&mut self, callee: &Expr, args: &[Expr]) -> (Operand, Ty) {
        let name = match callee {
            Expr::Ident(n, _) => n.clone(),
            _ => {
                self.errs.push("Aufruf-Ziel M2: nur benannte Funktionen".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        let lowered: Vec<(Operand, Ty)> = args.iter().map(|a| self.lower_expr(a)).collect();
        // Intrinsic `print`
        if name == "print" {
            let (op, ty) = lowered.into_iter().next().unwrap_or((Operand::ConstI64(0), Ty::I64));
            let func = match ty {
                Ty::F64 | Ty::F32 => "jrt_println_double",
                Ty::Ref => "jrt_println_str",
                _ => "jrt_println_long",
            };
            let arg = if matches!(ty, Ty::F64 | Ty::F32 | Ty::Ref) { op } else { to_i64(op) };
            self.emit(Statement::Call { dest: None, func: func.to_string(), args: vec![arg] });
            return (Operand::ConstI64(0), Ty::Void);
        }
        // Aufruf einer eigenen Funktion
        let (_, ret) = self.sigs.get(&name).cloned().unwrap_or((vec![], Ty::I64));
        let arg_ops: Vec<Operand> = lowered.into_iter().map(|(o, _)| o).collect();
        if ret == Ty::Void {
            self.emit(Statement::Call { dest: None, func: name, args: arg_ops });
            (Operand::ConstI64(0), Ty::Void)
        } else {
            let d = self.new_local(ret);
            self.emit(Statement::Call { dest: Some(d), func: name, args: arg_ops });
            (Operand::Copy(d), ret)
        }
    }

    fn lower_if(&mut self, cond: &Expr, then: &Block2, elifs: &[(Expr, Block2)], els: &Option<Block2>) -> (Operand, Ty) {
        let (c, _) = self.lower_expr(cond);
        let thenb = self.new_block();
        let elseb = self.new_block();
        let merge = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Branch { cond: c, then_blk: thenb, else_blk: elseb });
        // then-Zweig → Wert + Endblock (noch nicht terminiert).
        self.cur = thenb.0 as usize;
        let (tv, tty) = self.lower_block_val(then);
        let te = self.cur;
        // else-Zweig: weitere `elif`s rekursiv, sonst `else`-Block, sonst kein Wert.
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
        // Ergebnistyp: der nicht-Void-Zweig gewinnt (beide gleich, wenn Wert-if).
        let rty = if tty != Ty::Void { tty } else { ety };
        if rty != Ty::Void {
            // Phi-Ersatz: gemeinsames Ergebnis-Local, in beiden Endblöcken belegt.
            let res = self.new_local(rty);
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

// Der AST nennt Block; hier Alias, um Namenskollision mit ir::Block zu vermeiden.
use crate::ast::Block as Block2;

fn lower_fn(f: &FnDef, sigs: &HashMap<String, (Vec<Ty>, Ty)>, strings: &mut Vec<String>) -> Result<Function, Vec<String>> {
    let ret = guess_ret_ty(f);
    let name = if f.sig.name == "main" { "java_main".to_string() } else { f.sig.name.clone() };
    let mut fl = FnLower {
        locals: Vec::new(),
        blocks: Vec::new(),
        cur: 0,
        scopes: vec![HashMap::new()],
        sigs,
        strings,
        errs: Vec::new(),
        loops: Vec::new(),
    };
    // Block 0
    fl.new_block();
    // Parameter → Locals 0..n
    let mut param_tys = Vec::new();
    for p in &f.sig.params {
        let t = ty_of(p.ty.as_ref());
        param_tys.push(t);
        let l = fl.new_local(t);
        fl.bind(&p.name, l, t);
    }
    if let Some(body) = &f.body {
        // Statements + Tail (Tail = Rückgabewert, falls ret != Void)
        fl.scopes.push(HashMap::new());
        for s in &body.stmts {
            fl.lower_stmt(s);
        }
        let term = if let Some(t) = &body.tail {
            let (op, _) = fl.lower_expr(t);
            if ret == Ty::Void { Terminator::Return(None) } else { Terminator::Return(Some(op)) }
        } else {
            Terminator::Return(None)
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
    Ok(Function {
        name,
        params: param_tys,
        ret,
        locals: fl.locals,
        blocks: fl.blocks,
        receiver_nonnull: false,
    })
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
    }
}

/// Für Vergleiche/Range mit gemischten Const-Breiten: i32-Konstanten als i64 nutzen.
fn to_i64(op: Operand) -> Operand {
    match op {
        Operand::ConstI32(v) => Operand::ConstI64(v as i64),
        other => other,
    }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Str(..) => "Str", Expr::Char(..) => "Char", Expr::SelfExpr(..) => "self",
        Expr::Field { .. } => "Feldzugriff", Expr::Index { .. } => "Index",
        Expr::Match { .. } => "match", Expr::Lambda { .. } => "Lambda",
        Expr::List(..) => "Liste", Expr::Try { .. } => "?", Expr::Cast { .. } => "as",
        Expr::Comptime { .. } => "comptime", Expr::Capsule { .. } => "capsule",
        _ => "Ausdruck",
    }
}
