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

/// A shader value type: a float scalar or an N-component float vector.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Float,
    Vec(u8),
}

impl Ty {
    fn spirv(self) -> &'static str {
        match self {
            Ty::Float => "%float",
            Ty::Vec(2) => "%v2float",
            Ty::Vec(3) => "%v3float",
            Ty::Vec(_) => "%v4float",
        }
    }
}

struct Cx {
    consts: String,             // `%kN = OpConstant %float …` lines
    body: String,               // function-body instructions
    const_cache: HashMap<u32, String>, // float bits → id
    env: HashMap<String, (String, Ty)>, // local name → (id, type)
    uses_fragcoord: bool,       // `frag_x/frag_y/frag_coord` → declare gl_FragCoord
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

    fn expr(&mut self, e: &Expr) -> Result<(String, Ty), String> {
        match e {
            Expr::Float(v, _) => Ok((self.constant(*v as f32), Ty::Float)),
            Expr::Int(v, _) => Ok((self.constant(*v as f32), Ty::Float)),
            Expr::Ident(n, _) => self
                .env
                .get(n)
                .cloned()
                .ok_or_else(|| format!("shader: unknown variable `{n}`")),
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
                let n = match name {
                    "vec2" => 2u8,
                    "vec3" => 3,
                    "vec4" => 4,
                    other => return Err(format!("shader: unsupported call `{other}` (only vec2/3/4)")),
                };
                if args.len() != n as usize {
                    return Err(format!("shader: {name} needs {n} scalar args"));
                }
                let mut parts = Vec::new();
                for a in args {
                    let (id, t) = self.expr(a)?;
                    if t != Ty::Float {
                        return Err(format!("shader: {name} args must be scalars"));
                    }
                    parts.push(id);
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
            _ => Err("shader: unsupported expression (only literals, vars, vecN, +-*/)".into()),
        }
    }

    fn binary(&mut self, op: BinOp, a: String, ta: Ty, b: String, tb: Ty) -> Result<(String, Ty), String> {
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

    fn block_output(&mut self, b: &Block) -> Result<(String, Ty), String> {
        for st in &b.stmts {
            match st {
                Stmt::Let { name, value: Some(v), .. } => {
                    let r = self.expr(v)?;
                    self.env.insert(name.clone(), r);
                }
                Stmt::Return(Some(e), _) => return self.expr(e),
                _ => return Err("shader: only `let`/`mut` bindings and a final color expression are supported".into()),
            }
        }
        match &b.tail {
            Some(t) => self.expr(t),
            None => Err("shader: the fragment must end in a color expression (a Vec4)".into()),
        }
    }
}

/// Compile an `@fragment fn` to SPIR-V assembly, or an error message.
pub fn compile_fragment(f: &FnDef) -> Result<String, String> {
    let body = f.body.as_ref().ok_or("shader: `@fragment` fn has no body")?;
    let mut cx = Cx {
        consts: String::new(),
        body: String::new(),
        const_cache: HashMap::new(),
        env: HashMap::new(),
        uses_fragcoord: false,
        n: 0,
    };
    let (out, ty) = cx.block_output(body)?;
    if ty != Ty::Vec(4) {
        return Err("shader: the fragment output must be a Vec4".into());
    }
    // gl_FragCoord (the pixel position) is declared only when a `frag_*` builtin is
    // used — listed in the entry-point interface + decorated BuiltIn FragCoord.
    let (iface, fc_decorate, fc_decl) = if cx.uses_fragcoord {
        (
            " %gl_FragCoord",
            "               OpDecorate %gl_FragCoord BuiltIn FragCoord\n",
            "%_ptr_Input_v4float = OpTypePointer Input %v4float\n%gl_FragCoord = OpVariable %_ptr_Input_v4float Input\n",
        )
    } else {
        ("", "", "")
    };
    Ok(format!(
"               OpCapability Shader
               OpMemoryModel Logical GLSL450
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
{fc_decl}{consts}       %main = OpFunction %void None %fnty
        %lbl = OpLabel
{body}               OpStore %color {out}
               OpReturn
               OpFunctionEnd
",
        iface = iface,
        fc_decorate = fc_decorate,
        fc_decl = fc_decl,
        consts = cx.consts,
        body = cx.body,
        out = out
    ))
}
