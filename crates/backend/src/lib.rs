//! Naive Absenkung Mittel-IR → textuelles LLVM-IR.
//!
//! Bewusst dumm gehalten: jedes IR-Local wird ein `alloca`, jeder Zugriff
//! ein Load/Store — LLVMs mem2reg/SROA stellt SSA wieder her. Textuelle
//! `.ll`-Ausgabe statt API-Bindings, weil llvm-sys/inkwell dem
//! installierten LLVM 22 hinterherhinken.
//!
//! Objektmodell (Stufe 2):
//! - `%class.C = type { ptr, felder… }` — Slot 0 ist der Vtable-Zeiger;
//!   Superklassen-Felder liegen vor den eigenen, dadurch sind GEP-Indizes
//!   über die ganze Subklassen-Hierarchie stabil (Prefix-Layout).
//! - Vtable-Slots: geerbte Slots zuerst (Overrides ersetzen in place),
//!   neue virtuelle Methoden in Deklarationsreihenfolge dahinter.
//! - getfield/putfield/invokevirtual prüfen den Receiver auf null
//!   (Java-Semantik; HotSpots Segfault-Trick wäre Runtime, DESIGN.md §6).
//!
//! Java-Semantik-Punkte:
//! - idiv/irem via Runtime-Helfer (Exception bei /0, MIN/-1 definiert)
//! - Shift-Betrag wird mit &31 maskiert (JLS 15.19)
//! - Addition etc. wrappen (LLVM add ohne nsw/nuw wrappt bereits)

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use fastllvm_ir::*;

fn llty(ty: Ty) -> &'static str {
    match ty {
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::Ref => "ptr",
        Ty::Void => "void",
    }
}

/// Intrinsics und Runtime-Helfer, die die Mini-Runtime (runtime.c) definiert.
const RUNTIME_DECLS: &[(&str, &str)] = &[
    ("jrt_println_str", "void (ptr)"),
    ("jrt_println_int", "void (i32)"),
    ("jrt_println_ln", "void ()"),
    ("jrt_print_str", "void (ptr)"),
    ("jrt_print_int", "void (i32)"),
    ("jrt_idiv", "i32 (i32, i32)"),
    ("jrt_irem", "i32 (i32, i32)"),
    ("jrt_alloc", "ptr (i64)"),
    ("jrt_null_check", "void (ptr)"),
    ("jrt_retain", "void (ptr)"),
    ("jrt_release", "void (ptr)"),
];

/// Feste Header-Slots vor den Instanzfeldern: refcount (i64) + vtable (ptr).
/// Instanzfelder beginnen daher bei GEP-Index 2, der Vtable-Zeiger bei 1.
const HEADER_SLOTS: usize = 2;
/// Vtable-Slot 0 ist die Drop-Funktion; virtuelle Methoden ab Slot 1.
const VTABLE_DROP_SLOTS: usize = 1;

/// Klassen-Kontext: Layouts und Vtables, aus `Program::classes` berechnet.
struct Ctx<'a> {
    program: &'a Program,
}

