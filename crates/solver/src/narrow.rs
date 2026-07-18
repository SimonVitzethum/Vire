//! Field auto-narrowing via value-range analysis.
//!
//! An `Int`(i64) field whose stored values ALL PROVABLY fit in the
//! i32 range is narrowed to i32 (4 instead of 8 bytes → RAM). Sound: on
//! any uncertainty ⊤ (do not narrow); widening guarantees termination and
//! over-approximates growing recurrences (e.g. an accumulator/counter → ⊤ →
//! stays i64). The rewrite inserts `Convert` at narrowed field accesses
//! (read: i32→i64 sext; write: i64→i32 trunc, lossless since provably fitting).

use std::collections::HashMap;

use fastllvm_ir::{BasicBlock, Local, Operand, Program, Rvalue, Statement, Ty};

const NEG_INF: i64 = i64::MIN;
const POS_INF: i64 = i64::MAX;

#[derive(Clone, Copy, PartialEq)]
struct Range {
    lo: i64,
    hi: i64,
    bot: bool, // ⊥ (no value yet)
}
impl Range {
    fn bottom() -> Range {
        Range { lo: 0, hi: 0, bot: true }
    }
    fn top() -> Range {
        Range { lo: NEG_INF, hi: POS_INF, bot: false }
    }
    fn point(v: i64) -> Range {
        Range { lo: v, hi: v, bot: false }
    }
    fn is_top(&self) -> bool {
        !self.bot && (self.lo == NEG_INF || self.hi == POS_INF)
    }
    /// Plain join (union, NO widening): lo=min, hi=max.
    fn join(&self, other: Range) -> Range {
        if other.bot {
            return *self;
        }
        if self.bot {
            return other;
        }
        Range { lo: self.lo.min(other.lo), hi: self.hi.max(other.hi), bot: false }
    }
    /// Widening from `self` (old) to `new`: set every bound that has grown
    /// to ±∞ (termination; sound). Apply only after sustained growth
    /// (delayed), otherwise pure propagation would wrongly widen to ⊤.
    fn widen_to(&self, new: Range) -> Range {
        if self.bot {
            return new;
        }
        if new.bot {
            return *self;
        }
        let lo = if new.lo < self.lo { NEG_INF } else { new.lo };
        let hi = if new.hi > self.hi { POS_INF } else { new.hi };
        Range { lo, hi, bot: false }
    }
    fn fits_i32(&self) -> bool {
        !self.bot && self.lo >= i32::MIN as i64 && self.hi <= i32::MAX as i64
    }
}

fn add(a: Range, b: Range) -> Range {
    if a.bot || b.bot {
        return Range::bottom();
    }
    if a.is_top() || b.is_top() {
        return Range::top();
    }
    match (a.lo.checked_add(b.lo), a.hi.checked_add(b.hi)) {
        (Some(lo), Some(hi)) => Range { lo, hi, bot: false },
        _ => Range::top(),
    }
}
fn sub(a: Range, b: Range) -> Range {
    if a.bot || b.bot {
        return Range::bottom();
    }
    if a.is_top() || b.is_top() {
        return Range::top();
    }
    match (a.lo.checked_sub(b.hi), a.hi.checked_sub(b.lo)) {
        (Some(lo), Some(hi)) => Range { lo, hi, bot: false },
        _ => Range::top(),
    }
}
fn mul(a: Range, b: Range) -> Range {
    if a.bot || b.bot {
        return Range::bottom();
    }
    if a.is_top() || b.is_top() {
        return Range::top();
    }
    let prods = [a.lo.checked_mul(b.lo), a.lo.checked_mul(b.hi), a.hi.checked_mul(b.lo), a.hi.checked_mul(b.hi)];
    if prods.iter().any(|p| p.is_none()) {
        return Range::top();
    }
    let vs: Vec<i64> = prods.iter().map(|p| p.unwrap()).collect();
    Range { lo: *vs.iter().min().unwrap(), hi: *vs.iter().max().unwrap(), bot: false }
}
/// Division precise only with a constant positive divisor, otherwise ⊤.
fn div(a: Range, b: Range) -> Range {
    if a.bot || b.bot {
        return Range::bottom();
    }
    if a.is_top() || b.lo != b.hi || b.lo <= 0 {
        return Range::top();
    }
    let d = b.lo;
    Range { lo: a.lo / d, hi: a.hi / d, bot: false }
}
/// Remainder with a constant divisor: |result| < |d|.
fn rem(_a: Range, b: Range) -> Range {
    if b.bot || b.lo != b.hi || b.lo == 0 {
        return Range::top();
    }
    let m = b.lo.saturating_abs() - 1;
    Range { lo: -m, hi: m, bot: false }
}

