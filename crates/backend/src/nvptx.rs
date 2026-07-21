//! NVPTX device-code emitter for Vire `@gpu` kernels, plus the generated C host
//! launch stubs.
//!
//! A `@gpu` function is kept out of `Program::functions` (so no host solver pass,
//! RTA, or inliner touches it) and lives in `Program::gpu_kernels`. This module
//! turns each kernel into two artifacts:
//!
//!  1. **Device IR** (`emit_ptx`): one `nvptx64-nvidia-cuda` LLVM module with each
//!     kernel as a `ptx_kernel` function. Array parameters become raw
//!     `ptr addrspace(1)` (global memory); `gpu_*` intrinsics become
//!     `@llvm.nvvm.read.ptx.sreg.*` reads. `llc -march=nvptx64` compiles this to
//!     PTX. Device code is UNCHECKED (like CUDA): the `if gpu_gid() < n` guard is
//!     the programmer's responsibility — there are no bounds checks and no RC.
//!
//!  2. **Host launch stubs** (`emit_gpu_stubs`): a C function per kernel whose
//!     symbol is exactly the kernel's mangled name, so a host `Call { func: name }`
//!     links straight to it. It marshals arguments (scalars by value, arrays via
//!     H2D upload → launch → D2H copyback) through the `jrt_gpu_*` runtime.
//!
//! The design mirrors NVlabs/cuda-oxide (Apache-2.0, see third_party/cuda-oxide):
//! single-source kernels → LLVM IR → PTX, typed device buffers, launch-by-N.

use std::fmt::Write;

use fastllvm_ir::*;

/// The scalar LLVM type of a kernel value type.
fn scal(ty: Ty) -> &'static str {
    match ty {
        Ty::I32 => "i32",
        Ty::I64 => "i64",
        Ty::F32 => "float",
        Ty::F64 => "double",
        Ty::Ref => "ptr",
        Ty::Void => "void",
    }
}

/// Device storage element type for an array kind.
fn elem(k: ArrKind) -> &'static str {
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

fn is_float(ty: Ty) -> bool {
    matches!(ty, Ty::F32 | Ty::F64)
}

/// Emits the combined NVPTX device module for all kernels, or `Ok(None)` if the
/// program has none. `Err` collects every unsupported-construct diagnostic.
pub fn emit_ptx(program: &Program) -> Result<Option<String>, Vec<String>> {
    if program.gpu_kernels.is_empty() {
        return Ok(None);
    }
    let mut w = String::new();
    w.push_str("; Vire @gpu device module — target NVPTX (see language/GPU-KERNELS.md)\n");
    w.push_str("target triple = \"nvptx64-nvidia-cuda\"\n");
    w.push_str("target datalayout = \"e-i64:64-i128:128-v16:16-v32:32-n16:32:64\"\n\n");
    // Special-register reads used by the gpu_* intrinsics.
    for sreg in ["tid.x", "ctaid.x", "ntid.x", "nctaid.x"] {
        writeln!(w, "declare i32 @llvm.nvvm.read.ptx.sreg.{sreg}()").unwrap();
    }
    w.push('\n');

    let mut errs = Vec::new();
    for k in &program.gpu_kernels {
        let mut dev = Dev { f: &k.func, param_arr: &k.param_arr, w: &mut w, t: 0, errs: &mut errs };
        dev.emit_kernel();
    }
    // nvvm.annotations: mark each kernel (belt-and-braces alongside ptx_kernel CC).
    write!(w, "!nvvm.annotations = !{{").unwrap();
    for i in 0..program.gpu_kernels.len() {
        if i > 0 {
            w.push_str(", ");
        }
        write!(w, "!{i}").unwrap();
    }
    w.push_str("}\n");
    for (i, k) in program.gpu_kernels.iter().enumerate() {
        writeln!(w, "!{i} = !{{ptr @{}, !\"kernel\", i32 1}}", k.func.name).unwrap();
    }

    if errs.is_empty() {
        Ok(Some(w))
    } else {
        Err(errs)
    }
}

/// Per-kernel device emitter state.
struct Dev<'a> {
    f: &'a Function,
    param_arr: &'a [Option<ArrKind>],
    w: &'a mut String,
    t: usize,
    errs: &'a mut Vec<String>,
}

