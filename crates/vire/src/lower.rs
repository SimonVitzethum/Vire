//! Absenkung Vire-AST → `crates/ir` (SSA-nah, keine Slot-Wiederverwendung).
//! Deckt den M2-Kern ab: Funktionen, Arithmetik, Kontrollfluss (if/while/
//! for-über-Range), `print`, Aufrufe eigener Funktionen. Generics/Traits/
//! Closures/capsule folgen (FRONTEND-PLAN F5–F8).

use std::collections::HashMap;

use fastllvm_ir::{ArrKind, BasicBlock, BinOp as IB, Block, Function, Local, Operand, Program, Rvalue, Statement, Terminator, Ty};

use crate::ast::*;

/// Feldlayout eines Nutzertyps: (Feldname, IR-Typ, Ref-Ziel-Klasse).
type Layout = Vec<(String, Ty, Option<String>)>;

/// Variante eines Summentyps: (Summentyp-Name, Tag, Felder als (geflachter Name, Typ, Ref-Klasse)).
type VariantInfo = (String, i64, Vec<(String, Ty, Option<String>)>);

/// Info über eine generische Funktion für die Monomorphisierung an Aufrufstellen.
#[derive(Clone)]
struct GInfo {
    /// Typ-Parameter-Namen, z.B. `["T"]` bei `fn f[T](…)`.
    tparams: Vec<String>,
    /// Parameter-Typannotate (mit T-Platzhaltern), zum Binden der Typargumente.
    param_tys: Vec<Option<Type>>,
    /// Rückgabe-Annotat (mit T).
    ret: Option<Type>,
}

/// Symbolname einer Monomorph.-Instanz: `f$Int$Point`.
fn mono_sym(name: &str, targs: &[String]) -> String {
    format!("{name}${}", targs.join("$"))
}

/// Konkreter Typname eines Arguments (für Typ-Argument-Bindung).
fn concrete_tyname(ty: Ty, class: Option<&String>) -> String {
    match ty {
        Ty::F64 => "Float".into(),
        Ty::F32 => "F32".into(),
        Ty::I32 => "I32".into(),
        Ty::Ref => class.cloned().unwrap_or_else(|| "Str".into()),
        _ => "Int".into(),
    }
}

/// Ersetzt Typparameter-Namen in einem `Type` durch konkrete Typen.
fn subst_type(t: &Type, bind: &HashMap<String, String>) -> Type {
    let name = bind.get(&t.name).cloned().unwrap_or_else(|| t.name.clone());
    Type { name, args: t.args.iter().map(|a| subst_type(a, bind)).collect(), borrowed: t.borrowed, span: t.span }
}

/// Klont eine generische FnDef und substituiert die Typparameter in Signatur +
/// Body-Annotaten (Let/Cast). Der Rest des Bodies läuft über Inferenz.
fn subst_fndef(f: &FnDef, bind: &HashMap<String, String>) -> FnDef {
    let mut nf = f.clone();
    nf.sig.generics = vec![]; // Instanz ist nicht mehr generisch
    for p in &mut nf.sig.params {
        if let Some(t) = &p.ty {
            p.ty = Some(subst_type(t, bind));
        }
    }
    if let Some(t) = &nf.sig.ret {
        nf.sig.ret = Some(subst_type(t, bind));
    }
    if let Some(b) = &mut nf.body {
        subst_block(b, bind);
    }
    nf
}

fn subst_block(b: &mut crate::ast::Block, bind: &HashMap<String, String>) {
    for s in &mut b.stmts {
        subst_stmt(s, bind);
    }
    // Tail-Ausdrücke enthalten selten Typannotate; Casts darin über subst_expr.
    if let Some(t) = &mut b.tail {
        subst_expr(t, bind);
    }
}

fn subst_stmt(s: &mut Stmt, bind: &HashMap<String, String>) {
    match s {
        Stmt::Let { value: Some(v), .. } => subst_expr(v, bind),
        Stmt::Expr(e) | Stmt::Return(Some(e), _) => subst_expr(e, bind),
        Stmt::Assign { value, .. } => subst_expr(value, bind),
        Stmt::While { body, .. } | Stmt::For { body, .. } => subst_block(body, bind),
        _ => {}
    }
}

fn subst_expr(e: &mut Expr, bind: &HashMap<String, String>) {
    match e {
        Expr::Cast { ty, inner, .. } => {
            *ty = subst_type(ty, bind);
            subst_expr(inner, bind);
        }
        Expr::Binary { lhs, rhs, .. } => {
            subst_expr(lhs, bind);
            subst_expr(rhs, bind);
        }
        Expr::Unary { rhs, .. } => subst_expr(rhs, bind),
        Expr::Call { callee, args, .. } => {
            subst_expr(callee, bind);
            for a in args {
                subst_expr(a, bind);
            }
        }
        Expr::If { cond, then, elifs, els, .. } => {
            subst_expr(cond, bind);
            subst_block(then, bind);
            for (c, b) in elifs {
                subst_expr(c, bind);
                subst_block(b, bind);
            }
            if let Some(b) = els {
                subst_block(b, bind);
            }
        }
        Expr::Block(b) => subst_block(b, bind),
        Expr::Field { base, .. } | Expr::Try { inner: base, .. } => subst_expr(base, bind),
        _ => {}
    }
}

