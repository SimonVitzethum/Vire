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
        Ty::F64 => "double",
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
    ("jrt_print_char", "void (i32)"),
    ("jrt_println_char", "void (i32)"),
    ("jrt_str_length", "i32 (ptr)"),
    ("jrt_str_is_empty", "i32 (ptr)"),
    ("jrt_str_char_at", "i32 (ptr, i32)"),
    ("jrt_str_equals", "i32 (ptr, ptr)"),
    ("jrt_str_concat", "ptr (ptr, ptr)"),
    ("jrt_int_to_str", "ptr (i32)"),
    ("jrt_long_to_str", "ptr (i64)"),
    ("jrt_char_to_str", "ptr (i32)"),
    ("jrt_bool_to_str", "ptr (i32)"),
    ("jrt_double_to_str", "ptr (double)"),
    ("jrt_idiv", "i32 (i32, i32)"),
    ("jrt_irem", "i32 (i32, i32)"),
    ("jrt_ldiv", "i64 (i64, i64)"),
    ("jrt_lrem", "i64 (i64, i64)"),
    ("jrt_lcmp", "i32 (i64, i64)"),
    ("jrt_dcmpl", "i32 (double, double)"),
    ("jrt_dcmpg", "i32 (double, double)"),
    ("jrt_d2i", "i32 (double)"),
    ("jrt_d2l", "i64 (double)"),
    ("jrt_print_long", "void (i64)"),
    ("jrt_println_long", "void (i64)"),
    ("jrt_print_double", "void (double)"),
    ("jrt_println_double", "void (double)"),
    ("jrt_alloc", "ptr (i64)"),
    ("jrt_null_check", "void (ptr)"),
    ("jrt_retain", "void (ptr)"),
    ("jrt_release", "void (ptr)"),
    ("jrt_throw", "void (ptr)"),
    ("jrt_pending_set", "i32 ()"),
    ("jrt_take_pending", "ptr ()"),
    ("jrt_check_uncaught", "void ()"),
    ("jrt_alloc_array", "ptr (i64, i64, ptr)"),
    ("jrt_bounds_check", "void (ptr, i32)"),
    ("jrt_array_ref_drop", "void (ptr)"),
    ("jrt_array_ref_trace", "void (ptr, ptr)"),
    ("jrt_noop_drop", "void (ptr)"),
    ("jrt_noop_trace", "void (ptr, ptr)"),
];

/// LLVM-Struct eines Arrays: gleicher Header wie Objekte, dann Länge und
/// die (flexibel dimensionierten) Elemente.
fn array_struct(elem: Ty) -> &'static str {
    match elem {
        Ty::Ref => "%arr.ref",
        _ => "%arr.int",
    }
}

fn array_vtable(elem: Ty) -> &'static str {
    match elem {
        Ty::Ref => "@vt.array.ref",
        _ => "@vt.array.int",
    }
}

/// Elementgröße in Bytes (für die Allokation).
fn elem_size(elem: Ty) -> usize {
    match elem {
        Ty::Ref | Ty::I64 => 8,
        _ => 4,
    }
}

/// Feste Header-Slots vor den Instanzfeldern:
///   Slot 0: refcount (i64), <0 = immortal
///   Slot 1: rcflags (i64) — Farbe/Buffered-Bit für den Zyklen-Collector
///   Slot 2: vtable (ptr)
/// Instanzfelder beginnen daher bei GEP-Index 3.
const HEADER_SLOTS: usize = 3;
/// Word-Offset des Vtable-Zeigers im Header (für ptr-getelementptr).
const VTABLE_WORD: usize = 2;
/// Vtable-Slot 0 = Drop-Funktion, Slot 1 = Trace-Funktion (Zyklen-Collector);
/// virtuelle Methoden beginnen ab Slot 2.
const VTABLE_METHOD_OFFSET: usize = 2;

