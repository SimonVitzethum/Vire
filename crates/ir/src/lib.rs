//! Mid-level IR modeled on rustc's MIR (see DESIGN.md §2): functions of
//! basic blocks, locals instead of an operand stack, an explicit terminator per
//! block. Devirtualization, escape analysis, inlining, and guarded speculation
//! later run on this IR — before the LLVM lowering.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    I32,
    I64,
    F32,
    F64,
    /// Reference type; opaque for now (pointer). Used for string literals.
    Ref,
    Void,
}

/// Index of a local in `Function::locals`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Local(pub u32);

/// Index of a basic block in `Function::blocks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Block(pub u32);

#[derive(Debug, Clone)]
pub enum Operand {
    Copy(Local),
    ConstI32(i32),
    ConstI64(i64),
    ConstF32(f32),
    ConstF64(f64),
    /// Reference to a string literal in `Program::strings`.
    ConstStr(u32),
    /// Class object of a class resolved at compile time (reflection,
    /// DESIGN.md §1.3). Singleton per class → `==` is pointer equality,
    /// as in Java.
    ConstClass(String),
    ConstNull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    /// Java semantics: throws on divisor 0; the lowering inserts the check.
    Div,
    Rem,
    Shl,
    Shr,
    UShr,
    And,
    Or,
    Xor,
    CmpEq,
    CmpNe,
    CmpLt,
    CmpGe,
    CmpGt,
    CmpLe,
}

#[derive(Debug, Clone)]
pub enum Rvalue {
    Use(Operand),
    Binary(BinOp, Operand, Operand),
    Neg(Operand),
    /// Numeric conversion; source type = type of the operand, target type =
    /// type of the destination local. Only lossless/defined cases inline
    /// (i2l/i2d/l2d/l2i); saturating d2i/d2l go through runtime calls.
    Convert(Operand),
}

#[derive(Debug, Clone)]
pub enum Statement {
    Assign(Local, Rvalue),
    /// Direct call (static, devirtualized, or runtime intrinsic).
    Call {
        dest: Option<Local>,
        func: String,
        args: Vec<Operand>,
    },
    /// Devirtualized instance call: like `Call`, but `args[0]` (the
    /// receiver) is null-checked → catchable NullPointerException.
    CallGuarded {
        dest: Option<Local>,
        func: String,
        args: Vec<Operand>,
    },
    /// Virtual call through the vtable; `args[0]` is the receiver.
    /// `class` is the static type of the call site; the solver replaces
    /// monomorphic sites with `Call` (CHA devirtualization).
    CallVirtual {
        dest: Option<Local>,
        class: String,
        name: String,
        desc: String,
        params: Vec<Ty>,
        ret: Ty,
        args: Vec<Operand>,
    },
    /// Polymorphic but small call site (2–3 instantiated target classes):
    /// the solver replaces the vtable dispatch with a type-guard cascade
    /// of direct calls (guarded devirtualization / polymorphic inline
    /// cache). `args[0]` is the receiver (null-checked); `targets` are
    /// (concrete class, symbol) pairs, the last serving as the else branch.
    CallPoly {
        dest: Option<Local>,
        ret: Ty,
        args: Vec<Operand>,
        targets: Vec<(String, String)>,
    },
    /// Object allocation; fields are zeroed (Java default), header set.
    New { dest: Local, class: String },
    /// Stack allocation: the escape analysis has proven that the object
    /// never leaves the function (ownership light, DESIGN.md §6a) —
    /// lifetime = stack frame, like a Rust value without a Box.
    StackNew { dest: Local, class: String },
    GetField { dest: Local, obj: Operand, class: String, field: String },
    PutField { obj: Operand, class: String, field: String, value: Operand },
    GetStatic { dest: Local, class: String, field: String },
    PutStatic { class: String, field: String, value: Operand },
    /// `dest = (pending exception instanceof class) ? 1 : 0` — for
    /// discriminating the types of multiple catch blocks.
    InstanceOfPending { dest: Local, class: String },
    /// Runtime checkcast to a modeled class: throws
    /// ClassCastException on mismatch, otherwise passthrough.
    CheckCast { obj: Operand, class: String },
    /// `dest = (obj instanceof class) ? 1 : 0`.
    InstanceOf { dest: Local, obj: Operand, class: String },
    /// Array allocation of length `len`.
    NewArray { dest: Local, kind: ArrKind, len: Operand },
    /// Stack array allocation, `len` a compile-time constant: the escape analysis
    /// proved this (primitive) array never leaves the function, so its storage is
    /// an entry-block `alloca [len x elem]` with an immortal header — no heap
    /// allocation, freed with the frame. Same idea as `StackNew` for objects.
    StackNewArray { dest: Local, kind: ArrKind, len: i64 },
    /// Region array allocation: a non-escaping primitive array that is dynamic or
    /// too large for the call stack, and NOT inside a loop. It is bump-allocated
    /// (immortal) in a per-function region (`jrt_region_array`); the backend
    /// brackets the function with `jrt_region_enter`/`_leave`, freeing it en bloc
    /// at return. Cheaper than the RC heap for hot scratch-buffer functions.
    RegionNewArray { dest: Local, kind: ArrKind, len: Operand },
    ArrayLen { dest: Local, arr: Operand },
    /// `dest = arr[index]`; bounds-checked if `checked`.
    ArrayLoad { dest: Local, arr: Operand, index: Operand, kind: ArrKind, checked: bool },
    /// `arr[index] = value`; bounds-checked if `checked`.
    ArrayStore { arr: Operand, index: Operand, value: Operand, kind: ArrKind, checked: bool },
    /// Debug marker: the inline stack for the statements that follow, until the
    /// next marker — `(function symbol, source line)` innermost first. Emits no
    /// code; drives `!DILocation` (with `inlinedAt` chains) when debug info is on.
    /// The inliner appends the call-site frame so an inlined crash shows the full
    /// caller chain. Robust across the optimizing passes (a bare marker statement).
    DebugLine(Vec<(String, u32)>),
}