impl Dev<'_> {
    fn fresh(&mut self) -> String {
        let n = self.t;
        self.t += 1;
        format!("%t{n}")
    }

    /// Is local `l` an array-pointer parameter?
    fn arr_kind(&self, l: usize) -> Option<ArrKind> {
        self.param_arr.get(l).copied().flatten()
    }

    /// LLVM type of a local's alloca slot value.
    fn local_ty(&self, l: usize) -> &'static str {
        if self.arr_kind(l).is_some() {
            "ptr addrspace(1)"
        } else {
            scal(self.f.locals[l])
        }
    }

    fn emit_kernel(&mut self) {
        let name = self.f.name.clone();
        if self.f.ret != Ty::Void {
            self.errs.push(format!("@gpu kernel `{name}` must return () / Void"));
        }
        let nparams = self.f.params.len();
        // Signature.
        write!(self.w, "define ptx_kernel void @{name}(").unwrap();
        for i in 0..nparams {
            if i > 0 {
                self.w.push_str(", ");
            }
            let ty = self.local_ty(i);
            write!(self.w, "{ty} %a{i}").unwrap();
        }
        self.w.push_str(") {\nentry:\n");
        // Alloca every local; store incoming params.
        for l in 0..self.f.locals.len() {
            let ty = self.local_ty(l);
            writeln!(self.w, "  %slot{l} = alloca {ty}").unwrap();
        }
        for i in 0..nparams {
            let ty = self.local_ty(i);
            writeln!(self.w, "  store {ty} %a{i}, ptr %slot{i}").unwrap();
        }
        writeln!(self.w, "  br label %bb0").unwrap();
        // Blocks.
        let blocks = self.f.blocks.clone();
        for (bi, b) in blocks.iter().enumerate() {
            writeln!(self.w, "bb{bi}:").unwrap();
            for st in &b.statements {
                self.stmt(st);
            }
            self.term(&b.terminator);
        }
        self.w.push_str("}\n\n");
    }

    /// Materialize an operand as `(llvm-value, Ty)`. Array-pointer locals get the
    /// synthetic type `Ty::Ref`.
    fn operand(&mut self, op: &Operand) -> (String, Ty) {
        match op {
            Operand::ConstI32(v) => (v.to_string(), Ty::I32),
            Operand::ConstI64(v) => (v.to_string(), Ty::I64),
            Operand::ConstF64(v) => (format!("0x{:016X}", v.to_bits()), Ty::F64),
            Operand::ConstF32(v) => (format!("0x{:016X}", (*v as f64).to_bits()), Ty::F32),
            Operand::Copy(l) => {
                let li = l.0 as usize;
                if let Some(_k) = self.arr_kind(li) {
                    let t = self.fresh();
                    writeln!(self.w, "  {t} = load ptr addrspace(1), ptr %slot{li}").unwrap();
                    (t, Ty::Ref)
                } else {
                    let ty = self.f.locals[li];
                    let t = self.fresh();
                    writeln!(self.w, "  {t} = load {}, ptr %slot{li}", scal(ty)).unwrap();
                    (t, ty)
                }
            }
            Operand::ConstStr(_) | Operand::ConstClass(_) | Operand::ConstNull => {
                self.errs.push(format!("@gpu kernel `{}`: string/class/null operands are not supported on the device", self.f.name));
                ("0".into(), Ty::I64)
            }
        }
    }

    /// Cast `val` of type `from` to type `to` (numeric only).
    fn coerce(&mut self, val: String, from: Ty, to: Ty) -> String {
        if from == to {
            return val;
        }
        let op = match (from, to) {
            (Ty::I32, Ty::I64) => "sext",
            (Ty::I64, Ty::I32) => "trunc",
            (Ty::F32, Ty::F64) => "fpext",
            (Ty::F64, Ty::F32) => "fptrunc",
            (a, b) if !is_float(a) && is_float(b) => "sitofp",
            (a, b) if is_float(a) && !is_float(b) => "fptosi",
            _ => "bitcast",
        };
        let t = self.fresh();
        writeln!(self.w, "  {t} = {op} {} {val} to {}", scal(from), scal(to)).unwrap();
        t
    }

    fn store_local(&mut self, l: usize, val: String, val_ty: Ty) {
        let dty = self.f.locals[l];
        let v = self.coerce(val, val_ty, dty);
        writeln!(self.w, "  store {} {v}, ptr %slot{l}", scal(dty)).unwrap();
    }

    fn stmt(&mut self, st: &Statement) {
        match st {
            Statement::Assign(dest, rv) => {
                let (val, ty) = self.rvalue(rv);
                self.store_local(dest.0 as usize, val, ty);
            }
            Statement::Call { dest, func, args } => {
                if let Some((val, ty)) = self.gpu_intrinsic(func, args) {
                    if let Some(d) = dest {
                        self.store_local(d.0 as usize, val, ty);
                    }
                } else {
                    self.errs.push(format!("@gpu kernel `{}`: call to `{func}` is not supported on the device (only gpu_* intrinsics and array/arithmetic ops)", self.f.name));
                }
            }
            Statement::ArrayLoad { dest, arr, index, kind, .. } => {
                let p = self.gep(arr, index, *kind);
                let et = elem(*kind);
                let v = self.fresh();
                writeln!(self.w, "  {v} = load {et}, ptr addrspace(1) {p}").unwrap();
                // Extend sub-word element to the destination value type.
                let vty = kind.value_ty();
                let loaded = self.widen_from_elem(v, *kind, vty);
                self.store_local(dest.0 as usize, loaded, vty);
            }
            Statement::ArrayStore { arr, index, value, kind, .. } => {
                let p = self.gep(arr, index, *kind);
                let (v, vt) = self.operand(value);
                let et = elem(*kind);
                let sv = self.narrow_to_elem(v, vt, *kind);
                writeln!(self.w, "  store {et} {sv}, ptr addrspace(1) {p}").unwrap();
            }
            Statement::DebugLine(_) => {}
            other => {
                self.errs.push(format!("@gpu kernel `{}`: unsupported statement on the device: {other:?}", self.f.name));
            }
        }
    }

    /// GEP into an array-pointer operand by index (index coerced to i64).
    fn gep(&mut self, arr: &Operand, index: &Operand, kind: ArrKind) -> String {
        let (base, bt) = self.operand(arr);
        if bt != Ty::Ref {
            self.errs.push(format!("@gpu kernel `{}`: array access on a non-array operand", self.f.name));
        }
        let (idx, it) = self.operand(index);
        let idx = self.coerce(idx, it, Ty::I64);
        let p = self.fresh();
        writeln!(self.w, "  {p} = getelementptr {}, ptr addrspace(1) {base}, i64 {idx}", elem(kind)).unwrap();
        p
    }

    /// Extend a freshly-loaded element to its value type (sub-word → i32).
    fn widen_from_elem(&mut self, v: String, kind: ArrKind, vty: Ty) -> String {
        let et = elem(kind);
        let vt = scal(vty);
        if et == vt {
            return v;
        }
        // Only integer sub-word widening is possible here.
        let op = match kind {
            ArrKind::Bool | ArrKind::Char => "zext",
            ArrKind::Byte | ArrKind::Short => "sext",
            _ => "bitcast",
        };
        let t = self.fresh();
        writeln!(self.w, "  {t} = {op} {et} {v} to {vt}").unwrap();
        t
    }

    /// Narrow a value to the array element storage type before a store.
    fn narrow_to_elem(&mut self, v: String, vt: Ty, kind: ArrKind) -> String {
        let et = elem(kind);
        let vs = scal(vt);
        if et == vs {
            return v;
        }
        let op = match kind {
            ArrKind::Bool | ArrKind::Byte | ArrKind::Char | ArrKind::Short => "trunc",
            ArrKind::Int if vt == Ty::I64 => "trunc",
            ArrKind::Long if vt == Ty::I32 => "sext",
            ArrKind::Float if vt == Ty::F64 => "fptrunc",
            ArrKind::Double if vt == Ty::F32 => "fpext",
            _ => "bitcast",
        };
        let t = self.fresh();
        writeln!(self.w, "  {t} = {op} {vs} {v} to {et}").unwrap();
        t
    }

    /// Lower a `gpu_*` intrinsic call to nvvm sreg reads. Returns `(value, i64)`.
    fn gpu_intrinsic(&mut self, func: &str, _args: &[Operand]) -> Option<(String, Ty)> {
        let read = |dev: &mut Self, sreg: &str| -> String {
            let t = dev.fresh();
            writeln!(dev.w, "  {t} = call i32 @llvm.nvvm.read.ptx.sreg.{sreg}()").unwrap();
            t
        };
        let sext = |dev: &mut Self, v: String| -> String {
            let t = dev.fresh();
            writeln!(dev.w, "  {t} = sext i32 {v} to i64").unwrap();
            t
        };
        let val32 = match func {
            "__gpu_tid" => read(self, "tid.x"),
            "__gpu_bid" => read(self, "ctaid.x"),
            "__gpu_bdim" => read(self, "ntid.x"),
            "__gpu_gdim" => read(self, "nctaid.x"),
            "__gpu_gid" => {
                let cta = read(self, "ctaid.x");
                let ntid = read(self, "ntid.x");
                let tid = read(self, "tid.x");
                let m = self.fresh();
                writeln!(self.w, "  {m} = mul i32 {cta}, {ntid}").unwrap();
                let g = self.fresh();
                writeln!(self.w, "  {g} = add i32 {m}, {tid}").unwrap();
                g
            }
            "__gpu_gsize" => {
                let nctaid = read(self, "nctaid.x");
                let ntid = read(self, "ntid.x");
                let m = self.fresh();
                writeln!(self.w, "  {m} = mul i32 {nctaid}, {ntid}").unwrap();
                m
            }
            _ => return None,
        };
        let v64 = sext(self, val32);
        Some((v64, Ty::I64))
    }

    fn rvalue(&mut self, rv: &Rvalue) -> (String, Ty) {
        match rv {
            Rvalue::Use(op) => self.operand(op),
            Rvalue::Neg(op) => {
                let (v, t) = self.operand(op);
                let r = self.fresh();
                if is_float(t) {
                    writeln!(self.w, "  {r} = fneg {} {v}", scal(t)).unwrap();
                } else {
                    writeln!(self.w, "  {r} = sub {} 0, {v}", scal(t)).unwrap();
                }
                (r, t)
            }
            Rvalue::Convert(op) => {
                // Source = operand type, target decided by the destination local at
                // the Assign; here we just pass through the operand and let
                // store_local coerce. We return the operand as-is.
                self.operand(op)
            }
            Rvalue::Binary(bop, a, b) => self.binary(*bop, a, b),
        }
    }

    fn binary(&mut self, bop: BinOp, a: &Operand, b: &Operand) -> (String, Ty) {
        let (va, ta) = self.operand(a);
        let (vb, tb) = self.operand(b);
        // Operation type: float if either float, else the wider integer.
        let opty = if is_float(ta) || is_float(tb) {
            if ta == Ty::F64 || tb == Ty::F64 { Ty::F64 } else { Ty::F32 }
        } else if ta == Ty::I64 || tb == Ty::I64 {
            Ty::I64
        } else {
            Ty::I32
        };
        let va = self.coerce(va, ta, opty);
        let vb = self.coerce(vb, tb, opty);
        let flt = is_float(opty);
        let tys = scal(opty);
        // Comparisons yield i1 → zext to i32 (Vire Bool).
        let cmp = |dev: &mut Self, pred_i: &str, pred_f: &str| -> (String, Ty) {
            let c = dev.fresh();
            if flt {
                writeln!(dev.w, "  {c} = fcmp {pred_f} {tys} {va}, {vb}").unwrap();
            } else {
                writeln!(dev.w, "  {c} = icmp {pred_i} {tys} {va}, {vb}").unwrap();
            }
            let z = dev.fresh();
            writeln!(dev.w, "  {z} = zext i1 {c} to i32").unwrap();
            (z, Ty::I32)
        };
        let arith = |dev: &mut Self, iop: &str, fop: &str| -> (String, Ty) {
            let r = dev.fresh();
            let o = if flt { fop } else { iop };
            writeln!(dev.w, "  {r} = {o} {tys} {va}, {vb}").unwrap();
            (r, opty)
        };
        match bop {
            BinOp::Add => arith(self, "add", "fadd"),
            BinOp::Sub => arith(self, "sub", "fsub"),
            BinOp::Mul => arith(self, "mul", "fmul"),
            BinOp::Div => arith(self, "sdiv", "fdiv"),
            BinOp::Rem => arith(self, "srem", "frem"),
            BinOp::Shl => arith(self, "shl", "shl"),
            BinOp::Shr => arith(self, "ashr", "ashr"),
            BinOp::UShr => arith(self, "lshr", "lshr"),
            BinOp::And => arith(self, "and", "and"),
            BinOp::Or => arith(self, "or", "or"),
            BinOp::Xor => arith(self, "xor", "xor"),
            BinOp::CmpEq => cmp(self, "eq", "oeq"),
            BinOp::CmpNe => cmp(self, "ne", "one"),
            BinOp::CmpLt => cmp(self, "slt", "olt"),
            BinOp::CmpGe => cmp(self, "sge", "oge"),
            BinOp::CmpGt => cmp(self, "sgt", "ogt"),
            BinOp::CmpLe => cmp(self, "sle", "ole"),
        }
    }

    fn term(&mut self, t: &Terminator) {
        match t {
            Terminator::Goto(b) => {
                writeln!(self.w, "  br label %bb{}", b.0).unwrap();
            }
            Terminator::Branch { cond, then_blk, else_blk } => {
                let (c, ct) = self.operand(cond);
                let b = self.fresh();
                writeln!(self.w, "  {b} = icmp ne {} {c}, 0", scal(ct)).unwrap();
                writeln!(self.w, "  br i1 {b}, label %bb{}, label %bb{}", then_blk.0, else_blk.0).unwrap();
            }
            Terminator::Switch { value, default, cases } => {
                let (v, vt) = self.operand(value);
                let v = self.coerce(v, vt, Ty::I32);
                write!(self.w, "  switch i32 {v}, label %bb{} [", default.0).unwrap();
                for (val, blk) in cases {
                    write!(self.w, " i32 {val}, label %bb{}", blk.0).unwrap();
                }
                self.w.push_str(" ]\n");
            }
            Terminator::Return(None) => {
                self.w.push_str("  ret void\n");
            }
            Terminator::Return(Some(_)) => {
                self.errs.push(format!("@gpu kernel `{}` must not return a value", self.f.name));
                self.w.push_str("  ret void\n");
            }
        }
    }
}