impl<'a> Ctx<'a> {
    fn class(&self, name: &str) -> Option<&'a ClassInfo> {
        self.program.class(name)
    }

    fn struct_name(&self, class: &str) -> String {
        format!("%class.{}", sanitize(class))
    }

    /// Instanzfelder in Layout-Reihenfolge: Superklassen zuerst.
    fn flatten_fields(&self, class: &str) -> Vec<(String, String, Ty)> {
        let Some(ci) = self.class(class) else { return Vec::new() };
        let mut out = match &ci.super_name {
            Some(s) => self.flatten_fields(s),
            None => Vec::new(),
        };
        for f in &ci.fields {
            out.push((ci.name.clone(), f.name.clone(), f.ty));
        }
        out
    }

    /// GEP-Index (nach dem Header) und Typ eines Felds, aufgelöst
    /// ab `class` die Superkette hoch.
    fn field_slot(&self, class: &str, field: &str) -> Option<(String, usize, Ty)> {
        let (owner, ty) = self.program.resolve_field(class, field)?;
        let owner = owner.to_string();
        let flat = self.flatten_fields(&owner);
        let idx = flat.iter().position(|(o, n, _)| *o == owner && n == field)?;
        Some((owner, idx + HEADER_SLOTS, ty))
    }

    /// Ref-Felder von `class` (inkl. geerbter) als GEP-Index-Liste — für
    /// die generierte Drop-Funktion.
    fn ref_field_slots(&self, class: &str) -> Vec<usize> {
        self.flatten_fields(class)
            .iter()
            .enumerate()
            .filter(|(_, (_, _, t))| *t == Ty::Ref)
            .map(|(i, _)| i + HEADER_SLOTS)
            .collect()
    }

    /// Vtable-Slots von `class`: (name, desc, Implementierungs-Symbol).
    fn vtable_slots(&self, class: &str) -> Vec<(String, String, Option<String>)> {
        let Some(ci) = self.class(class) else { return Vec::new() };
        let mut slots = match &ci.super_name {
            Some(s) => self.vtable_slots(s),
            None => Vec::new(),
        };
        for m in &ci.methods {
            if !m.is_virtual() {
                continue;
            }
            let impl_sym = m.has_body.then(|| m.mangled.clone());
            if let Some(slot) = slots.iter_mut().find(|(n, d, _)| *n == m.name && *d == m.desc) {
                slot.2 = impl_sym;
            } else {
                slots.push((m.name.clone(), m.desc.clone(), impl_sym));
            }
        }
        slots
    }

    /// GEP-Index eines Methoden-Slots in der Vtable (nach dem Drop-Slot).
    fn vtable_index(&self, class: &str, name: &str, desc: &str) -> Option<usize> {
        self.vtable_slots(class)
            .iter()
            .position(|(n, d, _)| n == name && d == desc)
            .map(|i| i + VTABLE_DROP_SLOTS)
    }
}

