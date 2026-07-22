//! Vire → SPIR-V shader compiler (`@vulkan`, VS step 2). Compiles a Vire
//! `@fragment fn` **body** — not just a constant — to SPIR-V *assembly* (assembled
//! by `spirv-as` in the driver). Supports float/vector arithmetic, `let`/`mut`
//! bindings, and `vecN(...)` constructors; the tail expression is the output color
//! (a `Vec4`). Vectors are shader-local types here (no host type-system change).
//!
//! SPIR-V needs all `OpType`/`OpConstant`/global vars before the function body, so
//! the base types are a fixed preamble and only float constants are collected as
//! encountered (they depend only on `%float`, already declared). Straight-line only
//! for now — control flow (`OpLoopMerge`/`OpSelectionMerge`) and fragment inputs
//! (varyings/`gl_FragCoord`) are the next steps.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use crate::ast::{BinOp, Block, Expr, FnDef, Stmt};

/// Extract a non-negative integer literal (mesh/task indices and counts are constants).
fn int_lit(e: &Expr) -> Result<i64, String> {
    match e {
        Expr::Int(v, _) if *v >= 0 => Ok(*v as i64),
        _ => Err("shader: expected a non-negative integer literal".into()),
    }
}

/// A fresh `Cx` for a shader stage that computes values (positions/colors).
fn new_cx() -> Cx {
    Cx {
        consts: String::new(),
        vars: String::new(),
        body: String::new(),
        const_cache: HashMap::new(),
        env: HashMap::new(),
        uses_fragcoord: false,
        emits_varying: false,
        uses_varying: false,
        uses_attr_color: false,
        uses_glsl: false,
        uses_push_constant: false,
        uses_ssbo: false,
        uses_workgroup_id: false,
        uses_payload: false,
        uses_global_id: false,
        n: 0,
    }
}

/// The GPU scene record — a typed Vire struct, shared by every stage that touches the
/// scene buffer so they agree on the layout. `offset` is the meshlet's (x,y) centre;
/// `cone` is its 2D facing direction (for cone/backface culling in `@task`). std430:
/// two vec2 fields at offsets 0 and 8, array stride 16.
///
/// Build the (entry-point interface, decorations, type/var decls) for the GPU-driven
/// resources a stage touches — the scene SSBO (binding 0), gl_WorkGroupID /
/// gl_GlobalInvocationID, the task→mesh payload, and the frustum push constant. Shared
/// by `@mesh`/`@task`/`@compute` so all declare them identically (SPIR-V 1.4 requires
/// every global in the interface). `%i_0`/`%i_1` and `%v3uint` must already be declared
/// by the caller's preamble. `writable` drops the read-only decorations (the `@compute`
/// builder writes the scene; the graphics stages read it).
fn resource_decls(ssbo: bool, wgid: bool, global_id: bool, payload: bool, push: bool, writable: bool) -> (String, String, String) {
    let mut iface = String::new();
    let mut decor = String::new();
    let mut decl = String::new();
    if wgid {
        iface.push_str(" %gl_WorkGroupID");
        decor.push_str("               OpDecorate %gl_WorkGroupID BuiltIn WorkgroupId\n");
        decl.push_str("%_ptr_Input_v3uint = OpTypePointer Input %v3uint\n%gl_WorkGroupID = OpVariable %_ptr_Input_v3uint Input\n");
    }
    if global_id {
        iface.push_str(" %gl_GlobalInvocationID");
        decor.push_str("               OpDecorate %gl_GlobalInvocationID BuiltIn GlobalInvocationId\n");
        decl.push_str("%_ptr_Input_v3uint = OpTypePointer Input %v3uint\n%gl_GlobalInvocationID = OpVariable %_ptr_Input_v3uint Input\n");
    }
    if ssbo {
        iface.push_str(" %scene");
        let ro = if writable { "" } else { "               OpMemberDecorate %Scene 0 NonWritable\n               OpDecorate %scene NonWritable\n" };
        decor.push_str(&format!("               OpDecorate %_rt_Meshlet ArrayStride 16\n               OpMemberDecorate %Meshlet 0 Offset 0\n               OpMemberDecorate %Meshlet 1 Offset 8\n               OpDecorate %Scene Block\n{ro}               OpMemberDecorate %Scene 0 Offset 0\n               OpDecorate %scene DescriptorSet 0\n               OpDecorate %scene Binding 0\n"));
        decl.push_str("    %Meshlet = OpTypeStruct %v2float %v2float\n%_rt_Meshlet = OpTypeRuntimeArray %Meshlet\n      %Scene = OpTypeStruct %_rt_Meshlet\n%_ptr_ssbo_Scene = OpTypePointer StorageBuffer %Scene\n      %scene = OpVariable %_ptr_ssbo_Scene StorageBuffer\n%_ptr_ssbo_v2float = OpTypePointer StorageBuffer %v2float\n");
    }
    if payload {
        iface.push_str(" %pl");
        decl.push_str("    %Payload = OpTypeStruct %uint\n%_ptr_pl_Payload = OpTypePointer TaskPayloadWorkgroupEXT %Payload\n         %pl = OpVariable %_ptr_pl_Payload TaskPayloadWorkgroupEXT\n%_ptr_pl_uint = OpTypePointer TaskPayloadWorkgroupEXT %uint\n");
    }
    if push {
        iface.push_str(" %pcv");
        decor.push_str("               OpDecorate %pcblock Block\n               OpMemberDecorate %pcblock 0 Offset 0\n");
        decl.push_str("     %pcblock = OpTypeStruct %v4float\n%_ptr_pc_block = OpTypePointer PushConstant %pcblock\n        %pcv = OpVariable %_ptr_pc_block PushConstant\n%_ptr_pc_v4float = OpTypePointer PushConstant %v4float\n");
    }
    (iface, decor, decl)
}

/// A shader value type: a float scalar, an N-component float vector, or a bool
/// (produced by comparisons, consumed by `if`/`while` conditions).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Float,
    Vec(u8),
    Bool,
}

impl Ty {
    fn spirv(self) -> &'static str {
        match self {
            Ty::Float => "%float",
            Ty::Vec(2) => "%v2float",
            Ty::Vec(3) => "%v3float",
            Ty::Vec(_) => "%v4float",
            Ty::Bool => "%bool",
        }
    }
    /// The `Function`-storage pointer type for a mutable local of this type.
    fn pf(self) -> &'static str {
        match self {
            Ty::Float => "%pf_float",
            Ty::Vec(2) => "%pf_v2float",
            Ty::Vec(3) => "%pf_v3float",
            Ty::Vec(_) => "%pf_v4float",
            Ty::Bool => "%pf_bool",
        }
    }
}

struct Cx {
    consts: String,             // `%kN = OpConstant %float …` lines
    vars: String,               // `%vN = OpVariable %pf_… Function` decls (entry-block top)
    body: String,               // function-body instructions
    const_cache: HashMap<u32, String>, // float bits → id
    env: HashMap<String, (String, Ty)>, // local name → (Function-pointer id, type)
    uses_fragcoord: bool,       // `frag_x/frag_y/frag_coord` → declare gl_FragCoord
    emits_varying: bool,        // vertex `out_color(vec3)` → declare the Location-0 Output
    uses_varying: bool,         // fragment `in_color()` → declare the Location-0 Input
    uses_attr_color: bool,      // vertex `attr_color()` → per-vertex color attribute (Location 1)
    uses_glsl: bool,            // a GLSL.std.450 builtin (sqrt/normalize/dot/…) → import the set
    uses_push_constant: bool,   // task `cull_plane()` → a vec4 push constant (the frustum plane)
    uses_ssbo: bool,            // `meshlet_offset()`/`culled_offset()` → the scene SSBO (binding 0)
    uses_workgroup_id: bool,    // read gl_WorkGroupID (meshlet_offset, emit_visible)
    uses_payload: bool,         // task→mesh payload (the surviving meshlet index)
    uses_global_id: bool,       // compute `meshlet_index()`/`set_meshlet()` → gl_GlobalInvocationID
    n: u32,
}

impl Cx {
    fn id(&mut self, prefix: &str) -> String {
        let k = self.n;
        self.n += 1;
        format!("%{prefix}{k}")
    }

    fn constant(&mut self, v: f32) -> String {
        // spirv-as parses decimals; cache by bit pattern so equal values share an id.
        if let Some(id) = self.const_cache.get(&v.to_bits()) {
            return id.clone();
        }
        let id = self.id("k");
        writeln!(self.consts, "{id} = OpConstant %float {:.9}", v).unwrap();
        self.const_cache.insert(v.to_bits(), id.clone());
        id
    }

    /// Declare a fresh `Function`-storage variable of `ty` (at the entry block) and
    /// return its pointer id. Locals are storage-backed so assignment and mutation
    /// across `if`/`while` boundaries just work (no SSA phi bookkeeping).
    fn fresh_var(&mut self, ty: Ty) -> String {
        let ptr = self.id("v");
        writeln!(self.vars, "{ptr} = OpVariable {} Function", ty.pf()).unwrap();
        ptr
    }