/// Array element kind: value type (stack) + storage width. Bool/Byte = 1 byte,
/// Char/Short = 2 (value int); Int/Float = 4; Long/Double/Ref = 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrKind {
    Bool,
    Byte,
    Char,
    Short,
    Int,
    Long,
    Float,
    Double,
    Ref,
}

impl ArrKind {
    /// Value type on the operand stack.
    pub fn value_ty(self) -> Ty {
        match self {
            ArrKind::Long => Ty::I64,
            ArrKind::Float => Ty::F32,
            ArrKind::Double => Ty::F64,
            ArrKind::Ref => Ty::Ref,
            _ => Ty::I32,
        }
    }
    /// Storage width in bytes.
    pub fn size(self) -> usize {
        match self {
            ArrKind::Bool | ArrKind::Byte => 1,
            ArrKind::Char | ArrKind::Short => 2,
            ArrKind::Int | ArrKind::Float => 4,
            ArrKind::Long | ArrKind::Double | ArrKind::Ref => 8,
        }
    }
    pub fn is_ref(self) -> bool {
        matches!(self, ArrKind::Ref)
    }
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Goto(Block),
    /// if op != 0 → then_blk otherwise else_blk (comparisons yield 0/1).
    Branch {
        cond: Operand,
        then_blk: Block,
        else_blk: Block,
    },
    /// Multi-way branch (tableswitch/lookupswitch) on an i32 value.
    Switch {
        value: Operand,
        default: Block,
        cases: Vec<(i32, Block)>,
    },
    Return(Option<Operand>),
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone)]
pub struct Function {
    /// Mangled, linkable name (e.g. `J_Hello_main_...`).
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Locals[0..params.len()] are the parameters.
    pub locals: Vec<Ty>,
    pub blocks: Vec<BasicBlock>,
    /// Instance method: local 0 is `this` and provably non-null (the caller
    /// checks the receiver, or `this` comes from `new`). Lets the backend omit
    /// the inline null check on `this` field accesses.
    pub receiver_nonnull: bool,
    /// Source line of the function declaration (for the `DISubprogram`); 0 = none.
    pub line: u32,
}

// --- Class model (closed world: all classes are known at build time) ---

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Ty,
    /// For ref fields: internal name of the referenced type (element type for
    /// arrays), `java/lang/Object` if unknown/wide. `None` for primitives
    /// AND for primitive arrays (`int[]` references nothing → no cycle
    /// edge). Basis of the acyclicity analysis (cycle collector elimination).
    pub ref_target: Option<String>,
}

/// Compile-time initial value of a static field (ConstantValue).
#[derive(Debug, Clone)]
pub enum ConstInit {
    I32(i32),
    I64(i64),
    F64(f64),
    Str(u32),
}