/// Eingebaute FFI-/Python-Brücken-Signaturen (Ptr = i64). Immer verfügbar, damit
/// Python aus reinem Vire ohne `extern`-Block nutzbar ist.
fn builtin_ffi_sigs() -> Vec<(&'static str, Vec<Ty>, Ty)> {
    use Ty::*;
    vec![
        ("py_import", vec![I64], I64),
        ("py_getattr", vec![I64, I64], I64),
        ("py_call_f", vec![I64, F64], I64),
        ("py_call_ff", vec![I64, F64, F64], I64),
        ("py_call_i", vec![I64, I64], I64),
        ("py_call_s", vec![I64, I64], I64),
        ("py_float", vec![F64], I64),
        ("py_int", vec![I64], I64),
        ("py_str", vec![I64], I64),
        ("py_asfloat", vec![I64], F64),
        ("py_asint", vec![I64], I64),
        ("py_getitem_i", vec![I64, I64], I64),
        ("vire_py_eval_f", vec![Ref, F64], F64),
        ("vire_py_eval_i", vec![Ref, I64], I64),
    ]
}

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
    // Eingebaute Summentypen Option/Result (falls nicht vom Nutzer definiert).
    // Payload ist derzeit i64-breit (Int/Zeiger); typisierte/Float-Payloads
    // brauchen generische Typen (nächster Schritt).
    if !types.contains_key("Option") {
        types.insert("Option".into(), vec![("__tag".into(), Ty::I64, None), ("Some_value".into(), Ty::I64, None)]);
        variants.insert("Some".into(), ("Option".into(), 0, vec![("Some_value".into(), Ty::I64, None)]));
        variants.insert("None".into(), ("Option".into(), 1, vec![]));
    }
    if !types.contains_key("Result") {
        types.insert("Result".into(), vec![("__tag".into(), Ty::I64, None), ("Ok_value".into(), Ty::I64, None), ("Err_error".into(), Ty::I64, None)]);
        variants.insert("Ok".into(), ("Result".into(), 0, vec![("Ok_value".into(), Ty::I64, None)]));
        variants.insert("Err".into(), ("Result".into(), 1, vec![("Err_error".into(), Ty::I64, None)]));
    }
    // ClassInfo je Typ (Nutzer + eingebaut) registrieren.
    let mut all_type_names: Vec<String> = m.items.iter().filter_map(|it| if let Item::Type(t) = it { Some(t.name.clone()) } else { None }).collect();
    for bi in ["Option", "Result"] {
        if types.contains_key(bi) && !all_type_names.iter().any(|n| n == bi) {
            all_type_names.push(bi.into());
        }
    }
    for tname in &all_type_names {
        let fields = types[tname]
            .iter()
            .map(|(n, ty, rt)| fastllvm_ir::FieldInfo { name: n.clone(), ty: *ty, ref_target: rt.clone() })
            .collect();
        prog.classes.push(fastllvm_ir::ClassInfo {
            name: tname.clone(),
            super_name: Some("java/lang/Object".to_string()),
            is_interface: false,
            interfaces: vec![],
            fields,
            static_fields: vec![],
            methods: vec![],
            has_clinit: false,
        });
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
    // Eingebaute Python-Brücke: Signaturen immer registrieren, damit `py_import`
    // & Co. OHNE `extern`-Block aus reinem Vire aufrufbar sind (die Absenkung
    // emittiert Calls, das Backend deklariert, der Treiber linkt die Brücke).
    for (name, params, ret) in builtin_ffi_sigs() {
        sigs.entry(name.to_string()).or_insert(Sig { params, ret, ret_class: None });
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
    // Generische Funktionen sammeln (NICHT direkt absenken — pro Aufruf-Typargument
    // eine Monomorph.-Instanz). Trait-Schranken werden geparst, aber noch nicht
    // aufgelöst (Trait-Solving/Kohärenz ist die offene schwere Hälfte).
    let mut generics: HashMap<String, GInfo> = HashMap::new();
    let mut generic_defs: HashMap<String, FnDef> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if !f.sig.generics.is_empty() {
                generics.insert(
                    f.sig.name.clone(),
                    GInfo {
                        tparams: f.sig.generics.iter().map(|g| g.name.clone()).collect(),
                        param_tys: f.sig.params.iter().map(|p| p.ty.clone()).collect(),
                        ret: f.sig.ret.clone(),
                    },
                );
                generic_defs.insert(f.sig.name.clone(), f.clone());
            }
        }
    }
    let mut mono_queue: Vec<(String, Vec<String>)> = Vec::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if !f.sig.generics.is_empty() {
                continue; // generisch → nur bei Bedarf instanziiert
            }
            match lower_fn(f, &sigs, &types, &variants, &generics, &mut prog.strings, None, None) {
                Ok((func, mono)) => {
                    prog.functions.push(func);
                    mono_queue.extend(mono);
                }
                Err(mut e) => errs.append(&mut e),
            }
        }
        // trait/const/use: hier übersprungen (Trait-Dispatch offen)
    }
    for (class, meth) in &methods {
        let sym = format!("{class}.{}", meth.sig.name);
        match lower_fn(meth, &sigs, &types, &variants, &generics, &mut prog.strings, Some(class), Some(&sym)) {
            Ok((func, mono)) => {
                prog.functions.push(func);
                mono_queue.extend(mono);
            }
            Err(mut e) => errs.append(&mut e),
        }
    }
    // Monomorphisierungs-Worklist: jede angeforderte Instanz substituieren +
    // absenken (kann weitere Instanzen anfordern), bis Fixpunkt.
    let mut mono_done: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some((gname, targs)) = mono_queue.pop() {
        let sym = mono_sym(&gname, &targs);
        if !mono_done.insert(sym.clone()) {
            continue;
        }
        let Some(gdef) = generic_defs.get(&gname) else { continue };
        let bind: HashMap<String, String> = gdef.sig.generics.iter().map(|g| g.name.clone()).zip(targs.iter().cloned()).collect();
        let inst = subst_fndef(gdef, &bind);
        // Instanz-Signatur registrieren (für Rekursion/gegenseitige Aufrufe).
        let ps = inst.sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
        sigs.insert(sym.clone(), Sig { params: ps, ret: guess_ret_ty(&inst), ret_class: class_of(inst.sig.ret.as_ref()) });
        match lower_fn(&inst, &sigs, &types, &variants, &generics, &mut prog.strings, None, Some(&sym)) {
            Ok((func, mono)) => {
                prog.functions.push(func);
                mono_queue.extend(mono);
            }
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
        // `Ptr` = opaker Roh-Zeiger (FFI): i64-breit, KEIN RC (kein Vire-Objekt).
        Some("Ptr") => Ty::I64,
        Some("Unit") | None => Ty::I64, // Default-Ganzzahl, wenn nichts steht
        // Alles andere ist ein (Nutzer-)Referenztyp: Objekt auf dem Heap.
        Some(_) => Ty::Ref,
    }
}