    /// Bind `name` to `val` (a computed SSA id of `ty`): reuse the local's variable
    /// if it already exists with the same type, else declare one, then store.
    fn bind(&mut self, name: &str, val: &str, ty: Ty) {
        let ptr = match self.env.get(name) {
            Some((p, t)) if *t == ty => p.clone(),
            _ => self.fresh_var(ty),
        };
        writeln!(self.body, "OpStore {ptr} {val}").unwrap();
        self.env.insert(name.to_string(), (ptr, ty));
    }

    fn expr(&mut self, e: &Expr) -> Result<(String, Ty), String> {
        match e {
            Expr::Float(v, _) => Ok((self.constant(*v as f32), Ty::Float)),
            Expr::Int(v, _) => Ok((self.constant(*v as f32), Ty::Float)),
            Expr::Ident(n, _) => {
                let (ptr, ty) = self
                    .env
                    .get(n)
                    .cloned()
                    .ok_or_else(|| format!("shader: unknown variable `{n}`"))?;
                let id = self.id("t");
                writeln!(self.body, "{id} = OpLoad {} {ptr}", ty.spirv()).unwrap();
                Ok((id, ty))
            }
            Expr::Call { callee, args, .. } => {
                let name = match callee.as_ref() {
                    Expr::Ident(n, _) => n.as_str(),
                    _ => return Err("shader: only vecN(...) calls are supported".into()),
                };
                // Fragment input builtins: the pixel position (gl_FragCoord).
                if matches!(name, "frag_x" | "frag_y" | "frag_coord") {
                    if !args.is_empty() {
                        return Err(format!("shader: {name}() takes no arguments"));
                    }
                    self.uses_fragcoord = true;
                    let fc = self.id("t");
                    writeln!(self.body, "{fc} = OpLoad %v4float %gl_FragCoord").unwrap();
                    if name == "frag_coord" {
                        return Ok((fc, Ty::Vec(4)));
                    }
                    let comp = if name == "frag_x" { 0 } else { 1 };
                    let id = self.id("t");
                    writeln!(self.body, "{id} = OpCompositeExtract %float {fc} {comp}").unwrap();
                    return Ok((id, Ty::Float));
                }
                // Per-vertex color attribute (vertex stage only): read the vec3 the
                // vertex buffer supplies at Location 1 (`vk_mesh_c` interleaves it after
                // the x,y position). Typically forwarded with `out_color(attr_color())`.
                if name == "attr_color" {
                    if !args.is_empty() {
                        return Err("shader: attr_color() takes no arguments".into());
                    }
                    self.uses_attr_color = true;
                    let id = self.id("t");
                    writeln!(self.body, "{id} = OpLoad %v3float %col_in").unwrap();
                    return Ok((id, Ty::Vec(3)));
                }
                // Scene buffer read (mesh stage): the per-meshlet (x,y) offset for THIS
                // workgroup — `scene[gl_WorkGroupID.x]` from the SSBO the host fills.
                // Lets one dispatch of N mesh workgroups draw N meshlets from Vire data.
                if name == "meshlet_offset" {
                    if !args.is_empty() {
                        return Err("shader: meshlet_offset() takes no arguments".into());
                    }
                    self.uses_ssbo = true;
                    self.uses_workgroup_id = true;
                    let wid = self.id("t");
                    writeln!(self.body, "{wid} = OpLoad %v3uint %gl_WorkGroupID").unwrap();
                    let wx = self.id("t");
                    writeln!(self.body, "{wx} = OpCompositeExtract %uint {wid} 0").unwrap();
                    let p = self.id("t");
                    writeln!(self.body, "{p} = OpAccessChain %_ptr_ssbo_v2float %scene %i_0 {wx} %i_0").unwrap();
                    let id = self.id("t");
                    writeln!(self.body, "{id} = OpLoad %v2float {p}").unwrap();
                    return Ok((id, Ty::Vec(2)));
                }
                // The meshlet's facing direction (record.cone, member 1) for THIS task
                // workgroup — cone/backface culling: emit only when it faces the camera.
                if name == "meshlet_cone" {
                    if !args.is_empty() {
                        return Err("shader: meshlet_cone() takes no arguments".into());
                    }
                    self.uses_ssbo = true;
                    self.uses_workgroup_id = true;
                    let wid = self.id("t");
                    writeln!(self.body, "{wid} = OpLoad %v3uint %gl_WorkGroupID").unwrap();
                    let wx = self.id("t");
                    writeln!(self.body, "{wx} = OpCompositeExtract %uint {wid} 0").unwrap();
                    let p = self.id("t");
                    writeln!(self.body, "{p} = OpAccessChain %_ptr_ssbo_v2float %scene %i_0 {wx} %i_1").unwrap();
                    let id = self.id("t");
                    writeln!(self.body, "{id} = OpLoad %v2float {p}").unwrap();
                    return Ok((id, Ty::Vec(2)));
                }
                // `@gpuvk` element read: `buffer[gl_GlobalInvocationID.x]`, this
                // invocation's element of the data-parallel float buffer.
                if name == "elem" {
                    if !args.is_empty() {
                        return Err("shader: elem() takes no arguments".into());
                    }
                    let g = self.id("t");
                    writeln!(self.body, "{g} = OpLoad %v3uint %gvid").unwrap();
                    let gx = self.id("t");
                    writeln!(self.body, "{gx} = OpCompositeExtract %uint {g} 0").unwrap();
                    let p = self.id("t");
                    writeln!(self.body, "{p} = OpAccessChain %_ptr_ssbo_float %vbuf %i_0 {gx}").unwrap();
                    let v = self.id("t");
                    writeln!(self.body, "{v} = OpLoad %float {p}").unwrap();
                    return Ok((v, Ty::Float));
                }
                // Compute builder: this invocation's meshlet index as a float —
                // float(gl_GlobalInvocationID.x). Lets a @compute place meshlet i by
                // formula (e.g. `-0.5 + meshlet_index() * spacing`).
                if name == "meshlet_index" {
                    if !args.is_empty() {
                        return Err("shader: meshlet_index() takes no arguments".into());
                    }
                    self.uses_global_id = true;
                    let g = self.id("t");
                    writeln!(self.body, "{g} = OpLoad %v3uint %gl_GlobalInvocationID").unwrap();
                    let gx = self.id("t");
                    writeln!(self.body, "{gx} = OpCompositeExtract %uint {g} 0").unwrap();
                    let id = self.id("t");
                    writeln!(self.body, "{id} = OpConvertUToF %float {gx}").unwrap();
                    return Ok((id, Ty::Float));
                }
                // Culled scene read (mesh stage, fused cull path): the offset of the
                // meshlet THIS mesh workgroup was launched for — `scene[payload.idx]`,
                // where the @task wrote the surviving meshlet's index into the payload.
                if name == "culled_offset" {
                    if !args.is_empty() {
                        return Err("shader: culled_offset() takes no arguments".into());
                    }
                    self.uses_ssbo = true;
                    self.uses_payload = true;
                    let ip = self.id("t");
                    writeln!(self.body, "{ip} = OpAccessChain %_ptr_pl_uint %pl %i_0").unwrap();
                    let idx = self.id("t");
                    writeln!(self.body, "{idx} = OpLoad %uint {ip}").unwrap();
                    let p = self.id("t");
                    writeln!(self.body, "{p} = OpAccessChain %_ptr_ssbo_v2float %scene %i_0 {idx} %i_0").unwrap();
                    let id = self.id("t");
                    writeln!(self.body, "{id} = OpLoad %v2float {p}").unwrap();
                    return Ok((id, Ty::Vec(2)));
                }
                // Push constant: the frustum plane (nx,ny,nz,d) the host supplies for
                // `@task` culling — read as a vec4. The stage declares the push-constant
                // block only when this is used (currently the task stage).
                if name == "cull_plane" {
                    if !args.is_empty() {
                        return Err("shader: cull_plane() takes no arguments".into());
                    }
                    self.uses_push_constant = true;
                    let p = self.id("t");
                    writeln!(self.body, "{p} = OpAccessChain %_ptr_pc_v4float %pcv %i_0").unwrap();
                    let id = self.id("t");
                    writeln!(self.body, "{id} = OpLoad %v4float {p}").unwrap();
                    return Ok((id, Ty::Vec(4)));
                }
                // Varying input: the interpolated per-vertex color the `@vertex`
                // stage wrote with `out_color(...)` (Location 0, a vec3).
                if name == "in_color" {
                    if !args.is_empty() {
                        return Err("shader: in_color() takes no arguments".into());
                    }
                    self.uses_varying = true;
                    let id = self.id("t");
                    writeln!(self.body, "{id} = OpLoad %v3float %vcol_in").unwrap();
                    return Ok((id, Ty::Vec(3)));
                }
                // GLSL.std.450 math builtins (OpExtInst) — enough for lighting/geometry.
                if let Some(r) = self.glsl_builtin(name, args)? {
                    return Ok(r);
                }
                let n = match name {
                    "vec2" => 2u8,
                    "vec3" => 3,
                    "vec4" => 4,
                    other => return Err(format!("shader: unsupported call `{other}` (only vec2/3/4)")),
                };
                // Mixed construction: args may be scalars or smaller vectors whose
                // component counts sum to n (e.g. `vec4(pos, 0.0, 1.0)`).
                let mut parts = Vec::new();
                let mut count = 0u8;
                for a in args {
                    let (id, t) = self.expr(a)?;
                    count += match t {
                        Ty::Float => 1,
                        Ty::Vec(k) => k,
                        Ty::Bool => return Err("shader: a bool cannot be a vector component".into()),
                    };
                    parts.push(id);
                }
                if count != n {
                    return Err(format!("shader: {name} components sum to {count}, need {n}"));
                }
                let id = self.id("t");
                writeln!(
                    self.body,
                    "{id} = OpCompositeConstruct {} {}",
                    Ty::Vec(n).spirv(),
                    parts.join(" ")
                )
                .unwrap();
                Ok((id, Ty::Vec(n)))
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let (a, ta) = self.expr(lhs)?;
                let (b, tb) = self.expr(rhs)?;
                self.binary(*op, a, ta, b, tb)
            }
            // Swizzle: `v.x` (→ float, OpCompositeExtract) or a multi-component swizzle
            // `v.xy`/`.xyz`/`.rgb`/… (→ vecN, OpVectorShuffle). Components may repeat.
            Expr::Field { base, name, .. } => {
                let (id, t) = self.expr(base)?;
                let k = match t {
                    Ty::Vec(k) => k,
                    Ty::Float | Ty::Bool => return Err("shader: swizzle on a non-vector".into()),
                };
                // Accept x/y/z/w and the r/g/b/a aliases.
                let comps: Result<Vec<u8>, String> = name.chars().map(|c| match c {
                    'x' | 'r' => Ok(0u8),
                    'y' | 'g' => Ok(1),
                    'z' | 'b' => Ok(2),
                    'w' | 'a' => Ok(3),
                    other => Err(format!("shader: unknown swizzle component `.{other}`")),
                }).collect();
                let comps = comps?;
                if comps.is_empty() || comps.len() > 4 {
                    return Err(format!("shader: bad swizzle `.{name}`"));
                }
                if let Some(&bad) = comps.iter().find(|&&c| c >= k) {
                    return Err(format!("shader: `.{name}` uses component {bad}, out of range for a vec{k}"));
                }
                if comps.len() == 1 {
                    let r = self.id("t");
                    writeln!(self.body, "{r} = OpCompositeExtract %float {id} {}", comps[0]).unwrap();
                    return Ok((r, Ty::Float));
                }
                // Multi-component → OpVectorShuffle (shuffle with the same vector twice).
                let n = comps.len() as u8;
                let idxs: Vec<String> = comps.iter().map(|c| c.to_string()).collect();
                let r = self.id("t");
                writeln!(self.body, "{r} = OpVectorShuffle {} {id} {id} {}", Ty::Vec(n).spirv(), idxs.join(" ")).unwrap();
                Ok((r, Ty::Vec(n)))
            }
            // `if cond { … valexpr } else { … valexpr }` as a value: a structured
            // selection whose branches store into a result variable read after merge.
            Expr::If { cond, then, elifs, els, .. } => {
                let els = els.as_ref().ok_or("shader: `if` used as a value needs an `else`")?;
                self.lower_if_value(cond, then, elifs, els)
            }
            _ => Err("shader: unsupported expression (literals, vars, vecN, swizzle, +-*/, if)".into()),
        }
    }

