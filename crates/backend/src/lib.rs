//! Naive lowering of mid-level IR → textual LLVM IR.
//!
//! Deliberately kept dumb: every IR local becomes an `alloca`, every access
//! a load/store — LLVM's mem2reg/SROA restores SSA. Textual
//! `.ll` output instead of API bindings, because llvm-sys/inkwell lag
//! behind the installed LLVM 22.
//!
//! Object model (stage 2):
//! - `%class.C = type { ptr, fields… }` — slot 0 is the vtable pointer;
//!   superclass fields precede the class's own, which makes GEP indices
//!   stable across the whole subclass hierarchy (prefix layout).
//! - Vtable slots: inherited slots first (overrides replace in place),
//!   new virtual methods after them in declaration order.
//! - getfield/putfield/invokevirtual check the receiver for null
//!   (Java semantics; HotSpot's segfault trick would be a runtime, DESIGN.md §6).
//!
//! Java semantics points:
//! - idiv/irem via runtime helpers (exception on /0, MIN/-1 defined)
//! - shift amount is masked with &31 (JLS 15.19)
//! - addition etc. wrap (LLVM add without nsw/nuw already wraps)

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

/// Intrinsics and runtime helpers that the mini-runtime (runtime.c) defines.
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
    ("jrt_str_indexof", "i32 (ptr, ptr)"),
    ("jrt_str_startswith", "i32 (ptr, ptr)"),
    ("jrt_str_endswith", "i32 (ptr, ptr)"),
    ("jrt_str_compareto", "i32 (ptr, ptr)"),
    ("jrt_integer_compareto", "i32 (ptr, ptr)"),
    ("jrt_long_compareto", "i32 (ptr, ptr)"),
    ("jrt_double_compareto", "i32 (ptr, ptr)"),
    ("jrt_float_compareto", "i32 (ptr, ptr)"),
    ("jrt_character_compareto", "i32 (ptr, ptr)"),
    ("jrt_boolean_compareto", "i32 (ptr, ptr)"),
    ("jrt_str_substring1", "ptr (ptr, i32)"),
    ("jrt_str_substring2", "ptr (ptr, i32, i32)"),
    ("jrt_str_trim", "ptr (ptr)"),
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
    ("jrt_array_data", "ptr (ptr)"),
    ("jrt_null_check", "void (ptr)"),
    ("jrt_throw_npe", "void ()"),
    ("jrt_throw_bounds", "void ()"),
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
    ("jrt_baload", "i32 (ptr, i32)"),
    ("jrt_bastore", "void (ptr, i32, i32)"),
    ("jrt_caload", "i32 (ptr, i32)"),
    ("jrt_castore", "void (ptr, i32, i32)"),
    ("jrt_saload", "i32 (ptr, i32)"),
    ("jrt_sastore", "void (ptr, i32, i32)"),
    ("jrt_laload", "i64 (ptr, i32)"),
    ("jrt_lastore", "void (ptr, i32, i64)"),
    ("jrt_daload", "double (ptr, i32)"),
    ("jrt_dastore", "void (ptr, i32, double)"),
    ("jrt_faload", "float (ptr, i32)"),
    ("jrt_fastore", "void (ptr, i32, float)"),
    ("jrt_arraylen", "i32 (ptr)"),
    ("jrt_array_clone", "ptr (ptr, i64, i32)"),
    ("jrt_arraycopy", "void (ptr, i32, ptr, i32, i32)"),
    ("jrt_enum_valueof", "ptr (ptr, ptr)"),
    ("jrt_throwable_message", "ptr (ptr)"),
    ("jrt_get_class", "ptr (ptr)"),
    ("jrt_record_memeq", "i32 (ptr, ptr, i32, i64)"),
    ("jrt_print_bool", "void (i32)"),
    ("jrt_println_bool", "void (i32)"),
    ("jrt_monitor_enter", "void (ptr)"),
    ("jrt_monitor_exit", "void (ptr)"),
    ("jrt_thread_start", "void (ptr)"),
    ("jrt_thread_join", "void (ptr)"),
    // Vire concurrency: spawn/join + Atomic (jrt_spawn itself is referenced only
    // from the generated per-worker C shim, so it needs no IR declaration here).
    ("jrt_join", "i64 (ptr)"),
    ("jrt_atomic_new", "ptr (i64)"),
    ("jrt_atomic_add", "i64 (ptr, i64)"),
    ("jrt_atomic_get", "i64 (ptr)"),
    ("jrt_mutex_new", "ptr (i64)"),
    ("jrt_mutex_lock", "void (ptr)"),
    ("jrt_mutex_unlock", "void (ptr)"),
    ("jrt_mutex_get", "i64 (ptr)"),
    ("jrt_mutex_set", "void (ptr, i64)"),
    ("jrt_class_getname", "ptr (ptr)"),
    ("jrt_class_getsimplename", "ptr (ptr)"),
    ("jrt_parse_int", "i32 (ptr)"),
    ("jrt_parse_long", "i64 (ptr)"),
    ("jrt_math_abs_i", "i32 (i32)"),
    ("jrt_math_abs_l", "i64 (i64)"),
    ("jrt_math_abs_d", "double (double)"),
    ("jrt_math_abs_f", "float (float)"),
    ("jrt_math_max_i", "i32 (i32, i32)"),
    ("jrt_math_min_i", "i32 (i32, i32)"),
    ("jrt_math_max_l", "i64 (i64, i64)"),
    ("jrt_math_min_l", "i64 (i64, i64)"),
    ("jrt_math_max_d", "double (double, double)"),
    ("jrt_math_min_d", "double (double, double)"),
    ("jrt_math_sqrt", "double (double)"),
    ("jrt_current_time_millis", "i64 ()"),
    ("jrt_nano_time", "i64 ()"),
    ("jrt_array_ref_drop", "void (ptr)"),
    ("jrt_array_ref_trace", "void (ptr, ptr)"),
    ("jrt_noop_drop", "void (ptr)"),
    ("jrt_noop_trace", "void (ptr, ptr)"),
];

fn array_vtable(kind: ArrKind) -> &'static str {
    if kind.is_ref() {
        "@vt.array.ref"
    } else {
        "@vt.array.int"
    }
}

/// Fixed header slots before the instance fields:
///   Slot 0: refcount (i64), <0 = immortal
///   Slot 1: rcflags (i64) — color/buffered bit for the cycle collector
///   Slot 2: vtable (ptr)
/// Instance fields therefore begin at GEP index 3.
const HEADER_SLOTS: usize = 2;
/// Word offset of the vtable pointer in the header (for ptr getelementptr).
const VTABLE_WORD: usize = 1;
/// Vtable slot 0 = drop, slot 1 = trace (cycle collector), slot 2 =
/// type descriptor (instanceof); interface/virtual methods from slot 3 on.
const VTABLE_METHOD_OFFSET: usize = 3;
/// Vtable slot of the type descriptor.
const VTABLE_TYPEDESC_SLOT: usize = 2;

/// Class context: layouts and vtables, computed from `Program::classes`.
struct Ctx<'a> {
    program: &'a Program,
    /// Global vtable slots for called interface methods, so that
    /// the same interface method sits at the same slot in every
    /// implementing class. Key: (interface, name, desc).
    iface_slots: Vec<(String, String, String)>,
    /// TBAA access tag (metadata number `!N`) per declared instance field
    /// (owner class, field name). Different fields → sibling type nodes →
    /// provably alias-free; same field → same node → LLVM stays
    /// conservative. Untagged accesses (RC header, vtable, arrays via the
    /// runtime) alias conservatively with everything — hence soundness-neutral.
    tbaa: BTreeMap<(String, String), usize>,
    /// Per function, the static fields written (transitively through callees).
    /// A field that a function (and its callees) does NOT write is constant
    /// during its execution → `GetStatic` yields a stable reference kept alive
    /// by the static root and needs no retain/release.
    static_writes: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Interprocedural instance-field write sets (+ opaque flag) for region inference.
    field_writes: BTreeMap<String, (BTreeSet<(String, String)>, bool)>,
    /// AOT hot path: metadata IDs of the two shared `branch_weights` nodes
    /// (then-hot, else-hot). Set from static loop estimation on `!prof` —
    /// LLVM then orders/optimizes hot paths itself.
    bw_then: usize,
    bw_else: usize,
    /// Metadata node for `!invariant.load` (empty node). Marks loads of
    /// provably immutable memory locations (array length, vtable pointer) —
    /// LLVM may hoist them out of loops and CSE them (like Rust's slice length).
    md_inv: usize,
}

