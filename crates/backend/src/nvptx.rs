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
//! The design mirrors NVlabs/cuda-oxide (Apache-2.0, see crates/cuda-oxide/NOTICE.md):
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
        ArrKind::Bool | ArrKind::Byte | ArrKind::U8 => "i8",
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
    // G1 device primitives: block barrier, warp shuffle, IEEE math. (Atomics use
    // the `atomicrmw` instruction, no declaration needed.)
    w.push_str("declare void @llvm.nvvm.barrier0()\n");
    w.push_str("declare i32 @llvm.nvvm.shfl.sync.down.i32(i32, i32, i32, i32)\n");
    for f in ["sqrt", "fabs", "floor", "ceil"] {
        writeln!(w, "declare double @llvm.{f}.f64(double)").unwrap();
    }
    w.push_str("declare double @llvm.minnum.f64(double, double)\n");
    w.push_str("declare double @llvm.maxnum.f64(double, double)\n");
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
        // Signature: parameter 0 is the injected global thread index — NOT a PTX
        // parameter — so the device signature is params 1.. (matching the host
        // launch stub, which passes only those).
        write!(self.w, "define ptx_kernel void @{name}(").unwrap();
        let mut first = true;
        for i in 1..nparams {
            if !first {
                self.w.push_str(", ");
            }
            first = false;
            let ty = self.local_ty(i);
            write!(self.w, "{ty} %a{i}").unwrap();
        }
        self.w.push_str(") {\nentry:\n");
        // Alloca every local.
        for l in 0..self.f.locals.len() {
            let ty = self.local_ty(l);
            writeln!(self.w, "  %slot{l} = alloca {ty}").unwrap();
        }
        // slot0 = global thread index (blockIdx.x*blockDim.x + threadIdx.x).
        if nparams >= 1 {
            let cta = self.fresh();
            writeln!(self.w, "  {cta} = call i32 @llvm.nvvm.read.ptx.sreg.ctaid.x()").unwrap();
            let ntid = self.fresh();
            writeln!(self.w, "  {ntid} = call i32 @llvm.nvvm.read.ptx.sreg.ntid.x()").unwrap();
            let tid = self.fresh();
            writeln!(self.w, "  {tid} = call i32 @llvm.nvvm.read.ptx.sreg.tid.x()").unwrap();
            let m = self.fresh();
            writeln!(self.w, "  {m} = mul i32 {cta}, {ntid}").unwrap();
            let g = self.fresh();
            writeln!(self.w, "  {g} = add i32 {m}, {tid}").unwrap();
            if self.f.locals[0] == Ty::I64 {
                let g64 = self.fresh();
                writeln!(self.w, "  {g64} = sext i32 {g} to i64").unwrap();
                writeln!(self.w, "  store i64 {g64}, ptr %slot0").unwrap();
            } else {
                writeln!(self.w, "  store i32 {g}, ptr %slot0").unwrap();
            }
        }
        // Store the real (caller-provided) params 1.. into their slots.
        for i in 1..nparams {
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

    /// Lower a `gpu_*` device intrinsic call. Returns `(value, ty)`, or `None` if
    /// `func` is not a device intrinsic. Covers the nullary special-register reads
    /// and the G1 primitives (barrier, atomics, warp shuffle/reduce, IEEE math).
    fn gpu_intrinsic(&mut self, func: &str, args: &[Operand]) -> Option<(String, Ty)> {
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
        // Nullary special-register reads (thread/block indices + dimensions).
        let sreg = match func {
            "__gpu_tid" => Some(read(self, "tid.x")),
            "__gpu_bid" => Some(read(self, "ctaid.x")),
            "__gpu_bdim" => Some(read(self, "ntid.x")),
            "__gpu_gdim" => Some(read(self, "nctaid.x")),
            "__gpu_gid" => {
                let cta = read(self, "ctaid.x");
                let ntid = read(self, "ntid.x");
                let tid = read(self, "tid.x");
                let m = self.fresh();
                writeln!(self.w, "  {m} = mul i32 {cta}, {ntid}").unwrap();
                let g = self.fresh();
                writeln!(self.w, "  {g} = add i32 {m}, {tid}").unwrap();
                Some(g)
            }
            "__gpu_gsize" => {
                let nctaid = read(self, "nctaid.x");
                let ntid = read(self, "ntid.x");
                let m = self.fresh();
                writeln!(self.w, "  {m} = mul i32 {nctaid}, {ntid}").unwrap();
                Some(m)
            }
            _ => None,
        };
        if let Some(v32) = sreg {
            return Some((sext(self, v32), Ty::I64));
        }

        // A full-warp shuffle-down by `off` (all 32 lanes, warp-width clamp c=31).
        let shfl_down = |dev: &mut Self, v: &str, off: &str| -> String {
            let r = dev.fresh();
            writeln!(dev.w, "  {r} = call i32 @llvm.nvvm.shfl.sync.down.i32(i32 -1, i32 {v}, i32 {off}, i32 31)").unwrap();
            r
        };
        match func {
            // __syncthreads block barrier. Called for effect; value is unused.
            "__gpu_sync" => {
                writeln!(self.w, "  call void @llvm.nvvm.barrier0()").unwrap();
                Some(("0".into(), Ty::I64))
            }
            // atomicAdd(arr, idx, val) → old value. Global-memory i32/i64 only.
            "__gpu_atomic_add" => {
                let kind = match &args[0] {
                    Operand::Copy(l) => self.arr_kind(l.0 as usize),
                    _ => None,
                };
                let kind = match kind {
                    Some(k @ (ArrKind::Int | ArrKind::Long)) => k,
                    _ => {
                        self.errs.push(format!(
                            "@gpu kernel `{}`: gpu_atomic_add needs an Int/Long array argument",
                            self.f.name
                        ));
                        ArrKind::Int
                    }
                };
                let et = elem(kind);
                let (base, _) = self.operand(&args[0]);
                let (idx, it) = self.operand(&args[1]);
                let idx = self.coerce(idx, it, Ty::I64);
                let p = self.fresh();
                writeln!(self.w, "  {p} = getelementptr {et}, ptr addrspace(1) {base}, i64 {idx}").unwrap();
                let (v, vt) = self.operand(&args[2]);
                let sv = self.narrow_to_elem(v, vt, kind);
                let old = self.fresh();
                writeln!(self.w, "  {old} = atomicrmw add ptr addrspace(1) {p}, {et} {sv} monotonic").unwrap();
                let old64 = if kind == ArrKind::Long { old } else { sext(self, old) };
                Some((old64, Ty::I64))
            }
            // Warp shuffle-down (single step) and a full-warp sum reduction.
            "__gpu_shfl_down" => {
                let (v, vt) = self.operand(&args[0]);
                let v = self.coerce(v, vt, Ty::I32);
                let (d, dt) = self.operand(&args[1]);
                let d = self.coerce(d, dt, Ty::I32);
                let r = shfl_down(self, &v, &d);
                Some((sext(self, r), Ty::I64))
            }
            "__gpu_warp_reduce_add" => {
                let (v, vt) = self.operand(&args[0]);
                let mut acc = self.coerce(v, vt, Ty::I32);
                for off in ["16", "8", "4", "2", "1"] {
                    let s = shfl_down(self, &acc, off);
                    let n = self.fresh();
                    writeln!(self.w, "  {n} = add i32 {acc}, {s}").unwrap();
                    acc = n;
                }
                Some((sext(self, acc), Ty::I64))
            }
            // IEEE math (round-to-nearest → bit-exact vs the CPU runtime).
            "__gpu_sqrt" | "__gpu_fabs" | "__gpu_floor" | "__gpu_ceil" => {
                let ll = &func["__gpu_".len()..];
                let (x, xt) = self.operand(&args[0]);
                let x = self.coerce(x, xt, Ty::F64);
                let r = self.fresh();
                writeln!(self.w, "  {r} = call double @llvm.{ll}.f64(double {x})").unwrap();
                Some((r, Ty::F64))
            }
            "__gpu_fmin" | "__gpu_fmax" => {
                let nv = if func.ends_with("fmin") { "minnum" } else { "maxnum" };
                let (a, at) = self.operand(&args[0]);
                let a = self.coerce(a, at, Ty::F64);
                let (b, bt) = self.operand(&args[1]);
                let b = self.coerce(b, bt, Ty::F64);
                let r = self.fresh();
                writeln!(self.w, "  {r} = call double @llvm.{nv}.f64(double {a}, double {b})").unwrap();
                Some((r, Ty::F64))
            }
            _ => None,
        }
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

/// Which array parameters the kernel only READS (never `ArrayStore`s into) and
/// so need no D2H copyback. Design adapted from cuda-oxide's typed in/out
/// `DeviceBuffer` distinction (idea, not code): an input-only buffer stays on the
/// host untouched, so downloading it back is pure waste.
///
/// Sound-conservative by construction: skipping a *needed* copyback would silently
/// drop a result, so the analysis only marks a param read-only when it can PROVE
/// no store reaches it. Array pointers on the device originate solely from
/// parameters (there is no device allocation), so every `ArrayStore` base traces
/// to a param via copy-aliasing; any base we cannot trace forces every array to
/// in/out (returns all-false). Returns a `bool` per parameter index (scalars →
/// false, irrelevant).
fn read_only_params(k: &GpuKernel) -> Vec<bool> {
    let nparams = k.func.params.len();
    let nlocals = k.func.locals.len();
    // Bitset of param indices a local may hold; kernels are tiny, but guard >64.
    if nparams > 64 {
        return vec![false; nparams];
    }
    let mut points = vec![0u64; nlocals];
    // Seed: locals 0..nparams ARE the parameters (local 0 = injected thread index);
    // each array param initially points to itself.
    for i in 0..nparams {
        if k.param_arr.get(i).copied().flatten().is_some() {
            points[i] = 1u64 << i;
        }
    }
    // Fixpoint over copy-aliasing (`dest = src`): dest may hold whatever src does.
    let mut changed = true;
    while changed {
        changed = false;
        for b in &k.func.blocks {
            for st in &b.statements {
                if let Statement::Assign(dest, Rvalue::Use(Operand::Copy(src))) = st {
                    let s = points[src.0 as usize];
                    let d = &mut points[dest.0 as usize];
                    if *d | s != *d {
                        *d |= s;
                        changed = true;
                    }
                }
            }
        }
    }
    // Collect which params can be written; bail (all in/out) on an untraceable base.
    // A store's base is written; and — soundly — any array passed to a Call may be
    // mutated by it (device atomics like `gpu_atomic_add` write through the arg,
    // and we cannot see inside an intrinsic), so an array-typed call argument counts
    // as written too. The pure intrinsics (math/warp/sreg) take no array args, so
    // this costs them nothing.
    let mut written = 0u64;
    for b in &k.func.blocks {
        for st in &b.statements {
            match st {
                Statement::ArrayStore { arr, .. } => match arr {
                    Operand::Copy(l) => {
                        let p = points[l.0 as usize];
                        if p == 0 {
                            return vec![false; nparams]; // base not traced to a param
                        }
                        written |= p;
                    }
                    _ => return vec![false; nparams], // non-local array base
                },
                Statement::Call { args, .. } => {
                    for a in args {
                        if let Operand::Copy(l) = a {
                            written |= points[l.0 as usize]; // array arg (0 if scalar)
                        }
                    }
                }
                _ => {}
            }
        }
    }
    (0..nparams)
        .map(|i| {
            k.param_arr.get(i).copied().flatten().is_some() && (written & (1u64 << i)) == 0
        })
        .collect()
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
        let read_only = read_only_params(k);
        // Parameter 0 is the injected thread index — supplied on the device, NOT by
        // the host — so the stub signature and the launch marshal params 1.. only.
        write!(w, "void {name}(").unwrap();
        if n <= 1 {
            w.push_str("void");
        }
        let mut first = true;
        for i in 1..n {
            if !first {
                w.push_str(", ");
            }
            first = false;
            let is_arr = k.param_arr.get(i).copied().flatten().is_some();
            write!(w, "{} p{i}", c_param_ty(k.func.params[i], is_arr)).unwrap();
        }
        w.push_str(") {\n");
        w.push_str("  jrt_gpu_ensure();\n");
        writeln!(w, "  void* _fn = jrt_gpu_func(\"{name}\");").unwrap();
        // Launch thread count = the first caller-provided int param (index >= 1).
        writeln!(w, "  int64_t _n = (int64_t)p{};", k.launch_param).unwrap();
        // Upload array arguments (H2D). `data = arr+32`, `bytes = len*elemsize`.
        for i in 1..n {
            if let Some(kind) = k.param_arr.get(i).copied().flatten() {
                let sz = kind.size();
                writeln!(w, "  int64_t _b{i} = jrt_gpu_arrlen(p{i}) * {sz};").unwrap();
                writeln!(w, "  void* _d{i} = jrt_gpu_upload((char*)p{i} + 32, _b{i});").unwrap();
            }
        }
        // Kernel parameter array (addresses of scalars / device pointers), in the
        // order of the device signature (params 1..).
        let nkp = n.saturating_sub(1);
        writeln!(w, "  void* _params[{}];", nkp.max(1)).unwrap();
        let mut slot = 0;
        for i in 1..n {
            if k.param_arr.get(i).copied().flatten().is_some() {
                writeln!(w, "  _params[{slot}] = &_d{i};").unwrap();
            } else {
                writeln!(w, "  _params[{slot}] = &p{i};").unwrap();
            }
            slot += 1;
        }
        writeln!(w, "  jrt_gpu_launch(_fn, _n, _params, {nkp});").unwrap();
        // Copy back written arrays (D2H) and free. A read-only array (the kernel
        // never stores into it — see `read_only_params`) skips the copyback: the
        // host data is unchanged, so downloading it back is pure waste.
        for i in 1..n {
            if k.param_arr.get(i).copied().flatten().is_some() {
                if read_only[i] {
                    writeln!(w, "  /* p{i} is read-only on the device — D2H skipped */").unwrap();
                } else {
                    writeln!(w, "  jrt_gpu_download((char*)p{i} + 32, _d{i}, _b{i});").unwrap();
                }
                writeln!(w, "  jrt_gpu_free(_d{i});").unwrap();
            }
        }
        w.push_str("}\n\n");
    }
    Some(w)
}
