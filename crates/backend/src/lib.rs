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
        Ty::F32 => "float",
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
    ("jrt_str_hashcode", "i32 (ptr)"),
    ("jrt_str_tostring", "ptr (ptr)"),
    ("jrt_obj_equals", "i32 (ptr, ptr)"),
    ("jrt_obj_hashcode", "i32 (ptr)"),
    ("jrt_obj_tostring", "ptr (ptr)"),
    ("jrt_integer_valueof", "ptr (i32)"),
    ("jrt_integer_intvalue", "i32 (ptr)"),
    ("jrt_integer_equals", "i32 (ptr, ptr)"),
    ("jrt_integer_hashcode", "i32 (ptr)"),
    ("jrt_integer_tostring", "ptr (ptr)"),
    ("jrt_long_valueof", "ptr (i64)"),
    ("jrt_long_longvalue", "i64 (ptr)"),
    ("jrt_long_equals", "i32 (ptr, ptr)"),
    ("jrt_long_hashcode", "i32 (ptr)"),
    ("jrt_long_tostring", "ptr (ptr)"),
    ("jrt_boolean_valueof", "ptr (i32)"),
    ("jrt_boolean_booleanvalue", "i32 (ptr)"),
    ("jrt_boolean_equals", "i32 (ptr, ptr)"),
    ("jrt_boolean_hashcode", "i32 (ptr)"),
    ("jrt_boolean_tostring", "ptr (ptr)"),
    ("jrt_double_valueof", "ptr (double)"),
    ("jrt_double_doublevalue", "double (ptr)"),
    ("jrt_double_equals", "i32 (ptr, ptr)"),
    ("jrt_double_hashcode", "i32 (ptr)"),
    ("jrt_double_tostring", "ptr (ptr)"),
    ("jrt_character_valueof", "ptr (i32)"),
    ("jrt_character_charvalue", "i32 (ptr)"),
    ("jrt_character_equals", "i32 (ptr, ptr)"),
    ("jrt_character_hashcode", "i32 (ptr)"),
    ("jrt_character_tostring", "ptr (ptr)"),
    ("jrt_float_valueof", "ptr (float)"),
    ("jrt_float_floatvalue", "float (ptr)"),
    ("jrt_float_equals", "i32 (ptr, ptr)"),
    ("jrt_float_hashcode", "i32 (ptr)"),
    ("jrt_float_tostring", "ptr (ptr)"),
    ("jrt_str_concat", "ptr (ptr, ptr)"),
    ("jrt_sb_new", "ptr ()"),
    ("jrt_sb_append_str", "ptr (ptr, ptr)"),
    ("jrt_sb_append_int", "ptr (ptr, i32)"),
    ("jrt_sb_append_char", "ptr (ptr, i32)"),
    ("jrt_sb_append_long", "ptr (ptr, i64)"),
    ("jrt_sb_append_double", "ptr (ptr, double)"),
    ("jrt_sb_append_bool", "ptr (ptr, i32)"),
    ("jrt_sb_tostring", "ptr (ptr)"),
    ("jrt_sb_length", "i32 (ptr)"),
    ("jrt_sb_init_str", "void (ptr, ptr)"),
    ("jrt_str_format", "ptr (ptr, ptr)"),
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
    ("jrt_fcmpl", "i32 (float, float)"),
    ("jrt_fcmpg", "i32 (float, float)"),
    ("jrt_f2i", "i32 (float)"),
    ("jrt_f2l", "i64 (float)"),
    ("jrt_print_long", "void (i64)"),
    ("jrt_println_long", "void (i64)"),
    ("jrt_print_double", "void (double)"),
    ("jrt_println_double", "void (double)"),
    ("jrt_print_float", "void (float)"),
    ("jrt_println_float", "void (float)"),
    ("jrt_float_to_str", "ptr (float)"),
    ("jrt_alloc", "ptr (i64)"),
    ("jrt_null_check", "void (ptr)"),
    ("jrt_throw_npe", "void ()"),
    ("jrt_retain", "void (ptr)"),
    ("jrt_release", "void (ptr)"),
    ("jrt_throw", "void (ptr)"),
    ("jrt_pending_set", "i32 ()"),
    ("jrt_take_pending", "ptr ()"),
    ("jrt_check_uncaught", "void ()"),
    ("jrt_pending_instanceof", "i32 (ptr)"),
    ("jrt_instanceof", "i32 (ptr, ptr)"),
    ("jrt_checkcast", "void (ptr, ptr)"),
    ("jrt_alloc_array", "ptr (i64, i64, ptr)"),
    ("jrt_bounds_check", "void (ptr, i32)"),
    ("jrt_iaload", "i32 (ptr, i32)"),
    ("jrt_iastore", "void (ptr, i32, i32)"),
    ("jrt_aaload", "ptr (ptr, i32)"),
    ("jrt_aastore", "void (ptr, i32, ptr)"),
    ("jrt_arraylen", "i32 (ptr)"),
    ("jrt_array_clone", "ptr (ptr, i64, i32)"),
    ("jrt_enum_valueof", "ptr (ptr, ptr)"),
    ("jrt_throwable_message", "ptr (ptr)"),
    ("jrt_array_ref_drop", "void (ptr)"),
    ("jrt_array_ref_trace", "void (ptr, ptr)"),
    ("jrt_noop_drop", "void (ptr)"),
    ("jrt_noop_trace", "void (ptr, ptr)"),
];

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
/// Vtable-Slot 0 = Drop, Slot 1 = Trace (Zyklen-Collector), Slot 2 =
/// Type-Descriptor (instanceof); Interface-/virtuelle Methoden ab Slot 3.
const VTABLE_METHOD_OFFSET: usize = 3;
/// Vtable-Slot des Type-Descriptors.
const VTABLE_TYPEDESC_SLOT: usize = 2;