#[derive(Debug, Clone)]
pub struct StaticFieldInfo {
    pub name: String,
    pub ty: Ty,
    pub init: Option<ConstInit>,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub desc: String,
    pub is_static: bool,
    /// false for abstract (no Code attribute).
    pub has_body: bool,
    /// Mangled function name of the definition in this class.
    pub mangled: String,
}

impl MethodInfo {
    /// Virtual = participates in dispatch (instance method, not a constructor).
    pub fn is_virtual(&self) -> bool {
        !self.is_static && self.name != "<init>"
    }
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Internal JVM name (e.g. `pkg/Foo`).
    pub name: String,
    /// None only for java/lang/Object (implicit, not registered).
    pub super_name: Option<String>,
    pub is_interface: bool,
    /// Directly implemented/extended interfaces (JVM names).
    pub interfaces: Vec<String>,
    /// Only declared instance fields; superclass fields via the chain.
    pub fields: Vec<FieldInfo>,
    pub static_fields: Vec<StaticFieldInfo>,
    pub methods: Vec<MethodInfo>,
    /// Does the class have a static initializer (<clinit>)?
    pub has_clinit: bool,
}

/// A GPU kernel: a `@gpu`-annotated Vire function that is NOT lowered as a host
/// function. It lives here — outside `Program::functions` — so the host solver
/// passes, RTA, and the inliner never touch it (no RC, no bounds checks, no
/// arena on the device). The backend emits it twice: once as NVPTX device IR
/// (→ PTX via `llc`) and once as a generated C host launch stub whose symbol is
/// exactly `func.name`, so a host `Call { func: name }` links straight to it.
#[derive(Debug, Clone)]
pub struct GpuKernel {
    pub func: Function,
    /// Per-parameter array element kind (`Some(k)` if the param is an array,
    /// `None` for a scalar). Drives the device pointer element type and the
    /// host H2D/D2H copy element size.
    pub param_arr: Vec<Option<ArrKind>>,
    /// Index of the scalar-integer parameter used as the launch thread count N
    /// (the runtime launches N threads; the kernel guards `if gid < N`).
    pub launch_param: usize,
}

/// A descriptor-set binding reflected from a shader stage's declared resource
/// usage (the `uses_*` flags in `crates/vire/src/shader.rs`). The @vulkan runtime
/// builds the `VkDescriptorSetLayout` + `VkPipelineLayout` from these instead of a
/// hardcoded per-demo layout — the pipeline is derived from the shader signatures
/// (TODO V3). `kind`: 0 = storage buffer, 1 = combined image sampler. `stages` is a
/// `VkShaderStageFlags` bitmask.
#[derive(Debug, Clone)]
pub struct VkBinding {
    pub binding: u32,
    pub kind: u8,
    pub stages: u32,
}

/// The reflected interface of a whole @vulkan shader set (descriptor set 0 today):
/// the bindings each stage touches, unioned, plus the push-constant range.
#[derive(Debug, Clone, Default)]
pub struct VkIface {
    pub bindings: Vec<VkBinding>,
    /// Push-constant block size in bytes (0 = none).
    pub push_size: u32,
    /// `VkShaderStageFlags` the push-constant block is visible to.
    pub push_stages: u32,
}

// VkShaderStageFlagBits (subset), so the frontend and the reflected C data agree.
pub const VK_STAGE_VERTEX: u32 = 0x1;
pub const VK_STAGE_FRAGMENT: u32 = 0x10;
pub const VK_STAGE_COMPUTE: u32 = 0x20;
pub const VK_STAGE_TASK: u32 = 0x40;
pub const VK_STAGE_MESH: u32 = 0x80;
// Descriptor kinds.
pub const VK_KIND_STORAGE_BUFFER: u8 = 0;
pub const VK_KIND_COMBINED_IMAGE_SAMPLER: u8 = 1;

impl VkIface {
    /// Union another stage's interface in: same (binding, kind) OR-s the stage
    /// flags; a new binding is appended; the push range takes the max size and
    /// OR-s the stages.
    pub fn merge(&mut self, other: &VkIface) {
        for b in &other.bindings {
            if let Some(e) = self
                .bindings
                .iter_mut()
                .find(|e| e.binding == b.binding && e.kind == b.kind)
            {
                e.stages |= b.stages;
            } else {
                self.bindings.push(b.clone());
            }
        }
        self.push_size = self.push_size.max(other.push_size);
        self.push_stages |= other.push_stages;
    }
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty() && self.push_size == 0
    }
}