/// Is the local type an integer (relevant to narrowing)?
fn is_int(ty: Ty) -> bool {
    matches!(ty, Ty::I32 | Ty::I64)
}

/// Narrows provably-i32-fitting `Int` fields to i32 + rewrites the accesses.
/// Return: number of narrowed fields.
pub fn narrow_fields(program: &mut Program) -> usize {
    // Fixpoint: field ranges + local ranges (not flow-sensitive: join of all defs).
    let mut field_r: HashMap<(String, String), Range> = HashMap::new();
    let mut local_r: HashMap<(usize, u32), Range> = HashMap::new();

    let eval_op = |op: &Operand, fi: usize, local_r: &HashMap<(usize, u32), Range>| -> Range {
        match op {
            Operand::ConstI64(v) => Range::point(*v),
            Operand::ConstI32(v) => Range::point(*v as i64),
            Operand::Copy(l) => *local_r.get(&(fi, l.0)).unwrap_or(&Range::bottom()),
            _ => Range::top(),
        }
    };

    // Delayed widening: a bound that keeps growing over MORE than K iterations
    // is a true recurrence (counter/accumulator) → widen to ±∞.
    // Pure propagation (finite chains) stabilizes before that → no widening,
    // full precision. K must outlast the longest acyclic chain.
    const K: u32 = 12;
    let mut local_age: HashMap<(usize, u32), u32> = HashMap::new();
    let mut field_age: HashMap<(String, String), u32> = HashMap::new();
    for _pass in 0..(K + 8) {
        // Phase 1: new ranges via PLAIN JOIN, read from the OLD maps (Jacobi).
        let mut nl: HashMap<(usize, u32), Range> = HashMap::new();
        let mut nf: HashMap<(String, String), Range> = HashMap::new();
        let mut jl = |key: (usize, u32), r: Range, nl: &mut HashMap<(usize, u32), Range>| {
            let e = nl.entry(key).or_insert(Range::bottom());
            *e = e.join(r);
        };
        for (fi, f) in program.functions.iter().enumerate() {
            for bb in &f.blocks {
                for st in &bb.statements {
                    match st {
                        Statement::Assign(d, rv) if is_int(f.locals[d.0 as usize]) => {
                            let r = match rv {
                                Rvalue::Use(op) => eval_op(op, fi, &local_r),
                                Rvalue::Neg(op) => sub(Range::point(0), eval_op(op, fi, &local_r)),
                                Rvalue::Convert(op) => eval_op(op, fi, &local_r),
                                Rvalue::Binary(bop, a, b) => {
                                    use fastllvm_ir::BinOp::*;
                                    let (ra, rb) = (eval_op(a, fi, &local_r), eval_op(b, fi, &local_r));
                                    match bop {
                                        Add => add(ra, rb),
                                        Sub => sub(ra, rb),
                                        Mul => mul(ra, rb),
                                        Div => div(ra, rb),
                                        Rem => rem(ra, rb),
                                        CmpEq | CmpNe | CmpLt | CmpGe | CmpGt | CmpLe => Range { lo: 0, hi: 1, bot: false },
                                        _ => Range::top(),
                                    }
                                }
                            };
                            jl((fi, d.0), r, &mut nl);
                        }
                        // NO field feedback: a GetField yields ⊤. This way no
                        // field range can (transitively) depend on itself → growing
                        // recurrences (e.g. an accumulating field) become ⊤ and are NEVER
                        // wrongly narrowed. Sound; still narrows constants/`i%256`
                        // etc. (values that do NOT stem from the field itself).
                        Statement::GetField { dest, .. } if is_int(f.locals[dest.0 as usize]) => {
                            jl((fi, dest.0), Range::top(), &mut nl);
                        }
                        Statement::PutField { class, field, value, .. } => {
                            let r = eval_op(value, fi, &local_r);
                            let e = nf.entry((class.clone(), field.clone())).or_insert(Range::bottom());
                            *e = e.join(r);
                        }
                        Statement::Call { dest: Some(d), func, args } if is_int(f.locals[d.0 as usize]) => {
                            let r = match func.as_str() {
                                "jrt_lrem" | "jrt_irem" if args.len() == 2 => rem(eval_op(&args[0], fi, &local_r), eval_op(&args[1], fi, &local_r)),
                                "jrt_ldiv" | "jrt_idiv" if args.len() == 2 => div(eval_op(&args[0], fi, &local_r), eval_op(&args[1], fi, &local_r)),
                                "jrt_lcmp" | "jrt_dcmpl" | "jrt_dcmpg" | "jrt_fcmpl" | "jrt_fcmpg" => Range { lo: -1, hi: 1, bot: false },
                                _ => Range::top(),
                            };
                            jl((fi, d.0), r, &mut nl);
                        }
                        Statement::Call { dest: Some(d), .. }
                        | Statement::CallGuarded { dest: Some(d), .. }
                        | Statement::CallVirtual { dest: Some(d), .. }
                        | Statement::CallPoly { dest: Some(d), .. }
                        | Statement::ArrayLoad { dest: d, .. } => {
                            if is_int(f.locals[d.0 as usize]) {
                                jl((fi, d.0), Range::top(), &mut nl);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        // Phase 2: merge into the old maps with delayed widening.
        let mut changed = false;
        for (key, new) in nl {
            let old = *local_r.get(&key).unwrap_or(&Range::bottom());
            if new != old {
                let age = local_age.entry(key).or_insert(0);
                let merged = if *age >= K { old.widen_to(new) } else { new };
                *age += 1;
                if merged != old {
                    local_r.insert(key, merged);
                    changed = true;
                }
            }
        }
        for (key, new) in nf {
            let old = *field_r.get(&key).unwrap_or(&Range::bottom());
            if new != old {
                let age = field_age.entry(key.clone()).or_insert(0);
                let merged = if *age >= K { old.widen_to(new) } else { new };
                *age += 1;
                if merged != old {
                    field_r.insert(key, merged);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Determine narrowable fields: only declared I64 `Int` fields whose range
    // provably fits in i32 (and is not ⊥ = is actually written).
    let mut narrow: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for c in &program.classes {
        for fld in &c.fields {
            if fld.ty == Ty::I64 {
                if let Some(r) = field_r.get(&(c.name.clone(), fld.name.clone())) {
                    if !r.bot && r.fits_i32() {
                        narrow.insert((c.name.clone(), fld.name.clone()));
                    }
                }
            }
        }
    }
    if narrow.is_empty() {
        return 0;
    }

    // Set field types to i32.
    for c in &mut program.classes {
        for fld in &mut c.fields {
            if narrow.contains(&(c.name.clone(), fld.name.clone())) {
                fld.ty = Ty::I32;
            }
        }
    }

    // Rewrite accesses: read i32→i64 (sext via Convert into a fresh i64
    // local, then Use), write i64→i32 (trunc via Convert into a fresh i32 local).
    for f in &mut program.functions {
        for bi in 0..f.blocks.len() {
            let mut out: Vec<Statement> = Vec::new();
            let stmts = std::mem::take(&mut f.blocks[bi].statements);
            for st in stmts {
                match st {
                    Statement::GetField { dest, obj, class, field } if narrow.contains(&(class.clone(), field.clone())) => {
                        // Field is now i32 → load into an i32 temp, then sext into dest.
                        f.locals.push(Ty::I32);
                        let tmp = Local((f.locals.len() - 1) as u32);
                        out.push(Statement::GetField { dest: tmp, obj, class, field });
                        out.push(Statement::Assign(dest, Rvalue::Convert(Operand::Copy(tmp))));
                    }
                    Statement::PutField { obj, class, field, value } if narrow.contains(&(class.clone(), field.clone())) => {
                        match value {
                            // Constant provably fits → directly as i32.
                            Operand::ConstI64(v) => {
                                out.push(Statement::PutField { obj, class, field, value: Operand::ConstI32(v as i32) });
                            }
                            other => {
                                f.locals.push(Ty::I32);
                                let tmp = Local((f.locals.len() - 1) as u32);
                                out.push(Statement::Assign(tmp, Rvalue::Convert(other)));
                                out.push(Statement::PutField { obj, class, field, value: Operand::Copy(tmp) });
                            }
                        }
                    }
                    other => out.push(other),
                }
            }
            f.blocks[bi] = BasicBlock { statements: out, terminator: f.blocks[bi].terminator.clone() };
        }
    }
    narrow.len()
}