// ===================== C host launch stubs =====================

/// C type of a host stub parameter.
fn c_param_ty(ty: Ty, is_arr: bool) -> &'static str {
    if is_arr {
        return "void*";
    }
    match ty {
        Ty::I32 => "int32_t",
        Ty::I64 => "int64_t",
        Ty::F32 => "float",
        Ty::F64 => "double",
        Ty::Ref => "void*",
        Ty::Void => "void",
    }
}

/// Generates `gpu_stubs.c`: the per-kernel host launch stubs + `jrt_gpu_*`
/// forward declarations. Returns `None` if there are no kernels.
pub fn emit_gpu_stubs(program: &Program) -> Option<String> {
    if program.gpu_kernels.is_empty() {
        return None;
    }
    let mut w = String::new();
    w.push_str("/* Generated host launch stubs for Vire @gpu kernels. */\n");
    w.push_str("#include <stdint.h>\n\n");
    // jrt_gpu_* runtime ABI (defined in gpu_runtime.c) — kept cuda.h-free here.
    w.push_str("void  jrt_gpu_ensure(void);\n");
    w.push_str("void* jrt_gpu_func(const char* name);\n");
    w.push_str("void* jrt_gpu_upload(void* host, int64_t bytes);\n");
    w.push_str("void  jrt_gpu_download(void* host, void* dev, int64_t bytes);\n");
    w.push_str("void  jrt_gpu_free(void* dev);\n");
    w.push_str("void  jrt_gpu_launch(void* fn, int64_t n_threads, void** params, int nparams);\n");
    w.push_str("int64_t jrt_gpu_arrlen(void* arr);\n\n");

    for k in &program.gpu_kernels {
        let name = &k.func.name;
        let n = k.func.params.len();
        // Signature.
        write!(w, "void {name}(").unwrap();
        if n == 0 {
            w.push_str("void");
        }
        for i in 0..n {
            if i > 0 {
                w.push_str(", ");
            }
            let is_arr = k.param_arr.get(i).copied().flatten().is_some();
            write!(w, "{} p{i}", c_param_ty(k.func.params[i], is_arr)).unwrap();
        }
        w.push_str(") {\n");
        w.push_str("  jrt_gpu_ensure();\n");
        writeln!(w, "  void* _fn = jrt_gpu_func(\"{name}\");").unwrap();
        // Launch thread count.
        writeln!(w, "  int64_t _n = (int64_t)p{};", k.launch_param).unwrap();
        // Upload array arguments (H2D). `data = arr+32`, `bytes = len*elemsize`.
        for i in 0..n {
            if let Some(kind) = k.param_arr.get(i).copied().flatten() {
                let sz = kind.size();
                writeln!(w, "  int64_t _b{i} = jrt_gpu_arrlen(p{i}) * {sz};").unwrap();
                writeln!(w, "  void* _d{i} = jrt_gpu_upload((char*)p{i} + 32, _b{i});").unwrap();
            }
        }
        // Kernel parameter array (addresses of scalars / device pointers).
        writeln!(w, "  void* _params[{}];", n.max(1)).unwrap();
        for i in 0..n {
            if k.param_arr.get(i).copied().flatten().is_some() {
                writeln!(w, "  _params[{i}] = &_d{i};").unwrap();
            } else {
                writeln!(w, "  _params[{i}] = &p{i};").unwrap();
            }
        }
        writeln!(w, "  jrt_gpu_launch(_fn, _n, _params, {n});").unwrap();
        // Copy back all array arguments (D2H) and free. v1: every array is treated
        // as in/out (simple + correct); a future read-only analysis can skip these.
        for i in 0..n {
            if k.param_arr.get(i).copied().flatten().is_some() {
                writeln!(w, "  jrt_gpu_download((char*)p{i} + 32, _d{i}, _b{i});").unwrap();
                writeln!(w, "  jrt_gpu_free(_d{i});").unwrap();
            }
        }
        w.push_str("}\n\n");
    }
    Some(w)
}