pub fn emit(program: &Program) -> String {
    let mut out = String::new();
    let w = &mut out;
    let ctx = Ctx { program };

    writeln!(w, "; erzeugt von fastllvm (naive Absenkung, siehe DESIGN.md)").unwrap();

    // String-Literale: immortaler Header (refcount -1) + Länge + Bytes.
    // Refcount -1 macht retain/release zu No-Ops (Read-only-Konstante bleibt
    // unberührt), sodass Literale wie normale Referenzen fließen können.
    for (i, s) in program.strings.iter().enumerate() {
        let bytes = s.as_bytes();
        writeln!(
            w,
            "@jstr.{i} = private unnamed_addr constant {{ i64, i64, [{n} x i8] }} {{ i64 -1, i64 {n}, [{n} x i8] c\"{esc}\" }}",
            n = bytes.len(),
            esc = escape_ll(bytes),
        )
        .unwrap();
    }
    // Class-Objekt-Singletons (Reflection): immortaler Header + Namens-String.
    // Pointer-Identität ersetzt Javas Class-Gleichheit.
    for (class, sid) in &program.class_objects {
        writeln!(
            w,
            "@jclass.{} = internal unnamed_addr constant {{ i64, ptr }} {{ i64 -1, ptr @jstr.{sid} }}",
            sanitize(class),
        )
        .unwrap();
    }
    writeln!(w).unwrap();

    // Struct-Typen: { i64 refcount, ptr vtable, felder… }.
    for c in &program.classes {
        let mut parts = vec!["i64".to_string(), "ptr".to_string()];
        parts.extend(ctx.flatten_fields(&c.name).iter().map(|(_, _, t)| llty(*t).to_string()));
        writeln!(w, "{} = type {{ {} }}", ctx.struct_name(&c.name), parts.join(", ")).unwrap();
    }
    writeln!(w).unwrap();

    let defined: BTreeSet<&str> = program.functions.iter().map(|f| f.name.as_str()).collect();

    // Vtables für instanziierte Klassen. Slots, deren Implementierung dem
    // Pruning zum Opfer fiel (RTA-tot), werden null — kein erreichbarer
    // Site kann dorthin dispatchen.
    let instantiated: BTreeSet<&str> = program
        .functions
        .iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.statements)
        .filter_map(|st| match st {
            Statement::New { class, .. } | Statement::StackNew { class, .. } => Some(class.as_str()),
            _ => None,
        })
        .collect();
    for class in &instantiated {
        let slots = ctx.vtable_slots(class);
        // Slot 0: Drop-Funktion der Klasse; danach die Methoden-Slots.
        let mut entries = vec![format!("ptr @drop.{}", sanitize(class))];
        entries.extend(slots.iter().map(|(_, _, sym)| match sym {
            Some(s) if defined.contains(s.as_str()) => format!("ptr @{s}"),
            _ => "ptr null".to_string(),
        }));
        writeln!(
            w,
            "@vt.{} = internal unnamed_addr constant [{} x ptr] [{}]",
            sanitize(class),
            entries.len(),
            entries.join(", "),
        )
        .unwrap();
    }
    writeln!(w).unwrap();

    // Drop-Funktionen: released die Ref-Felder des Objekts (nicht rekursiv
    // im Code — die Runtime ruft jrt_release, das ggf. weiter absteigt).
    for class in &instantiated {
        writeln!(w, "define internal void @drop.{}(ptr %o) {{", sanitize(class)).unwrap();
        for (k, slot) in ctx.ref_field_slots(class).into_iter().enumerate() {
            writeln!(w, "  %f{k} = getelementptr {}, ptr %o, i32 0, i32 {slot}", ctx.struct_name(class)).unwrap();
            writeln!(w, "  %v{k} = load ptr, ptr %f{k}").unwrap();
            writeln!(w, "  call void @jrt_release(ptr %v{k})").unwrap();
        }
        writeln!(w, "  ret void").unwrap();
        writeln!(w, "}}").unwrap();
    }
    writeln!(w).unwrap();

    for (name, sig) in RUNTIME_DECLS {
        let (ret, params) = sig.split_once(' ').unwrap();
        writeln!(w, "declare {ret} @{name}{params}").unwrap();
    }

    // Aufgerufene, aber nicht definierte Funktionen deklarieren.
    let mut external: BTreeMap<&str, (Ty, Vec<Ty>)> = BTreeMap::new();
    for f in &program.functions {
        for bb in &f.blocks {
            for st in &bb.statements {
                if let Statement::Call { dest, func, args } = st {
                    if defined.contains(func.as_str())
                        || RUNTIME_DECLS.iter().any(|(n, _)| n == func)
                    {
                        continue;
                    }
                    let ret = dest.map(|l| f.locals[l.0 as usize]).unwrap_or(Ty::Void);
                    let atys = args.iter().map(|a| operand_ty(f, a)).collect();
                    external.insert(func, (ret, atys));
                }
            }
        }
    }
    for (name, (ret, atys)) in &external {
        let ps: Vec<&str> = atys.iter().map(|t| llty(*t)).collect();
        writeln!(w, "declare {} @{}({})", llty(*ret), name, ps.join(", ")).unwrap();
    }
    writeln!(w).unwrap();

    for f in &program.functions {
        emit_function(w, &ctx, f);
    }

    if defined.contains("java_main") {
        writeln!(w, "define i32 @main() {{").unwrap();
        writeln!(w, "  call void @java_main()").unwrap();
        writeln!(w, "  ret i32 0").unwrap();
        writeln!(w, "}}").unwrap();
    }

    out
}