impl<'a> Ctx<'a> {
    fn class(&self, name: &str) -> Option<&'a ClassInfo> {
        self.program.class(name)
    }

    /// First vtable slot of the class's own virtual methods (after
    /// drop, trace, and the global interface slots).
    fn method_base(&self) -> usize {
        VTABLE_METHOD_OFFSET + self.iface_slots.len()
    }

    /// TBAA access-tag suffix (`, !tbaa !N`) for a field, otherwise empty.
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

    /// Is `class` dispatched globally (via consistent vtable slots)?
    /// Interfaces and the Object root methods.
    fn is_global_dispatch(&self, class: &str) -> bool {
        class == "java/lang/Object" || self.class(class).map(|c| c.is_interface).unwrap_or(false)
    }

    fn struct_name(&self, class: &str) -> String {
        format!("%class.{}", sanitize(class))
    }

    /// Instance fields in layout order: superclasses first.
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

    /// GEP index (after the header) and type of a field, resolved
    /// from `class` up the super chain.
    fn field_slot(&self, class: &str, field: &str) -> Option<(String, usize, Ty)> {
        let (owner, ty) = self.program.resolve_field(class, field)?;
        let owner = owner.to_string();
        let flat = self.flatten_fields(&owner);
        let idx = flat.iter().position(|(o, n, _)| *o == owner && n == field)?;
        Some((owner, idx + HEADER_SLOTS, ty))
    }

    /// Global symbol and type of a static field (up the super chain).
    fn static_field(&self, class: &str, field: &str) -> Option<(String, Ty)> {
        let (owner, ty) = self.program.resolve_static_field(class, field)?;
        Some((format!("@sf.{}.{}", sanitize(owner), sanitize(field)), ty))
    }

    /// Ref fields of `class` (including inherited) as a GEP index list — for
    /// the generated drop function.
    fn ref_field_slots(&self, class: &str) -> Vec<usize> {
        self.flatten_fields(class)
            .iter()
            .enumerate()
            .filter(|(_, (_, _, t))| *t == Ty::Ref)
            .map(|(i, _)| i + HEADER_SLOTS)
            .collect()
    }

    /// Vtable slots of `class`: (name, desc, implementation symbol).
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

    /// GEP index of a method slot in the vtable. Interface methods
    /// sit in the global interface slots, virtual ones after them.
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

    // Collect called interface methods globally (for consistent
    // vtable slots across all implementing classes).
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
    // Runnable.run() is dispatched only via the native thread trampoline
    // (no CallVirtual in the IR) → force a global vtable slot so that
    // @jrt_invoke_runnable finds it.
    if program.class("java/lang/Runnable").is_some() {
        let key = ("java/lang/Runnable".to_string(), "run".to_string(), "()V".to_string());
        if !iface_slots.contains(&key) {
            iface_slots.push(key);
        }
    }
    // TBAA registry: assign an access tag to every declared instance field.
    // Metadata layout: !0 = root; field k → type node !(1+2k), tag !(2+2k).
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
    let static_writes = static_write_effects(program);
    let field_writes = instance_field_writes(program);
    // AOT hot path: metadata IDs for the two shared branch_weights nodes,
    // above the TBAA IDs (max TBAA ID = 2*len for len fields, otherwise 0).
    let bw_base = if tbaa.is_empty() { 0 } else { 2 * tbaa.len() + 1 };
    let bw_then = bw_base;
    let bw_else = bw_base + 1;
    let md_inv = bw_base + 2;
    let ctx = Ctx { program, iface_slots, tbaa, static_writes, field_writes, bw_then, bw_else, md_inv };

    writeln!(w, "; generated by fastllvm (naive lowering, see DESIGN.md)").unwrap();

    writeln!(w, "@jrt_dyn_string_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_integer_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_long_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_boolean_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_double_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_character_vt = external global ptr").unwrap();
    writeln!(w, "@jrt_float_vt = external global ptr").unwrap();
    // String literals: full object header (uniform with runtime strings),
    // but refcount -1 = immortal → retain/release/collector no-op, the
    // read-only constant stays untouched. Vtable = @vt.java_lang_String
    // (Object method slots), so obj.equals/hashCode works on strings.
    for (i, s) in program.strings.iter().enumerate() {
        let bytes = s.as_bytes();
        writeln!(
            w,
            "@jstr.{i} = private unnamed_addr constant {{ i64, ptr, i64, [{n} x i8] }} {{ i64 -1, ptr @vt.java_lang_String, i64 {n}, [{n} x i8] c\"{esc}\" }}",
            n = bytes.len(),
            esc = escape_ll(bytes),
        )
        .unwrap();
    }
    // Class object singletons for EVERY class (reflection: getClass/getName/
    // getSimpleName). Immortal header {refcount=-1, rcflags, vtable=null},
    // then name and simpleName JStr pointers. Pointer identity replaces Java's
    // Class equality; the type descriptors link to these (getClass).
    let _ = &program.class_objects; // (former reflection path, now general)
    for c in &program.classes {
        let dotted = c.name.replace('/', ".");
        let simple = dotted.rsplit(['.', '$']).next().unwrap_or(&dotted).to_string();
        let s = sanitize(&c.name);
        emit_jstr_const(w, &format!("jclassname.{s}"), dotted.as_bytes());
        emit_jstr_const(w, &format!("jclasssimple.{s}"), simple.as_bytes());
        writeln!(
            w,
            "@jclass.{s} = internal unnamed_addr constant {{ i64, ptr, ptr, ptr }} \
             {{ i64 -1, ptr null, ptr @jclassname.{s}, ptr @jclasssimple.{s} }}",
        )
        .unwrap();
    }
    writeln!(w).unwrap();

    // Struct types: { i64 refcount, i64 rcflags, ptr vtable, fields… }.
    for c in &program.classes {
        let mut parts = vec!["i64".to_string(), "ptr".to_string()];
        parts.extend(ctx.flatten_fields(&c.name).iter().map(|(_, _, t)| llty(*t).to_string()));
        writeln!(w, "{} = type {{ {} }}", ctx.struct_name(&c.name), parts.join(", ")).unwrap();
    }
    // Array types (header + i64 length + flexible element field) and their
    // vtables. int[] has no ref elements → no-op drop/trace; ref[]
    // releases/visits its elements via runtime helpers.
    // Header (packed 16 B): refcount, vtable, length, elem_size (then elements).
    writeln!(w, "%arr.int = type {{ i64, ptr, i64, i64, [0 x i32] }}").unwrap();
    writeln!(w, "%arr.ref = type {{ i64, ptr, i64, i64, [0 x ptr] }}").unwrap();
    // Arrays have no type descriptor (slot 2 = null → instanceof false).
    writeln!(w, "@vt.array.int = internal unnamed_addr constant [3 x ptr] [ptr @jrt_noop_drop, ptr @jrt_noop_trace, ptr null]").unwrap();
    writeln!(w, "@vt.array.ref = internal unnamed_addr constant [3 x ptr] [ptr @jrt_array_ref_drop, ptr @jrt_array_ref_trace, ptr null]").unwrap();
    writeln!(w).unwrap();

    // Type descriptors for instanceof: { ptr super, ptr name }. The chain
    // ends at null (Object/non-modeled base). jrt_instanceof walks
    // it; the name (dotted) serves the uncaught message.
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
        // Transitive interface set as a null-terminated array of type
        // descriptors (for instanceof/checkcast against interfaces).
        let ifaces: Vec<String> = program
            .all_interfaces(&c.name)
            .iter()
            .filter(|i| program.class(i).is_some())
            .map(|i| format!("ptr @td.{}", sanitize(i)))
            .collect();
        let ifaces_ref = if ifaces.is_empty() {
            "null".to_string()
        } else {
            let n = ifaces.len() + 1;
            writeln!(
                w,
                "@ifaces.{} = internal constant [{n} x ptr] [{}, ptr null]",
                sanitize(&c.name),
                ifaces.join(", "),
            )
            .unwrap();
            format!("@ifaces.{}", sanitize(&c.name))
        };
        writeln!(
            w,
            "@td.{s} = internal constant {{ ptr, ptr, ptr, ptr }} \
             {{ ptr {super_td}, ptr @cname.{s}, ptr @jclass.{s}, ptr {ifaces_ref} }}",
            s = sanitize(&c.name),
        )
        .unwrap();
    }
    writeln!(w).unwrap();

    // Static fields as global variables (with ConstantValue initial value).
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

    // Vtables for instantiated classes. Slots whose implementation fell
    // victim to pruning (RTA-dead) become null — no reachable
    // site can dispatch there.
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
    // Strings/wrappers take part in virtual dispatch (equals/hashCode/
    // toString) → their own vtable, even though they are not created via `new`.
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
    // (valueOf function, class, dynamic vtable pointer)
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
        // Slot 0: drop, slot 1: trace (cycle collector); then the global
        // interface slots, then the class's own virtual methods.
        let mut entries = vec![
            format!("ptr @drop.{}", sanitize(class)),
            format!("ptr @trace.{}", sanitize(class)),
            format!("ptr @td.{}", sanitize(class)),
        ];
        // jrt_* symbols are runtime functions (external), considered valid.
        let sym_entry = |sym: Option<String>| match sym {
            Some(s) if s.starts_with("jrt_") || defined.contains(s.as_str()) => format!("ptr @{s}"),
            _ => "ptr null".to_string(),
        };
        for (iface, name, desc) in &ctx.iface_slots {
            let sym = if iface == "java/lang/Object" {
                // Root method: the class's override or the Object default.
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
    // Vire programs use string literals (print) but define no
    // java/lang/String class → its vtable is missing while the @jstr constants
    // reference it. Supply a minimal vtable (only no-op drop/trace +
    // null type descriptor); string method dispatch does not exist in Vire yet.
    // The class-name constants (@jclassname.*) are also @jstr → String vtable.
    if !instantiated.contains("java/lang/String") && (!program.strings.is_empty() || !program.classes.is_empty()) {
        writeln!(w, "@vt.java_lang_String = internal unnamed_addr constant [3 x ptr] [ptr @jrt_noop_drop, ptr @jrt_noop_trace, ptr null]").unwrap();
    }
    writeln!(w).unwrap();

    // Drop functions: release the object's ref fields (the runtime
    // descends recursively via jrt_release).
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

    // Trace functions: call the collector visitor on each ref field.
    // The Bacon-Rajan collector uses them to traverse object graphs
    // without knowing the field structure.
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

    // Error/exception helpers are cold: marked `cold`, LLVM moves their
    // calls out of the hot path (better block layout, branch prediction
    // like Rust's `#[cold]` panic). No `noreturn` — Vire's pending model returns.
    const COLD: &[&str] = &["jrt_throw_npe", "jrt_throw_bounds", "jrt_throw", "jrt_check_uncaught"];
    for (name, sig) in RUNTIME_DECLS {
        let (ret, params) = sig.split_once(' ').unwrap();
        let attr = if COLD.contains(name) { " cold" } else { "" };
        writeln!(w, "declare {ret} @{name}{params}{attr}").unwrap();
    }

    // Declare functions that are called but not defined.
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

    // Thread trampoline: called by the runtime (pthread or synchronous),
    // dispatches run() on the Runnable via the global vtable slot.
    if let Some(slot) = ctx.vtable_index("java/lang/Runnable", "run", "()V") {
        writeln!(w, "define void @jrt_invoke_runnable(ptr %r) {{").unwrap();
        writeln!(w, "  %vtp = getelementptr ptr, ptr %r, i64 {VTABLE_WORD}").unwrap();
        writeln!(w, "  %vt = load ptr, ptr %vtp, !invariant.load !{}", ctx.md_inv).unwrap();
        writeln!(w, "  %sp = getelementptr ptr, ptr %vt, i64 {slot}").unwrap();
        writeln!(w, "  %fn = load ptr, ptr %sp").unwrap();
        writeln!(w, "  call void %fn(ptr %r)").unwrap();
        writeln!(w, "  ret void").unwrap();
        writeln!(w, "}}").unwrap();
    }

    if defined.contains("java_main") {
        writeln!(w, "define i32 @main() {{").unwrap();
        // Vtable pointer for String/wrapper objects created at runtime —
        // only if the String class occurs at all (Vire programs without
        // strings do not define `@vt.java_lang_String`).
        if instantiated.contains("java/lang/String") {
            writeln!(w, "  store ptr @vt.java_lang_String, ptr @jrt_dyn_string_vt").unwrap();
        }
        for (vf, cls, vt) in &wrappers {
            if calls_fn(vf) {
                writeln!(w, "  store ptr @vt.{}, ptr @{vt}", sanitize(cls)).unwrap();
            }
        }
        // Static initializers before main, superclass before subclass.
        let mut emitted: BTreeSet<String> = BTreeSet::new();
        for c in &program.classes {
            emit_clinit_chain(w, &ctx, &c.name, &defined, &mut emitted);
        }
        writeln!(w, "  call void @java_main()").unwrap();
        // Release static ref fields (GC roots until program end) —
        // keeps the heap balance clean.
        for c in &program.classes {
            for f in &c.static_fields {
                if f.ty == Ty::Ref {
                    let t = format!("%sf_{}_{}", sanitize(&c.name), sanitize(&f.name));
                    writeln!(w, "  {t} = load ptr, ptr @sf.{}.{}", sanitize(&c.name), sanitize(&f.name)).unwrap();
                    writeln!(w, "  call void @jrt_release(ptr {t})").unwrap();
                }
            }
        }
        // Report an unhandled exception from main (instead of silently ignoring it).
        writeln!(w, "  call void @jrt_check_uncaught()").unwrap();
        writeln!(w, "  ret i32 0").unwrap();
        writeln!(w, "}}").unwrap();
    }

    // TBAA metadata tree: root !0, one type node + access tag per field.
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
    // AOT hot path: the two shared branch_weights nodes (100:3 / 3:100).
    if ctx.tbaa.is_empty() {
        writeln!(w).unwrap();
    }
    writeln!(w, "!{} = !{{!\"branch_weights\", i32 100, i32 3}}", ctx.bw_then).unwrap();
    writeln!(w, "!{} = !{{!\"branch_weights\", i32 3, i32 100}}", ctx.bw_else).unwrap();
    writeln!(w, "!{} = !{{}}", ctx.md_inv).unwrap();

    out
}

/// AOT hot path: static loop estimation. For each conditional branch,
/// estimate which branch stays in a loop (hot). Reducible
/// CFG from our lowering: an edge `u → v` with `v ≤ u` is a
/// back edge (loop header `v`, latch `u`); blocks in `[v, u]` are in the
/// loop body. A branch with one target block in the body and the
/// other outside (loop exit) weights the body branch as hot.
/// Returns: block index → true (then hot) / false (else hot).
fn loop_branch_bias(f: &Function) -> std::collections::HashMap<usize, bool> {
    let succ = |bb: &BasicBlock| -> Vec<usize> {
        match &bb.terminator {
            Terminator::Goto(b) => vec![b.0 as usize],
            Terminator::Branch { then_blk, else_blk, .. } => vec![then_blk.0 as usize, else_blk.0 as usize],
            Terminator::Switch { default, cases, .. } => {
                let mut v: Vec<usize> = cases.iter().map(|(_, b)| b.0 as usize).collect();
                v.push(default.0 as usize);
                v
            }
            Terminator::Return(_) => vec![],
        }
    };
    // Loop spans: header v → largest latch index u (back edge u→v).
    let mut span_max: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for (u, bb) in f.blocks.iter().enumerate() {
        for v in succ(bb) {
            if v <= u {
                let e = span_max.entry(v).or_insert(u);
                if u > *e {
                    *e = u;
                }
            }
        }
    }
    let inloop = |t: usize| span_max.iter().any(|(&h, &l)| h <= t && t <= l);
    let mut bias = std::collections::HashMap::new();
    for (b, bb) in f.blocks.iter().enumerate() {
        if let Terminator::Branch { then_blk, else_blk, .. } = &bb.terminator {
            let (t, el) = (then_blk.0 as usize, else_blk.0 as usize);
            match (inloop(t), inloop(el)) {
                (true, false) => {
                    bias.insert(b, true);
                }
                (false, true) => {
                    bias.insert(b, false);
                }
                _ => {}
            }
        }
    }
    bias
}

/// Calls the <clinit> of `class`, but the superclass's first
/// (JVMS 5.5) — each at most once.
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
            // Static dependencies: if the <clinit> reads/creates another
            // class's statics (e.g. the enum-switch helper Main$1 calls
            // Dir.values() and reads Dir.N), that class's <clinit> must run first.
            // Java initializes lazily on first access; we do it eagerly, so pull
            // it forward topologically here (the emitted guard breaks any cycles).
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

/// Classes whose statics a `<clinit>` body touches (field/new/cast/
/// virtual access as well as direct calls into their methods) — candidates
/// that must be initialized before this `<clinit>`.
fn clinit_deps(ctx: &Ctx, f: &Function) -> BTreeSet<String> {
    // Symbol → declaring class, to map call targets to a class.
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

/// Runtime default implementation of an Object root method.
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
    /// Running index of the StackNew slots (%sn<k>), reserved in the entry block.
    sn: u32,
    /// Borrowed ref-parameter slots (never reassigned): RC elision — no
    /// entry retain, no cleanup release. The caller holds the reference for
    /// the call's duration (arguments are borrowed); copies into other locals
    /// retain themselves.
    borrowed: BTreeSet<u32>,
    /// Locals that hold only immortal values (StackNew/literal/null): RC-free.
    imm: BTreeSet<u32>,
    /// Borrow slots: non-parameter ref locals that hold exclusively copies
    /// of borrowed parameters (e.g. `this`) or null — RC-free, because the
    /// caller keeps the value alive for the call's duration (javac's `aload_0`
    /// reloads of `this` before every `getfield`).
    borrow: BTreeSet<u32>,
    /// Provably non-null locals — the inline null check on field accesses
    /// is omitted.
    nn: BTreeSet<u32>,
    /// Metadata ID for `!invariant.load` (immutable loads: array length,
    /// vtable). Lets LLVM hoist/CSE across loops.
    md_inv: usize,
}

impl FnEmitter<'_> {
    fn nonnull(&self, op: &Operand) -> bool {
        matches!(op, Operand::Copy(l) if self.nn.contains(&l.0))
    }
}

