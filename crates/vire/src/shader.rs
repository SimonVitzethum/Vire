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

use std::collections::HashMap;
use std::fmt::Write;

use crate::ast::{BinOp, Block, Expr, FnDef, Stmt};

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
            // Single-component swizzle: `v.x`/`.y`/`.z`/`.w` → OpCompositeExtract.
            Expr::Field { base, name, .. } => {
                let (id, t) = self.expr(base)?;
                let k = match t {
                    Ty::Vec(k) => k,
                    Ty::Float | Ty::Bool => return Err("shader: swizzle on a non-vector".into()),
                };
                let comp = match name.as_str() {
                    "x" => 0,
                    "y" => 1,
                    "z" => 2,
                    "w" => 3,
                    _ => return Err(format!("shader: unknown swizzle `.{name}` (only .x/.y/.z/.w)")),
                };
                if comp >= k {
                    return Err(format!("shader: `.{name}` out of range for a vec{k}"));
                }
                let r = self.id("t");
                writeln!(self.body, "{r} = OpCompositeExtract %float {id} {comp}").unwrap();
                Ok((r, Ty::Float))
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
            Stmt::Expr(Expr::If { .. }) => {
                Err("shader: `if` is supported as a value (the block's result), not as a bare statement".into())
            }
            _ => Err("shader: only `let`/`mut`, assignment, `while`, `if`-value, `out_color(...)`, and a final value expression are supported".into()),
        }
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
    let mut cx = Cx {
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
        n: 0,
    };
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
    let mut cx = Cx {
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
        n: 0,
    };
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