fn operand_ty(f: &Function, op: &Operand) -> Ty {
    match op {
        Operand::Copy(l) => f.locals[l.0 as usize],
        Operand::ConstI32(_) => Ty::I32,
        Operand::ConstI64(_) => Ty::I64,
        Operand::ConstStr(_) | Operand::ConstClass(_) | Operand::ConstNull => Ty::Ref,
    }
}

struct FnEmitter<'a> {
    f: &'a Function,
    tmp: u32,
}

impl<'a> FnEmitter<'a> {
    fn fresh(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    /// Materialisiert einen Operanden als SSA-Wert; Locals werden geladen.
    fn operand(&mut self, w: &mut String, op: &Operand) -> String {
        match op {
            Operand::Copy(l) => {
                let ty = llty(self.f.locals[l.0 as usize]);
                let t = self.fresh();
                writeln!(w, "  {t} = load {ty}, ptr %l{}", l.0).unwrap();
                t
            }
            Operand::ConstI32(v) => v.to_string(),
            // ConstI64(0) dient dem Frontend als Ref-Dummy (System.out);
            // in Ref-Kontexten muss daraus null werden.
            Operand::ConstI64(0) => "null".to_string(),
            Operand::ConstI64(v) => v.to_string(),
            Operand::ConstStr(i) => format!("@jstr.{i}"),
            Operand::ConstClass(c) => format!("@jclass.{}", sanitize(c)),
            Operand::ConstNull => "null".to_string(),
        }
    }
}

fn emit_function(w: &mut String, ctx: &Ctx, f: &Function) {
    let ps: Vec<String> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{} %p{i}", llty(*t)))
        .collect();
    writeln!(w, "define {} @{}({}) {{", llty(f.ret), f.name, ps.join(", ")).unwrap();

    writeln!(w, "entry:").unwrap();
    for (i, ty) in f.locals.iter().enumerate() {
        writeln!(w, "  %l{i} = alloca {}", llty(*ty)).unwrap();
    }
    // Ref-Locals müssen vor dem ersten (Cleanup-)Load null sein, damit das
    // Massen-Release am Funktionsende keinen Garbage dereferenziert.
    let n_params = f.params.len();
    for (i, ty) in f.locals.iter().enumerate() {
        if *ty == Ty::Ref && i >= n_params {
            writeln!(w, "  store ptr null, ptr %l{i}").unwrap();
        }
    }
    for (i, ty) in f.params.iter().enumerate() {
        writeln!(w, "  store {} %p{i}, ptr %l{i}", llty(*ty)).unwrap();
        // Ref-Parameter sind geborgt; retain macht sie zu owned, sodass das
        // Cleanup sie uniform releasen darf (Aufrufer behält seine Referenz).
        if *ty == Ty::Ref {
            writeln!(w, "  call void @jrt_retain(ptr %p{i})").unwrap();
        }
    }
    writeln!(w, "  br label %bb0").unwrap();

    let mut e = FnEmitter { f, tmp: 0 };

    for (bi, bb) in f.blocks.iter().enumerate() {
        writeln!(w, "bb{bi}:").unwrap();
        for st in &bb.statements {
            emit_statement(w, ctx, &mut e, st);
        }
        match &bb.terminator {
            Terminator::Goto(b) => writeln!(w, "  br label %bb{}", b.0).unwrap(),
            Terminator::Branch { cond, then_blk, else_blk } => {
                let c = e.operand(w, cond);
                let b = e.fresh();
                writeln!(w, "  {b} = icmp ne i32 {c}, 0").unwrap();
                writeln!(w, "  br i1 {b}, label %bb{}, label %bb{}", then_blk.0, else_blk.0).unwrap();
            }
            Terminator::Return(None) => {
                emit_cleanup(w, &mut e);
                writeln!(w, "  ret void").unwrap();
            }
            Terminator::Return(Some(op)) => {
                let ty = operand_ty(f, op);
                let v = e.operand(w, op);
                // Rückgabe-Ref muss das Cleanup überleben → retain, dann
                // transferiert der Aufrufer die +1.
                if ty == Ty::Ref {
                    writeln!(w, "  call void @jrt_retain(ptr {v})").unwrap();
                }
                emit_cleanup(w, &mut e);
                writeln!(w, "  ret {} {v}", llty(ty)).unwrap();
            }
        }
    }
    writeln!(w, "}}\n").unwrap();
}

/// Released alle Ref-Locals der Funktion (Owning-Slot-Modell): jedes
/// Ref-Local hält eine Referenz, die beim Verlassen der Funktion endet.
fn emit_cleanup(w: &mut String, e: &mut FnEmitter) {
    for (i, ty) in e.f.locals.iter().enumerate() {
        if *ty == Ty::Ref {
            let t = e.fresh();
            writeln!(w, "  {t} = load ptr, ptr %l{i}").unwrap();
            writeln!(w, "  call void @jrt_release(ptr {t})").unwrap();
        }
    }
}

fn emit_statement(w: &mut String, ctx: &Ctx, e: &mut FnEmitter, st: &Statement) {
    match st {
        Statement::Assign(dest, rv) => {
            let dty = llty(e.f.locals[dest.0 as usize]);
            let val = match rv {
                Rvalue::Use(op) => e.operand(w, op),
                Rvalue::Neg(op) => {
                    let v = e.operand(w, op);
                    let t = e.fresh();
                    writeln!(w, "  {t} = sub i32 0, {v}").unwrap();
                    t
                }
                Rvalue::Binary(op, a, b) => {
                    let aty = operand_ty(e.f, a);
                    let av = e.operand(w, a);
                    let bv = e.operand(w, b);
                    emit_binop(w, e, *op, aty, &av, &bv)
                }
            };
            let _ = dty;
            // Kopien/Konstanten ins Ref-Local sind geborgt → retain der neue,
            // release der alte (store_dest). Nicht-Ref: schlichter store.
            store_dest(w, e, *dest, &val, true);
        }
        Statement::Call { dest, func, args } => {
            let avs = call_args(w, e, args);
            match dest {
                None => writeln!(w, "  call void @{func}({avs})").unwrap(),
                Some(d) => {
                    let rty = llty(e.f.locals[d.0 as usize]);
                    let t = e.fresh();
                    writeln!(w, "  {t} = call {rty} @{func}({avs})").unwrap();
                    // Ref-Rückgabe transferiert +1 (kein retain).
                    store_dest(w, e, *d, &t, false);
                }
            }
        }
        Statement::CallVirtual { dest, class, name, desc, params, ret, args } => {
            let slot = ctx
                .vtable_index(class, name, desc)
                .unwrap_or_else(|| panic!("Vtable-Slot {class}.{name}{desc} fehlt"));
            let recv = e.operand(w, &args[0]);
            writeln!(w, "  call void @jrt_null_check(ptr {recv})").unwrap();
            // Vtable liegt im Header-Slot 1 (hinter dem refcount).
            let vtp = e.fresh();
            writeln!(w, "  {vtp} = getelementptr ptr, ptr {recv}, i64 1").unwrap();
            let vt = e.fresh();
            writeln!(w, "  {vt} = load ptr, ptr {vtp}").unwrap();
            let slotp = e.fresh();
            writeln!(w, "  {slotp} = getelementptr ptr, ptr {vt}, i64 {slot}").unwrap();
            let fnp = e.fresh();
            writeln!(w, "  {fnp} = load ptr, ptr {slotp}").unwrap();
            // Receiver wurde schon materialisiert; restliche Argumente jetzt.
            let mut avs = vec![format!("ptr {recv}")];
            for a in &args[1..] {
                let ty = llty(operand_ty(e.f, a));
                let v = e.operand(w, a);
                avs.push(format!("{ty} {v}"));
            }
            let _ = params;
            match dest {
                None => writeln!(w, "  call {} {fnp}({})", llty(*ret), avs.join(", ")).unwrap(),
                Some(d) => {
                    let t = e.fresh();
                    writeln!(w, "  {t} = call {} {fnp}({})", llty(*ret), avs.join(", ")).unwrap();
                    // Ref-Rückgabe transferiert +1 (kein retain).
                    store_dest(w, e, *d, &t, false);
                }
            }
        }
        Statement::New { dest, class } => {
            let sn = ctx.struct_name(class);
            let t = e.fresh();
            // sizeof über GEP-Konstante; jrt_alloc nullt Felder und setzt
            // refcount=1 (Java-Defaultwerte + erste Referenz).
            writeln!(
                w,
                "  {t} = call ptr @jrt_alloc(i64 ptrtoint (ptr getelementptr ({sn}, ptr null, i32 1) to i64))"
            )
            .unwrap();
            store_vtable(w, e, &t, class);
            store_dest(w, e, *dest, &t, false); // alloc gab +1
        }
        Statement::StackNew { dest, class } => {
            let sn = ctx.struct_name(class);
            let t = e.fresh();
            // Escape-Analyse hat funktions-lokale Lebenszeit bewiesen:
            // alloca statt Heap. refcount=-1 macht das Objekt immortal —
            // retain/release sind No-Ops, es wird nie freigegeben.
            writeln!(w, "  {t} = alloca {sn}").unwrap();
            writeln!(w, "  store {sn} zeroinitializer, ptr {t}").unwrap();
            writeln!(w, "  store i64 -1, ptr {t}").unwrap();
            store_vtable(w, e, &t, class);
            store_dest(w, e, *dest, &t, false);
        }
        Statement::GetField { dest, obj, class, field } => {
            let (owner, idx, ty) = ctx
                .field_slot(class, field)
                .unwrap_or_else(|| panic!("Feld {class}.{field} fehlt"));
            let o = e.operand(w, obj);
            writeln!(w, "  call void @jrt_null_check(ptr {o})").unwrap();
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr {}, ptr {o}, i32 0, i32 {idx}", ctx.struct_name(&owner)).unwrap();
            let t = e.fresh();
            writeln!(w, "  {t} = load {}, ptr {p}", llty(ty)).unwrap();
            // Feldwert ist geborgt; die Kopie ins Local wird owned → retain.
            store_dest(w, e, *dest, &t, true);
        }
        Statement::PutField { obj, class, field, value } => {
            let (owner, idx, ty) = ctx
                .field_slot(class, field)
                .unwrap_or_else(|| panic!("Feld {class}.{field} fehlt"));
            let o = e.operand(w, obj);
            writeln!(w, "  call void @jrt_null_check(ptr {o})").unwrap();
            let v = e.operand(w, value);
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr {}, ptr {o}, i32 0, i32 {idx}", ctx.struct_name(&owner)).unwrap();
            if ty == Ty::Ref {
                // Feld übernimmt eine owning-Referenz: retain neu, release alt.
                writeln!(w, "  call void @jrt_retain(ptr {v})").unwrap();
                let old = e.fresh();
                writeln!(w, "  {old} = load ptr, ptr {p}").unwrap();
                writeln!(w, "  store ptr {v}, ptr {p}").unwrap();
                writeln!(w, "  call void @jrt_release(ptr {old})").unwrap();
            } else {
                writeln!(w, "  store {} {v}, ptr {p}", llty(ty)).unwrap();
            }
        }
    }
}

/// Speichert `val` in den Vtable-Slot des Objektheaders (Slot 1).
fn store_vtable(w: &mut String, e: &mut FnEmitter, obj: &str, class: &str) {
    let vtp = e.fresh();
    writeln!(w, "  {vtp} = getelementptr ptr, ptr {obj}, i64 1").unwrap();
    writeln!(w, "  store ptr @vt.{}, ptr {vtp}", sanitize(class)).unwrap();
    let _ = e;
}

/// Schreibt `val` in ein Local. Für Ref-Locals gilt die Owning-Slot-
/// Disziplin: der alte Wert wird released, der neue ggf. retained
/// (`retain_new`: true bei Kopie/geborgtem Wert, false bei transferierter
/// +1-Referenz aus New/Call).
fn store_dest(w: &mut String, e: &mut FnEmitter, dest: Local, val: &str, retain_new: bool) {
    let ty = e.f.locals[dest.0 as usize];
    if ty != Ty::Ref {
        writeln!(w, "  store {} {val}, ptr %l{}", llty(ty), dest.0).unwrap();
        return;
    }
    let old = e.fresh();
    writeln!(w, "  {old} = load ptr, ptr %l{}", dest.0).unwrap();
    if retain_new {
        writeln!(w, "  call void @jrt_retain(ptr {val})").unwrap();
    }
    writeln!(w, "  store ptr {val}, ptr %l{}", dest.0).unwrap();
    writeln!(w, "  call void @jrt_release(ptr {old})").unwrap();
}

fn call_args(w: &mut String, e: &mut FnEmitter, args: &[Operand]) -> String {
    args.iter()
        .map(|a| {
            let ty = llty(operand_ty(e.f, a));
            let v = e.operand(w, a);
            format!("{ty} {v}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn emit_binop(w: &mut String, e: &mut FnEmitter, op: BinOp, aty: Ty, a: &str, b: &str) -> String {
    // Shift-Beträge maskieren (JLS 15.19): b & 31 vor shl/ashr/lshr,
    // sonst wäre der LLVM-Wert bei b >= 32 poison.
    let masked = |w: &mut String, e: &mut FnEmitter, b: &str| -> String {
        let m = e.fresh();
        writeln!(w, "  {m} = and i32 {b}, 31").unwrap();
        m
    };
    let t = e.fresh();
    match op {
        BinOp::Add => writeln!(w, "  {t} = add i32 {a}, {b}").unwrap(),
        BinOp::Sub => writeln!(w, "  {t} = sub i32 {a}, {b}").unwrap(),
        BinOp::Mul => writeln!(w, "  {t} = mul i32 {a}, {b}").unwrap(),
        BinOp::Div => writeln!(w, "  {t} = call i32 @jrt_idiv(i32 {a}, i32 {b})").unwrap(),
        BinOp::Rem => writeln!(w, "  {t} = call i32 @jrt_irem(i32 {a}, i32 {b})").unwrap(),
        BinOp::And => writeln!(w, "  {t} = and i32 {a}, {b}").unwrap(),
        BinOp::Or => writeln!(w, "  {t} = or i32 {a}, {b}").unwrap(),
        BinOp::Xor => writeln!(w, "  {t} = xor i32 {a}, {b}").unwrap(),
        BinOp::Shl => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = shl i32 {a}, {m}").unwrap();
        }
        BinOp::Shr => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = ashr i32 {a}, {m}").unwrap();
        }
        BinOp::UShr => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = lshr i32 {a}, {m}").unwrap();
        }
        BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe => {
            let cc = match op {
                BinOp::CmpEq => "eq",
                BinOp::CmpNe => "ne",
                BinOp::CmpLt => "slt",
                BinOp::CmpGe => "sge",
                BinOp::CmpGt => "sgt",
                _ => "sle",
            };
            let c = e.fresh();
            // Ref-Vergleiche (ifnull, if_acmpeq) laufen über ptr.
            writeln!(w, "  {c} = icmp {cc} {} {a}, {b}", llty(aty)).unwrap();
            writeln!(w, "  {t} = zext i1 {c} to i32").unwrap();
        }
    }
    t
}

fn escape_ll(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        if b.is_ascii_graphic() && b != b'"' && b != b'\\' {
            s.push(b as char);
        } else {
            s.push_str(&format!("\\{b:02X}"));
        }
    }
    s
}