/// Locals that hold exclusively immortal values (stack objects, string/
/// class literals, null) — there retain/release are provably no-ops. Monotone
/// invalidation: starts optimistically (all ref non-parameters), removes
/// every local with a possibly heap-creating def until the fixpoint.
fn immortal_only_locals(f: &Function) -> BTreeSet<u32> {
    let n = f.locals.len();
    let n_params = f.params.len();
    let mut imm = vec![false; n];
    for l in n_params..n {
        if f.locals[l] == Ty::Ref {
            imm[l] = true;
        }
    }
    loop {
        let mut changed = false;
        for bb in &f.blocks {
            for st in &bb.statements {
                let (def, immortal): (Option<u32>, bool) = match st {
                    Statement::StackNew { dest, .. } => (Some(dest.0), true),
                    Statement::Assign(d, Rvalue::Use(op)) => {
                        let ip = match op {
                            Operand::ConstNull | Operand::ConstStr(_) | Operand::ConstClass(_) => true,
                            Operand::Copy(s) => imm[s.0 as usize],
                            _ => false,
                        };
                        (Some(d.0), ip)
                    }
                    Statement::Assign(d, _) => (Some(d.0), false),
                    Statement::New { dest, .. } => (Some(dest.0), false),
                    // `jrt_array_data` returns an INTERIOR pointer into the array (its
                    // data region), a borrow — it owns nothing, so its result must not be
                    // retained/released (releasing an interior pointer reads a bogus
                    // header). Treat it as immortal, like a stack/literal value.
                    Statement::Call { dest, func, .. } if func == "jrt_array_data" => (dest.map(|d| d.0), true),
                    Statement::Call { dest, .. }
                    | Statement::CallGuarded { dest, .. }
                    | Statement::CallVirtual { dest, .. }
                    | Statement::CallPoly { dest, .. } => (dest.map(|d| d.0), false),
                    Statement::GetField { dest, .. }
                    | Statement::GetStatic { dest, .. }
                    | Statement::NewArray { dest, .. }
                    | Statement::ArrayLoad { dest, .. } => (Some(dest.0), false),
                    _ => (None, false),
                };
                if let Some(d) = def {
                    if imm[d as usize] && !immortal {
                        imm[d as usize] = false;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    (0..n as u32).filter(|&l| imm[l as usize]).collect()
}

/// Per function, the statically written fields computed transitively (fixpoint over the
/// call graph). Direct `PutStatic` plus the effects of all callees; functions
/// with an unresolved virtual call are conservatively treated as "writes everything".
/// External/`jrt_` calls write no Java statics (C does not touch them).
/// Interprocedural instance-field write analysis: per function, the set of
/// (class, field) that it OR a transitive callee writes via `PutField`,
/// plus an `unknown` flag (opaque calls: virtual/poly/external → could write
/// anything). Basis for region inference: a `GetField` of a field that
/// the function does NOT (transitively) write and that no opaque call
/// can change may borrow (no retain/release) — even in functions
/// that call other (non-writing) user functions (the case v1 left out).
fn instance_field_writes(program: &Program) -> BTreeMap<String, (BTreeSet<(String, String)>, bool)> {
    let fn_names: BTreeSet<&str> = program.functions.iter().map(|f| f.name.as_str()).collect();
    let mut writes: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    let mut unknown: BTreeMap<String, bool> = BTreeMap::new();
    let mut callees: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for f in &program.functions {
        let mut d: BTreeSet<(String, String)> = BTreeSet::new();
        let mut c: BTreeSet<String> = BTreeSet::new();
        let mut op = false;
        for bb in &f.blocks {
            for st in &bb.statements {
                match st {
                    Statement::PutField { class, field, .. } => {
                        d.insert((class.clone(), field.clone()));
                    }
                    Statement::Call { func, .. } | Statement::CallGuarded { func, .. } => {
                        if fn_names.contains(func.as_str()) {
                            c.insert(func.clone());
                        } else if !func.starts_with("jrt_") {
                            op = true; // external/unknown → could change fields
                        }
                    }
                    Statement::CallVirtual { .. } => op = true,
                    Statement::CallPoly { targets, .. } => {
                        for (_, sym) in targets {
                            if fn_names.contains(sym.as_str()) {
                                c.insert(sym.clone());
                            } else {
                                op = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        writes.insert(f.name.clone(), d);
        unknown.insert(f.name.clone(), op);
        callees.insert(f.name.clone(), c);
    }
    // Fixpoint: propagate callee write sets and unknown upward.
    loop {
        let mut changed = false;
        let names: Vec<String> = writes.keys().cloned().collect();
        for name in &names {
            let cs = callees[name].clone();
            let mut add: BTreeSet<(String, String)> = BTreeSet::new();
            let mut op = unknown[name];
            for c in &cs {
                if let Some(w) = writes.get(c) {
                    for x in w {
                        if !writes[name].contains(x) {
                            add.insert(x.clone());
                        }
                    }
                }
                if unknown.get(c).copied().unwrap_or(true) {
                    op = true;
                }
            }
            if !add.is_empty() {
                writes.get_mut(name).unwrap().extend(add);
                changed = true;
            }
            if op != unknown[name] {
                unknown.insert(name.clone(), op);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    writes.into_iter().map(|(k, w)| (k.clone(), (w, unknown[&k]))).collect()
}

fn static_write_effects(program: &Program) -> BTreeMap<String, BTreeSet<(String, String)>> {
    let fn_names: BTreeSet<&str> = program.functions.iter().map(|f| f.name.as_str()).collect();
    let mut all_statics: BTreeSet<(String, String)> = BTreeSet::new();
    let mut writes: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    let mut callees: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut conservative: BTreeSet<String> = BTreeSet::new();
    for f in &program.functions {
        let mut d: BTreeSet<(String, String)> = BTreeSet::new();
        let mut c: BTreeSet<String> = BTreeSet::new();
        for bb in &f.blocks {
            for st in &bb.statements {
                match st {
                    Statement::PutStatic { class, field, .. } => {
                        d.insert((class.clone(), field.clone()));
                        all_statics.insert((class.clone(), field.clone()));
                    }
                    Statement::Call { func, .. } | Statement::CallGuarded { func, .. } => {
                        if fn_names.contains(func.as_str()) {
                            c.insert(func.clone());
                        }
                    }
                    Statement::CallVirtual { .. } => {
                        conservative.insert(f.name.clone());
                    }
                    Statement::CallPoly { targets, .. } => {
                        for (_, sym) in targets {
                            if fn_names.contains(sym.as_str()) {
                                c.insert(sym.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        writes.insert(f.name.clone(), d);
        callees.insert(f.name.clone(), c);
    }
    for name in &conservative {
        writes.get_mut(name).unwrap().extend(all_statics.iter().cloned());
    }
    // Fixpoint: propagate callee effects upward.
    loop {
        let mut changed = false;
        let names: Vec<String> = writes.keys().cloned().collect();
        for name in &names {
            let cs = callees[name].clone();
            let mut add: BTreeSet<(String, String)> = BTreeSet::new();
            for c in &cs {
                if let Some(w) = writes.get(c) {
                    for x in w {
                        if !writes[name].contains(x) {
                            add.insert(x.clone());
                        }
                    }
                }
            }
            if !add.is_empty() {
                writes.get_mut(name).unwrap().extend(add);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    writes
}

/// Borrow slots: non-parameter ref locals whose *every* definition is a copy
/// of a borrowed parameter, of another borrow slot, or an immortal
/// constant value (null/string/class literal). Such slots never own
/// a reference (the borrowed origin lives for the whole method), so
/// retain/release are provably superfluous. Sound, because heap stores (PutField/
/// aastore/PutStatic) retain the value themselves and so does `return` — a borrow
/// is handled correctly in every use position. Monotone invalidation
/// until the fixpoint.
fn borrow_slots(
    f: &Function,
    borrowed: &BTreeSet<u32>,
    static_writes: &BTreeSet<(String, String)>,
    field_writes: &(BTreeSet<(String, String)>, bool),
) -> BTreeSet<u32> {
    let n = f.locals.len();
    let n_params = f.params.len();
    let mut b = vec![false; n];
    for l in n_params..n {
        if f.locals[l] == Ty::Ref {
            b[l] = true;
        }
    }
    // Region inference (interprocedural): a `GetField` load of a field that
    // this function AND its transitive callees do NOT write (and that no
    // opaque call can change), from a stable (borrowed) base, is a
    // borrow — the field keeps the value alive, retain/release is dropped. This
    // now applies even in functions that call other (non-writing) user
    // functions (the case that the function-local v1 left out).
    let (written_fields, writes_unknown) = field_writes;
    let field_borrow_ok = |obj: &Operand, class: &str, field: &str, b: &[bool]| -> bool {
        !writes_unknown
            && !written_fields.contains(&(class.to_string(), field.to_string()))
            && matches!(obj, Operand::Copy(s) if borrowed.contains(&s.0) || b[s.0 as usize])
    };
    loop {
        let mut changed = false;
        for bb in &f.blocks {
            for st in &bb.statements {
                let (def, ok): (Option<u32>, bool) = match st {
                    Statement::Assign(d, Rvalue::Use(op)) if f.locals[d.0 as usize] == Ty::Ref => {
                        let ok = match op {
                            Operand::ConstNull | Operand::ConstStr(_) | Operand::ConstClass(_) => true,
                            Operand::Copy(s) => borrowed.contains(&s.0) || b[s.0 as usize],
                            _ => false,
                        };
                        (Some(d.0), ok)
                    }
                    Statement::Assign(d, _) if f.locals[d.0 as usize] == Ty::Ref => (Some(d.0), false),
                    // Stable static field (not written in this function):
                    // the static root keeps the value alive → borrow, no RC.
                    Statement::GetStatic { dest, class, field }
                        if f.locals[dest.0 as usize] == Ty::Ref =>
                    {
                        let stable = !static_writes.contains(&(class.clone(), field.clone()));
                        (Some(dest.0), stable)
                    }
                    // Traversal cursor: load of a never-written field from
                    // a stable base → borrow (see field_borrow_ok above).
                    Statement::GetField { dest, obj, class, field }
                        if f.locals[dest.0 as usize] == Ty::Ref =>
                    {
                        (Some(dest.0), field_borrow_ok(obj, class, field, &b))
                    }
                    Statement::New { dest, .. }
                    | Statement::StackNew { dest, .. }
                    | Statement::NewArray { dest, .. }
                    | Statement::ArrayLoad { dest, .. }
                        if f.locals[dest.0 as usize] == Ty::Ref =>
                    {
                        (Some(dest.0), false)
                    }
                    Statement::Call { dest, .. }
                    | Statement::CallGuarded { dest, .. }
                    | Statement::CallVirtual { dest, .. }
                    | Statement::CallPoly { dest, .. } => (dest.map(|d| d.0), false),
                    _ => (None, false),
                };
                if let Some(d) = def {
                    if b[d as usize] && !ok {
                        b[d as usize] = false;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    (0..n as u32).filter(|&l| b[l as usize]).collect()
}

/// Provably non-null ref locals: `this` (in instance methods, `receiver_
/// nonnull`), New/StackNew results, and copies of them. Allows omitting the
/// inline null check on field accesses (the caller checks the receiver;
/// `this.f` would otherwise re-check redundantly per access — hot virtual methods).
fn non_null_locals(f: &Function) -> BTreeSet<u32> {
    let n = f.locals.len();
    let mut nn = vec![false; n];
    if f.receiver_nonnull && !f.params.is_empty() && f.params[0] == Ty::Ref {
        nn[0] = true;
    }
    for bb in &f.blocks {
        for st in &bb.statements {
            if let Statement::New { dest, .. }
            | Statement::StackNew { dest, .. }
            | Statement::NewArray { dest, .. } = st
            {
                nn[dest.0 as usize] = true;
            }
        }
    }
    // Copies of non-null values: fixpoint.
    loop {
        let mut changed = false;
        for bb in &f.blocks {
            for st in &bb.statements {
                if let Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) = st {
                    if nn[s.0 as usize] && !nn[d.0 as usize] {
                        nn[d.0 as usize] = true;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Strike out locals with any possibly-null def again (flow-
    // insensitive conservative): every non-nn def invalidates.
    let mut maybe_null = vec![false; n];
    for bb in &f.blocks {
        for st in &bb.statements {
            let (def, ok) = match st {
                Statement::New { dest, .. }
                | Statement::StackNew { dest, .. }
                | Statement::NewArray { dest, .. } => (Some(dest.0), true),
                Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) => (Some(d.0), nn[s.0 as usize]),
                Statement::Assign(d, _) => (Some(d.0), false),
                Statement::GetField { dest, .. }
                | Statement::GetStatic { dest, .. }
                | Statement::ArrayLoad { dest, .. }
                | Statement::InstanceOf { dest, .. }
                | Statement::InstanceOfPending { dest, .. }
                | Statement::ArrayLen { dest, .. } => (Some(dest.0), false),
                Statement::Call { dest, .. }
                | Statement::CallGuarded { dest, .. }
                | Statement::CallVirtual { dest, .. }
                | Statement::CallPoly { dest, .. } => (dest.map(|d| d.0), false),
                _ => (None, false),
            };
            if let Some(d) = def {
                if !ok {
                    maybe_null[d as usize] = true;
                }
            }
        }
    }
    // `this` (local 0) is non-null despite the non-def; it is never redefined
    // in correct bytecode, but guard the case.
    if f.receiver_nonnull && !f.params.is_empty() && f.params[0] == Ty::Ref {
        maybe_null[0] = false;
    }
    (0..n as u32).filter(|&l| nn[l as usize] && !maybe_null[l as usize]).collect()
}

/// Locals that appear anywhere as a write target (for the borrow analysis).
fn written_locals(f: &Function) -> BTreeSet<u32> {
    let mut w = BTreeSet::new();
    let mut mark = |l: &Local| {
        w.insert(l.0);
    };
    for bb in &f.blocks {
        for st in &bb.statements {
            match st {
                Statement::Assign(d, _)
                | Statement::New { dest: d, .. }
                | Statement::StackNew { dest: d, .. }
                | Statement::GetField { dest: d, .. }
                | Statement::GetStatic { dest: d, .. }
                | Statement::NewArray { dest: d, .. }
                | Statement::ArrayLen { dest: d, .. }
                | Statement::ArrayLoad { dest: d, .. }
                | Statement::InstanceOf { dest: d, .. }
                | Statement::InstanceOfPending { dest: d, .. } => mark(d),
                Statement::Call { dest, .. }
                | Statement::CallGuarded { dest, .. }
                | Statement::CallVirtual { dest, .. }
                | Statement::CallPoly { dest, .. } => {
                    if let Some(d) = dest {
                        mark(d);
                    }
                }
                _ => {}
            }
        }
    }
    w
}

impl<'a> FnEmitter<'a> {
    fn fresh(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    /// Fresh LLVM block label (for mid-block branches like the
    /// null skip on field/receiver accesses).
    fn fresh_label(&mut self) -> String {
        self.label += 1;
        format!("nz{}", self.label)
    }

    /// Materializes an operand as an SSA value; locals are loaded.
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
            // LLVM requires exact double literals → bit pattern as hex.
            Operand::ConstF64(v) => format!("0x{:016X}", v.to_bits()),
            // float literals in LLVM: hex of the exactly promoted double value.
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
    // Ref locals must be null before the first (cleanup) load, so that the
    // bulk release at the function end dereferences no garbage.
    let n_params = f.params.len();
    for (i, ty) in f.locals.iter().enumerate() {
        if *ty == Ty::Ref && i >= n_params {
            writeln!(w, "  store ptr null, ptr %l{i}").unwrap();
        }
    }
    // Borrow analysis: never-reassigned ref parameters stay borrowed
    // (RC elision). `this` in instance methods is almost always like this.
    let written = written_locals(f);
    let borrowed: BTreeSet<u32> = (0..n_params as u32)
        .filter(|i| f.params[*i as usize] == Ty::Ref && !written.contains(i))
        .collect();
    for (i, ty) in f.params.iter().enumerate() {
        writeln!(w, "  store {} %p{i}, ptr %l{i}", llty(*ty)).unwrap();
        // Ref parameters: retain (→ owned, cleanup may release uniformly), except
        // borrowed ones (never reassigned) — there retain/release is redundant.
        if *ty == Ty::Ref && !borrowed.contains(&(i as u32)) {
            writeln!(w, "  call void @jrt_retain(ptr %p{i})").unwrap();
        }
    }
    // Reserve StackNew object storage ahead in the entry block (%sn<k>), in
    // statement order — so in loops the slot is a fixed,
    // reused alloca instead of an allocation per iteration.
    let mut snk = 0u32;
    for bb in &f.blocks {
        for st in &bb.statements {
            if let Statement::StackNew { class, .. } = st {
                writeln!(w, "  %sn{snk} = alloca {}", ctx.struct_name(class)).unwrap();
                snk += 1;
            }
        }
    }
    writeln!(w, "  br label %bb0").unwrap();

    let imm = immortal_only_locals(f);
    let empty_writes = BTreeSet::new();
    let sw = ctx.static_writes.get(&f.name).unwrap_or(&empty_writes);
    let empty_fw = (BTreeSet::new(), true); // unknown → conservative (no borrow)
    let fw = ctx.field_writes.get(&f.name).unwrap_or(&empty_fw);
    let borrow = borrow_slots(f, &borrowed, sw, fw);
    let nn = non_null_locals(f);
    let mut e = FnEmitter { f, tmp: 0, label: 0, borrowed, sn: 0, imm, borrow, nn, md_inv: ctx.md_inv };
    // AOT hot path: static loop estimation → which branch stays
    // in the loop (hot). Sets `!prof` weights, LLVM optimizes the rest.
    // `FASTLLVM_NO_PROF` turns the weights off (for A/B measurement of the ceiling).
    let bw_bias = if std::env::var_os("FASTLLVM_NO_PROF").is_some() {
        std::collections::HashMap::new()
    } else {
        loop_branch_bias(f)
    };

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
                // `!prof` branch weights from the loop estimation: the branch
                // that stays in the loop is hot.
                let prof = match bw_bias.get(&bi) {
                    Some(true) => format!(", !prof !{}", ctx.bw_then),
                    Some(false) => format!(", !prof !{}", ctx.bw_else),
                    None => String::new(),
                };
                writeln!(w, "  br i1 {b}, label %bb{}, label %bb{}{prof}", then_blk.0, else_blk.0).unwrap();
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
                emit_cleanup(w, ctx, &mut e);
                writeln!(w, "  ret void").unwrap();
            }
            Terminator::Return(Some(op)) => {
                let ty = operand_ty(f, op);
                let v = e.operand(w, op);
                // The returned ref must survive the cleanup → retain, then
                // the caller transfers the +1.
                if ty == Ty::Ref {
                    writeln!(w, "  call void @jrt_retain(ptr {v})").unwrap();
                }
                emit_cleanup(w, ctx, &mut e);
                writeln!(w, "  ret {} {v}", llty(ty)).unwrap();
            }
        }
    }
    writeln!(w, "}}\n").unwrap();
}

/// Releases all of the function's ref locals (owning-slot model): each
/// ref local holds a reference that ends when the function is left.
///
/// Stack-allocated objects (`StackNew`, immortal) need no field
/// release: the field-sensitive escape analysis promotes container and contents
/// only together (both-or-neither), so a stack container holds exclusively
/// immortal contents — nothing that could leak.
fn emit_cleanup(w: &mut String, _ctx: &Ctx, e: &mut FnEmitter) {
    for (i, ty) in e.f.locals.iter().enumerate() {
        // Borrowed parameters (never retained) and immortal-only slots (only no-op
        // values) need no cleanup release.
        if *ty == Ty::Ref
            && !e.borrowed.contains(&(i as u32))
            && !e.imm.contains(&(i as u32))
            && !e.borrow.contains(&(i as u32))
        {
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
                        // Floating point → integer (truncation).
                        (Ty::F64, Ty::I64) | (Ty::F64, Ty::I32) | (Ty::F32, Ty::I64) | (Ty::F32, Ty::I32) => "fptosi",
                        _ => panic!("unexpected conversion {from:?} -> {to:?}"),
                    };
                    writeln!(w, "  {t} = {inst} {} {v} to {}", llty(from), llty(to)).unwrap();
                    t
                }
            };
            let _ = dty;
            // Copies/constants into the ref local are borrowed → retain the new,
            // release the old (store_dest). Non-ref: a plain store.
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
                    // Ref return transfers +1 (no retain).
                    store_dest(w, e, *d, &t, false);
                }
            }
        }
        // Devirtualized instance call with catchable receiver NPE.
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
                .unwrap_or_else(|| panic!("vtable slot {class}.{name}{desc} missing"));
            let recv = e.operand(w, &args[0]);
            // Materialize the remaining arguments before branching (may be
            // used in both branches).
            let mut avs = vec![format!("ptr {recv}")];
            for a in &args[1..] {
                let ty = llty(operand_ty(e.f, a));
                let v = e.operand(w, a);
                avs.push(format!("{ty} {v}"));
            }
            let _ = params;
            // Catchable receiver NPE: on null go to the npe block, otherwise dispatch.
            let (nb, ok, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label());
            let isnull = e.fresh();
            writeln!(w, "  {isnull} = icmp eq ptr {recv}, null").unwrap();
            writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
            writeln!(w, "{nb}:").unwrap();
            writeln!(w, "  call void @jrt_throw_npe()").unwrap();
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{ok}:").unwrap();
            // The vtable sits in the header (after refcount + rcflags).
            let vtp = e.fresh();
            writeln!(w, "  {vtp} = getelementptr ptr, ptr {recv}, i64 {VTABLE_WORD}").unwrap();
            let vt = e.fresh();
            writeln!(w, "  {vt} = load ptr, ptr {vtp}, !invariant.load !{}", e.md_inv).unwrap();
            let slotp = e.fresh();
            writeln!(w, "  {slotp} = getelementptr ptr, ptr {vt}, i64 {slot}").unwrap();
            let fnp = e.fresh();
            writeln!(w, "  {fnp} = load ptr, ptr {slotp}").unwrap();
            match dest {
                None => writeln!(w, "  call {} {fnp}({})", llty(*ret), avs.join(", ")).unwrap(),
                Some(d) => {
                    let t = e.fresh();
                    writeln!(w, "  {t} = call {} {fnp}({})", llty(*ret), avs.join(", ")).unwrap();
                    // Ref return transfers +1 (no retain).
                    store_dest(w, e, *d, &t, false);
                }
            }
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{cont}:").unwrap();
        }
        Statement::CallPoly { dest, ret, args, targets } => {
            let recv = e.operand(w, &args[0]);
            // Materialize the arguments once (valid in all branches).
            let mut avs = vec![format!("ptr {recv}")];
            for a in &args[1..] {
                let ty = llty(operand_ty(e.f, a));
                let v = e.operand(w, a);
                avs.push(format!("{ty} {v}"));
            }
            let avs = avs.join(", ");
            let cont = e.fresh_label();
            // Catchable receiver NPE: on null → npe block.
            let (nb, ok) = (e.fresh_label(), e.fresh_label());
            let isnull = e.fresh();
            writeln!(w, "  {isnull} = icmp eq ptr {recv}, null").unwrap();
            writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
            writeln!(w, "{nb}:").unwrap();
            writeln!(w, "  call void @jrt_throw_npe()").unwrap();
            writeln!(w, "  br label %{cont}").unwrap();
            writeln!(w, "{ok}:").unwrap();
            // Load the receiver's vtable pointer.
            let vtp = e.fresh();
            writeln!(w, "  {vtp} = getelementptr ptr, ptr {recv}, i64 {VTABLE_WORD}").unwrap();
            let vt = e.fresh();
            writeln!(w, "  {vt} = load ptr, ptr {vtp}, !invariant.load !{}", e.md_inv).unwrap();
            // Cascade: one vtable comparison per class → direct call; the
            // last target is the else branch (closed world: the receiver is
            // guaranteed to be one of the instantiated target classes).
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
                    // last target: unconditional (else)
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
            // sizeof via a GEP constant; jrt_alloc zeroes fields and sets
            // refcount=1 (Java default values + first reference).
            writeln!(
                w,
                "  {t} = call ptr @jrt_alloc(i64 ptrtoint (ptr getelementptr ({sn}, ptr null, i32 1) to i64))"
            )
            .unwrap();
            store_vtable(w, e, &t, class);
            store_dest(w, e, *dest, &t, false); // alloc gave +1
        }
        Statement::StackNew { dest, class } => {
            let sn = ctx.struct_name(class);
            // The alloca slot is pre-reserved in the entry block (%sn<k>) —
            // otherwise a StackNew in a loop would allocate stack per
            // iteration (overflow). Here only (re)initialize: refcount=-1
            // makes the object immortal (retain/release = no-op, never freed).
            let t = format!("%sn{}", e.sn);
            e.sn += 1;
            writeln!(w, "  store {sn} zeroinitializer, ptr {t}").unwrap();
            writeln!(w, "  store i64 -1, ptr {t}").unwrap();
            store_vtable(w, e, &t, class);
            store_dest(w, e, *dest, &t, false);
        }
        Statement::GetField { dest, obj, class, field } => {
            let (owner, idx, ty) = ctx
                .field_slot(class, field)
                .unwrap_or_else(|| panic!("field {class}.{field} missing"));
            let nonnull = e.nonnull(obj);
            let o = e.operand(w, obj);
            // Catchable NPE: on null go to the npe block (pending), otherwise access.
            // For a provably non-null receiver (e.g. `this`) the check is omitted.
            let cont = if nonnull {
                None
            } else {
                let (nb, ok, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label());
                let isnull = e.fresh();
                writeln!(w, "  {isnull} = icmp eq ptr {o}, null").unwrap();
                writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
                writeln!(w, "{nb}:").unwrap();
                writeln!(w, "  call void @jrt_throw_npe()").unwrap();
                writeln!(w, "  br label %{cont}").unwrap();
                writeln!(w, "{ok}:").unwrap();
                Some(cont)
            };
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr {}, ptr {o}, i32 0, i32 {idx}", ctx.struct_name(&owner)).unwrap();
            let t = e.fresh();
            writeln!(w, "  {t} = load {}, ptr {p}{}", llty(ty), ctx.tbaa_suffix(&owner, field)).unwrap();
            // The field value is borrowed; the copy into the local becomes owned → retain.
            store_dest(w, e, *dest, &t, true);
            if let Some(cont) = cont {
                writeln!(w, "  br label %{cont}").unwrap();
                writeln!(w, "{cont}:").unwrap();
            }
        }
        Statement::PutField { obj, class, field, value } => {
            let (owner, idx, ty) = ctx
                .field_slot(class, field)
                .unwrap_or_else(|| panic!("field {class}.{field} missing"));
            let nonnull = e.nonnull(obj);
            let o = e.operand(w, obj);
            let v = e.operand(w, value);
            let cont = if nonnull {
                None
            } else {
                let (nb, ok, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label());
                let isnull = e.fresh();
                writeln!(w, "  {isnull} = icmp eq ptr {o}, null").unwrap();
                writeln!(w, "  br i1 {isnull}, label %{nb}, label %{ok}").unwrap();
                writeln!(w, "{nb}:").unwrap();
                writeln!(w, "  call void @jrt_throw_npe()").unwrap();
                writeln!(w, "  br label %{cont}").unwrap();
                writeln!(w, "{ok}:").unwrap();
                Some(cont)
            };
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr {}, ptr {o}, i32 0, i32 {idx}", ctx.struct_name(&owner)).unwrap();
            let tb = ctx.tbaa_suffix(&owner, field);
            if ty == Ty::Ref {
                // The field takes over an owning reference: retain new, release old.
                // `retain(null)` is a provable no-op (constant null) →
                // omit it (saves a call per `x.f = null` / field init to null;
                // helps allocation-heavy code like `Tree(null, null)`).
                if !matches!(value, Operand::ConstNull) {
                    writeln!(w, "  call void @jrt_retain(ptr {v})").unwrap();
                }
                let old = e.fresh();
                writeln!(w, "  {old} = load ptr, ptr {p}{tb}").unwrap();
                writeln!(w, "  store ptr {v}, ptr {p}{tb}").unwrap();
                writeln!(w, "  call void @jrt_release(ptr {old})").unwrap();
            } else {
                writeln!(w, "  store {} {v}, ptr {p}{tb}", llty(ty)).unwrap();
            }
            if let Some(cont) = cont {
                writeln!(w, "  br label %{cont}").unwrap();
                writeln!(w, "{cont}:").unwrap();
            }
        }
        Statement::GetStatic { dest, class, field } => {
            let (g, ty) = ctx.static_field(class, field).unwrap_or_else(|| panic!("static field {class}.{field} missing"));
            let t = e.fresh();
            writeln!(w, "  {t} = load {}, ptr {g}", llty(ty)).unwrap();
            // Ref copied from a global field into the local → owned (retain).
            store_dest(w, e, *dest, &t, ty == Ty::Ref);
        }
        Statement::PutStatic { class, field, value } => {
            let (g, ty) = ctx.static_field(class, field).unwrap_or_else(|| panic!("static field {class}.{field} missing"));
            let v = e.operand(w, value);
            if ty == Ty::Ref {
                if !matches!(value, Operand::ConstNull) {
                    writeln!(w, "  call void @jrt_retain(ptr {v})").unwrap();
                }
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
        Statement::NewArray { dest, kind, len } => {
            let n = e.operand(w, len);
            let n64 = e.fresh();
            writeln!(w, "  {n64} = sext i32 {n} to i64").unwrap();
            let t = e.fresh();
            writeln!(
                w,
                "  {t} = call ptr @jrt_alloc_array(i64 {n64}, i64 {}, ptr {})",
                kind.size(),
                array_vtable(*kind),
            )
            .unwrap();
            store_dest(w, e, *dest, &t, false); // alloc gave +1
        }
        Statement::ArrayLen { dest, arr } => {
            let a = e.operand(w, arr);
            let t = e.fresh();
            writeln!(w, "  {t} = call i32 @jrt_arraylen(ptr {a})").unwrap();
            writeln!(w, "  store i32 {t}, ptr %l{}", dest.0).unwrap();
        }
        Statement::ArrayLoad { dest, arr, index, kind, checked } => {
            let a = e.operand(w, arr);
            let i = e.operand(w, index);
            let vty = kind.value_ty();
            let _ = vty;
            // Inline (even when checked): null/bounds tests set pending via
            // jrt_throw_npe/jrt_throw_bounds, the access stays a visible
            // load — LLVM hoists the length check out of loops and can
            // vectorize, instead of an opaque jrt_?aload call per element.
            let ann = e.nonnull(arr);
            let v = if *checked && std::env::var_os("FASTLLVM_NO_BOUNDS").is_none() {
                emit_array_elem_load_checked(w, e, &a, &i, *kind, ann)
            } else {
                emit_array_elem_load(w, e, &a, &i, *kind)
            };
            store_dest(w, e, *dest, &v, kind.is_ref());
        }
        Statement::ArrayStore { arr, index, value, kind, checked } => {
            let a = e.operand(w, arr);
            let i = e.operand(w, index);
            let v = e.operand(w, value);
            let vty = kind.value_ty();
            // Ref stores checked via the runtime (jrt_aastore carries the
            // covariance/ArrayStoreException check that the inline path would
            // not have). Primitive stores are checked inline.
            let bck = *checked && std::env::var_os("FASTLLVM_NO_BOUNDS").is_none();
            if bck && kind.is_ref() {
                writeln!(w, "  call void @{}(ptr {a}, i32 {i}, {} {v})", arr_store_fn(*kind), llty(vty)).unwrap();
            } else if bck {
                emit_array_elem_store_checked(w, e, &a, &i, &v, *kind, e.nonnull(arr));
            } else {
                emit_array_elem_store(w, e, &a, &i, &v, *kind);
            }
        }
    }
}

fn arr_store_fn(k: ArrKind) -> &'static str {
    match k {
        ArrKind::Bool | ArrKind::Byte => "jrt_bastore",
        ArrKind::Char => "jrt_castore",
        ArrKind::Short => "jrt_sastore",
        ArrKind::Int => "jrt_iastore",
        ArrKind::Long => "jrt_lastore",
        ArrKind::Float => "jrt_fastore",
        ArrKind::Double => "jrt_dastore",
        ArrKind::Ref => "jrt_aastore",
    }
}
/// LLVM storage type of an array element.
fn arr_store_ty(k: ArrKind) -> &'static str {
    match k {
        ArrKind::Bool | ArrKind::Byte => "i8",
        ArrKind::Char | ArrKind::Short => "i16",
        ArrKind::Int => "i32",
        ArrKind::Long => "i64",
        ArrKind::Float => "float",
        ArrKind::Double => "double",
        ArrKind::Ref => "ptr",
    }
}
fn emit_array_elem_load(w: &mut String, e: &mut FnEmitter, a: &str, i: &str, k: ArrKind) -> String {
    let i64v = e.fresh();
    writeln!(w, "  {i64v} = sext i32 {i} to i64").unwrap();
    let off = e.fresh();
    writeln!(w, "  {off} = mul i64 {i64v}, {}", k.size()).unwrap();
    let base = e.fresh();
    writeln!(w, "  {base} = getelementptr i8, ptr {a}, i64 32").unwrap();
    let ep = e.fresh();
    writeln!(w, "  {ep} = getelementptr i8, ptr {base}, i64 {off}").unwrap();
    let raw = e.fresh();
    writeln!(w, "  {raw} = load {}, ptr {ep}", arr_store_ty(k)).unwrap();
    // Extend narrow types to i32 (byte/short signed, bool/char unsigned).
    match k {
        ArrKind::Byte | ArrKind::Short => {
            let x = e.fresh();
            writeln!(w, "  {x} = sext {} {raw} to i32", arr_store_ty(k)).unwrap();
            x
        }
        ArrKind::Bool | ArrKind::Char => {
            let x = e.fresh();
            writeln!(w, "  {x} = zext {} {raw} to i32", arr_store_ty(k)).unwrap();
            x
        }
        _ => raw,
    }
}
fn emit_array_elem_store(w: &mut String, e: &mut FnEmitter, a: &str, i: &str, v: &str, k: ArrKind) {
    let i64v = e.fresh();
    writeln!(w, "  {i64v} = sext i32 {i} to i64").unwrap();
    let off = e.fresh();
    writeln!(w, "  {off} = mul i64 {i64v}, {}", k.size()).unwrap();
    let base = e.fresh();
    writeln!(w, "  {base} = getelementptr i8, ptr {a}, i64 32").unwrap();
    let ep = e.fresh();
    writeln!(w, "  {ep} = getelementptr i8, ptr {base}, i64 {off}").unwrap();
    // Truncate the value to the storage width (byte/char/short).
    let sv = match k {
        ArrKind::Bool | ArrKind::Byte | ArrKind::Char | ArrKind::Short => {
            let x = e.fresh();
            writeln!(w, "  {x} = trunc i32 {v} to {}", arr_store_ty(k)).unwrap();
            x
        }
        _ => v.to_string(),
    };
    writeln!(w, "  store {} {sv}, ptr {ep}", arr_store_ty(k)).unwrap();
}

/// Neutral value (LLVM literal) for the error branch of a checked load.
fn zero_lit(vty: Ty) -> &'static str {
    match vty {
        Ty::Ref => "null",
        Ty::F32 | Ty::F64 => "0.0",
        _ => "0",
    }
}

/// Checked load, fully inline: null test → NPE, `idx (unsigned) >= length`
/// → bounds (both set pending and yield a neutral value; the pending check
/// inserted by the frontend then takes over control flow). The actual
/// access stays an LLVM `load` (hoistable/vectorizable).
fn emit_array_elem_load_checked(w: &mut String, e: &mut FnEmitter, a: &str, i: &str, k: ArrKind, nn: bool) -> String {
    let vty = k.value_ty();
    let sty = arr_store_ty(k);
    let (npe, ck, bd, ld, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label(), e.fresh_label(), e.fresh_label());
    // Null check only if the array is NOT provably non-null (created locally via
    // `array(n)` or similar). Saves a branch per access in tight loops.
    if !nn {
        let isnull = e.fresh();
        writeln!(w, "  {isnull} = icmp eq ptr {a}, null").unwrap();
        writeln!(w, "  br i1 {isnull}, label %{npe}, label %{ck}").unwrap();
        writeln!(w, "{npe}:").unwrap();
        writeln!(w, "  call void @jrt_throw_npe()").unwrap();
        writeln!(w, "  br label %{cont}").unwrap();
        writeln!(w, "{ck}:").unwrap();
    }
    let lenp = e.fresh();
    writeln!(w, "  {lenp} = getelementptr i8, ptr {a}, i64 16").unwrap();
    let len = e.fresh();
    writeln!(w, "  {len} = load i64, ptr {lenp}, !invariant.load !{}", e.md_inv).unwrap();
    let idx = e.fresh();
    writeln!(w, "  {idx} = sext i32 {i} to i64").unwrap();
    let oob = e.fresh();
    writeln!(w, "  {oob} = icmp uge i64 {idx}, {len}").unwrap();
    writeln!(w, "  br i1 {oob}, label %{bd}, label %{ld}").unwrap();
    writeln!(w, "{bd}:").unwrap();
    writeln!(w, "  call void @jrt_throw_bounds()").unwrap();
    writeln!(w, "  br label %{cont}").unwrap();
    writeln!(w, "{ld}:").unwrap();
    let off = e.fresh();
    writeln!(w, "  {off} = mul i64 {idx}, {}", k.size()).unwrap();
    let base = e.fresh();
    writeln!(w, "  {base} = getelementptr i8, ptr {a}, i64 32").unwrap();
    let ep = e.fresh();
    writeln!(w, "  {ep} = getelementptr i8, ptr {base}, i64 {off}").unwrap();
    let raw = e.fresh();
    writeln!(w, "  {raw} = load {sty}, ptr {ep}").unwrap();
    let ext = match k {
        ArrKind::Byte | ArrKind::Short => {
            let x = e.fresh();
            writeln!(w, "  {x} = sext {sty} {raw} to i32").unwrap();
            x
        }
        ArrKind::Bool | ArrKind::Char => {
            let x = e.fresh();
            writeln!(w, "  {x} = zext {sty} {raw} to i32").unwrap();
            x
        }
        _ => raw,
    };
    writeln!(w, "  br label %{cont}").unwrap();
    writeln!(w, "{cont}:").unwrap();
    let v = e.fresh();
    let z = zero_lit(vty);
    let npe_arm = if nn { String::new() } else { format!("[ {z}, %{npe} ], ") };
    writeln!(
        w,
        "  {v} = phi {} {npe_arm}[ {z}, %{bd} ], [ {ext}, %{ld} ]",
        llty(vty)
    )
    .unwrap();
    v
}

/// Checked store, inline (primitive elements). Null/bounds errors set
/// pending; in the valid case a visible `store`.
fn emit_array_elem_store_checked(w: &mut String, e: &mut FnEmitter, a: &str, i: &str, v: &str, k: ArrKind, nn: bool) {
    let sty = arr_store_ty(k);
    let (npe, ck, bd, st, cont) = (e.fresh_label(), e.fresh_label(), e.fresh_label(), e.fresh_label(), e.fresh_label());
    if !nn {
        let isnull = e.fresh();
        writeln!(w, "  {isnull} = icmp eq ptr {a}, null").unwrap();
        writeln!(w, "  br i1 {isnull}, label %{npe}, label %{ck}").unwrap();
        writeln!(w, "{npe}:").unwrap();
        writeln!(w, "  call void @jrt_throw_npe()").unwrap();
        writeln!(w, "  br label %{cont}").unwrap();
        writeln!(w, "{ck}:").unwrap();
    }
    let lenp = e.fresh();
    writeln!(w, "  {lenp} = getelementptr i8, ptr {a}, i64 16").unwrap();
    let len = e.fresh();
    writeln!(w, "  {len} = load i64, ptr {lenp}, !invariant.load !{}", e.md_inv).unwrap();
    let idx = e.fresh();
    writeln!(w, "  {idx} = sext i32 {i} to i64").unwrap();
    let oob = e.fresh();
    writeln!(w, "  {oob} = icmp uge i64 {idx}, {len}").unwrap();
    writeln!(w, "  br i1 {oob}, label %{bd}, label %{st}").unwrap();
    writeln!(w, "{bd}:").unwrap();
    writeln!(w, "  call void @jrt_throw_bounds()").unwrap();
    writeln!(w, "  br label %{cont}").unwrap();
    writeln!(w, "{st}:").unwrap();
    let off = e.fresh();
    writeln!(w, "  {off} = mul i64 {idx}, {}", k.size()).unwrap();
    let base = e.fresh();
    writeln!(w, "  {base} = getelementptr i8, ptr {a}, i64 32").unwrap();
    let ep = e.fresh();
    writeln!(w, "  {ep} = getelementptr i8, ptr {base}, i64 {off}").unwrap();
    let sv = match k {
        ArrKind::Bool | ArrKind::Byte | ArrKind::Char | ArrKind::Short => {
            let x = e.fresh();
            writeln!(w, "  {x} = trunc i32 {v} to {sty}").unwrap();
            x
        }
        _ => v.to_string(),
    };
    writeln!(w, "  store {sty} {sv}, ptr {ep}").unwrap();
    writeln!(w, "  br label %{cont}").unwrap();
    writeln!(w, "{cont}:").unwrap();
}

/// Stores the vtable in the object header (after refcount + rcflags).
fn store_vtable(w: &mut String, e: &mut FnEmitter, obj: &str, class: &str) {
    let vtp = e.fresh();
    writeln!(w, "  {vtp} = getelementptr ptr, ptr {obj}, i64 {VTABLE_WORD}").unwrap();
    writeln!(w, "  store ptr @vt.{}, ptr {vtp}", sanitize(class)).unwrap();
}

/// Writes `val` into a local. For ref locals the owning-slot
/// discipline applies: the old value is released, the new one retained if needed
/// (`retain_new`: true for a copy/borrowed value, false for a transferred
/// +1 reference from New/Call).
fn store_dest(w: &mut String, e: &mut FnEmitter, dest: Local, val: &str, retain_new: bool) {
    let ty = e.f.locals[dest.0 as usize];
    if ty != Ty::Ref {
        writeln!(w, "  store {} {val}, ptr %l{}", llty(ty), dest.0).unwrap();
        return;
    }
    // Phase 4: if the slot holds only immortal values (stack objects/literals/null),
    // retain/release are provably no-ops → omit them. This decouples the
    // object from the RC bookkeeping, so LLVM can eliminate it entirely
    // (for a dead object) — Rust-like ownership for the stack part.
    if e.imm.contains(&dest.0) || e.borrow.contains(&dest.0) {
        writeln!(w, "  store ptr {val}, ptr %l{}", dest.0).unwrap();
        return;
    }
    let old = e.fresh();
    writeln!(w, "  {old} = load ptr, ptr %l{}", dest.0).unwrap();
    // `retain(null)` is a no-op → omit it (constant null renders as "null").
    if retain_new && val != "null" {
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
    // Comparisons always yield i32 (0/1); operands are i32 or ptr
    // (long/double comparisons go through runtime lcmp/dcmp).
    if matches!(op, BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe) {
        let is_float = aty == Ty::F64 || aty == Ty::F32;
        // Float → fcmp with ordered predicates (o*); integer/pointer → icmp.
        let cc = match (op, is_float) {
            (BinOp::CmpEq, false) => "eq",
            (BinOp::CmpNe, false) => "ne",
            (BinOp::CmpLt, false) => "slt",
            (BinOp::CmpGe, false) => "sge",
            (BinOp::CmpGt, false) => "sgt",
            (BinOp::CmpLe, false) => "sle",
            (BinOp::CmpEq, true) => "oeq",
            (BinOp::CmpNe, true) => "one",
            (BinOp::CmpLt, true) => "olt",
            (BinOp::CmpGe, true) => "oge",
            (BinOp::CmpGt, true) => "ogt",
            (BinOp::CmpLe, true) => "ole",
            _ => unreachable!("emit_binop: only comparisons in this branch"),
        };
        let c = e.fresh();
        let cmp = if is_float { "fcmp" } else { "icmp" };
        writeln!(w, "  {c} = {cmp} {cc} {} {a}, {b}", llty(aty)).unwrap();
        writeln!(w, "  {t} = zext i1 {c} to i32").unwrap();
        return t;
    }

    // Floating-point arithmetic (double/float).
    if aty == Ty::F64 || aty == Ty::F32 {
        let inst = match op {
            BinOp::Add => "fadd",
            BinOp::Sub => "fsub",
            BinOp::Mul => "fmul",
            BinOp::Div => "fdiv",
            BinOp::Rem => "frem",
            _ => panic!("bitwise/shift operation on floating point"),
        };
        // `contract`: lets LLVM fuse `a*b+c` into an FMA (with
        // -march=native → a real FMA instruction). The safest fast-math level —
        // contraction only (usually HIGHER precision), NO reassociation/NaN
        // assumptions. Matches clang's default (`-ffp-contract=on`); closes the
        // measured ~12% gap to clang on float-heavy code (mandelbrot).
        writeln!(w, "  {t} = {inst} contract {} {a}, {b}", llty(aty)).unwrap();
        return t;
    }

    // int/long arithmetic. div/rem go through the runtime for both (not here).
    let ty = llty(aty);
    // Mask shift amounts (JLS 15.19): & 31 (int) or & 63 (long); the
    // amount is always int and is extended to i64 for long.
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
        // div/rem: with a NON-ZERO constant divisor no division-by-zero
        // can occur → inline `sdiv`/`srem` (LLVM strength-reduces: `/2`→shift,
        // `%2^n`→and, `/const`→multiplication trick). This is the big lever for
        // index/RNG-heavy code (binsearch `(lo+hi)/2`, LCG `%2^31`). Otherwise
        // (runtime divisor) through the runtime, which carries the null check + ArithmeticException.
        // Java/Vire semantics (trunc-toward-zero, remainder with the dividend's sign)
        // = LLVM's sdiv/srem.
        BinOp::Div | BinOp::Rem => {
            let inst = if matches!(op, BinOp::Div) { "sdiv" } else { "srem" };
            let const_nonzero = b.parse::<i64>().map(|d| d != 0).unwrap_or(false);
            if const_nonzero {
                writeln!(w, "  {t} = {inst} {ty} {a}, {b}").unwrap();
            } else if aty == Ty::I64 {
                let f = if matches!(op, BinOp::Div) { "jrt_ldiv" } else { "jrt_lrem" };
                writeln!(w, "  {t} = call i64 @{f}(i64 {a}, i64 {b})").unwrap();
            } else {
                let f = if matches!(op, BinOp::Div) { "jrt_idiv" } else { "jrt_irem" };
                writeln!(w, "  {t} = call i32 @{f}(i32 {a}, i32 {b})").unwrap();
            }
        }
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

/// Emits an immortal JStr constant `@<sym>` (full object header
/// + length + bytes), like a string literal — for reflection names.
fn emit_jstr_const(w: &mut String, sym: &str, bytes: &[u8]) {
    let n = bytes.len();
    writeln!(
        w,
        "@{sym} = private unnamed_addr constant {{ i64, ptr, i64, [{n} x i8] }} \
         {{ i64 -1, ptr @vt.java_lang_String, i64 {n}, [{n} x i8] c\"{esc}\" }}",
        esc = escape_ll(bytes),
    )
    .unwrap();
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
