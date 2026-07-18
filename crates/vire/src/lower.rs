//! Absenkung Vire-AST → `crates/ir` (SSA-nah, keine Slot-Wiederverwendung).
//! Deckt den M2-Kern ab: Funktionen, Arithmetik, Kontrollfluss (if/while/
//! for-über-Range), `print`, Aufrufe eigener Funktionen. Generics/Traits/
//! Closures/capsule folgen (FRONTEND-PLAN F5–F8).

use std::collections::HashMap;

use fastllvm_ir::{BasicBlock, BinOp as IB, Block, Function, Local, Operand, Program, Rvalue, Statement, Terminator, Ty};

use crate::ast::*;

/// Feldlayout eines Nutzertyps: (Feldname, IR-Typ, Ref-Ziel-Klasse).
type Layout = Vec<(String, Ty, Option<String>)>;

/// Variante eines Summentyps: (Summentyp-Name, Tag, Felder als (geflachter Name, Typ, Ref-Klasse)).
type VariantInfo = (String, i64, Vec<(String, Ty, Option<String>)>);

/// Alle Methoden (Type-inline + `impl`-Blöcke) als (Klassenname, Methode).
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

/// Aufruf-Signatur einer Funktion: Parametertypen, Rückgabetyp, Rückgabe-Klasse
/// (bei Objekt-Rückgabe der Klassenname — für Feldzugriff auf das Ergebnis).
struct Sig {
    params: Vec<Ty>,
    ret: Ty,
    ret_class: Option<String>,
}

pub fn lower_module(m: &Module) -> Result<Program, Vec<String>> {
    let mut prog = Program::default();
    let mut errs = Vec::new();

    // Produkttypen → Klassen. Summentypen → EINE getaggte Klasse: Feld `__tag`
    // (I64) + alle Variantenfelder geflacht (`Variant_field`). Match dispatcht
    // über `__tag`. (Platz = Summe aller Varianten; einfach, passt zum flachen
    // Klassenmodell. Kompaktere Union folgt später.)
    let mut types: HashMap<String, Layout> = HashMap::new();
    let mut variants: HashMap<String, VariantInfo> = HashMap::new();
    for it in &m.items {
        if let Item::Type(t) = it {
            if t.variants.is_empty() {
                let layout: Layout = t
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), ty_of(Some(&f.ty)), class_of(Some(&f.ty))))
                    .collect();
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
    // ClassInfo je Nutzertyp (Produkt UND Summe) registrieren (super = Object).
    for it in &m.items {
        if let Item::Type(t) = it {
            let fields = types[&t.name]
                .iter()
                .map(|(n, ty, rt)| fastllvm_ir::FieldInfo { name: n.clone(), ty: *ty, ref_target: rt.clone() })
                .collect();
            prog.classes.push(fastllvm_ir::ClassInfo {
                name: t.name.clone(),
                super_name: Some("java/lang/Object".to_string()),
                is_interface: false,
                interfaces: vec![],
                fields,
                static_fields: vec![],
                methods: vec![],
                has_clinit: false,
            });
        }
    }

    // Signatur-Tabelle (Name → (ParamTypen, RückgabeTyp, Rückgabe-Klasse)) für Aufrufe.
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            let ps = f.sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
            sigs.insert(f.sig.name.clone(), Sig { params: ps, ret: guess_ret_ty(f), ret_class: class_of(f.sig.ret.as_ref()) });
        }
        // extern "C" { fn name(...) -> T }: C-ABI-Funktion, direkt unter ihrem
        // Namen (keine Mangling). Aufrufe lösen darüber auf; das Backend
        // deklariert die gerufene-aber-undefinierte Funktion, clang linkt sie
        // (libc/libm/-lstdc++ / verlinkte Objekte).
        if let Item::Extern { items, .. } = it {
            for sig in items {
                let ps = sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
                sigs.insert(sig.name.clone(), Sig { params: ps, ret: ret_ty(sig), ret_class: class_of(sig.ret.as_ref()) });
            }
        }
    }
    // Methoden (Type-inline + impl-Blöcke) → Symbol `Class.method`, self = Ref.
    let methods = collect_methods(m);
    for (class, meth) in &methods {
        let ps = meth
            .sig
            .params
            .iter()
            .map(|p| if p.name == "self" { Ty::Ref } else { ty_of(p.ty.as_ref()) })
            .collect();
        let sym = format!("{class}.{}", meth.sig.name);
        sigs.insert(sym, Sig { params: ps, ret: guess_ret_ty(meth), ret_class: class_of(meth.sig.ret.as_ref()) });
    }
    for it in &m.items {
        if let Item::Fn(f) = it {
            match lower_fn(f, &sigs, &types, &variants, &mut prog.strings, None, None) {
                Ok(func) => prog.functions.push(func),
                Err(mut e) => errs.append(&mut e),
            }
        }
        // trait/const/use: M2+ — hier noch übersprungen
    }
    for (class, meth) in &methods {
        let sym = format!("{class}.{}", meth.sig.name);
        match lower_fn(meth, &sigs, &types, &variants, &mut prog.strings, Some(class), Some(&sym)) {
            Ok(func) => prog.functions.push(func),
            Err(mut e) => errs.append(&mut e),
        }
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
        Some("Int") | Some("I64") | Some("U64") => Ty::I64,
        Some("Unit") | None => Ty::I64, // Default-Ganzzahl, wenn nichts steht
        // Alles andere ist ein (Nutzer-)Referenztyp: Objekt auf dem Heap.
        Some(_) => Ty::Ref,
    }
}