/// Klassenname eines Referenztyp-Annotats (für GetField/New), sonst None.
fn class_of(t: Option<&Type>) -> Option<String> {
    let name = t?.name.as_str();
    match name {
        "Float" | "F64" | "F32" | "Bool" | "Str" | "I32" | "U32" | "Int" | "I64" | "U64" | "Unit" | "Ptr" => None,
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
    /// Generische Funktionen (Name → Info) für Aufruf-Monomorphisierung.
    generics: &'a HashMap<String, GInfo>,
    /// Angeforderte Monomorph.-Instanzen: (generischer Name, konkrete Typargumente).
    mono: Vec<(String, Vec<String>)>,
    /// Klasse eines Ref-Locals (Objekt-Local-Index → Klassenname) für Feldzugriff.
    local_class: HashMap<u32, String>,
    /// Elementart eines Array/List-Locals (für Index/len/for-über-Liste).
    local_arr: HashMap<u32, ArrKind>,
    /// Lambda-Locals: `mut f = x -> …` → (Parameter, Rumpf). Aufruf `f(a)` wird
    /// an der Stelle inline expandiert (fangende Closures im gleichen Scope gratis).
    local_lambda: HashMap<u32, (Vec<String>, Expr)>,
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
    /// Array-Elementart eines Operanden, falls er ein Array/List-Local ist.
    fn arr_of_operand(&self, op: &Operand) -> Option<ArrKind> {
        match op {
            Operand::Copy(l) => self.local_arr.get(&l.0).copied(),
            _ => None,
        }
    }
    /// Operand auf i32 bringen (Array-Index/Länge sind Java-`int`).
    fn to_i32(&mut self, op: Operand) -> Operand {
        match op {
            Operand::ConstI64(v) => Operand::ConstI32(v as i32),
            Operand::ConstI32(_) => op,
            other => {
                let d = self.new_local(Ty::I32);
                self.emit(Statement::Assign(d, Rvalue::Convert(other)));
                Operand::Copy(d)
            }
        }
    }
    /// Operand → String (Ref). Ref bleibt; Skalare über jrt_*_to_str.
    fn to_str(&mut self, op: Operand, ty: Ty) -> Operand {
        if ty == Ty::Ref {
            return op;
        }
        let func = match ty {
            Ty::F64 => "jrt_double_to_str",
            Ty::F32 => "jrt_float_to_str",
            Ty::I32 => "jrt_int_to_str",
            _ => "jrt_long_to_str",
        };
        let arg = if ty == Ty::I64 { op } else if matches!(ty, Ty::F64 | Ty::F32 | Ty::I32) { op } else { to_i64(op) };
        let d = self.new_local(Ty::Ref);
        self.emit(Statement::Call { dest: Some(d), func: func.into(), args: vec![arg] });
        Operand::Copy(d)
    }
    /// ArrayLen (i32) → i64-Operand für Vire (Ints sind i64).
    fn array_len_i64(&mut self, arr: Operand) -> Operand {
        let li32 = self.new_local(Ty::I32);
        self.emit(Statement::ArrayLen { dest: li32, arr });
        let l64 = self.new_local(Ty::I64);
        self.emit(Statement::Assign(l64, Rvalue::Convert(Operand::Copy(li32))));
        Operand::Copy(l64)
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
                // `mut f = x -> …`: Lambda merken (Aufruf wird inline expandiert).
                if let Some(Expr::Lambda { params, body, .. }) = value {
                    let l = self.new_local(Ty::I64);
                    self.local_lambda.insert(l.0, (params.clone(), (**body).clone()));
                    self.bind(name, l, Ty::I64);
                    return;
                }
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
                // Objekt-Klasse bzw. Array-Elementart an den neuen Local weiterreichen.
                if let Some(c) = self.class_of_operand(&op) {
                    self.local_class.insert(l.0, c);
                }
                if let Some(k) = self.arr_of_operand(&op) {
                    self.local_arr.insert(l.0, k);
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
                // Index-Zuweisung `xs[i] = v` (Array oder wachsende Liste).
                Expr::Index { base, index, .. } => {
                    let (arr, _) = self.lower_expr(base);
                    let (idx, _) = self.lower_expr(index);
                    let (mut v, vt) = self.lower_expr(value);
                    if self.class_of_operand(&arr).as_deref() == Some("$List") {
                        self.emit(Statement::Call { dest: None, func: "vire_list_set".into(), args: vec![arr, to_i64(idx), to_i64(v)] });
                    } else if let Some(kind) = self.arr_of_operand(&arr) {
                        if kind == ArrKind::Long && vt != Ty::I64 {
                            v = to_i64(v);
                        }
                        let idx32 = self.to_i32(idx);
                        self.emit(Statement::ArrayStore { arr, index: idx32, value: v, kind, checked: true });
                    } else {
                        self.errs.push("Index-Zuweisung: kein Array/Liste".into());
                    }
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
                let name = match pat {
                    Pattern::Bind(n, _) => n.clone(),
                    Pattern::Wildcard(_) => "_".into(),
                    _ => {
                        self.errs.push("for-Muster: nur `for x in …`".into());
                        return;
                    }
                };
                // `for x in liste` (nicht-Range) → über Array iterieren:
                // i=0; while i<len { x = arr[i]; body; i++ }.
                if !matches!(iter, Expr::Range { .. }) {
                    let (arr, _) = self.lower_expr(iter);
                    // for über eine wachsende Liste ($List) → vire_list_len/get.
                    if self.class_of_operand(&arr).as_deref() == Some("$List") {
                        let len = self.new_local(Ty::I64);
                        self.emit(Statement::Call { dest: Some(len), func: "vire_list_len".into(), args: vec![arr.clone()] });
                        let ivar = self.new_local(Ty::I64);
                        self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
                        let header = self.new_block();
                        let cur = self.cur;
                        self.term(cur, Terminator::Goto(header));
                        self.cur = header.0 as usize;
                        let c = self.new_local(Ty::I32);
                        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), Operand::Copy(len))));
                        let bodyb = self.new_block();
                        let latch = self.new_block();
                        let exit = self.new_block();
                        self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(c), then_blk: bodyb, else_blk: exit });
                        self.cur = bodyb.0 as usize;
                        let elem = self.new_local(Ty::I64);
                        self.emit(Statement::Call { dest: Some(elem), func: "vire_list_get".into(), args: vec![arr.clone(), Operand::Copy(ivar)] });
                        self.scopes.push(HashMap::new());
                        self.bind(&name, elem, Ty::I64);
                        self.loops.push((latch, exit));
                        self.lower_block(body);
                        self.loops.pop();
                        self.scopes.pop();
                        let end = self.cur;
                        self.term(end, Terminator::Goto(latch));
                        self.cur = latch.0 as usize;
                        self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                        self.term(latch.0 as usize, Terminator::Goto(header));
                        self.cur = exit.0 as usize;
                        return;
                    }
                    let kind = match self.arr_of_operand(&arr) {
                        Some(k) => k,
                        None => {
                            self.errs.push("for-Iterator: Range `a..b` oder eine Liste".into());
                            return;
                        }
                    };
                    let vty = kind.value_ty();
                    let len = self.array_len_i64(arr.clone());
                    let ivar = self.new_local(Ty::I64);
                    self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
                    let header = self.new_block();
                    let cur = self.cur;
                    self.term(cur, Terminator::Goto(header));
                    self.cur = header.0 as usize;
                    let cond = self.new_local(Ty::I32);
                    self.emit(Statement::Assign(cond, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), len)));
                    let bodyb = self.new_block();
                    let latch = self.new_block();
                    let exit = self.new_block();
                    self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(cond), then_blk: bodyb, else_blk: exit });
                    self.cur = bodyb.0 as usize;
                    let elem = self.new_local(vty);
                    let idx32 = self.to_i32(Operand::Copy(ivar));
                    self.emit(Statement::ArrayLoad { dest: elem, arr, index: idx32, kind, checked: true });
                    self.scopes.push(HashMap::new());
                    self.bind(&name, elem, vty);
                    self.loops.push((latch, exit));
                    self.lower_block(body);
                    self.loops.pop();
                    self.scopes.pop();
                    let end = self.cur;
                    self.term(end, Terminator::Goto(latch));
                    self.cur = latch.0 as usize;
                    self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                    self.term(latch.0 as usize, Terminator::Goto(header));
                    self.cur = exit.0 as usize;
                    return;
                }
                let (start, end_op, incl) = match iter {
                    Expr::Range { start, end, inclusive, .. } => {
                        let (s, _) = self.lower_expr(start);
                        let (e, _) = self.lower_expr(end);
                        (s, e, *inclusive)
                    }
                    _ => unreachable!(),
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
            Expr::Binary { .. } if const_eval(e).is_some() => {
                // Allgemeine Konstantenfaltung: `2 + 3`, `WIDTH * HEIGHT` etc. →
                // Konstante zur Compilezeit (nicht nur unter `comptime`).
                match const_eval(e).unwrap() {
                    CVal::Int(v) => (Operand::ConstI64(v), Ty::I64),
                    CVal::Float(v) => (Operand::ConstF64(v), Ty::F64),
                    CVal::Bool(b) => (Operand::ConstI32(if b { 1 } else { 0 }), Ty::I32),
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let (l, lt) = self.lower_expr(lhs);
                let (r, rt) = self.lower_expr(rhs);
                // String-Verkettung: `+` mit mindestens einer Ref-Seite → Concat,
                // Zahlen werden automatisch zu Strings (`"n=" + n`).
                if matches!(op, BinOp::Add) && (lt == Ty::Ref || rt == Ty::Ref) {
                    let ls = self.to_str(l, lt);
                    let rs = self.to_str(r, rt);
                    let d = self.new_local(Ty::Ref);
                    self.emit(Statement::Call { dest: Some(d), func: "jrt_str_concat".into(), args: vec![ls, rs] });
                    return (Operand::Copy(d), Ty::Ref);
                }
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
            // `comptime <expr>` → Compilezeit-Faltung konstanter Ausdrücke.
            // `x as T` — numerische Konvertierung (int↔float, Breiten).
            Expr::Cast { inner, ty, .. } => {
                let (op, from) = self.lower_expr(inner);
                let to = ty_of(Some(ty));
                if from == to {
                    return (op, to);
                }
                let d = self.new_local(to);
                self.emit(Statement::Assign(d, Rvalue::Convert(op)));
                (Operand::Copy(d), to)
            }
            Expr::Comptime { inner, .. } => match const_eval(inner) {
                Some(CVal::Int(v)) => (Operand::ConstI64(v), Ty::I64),
                Some(CVal::Float(v)) => (Operand::ConstF64(v), Ty::F64),
                Some(CVal::Bool(b)) => (Operand::ConstI32(if b { 1 } else { 0 }), Ty::I32),
                None => {
                    self.errs.push("comptime: Ausdruck ist nicht konstant-faltbar (nur Literale/Arithmetik/Vergleiche)".into());
                    (Operand::ConstI64(0), Ty::I64)
                }
            },
            // `e?` — Fehler-Propagation für Result: Ok(v) → v; Err(_) → return e.
            // (Desugart zu match; die umgebende Funktion muss Result zurückgeben.)
            Expr::Try { inner, .. } => {
                let (obj, _) = self.lower_expr(inner);
                let tag = self.new_local(Ty::I64);
                self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: "Result".into(), field: "__tag".into() });
                let is_ok = self.new_local(Ty::I32);
                self.emit(Statement::Assign(is_ok, Rvalue::Binary(IB::CmpEq, Operand::Copy(tag), Operand::ConstI64(0))));
                let okb = self.new_block();
                let errb = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Branch { cond: Operand::Copy(is_ok), then_blk: okb, else_blk: errb });
                // Err-Zweig: das ganze Result weiterreichen.
                self.term(errb.0 as usize, Terminator::Return(Some(obj.clone())));
                // Ok-Zweig: den Wert extrahieren.
                self.cur = okb.0 as usize;
                let v = self.new_local(Ty::I64);
                self.emit(Statement::GetField { dest: v, obj, class: "Result".into(), field: "Ok_value".into() });
                (Operand::Copy(v), Ty::I64)
            }
            // List-Literal `[a, b, c]` → NewArray + ArrayStore. Elementart aus dem
            // ersten Element (homogen). Leere Liste → Long (Default).
            Expr::List(elems, _) => {
                let lowered: Vec<(Operand, Ty)> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let kind = lowered.first().map(|(_, t)| arrkind_of(*t)).unwrap_or(ArrKind::Long);
                let arr = self.new_local(Ty::Ref);
                self.local_arr.insert(arr.0, kind);
                self.emit(Statement::NewArray { dest: arr, kind, len: Operand::ConstI32(elems.len() as i32) });
                for (i, (mut v, t)) in lowered.into_iter().enumerate() {
                    if kind == ArrKind::Long {
                        v = to_i64(v);
                    }
                    let _ = t;
                    self.emit(Statement::ArrayStore { arr: Operand::Copy(arr), index: Operand::ConstI32(i as i32), value: v, kind, checked: false });
                }
                (Operand::Copy(arr), Ty::Ref)
            }
            Expr::Comprehension { elem, var, iter, cond, .. } => self.lower_comprehension(elem, var, iter, cond.as_deref()),
            // Map-Literal `[k: v, …]` → map() + put je Paar.
            Expr::MapLit(pairs, _) => {
                let m = self.new_local(Ty::Ref);
                self.local_class.insert(m.0, "$Map".into());
                self.emit(Statement::Call { dest: Some(m), func: "vire_map_new".into(), args: vec![] });
                for (k, v) in pairs {
                    let (ko, kt) = self.lower_expr(k);
                    let (vo, vt) = self.lower_expr(v);
                    let ko = if kt == Ty::Ref { ko } else { to_i64(ko) };
                    let vo = if vt == Ty::Ref { vo } else { to_i64(vo) };
                    self.emit(Statement::Call { dest: None, func: "vire_map_put".into(), args: vec![Operand::Copy(m), ko, vo] });
                }
                (Operand::Copy(m), Ty::Ref)
            }
            // Indexierung `xs[i]` → ArrayLoad (bounds-gecheckt) bzw. vire_list_get.
            Expr::Index { base, index, .. } => {
                let (arr, _) = self.lower_expr(base);
                if self.class_of_operand(&arr).as_deref() == Some("$List") {
                    let (idx, _) = self.lower_expr(index);
                    let d = self.new_local(Ty::I64);
                    self.emit(Statement::Call { dest: Some(d), func: "vire_list_get".into(), args: vec![arr, to_i64(idx)] });
                    return (Operand::Copy(d), Ty::I64);
                }
                let kind = match self.arr_of_operand(&arr) {
                    Some(k) => k,
                    None => {
                        self.errs.push("Index `[]`: kein bekanntes Array (annotieren)".into());
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let (idx, _) = self.lower_expr(index);
                let idx32 = self.to_i32(idx);
                let vty = kind.value_ty();
                let d = self.new_local(vty);
                self.emit(Statement::ArrayLoad { dest: d, arr, index: idx32, kind, checked: true });
                (Operand::Copy(d), vty)
            }
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
            // `xs.len()` auf einem Array → ArrayLen.
            if name == "len" && args.is_empty() && self.arr_of_operand(&obj).is_some() {
                let l = self.array_len_i64(obj);
                return (l, Ty::I64);
            }
            // Methoden auf wachsenden Listen ($List) und Maps ($Map).
            if let Some(sent) = self.class_of_operand(&obj) {
                if sent == "$List" {
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "push" => ("vire_list_push", Ty::Void),
                        "pop" => ("vire_list_pop", Ty::I64),
                        "len" => ("vire_list_len", Ty::I64),
                        "get" => ("vire_list_get", Ty::I64),
                        "set" => ("vire_list_set", Ty::Void),
                        _ => {
                            self.errs.push(format!("List hat keine Methode `{name}`"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Map" {
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "put" => ("vire_map_put", Ty::Void),
                        "get" => ("vire_map_get", Ty::I64),
                        "has" => ("vire_map_has", Ty::I32),
                        "len" => ("vire_map_len", Ty::I64),
                        _ => {
                            self.errs.push(format!("Map hat keine Methode `{name}`"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
            }
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
        // Aufruf eines Lambda-Locals `f(args)` → Rumpf inline (Parameter gebunden).
        if let Some((l, _)) = self.lookup(&name) {
            if let Some((params, body)) = self.local_lambda.get(&l.0).cloned() {
                self.scopes.push(HashMap::new());
                for (p, arg) in params.iter().zip(args) {
                    let (op, ty) = self.lower_expr(arg);
                    let d = self.new_local(ty);
                    if let Some(c) = self.class_of_operand(&op) {
                        self.local_class.insert(d.0, c);
                    }
                    self.emit(Statement::Assign(d, Rvalue::Use(op)));
                    self.bind(p, d, ty);
                }
                let r = self.lower_expr(&body);
                self.scopes.pop();
                return r;
            }
        }
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
        // Dimensionierte typisierte Arrays: `array(n)` (Int), `farray(n)` (Float) —
        // echte bounds-gecheckte/-elidierbare Arrays (im Gegensatz zur i64-Liste).
        if name == "array" || name == "farray" {
            let kind = if name == "farray" { ArrKind::Double } else { ArrKind::Long };
            let n = lowered.into_iter().next().map(|(o, _)| o).unwrap_or(Operand::ConstI64(0));
            let len32 = self.to_i32(n);
            let arr = self.new_local(Ty::Ref);
            self.local_arr.insert(arr.0, kind);
            self.emit(Statement::NewArray { dest: arr, kind, len: len32 });
            return (Operand::Copy(arr), Ty::Ref);
        }
        // Collection-Builtins: `list()` (wachsende Liste), `map()` (Int→Int).
        if name == "list" || name == "map" {
            let (func, sentinel) = if name == "list" { ("vire_list_new", "$List") } else { ("vire_map_new", "$Map") };
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, sentinel.into());
            self.emit(Statement::Call { dest: Some(d), func: func.into(), args: vec![] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // Builtin `str(x)` → Text-Repräsentation (Ref).
        if name == "str" {
            let (op, ty) = lowered.into_iter().next().unwrap_or((Operand::ConstNull, Ty::Ref));
            return (self.to_str(op, ty), Ty::Ref);
        }
        // FFI-Builtin `cstr(s)` → NUL-terminierter char* (als Ptr/i64).
        if name == "cstr" {
            let arg = lowered.into_iter().next().map(|(o, _)| o).unwrap_or(Operand::ConstNull);
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "vire_cstr".into(), args: vec![arg] });
            return (Operand::Copy(d), Ty::I64);
        }
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
        // Aufruf einer generischen Funktion → Typargumente aus den Argumenttypen
        // binden, Monomorph.-Instanz `f$T…` anfordern, Aufruf auf die Instanz.
        if let Some(g) = self.generics.get(&name).cloned() {
            let mut bind: HashMap<String, String> = HashMap::new();
            for (i, pty) in g.param_tys.iter().enumerate() {
                if let Some(t) = pty {
                    if g.tparams.contains(&t.name) {
                        if let Some((op, ty)) = lowered.get(i) {
                            let cls = self.class_of_operand(op);
                            bind.entry(t.name.clone()).or_insert_with(|| concrete_tyname(*ty, cls.as_ref()));
                        }
                    }
                }
            }
            let targs: Vec<String> = g.tparams.iter().map(|tp| bind.get(tp).cloned().unwrap_or_else(|| "Int".into())).collect();
            let sym = mono_sym(&name, &targs);
            self.mono.push((name.clone(), targs.clone()));
            let ret = g.ret.as_ref().map(|t| ty_of(Some(&subst_type(t, &bind)))).unwrap_or(Ty::Void);
            let ret_class = g.ret.as_ref().and_then(|t| class_of(Some(&subst_type(t, &bind))));
            let arg_ops: Vec<Operand> = lowered.into_iter().map(|(o, _)| o).collect();
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
        // Aufruf einer eigenen Funktion
        let (ret, ret_class) = self.sigs.get(&name).map(|s| (s.ret, s.ret_class.clone())).unwrap_or((Ty::I64, None));
        // Bequemlichkeit: an `py_*`-Brückenfunktionen werden String-Argumente
        // automatisch zu C-Strings (`cstr`), damit man `py_import("math")` statt
        // `py_import(cstr("math"))` schreiben kann.
        let auto_cstr = name.starts_with("py_");
        let arg_ops: Vec<Operand> = lowered
            .into_iter()
            .map(|(o, t)| {
                if auto_cstr && t == Ty::Ref {
                    let d = self.new_local(Ty::I64);
                    self.emit(Statement::Call { dest: Some(d), func: "vire_cstr".into(), args: vec![o] });
                    Operand::Copy(d)
                } else {
                    o
                }
            })
            .collect();
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

    /// List-Comprehension `[elem for var in src (if cond)]` → Zwei-Pass:
    /// zählen (mit Filter) → Ergebnis-Array allozieren → füllen.
    fn lower_comprehension(&mut self, elem: &Expr, var: &str, iter: &Expr, cond: Option<&Expr>) -> (Operand, Ty) {
        let (src, _) = self.lower_expr(iter);
        let src_kind = match self.arr_of_operand(&src) {
            Some(k) => k,
            None => {
                self.errs.push("Comprehension: Quelle ist keine Liste".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        let src_vty = src_kind.value_ty();
        // Elementart des Ergebnisses: elem in einem toten Block proben.
        let elem_kind = {
            let saved = self.cur;
            let dead = self.new_block();
            self.cur = dead.0 as usize;
            self.scopes.push(HashMap::new());
            let pv = self.new_local(src_vty);
            self.bind(var, pv, src_vty);
            let (_, ety) = self.lower_expr(elem);
            self.scopes.pop();
            self.cur = saved;
            arrkind_of(ety)
        };
        // Pass 1: zählen.
        let count = self.new_local(Ty::I64);
        self.emit(Statement::Assign(count, Rvalue::Use(Operand::ConstI64(0))));
        self.comp_loop(src.clone(), src_kind, var, src_vty, cond, &mut |s, _elem_local| {
            s.emit(Statement::Assign(count, Rvalue::Binary(IB::Add, Operand::Copy(count), Operand::ConstI64(1))));
        });
        // Ergebnis-Array allozieren.
        let count32 = self.to_i32(Operand::Copy(count));
        let res = self.new_local(Ty::Ref);
        self.local_arr.insert(res.0, elem_kind);
        self.emit(Statement::NewArray { dest: res, kind: elem_kind, len: count32 });
        // Pass 2: füllen (elem auswerten, an Position j schreiben).
        let j = self.new_local(Ty::I64);
        self.emit(Statement::Assign(j, Rvalue::Use(Operand::ConstI64(0))));
        let elem_c = elem.clone();
        self.comp_loop(src, src_kind, var, src_vty, cond, &mut |s, _elem_local| {
            let (mut v, _) = s.lower_expr(&elem_c);
            if elem_kind == ArrKind::Long {
                v = to_i64(v);
            }
            let j32 = s.to_i32(Operand::Copy(j));
            s.emit(Statement::ArrayStore { arr: Operand::Copy(res), index: j32, value: v, kind: elem_kind, checked: true });
            s.emit(Statement::Assign(j, Rvalue::Binary(IB::Add, Operand::Copy(j), Operand::ConstI64(1))));
        });
        (Operand::Copy(res), Ty::Ref)
    }

    /// Emittiert `for var in src { if cond { body } }` — Schleifengerüst für
    /// Comprehensions. `body` wird im Rumpf (nach optionalem cond-Filter) gerufen.
    fn comp_loop(&mut self, src: Operand, kind: ArrKind, var: &str, vty: Ty, cond: Option<&Expr>, body: &mut dyn FnMut(&mut Self, Local)) {
        let len = self.array_len_i64(src.clone());
        let ivar = self.new_local(Ty::I64);
        self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
        let header = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Goto(header));
        self.cur = header.0 as usize;
        let c = self.new_local(Ty::I32);
        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), len)));
        let bodyb = self.new_block();
        let latch = self.new_block();
        let exit = self.new_block();
        self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(c), then_blk: bodyb, else_blk: exit });
        self.cur = bodyb.0 as usize;
        let elem = self.new_local(vty);
        let idx32 = self.to_i32(Operand::Copy(ivar));
        self.emit(Statement::ArrayLoad { dest: elem, arr: src, index: idx32, kind, checked: true });
        self.scopes.push(HashMap::new());
        self.bind(var, elem, vty);
        // optionaler Filter
        if let Some(cnd) = cond {
            let (cv, _) = self.lower_expr(cnd);
            let keep = self.new_block();
            self.term(self.cur, Terminator::Branch { cond: cv, then_blk: keep, else_blk: latch });
            self.cur = keep.0 as usize;
        }
        body(self, elem);
        self.scopes.pop();
        self.term(self.cur, Terminator::Goto(latch));
        self.cur = latch.0 as usize;
        self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
        self.term(latch.0 as usize, Terminator::Goto(header));
        self.cur = exit.0 as usize;
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
        let (obj, oty) = self.lower_expr(scrut);
        let class = self.class_of_operand(&obj);
        // Oder-Muster auf Arm-Ebene auffalten: `A | B -> body` → zwei Arme.
        let mut flat: Vec<(Pattern, Option<Expr>, Expr)> = Vec::new();
        for (pat, guard, body) in arms {
            match pat {
                Pattern::Or(ps, _) => {
                    for p in ps {
                        flat.push((p.clone(), guard.clone(), body.clone()));
                    }
                }
                _ => flat.push((pat.clone(), guard.clone(), body.clone())),
            }
        }
        // Erschöpfungsprüfung (Compilezeit): nicht-erschöpfend = HARTER FEHLER.
        self.check_exhaustive(&class, &flat);
        let merge = self.new_block();
        let mut ends: Vec<(usize, Operand, Ty)> = Vec::new();
        for (pat, guard, body) in &flat {
            let fail = self.new_block();
            self.scopes.push(HashMap::new());
            self.emit_pattern_test(obj.clone(), oty, class.clone(), pat, fail);
            // Guard nach erfolgreichem Muster.
            if let Some(g) = guard {
                let (gc, _) = self.lower_expr(g);
                let cont = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Branch { cond: gc, then_blk: cont, else_blk: fail });
                self.cur = cont.0 as usize;
            }
            let (v, t) = self.lower_expr(body);
            self.scopes.pop();
            ends.push((self.cur, v, t));
            self.cur = fail.0 as usize; // nächster Arm beginnt im fail-Block
        }
        let rty = ends.iter().map(|(_, _, t)| *t).find(|t| *t != Ty::Void).unwrap_or(Ty::Void);
        let res = if rty != Ty::Void { Some(self.new_local(rty)) } else { None };
        for (end, v, _) in &ends {
            if let Some(r) = res {
                self.blocks[*end].statements.push(Statement::Assign(r, Rvalue::Use(v.clone())));
            }
            self.blocks[*end].terminator = Terminator::Goto(merge);
        }
        // Fallthrough (unerreichbar, weil erschöpfend geprüft) typkorrekt schließen.
        let cur = self.cur;
        if let Some(r) = res {
            self.blocks[cur].statements.push(Statement::Assign(r, Rvalue::Use(zero_of(rty))));
        }
        self.term(cur, Terminator::Goto(merge));
        self.cur = merge.0 as usize;
        match res {
            Some(r) => (Operand::Copy(r), rty),
            None => (Operand::ConstI64(0), Ty::Void),
        }
    }

    /// Emittiert die Tests für EIN Muster gegen `obj`. Bei Nicht-Übereinstimmung
    /// → `fail`; bei Übereinstimmung läuft `self.cur` weiter (mit Bindungen im
    /// Scope). Rekursiv für verschachtelte Muster.
    fn emit_pattern_test(&mut self, obj: Operand, ty: Ty, class: Option<String>, pat: &Pattern, fail: Block) {
        match pat {
            Pattern::Wildcard(_) => {}
            Pattern::Bind(name, _) => {
                let l = match &obj {
                    Operand::Copy(l) => *l,
                    _ => {
                        let d = self.new_local(ty);
                        self.emit(Statement::Assign(d, Rvalue::Use(obj.clone())));
                        d
                    }
                };
                if let Some(c) = &class {
                    self.local_class.insert(l.0, c.clone());
                }
                self.bind(name, l, ty);
            }
            Pattern::Int(v, _) => self.emit_eq_test(obj, to_i64(Operand::ConstI64(*v as i64)), fail),
            Pattern::Bool(b, _) => self.emit_eq_test(obj, Operand::ConstI32(if *b { 1 } else { 0 }), fail),
            Pattern::Ctor { name, args, .. } => {
                let (sum, vtag, vfields) = match self.variants.get(name) {
                    Some(v) => v.clone(),
                    None => {
                        self.errs.push(format!("unbekannte Variante `{name}` im match"));
                        return;
                    }
                };
                let tag = self.new_local(Ty::I64);
                self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: sum.clone(), field: "__tag".into() });
                self.emit_eq_test(Operand::Copy(tag), Operand::ConstI64(vtag), fail);
                for (j, argpat) in args.iter().enumerate() {
                    if let Some((fname, fty, rt)) = vfields.get(j).cloned() {
                        let d = self.new_local(fty);
                        if let Some(rc) = &rt {
                            self.local_class.insert(d.0, rc.clone());
                        }
                        self.emit(Statement::GetField { dest: d, obj: obj.clone(), class: sum.clone(), field: fname });
                        self.emit_pattern_test(Operand::Copy(d), fty, rt, argpat, fail);
                    }
                }
            }
            Pattern::Or(ps, _) => {
                // Verschachteltes Oder: der Reihe nach probieren; passt eins → weiter.
                let matched = self.new_block();
                for p in ps {
                    let next = self.new_block();
                    self.emit_pattern_test(obj.clone(), ty, class.clone(), p, next);
                    let cur = self.cur;
                    self.term(cur, Terminator::Goto(matched));
                    self.cur = next.0 as usize;
                }
                let cur = self.cur;
                self.term(cur, Terminator::Goto(fail));
                self.cur = matched.0 as usize;
            }
            _ => self.errs.push("match-Muster: Tupel/String-Muster noch nicht abgesenkt".into()),
        }
    }

    /// `obj == val ? weiter : fail`.
    fn emit_eq_test(&mut self, obj: Operand, val: Operand, fail: Block) {
        let c = self.new_local(Ty::I32);
        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpEq, obj, val)));
        let cont = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Branch { cond: Operand::Copy(c), then_blk: cont, else_blk: fail });
        self.cur = cont.0 as usize;
    }

    /// Compilezeit-Erschöpfungsprüfung. Summentyp: alle Varianten oder `_`/Bind.
    /// Skalar/Literal: `_`/Bind-Zweig nötig. Sonst harter Fehler.
    fn check_exhaustive(&mut self, class: &Option<String>, arms: &[(Pattern, Option<Expr>, Expr)]) {
        let has_catchall = arms.iter().any(|(p, g, _)| g.is_none() && matches!(p, Pattern::Wildcard(_) | Pattern::Bind(..)));
        if has_catchall {
            return;
        }
        if let Some(sum) = class {
            // alle Varianten dieses Summentyps
            let all: Vec<(String, i64)> = self
                .variants
                .iter()
                .filter(|(_, (s, _, _))| s == sum)
                .map(|(n, (_, t, _))| (n.clone(), *t))
                .collect();
            if all.is_empty() {
                return; // kein Summentyp (z.B. Produkttyp) → keine Prüfung
            }
            let mut covered = std::collections::HashSet::new();
            for (p, g, _) in arms {
                if g.is_some() {
                    continue; // Guard kann fehlschlagen → deckt nicht sicher
                }
                if let Pattern::Ctor { name, args, .. } = p {
                    // deckt den Tag nur, wenn alle Argumente unwiderlegbar sind
                    if args.iter().all(is_irrefutable) {
                        if let Some((_, t, _)) = self.variants.get(name) {
                            covered.insert(*t);
                        }
                    }
                }
            }
            let missing: Vec<&str> = all.iter().filter(|(_, t)| !covered.contains(t)).map(|(n, _)| n.as_str()).collect();
            if !missing.is_empty() {
                self.errs.push(format!("nicht-erschöpfendes `match`: fehlt {} (oder `_`-Zweig)", missing.join(", ")));
            }
        } else {
            self.errs.push("`match` über Skalar/Literal braucht einen `_`-Zweig (nicht-erschöpfend)".into());
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

#[allow(clippy::too_many_arguments)]
fn lower_fn(
    f: &FnDef,
    sigs: &HashMap<String, Sig>,
    types: &HashMap<String, Layout>,
    variants: &HashMap<String, VariantInfo>,
    generics: &HashMap<String, GInfo>,
    strings: &mut Vec<String>,
    recv_class: Option<&str>,
    sym: Option<&str>,
) -> Result<(Function, Vec<(String, Vec<String>)>), Vec<String>> {
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
        generics,
        mono: Vec::new(),
        local_class: HashMap::new(),
        local_arr: HashMap::new(),
        local_lambda: HashMap::new(),
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
    let mono = fl.mono;
    Ok((
        Function {
            name,
            params: param_tys,
            ret,
            locals: fl.locals,
            blocks: fl.blocks,
            receiver_nonnull: false,
        },
        mono,
    ))
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

/// Compilezeit-Konstantenwert (für `comptime`).
enum CVal {
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Faltet einen konstanten Ausdruck zur Compilezeit. `None` = nicht konstant.
fn const_eval(e: &Expr) -> Option<CVal> {
    Some(match e {
        Expr::Int(v, _) => CVal::Int(*v as i64),
        Expr::Float(v, _) => CVal::Float(*v),
        Expr::Bool(b, _) => CVal::Bool(*b),
        Expr::Comptime { inner, .. } => return const_eval(inner),
        Expr::Unary { op, rhs, .. } => match (op, const_eval(rhs)?) {
            (UnOp::Neg, CVal::Int(v)) => CVal::Int(-v),
            (UnOp::Neg, CVal::Float(v)) => CVal::Float(-v),
            (UnOp::Not, CVal::Bool(b)) => CVal::Bool(!b),
            _ => return None,
        },
        Expr::Binary { op, lhs, rhs, .. } => {
            let (l, r) = (const_eval(lhs)?, const_eval(rhs)?);
            match (l, r) {
                (CVal::Int(a), CVal::Int(b)) => match op {
                    BinOp::Add | BinOp::AddWrap => CVal::Int(a.wrapping_add(b)),
                    BinOp::Sub | BinOp::SubWrap => CVal::Int(a.wrapping_sub(b)),
                    BinOp::Mul | BinOp::MulWrap => CVal::Int(a.wrapping_mul(b)),
                    BinOp::Div if b != 0 => CVal::Int(a / b),
                    BinOp::Rem if b != 0 => CVal::Int(a % b),
                    BinOp::Eq => CVal::Bool(a == b),
                    BinOp::Ne => CVal::Bool(a != b),
                    BinOp::Lt => CVal::Bool(a < b),
                    BinOp::Le => CVal::Bool(a <= b),
                    BinOp::Gt => CVal::Bool(a > b),
                    BinOp::Ge => CVal::Bool(a >= b),
                    BinOp::BitAnd => CVal::Int(a & b),
                    BinOp::BitOr => CVal::Int(a | b),
                    BinOp::BitXor => CVal::Int(a ^ b),
                    BinOp::Shl => CVal::Int(a << b),
                    BinOp::Shr => CVal::Int(a >> b),
                    _ => return None,
                },
                (CVal::Float(a), CVal::Float(b)) => match op {
                    BinOp::Add => CVal::Float(a + b),
                    BinOp::Sub => CVal::Float(a - b),
                    BinOp::Mul => CVal::Float(a * b),
                    BinOp::Div => CVal::Float(a / b),
                    BinOp::Lt => CVal::Bool(a < b),
                    BinOp::Le => CVal::Bool(a <= b),
                    BinOp::Gt => CVal::Bool(a > b),
                    BinOp::Ge => CVal::Bool(a >= b),
                    _ => return None,
                },
                (CVal::Bool(a), CVal::Bool(b)) => match op {
                    BinOp::And => CVal::Bool(a && b),
                    BinOp::Or => CVal::Bool(a || b),
                    BinOp::Eq => CVal::Bool(a == b),
                    BinOp::Ne => CVal::Bool(a != b),
                    _ => return None,
                },
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// Muster, das immer passt (deckt seinen Slot vollständig ab).
fn is_irrefutable(p: &Pattern) -> bool {
    matches!(p, Pattern::Wildcard(_) | Pattern::Bind(..))
}

/// IR-Wertetyp → Array-Elementart.
fn arrkind_of(t: Ty) -> ArrKind {
    match t {
        Ty::F64 => ArrKind::Double,
        Ty::F32 => ArrKind::Float,
        Ty::I32 => ArrKind::Int,
        Ty::Ref => ArrKind::Ref,
        _ => ArrKind::Long,
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