    /// A value-producing `if`/`elif`/`else`: `OpSelectionMerge` + `OpBranchConditional`,
    /// each branch storing its value into one result variable, loaded after the merge.
    fn lower_if_value(
        &mut self,
        cond: &Expr,
        then: &Block,
        elifs: &[(Expr, Block)],
        els: &Block,
    ) -> Result<(String, Ty), String> {
        let (c, ct) = self.expr(cond)?;
        if ct != Ty::Bool {
            return Err("shader: an `if` condition must be a comparison (bool)".into());
        }
        let then_l = self.id("then");
        let else_l = self.id("else");
        let merge_l = self.id("merge");
        writeln!(self.body, "OpSelectionMerge {merge_l} None").unwrap();
        writeln!(self.body, "OpBranchConditional {c} {then_l} {else_l}").unwrap();
        // then branch
        writeln!(self.body, "{then_l} = OpLabel").unwrap();
        let (tv, tt) = self.block_value(then)?;
        let res = self.fresh_var(tt);
        writeln!(self.body, "OpStore {res} {tv}").unwrap();
        writeln!(self.body, "OpBranch {merge_l}").unwrap();
        // else branch: the next `elif` folds in as a nested value-if, else the `else`.
        writeln!(self.body, "{else_l} = OpLabel").unwrap();
        let (ev, et) = if let Some(((econd, eblk), rest)) = elifs.split_first() {
            self.lower_if_value(econd, eblk, rest, els)?
        } else {
            self.block_value(els)?
        };
        if et != tt {
            return Err("shader: `if` and `else` must yield the same type".into());
        }
        writeln!(self.body, "OpStore {res} {ev}").unwrap();
        writeln!(self.body, "OpBranch {merge_l}").unwrap();
        // merge: the value is whichever branch ran.
        writeln!(self.body, "{merge_l} = OpLabel").unwrap();
        let out = self.id("t");
        writeln!(self.body, "{out} = OpLoad {} {res}", tt.spirv()).unwrap();
        Ok((out, tt))
    }

    fn binary(&mut self, op: BinOp, a: String, ta: Ty, b: String, tb: Ty) -> Result<(String, Ty), String> {
        // Comparisons (scalar float → bool) feed `if`/`while` conditions.
        let cmp = match op {
            BinOp::Lt => Some("OpFOrdLessThan"),
            BinOp::Le => Some("OpFOrdLessThanEqual"),
            BinOp::Gt => Some("OpFOrdGreaterThan"),
            BinOp::Ge => Some("OpFOrdGreaterThanEqual"),
            BinOp::Eq => Some("OpFOrdEqual"),
            BinOp::Ne => Some("OpFUnordNotEqual"),
            _ => None,
        };
        if let Some(opc) = cmp {
            if ta != Ty::Float || tb != Ty::Float {
                return Err("shader: comparisons need scalar floats".into());
            }
            let id = self.id("t");
            writeln!(self.body, "{id} = {opc} %bool {a} {b}").unwrap();
            return Ok((id, Ty::Bool));
        }
        // Logical `&&`/`||` combine bool conditions.
        if matches!(op, BinOp::And | BinOp::Or) {
            if ta != Ty::Bool || tb != Ty::Bool {
                return Err("shader: `&&`/`||` need boolean operands".into());
            }
            let opc = if op == BinOp::And { "OpLogicalAnd" } else { "OpLogicalOr" };
            let id = self.id("t");
            writeln!(self.body, "{id} = {opc} %bool {a} {b}").unwrap();
            return Ok((id, Ty::Bool));
        }
        // scalar·vector and vector·scalar multiply → OpVectorTimesScalar.
        if op == BinOp::Mul && ta != tb {
            let (vec, vt, scalar) = match (ta, tb) {
                (Ty::Vec(_), Ty::Float) => (a, ta, b),
                (Ty::Float, Ty::Vec(_)) => (b, tb, a),
                _ => return Err("shader: mismatched types in `*`".into()),
            };
            let id = self.id("t");
            writeln!(self.body, "{id} = OpVectorTimesScalar {} {vec} {scalar}", vt.spirv()).unwrap();
            return Ok((id, vt));
        }
        if ta != tb {
            return Err("shader: mismatched operand types (add/sub/div need equal types)".into());
        }
        let opcode = match op {
            BinOp::Add => "OpFAdd",
            BinOp::Sub => "OpFSub",
            BinOp::Mul => "OpFMul",
            BinOp::Div => "OpFDiv",
            _ => return Err("shader: only + - * / are supported".into()),
        };
        let id = self.id("t");
        writeln!(self.body, "{id} = {opcode} {} {a} {b}", ta.spirv()).unwrap();
        Ok((id, ta))
    }