/// Klassen-Kontext: Layouts und Vtables, aus `Program::classes` berechnet.
struct Ctx<'a> {
    program: &'a Program,
    /// Globale Vtable-Slots für aufgerufene Interface-Methoden, damit
    /// dieselbe Interface-Methode in jeder implementierenden Klasse am
    /// selben Slot liegt. Schlüssel: (interface, name, desc).
    iface_slots: Vec<(String, String, String)>,
    /// TBAA-Zugriffs-Tag (Metadaten-Nummer `!N`) pro deklariertem Instanzfeld
    /// (Owner-Klasse, Feldname). Verschiedene Felder → Geschwister-Typknoten →
    /// beweisbar alias-frei; gleiches Feld → selber Knoten → LLVM bleibt
    /// konservativ. Nicht getaggte Zugriffe (RC-Header, Vtable, Arrays über die
    /// Runtime) aliasieren konservativ mit allem — daher soundness-neutral.
    tbaa: BTreeMap<(String, String), usize>,
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

    /// TBAA-Zugriffs-Tag-Suffix (`, !tbaa !N`) für ein Feld, sonst leer.
    fn tbaa_suffix(&self, owner: &str, field: &str) -> String {
        match self.tbaa.get(&(owner.to_string(), field.to_string())) {
            Some(n) => format!(", !tbaa !{n}"),
            None => String::new(),
        }
    }

    fn iface_index(&self, iface: &str, name: &str, desc: &str) -> Option<usize> {
        self.iface_slots
            .iter()
            .position(|(i, n, d)| i == iface && n == name && d == desc)
    }

    /// Wird `class` global (über konsistente Vtable-Slots) dispatcht?
    /// Interfaces und die Object-Wurzelmethoden.
    fn is_global_dispatch(&self, class: &str) -> bool {
        class == "java/lang/Object" || self.class(class).map(|c| c.is_interface).unwrap_or(false)
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
        if self.is_global_dispatch(class) {
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
                    let global = class == "java/lang/Object"
                        || program.class(class).map(|c| c.is_interface).unwrap_or(false);
                    if global {
                        let key = (class.clone(), name.clone(), desc.clone());
                        if !iface_slots.contains(&key) {
                            iface_slots.push(key);
                        }
                    }
                }
            }
        }
    }
    // TBAA-Registry: jedem deklarierten Instanzfeld ein Zugriffs-Tag zuweisen.
    // Layout der Metadaten: !0 = Wurzel; Feld k → Typknoten !(1+2k), Tag !(2+2k).
    let mut tbaa: BTreeMap<(String, String), usize> = BTreeMap::new();
    for c in &program.classes {
        for f in &c.fields {
            let key = (c.name.clone(), f.name.clone());
            if !tbaa.contains_key(&key) {
                let k = tbaa.len();
                tbaa.insert(key, 2 + 2 * k);
            }
        }
    }
    let ctx = Ctx { program, iface_slots, tbaa };

    writeln!(w, "; erzeugt von fastllvm (naive Absenkung, siehe DESIGN.md)").unwrap();

    writeln!(w, "@jrt_dyn_string_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_integer_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_long_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_boolean_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_double_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_character_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_float_vt = external global ptr").unwrap();
    // String-Literale: voller Objekt-Header (uniform mit Laufzeit-Strings),
    // aber refcount -1 = immortal → retain/release/Collector No-Op, die
    // Read-only-Konstante bleibt unberührt. Vtable = @vt.java_lang_String
    // (Object-Methoden-Slots), damit obj.equals/hashCode auf Strings greift.
    for (i, s) in program.strings.iter().enumerate() {
        let bytes = s.as_bytes();
        writeln!(
            w,
            "@jstr.{i} = private unnamed_addr constant {{ i64, i64, ptr, i64, [{n} x i8] }} {{ i64 -1, i64 0, ptr @vt.java_lang_String, i64 {n}, [{n} x i8] c\"{esc}\" }}",
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
    // Arrays haben keinen Type-Descriptor (Slot 2 = null → instanceof false).
    writeln!(w, "@vt.array.int = internal unnamed_addr constant [3 x ptr] [ptr @jrt_noop_drop, ptr @jrt_noop_trace, ptr null]").unwrap();
    writeln!(w, "@vt.array.ref = internal unnamed_addr constant [3 x ptr] [ptr @jrt_array_ref_drop, ptr @jrt_array_ref_trace, ptr null]").unwrap();
    writeln!(w).unwrap();

    // Type-Descriptoren für instanceof: { ptr super, ptr name }. Die Kette
    // endet bei null (Object/nicht modellierte Basis). jrt_instanceof läuft
    // sie ab; der Name (gepunktet) dient der Uncaught-Meldung.
    for c in &program.classes {
        let super_td = match &c.super_name {
            Some(s) if program.class(s).is_some() => format!("@td.{}", sanitize(s)),
            _ => "null".to_string(),
        };
        let dotted = c.name.replace('/', ".");
        let bytes = dotted.as_bytes();
        writeln!(
            w,
            "@cname.{} = private unnamed_addr constant [{n} x i8] c\"{esc}\\00\"",
            sanitize(&c.name),
            n = bytes.len() + 1,
            esc = escape_ll(bytes),
        )
        .unwrap();
        writeln!(
            w,
            "@td.{} = internal constant {{ ptr, ptr }} {{ ptr {super_td}, ptr @cname.{} }}",
            sanitize(&c.name),
            sanitize(&c.name),
        )
        .unwrap();
    }
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
    // Strings/Wrapper nehmen am virtuellen Dispatch teil (equals/hashCode/
    // toString) → eigene Vtable, obwohl sie nicht via `new` erzeugt werden.
    let mut instantiated = instantiated;
    if program.class("java/lang/String").is_some() {
        instantiated.insert("java/lang/String");
    }
    let calls_fn = |sym: &str| {
        program
            .functions
            .iter()
            .flat_map(|f| &f.blocks)
            .flat_map(|b| &b.statements)
            .any(|st| matches!(st, Statement::Call { func, .. } if func == sym))
    };
    // (valueOf-Funktion, Klasse, dynamischer Vtable-Zeiger)
    let wrappers = [
        ("jrt_integer_valueof", "java/lang/Integer", "jrt_integer_vt"),
        ("jrt_long_valueof", "java/lang/Long", "jrt_long_vt"),
        ("jrt_boolean_valueof", "java/lang/Boolean", "jrt_boolean_vt"),
        ("jrt_double_valueof", "java/lang/Double", "jrt_double_vt"),
        ("jrt_character_valueof", "java/lang/Character", "jrt_character_vt"),
        ("jrt_float_valueof", "java/lang/Float", "jrt_float_vt"),
    ];
    for (vf, cls, _) in &wrappers {
        if calls_fn(vf) {
            instantiated.insert(cls);
        }
    }
    for class in &instantiated {
        // Slot 0: Drop, Slot 1: Trace (Zyklen-Collector); dann die globalen
        // Interface-Slots, dann die klassen-eigenen virtuellen Methoden.
        let mut entries = vec![
            format!("ptr @drop.{}", sanitize(class)),
            format!("ptr @trace.{}", sanitize(class)),
            format!("ptr @td.{}", sanitize(class)),
        ];
        // jrt_*-Symbole sind Runtime-Funktionen (extern), gelten als gültig.
        let sym_entry = |sym: Option<String>| match sym {
            Some(s) if s.starts_with("jrt_") || defined.contains(s.as_str()) => format!("ptr @{s}"),
            _ => "ptr null".to_string(),
        };
        for (iface, name, desc) in &ctx.iface_slots {
            let sym = if iface == "java/lang/Object" {
                // Wurzelmethode: Überschreibung der Klasse oder Object-Default.
                Some(
                    program
                        .resolve_method(class, name, desc)
                        .map(|(_, mi)| mi.mangled.clone())
                        .unwrap_or_else(|| object_default(name)),
                )
            } else if program.implements(class, iface) {
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
        // Vtable-Zeiger für zur Laufzeit erzeugte String-/Wrapper-Objekte.
        writeln!(w, "  store ptr @vt.java_lang_String, ptr @jrt_dyn_string_vt").unwrap();
        for (vf, cls, vt) in &wrappers {
            if calls_fn(vf) {
                writeln!(w, "  store ptr @vt.{}, ptr @{vt}", sanitize(cls)).unwrap();
            }
        }
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

    // TBAA-Metadatenbaum: Wurzel !0, pro Feld ein Typknoten + Zugriffs-Tag.
    if !ctx.tbaa.is_empty() {
        writeln!(w, "\n!0 = !{{!\"fastllvm-tbaa\"}}").unwrap();
        let mut fields: Vec<(&(String, String), &usize)> = ctx.tbaa.iter().collect();
        fields.sort_by_key(|(_, n)| **n);
        for ((owner, field), tag) in fields {
            let tynode = tag - 1;
            writeln!(w, "!{tynode} = !{{!\"fld.{}.{}\", !0}}", sanitize(owner), sanitize(field)).unwrap();
            writeln!(w, "!{tag} = !{{!{tynode}, !{tynode}, i64 0}}").unwrap();
        }
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
            // Statische Abhängigkeiten: liest/erzeugt der <clinit> die Statik
            // einer anderen Klasse (z.B. der enum-switch-Helfer Main$1 ruft
            // Dir.values() und liest Dir.N), muss deren <clinit> vorher laufen.
            // Java initialisiert lazy bei erstem Zugriff; wir eager, daher hier
            // topologisch vorziehen (der emitted-Guard bricht etwaige Zyklen).
            if let Some(f) = ctx.program.functions.iter().find(|f| f.name == sym) {
                for dep in clinit_deps(ctx, f) {
                    if dep != class {
                        emit_clinit_chain(w, ctx, &dep, defined, emitted);
                    }
                }
            }
            if defined.contains(sym.as_str()) {
                writeln!(w, "  call void @{sym}()").unwrap();
            }
        }
    }
}

/// Klassen, deren Statik ein `<clinit>`-Rumpf berührt (Feld-/New-/Cast-/
/// virtueller Zugriff sowie direkte Calls in ihre Methoden) — Kandidaten,
/// die vor diesem `<clinit>` initialisiert sein müssen.
fn clinit_deps(ctx: &Ctx, f: &Function) -> BTreeSet<String> {
    // Symbol → deklarierende Klasse, um Call-Ziele einer Klasse zuzuordnen.
    let sym_class = |sym: &str| -> Option<String> {
        ctx.program
            .classes
            .iter()
            .find(|c| c.methods.iter().any(|m| m.mangled == sym))
            .map(|c| c.name.clone())
    };
    let mut deps = BTreeSet::new();
    for bb in &f.blocks {
        for st in &bb.statements {
            match st {
                Statement::GetStatic { class, .. }
                | Statement::PutStatic { class, .. }
                | Statement::New { class, .. }
                | Statement::StackNew { class, .. }
                | Statement::GetField { class, .. }
                | Statement::PutField { class, .. }
                | Statement::CallVirtual { class, .. }
                | Statement::InstanceOf { class, .. }
                | Statement::InstanceOfPending { class, .. }
                | Statement::CheckCast { class, .. } => {
                    deps.insert(class.clone());
                }
                Statement::Call { func, .. } | Statement::CallGuarded { func, .. } => {
                    if let Some(c) = sym_class(func) {
                        deps.insert(c);
                    }
                }
                Statement::CallPoly { targets, .. } => {
                    for (c, _) in targets {
                        deps.insert(c.clone());
                    }
                }
                _ => {}
            }
        }
    }
    deps
}

/// Runtime-Default-Implementierung einer Object-Wurzelmethode.
fn object_default(name: &str) -> String {
    match name {
        "hashCode" => "jrt_obj_hashcode",
        "toString" => "jrt_obj_tostring",
        _ => "jrt_obj_equals",
    }
    .to_string()
}

fn operand_ty(f: &Function, op: &Operand) -> Ty {
    match op {
        Operand::Copy(l) => f.locals[l.0 as usize],
        Operand::ConstI32(_) => Ty::I32,
        Operand::ConstI64(_) => Ty::I64,
        Operand::ConstF32(_) => Ty::F32,
        Operand::ConstF64(_) => Ty::F64,
        Operand::ConstStr(_) | Operand::ConstClass(_) | Operand::ConstNull => Ty::Ref,
    }
}

struct FnEmitter<'a> {
    f: &'a Function,
    tmp: u32,
    label: u32,
}

impl<'a> FnEmitter<'a> {
    fn fresh(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    /// Frisches LLVM-Blocklabel (für Mid-Block-Verzweigungen wie den
    /// Null-Skip bei Feld-/Receiver-Zugriffen).
    fn fresh_label(&mut self) -> String {
        self.label += 1;
        format!("nz{}", self.label)
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
            // float-Literale in LLVM: Hex des exakt promoteten double-Werts.
            Operand::ConstF32(v) => format!("0x{:016X}", (*v as f64).to_bits()),
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

    let mut e = FnEmitter { f, tmp: 0, label: 0 };

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
            Terminator::Switch { value, default, cases } => {
                let v = e.operand(w, value);
                let arms: String = cases
                    .iter()
                    .map(|(k, b)| format!("i32 {k}, label %bb{}", b.0))
                    .collect::<Vec<_>>()
                    .join(" ");
                writeln!(w, "  switch i32 {v}, label %bb{} [{arms}]", default.0).unwrap();
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
                    if ty == Ty::F64 || ty == Ty::F32 {
                        writeln!(w, "  {t} = fneg {} {v}", llty(ty)).unwrap();
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
                        (Ty::I32, Ty::F32) | (Ty::I64, Ty::F32) => "sitofp",
                        (Ty::I64, Ty::I32) => "trunc",
                        (Ty::F32, Ty::F64) => "fpext",
                        (Ty::F64, Ty::F32) => "fptrunc",
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
        // Devirtualisierter Instanzaufruf mit abfangbarer Receiver-NPE.
        Statement::CallGuarded { dest, func, args } => {
            let recv = e.operand(w, &args[0]);
            let avs = call_args(w, e, args);
            let (nb, ok, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label());
            let isnull = e.fresh();
            writeln!(w, "  {isnull} = icmp eq ptr {recv}, null").unwrap();
            writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
            writeln!(w, "{nb}:").unwrap();
            writeln!(w, "  call void @jrt_throw_npe()").unwrap();
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{ok}:").unwrap();
            match dest {
                None => writeln!(w, "  call void @{func}({avs})").unwrap(),
                Some(d) => {
                    let rty = llty(e.f.locals[d.0 as usize]);
                    let t = e.fresh();
                    writeln!(w, "  {t} = call {rty} @{func}({avs})").unwrap();
                    store_dest(w, e, *d, &t, false);
                }
            }
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{cont}:").unwrap();
        }
        Statement::CallVirtual { dest, class, name, desc, params, ret, args } => {
            let slot = ctx
                .vtable_index(class, name, desc)
                .unwrap_or_else(|| panic!("Vtable-Slot {class}.{name}{desc} fehlt"));
            let recv = e.operand(w, &args[0]);
            // Restliche Argumente vor dem Verzweigen materialisieren (dürfen
            // in beiden Zweigen benutzt werden).
            let mut avs = vec![format!("ptr {recv}")];
            for a in &args[1..] {
                let ty = llty(operand_ty(e.f, a));
                let v = e.operand(w, a);
                avs.push(format!("{ty} {v}"));
            }
            let _ = params;
            // Abfangbare Receiver-NPE: bei null zum npe-Block, sonst Dispatch.
            let (nb, ok, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label());
            let isnull = e.fresh();
            writeln!(w, "  {isnull} = icmp eq ptr {recv}, null").unwrap();
            writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
            writeln!(w, "{nb}:").unwrap();
            writeln!(w, "  call void @jrt_throw_npe()").unwrap();
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{ok}:").unwrap();
            // Vtable liegt im Header (hinter refcount + rcflags).
            let vtp = e.fresh();
            writeln!(w, "  {vtp} = getelementptr ptr, ptr {recv}, i64 {VTABLE_WORD}").unwrap();
            let vt = e.fresh();
            writeln!(w, "  {vt} = load ptr, ptr {vtp}").unwrap();
            let slotp = e.fresh();
            writeln!(w, "  {slotp} = getelementptr ptr, ptr {vt}, i64 {slot}").unwrap();
            let fnp = e.fresh();
            writeln!(w, "  {fnp} = load ptr, ptr {slotp}").unwrap();
            match dest {
                None => writeln!(w, "  call {} {fnp}({})", llty(*ret), avs.join(", ")).unwrap(),
                Some(d) => {
                    let t = e.fresh();
                    writeln!(w, "  {t} = call {} {fnp}({})", llty(*ret), avs.join(", ")).unwrap();
                    // Ref-Rückgabe transferiert +1 (kein retain).
                    store_dest(w, e, *d, &t, false);
                }
            }
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{cont}:").unwrap();
        }
        Statement::CallPoly { dest, ret, args, targets } => {
            let recv = e.operand(w, &args[0]);
            // Argumente einmal materialisieren (in allen Zweigen gültig).
            let mut avs = vec![format!("ptr {recv}")];
            for a in &args[1..] {
                let ty = llty(operand_ty(e.f, a));
                let v = e.operand(w, a);
                avs.push(format!("{ty} {v}"));
            }
            let avs = avs.join(", ");
            let cont = e.fresh_label();
            // Abfangbare Receiver-NPE: bei null → npe-Block.
            let (nb, ok) = (e.fresh_label(), e.fresh_label());
            let isnull = e.fresh();
            writeln!(w, "  {isnull} = icmp eq ptr {recv}, null").unwrap();
            writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
            writeln!(w, "{nb}:").unwrap();
            writeln!(w, "  call void @jrt_throw_npe()").unwrap();
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{ok}:").unwrap();
            // Vtable-Zeiger des Receivers laden.
            let vtp = e.fresh();
            writeln!(w, "  {vtp} = getelementptr ptr, ptr {recv}, i64 {VTABLE_WORD}").unwrap();
            let vt = e.fresh();
            writeln!(w, "  {vt} = load ptr, ptr {vtp}").unwrap();
            // Kaskade: pro Klasse ein Vtable-Vergleich → Direkt-Call; das
            // letzte Ziel ist der else-Zweig (Closed World: Receiver ist
            // garantiert eine der instanziierten Zielklassen).
            let emit_call = |w: &mut String, e: &mut FnEmitter, sym: &str| {
                match dest {
                    None => writeln!(w, "  call {} @{sym}({avs})", llty(*ret)).unwrap(),
                    Some(d) => {
                        let t = e.fresh();
                        writeln!(w, "  {t} = call {} @{sym}({avs})", llty(*ret)).unwrap();
                        store_dest(w, e, *d, &t, false);
                    }
                }
            };
            for (k, (cls, sym)) in targets.iter().enumerate() {
                if k + 1 == targets.len() {
                    // letztes Ziel: unbedingt (else)
                    emit_call(w, e, sym);
                    writeln!(w, "  br label %{cont}").unwrap();
                } else {
                    let (hit, miss) = (e.fresh_label(), e.fresh_label());
                    let eqv = e.fresh();
                    writeln!(w, "  {eqv} = icmp eq ptr {vt}, @vt.{}", sanitize(cls)).unwrap();
                    writeln!(w, "  br i1 {eqv}, label %{hit}, label %{miss}").unwrap();
                    writeln!(w, "{hit}:").unwrap();
                    emit_call(w, e, sym);
                    writeln!(w, "  br label %{cont}").unwrap();
                    writeln!(w, "{miss}:").unwrap();
                }
            }
            writeln!(w, "{cont}:").unwrap();
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
            // Abfangbare NPE: bei null zum npe-Block (pending), sonst Zugriff.
            let (nb, ok, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label());
            let isnull = e.fresh();
            writeln!(w, "  {isnull} = icmp eq ptr {o}, null").unwrap();
            writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
            writeln!(w, "{nb}:").unwrap();
            writeln!(w, "  call void @jrt_throw_npe()").unwrap();
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{ok}:").unwrap();
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr {}, ptr {o}, i32 0, i32 {idx}", ctx.struct_name(&owner)).unwrap();
            let t = e.fresh();
            writeln!(w, "  {t} = load {}, ptr {p}{}", llty(ty), ctx.tbaa_suffix(&owner, field)).unwrap();
            // Feldwert ist geborgt; die Kopie ins Local wird owned → retain.
            store_dest(w, e, *dest, &t, true);
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{cont}:").unwrap();
        }
        Statement::PutField { obj, class, field, value } => {
            let (owner, idx, ty) = ctx
                .field_slot(class, field)
                .unwrap_or_else(|| panic!("Feld {class}.{field} fehlt"));
            let o = e.operand(w, obj);
            let v = e.operand(w, value);
            let (nb, ok, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label());
            let isnull = e.fresh();
            writeln!(w, "  {isnull} = icmp eq ptr {o}, null").unwrap();
            writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
            writeln!(w, "{nb}:").unwrap();
            writeln!(w, "  call void @jrt_throw_npe()").unwrap();
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{ok}:").unwrap();
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr {}, ptr {o}, i32 0, i32 {idx}", ctx.struct_name(&owner)).unwrap();
            let tb = ctx.tbaa_suffix(&owner, field);
            if ty == Ty::Ref {
                // Feld übernimmt eine owning-Referenz: retain neu, release alt.
                writeln!(w, "  call void @jrt_retain(ptr {v})").unwrap();
                let old = e.fresh();
                writeln!(w, "  {old} = load ptr, ptr {p}{tb}").unwrap();
                writeln!(w, "  store ptr {v}, ptr {p}{tb}").unwrap();
                writeln!(w, "  call void @jrt_release(ptr {old})").unwrap();
            } else {
                writeln!(w, "  store {} {v}, ptr {p}{tb}", llty(ty)).unwrap();
            }
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{cont}:").unwrap();
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
        Statement::InstanceOfPending { dest, class } => {
            let t = e.fresh();
            writeln!(w, "  {t} = call i32 @jrt_pending_instanceof(ptr @td.{})", sanitize(class)).unwrap();
            writeln!(w, "  store i32 {t}, ptr %l{}", dest.0).unwrap();
        }
        Statement::CheckCast { obj, class } => {
            let o = e.operand(w, obj);
            writeln!(w, "  call void @jrt_checkcast(ptr {o}, ptr @td.{})", sanitize(class)).unwrap();
        }
        Statement::InstanceOf { dest, obj, class } => {
            let o = e.operand(w, obj);
            let t = e.fresh();
            writeln!(w, "  {t} = call i32 @jrt_instanceof(ptr {o}, ptr @td.{})", sanitize(class)).unwrap();
            writeln!(w, "  store i32 {t}, ptr %l{}", dest.0).unwrap();
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
        // Array-Zugriffe über Runtime-Helfer (Check + Zugriff gekapselt),
        // damit NPE/ArrayIndexOutOfBounds abfangbar sind (pending-Modell).
        Statement::ArrayLen { dest, arr } => {
            let a = e.operand(w, arr);
            let t = e.fresh();
            writeln!(w, "  {t} = call i32 @jrt_arraylen(ptr {a})").unwrap();
            writeln!(w, "  store i32 {t}, ptr %l{}", dest.0).unwrap();
        }
        Statement::ArrayLoad { dest, arr, index, elem } => {
            let a = e.operand(w, arr);
            let i = e.operand(w, index);
            let (func, rty) = if *elem == Ty::Ref { ("jrt_aaload", "ptr") } else { ("jrt_iaload", "i32") };
            let t = e.fresh();
            writeln!(w, "  {t} = call {rty} @{func}(ptr {a}, i32 {i})").unwrap();
            // Ref-Element ist geborgt → Kopie ins Local wird owned (retain).
            store_dest(w, e, *dest, &t, *elem == Ty::Ref);
        }
        Statement::ArrayStore { arr, index, value, elem } => {
            let a = e.operand(w, arr);
            let i = e.operand(w, index);
            let v = e.operand(w, value);
            // aastore erledigt das RC (retain neu, release alt) intern.
            let func = if *elem == Ty::Ref { "jrt_aastore" } else { "jrt_iastore" };
            writeln!(w, "  call void @{func}(ptr {a}, i32 {i}, {} {v})", llty(*elem)).unwrap();
        }
    }
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

    // Gleitkomma-Arithmetik (double/float).
    if aty == Ty::F64 || aty == Ty::F32 {
        let inst = match op {
            BinOp::Add => "fadd",
            BinOp::Sub => "fsub",
            BinOp::Mul => "fmul",
            BinOp::Div => "fdiv",
            BinOp::Rem => "frem",
            _ => panic!("Bit-/Shift-Operation auf Gleitkomma"),
        };
        writeln!(w, "  {t} = {inst} {} {a}, {b}", llty(aty)).unwrap();
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