#[derive(Debug, Default)]
pub struct Program {
    pub functions: Vec<Function>,
    /// `@gpu` kernels (device code + host launch stubs), see [`GpuKernel`].
    pub gpu_kernels: Vec<GpuKernel>,
    /// `@vulkan` fragment shader compiled to SPIR-V **assembly** by the frontend
    /// shader compiler (`crates/vire/src/shader.rs`) from an `@fragment fn` body
    /// (arithmetic, `let`, `vecN`). `None` → the driver uses a default constant
    /// fragment. Assembled by `spirv-as` at build. "Vire is the shader language" —
    /// see language/GPU-VULKAN.md.
    pub frag_spvasm: Option<String>,
    /// `@vulkan` vertex shader compiled to SPIR-V assembly from an `@vertex fn`
    /// body (transforms the built-in triangle position). `None` → the driver uses
    /// the fixed bootstrap vertex. See `crates/vire/src/shader.rs`.
    pub vert_spvasm: Option<String>,
    /// `@vulkan` GPU-driven **mesh** shader (VK_EXT_mesh_shader) compiled from a Vire
    /// `@mesh fn` body (`set_mesh_outputs`/`mesh_pos`/`mesh_tri`). `None` → the driver
    /// uses the bootstrap mesh triangle. SPIR-V 1.4. See `crates/vire/src/shader.rs`.
    pub mesh_spvasm: Option<String>,
    /// `@vulkan` **task** (amplification) shader compiled from a Vire `@task fn` body
    /// (`emit_mesh_tasks(n)`): dispatches mesh workgroups, can cull. `None` → no task
    /// stage (the mesh shader runs directly). SPIR-V 1.4.
    pub task_spvasm: Option<String>,
    /// `@vulkan` **compute** meshlet builder compiled from a Vire `@compute fn` body
    /// (`set_meshlet(vec2)`): fills the scene SSBO on the GPU before the mesh draw, so
    /// the meshlet set is GPU-built. `None` → the host supplies the scene. SPIR-V 1.4.
    pub comp_spvasm: Option<String>,
    /// `@gpuvk` **vendor-neutral Vulkan compute** map compiled from a Vire `@gpuvk fn`
    /// body (`elem()` → new value): runs data-parallel over a Float buffer on any
    /// Vulkan device (Intel/NVIDIA/AMD), distinct from CUDA/ROCm `@gpu`. SPIR-V 1.4.
    pub gpuvk_spvasm: Option<String>,
    /// The @vulkan shader set's reflected descriptor/push interface (unioned over
    /// stages). The runtime builds its descriptor-set + pipeline layout from this,
    /// so the pipeline is derived from the shader signatures rather than hardcoded.
    /// Empty when the program uses no shader resources. See [`VkIface`].
    pub vk_iface: VkIface,
    /// Debug builds only: function name → source name of each local (indexed by
    /// local id; `None` for compiler temporaries). Drives `DILocalVariable` +
    /// `#dbg_declare` so gdb/lldb can inspect variables. Empty otherwise.
    pub debug_local_names: std::collections::HashMap<String, Vec<Option<String>>>,
    pub classes: Vec<ClassInfo>,
    /// String literal pool; `Operand::ConstStr` indexes into this.
    pub strings: Vec<String>,
    /// Classes with a Class object (touched by reflection):
    /// class name → string index of the dotted name (for getName).
    pub class_objects: Vec<(String, u32)>,
    /// Entry class (internal name, e.g. `com/x/App`), if known
    /// (JAR manifest `Main-Class` or `--main`). Only its `main` becomes
    /// `java_main`; if `None`, any `main` method applies (single-file mode).
    pub main_class: Option<String>,
    /// Functions kept as reachability roots because they are invoked through
    /// generated/native glue invisible to RTA (e.g. a Vire `spawn` worker, called
    /// from its C shim via `jrt_spawn`). Analogous to the Runnable.run() roots.
    pub exported: Vec<String>,
}