    /// A GLSL.std.450 math builtin call, if `name` is one. Returns `Ok(None)` when
    /// it is not a known builtin (so the caller falls through to `vecN`/user calls).
    /// Component-wise ops accept a scalar or a vector (genType) and preserve the type.
    fn glsl_builtin(&mut self, name: &str, args: &[Expr]) -> Result<Option<(String, Ty)>, String> {
        // Unary, same-type-in-out (float or vector, component-wise).
        let unary = match name {
            "sqrt" => Some("Sqrt"), "abs" => Some("FAbs"), "floor" => Some("Floor"),
            "ceil" => Some("Ceil"), "fract" => Some("Fract"), "sin" => Some("Sin"),
            "cos" => Some("Cos"), "exp" => Some("Exp"), "log" => Some("Log"),
            _ => None,
        };
        if let Some(op) = unary {
            if args.len() != 1 { return Err(format!("shader: {name}() takes one argument")); }
            let (x, tx) = self.expr(&args[0])?;
            self.uses_glsl = true;
            let id = self.id("t");
            writeln!(self.body, "{id} = OpExtInst {} %glsl {op} {x}", tx.spirv()).unwrap();
            return Ok(Some((id, tx)));
        }
        // Binary, matching types → same type.
        let binary = match name {
            "min" => Some("FMin"), "max" => Some("FMax"), "pow" => Some("Pow"), _ => None,
        };
        if let Some(op) = binary {
            if args.len() != 2 { return Err(format!("shader: {name}() takes two arguments")); }
            let (a, ta) = self.expr(&args[0])?;
            let (b, tb) = self.expr(&args[1])?;
            if ta != tb { return Err(format!("shader: {name}() needs matching argument types")); }
            self.uses_glsl = true;
            let id = self.id("t");
            writeln!(self.body, "{id} = OpExtInst {} %glsl {op} {a} {b}", ta.spirv()).unwrap();
            return Ok(Some((id, ta)));
        }
        // Ternary: clamp(x,lo,hi) / mix(a,b,t). For mix, `t` may be scalar.
        if name == "clamp" || name == "mix" {
            if args.len() != 3 { return Err(format!("shader: {name}() takes three arguments")); }
            let (a, ta) = self.expr(&args[0])?;
            let (b, tb) = self.expr(&args[1])?;
            let (c, tc) = self.expr(&args[2])?;
            if ta != tb { return Err(format!("shader: {name}(): first two args must match")); }
            let ok3 = tc == ta || (name == "mix" && tc == Ty::Float);
            if !ok3 { return Err(format!("shader: {name}(): third arg type mismatch")); }
            let op = if name == "clamp" { "FClamp" } else { "FMix" };
            self.uses_glsl = true;
            let id = self.id("t");
            writeln!(self.body, "{id} = OpExtInst {} %glsl {op} {a} {b} {c}", ta.spirv()).unwrap();
            return Ok(Some((id, ta)));
        }
        if name == "normalize" {
            if args.len() != 1 { return Err("shader: normalize() takes one argument".into()); }
            let (v, tv) = self.expr(&args[0])?;
            if !matches!(tv, Ty::Vec(_)) { return Err("shader: normalize() needs a vector".into()); }
            self.uses_glsl = true;
            let id = self.id("t");
            writeln!(self.body, "{id} = OpExtInst {} %glsl Normalize {v}", tv.spirv()).unwrap();
            return Ok(Some((id, tv)));
        }
        if name == "length" {
            if args.len() != 1 { return Err("shader: length() takes one argument".into()); }
            let (v, tv) = self.expr(&args[0])?;
            if !matches!(tv, Ty::Vec(_)) { return Err("shader: length() needs a vector".into()); }
            self.uses_glsl = true;
            let id = self.id("t");
            writeln!(self.body, "{id} = OpExtInst %float %glsl Length {v}").unwrap();
            return Ok(Some((id, Ty::Float)));
        }
        if name == "cross" {
            if args.len() != 2 { return Err("shader: cross() takes two arguments".into()); }
            let (a, ta) = self.expr(&args[0])?;
            let (b, tb) = self.expr(&args[1])?;
            if ta != Ty::Vec(3) || tb != Ty::Vec(3) {
                return Err("shader: cross() needs two vec3".into());
            }
            self.uses_glsl = true;
            let id = self.id("t");
            writeln!(self.body, "{id} = OpExtInst %v3float %glsl Cross {a} {b}").unwrap();
            return Ok(Some((id, Ty::Vec(3))));
        }
        // dot is a core instruction (OpDot), not an extended one.
        if name == "dot" {
            if args.len() != 2 { return Err("shader: dot() takes two arguments".into()); }
            let (a, ta) = self.expr(&args[0])?;
            let (b, tb) = self.expr(&args[1])?;
            if !matches!(ta, Ty::Vec(_)) || ta != tb {
                return Err("shader: dot() needs two matching vectors".into());
            }
            let id = self.id("t");
            writeln!(self.body, "{id} = OpDot %float {a} {b}").unwrap();
            return Ok(Some((id, Ty::Float)));
        }
        Ok(None)
    }