/// Klassen-Kontext: Layouts und Vtables, aus `Program::classes` berechnet.
struct Ctx<'a> {
    program: &'a Program,
    /// Globale Vtable-Slots für aufgerufene Interface-Methoden, damit
    /// dieselbe Interface-Methode in jeder implementierenden Klasse am
    /// selben Slot liegt. Schlüssel: (interface, name, desc).
    iface_slots: Vec<(String, String, String)>,
}

impl<'a> Ctx<'a> {
    fn class(&self, name: &str) -> Option<&'a ClassInfo> {
        self.program.class(name)
    }

    /// Erster Vtable-Slot der klassen-eigenen virtuellen Methoden (hinter
    /// drop, trace und den globalen Interface-Slots).
    fn method_base(&self) -> usize {
        VTABLE_METHOD_OFFSET + self.iface_slots.len()
    }

    fn iface_index(&self, iface: &str, name: &str, desc: &str) -> Option<usize> {
        self.iface_slots
            .iter()
            .position(|(i, n, d)| i == iface && n == name && d == desc)
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

    /// Global-Symbol und Typ eines statischen Feldes (Superkette hoch).
    fn static_field(&self, class: &str, field: &str) -> Option<(String, Ty)> {
        let (owner, ty) = self.program.resolve_static_field(class, field)?;
        Some((format!("@sf.{}.{}", sanitize(owner), sanitize(field)), ty))
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

    /// GEP-Index eines Methoden-Slots in der Vtable. Interface-Methoden
    /// liegen in den globalen Interface-Slots, virtuelle danach.
    fn vtable_index(&self, class: &str, name: &str, desc: &str) -> Option<usize> {
        if self.class(class).map(|c| c.is_interface).unwrap_or(false) {
            return self.iface_index(class, name, desc).map(|i| VTABLE_METHOD_OFFSET + i);
        }
        self.vtable_slots(class)
            .iter()
            .position(|(n, d, _)| n == name && d == desc)
            .map(|i| i + self.method_base())
    }
}

pub fn emit(program: &Program) -> String {
    let mut out = String::new();
    let w = &mut out;

    // Aufgerufene Interface-Methoden global sammeln (für konsistente
    // Vtable-Slots über alle implementierenden Klassen).
    let mut iface_slots: Vec<(String, String, String)> = Vec::new();
    for f in &program.functions {
        for bb in &f.blocks {
            for st in &bb.statements {
                if let Statement::CallVirtual { class, name, desc, .. } = st {
                    if program.class(class).map(|c| c.is_interface).unwrap_or(false) {
                        let key = (class.clone(), name.clone(), desc.clone());
                        if !iface_slots.contains(&key) {
                            iface_slots.push(key);
                        }
                    }
                }
            }
        }
    }
    let ctx = Ctx { program, iface_slots };

    writeln!(w, "; erzeugt von fastllvm (naive Absenkung, siehe DESIGN.md)").unwrap();

    writeln!(w, "@jrt_string_vtable = external constant [2 x ptr]").unwrap();
    // String-Literale: voller Objekt-Header (uniform mit Laufzeit-Strings),
    // aber refcount -1 = immortal → retain/release/Collector No-Op, die
    // Read-only-Konstante bleibt unberührt.
    for (i, s) in program.strings.iter().enumerate() {
        let bytes = s.as_bytes();
        writeln!(
            w,
            "@jstr.{i} = private unnamed_addr constant {{ i64, i64, ptr, i64, [{n} x i8] }} {{ i64 -1, i64 0, ptr @jrt_string_vtable, i64 {n}, [{n} x i8] c\"{esc}\" }}",
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

    // Struct-Typen: { i64 refcount, i64 rcflags, ptr vtable, felder… }.
    for c in &program.classes {
        let mut parts = vec!["i64".to_string(), "i64".to_string(), "ptr".to_string()];
        parts.extend(ctx.flatten_fields(&c.name).iter().map(|(_, _, t)| llty(*t).to_string()));
        writeln!(w, "{} = type {{ {} }}", ctx.struct_name(&c.name), parts.join(", ")).unwrap();
    }
    // Array-Typen (Header + i64 Länge + flexibles Elementfeld) und ihre
    // Vtables. int[] hat keine Ref-Elemente → No-Op-Drop/Trace; ref[]
    // released/besucht seine Elemente über Runtime-Helfer.
    writeln!(w, "%arr.int = type {{ i64, i64, ptr, i64, [0 x i32] }}").unwrap();
    writeln!(w, "%arr.ref = type {{ i64, i64, ptr, i64, [0 x ptr] }}").unwrap();
    writeln!(w, "@vt.array.int = internal unnamed_addr constant [2 x ptr] [ptr @jrt_noop_drop, ptr @jrt_noop_trace]").unwrap();
    writeln!(w, "@vt.array.ref = internal unnamed_addr constant [2 x ptr] [ptr @jrt_array_ref_drop, ptr @jrt_array_ref_trace]").unwrap();
    writeln!(w).unwrap();

    // Statische Felder als globale Variablen (mit ConstantValue-Initialwert).
    for c in &program.classes {
        for f in &c.static_fields {
            let init = match &f.init {
                None if f.ty == Ty::Ref => "null".to_string(),
                None if f.ty == Ty::F64 => "0.0".to_string(),
                None => "0".to_string(),
                Some(ConstInit::I32(v)) => v.to_string(),
                Some(ConstInit::I64(v)) => v.to_string(),
                Some(ConstInit::F64(v)) => format!("0x{:016X}", v.to_bits()),
                Some(ConstInit::Str(sid)) => format!("@jstr.{sid}"),
            };
            writeln!(
                w,
                "@sf.{}.{} = internal global {} {init}",
                sanitize(&c.name),
                sanitize(&f.name),
                llty(f.ty),
            )
            .unwrap();
        }
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
        // Slot 0: Drop, Slot 1: Trace (Zyklen-Collector); dann die globalen
        // Interface-Slots, dann die klassen-eigenen virtuellen Methoden.
        let mut entries = vec![
            format!("ptr @drop.{}", sanitize(class)),
            format!("ptr @trace.{}", sanitize(class)),
        ];
        let sym_entry = |sym: Option<String>| match sym {
            Some(s) if defined.contains(s.as_str()) => format!("ptr @{s}"),
            _ => "ptr null".to_string(),
        };
        for (iface, name, desc) in &ctx.iface_slots {
            let sym = if program.implements(class, iface) {
                program.resolve_method(class, name, desc).map(|(_, mi)| mi.mangled.clone())
            } else {
                None
            };
            entries.push(sym_entry(sym));
        }
        for (_, _, sym) in ctx.vtable_slots(class) {
            entries.push(sym_entry(sym));
        }
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

    // Drop-Funktionen: released die Ref-Felder des Objekts (die Runtime
    // steigt via jrt_release rekursiv ab).
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

    // Trace-Funktionen: rufen den Collector-Visitor auf jedes Ref-Feld.
    // Der Bacon-Rajan-Collector nutzt sie, um Objektgraphen zu durchlaufen,
    // ohne die Feldstruktur zu kennen.
    for class in &instantiated {
        writeln!(w, "define internal void @trace.{}(ptr %o, ptr %visit) {{", sanitize(class)).unwrap();
        for (k, slot) in ctx.ref_field_slots(class).into_iter().enumerate() {
            writeln!(w, "  %f{k} = getelementptr {}, ptr %o, i32 0, i32 {slot}", ctx.struct_name(class)).unwrap();
            writeln!(w, "  %v{k} = load ptr, ptr %f{k}").unwrap();
            writeln!(w, "  call void %visit(ptr %v{k})").unwrap();
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
        // Statische Initialisierer vor main, Superklasse vor Subklasse.
        let mut emitted: BTreeSet<String> = BTreeSet::new();
        for c in &program.classes {
            emit_clinit_chain(w, &ctx, &c.name, &defined, &mut emitted);
        }
        writeln!(w, "  call void @java_main()").unwrap();
        // Statische Ref-Felder freigeben (GC-Wurzeln bis Programmende) —
        // hält die Heap-Bilanz sauber.
        for c in &program.classes {
            for f in &c.static_fields {
                if f.ty == Ty::Ref {
                    let t = format!("%sf_{}_{}", sanitize(&c.name), sanitize(&f.name));
                    writeln!(w, "  {t} = load ptr, ptr @sf.{}.{}", sanitize(&c.name), sanitize(&f.name)).unwrap();
                    writeln!(w, "  call void @jrt_release(ptr {t})").unwrap();
                }
            }
        }
        // Unbehandelte Exception aus main melden (statt still zu ignorieren).
        writeln!(w, "  call void @jrt_check_uncaught()").unwrap();
        writeln!(w, "  ret i32 0").unwrap();
        writeln!(w, "}}").unwrap();
    }

    out
}

/// Ruft die <clinit> von `class` auf, aber erst die der Superklasse
/// (JVMS 5.5) — jede höchstens einmal.
fn emit_clinit_chain(
    w: &mut String,
    ctx: &Ctx,
    class: &str,
    defined: &BTreeSet<&str>,
    emitted: &mut BTreeSet<String>,
) {
    if !emitted.insert(class.to_string()) {
        return;
    }
    if let Some(ci) = ctx.class(class) {
        if let Some(sup) = &ci.super_name {
            emit_clinit_chain(w, ctx, sup, defined, emitted);
        }
        if ci.has_clinit {
            let sym = fastllvm_ir::clinit_symbol(class);
            if defined.contains(sym.as_str()) {
                writeln!(w, "  call void @{sym}()").unwrap();
            }
        }
    }
}

fn operand_ty(f: &Function, op: &Operand) -> Ty {
    match op {
        Operand::Copy(l) => f.locals[l.0 as usize],
        Operand::ConstI32(_) => Ty::I32,
        Operand::ConstI64(_) => Ty::I64,
        Operand::ConstF64(_) => Ty::F64,
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
            Operand::ConstI64(v) => v.to_string(),
            // LLVM verlangt exakte double-Literale → Bit-Muster als Hex.
            Operand::ConstF64(v) => format!("0x{:016X}", v.to_bits()),
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
                    let ty = operand_ty(e.f, op);
                    let v = e.operand(w, op);
                    let t = e.fresh();
                    if ty == Ty::F64 {
                        writeln!(w, "  {t} = fneg double {v}").unwrap();
                    } else {
                        writeln!(w, "  {t} = sub {} 0, {v}", llty(ty)).unwrap();
                    }
                    t
                }
                Rvalue::Binary(op, a, b) => {
                    let aty = operand_ty(e.f, a);
                    let av = e.operand(w, a);
                    let bv = e.operand(w, b);
                    emit_binop(w, e, *op, aty, &av, &bv)
                }
                Rvalue::Convert(op) => {
                    let from = operand_ty(e.f, op);
                    let to = e.f.locals[dest.0 as usize];
                    let v = e.operand(w, op);
                    let t = e.fresh();
                    let inst = match (from, to) {
                        (Ty::I32, Ty::I64) => "sext",
                        (Ty::I32, Ty::F64) | (Ty::I64, Ty::F64) => "sitofp",
                        (Ty::I64, Ty::I32) => "trunc",
                        _ => panic!("unerwartete Konvertierung {from:?} -> {to:?}"),
                    };
                    writeln!(w, "  {t} = {inst} {} {v} to {}", llty(from), llty(to)).unwrap();
                    t
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
            // Vtable liegt im Header (hinter refcount + rcflags).
            let vtp = e.fresh();
            writeln!(w, "  {vtp} = getelementptr ptr, ptr {recv}, i64 {VTABLE_WORD}").unwrap();
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
        Statement::GetStatic { dest, class, field } => {
            let (g, ty) = ctx.static_field(class, field).unwrap_or_else(|| panic!("statisches Feld {class}.{field} fehlt"));
            let t = e.fresh();
            writeln!(w, "  {t} = load {}, ptr {g}", llty(ty)).unwrap();
            // Ref aus globalem Feld ins Local kopiert → owned (retain).
            store_dest(w, e, *dest, &t, ty == Ty::Ref);
        }
        Statement::PutStatic { class, field, value } => {
            let (g, ty) = ctx.static_field(class, field).unwrap_or_else(|| panic!("statisches Feld {class}.{field} fehlt"));
            let v = e.operand(w, value);
            if ty == Ty::Ref {
                writeln!(w, "  call void @jrt_retain(ptr {v})").unwrap();
                let old = e.fresh();
                writeln!(w, "  {old} = load ptr, ptr {g}").unwrap();
                writeln!(w, "  store ptr {v}, ptr {g}").unwrap();
                writeln!(w, "  call void @jrt_release(ptr {old})").unwrap();
            } else {
                writeln!(w, "  store {} {v}, ptr {g}", llty(ty)).unwrap();
            }
        }
        Statement::NewArray { dest, elem, len } => {
            let n = e.operand(w, len);
            let n64 = e.fresh();
            writeln!(w, "  {n64} = sext i32 {n} to i64").unwrap();
            let t = e.fresh();
            writeln!(
                w,
                "  {t} = call ptr @jrt_alloc_array(i64 {n64}, i64 {}, ptr {})",
                elem_size(*elem),
                array_vtable(*elem),
            )
            .unwrap();
            store_dest(w, e, *dest, &t, false); // alloc gab +1
        }
        Statement::ArrayLen { dest, arr } => {
            let a = e.operand(w, arr);
            writeln!(w, "  call void @jrt_null_check(ptr {a})").unwrap();
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr %arr.int, ptr {a}, i32 0, i32 3").unwrap();
            let t = e.fresh();
            writeln!(w, "  {t} = load i64, ptr {p}").unwrap();
            let t32 = e.fresh();
            writeln!(w, "  {t32} = trunc i64 {t} to i32").unwrap();
            writeln!(w, "  store i32 {t32}, ptr %l{}", dest.0).unwrap();
        }
        Statement::ArrayLoad { dest, arr, index, elem } => {
            let a = e.operand(w, arr);
            let i = e.operand(w, index);
            writeln!(w, "  call void @jrt_null_check(ptr {a})").unwrap();
            writeln!(w, "  call void @jrt_bounds_check(ptr {a}, i32 {i})").unwrap();
            let p = elem_ptr(w, e, *elem, &a, &i);
            let t = e.fresh();
            writeln!(w, "  {t} = load {}, ptr {p}", llty(*elem)).unwrap();
            // Ref-Element geladen → Kopie ins Local wird owned (retain).
            store_dest(w, e, *dest, &t, *elem == Ty::Ref);
        }
        Statement::ArrayStore { arr, index, value, elem } => {
            let a = e.operand(w, arr);
            let i = e.operand(w, index);
            writeln!(w, "  call void @jrt_null_check(ptr {a})").unwrap();
            writeln!(w, "  call void @jrt_bounds_check(ptr {a}, i32 {i})").unwrap();
            let v = e.operand(w, value);
            let p = elem_ptr(w, e, *elem, &a, &i);
            if *elem == Ty::Ref {
                writeln!(w, "  call void @jrt_retain(ptr {v})").unwrap();
                let old = e.fresh();
                writeln!(w, "  {old} = load ptr, ptr {p}").unwrap();
                writeln!(w, "  store ptr {v}, ptr {p}").unwrap();
                writeln!(w, "  call void @jrt_release(ptr {old})").unwrap();
            } else {
                writeln!(w, "  store {} {v}, ptr {p}", llty(*elem)).unwrap();
            }
        }
    }
}

/// GEP auf das `index`-te Element eines Arrays.
fn elem_ptr(w: &mut String, e: &mut FnEmitter, elem: Ty, arr: &str, index: &str) -> String {
    let p = e.fresh();
    // sext des i32-Index nach i64 für den GEP.
    let i64idx = e.fresh();
    writeln!(w, "  {i64idx} = sext i32 {index} to i64").unwrap();
    writeln!(
        w,
        "  {p} = getelementptr {}, ptr {arr}, i32 0, i32 4, i64 {i64idx}",
        array_struct(elem),
    )
    .unwrap();
    p
}

/// Speichert die Vtable im Objektheader (hinter refcount + rcflags).
fn store_vtable(w: &mut String, e: &mut FnEmitter, obj: &str, class: &str) {
    let vtp = e.fresh();
    writeln!(w, "  {vtp} = getelementptr ptr, ptr {obj}, i64 {VTABLE_WORD}").unwrap();
    writeln!(w, "  store ptr @vt.{}, ptr {vtp}", sanitize(class)).unwrap();
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
    let t = e.fresh();
    // Vergleiche liefern immer i32 (0/1); Operanden sind i32 oder ptr
    // (long/double-Vergleiche laufen über Runtime-lcmp/dcmp).
    if matches!(op, BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe) {
        let cc = match op {
            BinOp::CmpEq => "eq",
            BinOp::CmpNe => "ne",
            BinOp::CmpLt => "slt",
            BinOp::CmpGe => "sge",
            BinOp::CmpGt => "sgt",
            _ => "sle",
        };
        let c = e.fresh();
        writeln!(w, "  {c} = icmp {cc} {} {a}, {b}", llty(aty)).unwrap();
        writeln!(w, "  {t} = zext i1 {c} to i32").unwrap();
        return t;
    }

    // double-Arithmetik.
    if aty == Ty::F64 {
        let inst = match op {
            BinOp::Add => "fadd",
            BinOp::Sub => "fsub",
            BinOp::Mul => "fmul",
            BinOp::Div => "fdiv",
            BinOp::Rem => "frem",
            _ => panic!("Bit-/Shift-Operation auf double"),
        };
        writeln!(w, "  {t} = {inst} double {a}, {b}").unwrap();
        return t;
    }

    // int/long-Arithmetik. div/rem laufen für beide über Runtime (nicht hier).
    let ty = llty(aty);
    // Shift-Beträge maskieren (JLS 15.19): & 31 (int) bzw. & 63 (long); der
    // Betrag ist immer int und wird für long auf i64 erweitert.
    let masked = |w: &mut String, e: &mut FnEmitter, b: &str| -> String {
        if aty == Ty::I64 {
            let ext = e.fresh();
            writeln!(w, "  {ext} = zext i32 {b} to i64").unwrap();
            let m = e.fresh();
            writeln!(w, "  {m} = and i64 {ext}, 63").unwrap();
            m
        } else {
            let m = e.fresh();
            writeln!(w, "  {m} = and i32 {b}, 31").unwrap();
            m
        }
    };
    match op {
        BinOp::Add => writeln!(w, "  {t} = add {ty} {a}, {b}").unwrap(),
        BinOp::Sub => writeln!(w, "  {t} = sub {ty} {a}, {b}").unwrap(),
        BinOp::Mul => writeln!(w, "  {t} = mul {ty} {a}, {b}").unwrap(),
        BinOp::Div => writeln!(w, "  {t} = call i32 @jrt_idiv(i32 {a}, i32 {b})").unwrap(),
        BinOp::Rem => writeln!(w, "  {t} = call i32 @jrt_irem(i32 {a}, i32 {b})").unwrap(),
        BinOp::And => writeln!(w, "  {t} = and {ty} {a}, {b}").unwrap(),
        BinOp::Or => writeln!(w, "  {t} = or {ty} {a}, {b}").unwrap(),
        BinOp::Xor => writeln!(w, "  {t} = xor {ty} {a}, {b}").unwrap(),
        BinOp::Shl => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = shl {ty} {a}, {m}").unwrap();
        }
        BinOp::Shr => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = ashr {ty} {a}, {m}").unwrap();
        }
        BinOp::UShr => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = lshr {ty} {a}, {m}").unwrap();
        }
        _ => unreachable!("Vergleich bereits behandelt"),
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