impl Program {
    pub fn intern_string(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return i as u32;
        }
        self.strings.push(s.to_string());
        (self.strings.len() - 1) as u32
    }

    /// Registers a class's Class object and returns the
    /// string index of its dotted name.
    pub fn intern_class_object(&mut self, class: &str) -> u32 {
        if let Some((_, sid)) = self.class_objects.iter().find(|(c, _)| c == class) {
            return *sid;
        }
        let dotted = class.replace('/', ".");
        let sid = self.intern_string(&dotted);
        self.class_objects.push((class.to_string(), sid));
        sid
    }

    pub fn class(&self, name: &str) -> Option<&ClassInfo> {
        self.classes.iter().find(|c| c.name == name)
    }

    /// Field resolution: walks the superclass chain from `class` up to the
    /// declaring class (JVMS 5.4.3.2). Returns (owner class, type).
    pub fn resolve_field(&self, class: &str, field: &str) -> Option<(&str, Ty)> {
        let mut cur = self.class(class)?;
        loop {
            if let Some(f) = cur.fields.iter().find(|f| f.name == field) {
                return Some((cur.name.as_str(), f.ty));
            }
            cur = self.class(cur.super_name.as_deref()?)?;
        }
    }

    /// Resolve a static field (up the superclass chain). Returns
    /// (owner class, type).
    pub fn resolve_static_field(&self, class: &str, field: &str) -> Option<(&str, Ty)> {
        let mut cur = self.class(class)?;
        loop {
            if let Some(f) = cur.static_fields.iter().find(|f| f.name == field) {
                return Some((cur.name.as_str(), f.ty));
            }
            cur = self.class(cur.super_name.as_deref()?)?;
        }
    }

    /// Method resolution: finds the implementation of `name`+`desc`
    /// from `class` upward. Returns the defining ClassInfo + MethodInfo.
    pub fn resolve_method(&self, class: &str, name: &str, desc: &str) -> Option<(&ClassInfo, &MethodInfo)> {
        let mut cur = self.class(class)?;
        loop {
            if let Some(m) = cur.methods.iter().find(|m| m.name == name && m.desc == desc && m.has_body) {
                return Some((cur, m));
            }
            cur = self.class(cur.super_name.as_deref()?)?;
        }
    }

    /// Is `sub` equal to `sup` or a (transitive) subclass?
    pub fn is_subclass(&self, sub: &str, sup: &str) -> bool {
        let mut cur = sub;
        loop {
            if cur == sup {
                return true;
            }
            match self.class(cur).and_then(|c| c.super_name.as_deref()) {
                Some(s) => cur = s,
                None => return false,
            }
        }
    }

    /// All interfaces that `class` implements (transitively via the super
    /// chain and interface inheritance).
    pub fn all_interfaces(&self, class: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let mut stack = vec![class.to_string()];
        while let Some(c) = stack.pop() {
            let Some(ci) = self.class(&c) else { continue };
            for i in &ci.interfaces {
                if out.insert(i.clone()) {
                    stack.push(i.clone());
                }
            }
            if let Some(s) = &ci.super_name {
                stack.push(s.clone());
            }
        }
        out
    }

    /// Does `class` implement the interface `iface` (or is it equal)?
    pub fn implements(&self, class: &str, iface: &str) -> bool {
        class == iface || self.all_interfaces(class).contains(iface)
    }
}

/// Turns a JVM name into a linkable identifier.
pub fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// Linker symbol of a method. Must be consistent across all crates.
pub fn mangle(class: &str, name: &str, descriptor: &str) -> String {
    if name == "main" && descriptor == "([Ljava/lang/String;)V" {
        return "java_main".to_string();
    }
    format!("J_{}_{}_{}", sanitize(class), sanitize(name), sanitize(descriptor))
}

/// Symbol of a class's static initializer.
pub fn clinit_symbol(class: &str) -> String {
    mangle(class, "<clinit>", "()V")
}

// --- Textual output for debugging (`--emit-ir`) ---

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.strings.iter().enumerate() {
            writeln!(f, "str{i} = {s:?}")?;
        }
        for func in &self.functions {
            write!(f, "{func}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\nfn {}({:?}) -> {:?} {{", self.name, self.params, self.ret)?;
        for (i, ty) in self.locals.iter().enumerate() {
            writeln!(f, "  let _{i}: {ty:?};")?;
        }
        for (i, bb) in self.blocks.iter().enumerate() {
            writeln!(f, "  bb{i}:")?;
            for st in &bb.statements {
                writeln!(f, "    {st:?}")?;
            }
            writeln!(f, "    {:?}", bb.terminator)?;
        }
        writeln!(f, "}}")
    }
}