    /// A statement-position `out_color(Vec3)` call (a `@vertex` varying write):
    /// stores to the Location-0 Output. Returns `false` if `e` is not that call.
    fn void_call(&mut self, e: &Expr) -> Result<bool, String> {
        if let Expr::Call { callee, args, .. } = e {
            if let Expr::Ident(n, _) = callee.as_ref() {
                if n == "out_color" {
                    if args.len() != 1 {
                        return Err("shader: out_color(Vec3) takes one argument".into());
                    }
                    let (id, t) = self.expr(&args[0])?;
                    if t != Ty::Vec(3) {
                        return Err("shader: out_color expects a Vec3".into());
                    }
                    writeln!(self.body, "OpStore %vcol {id}").unwrap();
                    self.emits_varying = true;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Lower one statement. Returns `Some(value)` if it is a `return expr` (which
    /// terminates the enclosing block's value), else `None`.
    fn stmt(&mut self, st: &Stmt) -> Result<Option<(String, Ty)>, String> {
        match st {
            Stmt::Let { name, value: Some(v), .. } => {
                let (id, ty) = self.expr(v)?;
                self.bind(name, &id, ty);
                Ok(None)
            }
            Stmt::Assign { target, op, value, .. } => {
                self.assign(target, *op, value)?;
                Ok(None)
            }
            Stmt::While { cond, body, .. } => {
                self.lower_while(cond, body)?;
                Ok(None)
            }
            Stmt::Return(Some(e), _) => Ok(Some(self.expr(e)?)),
            Stmt::Expr(e) if self.void_call(e)? => Ok(None),
            // `if cond { … } [else { … }]` as a statement: effect-only branches (the
            // bodies assign / mutate; no value is produced).
            Stmt::Expr(Expr::If { cond, then, elifs, els, .. }) => {
                self.lower_if_effect(cond, then, elifs, els.as_ref())?;
                Ok(None)
            }
            _ => Err("shader: only `let`/`mut`, assignment, `while`, `if`, `out_color(...)`, and a final value expression are supported".into()),
        }
    }

    /// `if cond { … } [elif … ] [else … ]` run for effects (no value). Both branches
    /// are lowered as effect blocks; a missing `else` branches straight to the merge.
    fn lower_if_effect(&mut self, cond: &Expr, then: &Block, elifs: &[(Expr, Block)], els: Option<&Block>) -> Result<(), String> {
        let (c, ct) = self.expr(cond)?;
        if ct != Ty::Bool {
            return Err("shader: an `if` condition must be a comparison (bool)".into());
        }
        let then_l = self.id("then");
        let merge_l = self.id("merge");
        // The false target: an else/elif block, or the merge directly.
        let has_else = !elifs.is_empty() || els.is_some();
        let else_l = if has_else { self.id("else") } else { merge_l.clone() };
        writeln!(self.body, "OpSelectionMerge {merge_l} None").unwrap();
        writeln!(self.body, "OpBranchConditional {c} {then_l} {else_l}").unwrap();
        writeln!(self.body, "{then_l} = OpLabel").unwrap();
        self.block_effects(then)?;
        writeln!(self.body, "OpBranch {merge_l}").unwrap();
        if has_else {
            writeln!(self.body, "{else_l} = OpLabel").unwrap();
            if let Some(((econd, eblk), rest)) = elifs.split_first() {
                self.lower_if_effect(econd, eblk, rest, els)?;
            } else if let Some(e) = els {
                self.block_effects(e)?;
            }
            writeln!(self.body, "OpBranch {merge_l}").unwrap();
        }
        writeln!(self.body, "{merge_l} = OpLabel").unwrap();
        Ok(())
    }

    /// `name [op]= value` → store into the local's variable (load-op-store for `op=`).
    fn assign(&mut self, target: &Expr, op: Option<BinOp>, value: &Expr) -> Result<(), String> {
        let name = match target {
            Expr::Ident(n, _) => n.clone(),
            _ => return Err("shader: can only assign to a variable".into()),
        };
        let (ptr, ty) = self
            .env
            .get(&name)
            .cloned()
            .ok_or_else(|| format!("shader: assignment to unknown variable `{name}`"))?;
        let (v, vt) = self.expr(value)?;
        let stored = match op {
            None => {
                if vt != ty {
                    return Err("shader: assignment type mismatch".into());
                }
                v
            }
            Some(binop) => {
                let cur = self.id("t");
                writeln!(self.body, "{cur} = OpLoad {} {ptr}", ty.spirv()).unwrap();
                let (r, rt) = self.binary(binop, cur, ty, v, vt)?;
                if rt != ty {
                    return Err("shader: compound-assignment type mismatch".into());
                }
                r
            }
        };
        writeln!(self.body, "OpStore {ptr} {stored}").unwrap();
        Ok(())
    }

    /// `while cond { body }` → a structured loop (`OpLoopMerge`), body run for effects.
    fn lower_while(&mut self, cond: &Expr, body: &Block) -> Result<(), String> {
        let head = self.id("head");
        let check = self.id("check");
        let body_l = self.id("loopbody");
        let cont = self.id("cont");
        let merge = self.id("loopmerge");
        writeln!(self.body, "OpBranch {head}").unwrap();
        writeln!(self.body, "{head} = OpLabel").unwrap();
        writeln!(self.body, "OpLoopMerge {merge} {cont} None").unwrap();
        writeln!(self.body, "OpBranch {check}").unwrap();
        writeln!(self.body, "{check} = OpLabel").unwrap();
        let (c, ct) = self.expr(cond)?;
        if ct != Ty::Bool {
            return Err("shader: a `while` condition must be a comparison (bool)".into());
        }
        writeln!(self.body, "OpBranchConditional {c} {body_l} {merge}").unwrap();
        writeln!(self.body, "{body_l} = OpLabel").unwrap();
        self.block_effects(body)?;
        writeln!(self.body, "OpBranch {cont}").unwrap();
        writeln!(self.body, "{cont} = OpLabel").unwrap();
        writeln!(self.body, "OpBranch {head}").unwrap();
        writeln!(self.body, "{merge} = OpLabel").unwrap();
        Ok(())
    }

    /// Lower a block's statements for their effects, ignoring any tail value (used for
    /// loop bodies).
    fn block_effects(&mut self, b: &Block) -> Result<(), String> {
        for st in &b.stmts {
            self.stmt(st)?;
        }
        if let Some(t) = &b.tail {
            self.expr(t)?;
        }
        Ok(())
    }

    /// Lower a block and return its value — the tail expression (or an early `return`).
    fn block_value(&mut self, b: &Block) -> Result<(String, Ty), String> {
        for st in &b.stmts {
            if let Some(v) = self.stmt(st)? {
                return Ok(v);
            }
        }
        match &b.tail {
            Some(t) => self.expr(t),
            None => Err("shader: this block must end in a value expression".into()),
        }
    }
}

/// Compile an `@vertex fn` to SPIR-V assembly. The stage receives the built-in
/// triangle corner as its `Vec2` parameter (indexed by `gl_VertexIndex` from a
/// fixed position array) and returns a `Vec4` `gl_Position` — so a Vire vertex
/// shader *transforms* the geometry (scale/translate/…) without a vertex buffer.
pub fn compile_vertex(f: &FnDef) -> Result<String, String> {
    let body = f.body.as_ref().ok_or("shader: `@vertex` fn has no body")?;
    let param = f
        .sig
        .params
        .first()
        .map(|p| p.name.clone())
        .ok_or("shader: `@vertex fn` needs a Vec2 position parameter")?;
    let mut cx = new_cx();
    // The position attribute is loaded into `%pos` by the preamble; bind the param to
    // a Function-storage variable so the body can read (and even reassign) it.
    cx.bind(&param, "%pos", Ty::Vec(2));
    let (out, ty) = cx.block_value(body)?;
    if ty != Ty::Vec(4) {
        return Err("shader: the vertex output must be a Vec4 (gl_Position)".into());
    }
    // A `out_color(vec3)` varying adds a Location-0 Output the fragment reads back.
    let (vary_iface, vary_dec, vary_decl) = if cx.emits_varying {
        (
            " %vcol",
            "               OpDecorate %vcol Location 0\n",
            "      %ov3ptr = OpTypePointer Output %v3float\n       %vcol = OpVariable %ov3ptr Output\n",
        )
    } else {
        ("", "", "")
    };
    // `attr_color()` adds a per-vertex color Input attribute at Location 1 (the
    // vertex buffer must be the colored layout — `vk_mesh_c`).
    let (attr_iface, attr_dec, attr_decl) = if cx.uses_attr_color {
        (
            " %col_in",
            "               OpDecorate %col_in Location 1\n",
            "      %in3ptr = OpTypePointer Input %v3float\n     %col_in = OpVariable %in3ptr Input\n",
        )
    } else {
        ("", "", "")
    };
    let vary_iface = format!("{vary_iface}{attr_iface}");
    let vary_dec = format!("{vary_dec}{attr_dec}");
    let vary_decl = format!("{vary_decl}{attr_decl}");
    let glsl_import = if cx.uses_glsl { "       %glsl = OpExtInstImport \"GLSL.std.450\"\n" } else { "" };
    Ok(format!(
"               OpCapability Shader
{glsl_import}               OpMemoryModel Logical GLSL450
               OpEntryPoint Vertex %main \"main\" %out %pos_in{vary_iface}
               OpDecorate %glpv Block
               OpMemberDecorate %glpv 0 BuiltIn Position
               OpDecorate %pos_in Location 0
{vary_dec}       %void = OpTypeVoid
       %fnty = OpTypeFunction %void
      %float = OpTypeFloat 32
    %v2float = OpTypeVector %float 2
    %v3float = OpTypeVector %float 3
    %v4float = OpTypeVector %float 4
       %glpv = OpTypeStruct %v4float
     %outptr = OpTypePointer Output %glpv
        %out = OpVariable %outptr Output
      %inptr = OpTypePointer Input %v2float
     %pos_in = OpVariable %inptr Input
        %int = OpTypeInt 32 1
      %int_0 = OpConstant %int 0
     %ov4ptr = OpTypePointer Output %v4float
       %bool = OpTypeBool
   %pf_float = OpTypePointer Function %float
 %pf_v2float = OpTypePointer Function %v2float
 %pf_v3float = OpTypePointer Function %v3float
 %pf_v4float = OpTypePointer Function %v4float
    %pf_bool = OpTypePointer Function %bool
{vary_decl}{consts}       %main = OpFunction %void None %fnty
        %lbl = OpLabel
{vars}        %pos = OpLoad %v2float %pos_in
{body}         %gp = OpAccessChain %ov4ptr %out %int_0
               OpStore %gp {out}
               OpReturn
               OpFunctionEnd
",
        glsl_import = glsl_import,
        vary_iface = vary_iface,
        vary_dec = vary_dec,
        vary_decl = vary_decl,
        consts = cx.consts,
        vars = cx.vars,
        body = cx.body,
        out = out
    ))
}

/// Compile an `@fragment fn` to SPIR-V assembly, or an error message.
pub fn compile_fragment(f: &FnDef) -> Result<String, String> {
    let body = f.body.as_ref().ok_or("shader: `@fragment` fn has no body")?;
    let mut cx = new_cx();
    let (out, ty) = cx.block_value(body)?;
    if ty != Ty::Vec(4) {
        return Err("shader: the fragment output must be a Vec4".into());
    }
    // gl_FragCoord (the pixel position) is declared only when a `frag_*` builtin is
    // used — listed in the entry-point interface + decorated BuiltIn FragCoord.
    let (fc_iface, fc_decorate, fc_decl) = if cx.uses_fragcoord {
        (
            " %gl_FragCoord",
            "               OpDecorate %gl_FragCoord BuiltIn FragCoord\n",
            "%_ptr_Input_v4float = OpTypePointer Input %v4float\n%gl_FragCoord = OpVariable %_ptr_Input_v4float Input\n",
        )
    } else {
        ("", "", "")
    };
    // The interpolated varying the `@vertex` stage wrote (`in_color()`): a Location-0
    // Input vec3. (Output Location 0 = %color and Input Location 0 are separate
    // namespaces in Vulkan, so they don't collide.)
    let (vy_iface, vy_decorate, vy_decl) = if cx.uses_varying {
        (
            " %vcol_in",
            "               OpDecorate %vcol_in Location 0\n",
            "%_ptr_Input_v3float = OpTypePointer Input %v3float\n%vcol_in = OpVariable %_ptr_Input_v3float Input\n",
        )
    } else {
        ("", "", "")
    };
    let iface = format!("{fc_iface}{vy_iface}");
    let fc_decorate = format!("{fc_decorate}{vy_decorate}");
    let fc_decl = format!("{fc_decl}{vy_decl}");
    let glsl_import = if cx.uses_glsl { "       %glsl = OpExtInstImport \"GLSL.std.450\"\n" } else { "" };
    Ok(format!(
"               OpCapability Shader
{glsl_import}               OpMemoryModel Logical GLSL450
               OpEntryPoint Fragment %main \"main\" %color{iface}
               OpExecutionMode %main OriginUpperLeft
               OpDecorate %color Location 0
{fc_decorate}       %void = OpTypeVoid
       %fnty = OpTypeFunction %void
      %float = OpTypeFloat 32
    %v2float = OpTypeVector %float 2
    %v3float = OpTypeVector %float 3
    %v4float = OpTypeVector %float 4
       %optr = OpTypePointer Output %v4float
      %color = OpVariable %optr Output
       %bool = OpTypeBool
   %pf_float = OpTypePointer Function %float
 %pf_v2float = OpTypePointer Function %v2float
 %pf_v3float = OpTypePointer Function %v3float
 %pf_v4float = OpTypePointer Function %v4float
    %pf_bool = OpTypePointer Function %bool
{fc_decl}{consts}       %main = OpFunction %void None %fnty
        %lbl = OpLabel
{vars}{body}               OpStore %color {out}
               OpReturn
               OpFunctionEnd
",
        glsl_import = glsl_import,
        iface = iface,
        fc_decorate = fc_decorate,
        fc_decl = fc_decl,
        consts = cx.consts,
        vars = cx.vars,
        body = cx.body,
        out = out
    ))
}

/// Compile a Vire `@mesh fn` to a SPIR-V mesh shader (VK_EXT_mesh_shader). The body
/// is a straight-line meshlet emit: `set_mesh_outputs(nv, np)` first, then
/// `mesh_pos(i, vec4expr)` to write each vertex position (the expression is full
/// Vire — arithmetic, `vecN`, GLSL builtins), and `mesh_tri(i, a, b, c)` to write
/// each triangle's vertex indices. `let` bindings may share computation. One
/// workgroup emits one meshlet (SPIR-V 1.4).
pub fn compile_mesh(f: &FnDef) -> Result<String, String> {
    let body = f.body.as_ref().ok_or("shader: `@mesh` fn has no body")?;
    let mut cx = new_cx();
    let mut ints: BTreeSet<i64> = BTreeSet::new();   // AccessChain indices (%i_N)
    let mut uints: BTreeSet<i64> = BTreeSet::new();   // sizes + triangle indices (%u_N)
    let mut caps: Option<(i64, i64)> = None;
    let mut prim_consts = String::new();              // OpConstantComposite per triangle
    let mut primk = 0u32;
    let mut emits_mesh_color = false;                 // mesh_color(i, vec3) → per-vertex Location-0 output
    uints.insert(1); // %u_1 sizes the built-in ClipDistance/CullDistance arrays
    ints.insert(0);  // %i_0 selects gl_Position / scene record member 0
    ints.insert(1);  // %i_1 selects the scene record's second field (cone)

    // A trailing call with no `;` parses as the block tail — treat it as a statement.
    let tail_stmt = body.tail.as_ref().map(|t| Stmt::Expr((**t).clone()));
    for (idx, st) in body.stmts.iter().chain(tail_stmt.iter()).enumerate() {
        match st {
            Stmt::Let { name, value: Some(v), .. } => {
                let (id, ty) = cx.expr(v)?;
                cx.bind(name, &id, ty);
            }
            Stmt::Expr(Expr::Call { callee, args, .. }) => {
                let name = match callee.as_ref() {
                    Expr::Ident(n, _) => n.as_str(),
                    _ => return Err("shader: unsupported @mesh call".into()),
                };
                match name {
                    "set_mesh_outputs" => {
                        if idx != 0 {
                            return Err("shader: set_mesh_outputs(nv, np) must be the first @mesh statement".into());
                        }
                        if args.len() != 2 { return Err("shader: set_mesh_outputs(nv, np)".into()); }
                        let nv = int_lit(&args[0])?;
                        let np = int_lit(&args[1])?;
                        uints.insert(nv);
                        uints.insert(np);
                        caps = Some((nv, np));
                        writeln!(cx.body, "OpSetMeshOutputsEXT %u_{nv} %u_{np}").unwrap();
                    }
                    "mesh_pos" => {
                        if args.len() != 2 { return Err("shader: mesh_pos(i, Vec4)".into()); }
                        let i = int_lit(&args[0])?;
                        let (id, ty) = cx.expr(&args[1])?;
                        if ty != Ty::Vec(4) { return Err("shader: mesh_pos position must be a Vec4".into()); }
                        ints.insert(i);
                        let ac = cx.id("t");
                        writeln!(cx.body, "{ac} = OpAccessChain %_ptr_Output_v4float %gl_MeshVerticesEXT %i_{i} %i_0").unwrap();
                        writeln!(cx.body, "OpStore {ac} {id}").unwrap();
                    }
                    "mesh_tri" => {
                        if args.len() != 4 { return Err("shader: mesh_tri(i, a, b, c)".into()); }
                        let i = int_lit(&args[0])?;
                        let a = int_lit(&args[1])?;
                        let b = int_lit(&args[2])?;
                        let c = int_lit(&args[3])?;
                        ints.insert(i);
                        uints.insert(a); uints.insert(b); uints.insert(c);
                        let prim = format!("%prim{primk}");
                        primk += 1;
                        writeln!(prim_consts, "{prim} = OpConstantComposite %v3uint %u_{a} %u_{b} %u_{c}").unwrap();
                        let ac = cx.id("t");
                        writeln!(cx.body, "{ac} = OpAccessChain %_ptr_Output_v3uint %gl_PrimitiveTriangleIndicesEXT %i_{i}").unwrap();
                        writeln!(cx.body, "OpStore {ac} {prim}").unwrap();
                    }
                    "mesh_color" => {
                        // A per-vertex colour output (Location 0) the fragment reads
                        // interpolated via in_color() — a mesh-shader vertex attribute.
                        if args.len() != 2 { return Err("shader: mesh_color(i, Vec3)".into()); }
                        let i = int_lit(&args[0])?;
                        let (id, ty) = cx.expr(&args[1])?;
                        if ty != Ty::Vec(3) { return Err("shader: mesh_color must be a Vec3".into()); }
                        ints.insert(i);
                        emits_mesh_color = true;
                        let ac = cx.id("t");
                        writeln!(cx.body, "{ac} = OpAccessChain %_ptr_Output_v3float %vColor %i_{i}").unwrap();
                        writeln!(cx.body, "OpStore {ac} {id}").unwrap();
                    }
                    other => return Err(format!("shader: unsupported @mesh call `{other}`")),
                }
            }
            _ => return Err("shader: `@mesh` supports set_mesh_outputs / mesh_pos / mesh_tri / mesh_color / let".into()),
        }
    }
    let (nv, np) = caps.ok_or("shader: `@mesh` must call set_mesh_outputs(nv, np) first")?;
    let mut const_decls = String::new();
    for u in &uints { writeln!(const_decls, "%u_{u} = OpConstant %uint {u}").unwrap(); }
    for i in &ints { writeln!(const_decls, "%i_{i} = OpConstant %int {i}").unwrap(); }
    let glsl_import = if cx.uses_glsl { "       %glsl = OpExtInstImport \"GLSL.std.450\"\n" } else { "" };
    // The GPU-driven resources this mesh stage touches (scene SSBO, gl_WorkGroupID for
    // the plain scene path, or the task payload for the fused-cull path).
    let (scene_iface, scene_decor, scene_decl) =
        resource_decls(cx.uses_ssbo, cx.uses_workgroup_id, false, cx.uses_payload, false, false);
    // A per-vertex colour output (Location 0, sized to the vertex cap) — declared only
    // when `mesh_color()` is used; the fragment reads it interpolated via `in_color()`.
    let (color_iface, color_decor, color_decl) = if emits_mesh_color {
        (
            " %vColor".to_string(),
            "               OpDecorate %vColor Location 0\n".to_string(),
            format!("%_arr_v3col = OpTypeArray %v3float %u_{nv}\n%_ptr_out_v3col = OpTypePointer Output %_arr_v3col\n     %vColor = OpVariable %_ptr_out_v3col Output\n%_ptr_Output_v3float = OpTypePointer Output %v3float\n"),
        )
    } else {
        (String::new(), String::new(), String::new())
    };
    Ok(format!(
"               OpCapability MeshShadingEXT
               OpExtension \"SPV_EXT_mesh_shader\"
{glsl_import}               OpMemoryModel Logical GLSL450
               OpEntryPoint MeshEXT %main \"main\" %gl_MeshVerticesEXT %gl_PrimitiveTriangleIndicesEXT{scene_iface}{color_iface}
               OpExecutionModeId %main LocalSizeId %u_1 %u_1 %u_1
               OpExecutionMode %main OutputVertices {nv}
               OpExecutionMode %main OutputPrimitivesEXT {np}
               OpExecutionMode %main OutputTrianglesEXT
{color_decor}{scene_decor}               OpDecorate %gl_MeshPerVertexEXT Block
               OpMemberDecorate %gl_MeshPerVertexEXT 0 BuiltIn Position
               OpMemberDecorate %gl_MeshPerVertexEXT 1 BuiltIn PointSize
               OpMemberDecorate %gl_MeshPerVertexEXT 2 BuiltIn ClipDistance
               OpMemberDecorate %gl_MeshPerVertexEXT 3 BuiltIn CullDistance
               OpDecorate %gl_PrimitiveTriangleIndicesEXT BuiltIn PrimitiveTriangleIndicesEXT
       %void = OpTypeVoid
       %fnty = OpTypeFunction %void
       %uint = OpTypeInt 32 0
        %int = OpTypeInt 32 1
      %float = OpTypeFloat 32
    %v2float = OpTypeVector %float 2
    %v3float = OpTypeVector %float 3
    %v4float = OpTypeVector %float 4
     %v3uint = OpTypeVector %uint 3
       %bool = OpTypeBool
   %pf_float = OpTypePointer Function %float
 %pf_v2float = OpTypePointer Function %v2float
 %pf_v3float = OpTypePointer Function %v3float
 %pf_v4float = OpTypePointer Function %v4float
    %pf_bool = OpTypePointer Function %bool
{const_decls}%_arr_float_u1 = OpTypeArray %float %u_1
%gl_MeshPerVertexEXT = OpTypeStruct %v4float %float %_arr_float_u1 %_arr_float_u1
%_arr_mpv = OpTypeArray %gl_MeshPerVertexEXT %u_{nv}
%_ptr_out_mpv = OpTypePointer Output %_arr_mpv
%gl_MeshVerticesEXT = OpVariable %_ptr_out_mpv Output
%_ptr_Output_v4float = OpTypePointer Output %v4float
%_arr_idx = OpTypeArray %v3uint %u_{np}
%_ptr_out_idx = OpTypePointer Output %_arr_idx
%gl_PrimitiveTriangleIndicesEXT = OpVariable %_ptr_out_idx Output
%_ptr_Output_v3uint = OpTypePointer Output %v3uint
{color_decl}{scene_decl}{prim_consts}{consts}       %main = OpFunction %void None %fnty
        %lbl = OpLabel
{vars}{body}               OpReturn
               OpFunctionEnd
",
        glsl_import = glsl_import,
        nv = nv, np = np,
        const_decls = const_decls,
        prim_consts = prim_consts,
        scene_iface = scene_iface,
        scene_decor = scene_decor,
        scene_decl = scene_decl,
        color_iface = color_iface,
        color_decor = color_decor,
        color_decl = color_decl,
        consts = cx.consts,
        vars = cx.vars,
        body = cx.body,
    ))
}

/// Compile a Vire `@task fn` (amplification shader) to a SPIR-V task shader. The body
/// dispatches mesh workgroups with `emit_mesh_tasks(arg)` — the GPU decides how many
/// meshlets run, the basis for GPU culling. `arg` is either an integer literal (a
/// fixed count) or a boolean (`emit 1 if true, 0 if false`, via `OpSelect`), so a
/// frustum test like `emit_mesh_tasks(dot(cull_plane(), center) > -r)` culls the
/// meshlet on the GPU. `let` bindings may share work. Terminates in `OpEmitMeshTasksEXT`
/// (SPIR-V 1.4).
pub fn compile_task(f: &FnDef) -> Result<String, String> {
    let body = f.body.as_ref().ok_or("shader: `@task` fn has no body")?;
    let mut cx = new_cx();
    let mut count_op: Option<String> = None;   // the emit count operand (a %uint id)
    let mut emit_payload = false;              // emit_visible → pass the payload to mesh
    let mut uints: BTreeSet<i64> = BTreeSet::new();
    uints.insert(0);
    uints.insert(1);

    let tail_stmt = body.tail.as_ref().map(|t| Stmt::Expr((**t).clone()));
    for st in body.stmts.iter().chain(tail_stmt.iter()) {
        let emit_call = |n: &str| n == "emit_mesh_tasks" || n == "emit_visible";
        match st {
            Stmt::Let { name, value: Some(v), .. } => {
                let (id, ty) = cx.expr(v)?;
                cx.bind(name, &id, ty);
            }
            Stmt::Expr(Expr::Call { callee, args, .. })
                if matches!(callee.as_ref(), Expr::Ident(n, _) if emit_call(n)) =>
            {
                let fname = match callee.as_ref() { Expr::Ident(n, _) => n.as_str(), _ => unreachable!() };
                if args.len() != 1 { return Err(format!("shader: {fname}(arg)")); }
                if count_op.is_some() {
                    return Err("shader: `@task` emits once".into());
                }
                // `emit_visible` writes THIS workgroup's index into the task payload so
                // the surviving mesh workgroup knows which meshlet it is (GPU cull).
                if fname == "emit_visible" {
                    emit_payload = true;
                    cx.uses_workgroup_id = true;
                    cx.uses_payload = true;
                    let wid = cx.id("t");
                    writeln!(cx.body, "{wid} = OpLoad %v3uint %gl_WorkGroupID").unwrap();
                    let wx = cx.id("t");
                    writeln!(cx.body, "{wx} = OpCompositeExtract %uint {wid} 0").unwrap();
                    let pp = cx.id("t");
                    writeln!(cx.body, "{pp} = OpAccessChain %_ptr_pl_uint %pl %i_0").unwrap();
                    writeln!(cx.body, "OpStore {pp} {wx}").unwrap();
                }
                // Integer literal → a fixed count; a boolean → select 1/0 (GPU cull).
                if let Ok(k) = int_lit(&args[0]) {
                    uints.insert(k);
                    count_op = Some(format!("%u_{k}"));
                } else {
                    let (cond, ty) = cx.expr(&args[0])?;
                    if ty != Ty::Bool {
                        return Err(format!("shader: {fname}(arg) — arg must be an integer or a bool"));
                    }
                    let sel = cx.id("t");
                    writeln!(cx.body, "{sel} = OpSelect %uint {cond} %u_1 %u_0").unwrap();
                    count_op = Some(sel);
                }
            }
            _ => return Err("shader: `@task` supports `let` and one emit_mesh_tasks/emit_visible(arg)".into()),
        }
    }
    let count_op = count_op.ok_or("shader: `@task` must call emit_mesh_tasks/emit_visible(arg)")?;
    let payload_op = if emit_payload { " %pl" } else { "" };
    let mut const_decls = String::from("        %i_0 = OpConstant %int 0\n        %i_1 = OpConstant %int 1\n");
    for u in &uints { writeln!(const_decls, "%u_{u} = OpConstant %uint {u}").unwrap(); }
    let glsl_import = if cx.uses_glsl { "       %glsl = OpExtInstImport \"GLSL.std.450\"\n" } else { "" };
    let (res_iface, res_decor, res_decl) =
        resource_decls(cx.uses_ssbo, cx.uses_workgroup_id, false, cx.uses_payload, cx.uses_push_constant, false);
    Ok(format!(
"               OpCapability MeshShadingEXT
               OpExtension \"SPV_EXT_mesh_shader\"
{glsl_import}               OpMemoryModel Logical GLSL450
               OpEntryPoint TaskEXT %main \"main\"{res_iface}
               OpExecutionModeId %main LocalSizeId %u_1 %u_1 %u_1
{res_decor}       %void = OpTypeVoid
       %fnty = OpTypeFunction %void
       %uint = OpTypeInt 32 0
        %int = OpTypeInt 32 1
      %float = OpTypeFloat 32
    %v2float = OpTypeVector %float 2
    %v3float = OpTypeVector %float 3
    %v4float = OpTypeVector %float 4
     %v3uint = OpTypeVector %uint 3
       %bool = OpTypeBool
   %pf_float = OpTypePointer Function %float
 %pf_v2float = OpTypePointer Function %v2float
 %pf_v3float = OpTypePointer Function %v3float
 %pf_v4float = OpTypePointer Function %v4float
    %pf_bool = OpTypePointer Function %bool
{const_decls}{res_decl}{consts}       %main = OpFunction %void None %fnty
        %lbl = OpLabel
{vars}{body}               OpEmitMeshTasksEXT {count_op} %u_1 %u_1{payload_op}
               OpFunctionEnd
",
        glsl_import = glsl_import,
        res_iface = res_iface,
        res_decor = res_decor,
        res_decl = res_decl,
        const_decls = const_decls,
        consts = cx.consts,
        vars = cx.vars,
        body = cx.body,
        count_op = count_op,
        payload_op = payload_op,
    ))
}

/// Compile a Vire `@compute fn` to a SPIR-V compute shader that BUILDS the scene
/// buffer on the GPU. Each invocation (indexed by `gl_GlobalInvocationID.x`) computes
/// one meshlet record and writes it with `set_meshlet(vec2)` — so the meshlet set the
/// mesh pipeline later draws is produced on the GPU, not supplied by the host.
/// `meshlet_index()` gives this invocation's index as a float for placement formulas.
pub fn compile_compute(f: &FnDef) -> Result<String, String> {
    let body = f.body.as_ref().ok_or("shader: `@compute` fn has no body")?;
    let mut cx = new_cx();
    let tail_stmt = body.tail.as_ref().map(|t| Stmt::Expr((**t).clone()));
    for st in body.stmts.iter().chain(tail_stmt.iter()) {
        match st {
            Stmt::Let { name, value: Some(v), .. } => {
                let (id, ty) = cx.expr(v)?;
                cx.bind(name, &id, ty);
            }
            Stmt::Expr(Expr::Call { callee, args, .. })
                if matches!(callee.as_ref(), Expr::Ident(n, _) if n == "set_meshlet") =>
            {
                if args.is_empty() || args.len() > 2 { return Err("shader: set_meshlet(offset[, cone])".into()); }
                let (val, ty) = cx.expr(&args[0])?;
                if ty != Ty::Vec(2) { return Err("shader: set_meshlet expects a Vec2".into()); }
                cx.uses_ssbo = true;
                cx.uses_global_id = true;
                let g = cx.id("t");
                writeln!(cx.body, "{g} = OpLoad %v3uint %gl_GlobalInvocationID").unwrap();
                let gx = cx.id("t");
                writeln!(cx.body, "{gx} = OpCompositeExtract %uint {g} 0").unwrap();
                let p = cx.id("t");
                writeln!(cx.body, "{p} = OpAccessChain %_ptr_ssbo_v2float %scene %i_0 {gx} %i_0").unwrap();
                writeln!(cx.body, "OpStore {p} {val}").unwrap();
                // Optional second arg: the meshlet's cone/facing direction (member 1).
                if args.len() == 2 {
                    let (cval, cty) = cx.expr(&args[1])?;
                    if cty != Ty::Vec(2) { return Err("shader: set_meshlet cone must be a Vec2".into()); }
                    let cp = cx.id("t");
                    writeln!(cx.body, "{cp} = OpAccessChain %_ptr_ssbo_v2float %scene %i_0 {gx} %i_1").unwrap();
                    writeln!(cx.body, "OpStore {cp} {cval}").unwrap();
                }
            }
            _ => return Err("shader: `@compute` (builder) supports `let` and set_meshlet(Vec2)".into()),
        }
    }
    // The scene SSBO is WRITABLE here (the mesh/task stages read the same buffer
    // read-only); this compute builder writes it, indexed by gl_GlobalInvocationID.
    let (iface, decor, decl) =
        resource_decls(cx.uses_ssbo, false, cx.uses_global_id, false, false, true);
    let glsl_import = if cx.uses_glsl { "       %glsl = OpExtInstImport \"GLSL.std.450\"\n" } else { "" };
    Ok(format!(
"               OpCapability Shader
{glsl_import}               OpMemoryModel Logical GLSL450
               OpEntryPoint GLCompute %main \"main\"{iface}
               OpExecutionMode %main LocalSize 1 1 1
{decor}       %void = OpTypeVoid
       %fnty = OpTypeFunction %void
       %uint = OpTypeInt 32 0
        %int = OpTypeInt 32 1
      %float = OpTypeFloat 32
    %v2float = OpTypeVector %float 2
    %v3float = OpTypeVector %float 3
    %v4float = OpTypeVector %float 4
     %v3uint = OpTypeVector %uint 3
       %bool = OpTypeBool
   %pf_float = OpTypePointer Function %float
 %pf_v2float = OpTypePointer Function %v2float
 %pf_v3float = OpTypePointer Function %v3float
 %pf_v4float = OpTypePointer Function %v4float
    %pf_bool = OpTypePointer Function %bool
        %i_0 = OpConstant %int 0
        %i_1 = OpConstant %int 1
{decl}{consts}       %main = OpFunction %void None %fnty
        %lbl = OpLabel
{vars}{body}               OpReturn
               OpFunctionEnd
",
        glsl_import = glsl_import,
        iface = iface,
        decor = decor,
        decl = decl,
        consts = cx.consts,
        vars = cx.vars,
        body = cx.body,
    ))
}

/// Compile a Vire `@gpuvk fn` to a SPIR-V compute shader — vendor-neutral Vulkan
/// compute (runs on any Vulkan device: Intel / NVIDIA / AMD), distinct from the
/// CUDA/ROCm `@gpu` path. It is a data-parallel **map**: each invocation reads its
/// element with `elem()` (`buffer[gl_GlobalInvocationID.x]`), and the function's
/// value is written back to that element. A bounds guard (`gid < count`, count from a
/// push constant) makes an over-dispatched workgroup safe. SPIR-V 1.4.
pub fn compile_gpuvk(f: &FnDef) -> Result<String, String> {
    let body = f.body.as_ref().ok_or("shader: `@gpuvk` fn has no body")?;
    let mut cx = new_cx();
    // Bounds guard: `if gl_GlobalInvocationID.x < count { … store … }`.
    let g = cx.id("t");
    writeln!(cx.body, "{g} = OpLoad %v3uint %gvid").unwrap();
    let gx = cx.id("t");
    writeln!(cx.body, "{gx} = OpCompositeExtract %uint {g} 0").unwrap();
    let cp = cx.id("t");
    writeln!(cx.body, "{cp} = OpAccessChain %_ptr_pc_uint %pcv %i_0").unwrap();
    let cnt = cx.id("t");
    writeln!(cx.body, "{cnt} = OpLoad %uint {cp}").unwrap();
    let ok = cx.id("t");
    writeln!(cx.body, "{ok} = OpULessThan %bool {gx} {cnt}").unwrap();
    let run = cx.id("run");
    let mrg = cx.id("mrg");
    writeln!(cx.body, "OpSelectionMerge {mrg} None").unwrap();
    writeln!(cx.body, "OpBranchConditional {ok} {run} {mrg}").unwrap();
    writeln!(cx.body, "{run} = OpLabel").unwrap();
    let (val, ty) = cx.block_value(body)?;
    if ty != Ty::Float {
        return Err("shader: `@gpuvk` must return a Float (the new element value)".into());
    }
    let sp = cx.id("t");
    writeln!(cx.body, "{sp} = OpAccessChain %_ptr_ssbo_float %vbuf %i_0 {gx}").unwrap();
    writeln!(cx.body, "OpStore {sp} {val}").unwrap();
    writeln!(cx.body, "OpBranch {mrg}").unwrap();
    writeln!(cx.body, "{mrg} = OpLabel").unwrap();
    let glsl_import = if cx.uses_glsl { "       %glsl = OpExtInstImport \"GLSL.std.450\"\n" } else { "" };
    Ok(format!(
"               OpCapability Shader
{glsl_import}               OpMemoryModel Logical GLSL450
               OpEntryPoint GLCompute %main \"main\" %gvid %vbuf %pcv
               OpExecutionMode %main LocalSize 64 1 1
               OpDecorate %gvid BuiltIn GlobalInvocationId
               OpDecorate %_rt_float ArrayStride 4
               OpMemberDecorate %Buf 0 Offset 0
               OpDecorate %Buf Block
               OpDecorate %vbuf DescriptorSet 0
               OpDecorate %vbuf Binding 0
               OpDecorate %pcU Block
               OpMemberDecorate %pcU 0 Offset 0
       %void = OpTypeVoid
       %fnty = OpTypeFunction %void
       %uint = OpTypeInt 32 0
        %int = OpTypeInt 32 1
      %float = OpTypeFloat 32
    %v2float = OpTypeVector %float 2
    %v3float = OpTypeVector %float 3
    %v4float = OpTypeVector %float 4
     %v3uint = OpTypeVector %uint 3
       %bool = OpTypeBool
   %pf_float = OpTypePointer Function %float
 %pf_v2float = OpTypePointer Function %v2float
 %pf_v3float = OpTypePointer Function %v3float
 %pf_v4float = OpTypePointer Function %v4float
    %pf_bool = OpTypePointer Function %bool
        %i_0 = OpConstant %int 0
%_ptr_in_v3uint = OpTypePointer Input %v3uint
       %gvid = OpVariable %_ptr_in_v3uint Input
   %_rt_float = OpTypeRuntimeArray %float
        %Buf = OpTypeStruct %_rt_float
%_ptr_ssbo_Buf = OpTypePointer StorageBuffer %Buf
       %vbuf = OpVariable %_ptr_ssbo_Buf StorageBuffer
%_ptr_ssbo_float = OpTypePointer StorageBuffer %float
        %pcU = OpTypeStruct %uint
%_ptr_pc_U = OpTypePointer PushConstant %pcU
        %pcv = OpVariable %_ptr_pc_U PushConstant
%_ptr_pc_uint = OpTypePointer PushConstant %uint
{consts}       %main = OpFunction %void None %fnty
        %lbl = OpLabel
{vars}{body}               OpReturn
               OpFunctionEnd
",
        glsl_import = glsl_import,
        consts = cx.consts,
        vars = cx.vars,
        body = cx.body,
    ))
}