/// Klassenname eines Referenztyp-Annotats (für GetField/New), sonst None.
fn class_of(t: Option<&Type>) -> Option<String> {
    let name = t?.name.as_str();
    match name {
        "Float" | "F64" | "F32" | "Bool" | "Str" | "I32" | "U32" | "Int" | "I64" | "U64" | "Unit" => None,
        _ => Some(name.to_string()),
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
    sigs: &'a HashMap<String, Sig>,
    /// Nutzertyp-Layouts (Name → Felder) für New/GetField.
    types: &'a HashMap<String, Layout>,
    /// Varianten-Registry (Variantenname → Info) für Konstruktion + Match.
    variants: &'a HashMap<String, VariantInfo>,
    /// Klasse eines Ref-Locals (Objekt-Local-Index → Klassenname) für Feldzugriff.
    local_class: HashMap<u32, String>,
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
    /// Klasse eines Operanden, falls er ein Ref-Local mit bekannter Klasse ist.
    fn class_of_operand(&self, op: &Operand) -> Option<String> {
        match op {
            Operand::Copy(l) => self.local_class.get(&l.0).cloned(),
            _ => None,
        }
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
                        // Objekt-Klasse bei Neuzuweisung aktualisieren (Traversal
                        // `cur = cur.next` muss cur weiter als Node kennen).
                        if let Some(c) = self.class_of_operand(&op) {
                            self.local_class.insert(l.0, c);
                        }
                        self.emit(Statement::Assign(l, Rvalue::Use(op)));
                        return;
                    }
                }
                let (op, ty) = match value {
                    Some(v) => self.lower_expr(v),
                    None => (Operand::ConstI64(0), Ty::I64),
                };
                let l = self.new_local(ty);
                // Objekt-Klasse an den neuen Local weiterreichen (für p.x).
                if let Some(c) = self.class_of_operand(&op) {
                    self.local_class.insert(l.0, c);
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
                        }
                        let rv = match op {
                            None => Rvalue::Use(rhs),
                            Some(o) => Rvalue::Binary(map_op(*o), Operand::Copy(l), rhs),
                        };
                        self.emit(Statement::Assign(l, rv));
                    } else {
                        self.errs.push(format!("unbekannte Variable: {name}"));
                    }
                }
                // Feldmutation `p.x = v` bzw. `p.x op= v` → (Get)+Binary+PutField.
                Expr::Field { base, name, .. } => {
                    let (obj, _) = self.lower_expr(base);
                    let class = match self.class_of_operand(&obj) {
                        Some(c) => c,
                        None => {
                            self.errs.push(format!("Feldzuweisung `.{name}`: Typ des Objekts unbekannt (annotieren)"));
                            return;
                        }
                    };
                    let fty = match self.types.get(&class).and_then(|l| l.iter().find(|(n, ..)| n == name)) {
                        Some((_, ty, _)) => *ty,
                        None => {
                            self.errs.push(format!("`{class}` hat kein Feld `{name}`"));
                            return;
                        }
                    };
                    let (mut v, _) = self.lower_expr(value);
                    if let Some(o) = op {
                        // compound: alten Wert lesen, verrechnen.
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
                _ => {
                    self.errs.push("Zuweisungsziel M2: nur Variablen und Felder".into());
                }
            },
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
            // `null` — MESS-BOOTSTRAP (nicht die endgültige Sprache; die hat kein
            // null, sondern Option). Nur nötig, um verkettete/zyklische Graphen
            // zu konstruieren und damit ZUM ERSTEN MAL den RC-/Kollektor-Pfad auf
            // Vire-IR zu betreten (M0.1b-auf-Vire). Wird durch Option[T] ersetzt.
            Expr::Ident(name, _) if name == "null" && self.lookup(name).is_none() => {
                (Operand::ConstNull, Ty::Ref)
            }
            // Nullary-Variante als Ausdruck: `Empty` → getaggte Instanz.
            Expr::Ident(name, _) if self.variants.contains_key(name) && self.lookup(name).is_none() => {
                self.build_variant(name, &[])
            }
            Expr::Ident(name, _) => match self.lookup(name) {
                Some((l, ty)) => (Operand::Copy(l), ty),
                None => {
                    self.errs.push(format!("unbekannte Variable: {name}"));
                    (Operand::ConstI64(0), Ty::I64)
                }
            },
            // `self` = der als Parameter gebundene Empfänger.
            Expr::SelfExpr(_) => match self.lookup("self") {
                Some((l, ty)) => (Operand::Copy(l), ty),
                None => {
                    self.errs.push("`self` außerhalb einer Methode".into());
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
            Expr::Field { base, name, .. } => {
                let (obj, _) = self.lower_expr(base);
                let class = match self.class_of_operand(&obj) {
                    Some(c) => c,
                    None => {
                        self.errs.push(format!("Feldzugriff `.{name}`: Typ des Objekts unbekannt (annotieren)"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let (fty, rtarget) = match self.types.get(&class).and_then(|l| l.iter().find(|(n, ..)| n == name)) {
                    Some((_, ty, rt)) => (*ty, rt.clone()),
                    None => {
                        self.errs.push(format!("`{class}` hat kein Feld `{name}`"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let d = self.new_local(fty);
                if let Some(rt) = rtarget {
                    self.local_class.insert(d.0, rt);
                }
                self.emit(Statement::GetField { dest: d, obj, class, field: name.clone() });
                (Operand::Copy(d), fty)
            }
            Expr::Call { callee, args, .. } => self.lower_call(callee, args),
            Expr::If { cond, then, elifs, els, .. } => self.lower_if(cond, then, elifs, els),
            Expr::Match { scrutinee, arms, .. } => self.lower_match(scrutinee, arms),
            Expr::Block(b) => self.lower_block_val(b),
            // capsule (reine Form, Skalar-rein/-raus): der Rumpf läuft in einer
            // eigenen Arena. `jrt_arena_push` vor dem Rumpf routet alle Heap-
            // Allokationen dorthin (immortal → kein RC/Kollektor), `jrt_arena_pop`
            // danach gibt die Arena en bloc frei. NUR Skalar-Eingaben/-Ergebnis
            // erlaubt (harte Fehler sonst): Werte können nicht aliasieren, und kein
            // Objektzeiger überlebt die Arena → Isolation + Fault-Containment ohne
            // Deep-Copy. Objekt-rein/-raus (Deep-Copy) bleibt offen.
            Expr::Capsule { inputs, body, .. } => {
                for (nm, _borrowed) in inputs {
                    if let Some((_, Ty::Ref)) = self.lookup(nm) {
                        self.errs.push(format!(
                            "capsule: Objekt-Eingabe `{nm}` noch nicht erlaubt — die Isolation \
                             braucht Deep-Copy-in (noch nicht implementiert). Bis dahin nur \
                             Skalar-Eingaben (Int/Float/Bool), sonst wäre die Containment-Garantie \
                             eine Lüge."
                        ));
                    }
                }
                // `return` im Rumpf würde arena_pop überspringen (Arena-Leck) →
                // verbieten. break/continue: die Loop-Ziele werden gespeichert und
                // während des Rumpfs geleert (innere Schleifen setzen eigene).
                if body_has_return(body) {
                    self.errs.push("capsule: `return` im Rumpf nicht erlaubt (würde die Arena lecken) — nutze den Blockwert".into());
                }
                self.emit(Statement::Call { dest: None, func: "jrt_arena_push".into(), args: vec![] });
                let saved_loops = std::mem::take(&mut self.loops);
                let body_locals_start = self.locals.len(); // ab hier: Rumpf-Locals
                let (val, ty) = self.lower_block_val(body);
                self.loops = saved_loops;
                if ty == Ty::Ref {
                    self.errs.push(
                        "capsule: Objekt-Ergebnis noch nicht erlaubt — das braucht Deep-Copy-out \
                         (sonst dangling in die freigegebene Arena). Bis dahin nur Skalar-Ergebnis."
                            .into(),
                    );
                }
                // Skalar-Ergebnis zuerst festhalten (Register/Const, überlebt den Pop).
                let res = self.new_local(if ty == Ty::Void { Ty::I64 } else { ty });
                if ty != Ty::Void {
                    self.emit(Statement::Assign(res, Rvalue::Use(val)));
                }
                // Alle im Rumpf erzeugten Ref-Locals zeigen in die Arena. Nach dem
                // Pop ist der Speicher weg; das Backend gibt Ref-Locals aber beim
                // Funktionsende frei (liest den Header → use-after-free). Deshalb VOR
                // dem Pop auf null setzen → jrt_release(null) ist ein No-Op.
                for idx in body_locals_start..self.locals.len() {
                    if self.locals[idx] == Ty::Ref {
                        self.emit(Statement::Assign(Local(idx as u32), Rvalue::Use(Operand::ConstNull)));
                    }
                }
                self.emit(Statement::Call { dest: None, func: "jrt_arena_pop".into(), args: vec![] });
                if ty == Ty::Void {
                    (Operand::ConstI64(0), Ty::Void)
                } else {
                    (Operand::Copy(res), ty)
                }
            }
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
        // Methodenaufruf `obj.method(args)` → direkter Aufruf `Class.method(obj, args)`
        // (monomorph, kein virtueller Dispatch — Vire-Typen sind (noch) flach).
        if let Expr::Field { base, name, .. } = callee {
            let (obj, _) = self.lower_expr(base);
            let class = match self.class_of_operand(&obj) {
                Some(c) => c,
                None => {
                    self.errs.push(format!("Methodenaufruf `.{name}()`: Typ des Empfängers unbekannt (annotieren)"));
                    return (Operand::ConstI64(0), Ty::I64);
                }
            };
            let sym = format!("{class}.{name}");
            let mut arg_ops = vec![obj];
            for a in args {
                arg_ops.push(self.lower_expr(a).0);
            }
            let (ret, ret_class) = self.sigs.get(&sym).map(|s| (s.ret, s.ret_class.clone())).unwrap_or_else(|| {
                self.errs.push(format!("`{class}` hat keine Methode `{name}`"));
                (Ty::I64, None)
            });
            if ret == Ty::Void {
                self.emit(Statement::Call { dest: None, func: sym, args: arg_ops });
                return (Operand::ConstI64(0), Ty::Void);
            }
            let d = self.new_local(ret);
            if let Some(c) = ret_class {
                self.local_class.insert(d.0, c);
            }
            self.emit(Statement::Call { dest: Some(d), func: sym, args: arg_ops });
            return (Operand::Copy(d), ret);
        }
        let name = match callee {
            Expr::Ident(n, _) => n.clone(),
            _ => {
                self.errs.push("Aufruf-Ziel M2: nur benannte Funktionen".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        // Varianten-Konstruktor eines Summentyps: `Circle(2.0)` → getaggte Instanz.
        if self.variants.contains_key(&name) {
            return self.build_variant(&name, args);
        }
        // Konstruktor eines Nutzertyps: `Point(x, y)` → New + PutField je Feld
        // (Feldreihenfolge = Deklarationsreihenfolge).
        if let Some(layout) = self.types.get(&name).cloned() {
            let obj = self.new_local(Ty::Ref);
            self.local_class.insert(obj.0, name.clone());
            self.emit(Statement::New { dest: obj, class: name.clone() });
            if args.len() != layout.len() {
                self.errs.push(format!("{name}: {} Felder erwartet, {} übergeben", layout.len(), args.len()));
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
        let lowered: Vec<(Operand, Ty)> = args.iter().map(|a| self.lower_expr(a)).collect();
        // Intrinsic `print` — mehrargumentig: jedes Argument in eigener Zeile.
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
        // Aufruf einer eigenen Funktion
        let (ret, ret_class) = self.sigs.get(&name).map(|s| (s.ret, s.ret_class.clone())).unwrap_or((Ty::I64, None));
        let arg_ops: Vec<Operand> = lowered.into_iter().map(|(o, _)| o).collect();
        if ret == Ty::Void {
            self.emit(Statement::Call { dest: None, func: name, args: arg_ops });
            (Operand::ConstI64(0), Ty::Void)
        } else {
            let d = self.new_local(ret);
            if let Some(c) = ret_class {
                self.local_class.insert(d.0, c); // Objekt-Rückgabe: Klasse merken
            }
            self.emit(Statement::Call { dest: Some(d), func: name, args: arg_ops });
            (Operand::Copy(d), ret)
        }
    }

    /// Getaggte Variante bauen: `New Sum`, `__tag = t`, Felder aus den Argumenten.
    fn build_variant(&mut self, vname: &str, args: &[Expr]) -> (Operand, Ty) {
        let (sum, tag, vfields) = self.variants.get(vname).cloned().unwrap();
        let obj = self.new_local(Ty::Ref);
        self.local_class.insert(obj.0, sum.clone());
        self.emit(Statement::New { dest: obj, class: sum.clone() });
        self.emit(Statement::PutField { obj: Operand::Copy(obj), class: sum.clone(), field: "__tag".into(), value: Operand::ConstI64(tag) });
        if args.len() != vfields.len() {
            self.errs.push(format!("Variante `{vname}`: {} Felder erwartet, {} übergeben", vfields.len(), args.len()));
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

    /// `match s { Variant(binds) -> body … _ -> body }` → Dispatch über `__tag`,
    /// Feld-Extraktion je Arm, Phi-Ersatz über ein Ergebnis-Local (wie lower_if).
    fn lower_match(&mut self, scrut: &Expr, arms: &[(Pattern, Option<Expr>, Expr)]) -> (Operand, Ty) {
        let (obj, _) = self.lower_expr(scrut);
        let class = match self.class_of_operand(&obj) {
            Some(c) => c,
            None => {
                self.errs.push("match: Typ des Ausdrucks unbekannt (Summentyp annotieren)".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        let tag = self.new_local(Ty::I64);
        self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: class.clone(), field: "__tag".into() });
        let merge = self.new_block();
        let mut ends: Vec<(usize, Operand, Ty)> = Vec::new();
        for (pat, guard, body) in arms {
            if guard.is_some() {
                self.errs.push("match-Guard (`if`) noch nicht abgesenkt".into());
            }
            match pat {
                Pattern::Ctor { name, args, .. } => {
                    let (_, vtag, vfields) = match self.variants.get(name) {
                        Some(v) => v.clone(),
                        None => {
                            self.errs.push(format!("unbekannte Variante `{name}` im match"));
                            continue;
                        }
                    };
                    let cond = self.new_local(Ty::I32);
                    self.emit(Statement::Assign(cond, Rvalue::Binary(IB::CmpEq, Operand::Copy(tag), Operand::ConstI64(vtag))));
                    let armb = self.new_block();
                    let nextb = self.new_block();
                    let cur = self.cur;
                    self.term(cur, Terminator::Branch { cond: Operand::Copy(cond), then_blk: armb, else_blk: nextb });
                    self.cur = armb.0 as usize;
                    self.scopes.push(HashMap::new());
                    for (j, argpat) in args.iter().enumerate() {
                        if let Pattern::Bind(bn, _) = argpat {
                            if let Some((fname, fty, rt)) = vfields.get(j) {
                                let d = self.new_local(*fty);
                                if let Some(rc) = rt {
                                    self.local_class.insert(d.0, rc.clone());
                                }
                                self.emit(Statement::GetField { dest: d, obj: obj.clone(), class: class.clone(), field: fname.clone() });
                                self.bind(bn, d, *fty);
                            }
                        }
                    }
                    let (v, t) = self.lower_expr(body);
                    self.scopes.pop();
                    ends.push((self.cur, v, t));
                    self.cur = nextb.0 as usize;
                }
                Pattern::Wildcard(_) | Pattern::Bind(..) => {
                    self.scopes.push(HashMap::new());
                    if let Pattern::Bind(bn, _) = pat {
                        if let Operand::Copy(l) = obj {
                            self.bind(bn, l, Ty::Ref);
                            self.local_class.insert(l.0, class.clone());
                        }
                    }
                    let (v, t) = self.lower_expr(body);
                    self.scopes.pop();
                    ends.push((self.cur, v, t));
                    let dead = self.new_block();
                    self.cur = dead.0 as usize;
                }
                _ => self.errs.push("match-Muster M2: nur Varianten, `_` und Bindungen".into()),
            }
        }
        let rty = ends.iter().map(|(_, _, t)| *t).find(|t| *t != Ty::Void).unwrap_or(Ty::Void);
        if rty != Ty::Void {
            let res = self.new_local(rty);
            for (end, v, _) in &ends {
                self.blocks[*end].statements.push(Statement::Assign(res, Rvalue::Use(v.clone())));
                self.blocks[*end].terminator = Terminator::Goto(merge);
            }
            // Fallthrough (nicht-erschöpfend): Default-Wert.
            let cur = self.cur;
            self.blocks[cur].statements.push(Statement::Assign(res, Rvalue::Use(zero_of(rty))));
            self.term(cur, Terminator::Goto(merge));
            self.cur = merge.0 as usize;
            (Operand::Copy(res), rty)
        } else {
            for (end, _, _) in &ends {
                self.blocks[*end].terminator = Terminator::Goto(merge);
            }
            let cur = self.cur;
            self.term(cur, Terminator::Goto(merge));
            self.cur = merge.0 as usize;
            (Operand::ConstI64(0), Ty::Void)
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

fn lower_fn(
    f: &FnDef,
    sigs: &HashMap<String, Sig>,
    types: &HashMap<String, Layout>,
    variants: &HashMap<String, VariantInfo>,
    strings: &mut Vec<String>,
    recv_class: Option<&str>,
    sym: Option<&str>,
) -> Result<Function, Vec<String>> {
    let ret = guess_ret_ty(f);
    let name = match sym {
        Some(s) => s.to_string(),
        None if f.sig.name == "main" => "java_main".to_string(),
        None => f.sig.name.clone(),
    };
    let mut fl = FnLower {
        locals: Vec::new(),
        blocks: Vec::new(),
        cur: 0,
        scopes: vec![HashMap::new()],
        sigs,
        types,
        variants,
        local_class: HashMap::new(),
        strings,
        errs: Vec::new(),
        loops: Vec::new(),
    };
    // Block 0
    fl.new_block();
    // Parameter → Locals 0..n
    let mut param_tys = Vec::new();
    for p in &f.sig.params {
        // `self`-Empfänger: Ref auf die Methoden-Klasse.
        let (t, cls) = if p.name == "self" {
            (Ty::Ref, recv_class.map(|c| c.to_string()))
        } else {
            (ty_of(p.ty.as_ref()), class_of(p.ty.as_ref()))
        };
        param_tys.push(t);
        let l = fl.new_local(t);
        if let Some(c) = cls {
            fl.local_class.insert(l.0, c);
        }
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
        } else if ret == Ty::Void {
            Terminator::Return(None)
        } else {
            // Kein Tail, aber typisierter Rückgabewert: der Wert kommt aus einem
            // `return`-Statement; dieser Fallthrough-Block ist unerreichbar. Er
            // muss aber typkorrekt terminieren (sonst `ret void` in i64-Funktion).
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
    Ok(Function {
        name,
        params: param_tys,
        ret,
        locals: fl.locals,
        blocks: fl.blocks,
        receiver_nonnull: false,
    })
}

/// Enthält ein Block (rekursiv über verschachtelte Blöcke/Schleifen/if) ein
/// `return`-Statement? Für die capsule-Prüfung (return würde arena_pop überspringen).
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

/// Typkorrekter Null-/Default-Operand (für unerreichbare typisierte Returns).
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
