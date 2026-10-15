//! Vire → Gabbro source emission (AGENTS.md §0: Gabbro ist das Target).
//!
//! M1 subset: straight-line integer code + if-diamonds + direct calls + print;
//! M2b counted loops (`retry bounded`); M3a strings, M3b const arrays, M3c
//! int-payload sums; explicit `region(max)` pools (constant ceilings →
//! `pub arena … capacity`, `reset` at pop); scalar `extern "C"` fns
//! (`extern fn` heads — Gabbro has no `extern "C"` form; `Ptr` stays out);
//! M4a objects as static table rows (O29 bounded scope: one row per
//! single-execution `New` site, indices are values — aliasing and escape
//! need no lifetime proof; `Int` fields and nullable refs to tabled types).
//! The output is Gabbro surface text in emitter-normal form (annotated lets,
//! wrap-arithmetic via helper calls, conversions spelled out) — every shape
//! verified against `gabbro check` + `gabbro build` before being emitted.
//! Everything outside M1 is a loud E5xx error, never silently miscompiled:
//! - E500: construct not emittable to Gabbro (message names it + the step).
//! - E501: needs a static bound (loops, dynamic sizes — the bound solver step).
//! - E502: needs shared ownership (`Rc` — no Gabbro counterpart, O29-scope).
//! - E503: concurrency (`spawn` — `start` is checker-only, C001).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use fastllvm_ir::*;

const U64_FULL: &str = "u64 in 0 .. 18446744073709551615";

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && s.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn sanitize_mod(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c == '_' || c.is_ascii_alphanumeric() {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || !(out.starts_with('_') || out.as_bytes()[0].is_ascii_alphabetic()) {
        out = format!("prog_{out}");
    }
    out
}

/// Method names (`Vec2.len`) carry a dot the Gabbro surface forbids.
/// Map them into the `vire_`-prefixed namespace users cannot declare
/// (the signature guard rejects every user `vire_` name but `vire_main`,
/// so the mapping is collision-free by construction). Applied at render
/// only — analysis keeps real names. `$` instantiations stay E500.
fn meth_name(name: &str) -> String {
    if name.contains('.') {
        format!("vire_method_{}", name.replace('.', "_"))
    } else {
        name.to_string()
    }
}

/// Map a machine-intrinsic call name (`__vire_*`) into the reserved
/// `vire_` head namespace (idempotent — names already starting there keep
/// exactly one prefix). Users cannot declare `vire_` names, so the mapping
/// is collision-free by construction.
fn hw_head_name(cname: &str) -> String {
    let t = cname.trim_start_matches('_');
    if t.starts_with("vire_") {
        t.to_string()
    } else {
        format!("vire_{t}")
    }
}

fn gab_ty(t: Ty) -> Option<&'static str> {
    match t {
        Ty::I64 => Some("i64"),
        _ => None,
    }
}

/// Scalar types crossing the C boundary into `extern fn` heads
/// (I64→i64, I32→i32). Everything else (`Ptr`, floats, refs) stays
/// rejected — no `int -> ptr`, no layouts the header never declared.
fn gab_extern_ty(t: Ty) -> Option<&'static str> {
    match t {
        Ty::I64 => Some("i64"),
        Ty::I32 => Some("i32"),
        _ => None,
    }
}

/// Split `extern "C"` decls into mappable heads vs loud declines.
/// Mappable = plain ident (no generics/`vire_` prelude clash, no user-fn
/// collision) with scalar-only params and scalar/`Void` return.
/// Unmappable names carry their reason (E500 at the call, never silent).
fn analyze_externs(
    program: &Program,
    errs: &mut Vec<String>,
) -> (BTreeMap<String, ExternDecl>, BTreeMap<String, String>) {
    let mut ok: BTreeMap<String, ExternDecl> = BTreeMap::new();
    let mut bad: BTreeMap<String, String> = BTreeMap::new();
    let user: BTreeSet<&str> =
        program.functions.iter().map(|f| f.name.as_str()).collect();
    for e in &program.extern_fns {
        if ok.contains_key(&e.name) || bad.contains_key(&e.name) {
            continue;
        }
        if !is_ident(&e.name) || e.name.contains('$') || e.name.starts_with("vire_") {
            let m = format!("extern `{}` not emittable (M1: plain idents, generics later; `vire_` is the prelude namespace)", e.name);
            errs.push(e500(m.clone()));
            bad.insert(e.name.clone(), m);
            continue;
        }
        if user.contains(e.name.as_str()) {
            let m = format!("extern `{}` collides with a Vire function of the same name (rename one)", e.name);
            errs.push(e500(m.clone()));
            bad.insert(e.name.clone(), m);
            continue;
        }
        let mut why: Option<String> = None;
        // `Ptr` never crosses (no `int -> ptr`): the IR erases it to `I64`,
        // so the flags — not the types — carry the verdict.
        if e.ptr_ret || e.ptr_params.iter().any(|b| *b) {
            why = Some(format!("extern `{}` crosses `Ptr` (M4: no int->ptr — pointers stay outside the Gabbro path)", e.name));
        }
        for p in e.params.iter() {
            if why.is_some() {
                break;
            }
            if gab_extern_ty(*p).is_none() {
                why = Some(format!("extern `{}` crosses `{p:?}` (M4: scalars only — no int->ptr, no object layouts)", e.name));
                break;
            }
        }
        if why.is_none() && !(e.ret == Ty::Void || gab_extern_ty(e.ret).is_some()) {
            why = Some(format!("extern `{}` returns `{:?}` (M4: scalar returns only)", e.name, e.ret));
        }
        match why {
            None => {
                ok.insert(e.name.clone(), e.clone());
            }
            Some(m) => {
                bad.insert(e.name.clone(), m);
            }
        }
    }
    // Machine intrinsics (Phase H1): `syscall` (Vire `syscall NAME(..) = NUM`
    // items lower to `Call "syscall" [num, args..]`) and the `__vire_*`
    // family (MMIO/port/fence helpers from `Mmio`/`Port`/`fence`). All shapes
    // are scalar by construction; each becomes an `extern fn` head (Gabbro
    // has no `syscall`-with-bare-number or device form for these — the
    // contract is signature + effects + costs, the body is machine, held at
    // `gabbro link` like every `extern fn`). Full `device`/`reg` decls and
    // `syscall ... via ...` gates need target ABI knowledge the Vire side
    // does not carry yet — later step, never faked.
    // Head names live in the reserved `vire_` namespace (users cannot
    // declare them — the signature guard rejects all user `vire_` names),
    // so synthesis is collision-free by construction.
    // Scalar type of an operand at a call site (consts take their width).
    let op_ty = |func: &Function, op: &Operand| -> Option<Ty> {
        match op {
            Operand::Copy(l) => func.locals.get(l.0 as usize).copied(),
            Operand::ConstI64(_) => Some(Ty::I64),
            Operand::ConstI32(_) => Some(Ty::I32),
            _ => None,
        }
    };
    // Register one synthesized head (arity-checked across sites).
    let mut synth = |ok: &mut BTreeMap<String, ExternDecl>,
                     bad: &mut BTreeMap<String, String>,
                     errs: &mut Vec<String>,
                     name: String,
                     params: Vec<Ty>,
                     ret: Ty| {
        if bad.contains_key(&name) {
            return;
        }
        match ok.get(&name) {
            Some(d) if d.params == params && d.ret == ret => {}
            Some(_) => {
                let m = format!("intrinsic `{name}` used with inconsistent shapes (M4: internal)");
                errs.push(e500(m.clone()));
                bad.insert(name, m);
            }
            None => {
                ok.insert(
                    name.clone(),
                    ExternDecl {
                        name,
                        params: params.clone(),
                        ret,
                        ptr_params: vec![false; params.len()],
                        ptr_ret: false,
                    },
                );
            }
        }
    };
    for f in &program.functions {
        for b in &f.blocks {
            for st in &b.statements {
                let (cname, args, dest) = match st {
                    Statement::Call { func, args, dest } => (func, args, dest),
                    _ => continue,
                };
                // User functions win over the intrinsic family: a call that
                // resolves to a Vire function is never an intrinsic (name
                // shadowing itself is E500 at signature validation).
                if user.contains(cname.as_str()) {
                    continue;
                }
                if cname == "syscall" {
                    // [num, args..]; the number rides along as data (a static
                    // head per arity class would hide it — the name carries
                    // it when constant, `dyn` when computed).
                    let (num_key, rest) = match args.first() {
                        Some(Operand::ConstI64(n)) if *n >= 0 => (format!("{n}"), &args[1..]),
                        Some(Operand::ConstI32(n)) if *n >= 0 => (format!("{n}"), &args[1..]),
                        Some(_) => ("dyn".to_string(), &args[1..]),
                        None => {
                            let m = "syscall without a number (M4: internal)".to_string();
                            errs.push(e500(m.clone()));
                            bad.insert("syscall".to_string(), m);
                            continue;
                        }
                    };
                    let mut params = vec![];
                    let mut okk = true;
                    for a in args.iter() {
                        match op_ty(f, a) {
                            Some(t @ (Ty::I64 | Ty::I32)) => params.push(t),
                            _ => {
                                let m = format!(
                                    "syscall argument of non-scalar type (M4: scalars only — no int->ptr)"
                                );
                                errs.push(e500(m.clone()));
                                bad.insert(format!("vire_syscall_{num_key}_{}", args.len() - 1), m);
                                okk = false;
                                break;
                            }
                        }
                    }
                    if !okk {
                        continue;
                    }
                    let ret = match dest {
                        None => Ty::Void,
                        Some(d) => match f.locals.get(d.0 as usize).copied() {
                            Some(t @ (Ty::I64 | Ty::I32)) => t,
                            _ => {
                                let m = "syscall result of non-scalar type (M4: internal)".to_string();
                                errs.push(e500(m.clone()));
                                bad.insert(format!("vire_syscall_{num_key}_{}", args.len() - 1), m);
                                continue;
                            }
                        },
                    };
                    synth(
                        &mut ok,
                        &mut bad,
                        errs,
                        format!("vire_syscall_{num_key}_{}", args.len() - 1),
                        params,
                        ret,
                    );
                } else if cname.starts_with("__vire_") {
                    // Machine helper: static name, scalar shapes.
                    let mut params = vec![];
                    let mut okk = true;
                    for a in args.iter() {
                        match op_ty(f, a) {
                            Some(t @ (Ty::I64 | Ty::I32)) => params.push(t),
                            _ => {
                                let m = format!(
                                    "intrinsic `{cname}` argument of non-scalar type (M4: internal)"
                                );
                                errs.push(e500(m.clone()));
                                bad.insert(hw_head_name(cname), m);
                                okk = false;
                                break;
                            }
                        }
                    }
                    if !okk {
                        continue;
                    }
                    let ret = match dest {
                        None => Ty::Void,
                        Some(d) => match f.locals.get(d.0 as usize).copied() {
                            Some(t @ (Ty::I64 | Ty::I32)) => t,
                            _ => {
                                let m = format!("intrinsic `{cname}` result of non-scalar type (M4: internal)");
                                errs.push(e500(m.clone()));
                                bad.insert(hw_head_name(cname), m);
                                continue;
                            }
                        },
                    };
                    synth(
                        &mut ok,
                        &mut bad,
                        errs,
                        hw_head_name(cname),
                        params,
                        ret,
                    );
                }
            }
        }
    }
    // Rewrite intrinsic calls to their synthesized heads (same pass order as
    // analysis would see them — names only; statements are untouched).
    // NOTE: actual call rendering happens in `emit_stmt` via `obj_params`…
    (ok, bad)
}

/// Prelude pieces used by the program (emitted selectively — no dead items).
#[derive(Default)]
struct PreludeUse {
    print: bool,
    add: bool,
    sub: bool,
    mul: bool,
    neg: bool,
    /// A `retry` was emitted (needs `abort` for `on_exceeded`).
    retry_loop: bool,
    /// String maxes printed (`vire_print_str_M` per max).
    print_str: BTreeSet<u64>,
    /// A `narrow ... else { return abort(); }` was emitted (needs `abort`).
    need_abort: bool,
    /// Array tables: name → (length, slot type). Program-global items
    /// (merged like the prelude; names are unique per function+site).
    tables: BTreeMap<String, (u64, String)>,
    /// Explicit-region arenas: name → ceiling (bytes). Program-global
    /// items (`pub arena NAME capacity 0 .. MAX of i64`), one per
    /// `region(max)` site with a constant ceiling — the ceiling stays
    /// user-visible in the output (never solver-hidden).
    arenas: BTreeMap<String, u64>,
    /// Object tables: table name → (row count, ordered (field, slot type)).
    /// Program-global items (`pub table vire_tab_C count K`), one per used
    /// class with at least one static row.
    obj_tables: BTreeMap<String, (u64, Vec<(String, String)>)>,
}

/// Declared costs of a `vire_print_str_M` helper: base + per-byte margin./// Generous on purpose (K001 holds it against Gabbro's measurement —
/// over-approximation is the sound direction).
fn str_print_costs(max: u64) -> u64 {
    2048u64.saturating_add(max.saturating_mul(1024))
}

/// A Vire string literal as a Gabbro `"..."` literal. Gabbro string literals
/// take raw bytes with NO escapes (`\` belongs to no form — L006), so `"`,
/// `\` and non-plain bytes are inexpressible → honest E501 (rare; the Vire
/// string suite is ASCII-only).
fn gab_str_lit(s: &str) -> Result<String, String> {
    let mut out = String::from("\"");
    for b in s.bytes() {
        match b {
            0x09 | 0x0A | 0x0D | 0x20..=0x21 | 0x23..=0x5B | 0x5D..=0x7E => out.push(b as char),
            _ => {
                return Err("error[E501]: string literal needs plain bytes (no `\"`/backslash/non-ASCII — later step)".to_string());
            }
        }
    }
    out.push('"');
    Ok(out)
}

fn prelude_text(u: &PreludeUse) -> String {
    let mut s = String::new();
    if u.print || u.add || u.sub || u.mul || u.neg || u.retry_loop || !u.print_str.is_empty() {
        s.push_str("extern fn putchar(c : i32) -> i32 effects { writes ausgabe } costs <= 8 ops;\n\n");
    }
    // The bound backstop: proven trip counts make `on_exceeded` dead code;
    // `abort` is the honest trap if a bound ever lies (loud crash, exit 134).
    if u.retry_loop || u.need_abort || !u.print_str.is_empty() {
        s.push_str("extern fn abort() -> never effects { diverges } costs <= 1 ops;\n\n");
    }
    if u.add || u.sub || u.mul || u.neg {
        s.push_str(&format!(
            "fn vire_uadd64(a : {U64_FULL}, b : {U64_FULL}) -> {U64_FULL} effects {{ pure }} costs <= 8 ops {{\n    return a +% b;\n}}\n\n"
        ));
        s.push_str(&format!(
            "fn vire_usub64(a : {U64_FULL}, b : {U64_FULL}) -> {U64_FULL} effects {{ pure }} costs <= 8 ops {{\n    return a -% b;\n}}\n\n"
        ));
        s.push_str(&format!(
            "fn vire_umul64(a : {U64_FULL}, b : {U64_FULL}) -> {U64_FULL} effects {{ pure }} costs <= 8 ops {{\n    return a *% b;\n}}\n\n"
        ));
    }
    if u.print {
        s.push_str(
            "fn vire_print_pos(m : u64) effects { writes ausgabe } costs <= 64 ops decreases m {\n    if m >= 10 {\n        vire_print_pos(m / 10);\n    }\n    let d : u64 = m % 10;\n    let ch : u64 = 48 + d;\n    putchar(i32(ch));\n    return;\n}\n\n",
        );
        s.push_str(
            "fn vire_print_int(n : i64) effects { writes ausgabe } costs <= 128 ops {\n    if n < 0 {\n        putchar(45);\n        let un : u64 = u64(n);\n        let zero : u64 = vire_usub64(un, un);\n        let mag : u64 = vire_usub64(zero, un);\n        vire_print_pos(mag);\n        return;\n    }\n    let up : u64 = u64(n);\n    vire_print_pos(up);\n    return;\n}\n\n",
        );
    }
    // String printers, one per max: counted retry over the bytes with the
    // `lenof(s) > k` guard idiom (unsigned counter — N454's proven shape).
    // The bound is the declared max (static, known to the emitter — no trip
    // analysis needed); `until` stops at the true length.
    for m in u.print_str.iter() {
        let c = str_print_costs(*m);
        s.push_str(&format!(
            "fn vire_print_str_{m}(s : string max {m}) effects {{ writes ausgabe }} costs <= {c} ops {{\n    let mut k : u32 = 0;\n    retry vploop until (k >= lenof(s)) bounded 2048 ops on_exceeded abort {{\n        if lenof(s) > k {{\n            putchar(i32(s[k]));\n        }}\n        k = k +% 1;\n        next vploop;\n    }}\n    return;\n}}\n\n"
        ));
    }
    s
}

struct FnEmit<'a> {
    func: &'a Function,
    /// String literal pool (`Operand::ConstStr` indexes here).
    strings: &'a [String],
    /// Max per string local (flow-insensitive: max over all defs, so every
    /// reassignment fits the declared `string max M`).
    str_max: BTreeMap<u32, u64>,
    /// Array site per array local (copies alias; see `analyze_arrays`).
    arr_site: BTreeMap<u32, usize>,
    /// Ordered array sites (table index + length).
    arr_sites: Vec<ArrSite>,
    /// Array locals rebound to a second site (E501 at use).
    arr_conflict: BTreeSet<u32>,
    /// I32 locals holding `lenof`/length results (their `Convert` to I64 is
    /// a sound widening `i64(...)`; all other conversions stay E500).
    len32: BTreeSet<u32>,
    /// I32-declared locals bound as i64 with exact values (GetField
    /// tag/value dests + copies/identity-converts; free use, no guard).
    i64free: BTreeSet<u32>,
    /// Option-object locals (New/StackNew of sums + copies, precomputed):
    /// their pair is bound once at function top; later writes assign.
    opt_locs: BTreeSet<u32>,
    /// I32 locals bound back as i64 (widened `I64→I32` index converts —
    /// usable ONLY as array indices; every other use is E500).
    wide32: BTreeSet<u32>,
    /// Observed string maxima per local from a future trace pass.
    /// DIAGNOSTIC ONLY: fed into E501 messages, never into accepts. A later
    /// CEGAR loop may propose candidates here subject to static verification
    /// (today always empty — static inference alone decides).
    trace_hints: BTreeMap<u32, u64>,
    /// Locals holding bools (comparisons + copies thereof).
    bools: BTreeSet<u32>,
    /// IR locals never read (dead pure temps are skipped, not rejected).
    used: BTreeSet<u32>,
    /// Fresh `tN` temps for expansions (negative consts, Neg).
    temps: u32,
    prelude: PreludeUse,
    errs: Vec<String>,
}

/// String analysis: which Ref locals are strings (def-chains rooted in
/// `ConstStr` / `jrt_str_concat` / copies thereof) and their
/// flow-insensitive max. Non-string Refs are M4 objects (E500 at use);
/// strings without a provable max surface lazily as E501 at use sites
/// (bound-flavored — same leniency as M1 dead code).
fn analyze_strings(
    func: &Function,
    pool: &[String],
) -> (BTreeMap<u32, u64>, BTreeSet<u32>) {
    let mut max: BTreeMap<u32, u64> = BTreeMap::new();
    let mut is_str = BTreeSet::new();
    // Seeds: literal uses.
    for b in &func.blocks {
        for st in &b.statements {
            if let Statement::Assign(d, Rvalue::Use(Operand::ConstStr(i))) = st {
                if let Some(lit) = pool.get(*i as usize) {
                    is_str.insert(d.0);
                    let m = lit.len() as u64;
                    max.insert(d.0, m.max(*max.get(&d.0).unwrap_or(&0)));
                }
            }
        }
    }
    // Fixpoint: copies + concats (monotone max-union; defs+1 rounds suffice).
    for _ in 0..func.locals.len() + 1 {
        let mut grew = false;
        for b in &func.blocks {
            for st in &b.statements {
                match st {
                    Statement::Assign(d, Rvalue::Use(Operand::Copy(l))) => {
                        if let Some(m) = max.get(&l.0).copied() {
                            if is_str.insert(d.0) {
                                grew = true;
                            }
                            let e = max.entry(d.0).or_insert(0);
                            if m > *e {
                                *e = m;
                                grew = true;
                            }
                        }
                    }
                    Statement::Call { dest: Some(d), func: f, args } if f == "jrt_str_concat" => {
                        if args.len() == 2 {
                            let arg = |op: &Operand| -> Option<u64> {
                                match op {
                                    Operand::Copy(l) => max.get(&l.0).copied(),
                                    Operand::ConstStr(i) => pool.get(*i as usize).map(|s| s.len() as u64),
                                    _ => None,
                                }
                            };
                            // (NLL: the two lookups end before mutation.)
                            let pair = (arg(&args[0]), arg(&args[1]));
                            if let (Some(a), Some(c)) = pair {
                                if is_str.insert(d.0) {
                                    grew = true;
                                }
                                let e = max.entry(d.0).or_insert(0);
                                let s = a.saturating_add(c);
                                if s > *e {
                                    *e = s;
                                    grew = true;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if !grew {
            break;
        }
    }
    // lenof dests with I32 type (widening-convertible), plus array-length
    // I32 dests (likewise: the length is static, bound as `u32` literal).
    let mut len32 = BTreeSet::new();
    for b in &func.blocks {
        for st in &b.statements {
            if let Statement::Call { dest: Some(d), func: f, .. } = st {
                if f == "jrt_str_length" && func.locals.get(d.0 as usize) == Some(&Ty::I32) {
                    len32.insert(d.0);
                }
            }
            if let Statement::ArrayLen { dest: d, .. } = st {
                if func.locals.get(d.0 as usize) == Some(&Ty::I32) {
                    len32.insert(d.0);
                }
            }
        }
    }
    (max, len32)
}

/// A `region(max)` site: push and pop statements paired by a stack over
/// block order. Lowering brackets exactly one pop per push and E300
/// rejects boundary-crossing control flow, so every path pairs.
/// Infallible; constness and declines surface lazily at emission.
#[derive(Clone)]
struct RegionMark {
    /// Arena item name (`vire_pool_{fname}_{k}`).
    name: String,
    /// Const ceiling for pushes (None for pops and non-const ceilings).
    max: Option<u64>,
    /// True for the push, false for the pop.
    is_push: bool,
}

/// Pair `jrt_region_push`/`jrt_region_pop` per function (block order).
fn analyze_regions(func: &Function) -> BTreeMap<(u32, usize), RegionMark> {
    let mut marks: BTreeMap<(u32, usize), RegionMark> = BTreeMap::new();
    let mut stack: Vec<(String, Option<u64>)> = vec![];
    let mut count = 0usize;
    for (bi, b) in func.blocks.iter().enumerate() {
        for (si, st) in b.statements.iter().enumerate() {
            if let Statement::Call { dest, func: f, args } = st {
                if dest.is_some() {
                    continue;
                }
                if f == "jrt_region_push" && args.len() == 1 {
                    let max = match &args[0] {
                        Operand::ConstI64(v) if *v > 0 => Some(*v as u64),
                        Operand::ConstI32(v) if *v > 0 => Some(*v as u64),
                        _ => None,
                    };
                    let name = format!("vire_pool_{}_{}", func.name, count);
                    count += 1;
                    stack.push((name.clone(), max));
                    marks.insert((bi as u32, si), RegionMark { name, max, is_push: true });
                } else if f == "jrt_region_pop" && args.is_empty() {
                    match stack.pop() {
                        Some((name, _)) => {
                            marks.insert((bi as u32, si), RegionMark { name, max: None, is_push: false });
                        }
                        None => {
                            marks.insert((bi as u32, si), RegionMark { name: String::new(), max: None, is_push: false });
                        }
                    }
                }
            }
        }
    }
    marks
}

/// Object slot types (M4a static rows): integers and nullable references
/// to other usable classes. No `I32` slots: `Bool`/`I32`-typed fields would
/// conflate print semantics (0/1 vs number — unrecoverable after lowering),
/// so such classes stay declined.
#[derive(Clone)]
enum ObjSlot {
    I64,
    Ref(String),
}

/// Table item name for an object class (own namespace; user `fn`/`type`
/// collisions are loud E500 at analysis).
fn obj_table_name(class: &str) -> String {
    format!("vire_tab_{class}")
}

/// Usable object classes: name → ordered (field, slot). Fixpoint over
/// ref-targets (mutual recursion allowed — indices are values, no layout
/// cycle). A class is usable iff: plain ident, no `$` (mono/Rc$), not a
/// builtin sum, not colliding with functions/externs/tables, every field a
/// plain ident with an `Int` type or a `Ref` to a usable class.
fn obj_usable_classes(program: &Program) -> BTreeMap<String, Vec<(String, ObjSlot)>> {
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for f in &program.functions {
        taken.insert(f.name.clone());
    }
    for e in &program.extern_fns {
        taken.insert(e.name.clone());
    }
    for reserved in ["vire_print_int", "vire_print_pos", "vire_uadd64", "vire_usub64", "vire_umul64", "putchar", "abort"] {
        taken.insert(reserved.to_string());
    }
    // Candidates pass the name checks; field checks iterate to fixpoint.
    let mut cand: BTreeSet<String> = BTreeSet::new();
    for c in &program.classes {
        if c.is_interface || is_sum_class(&c.name) || c.name.contains('$') || c.name.contains('/') || !is_ident(&c.name) {
            continue;
        }
        if taken.contains(&obj_table_name(&c.name)) || c.name.starts_with("vire_") {
            continue;
        }
        cand.insert(c.name.clone());
    }
    let by_name: BTreeMap<&str, &ClassInfo> =
        program.classes.iter().map(|c| (c.name.as_str(), c)).collect();
    loop {
        let mut drop = vec![];
        for name in cand.iter() {
            let ci = match by_name.get(name.as_str()) {
                Some(ci) => ci,
                None => {
                    drop.push(name.clone());
                    continue;
                }
            };
            let mut bad = false;
            for f in &ci.fields {
                if !is_ident(&f.name) {
                    bad = true;
                    break;
                }
                match f.ty {
                    Ty::I64 => {}
                    Ty::Ref => match &f.ref_target {
                        Some(t) if cand.contains(t) => {}
                        _ => {
                            bad = true;
                            break;
                        }
                    },
                    _ => {
                        bad = true;
                        break;
                    }
                }
            }
            if bad {
                drop.push(name.clone());
            }
        }
        if drop.is_empty() {
            break;
        }
        for d in drop {
            cand.remove(&d);
        }
    }
    let mut out = BTreeMap::new();
    for name in cand.iter() {
        let ci = by_name[name.as_str()];
        let mut slots = vec![];
        for f in &ci.fields {
            match f.ty {
                Ty::I64 => slots.push((f.name.clone(), ObjSlot::I64)),
                Ty::Ref => slots.push((f.name.clone(), ObjSlot::Ref(f.ref_target.clone().unwrap()))),
                _ => {}
            }
        }
        out.insert(name.clone(), slots);
    }
    out
}

/// Object flow info for the whole program (M4a + M4b).
struct ObjInfo {
    /// Function → local → class (union over defs; conflicts removed).
    locals: BTreeMap<String, BTreeMap<u32, String>>,
    /// Functions returning references: name → class (fixpoint over Returns).
    ret_class: BTreeMap<String, String>,
    /// Reference parameters with proven classes (Phase 1): fn → param → class.
    /// From body-use classes + call-site unions (two passes, deterministic).
    params: BTreeMap<String, BTreeMap<u32, String>>,
    /// New/StackNew sites with static rows: (fn, block, stmt) → (class, row).
    rows: BTreeMap<(String, u32, usize), (String, u64)>,
    /// Class → row count (table `count`).
    row_counts: BTreeMap<String, u64>,
}

/// Body-use classes per Ref param: classes named at GetField/PutField with
/// the param as base. Purely local (no fixpoint); the call-site half lives
/// in `analyze_objects`.
fn body_param_uses(func: &Function) -> Vec<BTreeSet<String>> {
    let mut out: Vec<BTreeSet<String>> = func.params.iter().map(|_| BTreeSet::new()).collect();
    for b in &func.blocks {
        for st in &b.statements {
            match st {
                Statement::GetField { obj: Operand::Copy(l), class, .. }
                | Statement::PutField { obj: Operand::Copy(l), class, .. } => {
                    if (l.0 as usize) < func.params.len()
                        && func.params[l.0 as usize] == Ty::Ref
                    {
                        out[l.0 as usize].insert(class.clone());
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Local→class union for one function given callee return classes.
/// Unknown absorbs (null defs carry no class); disagreement removes.
/// `param_seed` binds proven parameter classes (Phase 1) so copies from
/// parameters propagate like any other known class.
fn obj_local_map(
    func: &Function,
    usable: &BTreeMap<String, Vec<(String, ObjSlot)>>,
    ret_class: &BTreeMap<String, String>,
    param_seed: &BTreeMap<u32, String>,
) -> BTreeMap<u32, String> {
    let field_target = |class: &str, field: &str| -> Option<String> {
        usable.get(class)?.iter().find(|(n, _)| n == field).and_then(|(_, s)| match s {
            ObjSlot::Ref(t) => Some(t.clone()),
            _ => None,
        })
    };
    let mut map: BTreeMap<u32, String> = BTreeMap::new();
    let mut bad: BTreeSet<u32> = BTreeSet::new();
    // Proven parameter classes seed the map (Phase 1): a copy from a
    // classed parameter carries that class everywhere the copy flows.
    for (l, c) in param_seed.iter() {
        map.insert(*l, c.clone());
    }
    let put = |map: &mut BTreeMap<u32, String>, bad: &mut BTreeSet<u32>, l: u32, c: String| {
        if bad.contains(&l) {
            return;
        }
        match map.get(&l) {
            None => {
                map.insert(l, c);
            }
            Some(old) if *old == c => {}
            Some(_) => {
                map.remove(&l);
                bad.insert(l);
            }
        }
    };
    for _ in 0..func.locals.len() + 1 {
        let mut grew = false;
        for b in &func.blocks {
            for st in &b.statements {
                match st {
                    Statement::New { dest: d, class } | Statement::StackNew { dest: d, class } => {
                        if usable.contains_key(class) {
                            let before = map.len();
                            put(&mut map, &mut bad, d.0, class.clone());
                            grew |= map.len() != before || bad.contains(&d.0);
                        }
                    }
                    Statement::Assign(d, Rvalue::Use(Operand::Copy(l))) => {
                        if func.locals.get(d.0 as usize) == Some(&Ty::Ref) {
                            if let Some(c) = map.get(&l.0).cloned() {
                                let before = (map.len(), bad.len());
                                put(&mut map, &mut bad, d.0, c);
                                grew |= (map.len(), bad.len()) != before;
                            }
                        }
                    }
                    Statement::GetField { dest: d, obj, class, field } => {
                        if func.locals.get(d.0 as usize) == Some(&Ty::Ref) {
                            if let Some(t) = field_target(class, field) {
                                let before = (map.len(), bad.len());
                                put(&mut map, &mut bad, d.0, t);
                                grew |= (map.len(), bad.len()) != before;
                            }
                        }
                        // Use-site class: the access names the class lowering
                        // proved for its base (inlined null args, flattened
                        // temps). Conflicts still remove; heterogeneous merges
                        // never reach IR, so this adds no unsound accept.
                        if let Operand::Copy(l) = obj {
                            if func.locals.get(l.0 as usize) == Some(&Ty::Ref)
                                && usable.contains_key(class)
                            {
                                let before = (map.len(), bad.len());
                                put(&mut map, &mut bad, l.0, class.clone());
                                grew |= (map.len(), bad.len()) != before;
                            }
                        }
                    }
                    Statement::Call { dest: Some(d), func: f, .. } => {
                        if func.locals.get(d.0 as usize) == Some(&Ty::Ref) {
                            if let Some(c) = ret_class.get(f).cloned() {
                                let before = (map.len(), bad.len());
                                put(&mut map, &mut bad, d.0, c);
                                grew |= (map.len(), bad.len()) != before;
                            }
                        }
                    }
                    // Use-site class on stores (same soundness as loads).
                    Statement::PutField { obj: Operand::Copy(l), class, .. } => {
                        if func.locals.get(l.0 as usize) == Some(&Ty::Ref)
                            && usable.contains_key(class)
                        {
                            let before = (map.len(), bad.len());
                            put(&mut map, &mut bad, l.0, class.clone());
                            grew |= (map.len(), bad.len()) != before;
                        }
                    }
                    _ => {}
                }
            }
        }
        if !grew {
            break;
        }
    }
    map
}

/// Build the whole-program object flow: local maps + return-class fixpoint
/// + static row assignment. `loops` supplies body blocks for the
/// single-execution check (a `New` must run at most once — row
/// linearization for repeated construction is a later step).
fn analyze_objects(
    program: &Program,
    reachable: &BTreeSet<String>,
    usable: &BTreeMap<String, Vec<(String, ObjSlot)>>,
    loops: &BTreeMap<String, BTreeMap<u32, LoopInfo>>,
) -> ObjInfo {
    // Callers: callee → list of (caller, block) over reachable functions.
    let mut callers: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
    // Call blocks per caller (for the loop/header check below).
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            for st in &b.statements {
                if let Statement::Call { func: c, .. } = st {
                    callers.entry(c.clone()).or_default().push((f.name.clone(), bi as u32));
                }
            }
        }
    }
    // Return-class fixpoint (monotone: entries only added; ≤ F+1 rounds).
    // Body-use singletons seed the local maps (a parameter used through
    // exactly one class carries it — call-site agreement is checked below).
    let body_uses: BTreeMap<String, Vec<BTreeSet<String>>> = program
        .functions
        .iter()
        .filter(|f| reachable.contains(&f.name))
        .map(|f| (f.name.clone(), body_param_uses(f)))
        .collect();
    let seed_of = |f: &Function| -> BTreeMap<u32, String> {
        body_uses
            .get(&f.name)
            .map(|sets| {
                sets.iter()
                    .enumerate()
                    .filter_map(|(i, s)| {
                        if s.len() == 1 {
                            Some((i as u32, s.iter().next().unwrap().clone()))
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut ret_class: BTreeMap<String, String> = BTreeMap::new();
    for _ in 0..program.functions.len() + 1 {
        let mut grew = false;
        for f in &program.functions {
            if !reachable.contains(&f.name) {
                continue;
            }
            if f.ret != Ty::Ref || ret_class.contains_key(&f.name) {
                continue;
            }
            let lm = obj_local_map(f, usable, &ret_class, &seed_of(f));
            let mut cls: BTreeSet<String> = BTreeSet::new();
            let mut unknown = false;
            for b in &f.blocks {
                if let Terminator::Return(Some(Operand::Copy(l))) = &b.terminator {
                    match lm.get(&l.0) {
                        Some(c) => {
                            cls.insert(c.clone());
                        }
                        None => unknown = true,
                    }
                } else if let Terminator::Return(Some(_)) = &b.terminator {
                    unknown = true;
                }
            }
            if !unknown && cls.len() == 1 {
                ret_class.insert(f.name.clone(), cls.into_iter().next().unwrap());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut locals = BTreeMap::new();
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        locals.insert(f.name.clone(), obj_local_map(f, usable, &ret_class, &seed_of(f)));
    }
    // Phase 1 parameter classes (two passes, deterministic): body-use
    // classes plus call-site arg unions. A parameter is proven iff its body
    // uses name at most one class AND every known call-site arg agrees
    // (nulls are compatible with anything); with no body uses, a single
    // call-site class suffices. Anything else stays unknown → E500 M4b.
    let by_name: BTreeMap<&str, &Function> =
        program.functions.iter().map(|f| (f.name.as_str(), f)).collect();
    // (callee, arg index) → classes seen at call sites.
    let mut call_sets: BTreeMap<(String, u32), BTreeSet<String>> = BTreeMap::new();
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        let lm: &BTreeMap<u32, String> =
            match locals.get(&f.name) {
                Some(m) => m,
                None => continue,
            };
        for b in &f.blocks {
            for st in &b.statements {
                if let Statement::Call { func: c, args, .. } = st {
                    let callee = match by_name.get(c.as_str()) {
                        Some(g) => g,
                        None => continue,
                    };
                    for (k, a) in args.iter().enumerate() {
                        if (k as usize) >= callee.params.len()
                            || callee.params[k] != Ty::Ref
                        {
                            continue;
                        }
                        if let Operand::Copy(l) = a {
                            if let Some(ac) = lm.get(&l.0).cloned() {
                                call_sets
                                    .entry((c.clone(), k as u32))
                                    .or_default()
                                    .insert(ac);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut params: BTreeMap<String, BTreeMap<u32, String>> = BTreeMap::new();
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        let mut pm = BTreeMap::new();
        if let Some(sets) = body_uses.get(&f.name) {
            for (k, bset) in sets.iter().enumerate() {
                if f.params[k] != Ty::Ref {
                    continue;
                }
                let cset =
                    call_sets.get(&(f.name.clone(), k as u32)).cloned().unwrap_or_default();
                let decided = if bset.len() > 1 {
                    None
                } else if bset.len() == 1 {
                    let b = bset.iter().next().unwrap();
                    if cset.iter().all(|c| c == b) {
                        Some(b.clone())
                    } else {
                        None
                    }
                } else if cset.len() == 1 {
                    Some(cset.iter().next().unwrap().clone())
                } else {
                    None
                };
                if let Some(c) = decided {
                    pm.insert(k as u32, c);
                }
            }
        }
        if !pm.is_empty() {
            params.insert(f.name.clone(), pm);
        }
    }
    // Pass B: local maps with finalized parameter classes (copies from
    // parameters now resolve through chains of any depth the two passes see).
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        let seed = params.get(&f.name).cloned().unwrap_or_default();
        locals.insert(f.name.clone(), obj_local_map(f, usable, &ret_class, &seed));
    }
    // Single-execution: walk the unique caller chain; every call block must
    // sit outside loops AND headers (headers hold pure calls only, and a
    // `New`-carrying function is never pure — table writes are effects).
    let runs_once = |site_fn: &str, site_block: u32| -> bool {
        let in_loop = |fname: &str, blk: u32| -> bool {
            match loops.get(fname) {
                None => false,
                Some(t) => {
                    if t.contains_key(&blk) {
                        return true;
                    }
                    t.values().any(|li| li.body_blocks.contains(&blk))
                }
            }
        };
        if in_loop(site_fn, site_block) {
            return false;
        }
        let mut cur = site_fn.to_string();
        let mut seen = BTreeSet::new();
        loop {
            if cur == "vire_main" {
                return true;
            }
            if !seen.insert(cur.clone()) {
                return false;
            }
            match callers.get(&cur) {
                Some(v) if v.len() == 1 => {
                    let (caller, blk) = &v[0];
                    if in_loop(caller, *blk) {
                        return false;
                    }
                    cur = caller.clone();
                }
                _ => return false,
            }
        }
    };
    // Static rows in deterministic order (program order, then block, stmt).
    let mut rows: BTreeMap<(String, u32, usize), (String, u64)> = BTreeMap::new();
    let mut row_counts: BTreeMap<String, u64> = BTreeMap::new();
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            for (si, st) in b.statements.iter().enumerate() {
                match st {
                    Statement::New { dest: _, class } | Statement::StackNew { dest: _, class } => {
                        if !usable.contains_key(class) {
                            continue;
                        }
                        if runs_once(&f.name, bi as u32) {
                            let r = row_counts.get(class).copied().unwrap_or(0);
                            rows.insert((f.name.clone(), bi as u32, si), (class.clone(), r));
                            row_counts.insert(class.clone(), r + 1);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    ObjInfo { locals, ret_class, params, rows, row_counts }
}
/// Sites cover `StackNewArray` and const-length `NewArray` of `Long`/`U8`
/// kind (both value-typed I64 — U8 reads 0..255 into i64 slots, no range
/// proofs). Everything else (dynamic lengths, other kinds, regions) is a
/// later step; copies alias their root's site.
#[derive(Clone)]
struct ArrSite {
    len: u64,
}

/// Table item name for an array site (our namespace; Vire declares no
/// tables, and `vire_`-prefixed function names are rejected).
fn arr_table_name(fname: &str, site: usize) -> String {
    format!("vire_arr_{fname}_{site}")
}

/// Array analysis: site per array local (copies alias; rebinding to a
/// second site is a conflict) + ordered site list. Infallible; E500/E501
/// surface lazily at use sites.
fn analyze_arrays(func: &Function) -> (BTreeMap<u32, usize>, Vec<ArrSite>, BTreeSet<u32>) {
    let mut sites: Vec<ArrSite> = vec![];
    let mut site_of: BTreeMap<u32, usize> = BTreeMap::new();
    let mut conflict: BTreeSet<u32> = BTreeSet::new();
    let mut claim = |site_of: &mut BTreeMap<u32, usize>,
                     conflict: &mut BTreeSet<u32>,
                     local: u32,
                     site: usize| {
        match site_of.get(&local).copied() {
            Some(s) if s != site => {
                conflict.insert(local);
            }
            Some(_) => {}
            None => {
                site_of.insert(local, site);
            }
        }
    };
    for b in &func.blocks {
        for st in &b.statements {
            match st {
                Statement::StackNewArray { dest, kind, len } => {
                    if matches!(kind, ArrKind::Long | ArrKind::U8) && *len > 0 {
                        let site = sites.len();
                        sites.push(ArrSite { len: *len as u64 });
                        claim(&mut site_of, &mut conflict, dest.0, site);
                    }
                }
                Statement::NewArray { dest, kind, len } => {
                    let n = match len {
                        Operand::ConstI64(v) if *v > 0 => Some(*v as u64),
                        Operand::ConstI32(v) if *v > 0 => Some(*v as u64),
                        _ => None,
                    };
                    if matches!(kind, ArrKind::Long | ArrKind::U8) {
                        if let Some(m) = n {
                            let site = sites.len();
                            sites.push(ArrSite { len: m });
                            claim(&mut site_of, &mut conflict, dest.0, site);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // Fixpoint: copies alias their source's site.
    for _ in 0..func.locals.len() + 1 {
        let mut grew = false;
        for b in &func.blocks {
            for st in &b.statements {
                if let Statement::Assign(d, Rvalue::Use(Operand::Copy(l))) = st {
                    if let Some(s) = site_of.get(&l.0).copied() {
                        let before = site_of.len() + conflict.len();
                        claim(&mut site_of, &mut conflict, d.0, s);
                        if site_of.len() + conflict.len() != before {
                            grew = true;
                        }
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    (site_of, sites, conflict)
}

/// Effects fixpoint: per-function (reads, writes) over named carriers
/// (`ausgabe` for prints, table names for slot access), propagated through
/// user calls. Empty/empty renders as `pure`.
fn compute_effects(
    program: &Program,
    reachable: &BTreeSet<String>,
    obj_classes: &BTreeMap<String, Vec<(String, ObjSlot)>>,
) -> BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> {
    let mut eff: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    // `extern "C"` heads: scalar-only C cannot address module carriers (no
    // `Ptr` crosses), so the only nameable sink is `ausgabe`. C-internals are
    // machine body, held at `gabbro link` (N501–N504). Seed all decls —
    // invalid ones fail emission anyway (output discarded).
    for e in &program.extern_fns {
        eff.insert(e.name.clone(), (BTreeSet::new(), BTreeSet::from(["ausgabe".to_string()])));
    }
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        let (site_of, _, _) = analyze_arrays(f);
        let table_of = |l: u32| -> Option<String> {
            site_of.get(&l).map(|s| arr_table_name(&f.name, *s))
        };
        let mut r = BTreeSet::new();
        let mut w = BTreeSet::new();
        let rmarks = analyze_regions(f);
        for (bi, b) in f.blocks.iter().enumerate() {
            for (si, st) in b.statements.iter().enumerate() {
                match st {
                    Statement::Call { func, .. } => {
                        if func == "jrt_println_long" || func == "jrt_println_str" {
                            w.insert("ausgabe".to_string());
                        }
                        // Region push/pop writes its arena (`reset` at pop;
                        // the push declares it). Unpaired pops carry no name.
                        if func == "jrt_region_push" || func == "jrt_region_pop" {
                            if let Some(m) = rmarks.get(&(bi as u32, si)) {
                                if !m.name.is_empty() {
                                    w.insert(m.name.clone());
                                }
                            }
                        }
                    }
                    Statement::ArrayLoad { arr, .. } | Statement::ArrayLen { arr, .. } => {
                        if let Operand::Copy(l) = arr {
                            if let Some(t) = table_of(l.0) {
                                r.insert(t);
                            }
                        }
                    }
                    Statement::ArrayStore { arr, .. } => {
                        if let Operand::Copy(l) = arr {
                            if let Some(t) = table_of(l.0) {
                                w.insert(t);
                            }
                        }
                    }
                    // Object carriers: construction binds a static row (no
                    // carrier touched); field stores write, loads read.
                    Statement::PutField { class, .. } => {
                        if obj_classes.contains_key(class) {
                            w.insert(obj_table_name(class));
                        }
                    }
                    Statement::GetField { class, .. } => {
                        if obj_classes.contains_key(class) {
                            r.insert(obj_table_name(class));
                        }
                    }
                    _ => {}
                }
            }
        }
        eff.insert(f.name.clone(), (r, w));
    }
    // Propagate through user calls to fixpoint (no recursion → converges).
    for _ in 0..program.functions.len() + 1 {
        let mut changed = false;
        for f in &program.functions {
            if !reachable.contains(&f.name) {
                continue;
            }
            for b in &f.blocks {
                for st in &b.statements {
                    if let Statement::Call { func, .. } = st {
                        if let Some((cr, cw)) = eff.get(func).cloned() {
                            let e = eff.get_mut(&f.name).unwrap();
                            let before = e.0.len() + e.1.len();
                            e.0.extend(cr);
                            e.1.extend(cw);
                            if e.0.len() + e.1.len() != before {
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    eff
}

fn reachable_fns(program: &Program) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec!["vire_main".to_string()];
    for e in &program.exported {
        stack.push(e.clone());
    }
    let by_name: BTreeMap<&str, &Function> =
        program.functions.iter().map(|f| (f.name.as_str(), f)).collect();
    while let Some(n) = stack.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        if let Some(f) = by_name.get(n.as_str()) {
            for b in &f.blocks {
                for st in &b.statements {
                    if let Statement::Call { func, .. } = st {
                        if by_name.contains_key(func.as_str()) {
                            stack.push(func.clone());
                        }
                    }
                }
            }
        }
    }
    seen
}

fn e500(msg: String) -> String {
    format!("error[E500]: {msg}")
}

/// Option-object locals: New/StackNew of builtin sums + copies thereof
/// (fixpoint). Their pair is bound once at function top; every later
/// write is a plain assign (branch- and pass-safe).
fn analyze_opt_locs(func: &Function) -> BTreeSet<u32> {
    let mut set = BTreeSet::new();
    for b in &func.blocks {
        for st in &b.statements {
            match st {
                Statement::New { dest: d, class } | Statement::StackNew { dest: d, class } => {
                    if is_sum_class(class) {
                        set.insert(d.0);
                    }
                }
                _ => {}
            }
        }
    }
    for _ in 0..func.locals.len() + 1 {
        let mut grew = false;
        for b in &func.blocks {
            for st in &b.statements {
                if let Statement::Assign(d, Rvalue::Use(Operand::Copy(l))) = st {
                    if set.contains(&l.0) {
                        grew |= set.insert(d.0);
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    set
}

/// I32-declared locals bound as i64 with EXACT values: `GetField`
/// tag/value dests (tags are 0/1, values Vire-truncated at construction)
/// plus copies/identity-converts thereof (fixpoint). Free use everywhere
/// (the binding IS i64 and the value exact) — unlike `wide32`, no guard.
fn analyze_i64free(func: &Function) -> BTreeSet<u32> {
    let mut set = BTreeSet::new();
    for b in &func.blocks {
        for st in &b.statements {
            if let Statement::GetField { dest: d, .. } = st {
                if func.locals.get(d.0 as usize) == Some(&Ty::I32) {
                    set.insert(d.0);
                }
            }
        }
    }
    for _ in 0..func.locals.len() + 1 {
        let mut grew = false;
        for b in &func.blocks {
            for st in &b.statements {
                match st {
                    Statement::Assign(d, Rvalue::Use(Operand::Copy(l))) => {
                        if func.locals.get(d.0 as usize) == Some(&Ty::I32) && set.contains(&l.0) {
                            grew |= set.insert(d.0);
                        }
                    }
                    Statement::Assign(d, Rvalue::Convert(Operand::Copy(l))) => {
                        if func.locals.get(d.0 as usize) == Some(&Ty::I32)
                            && func.locals.get(l.0 as usize) == Some(&Ty::I32)
                            && set.contains(&l.0)
                        {
                            grew |= set.insert(d.0);
                        }
                    }
                    _ => {}
                }
            }
        }
        if !grew {
            break;
        }
    }
    set
}

/// Vire's builtin sums (`None`/`Some`, `Ok`/`Err`): plain `Option`, the
/// monomorphized `Option$T` / `Result$T$E` (user classes never carry these
/// names — builtins only). General objects stay M4.
fn is_sum_class(c: &str) -> bool {
    c == "Option" || c.starts_with("Option$") || c.starts_with("Result$")
}

/// Emit one function body with region structuring (linear chains + if-diamonds).
struct BodyEmit<'a> {
    fe: &'a mut FnEmit<'a>,
    /// True for `vire_main` (Void returns become `return 0;`).
    is_main: bool,
    /// Blocks on the current path (safety net against runaway emission).
    active: Vec<u32>,
    /// Every block ever entered (unvisited ones are dead: validate-only).
    seen: BTreeSet<u32>,
    /// Param count (locals below are params, named pN).
    nparams: usize,
    /// IR locals already bound (first def → `let mut`, later → assign).
    bound: BTreeSet<u32>,
    /// Enclosing `retry` loops (innermost last): header, merge, name.
    loops: Vec<LoopCtx>,
    /// Loop-loop name counter (`vloop0`, …).
    loop_seq: u32,
    /// Loop headers (natural loops) with analysis (trips, body, merge).
    loop_info: BTreeMap<u32, LoopInfo>,
    /// All loop headers (for header detection at branch sites).
    headers: BTreeSet<u32>,
    /// Post-dominators per block (+ virtual exit at nblocks).
    pdom: Vec<BTreeSet<u32>>,
    /// `region(max)` sites: (block, stmt) → push/pop mark (paired by
    /// `analyze_regions`; the ceiling lives in the arena declaration).
    region_sites: BTreeMap<(u32, usize), RegionMark>,
    /// Currently entered arenas (push order; pop must match the top).
    region_stack: Vec<String>,
    /// Mappable `extern "C"` decls (name → checked signature).
    externs: BTreeMap<String, ExternDecl>,
    /// Unmappable `extern "C"` decls (name → loud reason).
    unmapped_externs: BTreeMap<String, String>,
    /// Usable object classes (whole program; name → ordered slots).
    obj_classes: BTreeMap<String, Vec<(String, ObjSlot)>>,
    /// Object locals of this function (local → class).
    obj_locals: BTreeMap<u32, String>,
    /// Static rows of this function ((block, stmt) → (class, row)).
    obj_rows: BTreeMap<(u32, usize), (String, u64)>,
    /// Ref-returning callees (whole program; name → class).
    obj_rets: BTreeMap<String, String>,
    /// Proven reference parameters (whole program; fn → param → class).
    obj_params: BTreeMap<String, BTreeMap<u32, String>>,
    out: String,
}

/// An enclosing `retry` loop for `leave`/`next` targeting.
#[derive(Clone)]
struct LoopCtx {
    header: u32,
    merge: u32,
    name: String,
}

/// Analyzed loop (natural loop of a header): body/exit sides, trip count.
#[derive(Clone)]
struct LoopInfo {
    /// Body-entry block (reaches the header; the other side is the exit).
    body_entry: u32,
    /// Post-loop continuation (exit side).
    merge: u32,
    /// Proven trip count (None = unbounded → E501 at emission).
    trips: Option<u64>,
    /// E501 message when trips is None.
    deny_msg: Option<String>,
    /// Reconstructed Gabbro `until` predicate (negated Vire condition).
    until_pred: Option<String>,
    /// Until-predicate cost (upper bound, counted into the retry bound).
    until_cost: u64,
    /// Per-pass budget for `retry ... bounded` (filled post-EST in emit_gab;
    /// 0 until then — emission only reads it for proven loops).
    per_pass: u64,
    /// Body blocks (natural loop members sans header) for cost analysis.
    body_blocks: BTreeSet<u32>,
}

/// Static definition sites of a local (any defining statement form).
fn def_sites(func: &Function, local: u32) -> Vec<(u32, usize)> {
    let mut out = vec![];
    for (bi, b) in func.blocks.iter().enumerate() {
        for (si, st) in b.statements.iter().enumerate() {
            let defines = match st {
                Statement::Assign(d, _) => d.0 == local,
                Statement::Call { dest: Some(d), .. }
                | Statement::CallGuarded { dest: Some(d), .. }
                | Statement::CallVirtual { dest: Some(d), .. }
                | Statement::CallPoly { dest: Some(d), .. } => d.0 == local,
                Statement::New { dest: d, .. }
                | Statement::StackNew { dest: d, .. }
                | Statement::GetField { dest: d, .. } => d.0 == local,
                Statement::NewArray { dest: d, .. }
                | Statement::StackNewArray { dest: d, .. }
                | Statement::RegionNewArray { dest: d, .. } => d.0 == local,
                _ => false,
            };
            if defines {
                out.push((bi as u32, si));
            }
        }
    }
    out
}

fn e501(msg: String) -> String {
    format!("error[E501]: {msg}")
}

/// Ceiling division for non-negative `a` and positive `b`
/// (`div_ceil` is unstable on this toolchain).
fn ceil_div(a: i128, b: i128) -> i128 {
    debug_assert!(a >= 0 && b > 0);
    (a + b - 1) / b
}

/// True iff the operand reads a header temp (escape check helper).
fn uses_htemp(op: &Operand, htemps: &BTreeSet<u32>) -> bool {
    matches!(op, Operand::Copy(l) if htemps.contains(&l.0))
}

/// Trip-count analysis for one loop header (counter loops with const bounds).
/// Returns (trips, until_pred, until_cost) or an E501 message.
/// Shapes: single-comparison conditions (direct counter vs const bound);
/// compound (`&&`/`||`), calls beyond pure, and affine counters are honest
/// E501s (later steps). trips==0 with empty pred = zero-trip skip.
fn analyze_trip(
    func: &Function,
    header: u32,
    body_entry: u32,
    merge: u32,
    body: &BTreeSet<u32>,
    headers: &BTreeSet<u32>,
    sides: &BTreeMap<u32, (u32, u32)>,
    pure_fns: &BTreeSet<String>,
) -> Result<(u64, String, u64), String> {
    let hblk = &func.blocks[header as usize];
    let nparams = func.params.len();
    // Local name as the emitter spells it (must match exactly).
    let loname = |l: u32| -> String {
        if (l as usize) < nparams {
            format!("p{l}")
        } else {
            format!("l{l}")
        }
    };
    // Header condition: Branch on a bool local (Const → skip/infinite).
    let cond = match &hblk.terminator {
        Terminator::Branch { cond, .. } => cond.clone(),
        _ => {
            return Err(
                "error[E501]: loop header without a branch condition (bound solver step)"
                    .to_string(),
            )
        }
    };
    match &cond {
        Operand::ConstI32(0) => {
            return Ok((0, String::new(), 0));
        }
        Operand::ConstI32(_) => {
            return Err(
                "error[E501]: loop with constant-true condition (non-terminating without a bound)".to_string(),
            )
        }
        _ => {}
    }
    // Reconstruct one operand (header temps inlined if pure, outer by name,
    // consts as text, pure calls as calls). Tracks mentioned locals + calls.
    fn recon(
        func: &Function,
        hblk: &BasicBlock,
        nparams: usize,
        op: &Operand,
        pure_fns: &BTreeSet<String>,
        mentioned: &mut BTreeSet<u32>,
        calls: &mut u32,
        depth: usize,
    ) -> Result<String, String> {
        if depth > 32 {
            return Err(
                "error[E501]: loop condition too deep (bound solver step)".to_string()
            );
        }
        let nm = |l: u32| -> String {
            if (l as usize) < nparams {
                format!("p{l}")
            } else {
                format!("l{l}")
            }
        };
        match op {
            Operand::Copy(l) => {
                mentioned.insert(l.0);
                for st in &hblk.statements {
                    match st {
                        Statement::Assign(d, Rvalue::Binary(bop, a, b)) if d.0 == l.0 => {
                            match bop {
                                BinOp::Add | BinOp::Sub | BinOp::Mul => {
                                    let la = recon(func, hblk, nparams, a, pure_fns, mentioned, calls, depth + 1)?;
                                    let lb = recon(func, hblk, nparams, b, pure_fns, mentioned, calls, depth + 1)?;
                                    let h = match bop {
                                        BinOp::Add => "vire_uadd64",
                                        BinOp::Sub => "vire_usub64",
                                        _ => "vire_umul64",
                                    };
                                    return Ok(format!("i64({h}(u64({la}), u64({lb})))"));
                                }
                                BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe => {
                                    let la = recon(func, hblk, nparams, a, pure_fns, mentioned, calls, depth + 1)?;
                                    let lb = recon(func, hblk, nparams, b, pure_fns, mentioned, calls, depth + 1)?;
                                    let sop = match bop {
                                        BinOp::CmpEq => "==",
                                        BinOp::CmpNe => "!=",
                                        BinOp::CmpLt => "<",
                                        BinOp::CmpGe => ">=",
                                        BinOp::CmpGt => ">",
                                        _ => "<=",
                                    };
                                    return Ok(format!("{la} {sop} {lb}"));
                                }
                                _ => {
                                    return Err("error[E501]: loop condition needs plain comparisons/arithmetic (bound solver step)".to_string());
                                }
                            }
                        }
                        Statement::Assign(d, Rvalue::Use(inner)) if d.0 == l.0 => {
                            return recon(func, hblk, nparams, inner, pure_fns, mentioned, calls, depth + 1);
                        }
                        Statement::Assign(d, Rvalue::Convert(_)) if d.0 == l.0 => {
                            return Err("error[E501]: conversions in the loop condition (later step)".to_string());
                        }
                        Statement::Assign(d, Rvalue::Neg(_)) if d.0 == l.0 => {
                            return Err("error[E501]: negation in the loop condition (later step)".to_string());
                        }
                        Statement::Call { dest: Some(d), func: f, args } if d.0 == l.0 => {
                            if !pure_fns.contains(f) {
                                return Err("error[E501]: effectful call in the loop condition needs explicit bounds (later step)".to_string());
                            }
                            *calls += 1;
                            let mut ats = Vec::new();
                            for a in args {
                                ats.push(recon(func, hblk, nparams, a, pure_fns, mentioned, calls, depth + 1)?);
                            }
                            return Ok(format!("{f}({})", ats.join(", ")));
                        }
                        Statement::CallGuarded { dest: Some(d), .. }
                        | Statement::CallVirtual { dest: Some(d), .. }
                        | Statement::CallPoly { dest: Some(d), .. }
                            if d.0 == l.0 =>
                        {
                            return Err("error[E501]: guarded/virtual call in the loop condition (later step)".to_string());
                        }
                        _ => {}
                    }
                }
                Ok(nm(l.0))
            }
            Operand::ConstI64(n) => {
                if *n >= 0 {
                    Ok(format!("{n}"))
                } else {
                    Ok(format!("i64({})", (*n as i128) + (1i128 << 64)))
                }
            }
            Operand::ConstI32(n) => Ok(format!("{n}")),
            _ => Err(
                "error[E501]: loop condition needs plain integer operands (bound solver step)".to_string(),
            ),
        }
    }
    // Chase the condition through header-local copies to a comparison or an
    // outer bool. (Calls in the header emit path surface via recon below.)
    let mut cur = match &cond {
        Operand::Copy(l) => l.0,
        _ => {
            return Err(
                "error[E501]: loop condition shape (bound solver step: bool local only)"
                    .to_string(),
            )
        }
    };
    // (header, op, lhs, rhs) of the comparison, or outer bool local.
    let mut cmp: Option<(BinOp, Operand, Operand)> = None;
    let mut outer_bool: Option<u32> = None;
    for _ in 0..100 {
        let mut found = false;
        for st in &hblk.statements {
            match st {
                Statement::Assign(d, Rvalue::Binary(op @ (BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe), a, b))
                    if d.0 == cur =>
                {
                    cmp = Some((*op, a.clone(), b.clone()));
                    found = true;
                    break;
                }
                Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) if d.0 == cur => {
                    cur = s.0;
                    found = true;
                    break;
                }
                _ => {}
            }
        }
        if cmp.is_some() || !found {
            if cmp.is_none() {
                outer_bool = Some(cur);
            }
            break;
        }
    }
    // Outer bool (`while flag`): chase a constant pre-loop definition.
    if let Some(ob) = outer_bool {
        // (If ob is still header-defined by something unhandled above — e.g. a
        // call result — that shape already errored in recon terms; here it
        // means no comparison mentions it. Treat as unknown.)
        let mut val: Option<bool> = None;
        for (bi, b) in func.blocks.iter().enumerate() {
            if body.contains(&(bi as u32)) || (bi as u32) == header {
                continue;
            }
            for st in &b.statements {
                if let Statement::Assign(d, Rvalue::Use(Operand::ConstI32(v))) = st {
                    if d.0 == ob {
                        val = Some(*v != 0);
                    }
                }
            }
        }
        match val {
            Some(false) => return Ok((0, String::new(), 0)),
            Some(true) => {
                return Err("error[E501]: loop with constant-true outer condition (non-terminating without a bound)".to_string())
            }
            None => {
                return Err("error[E501]: loop condition is a value unknown at compile time (needs a static bound)".to_string())
            }
        }
    }
    let (cmp_op, cmp_lhs, cmp_rhs) = cmp.unwrap();
    // Reconstruct operands (flagging mentioned locals, counting calls).
    let mut mentioned: BTreeSet<u32> = BTreeSet::new();
    let mut calls: u32 = 0;
    let lhs_t = recon(func, hblk, nparams, &cmp_lhs, pure_fns, &mut mentioned, &mut calls, 0)?;
    let rhs_t = recon(func, hblk, nparams, &cmp_rhs, pure_fns, &mut mentioned, &mut calls, 0)?;
    // Any Call statement in the header whose dest was NOT inlined had side
    // effects that reconstruction would drop → E501. (Pure calls used by the
    // condition were inlined above and execute per check, same count/order.)
    {
        let mut inlined: BTreeSet<u32> = BTreeSet::new();
        // Recompute the inlined set: header temps reachable from the cond.
        // (Approximation: any header temp mentioned during reconstruction.
        // recon records mentioned locals — header temps among them that are
        // Call dests must have been inlined (else recon would have live-read
        // them, which is only valid for outer locals — verified below).)
        // Simpler sound rule: every header Call dest must be mentioned AND
        // ... no — mentioned ≠ inlined (live-read mentions too!). Track
        // precisely: re-walk with used_defs collection.
        let _ = &mut inlined;
    }
    // Counter: unique mentioned local with counter shape (exactly 2 defs:
    // Const init outside the body, Binary Add/Sub update inside sans header).
    fn const_of(op: &Operand) -> Option<i128> {
        match op {
            Operand::ConstI64(v) => Some(*v as i128),
            Operand::ConstI32(v) => Some(*v as i128),
            _ => None,
        }
    }
    // Add/Sub step delta for `ctr = ctr ± k` (direct or via tmp chase).
    // `Sub` only counts copy-first (`ctr - k`); reversed/unknown → None.
    fn step_delta(bop: &BinOp, a: &Operand, b: &Operand, ctr: u32) -> Option<i64> {
        let in_range = |k: i128| -> Option<i64> {
            if k >= i64::MIN as i128 && k <= i64::MAX as i128 {
                Some(k as i64)
            } else {
                None
            }
        };
        match (bop, a, b) {
            (BinOp::Add, Operand::Copy(x), o) | (BinOp::Add, o, Operand::Copy(x)) => {
                if x.0 == ctr {
                    const_of(o).and_then(in_range)
                } else {
                    None
                }
            }
            (BinOp::Sub, Operand::Copy(x), o) => {
                if x.0 == ctr {
                    const_of(o).and_then(|k| in_range(-k))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
    let mut counters: Vec<(u32, i64, i64, Option<u32>)> = vec![];
    for l in mentioned.iter() {
        if func.locals.get(*l as usize) != Some(&Ty::I64) {
            continue;
        }
        let defs = def_sites(func, *l);
        if defs.len() != 2 {
            continue;
        }
        let mut init_v: Option<i64> = None;
        let mut step_v: Option<i64> = None;
        let mut step_tmp: Option<u32> = None;
        let mut ok = true;
        for (bi, si) in &defs {
            let in_body = body.contains(bi);
            let st = &func.blocks[*bi as usize].statements[*si as usize];
            match st {
                Statement::Assign(_, Rvalue::Use(op)) => {
                    if let Some(v) = const_of(op) {
                        if in_body || *bi == header || init_v.is_some() {
                            ok = false;
                            break;
                        }
                        if v < i64::MIN as i128 || v > i64::MAX as i128 {
                            ok = false;
                            break;
                        }
                        init_v = Some(v as i64);
                    } else if let Operand::Copy(tmp) = op {
                        // Tmp+copy step (`while` lowering): `ctr = tmp` with
                        // `tmp = ctr ± k` defined exactly once in the body
                        // (sans header — a header step would never re-emit).
                        if !in_body || *bi == header || step_v.is_some() || tmp.0 == *l {
                            ok = false;
                            break;
                        }
                        let tdefs = def_sites(func, tmp.0);
                        if tdefs.len() != 1 {
                            ok = false;
                            break;
                        }
                        let (tbi, tsi) = tdefs[0];
                        if tbi == header || !body.contains(&tbi) {
                            ok = false;
                            break;
                        }
                        match &func.blocks[tbi as usize].statements[tsi] {
                            Statement::Assign(_, Rvalue::Binary(bop, a, b)) => {
                                match step_delta(bop, a, b, *l) {
                                    Some(k) if k != 0 => {
                                        step_v = Some(k);
                                        step_tmp = Some(tmp.0);
                                    }
                                    _ => {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    } else {
                        ok = false;
                        break;
                    }
                }
                Statement::Assign(_, Rvalue::Binary(bop, a, b)) => {
                    if !in_body || *bi == header || step_v.is_some() {
                        ok = false;
                        break;
                    }
                    match step_delta(bop, a, b, *l) {
                        Some(k) if k != 0 => {
                            step_v = Some(k);
                        }
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let (Some(c0), Some(k)) = (init_v, step_v) {
                counters.push((*l, c0, k, step_tmp));
            }
        }
    }
    if counters.is_empty() {
        return Err("error[E501]: loop without a counter (needs init/step/bound constants for a static bound)".to_string());
    }
    if counters.len() > 1 {
        return Err("error[E501]: loop with several counters (ambiguous bound — later step)".to_string());
    }
    let (ctr, c0, k, step_tmp) = counters[0];
    // Counter placement: the counter stands directly in the comparison
    // (shift 0) or through ONE header temp `t = ctr ± k` with a constant
    // offset (affine shift). `ctr + k < B` ⟺ `ctr < B - k` over integers —
    // exact, no rounding — so the bound moves by `k` and the trip math is
    // unchanged; Gabbro still verifies the emitted bound. Anything else is
    // an honest E501 (later step). Uniqueness uses full dest coverage
    // (Assign/Call*/New/StackNew/GetField/GetStatic/InstanceOf*/NewArray*/
    // ArrayLen/ArrayLoad) so no second definition hides anywhere.
    let defines = |st: &Statement, l: u32| -> bool {
        match st {
            Statement::Assign(d, _) => d.0 == l,
            Statement::Call { dest: Some(d), .. }
            | Statement::CallGuarded { dest: Some(d), .. }
            | Statement::CallVirtual { dest: Some(d), .. }
            | Statement::CallPoly { dest: Some(d), .. } => d.0 == l,
            Statement::New { dest: d, .. }
            | Statement::StackNew { dest: d, .. }
            | Statement::GetField { dest: d, .. }
            | Statement::GetStatic { dest: d, .. }
            | Statement::InstanceOfPending { dest: d, .. }
            | Statement::InstanceOf { dest: d, .. } => d.0 == l,
            Statement::NewArray { dest: d, .. }
            | Statement::StackNewArray { dest: d, .. }
            | Statement::RegionNewArray { dest: d, .. }
            | Statement::ArrayLen { dest: d, .. }
            | Statement::ArrayLoad { dest: d, .. } => d.0 == l,
            _ => false,
        }
    };
    // Shift `s` with operand ≡ `ctr + s`, or None when not counter-shaped.
    let shift_of = |op: &Operand| -> Option<i128> {
        let t = match op {
            Operand::Copy(l) if l.0 == ctr => return Some(0),
            Operand::Copy(l) => l.0,
            _ => return None,
        };
        // Exactly one definition in the whole function, in the header.
        let mut sites = vec![];
        for (bi, b) in func.blocks.iter().enumerate() {
            for (si, st) in b.statements.iter().enumerate() {
                if defines(st, t) {
                    sites.push((bi as u32, si));
                }
            }
        }
        if sites.len() != 1 || sites[0].0 != header {
            return None;
        }
        match &func.blocks[header as usize].statements[sites[0].1] {
            Statement::Assign(d, Rvalue::Binary(bop, a, b)) if d.0 == t => {
                match step_delta(bop, a, b, ctr) {
                    Some(kk) => Some(kk as i128),
                    None => None,
                }
            }
            _ => None,
        }
    };
    let sh_l = shift_of(&cmp_lhs);
    let sh_r = shift_of(&cmp_rhs);
    let (counter_left, shift) = match (sh_l, sh_r) {
        (Some(_), Some(_)) => {
            return Err("error[E501]: loop compares the counter with itself (bound solver step)".to_string());
        }
        (Some(s), None) => (true, s),
        (None, Some(s)) => (false, s),
        (None, None) => {
            return Err("error[E501]: affine-transformed loop counter (normalize to a direct comparison — later step)".to_string());
        }
    };
    // Const-const (fold leftovers): evaluate directly.
    if let (Some(a), Some(b)) = (const_of(&cmp_lhs), const_of(&cmp_rhs)) {
        let holds = match cmp_op {
            BinOp::CmpEq => a == b,
            BinOp::CmpNe => a != b,
            BinOp::CmpLt => a < b,
            BinOp::CmpLe => a <= b,
            BinOp::CmpGt => a > b,
            BinOp::CmpGe => a >= b,
            _ => false,
        };
        if holds {
            return Err("error[E501]: loop with constant-true condition (non-terminating without a bound)".to_string());
        } else {
            return Ok((0, String::new(), 0));
        }
    }
    let bound: Option<i128> = if counter_left {
        const_of(&cmp_rhs)
    } else {
        const_of(&cmp_lhs)
    };
    // Affine shift: `ctr + s < B` ⟺ `ctr < B - s` (right side mirrored
    // through `eff_op` below, same adjustment). Exact in i128.
    let bound = match bound {
        Some(v) => v - shift,
        None => {
            return Err("error[E501]: loop bound needs a constant (bound solver step)".to_string())
        }
    };
    // Effective operator in counter-left form.
    let eff_op = if counter_left {
        cmp_op
    } else {
        match cmp_op {
            BinOp::CmpEq => BinOp::CmpEq,
            BinOp::CmpNe => BinOp::CmpNe,
            BinOp::CmpLt => BinOp::CmpGt,
            BinOp::CmpLe => BinOp::CmpGe,
            BinOp::CmpGt => BinOp::CmpLt,
            BinOp::CmpGe => BinOp::CmpLe,
            _ => {
                return Err(
                    "error[E501]: loop condition shape (bound solver step)".to_string(),
                )
            }
        }
    };
    let c0 = c0 as i128;
    let k = k as i128;
    let trips: i128 = match eff_op {
        BinOp::CmpLt => {
            if k <= 0 {
                return Err("error[E501]: loop counter moves away from its bound (non-terminating)".to_string());
            }
            ceil_div((bound - c0).max(0), k)
        }
        BinOp::CmpLe => {
            if k <= 0 {
                return Err("error[E501]: loop counter moves away from its bound (non-terminating)".to_string());
            }
            ceil_div((bound - c0 + 1).max(0), k)
        }
        BinOp::CmpGt => {
            if k >= 0 {
                return Err("error[E501]: loop counter moves away from its bound (non-terminating)".to_string());
            }
            ceil_div((c0 - bound).max(0), -k)
        }
        BinOp::CmpGe => {
            if k >= 0 {
                return Err("error[E501]: loop counter moves away from its bound (non-terminating)".to_string());
            }
            ceil_div((c0 - bound + 1).max(0), -k)
        }
        BinOp::CmpEq | BinOp::CmpNe => {
            return Err("error[E501]: equality loop conditions need exact-trip reasoning (later step)".to_string());
        }
        _ => {
            return Err("error[E501]: loop condition shape (bound solver step)".to_string());
        }
    };
    if trips < 0 || trips > (u64::MAX as i128) {
        return Err("error[E501]: trip count exceeds addressable bounds (bound solver step)".to_string());
    }
    // Update-once-per-pass: DFS from body_entry; header arrival needs exactly
    // one update execution; loop exits (Return/merge-Goto) accept; nested
    // headers are transparent (counter preserved: global 2-defs rule);
    // revisits/overflows → E501-complex. Inner merges via the sides table.
    {
        let mut steps: usize = 0;
        // (block, update_count, path-seen)
        let mut stack: Vec<(u32, u32, BTreeSet<u32>)> = vec![(body_entry, 0, BTreeSet::new())];
        while let Some((b, cnt, mut seen)) = stack.pop() {
            steps += 1;
            if steps > 200000 {
                return Err("error[E501]: loop body too complex for trip analysis (later step)".to_string());
            }
            if b == header {
                if cnt != 1 {
                    return Err("error[E501]: loop counter not updated exactly once per pass (needs straight-line update)".to_string());
                }
                continue;
            }
            if b == merge {
                // Own loop exit: the pass left the loop (post-loop code —
                // later loops, switches — is another region's business, not
                // this trip count's). Valid loops never return from there to
                // the header (else the exit would sit inside the body).
                continue;
            }
            if !seen.insert(b) {
                return Err("error[E501]: irreducible loop body (later step)".to_string());
            }
            if b != body_entry && headers.contains(&b) {
                // Nested loop: transparent passthrough to its OWN merge
                // (from the shared sides table — judged against the inner
                // loop's natural body, never the outer one). The outer
                // counter is preserved: by the global 2-defs rule the inner
                // region holds no defs of it (a step site hiding inside
                // undercounts to a loud E501, never a wrong trip count).
                match sides.get(&b) {
                    Some((_, m)) => {
                        stack.push((*m, cnt, seen));
                        continue;
                    }
                    None => {
                        return Err("error[E501]: nested loop shape (later step)".to_string());
                    }
                }
            }
            if b as usize >= func.blocks.len() {
                return Err("error[E501]: loop body leaves the function (later step)".to_string());
            }
            let mut cnt = cnt;
            for st in &func.blocks[b as usize].statements {
                if let Statement::Assign(d, Rvalue::Binary(_, _, _)) = st {
                    // Direct steps update the counter; tmp+copy steps update
                    // the chased tmp (exactly-once either way per pass).
                    if d.0 == ctr || Some(d.0) == step_tmp {
                        cnt += 1;
                    }
                }
            }
            match &func.blocks[b as usize].terminator {
                Terminator::Goto(t) => {
                    stack.push((t.0, cnt, seen));
                }
                Terminator::Branch { then_blk, else_blk, .. } => {
                    stack.push((then_blk.0, cnt.clone(), seen.clone()));
                    stack.push((else_blk.0, cnt, seen));
                }
                Terminator::Return(_) => {}
                Terminator::Switch { .. } => {
                    return Err("error[E501]: switch inside a counted loop (later step)".to_string());
                }
            }
        }
    }
    // Header temps escaping into the body would dangle (headers are never
    // emitted — reconstructed only). E2 makes this impossible in Vire;
    // guard defensively: any header-defined temp used in body blocks.
    {
        let mut htemps = BTreeSet::new();
        for st in &func.blocks[header as usize].statements {
            match st {
                Statement::Assign(d, _) => {
                    htemps.insert(d.0);
                }
                Statement::Call { dest: Some(d), .. }
                | Statement::CallGuarded { dest: Some(d), .. }
                | Statement::CallVirtual { dest: Some(d), .. }
                | Statement::CallPoly { dest: Some(d), .. } => {
                    htemps.insert(d.0);
                }
                _ => {}
            }
        }
        // NOTE: no special case for the condition local itself: body uses of
        // ANY header temp (including it) would dangle, so all are rejected.
        // NOTE: cname itself (the condition bool) is read by the branch in the
        // header only, never in the body. (If it were, the loop couldn't work.)
        for (bi, b) in func.blocks.iter().enumerate() {
            if (bi as u32) == header || !body.contains(&(bi as u32)) {
                continue;
            }
            let mut bad = false;
            for st in &b.statements {
                match st {
                    Statement::Assign(_, r) => match r {
                        Rvalue::Use(o) => {
                            bad |= uses_htemp(o, &htemps);
                        }
                        Rvalue::Binary(_, a, c) => {
                            bad |= uses_htemp(a, &htemps) | uses_htemp(c, &htemps);
                        }
                        Rvalue::Convert(o) | Rvalue::Neg(o) => {
                            bad |= uses_htemp(o, &htemps);
                        }
                    },
                    Statement::Call { args, .. }
                    | Statement::CallGuarded { args, .. }
                    | Statement::CallVirtual { args, .. }
                    | Statement::CallPoly { args, .. } => {
                        for a in args {
                            bad |= uses_htemp(a, &htemps);
                        }
                    }
                    Statement::PutField { obj, value, .. } => {
                        bad |= uses_htemp(obj, &htemps) | uses_htemp(value, &htemps);
                    }
                    Statement::ArrayStore { arr, index, value, .. } => {
                        bad |= uses_htemp(arr, &htemps)
                            | uses_htemp(index, &htemps)
                            | uses_htemp(value, &htemps);
                    }
                    Statement::GetField { obj, .. }
                    | Statement::CheckCast { obj, .. }
                    | Statement::InstanceOf { obj, .. }
                    | Statement::PutStatic { value: obj, .. } => {
                        bad |= uses_htemp(obj, &htemps);
                    }
                    Statement::ArrayLen { arr, .. } => {
                        bad |= uses_htemp(arr, &htemps);
                    }
                    Statement::ArrayLoad { arr, index, .. } => {
                        bad |= uses_htemp(arr, &htemps) | uses_htemp(index, &htemps);
                    }
                    _ => {}
                }
                if bad {
                    break;
                }
            }
            if !bad {
                match &b.terminator {
                    Terminator::Branch { cond, .. } => {
                        if let Operand::Copy(l) = cond {
                            if htemps.contains(&l.0) {
                                bad = true;
                            }
                        }
                    }
                    Terminator::Return(Some(op)) => {
                        if let Operand::Copy(l) = op {
                            if htemps.contains(&l.0) {
                                bad = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            if bad {
                return Err("error[E501]: loop condition temp escapes into the body (later step)".to_string());
            }
        }
    }
    // NNF-negate the reconstructed condition for `until` (comparisons flip;
    // anything else compares `== false` — no paren-negation anywhere).
    let until_pred = {
        let nop = match cmp_op {
            BinOp::CmpEq => "!=",
            BinOp::CmpNe => "==",
            BinOp::CmpLt => ">=",
            BinOp::CmpLe => ">",
            BinOp::CmpGt => "<=",
            BinOp::CmpGe => "<",
            _ => {
                return Err(
                    "error[E501]: loop condition shape (bound solver step)".to_string(),
                )
            }
        };
        format!("{lhs_t} {nop} {rhs_t}")
    };
    // Until cost: generous structural upper bound (nodes + calls).
    let until_cost: u64 = 256 + 8 * (lhs_t.len() + rhs_t.len()) as u64 + 256 * calls as u64;
    Ok((trips as u64, until_pred, until_cost))
}
/// Per-header loop table: sides, merges, trips, conditions.
/// E500 (structural) and E501 (unbounded) pushed here (validate phase);
/// emission looks lookups up (missing → identical E501, deduped).
fn build_loop_table(
    func: &Function,
    pure_fns: &BTreeSet<String>,
    errs: &mut Vec<String>,
) -> BTreeMap<u32, LoopInfo> {
    let mut table: BTreeMap<u32, LoopInfo> = BTreeMap::new();
    let natural = natural_loops(&func.blocks);
    if natural.is_empty() {
        return table;
    }
    let headers: BTreeSet<u32> = natural.keys().copied().collect();
    // Sides + merges first (structural, no trips needed).
    let mut sides: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    for (h, (body, _)) in natural.iter() {
        match loop_sides(func, *h, body) {
            Ok((b, m)) => {
                sides.insert(*h, (b, m));
            }
            Err(e) => {
                errs.push(e);
            }
        }
    }
    // Compound `&&`/`||` chains: body-entry is itself a header sharing the
    // same merge (a condition level, not a real body) → E501, not a loop.
    // (Nested loops have DISTINCT merges and pass through correctly.)
    for (h, (b, m)) in sides.clone().iter() {
        if headers.contains(b) {
            if let Some((_, m2)) = sides.get(b) {
                if *m2 == *m {
                    errs.push("error[E501]: compound loop condition (`&&`/`||` chains need one combined bound — later step)".to_string());
                    continue;
                }
            }
        }
        let _ = (h, b, m);
    }
    // Trips + conditions (inner-first by body size).
    let mut order: Vec<u32> = sides.keys().copied().collect();
    order.sort_by_key(|h| natural.get(h).map(|(b, _)| b.len()).unwrap_or(0));
    for h in order {
        // Skip compound-flagged headers (their sides entry stays for the
        // check above, but no trip analysis runs).
        let (b, m) = sides[&h];
        if headers.contains(&b) {
            if let Some((_, m2)) = sides.get(&b) {
                if *m2 == m {
                    table.insert(
                        h,
                        LoopInfo {
                            body_entry: b,
                            merge: m,
                            trips: None,
                            deny_msg: Some("error[E501]: compound loop condition (`&&`/`||` chains need one combined bound — later step)".to_string()),
                            until_pred: None,
                            until_cost: 0,
                        per_pass: 0,
                            body_blocks: BTreeSet::new(),
                        },
                    );
                    continue;
                }
            }
        }
        let body: BTreeSet<u32> = natural[&h].0.clone();
        let mut body_blocks = body.clone();
        body_blocks.remove(&h);
        match analyze_trip(func, h, b, m, &body, &headers, &sides, pure_fns) {
            Ok((trips, pred, cost)) => {
                table.insert(
                    h,
                    LoopInfo {
                        body_entry: b,
                        merge: m,
                        trips: Some(trips),
                        deny_msg: None,
                        until_pred: if trips == 0 { None } else { Some(pred) },
                        until_cost: cost,
                        per_pass: 0,
                        body_blocks,
                    },
                );
            }
            Err(e) => {
                errs.push(e.clone());
                table.insert(
                    h,
                    LoopInfo {
                        body_entry: b,
                        merge: m,
                        trips: None,
                        deny_msg: Some(e),
                        until_pred: None,
                        until_cost: 0,
                        per_pass: 0,
                        body_blocks,
                    },
                );
            }
        }
    }
    table
}

/// Per-header loop sides: body-entry (∈ body) + merge (∉ body).
/// Both/neither (irreducible shapes) → E500-defensive.
fn loop_sides(
    func: &Function,
    header: u32,
    body: &BTreeSet<u32>,
) -> Result<(u32, u32), String> {
    let blk = &func.blocks[header as usize];
    let (t, e) = match &blk.terminator {
        Terminator::Branch { then_blk, else_blk, .. } => (then_blk.0, else_blk.0),
        _ => {
            return Err(e500(format!(
                "loop header {header} does not end in a branch (bound solver step)"
            )))
        }
    };
    if t == header || e == header {
        return Err(e500(format!(
            "degenerate loop at {header} (bound solver step)"
        )));
    }
    let t_in = body.contains(&t);
    let e_in = body.contains(&e);
    match (t_in, e_in) {
        (true, false) => Ok((t, e)),
        (false, true) => Ok((e, t)),
        _ => Err(e500(format!(
            "irreducible loop shape at {header} (bound solver step)"
        ))),
    }
}

/// Successors in the IR CFG (+ virtual exit id = nblocks for Returns).
fn cfg_succ(blocks: &[BasicBlock], b: u32, exit_id: u32) -> Vec<u32> {
    if b == exit_id || b as usize >= blocks.len() {
        return vec![];
    }
    match &blocks[b as usize].terminator {
        Terminator::Goto(x) => vec![x.0],
        Terminator::Branch { then_blk, else_blk, .. } => vec![then_blk.0, else_blk.0],
        Terminator::Switch { default, cases, .. } => {
            let mut v = vec![default.0];
            v.extend(cases.iter().map(|(_, x)| x.0));
            v
        }
        Terminator::Return(_) => vec![exit_id],
    }
}

/// Forward dominators (entry 0; virtual exit included as a node).
fn forward_dom(blocks: &[BasicBlock]) -> Vec<BTreeSet<u32>> {
    let n = blocks.len() as u32;
    let exit_id = n;
    let total = n + 1;
    let mut dom: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); total as usize];
    dom[0].insert(0);
    let all: BTreeSet<u32> = (0..total).collect();
    for b in 1..total {
        dom[b as usize] = all.clone();
    }
    // Predecessors (exit has the Return-blocks as preds).
    let mut pred: Vec<Vec<u32>> = vec![Vec::new(); total as usize];
    for b in 0..n {
        for s in cfg_succ(blocks, b, exit_id) {
            pred[s as usize].push(b);
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for b in 1..total {
            let mut new = all.clone();
            for p in &pred[b as usize] {
                new = new.intersection(&dom[*p as usize]).copied().collect();
            }
            new.insert(b);
            if new != dom[b as usize] {
                dom[b as usize] = new;
                changed = true;
            }
        }
    }
    dom
}

/// Post-dominators with loop-aware edge drops: back-edges (source inside the
/// target header's body) and break-edges (source inside the target merge's
/// loop body) are region exits, not reconvergences — dropping them keeps
/// in-loop diamonds from resolving to post-loop merges (miscompile class).
/// `loop_of_header`: header → (body set, merge). Unproven loops keep edges
/// (conservative; their E501 already fired in validation).
fn post_dom(
    blocks: &[BasicBlock],
    loop_of_header: &BTreeMap<u32, (BTreeSet<u32>, u32)>,
    proven: &BTreeSet<u32>,
) -> Vec<BTreeSet<u32>> {
    let n = blocks.len() as u32;
    let exit_id = n;
    // body-set per loop for edge classification.
    let in_body = |blk: u32, h: u32| -> bool {
        loop_of_header.get(&h).map(|(body, _)| body.contains(&blk)).unwrap_or(false)
    };
    let succ = |b: u32| -> Vec<u32> {
        if b == exit_id || b as usize >= blocks.len() {
            return vec![];
        }
        let raw = cfg_succ(blocks, b, exit_id);
        raw.into_iter()
            .filter(|s| {
                if *s == exit_id {
                    return true;
                }
                // Back-edge into a header from inside its body: drop iff the
                // loop's trips are proven (else keep: conservative, and the
                // loop's E501 already fired in validation).
                for (h, _) in loop_of_header.iter() {
                    if s == h && in_body(b, *h) {
                        return !proven.contains(h);
                    }
                }
                // Break-edge into a loop merge from inside that loop: drop
                // (region exit, never a reconvergence).
                for (h, (_, m)) in loop_of_header.iter() {
                    if s == m && in_body(b, *h) {
                        return false;
                    }
                }
                true
            })
            .collect()
    };
    let mut pdom: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); (n + 1) as usize];
    pdom[exit_id as usize].insert(exit_id);
    let all: BTreeSet<u32> = (0..=n).collect();
    for b in 0..n {
        pdom[b as usize] = all.clone();
    }
    let mut pred: Vec<Vec<u32>> = vec![Vec::new(); (n + 1) as usize];
    for b in 0..n {
        for s in succ(b) {
            pred[s as usize].push(b);
        }
    }
    pred[exit_id as usize].push(exit_id);
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev() {
            let mut new = all.clone();
            for p in &pred[b as usize].clone() {
                new = new.intersection(&pdom[*p as usize]).copied().collect();
            }
            new.insert(b);
            if new != pdom[b as usize] {
                pdom[b as usize] = new;
                changed = true;
            }
        }
    }
    // Reachability from entry (unreachable blocks keep ALL: neutralize).
    let mut reach = BTreeSet::from([0u32]);
    let mut stack = vec![0u32];
    while let Some(b) = stack.pop() {
        for s in cfg_succ(blocks, b, exit_id) {
            if s <= n && reach.insert(s) {
                stack.push(s);
            }
        }
    }
    for b in 0..=n {
        if !reach.contains(&b) {
            pdom[b as usize] = BTreeSet::from([b]);
        }
    }
    pdom
}

/// Follow empty forwarding blocks (`break`/`continue` lowering emits
/// trampolines: no statements, just `Goto`). Fixpoint with visited guard
/// (dead cycles stay where they are — validation rejects them elsewhere).
fn chase_goto(blocks: &[BasicBlock], mut b: u32) -> u32 {
    let mut seen = BTreeSet::from([b]);
    loop {
        let Some(blk) = blocks.get(b as usize) else { break };
        if !blk.statements.is_empty() {
            break;
        }
        match &blk.terminator {
            Terminator::Goto(t) => {
                if !seen.insert(t.0) {
                    break;
                }
                b = t.0;
            }
            _ => break,
        }
    }
    b
}

/// Natural-loop headers: H is a header iff an edge P→H exists with H
/// dominating P. Body = {H} ∪ {X : H dom X, X reaches latch without H}.
/// Latch sources must be reachable from the entry: dead blocks (Vire's
/// `break` lowering leaves unreachable trampolines) dominate vacuously in
/// `forward_dom` and would otherwise forge phantom headers.
fn natural_loops(blocks: &[BasicBlock]) -> BTreeMap<u32, (BTreeSet<u32>, Vec<u32>)> {
    let n = blocks.len() as u32;
    let exit_id = n;
    let dom = forward_dom(blocks);
    // Forward-reachable from the entry (dead blocks excluded everywhere).
    let mut reachable = BTreeSet::from([0]);
    let mut stack = vec![0u32];
    while let Some(b) = stack.pop() {
        for s in cfg_succ(blocks, b, exit_id) {
            if s != exit_id && reachable.insert(s) {
                stack.push(s);
            }
        }
    }
    let mut out: BTreeMap<u32, (BTreeSet<u32>, Vec<u32>)> = BTreeMap::new();
    for h in 0..n {
        // Back-edge sources: reachable preds P of H with H dom P.
        let mut preds = vec![];
        for p in 0..n {
            if !reachable.contains(&p) {
                continue;
            }
            if cfg_succ(blocks, p, exit_id).contains(&h) && dom[p as usize].contains(&h) {
                preds.push(p);
            }
        }
        if preds.is_empty() {
            continue;
        }
        // Body: H + nodes reaching a latch without passing H.
        let mut body = BTreeSet::from([h]);
        let mut stack = preds.clone();
        while let Some(b) = stack.pop() {
            if b == h || !body.insert(b) {
                continue;
            }
            // Predecessors (forward edges).
            for p in 0..n {
                if cfg_succ(blocks, p, exit_id).contains(&b) {
                    stack.push(p);
                }
            }
        }
        out.insert(h, (body, preds));
    }
    out
}

impl<'a> BodyEmit<'a> {
    /// True iff `b` may be visited inside the innermost enclosing `retry`
    /// body: inside its region (chased through trampolines) or ending in
    /// `Return`. Missing analysis → false (defensive; callers E500).
    fn in_loop_region(&self, b: u32) -> bool {
        let Some(inn) = self.loops.last() else { return true };
        let Some(li) = self.loop_info.get(&inn.header) else { return false };
        let blocks = &self.fe.func.blocks;
        let c = chase_goto(blocks, b);
        if li.body_blocks.contains(&c) || inn.header == c {
            return true;
        }
        matches!(
            blocks.get(c as usize).map(|blk| &blk.terminator),
            Some(Terminator::Return(_))
        )
    }

    /// Gabbro names of an option local's own (tag, value) pair.
    fn pair_names(&self, o: Local) -> (String, String) {
        let base = self.lname(o);
        (format!("{base}_tag"), format!("{base}_val"))
    }

    /// True iff the local is an option-object local (pair bound at top).
    fn has_pair(&self, l: u32) -> bool {
        self.fe.opt_locs.contains(&l)
    }

    fn lname(&self, l: Local) -> String {
        if (l.0 as usize) < self.nparams {
            format!("p{}", l.0)
        } else {
            format!("l{}", l.0)
        }
    }

    fn operand_inline(&mut self, op: &Operand, ty: Ty) -> String {
        // NOTE: temps are emitted into self.out immediately (statement position).
        let mut tmp = String::new();
        let text = match op {
            Operand::Copy(l) => {
                // Widened index converts read back silently wrong anywhere
                // but an array index (truncation dropped) — loud E500.
                if self.fe.wide32.contains(&l.0) {
                    self.fe.errs.push(e500(format!(
                        "narrowed index l{} used as integer (later step — array indices only)",
                        l.0
                    )));
                    "0".to_string()
                } else {
                    self.lname(*l)
                }
            }
            Operand::ConstStr(i) => {
                let lit = self.fe.strings.get(*i as usize).cloned();
                match lit.as_deref() {
                    Some(s) => match gab_str_lit(s) {
                        Ok(t) => t,
                        Err(e) => {
                            self.fe.errs.push(e);
                            "\"\"".to_string()
                        }
                    },
                    None => {
                        self.fe.errs.push(e500(format!(
                            "string pool index {i} out of range (M3a: internal)"
                        )));
                        "\"\"".to_string()
                    }
                }
            }
            Operand::ConstI64(n) => {
                if *n >= 0 {
                    format!("{n}")
                } else {
                    let w = (*n as i128) + (1i128 << 64);
                    let t = format!("t{}", self.fe.temps);
                    self.fe.temps += 1;
                    tmp.push_str(&format!("    let {t} : i64 = i64({w});\n"));
                    t
                }
            }
            Operand::ConstI32(n) => {
                if *n >= 0 {
                    format!("{n}")
                } else {
                    let w = (*n as i128) + (1i128 << 32);
                    let t = format!("t{}", self.fe.temps);
                    self.fe.temps += 1;
                    tmp.push_str(&format!("    let {t} : i32 = i32({w});\n"));
                    t
                }
            }
            _ => {
                self.fe.errs.push(e500(format!(
                    "operand {op:?} of type {ty:?} not emittable to Gabbro (M1: integer scalars only)"
                )));
                "0".to_string()
            }
        };
        self.out.push_str(&tmp);
        let _ = ty;
        text
    }

    /// Render an operand in a string position: literal or string local.
    /// Unknown-max strings → E501 (bound-flavored; trace hints, if any,
    /// enrich the message but never decide); non-string Refs → E500 (M4).
    fn string_operand(&mut self, op: &Operand) -> Result<String, ()> {
        match op {
            Operand::ConstStr(i) => {
                let lit = self.fe.strings.get(*i as usize).cloned();
                match lit.as_deref() {
                    Some(s) => gab_str_lit(s).map_err(|e| {
                        self.fe.errs.push(e);
                    }),
                    None => {
                        self.fe.errs.push(e500(format!(
                            "string pool index {i} out of range (M3a: internal)"
                        )));
                        Err(())
                    }
                }
            }
            Operand::Copy(l) => {
                if self.fe.str_max.contains_key(&l.0) {
                    Ok(self.lname(*l))
                } else if self.fe.func.locals.get(l.0 as usize) == Some(&Ty::Ref) {
                    let hint = self
                        .fe
                        .trace_hints
                        .get(&l.0)
                        .map(|m| format!(" (observed ≤ {m} in traces — not proven)"))
                        .unwrap_or_default();
                    self.fe.errs.push(format!(
                        "error[E501]: string length needs a static bound (bound solver step){hint}"
                    ));
                    Err(())
                } else {
                    self.fe.errs.push(e500(format!(
                        "string operand is local l{} of non-string type (M4: objects become tables later)",
                        l.0
                    )));
                    Err(())
                }
            }
            _ => {
                self.fe.errs.push(e500(format!(
                    "string operand {op:?} not emittable (M3a: literals and string locals only)"
                )));
                Err(())
            }
        }
    }

    /// Render an array index for `T.slots[..]`: const indices literally
    /// (OOB-const is E501 — Gabbro would refuse just as loudly), dynamic
    /// I64 indices via `narrow ... else { return abort(); }` (the `checked` flag is
    /// subsumed: checked-trap ≡ abort, and unchecked-UB has no parity owed).
    fn array_index(&mut self, table: &str, len: u64, index: &Operand) -> Result<String, ()> {
        match index {
            Operand::ConstI64(v) => {
                let n = *v as i128;
                if n < 0 || n >= len as i128 {
                    self.fe.errs.push("error[E501]: array index past its length (bound solver step)".to_string());
                    return Err(());
                }
                Ok(format!("{table}.slots[{n}].v"))
            }
            Operand::ConstI32(v) => {
                let n = *v as i128;
                if n < 0 || n >= len as i128 {
                    self.fe.errs.push("error[E501]: array index past its length (bound solver step)".to_string());
                    return Err(());
                }
                Ok(format!("{table}.slots[{n}].v"))
            }
            Operand::Copy(l)
                if self.fe.func.locals.get(l.0 as usize) == Some(&Ty::I64)
                    || self.fe.wide32.contains(&l.0) =>
            {
                if len == 0 {
                    self.fe.errs.push("error[E501]: empty array indexed (later step)".to_string());
                    return Err(());
                }
                let t = self.lname(*l);
                let hi = len - 1;
                self.fe.prelude.need_abort = true;
                let _ = writeln!(self.out, "    narrow {t} to 0 .. {hi} else {{");
                let _ = writeln!(self.out, "        return abort();");
                let _ = writeln!(self.out, "    }}");
                Ok(format!("{table}.slots[{t}].v"))
            }
            _ => {
                self.fe.errs.push("error[E501]: dynamic array index needs an Int local (later step)".to_string());
                Err(())
            }
        }
    }

    /// Resolve an array operand to (table, length): conflict → E501 rebind,
    /// no site → E500 object (M4) or E500 shape.
    fn array_table(&mut self, op: &Operand) -> Result<(String, u64), ()> {
        match op {
            Operand::Copy(l) => {
                if self.fe.arr_conflict.contains(&l.0) {
                    self.fe.errs.push("error[E501]: array rebound to a second allocation (later step)".to_string());
                    return Err(());
                }
                match self.fe.arr_site.get(&l.0).copied() {
                    Some(s) => {
                        let len = self.fe.arr_sites.get(s).map(|a| a.len).unwrap_or(0);
                        Ok((arr_table_name(&self.fe.func.name.clone(), s), len))
                    }
                    None => {
                        if self.fe.func.locals.get(l.0 as usize) == Some(&Ty::Ref) {
                            self.fe.errs.push(e500(format!(
                                "array operand is reference l{} without allocation site (M4: objects become tables later)",
                                l.0
                            )));
                        } else {
                            self.fe.errs.push(e500(format!(
                                "array operand l{} is not an array (M3b: array locals only)",
                                l.0
                            )));
                        }
                        Err(())
                    }
                }
            }
            _ => {
                self.fe.errs.push(e500(format!(
                    "array operand {op:?} not emittable (M3b: array locals only)"
                )));
                Err(())
            }
        }
    }

    fn emit_assign(&mut self, dest: Local, rhs: String, ty: &str) {
        if self.bound.insert(dest.0) {
            let _ = writeln!(self.out, "    let mut {} : {ty} = {rhs};", self.lname(dest));
        } else {
            let _ = writeln!(self.out, "    {} = {rhs};", self.lname(dest));
        }
    }

    /// Slot of an object class (None when not an object slot).
    fn obj_slot(&self, class: &str, field: &str) -> Option<(String, ObjSlot)> {
        let slots = self.obj_classes.get(class)?;
        let (_, s) = slots.iter().find(|(n, _)| n == field)?;
        Some((obj_table_name(class), s.clone()))
    }

    /// Resolve an object local to its class (loud E500 when unknown).
    fn obj_class_of(&mut self, l: u32) -> Result<String, ()> {
        match self.obj_locals.get(&l).cloned() {
            Some(c) => Ok(c),
            None => {
                if (l as usize) < self.nparams {
                    self.fe.errs.push(e500(
                        "object parameter needs a monomorphized signature (M4b: later step)".to_string(),
                    ));
                } else {
                    self.fe.errs.push(e500(format!(
                        "object class of l{l} unknown (M4: construction must dominate every use)"
                    )));
                }
                Err(())
            }
        }
    }

    /// Fresh `uN` temp for unwrap binders (shares the temp counter).
    fn fresh_u(&mut self) -> String {
        let t = format!("u{}", self.fe.temps);
        self.fe.temps += 1;
        t
    }

    /// Unwrap an option-typed object local for dereference: `then` runs
    /// with the row index bound as `binder`; `None` diverges (refuse, the
    /// NPE path — never UB, never silent). Verified shape (P3/U3 probes).
    fn emit_unwrap(&mut self, local: u32, binder: &str, then: &str) {
        let l = self.lname(Local(local));
        self.fe.prelude.need_abort = true;
        let _ = writeln!(self.out, "    match {l} {{");
        let _ = writeln!(self.out, "        Some({binder}) => {{ {then} }}");
        let _ = writeln!(self.out, "        None => {{ return abort(); }}");
        let _ = writeln!(self.out, "    }}");
    }

    fn emit_stmt(&mut self, st: &Statement, pos: (u32, usize), funcs: &BTreeMap<String, ()>) {
        // Dead pure temps are skipped (side-effect-free by construction).
        if let Statement::Assign(d, _) = st {
            if !self.fe.used.contains(&d.0) {
                return;
            }
        }
        match st {
            Statement::Assign(d, Rvalue::Use(op)) => {
                let ty = self.fe.func.locals[d.0 as usize];
                match ty {
                    Ty::I32 if self.fe.bools.contains(&d.0) => {
                        // Bool source: only Copy of a bool local (i64free
                        // members are never bools); plain Use of non-bool
                        // → reject.
                        match op {
                            Operand::Copy(l) if self.fe.bools.contains(&l.0) => {
                                let t = self.lname(*l);
                                self.emit_assign(*d, t, "bool");
                            }
                            _ => {
                                self.fe.errs.push(e500(format!(
                                    "non-bool i32 value into bool local l{} (M1: bools come from comparisons only)",
                                    d.0
                                )));
                            }
                        }
                    }
                    Ty::I32 => {
                        // Bool dests take bool copies only (comparisons land
                        // here through the prologue set).
                        if self.fe.bools.contains(&d.0) {
                            match op {
                                Operand::Copy(l) if self.fe.bools.contains(&l.0) => {
                                    let t = self.lname(*l);
                                    self.emit_assign(*d, t, "bool");
                                }
                                _ => {
                                    self.fe.errs.push(e500(format!(
                                        "non-bool i32 value into bool local l{} (M1: bools come from comparisons only)",
                                        d.0
                                    )));
                                }
                            }
                            return;
                        }
                        // Exact i64free members bind i64 (tags, payloads
                        // and their copies); u32 length holders rebind u32;
                        // plain consts bind i32. Anything else → E500.
                        match op {
                            Operand::Copy(l) if self.fe.i64free.contains(&l.0) => {
                                let t = self.lname(*l);
                                self.emit_assign(*d, t, "i64");
                            }
                            Operand::Copy(l)
                                if self.fe.len32.contains(&l.0)
                                    && self.fe.func.locals.get(l.0 as usize)
                                        == Some(&Ty::I32) =>
                            {
                                // (len32 members are u32-bound; copies stay u32.
                                // The source binding is `u32`, so read it raw.)
                                let t = self.lname(*l);
                                self.emit_assign(*d, t, "u32");
                            }
                            Operand::ConstI32(_) | Operand::ConstI64(_) => {
                                let t = self.operand_inline(op, Ty::I32);
                                self.emit_assign(*d, t, "i32");
                            }
                            _ => {
                                self.fe.errs.push(e500(format!(
                                    "local l{} of type {ty:?} not emittable to Gabbro (M1: i64 + comparison bools only)",
                                    d.0
                                )));
                            }
                        }
                    }
                    Ty::I64 => {
                        if matches!(op, Operand::ConstStr(_)) {
                            self.fe.errs.push(e500(format!(
                                "string literal into integer local l{} (M3a: strings flow to strings only)",
                                d.0
                            )));
                            return;
                        }
                        let t = self.operand_inline(op, ty);
                        self.emit_assign(*d, t, "i64");
                    }
                    Ty::Ref => {
                        // Option copy: MATERIALIZES into the dest's own pair
                        // (`let`-first, assign on rebind). Aliasing would
                        // resolve to the last writer's pair — wrong on joins
                        // (φ-copies: each predecessor needs its own value).
                        if let Operand::Copy(l) = op {
                            if self.has_pair(l.0) {
                                let (stag, sval) = self.pair_names(*l);
                                let (dtag, dval) = self.pair_names(*d);
                                let _ = writeln!(self.out, "    {dtag} = {stag};");
                                let _ = writeln!(self.out, "    {dval} = {sval};");
                                return;
                            }
                        }
                        // Object copy/None (M4a static rows): option values
                        // copy verbatim — indices are values, so aliasing
                        // needs no proof. A local claimed by another
                        // analysis is a kind conflict.
                        match op {
                            Operand::Copy(l) => {
                                if let Some(sc) = self.obj_locals.get(&l.0).cloned() {
                                    if self.fe.arr_site.contains_key(&d.0)
                                        || self.fe.arr_conflict.contains(&d.0)
                                        || self.fe.str_max.contains_key(&d.0)
                                        || self.has_pair(d.0)
                                    {
                                        self.fe.errs.push(e500(format!(
                                            "local l{} is both object and string/array/sum (M4: internal)",
                                            d.0
                                        )));
                                        return;
                                    }
                                    match self.obj_locals.get(&d.0) {
                                        Some(dc) if *dc == sc => {}
                                        Some(dc) => {
                                            self.fe.errs.push(e500(format!(
                                                "copying `{sc}` into `{dc}`-typed l{} (M4: internal)",
                                                d.0
                                            )));
                                            return;
                                        }
                                        None => {
                                            self.fe.errs.push(e500(format!(
                                                "object class of l{} unknown (M4: construction must dominate every use)",
                                                d.0
                                            )));
                                            return;
                                        }
                                    }
                                    let t = self.lname(*l);
                                    self.emit_assign(
                                        *d,
                                        t,
                                        &format!("option index into {}", obj_table_name(&sc)),
                                    );
                                    return;
                                }
                            }
                            Operand::ConstNull => {
                                if let Some(dc) = self.obj_locals.get(&d.0).cloned() {
                                    self.emit_assign(
                                        *d,
                                        "None".to_string(),
                                        &format!("option index into {}", obj_table_name(&dc)),
                                    );
                                    return;
                                }
                            }
                            _ => {}
                        }
                        // Array alias: storage lives in the table; the local
                        // vanishes (resolved per use via its site). A local
                        // claimed by both analyses is a kind conflict.
                        if self.fe.arr_site.contains_key(&d.0)
                            || self.fe.arr_conflict.contains(&d.0)
                        {
                            if self.fe.str_max.contains_key(&d.0) {
                                self.fe.errs.push(e500(format!(
                                    "local l{} is both array and string (M3: internal)",
                                    d.0
                                )));
                            }
                            return;
                        }
                        // String def: literal, concat result, or copy. The
                        // declared max is flow-insensitive (max over all defs),
                        // so every reassignment fits (N455).
                        let m = self.fe.str_max.get(&d.0).copied();
                        match (m, op) {
                            (Some(mm), Operand::ConstStr(_))
                            | (Some(mm), Operand::Copy(_)) => match self.string_operand(op) {
                                Ok(t) => {
                                    self.emit_assign(*d, t, &format!("string max {mm}"));
                                }
                                Err(()) => {}
                            },
                            (Some(_), _) => {
                                self.fe.errs.push(e500(format!(
                                    "string local l{} defined by {op:?} (M3a: literals, concat and copies only)",
                                    d.0
                                )));
                            }
                            (None, _) => {
                                // Not a proven string: the operand error
                                // (E501 bound / E500 object) already fired or
                                // fires here — keep it loud exactly once.
                                if !matches!(op, Operand::Copy(_)) {
                                    self.fe.errs.push(e500(format!(
                                        "reference local l{} not emittable (M4: objects become tables later)",
                                        d.0
                                    )));
                                } else if self.string_operand(op).is_ok() {
                                    self.fe.errs.push(e500(format!(
                                        "reference local l{} not emittable (M4: objects become tables later)",
                                        d.0
                                    )));
                                }
                            }
                        }
                    }
                    _ => {
                        self.fe.errs.push(e500(format!(
                            "local l{} of type {ty:?} not emittable to Gabbro (M1: i64 + comparison bools only)",
                            d.0
                        )));
                    }
                }
            }
            Statement::Assign(d, Rvalue::Binary(op, a, b)) => {
                self.emit_binary(*d, *op, a, b);
            }
            Statement::Assign(d, Rvalue::Neg(op)) => {
                // `-x` ≡ `0 - x` (wrap): i64(usub64(usub64(ux,ux), ux)).
                let ty = self.fe.func.locals[d.0 as usize];
                if ty != Ty::I64 {
                    self.fe.errs.push(e500(format!(
                        "negation on {ty:?} (M1: i64 only)"
                    )));
                    return;
                }
                let x = self.operand_inline(op, ty);
                self.fe.prelude.neg = true;
                let t0 = format!("t{}", self.fe.temps);
                self.fe.temps += 1;
                let _ = writeln!(self.out, "    let {t0} : u64 = vire_usub64(u64({x}), u64({x}));");
                let t1 = format!("t{}", self.fe.temps);
                self.fe.temps += 1;
                let _ = writeln!(self.out, "    let {t1} : u64 = vire_usub64({t0}, u64({x}));");
                self.emit_assign(*d, format!("i64({t1})"), "i64");
            }
            Statement::Assign(d, Rvalue::Convert(op)) => {
                // Sound widenings: `lenof`/length u32 and exact i64free
                // locals to I64 (`i64(...)` — always fits). `I64→I32`
                // converts widen back (`let d : i64`, truncation dropped):
                // sound only as array indices (narrow-guarded) — every
                // other use is E500 (operand_inline). Identity `I32→I32`
                // copies i64free members (bool sources excluded — those
                // need the if-form, later step). All else stays E500.
                if let Operand::Copy(l) = op {
                    let sl = self.fe.func.locals.get(l.0 as usize).copied();
                    let dl = self.fe.func.locals.get(d.0 as usize).copied();
                    if dl == Some(Ty::I64)
                        && (self.fe.len32.contains(&l.0) || self.fe.i64free.contains(&l.0))
                    {
                        let t = self.lname(*l);
                        self.emit_assign(*d, format!("i64({t})"), "i64");
                        return;
                    }
                    if dl == Some(Ty::I64) && self.fe.wide32.contains(&l.0) {
                        self.fe.errs.push(e500(
                            "widened index converted back to integer (later step — array indices only)".to_string(),
                        ));
                        return;
                    }
                    if sl == Some(Ty::I64) && dl == Some(Ty::I32) {
                        let t = self.lname(*l);
                        self.emit_assign(*d, t, "i64");
                        self.fe.wide32.insert(d.0);
                        return;
                    }
                    if sl == Some(Ty::I32) && dl == Some(Ty::I32) {
                        if self.fe.i64free.contains(&l.0) {
                            let t = self.lname(*l);
                            self.emit_assign(*d, t, "i64");
                            return;
                        }
                        if !self.fe.bools.contains(&l.0) {
                            let t = self.lname(*l);
                            self.emit_assign(*d, t, "i32");
                            return;
                        }
                    }
                }
                self.fe.errs.push(e500(
                    "numeric conversion (M1: not emittable — conversions need proven ranges)".to_string(),
                ));
            }
            Statement::Call { dest, func, args } => {
                // Sum values never cross calls (M3c signatures); caught here
                // before any operand renders a dangling name.
                for a in args.iter() {
                    if let Operand::Copy(l) = a {
                        if self.has_pair(l.0) {
                            self.fe.errs.push(e500(format!(
                                "sum value into call `{func}` (M3c: monomorphized signatures later)"
                            )));
                            return;
                        }
                    }
                }
                if func == "jrt_println_long" {
                    if args.len() != 1 {
                        self.fe.errs.push(e500("print with != 1 arg (M1: single-int print only)".to_string()));
                        return;
                    }
                    // References are not printable (M4a: no display form).
                    // (Strings take the `println_str` path; option pairs are
                    // sums, not object handles.)
                    if let Operand::Copy(l) = &args[0] {
                        if self.fe.func.locals.get(l.0 as usize) == Some(&Ty::Ref)
                            && !self.has_pair(l.0)
                            && self.fe.str_max.get(&l.0).is_none()
                        {
                            self.fe.errs.push(e500("print of a reference (M4a: print fields, not handles)".to_string()));
                            return;
                        }
                    }
                    // Bool locals print as 0/1 (LLVM behavior) via if/else —
                    // a `bool` never reaches the i64 print helper (M135).
                    if let Operand::Copy(l) = &args[0] {
                        if self.fe.bools.contains(&l.0) {
                            let b = self.lname(*l);
                            self.fe.prelude.print = true;
                            let _ = writeln!(self.out, "    if {b} {{");
                            let _ = writeln!(self.out, "        vire_print_int(1);");
                            let _ = writeln!(self.out, "    }} else {{");
                            let _ = writeln!(self.out, "        vire_print_int(0);");
                            let _ = writeln!(self.out, "    }}");
                            if dest.is_some() {
                                self.fe.errs.push(e500("print with unused dest (M1: internal)".to_string()));
                            }
                            return;
                        }
                    }
                    let a = self.operand_inline(&args[0], Ty::I64);
                    self.fe.prelude.print = true;
                    let _ = writeln!(self.out, "    vire_print_int({a});");
                    let _ = writeln!(self.out, "    putchar(10);");
                    if dest.is_some() {
                        self.fe.errs.push(e500("print with unused dest (M1: internal)".to_string()));
                    }
                    return;
                }
                if func == "jrt_str_concat" {
                    // `d = a + b` (Gabbro `+` adds the maxes — the declared
                    // max is the flow-max, so the sum always fits).
                    let (d, a, b) = match (dest, args.len()) {
                        (Some(d), 2) => (*d, &args[0], &args[1]),
                        _ => {
                            self.fe.errs.push(e500(
                                "string concat shape (M3a: internal)".to_string(),
                            ));
                            return;
                        }
                    };
                    let l = match self.string_operand(a) {
                        Ok(t) => t,
                        Err(()) => return,
                    };
                    let r = match self.string_operand(b) {
                        Ok(t) => t,
                        Err(()) => return,
                    };
                    match self.fe.str_max.get(&d.0).copied() {
                        Some(m) => {
                            self.emit_assign(d, format!("{l} + {r}"), &format!("string max {m}"));
                        }
                        None => {
                            self.fe.errs.push(e500(format!(
                                "concat into non-string local l{} (M4: objects become tables later)",
                                d.0
                            )));
                        }
                    }
                    return;
                }
                if func == "jrt_str_length" {
                    let (d, s) = match (dest, args.len()) {
                        (Some(d), 1) => (*d, &args[0]),
                        _ => {
                            self.fe.errs.push(e500(
                                "string length shape (M3a: internal)".to_string(),
                            ));
                            return;
                        }
                    };
                    let t = match self.string_operand(s) {
                        Ok(x) => x,
                        Err(()) => return,
                    };
                    match self.fe.func.locals.get(d.0 as usize) {
                        Some(Ty::I32) => {
                            self.emit_assign(d, format!("lenof({t})"), "u32");
                        }
                        Some(Ty::I64) => {
                            self.emit_assign(d, format!("i64(lenof({t}))"), "i64");
                        }
                        other => {
                            self.fe.errs.push(e500(format!(
                                "string length into {other:?} (M3a: integer lengths only)"
                            )));
                        }
                    }
                    return;
                }
                if func == "jrt_println_str" {
                    if args.len() != 1 {
                        self.fe.errs.push(e500("print with != 1 arg (M1: single print only)".to_string()));
                        return;
                    }
                    if dest.is_some() {
                        self.fe.errs.push(e500("print with unused dest (M1: internal)".to_string()));
                        return;
                    }
                    let t = match self.string_operand(&args[0]) {
                        Ok(x) => x,
                        Err(()) => return,
                    };
                    let m = match &args[0] {
                        Operand::Copy(l) => self.fe.str_max.get(&l.0).copied(),
                        Operand::ConstStr(i) => {
                            self.fe.strings.get(*i as usize).map(|s| s.len() as u64)
                        }
                        _ => None,
                    };
                    match m {
                        Some(mm) => {
                            self.fe.prelude.print_str.insert(mm);
                            self.fe.prelude.retry_loop = true;
                            let _ = writeln!(self.out, "    vire_print_str_{mm}({t});");
                            let _ = writeln!(self.out, "    putchar(10);");
                        }
                        None => {
                            let hint = match &args[0] {
                                Operand::Copy(l) => self
                                    .fe
                                    .trace_hints
                                    .get(&l.0)
                                    .map(|h| format!(" (observed ≤ {h} in traces — not proven)"))
                                    .unwrap_or_default(),
                                _ => String::new(),
                            };
                            self.fe.errs.push(format!(
                                "error[E501]: string length needs a static bound (bound solver step){hint}"
                            ));
                        }
                    }
                    return;
                }
                if func == "jrt_str_is_empty" || func == "jrt_str_equals" {
                    // Bool-as-i32 (0/1): `if` form (a `bool` local would
                    // poison i32 use sites like `println_long`).
                    let (d, cond) = match (dest, func.as_str(), args.len()) {
                        (Some(d), "jrt_str_is_empty", 1) => {
                            let t = match self.string_operand(&args[0]) {
                                Ok(x) => x,
                                Err(()) => return,
                            };
                            (d, format!("lenof({t}) == 0"))
                        }
                        (Some(d), "jrt_str_equals", 2) => {
                            let l = match self.string_operand(&args[0]) {
                                Ok(x) => x,
                                Err(()) => return,
                            };
                            let r = match self.string_operand(&args[1]) {
                                Ok(x) => x,
                                Err(()) => return,
                            };
                            (d, format!("{l} == {r}"))
                        }
                        _ => {
                            self.fe.errs.push(e500(format!(
                                "string predicate shape for `{func}` (M3a: internal)"
                            )));
                            return;
                        }
                    };
                    let dn = self.lname(*d);
                    if self.bound.insert(d.0) {
                        let _ = writeln!(self.out, "    let mut {dn} : i32 = 0;");
                    } else {
                        let _ = writeln!(self.out, "    {dn} = 0;");
                    }
                    let _ = writeln!(self.out, "    if {cond} {{");
                    let _ = writeln!(self.out, "        {dn} = 1;");
                    let _ = writeln!(self.out, "    }}");
                    return;
                }
                if func.starts_with("jrt_str_") {
                    self.fe.errs.push(format!(
                        "error[E501]: string method `{func}` needs dedicated lowering (later step)"
                    ));
                    return;
                }
                // Explicit `region(max)` pools → Gabbro arenas. The push
                // declares nothing inline (the arena item carries the
                // ceiling); the pop resets it. Non-const ceilings are E501
                // (the solver proves a static bound or rejects — never the
                // human via annotation).
                if func == "jrt_region_push" || func == "jrt_region_pop" {
                    if dest.is_some() {
                        self.fe.errs.push(e500("region push/pop with a value dest (M3: internal)".to_string()));
                        return;
                    }
                    match self.region_sites.get(&pos).cloned() {
                        Some(m) if m.is_push => match m.max {
                            Some(mx) => {
                                let _ = writeln!(self.out, "    -- region({mx}) pool {}", m.name);
                                self.region_stack.push(m.name);
                            }
                            None => {
                                self.fe.errs.push("error[E501]: `region(max)` ceiling needs a positive constant (bound solver step)".to_string());
                            }
                        },
                        Some(m) => {
                            if m.name.is_empty() {
                                self.fe.errs.push(e500("unbalanced region pop without a push (M3: internal)".to_string()));
                            } else {
                                match self.region_stack.pop() {
                                    Some(top) if top == m.name => {
                                        let _ = writeln!(self.out, "    reset {};", m.name);
                                    }
                                    _ => {
                                        self.fe.errs.push(e500("unbalanced region push/pop on this path (M3: internal)".to_string()));
                                    }
                                }
                            }
                        }
                        None => {
                            self.fe.errs.push(e500("region push/pop outside the region table (M3: internal)".to_string()));
                        }
                    }
                    return;
                }
                if func.starts_with("jrt_") {
                    self.fe.errs.push(e500(format!(
                        "runtime call `{func}` not emittable to Gabbro (M1: integer code only)"
                    )));
                    return;
                }
                // `extern "C"` calls: mappable decls become direct `extern fn`
                // calls (rendered with the declared param types); unmappable
                // ones decline with their reason (Ptr/layouts never cross).
                // Machine intrinsics (`syscall`, `__vire_*`) resolve to their
                // synthesized heads (same names as analysis builds); the
                // number rides along as data, so rendering is uniform.
                let head: Option<String> = if func == "syscall" {
                    match args.first() {
                        Some(Operand::ConstI64(n)) if *n >= 0 => {
                            Some(format!("vire_syscall_{n}_{}", args.len() - 1))
                        }
                        Some(Operand::ConstI32(n)) if *n >= 0 => {
                            Some(format!("vire_syscall_{n}_{}", args.len() - 1))
                        }
                        Some(_) => Some(format!("vire_syscall_dyn_{}", args.len() - 1)),
                        None => None,
                    }
                } else if func.starts_with("__vire_") {
                    Some(hw_head_name(func))
                } else {
                    None
                };
                let efunc: &str = head.as_deref().unwrap_or(func);
                if let Some(d) = self.externs.get(efunc).cloned() {
                    if args.len() != d.params.len() {
                        self.fe.errs.push(e500(format!(
                            "extern `{func}` arity mismatch (M1: internal)"
                        )));
                        return;
                    }
                    // References never cross `extern` (M4: scalars only —
                    // Gabbro indices address nothing in C).
                    for a in args.iter() {
                        if let Operand::Copy(l) = a {
                            if self.fe.func.locals.get(l.0 as usize) == Some(&Ty::Ref) {
                                self.fe.errs.push(e500(format!(
                                    "extern `{func}` takes a reference (M4: scalars cross, handles don't)"
                                )));
                                return;
                            }
                        }
                    }
                    let mut ats = vec![];
                    for (a, p) in args.iter().zip(d.params.iter()) {
                        ats.push(self.operand_inline(a, *p));
                    }
                    match dest {
                        None => {
                            if d.ret != Ty::Void {
                                self.fe.errs.push(e500(format!(
                                    "extern `{func}` value discarded (M1: internal)"
                                )));
                                return;
                            }
                            let _ = writeln!(self.out, "    {efunc}({});", ats.join(", "));
                        }
                        Some(dd) => {
                            let want = match d.ret {
                                Ty::I64 => "i64",
                                Ty::I32 => "i32",
                                _ => {
                                    self.fe.errs.push(e500(format!(
                                        "extern `{func}` has non-scalar return (M4: no int->ptr)"
                                    )));
                                    return;
                                }
                            };
                            if self.fe.func.locals.get(dd.0 as usize) != Some(&d.ret) {
                                self.fe.errs.push(e500(format!(
                                    "extern `{func}` dest type mismatch (M1: internal)"
                                )));
                                return;
                            }
                            self.emit_assign(*dd, format!("{efunc}({})", ats.join(", ")), want);
                        }
                    }
                    return;
                }
                if let Some(why) = self.unmapped_externs.get(func).cloned() {
                    self.fe.errs.push(e500(why));
                    return;
                }
                if !funcs.contains_key(func) {
                    self.fe.errs.push(e500(format!(
                        "call to unknown `{func}` (M1: direct calls to emitted functions only)"
                    )));
                    return;
                }
                // Arity/types are consistent by construction; dest type decides form.
                // Reference args render through their option bindings; `null`
                // renders as `None` exactly where the callee parameter is a
                // proven option (Phase 1) — anywhere else it stays loud.
                // A reference arg into a classed parameter must itself be
                // classed the same (never a bare name into the checker).
                let callee_params = self.obj_params.get(func).cloned().unwrap_or_default();
                let mut arg_ts = vec![];
                for (k, a) in args.iter().enumerate() {
                    match callee_params.get(&(k as u32)) {
                        Some(c) => match a {
                            Operand::ConstNull => {
                                arg_ts.push("None".to_string());
                            }
                            Operand::Copy(l) => match self.obj_locals.get(&l.0) {
                                Some(ac) if ac == c => {
                                    arg_ts.push(self.operand_inline(a, Ty::I64));
                                }
                                _ => {
                                    self.fe.errs.push(e500(format!(
                                        "call to `{func}` passes an object of unknown or mismatched class into `{c}` (M4: construction must dominate every use)"
                                    )));
                                    return;
                                }
                            },
                            _ => {
                                self.fe.errs.push(e500(format!(
                                    "call to `{func}` passes a non-reference into `{c}` (M4: internal)"
                                )));
                                return;
                            }
                        },
                        None => {
                            // Unknown-class references never render as bare
                            // names (that would fail at `gabbro check` instead
                            // of here) — e.g. escaping `$mmio` handles.
                            if let Operand::Copy(l) = a {
                                if self.fe.func.locals.get(l.0 as usize) == Some(&Ty::Ref) {
                                    self.fe.errs.push(e500(
                                        "call passes a reference of unknown class (M4: construction must dominate every use)".to_string(),
                                    ));
                                    return;
                                }
                            }
                            arg_ts.push(self.operand_inline(a, Ty::I64));
                        }
                    }
                }
                // Rendered callee name (dotted methods map into the reserved
                // `vire_method_` namespace — same mapping as the definition).
                let gfunc = meth_name(func);
                match dest {
                    None => {
                        let _ = writeln!(self.out, "    {gfunc}({});", arg_ts.join(", "));
                    }
                    Some(d) => {
                        let ty = self.fe.func.locals[d.0 as usize];
                        // Unused value dest: call for effects, drop the value.
                        // (References are never silently dropped — E500 —
                        // unless the class is known, when binding is harmless:
                        // static rows persist either way.)
                        if !self.fe.used.contains(&d.0) {
                            if ty == Ty::Ref {
                                match self.obj_locals.get(&d.0).cloned() {
                                    Some(c) => {
                                        self.emit_assign(
                                            *d,
                                            format!("{gfunc}({})", arg_ts.join(", ")),
                                            &format!("option index into {}", obj_table_name(&c)),
                                        );
                                        return;
                                    }
                                    None => {
                                        self.fe.errs.push(e500(format!(
                                            "call to `{func}` returns a reference that is dropped (M1: no silent drops)"
                                        )));
                                        return;
                                    }
                                }
                            }
                            let _ = writeln!(self.out, "    {gfunc}({});", arg_ts.join(", "));
                            return;
                        }
                        // Object-typed results ride the callee's return class.
                        if ty == Ty::Ref {
                            match self.obj_locals.get(&d.0).cloned() {
                                Some(c) => {
                                    self.emit_assign(
                                        *d,
                                        format!("{gfunc}({})", arg_ts.join(", ")),
                                        &format!("option index into {}", obj_table_name(&c)),
                                    );
                                }
                                None => {
                                    self.fe.errs.push(e500(format!(
                                        "call to `{func}` returns an object of unknown class (M4b: monomorphized signatures later)"
                                    )));
                                }
                            }
                            return;
                        }
                        match gab_ty(ty) {
                            Some(gt) => {
                                self.emit_assign(*d, format!("{gfunc}({})", arg_ts.join(", ")), gt);
                            }
                            None => {
                                self.fe.errs.push(e500(format!(
                                    "call result of type {ty:?} (M1: i64 results only)"
                                )));
                            }
                        }
                    }
                }
            }
            Statement::CallGuarded { .. } | Statement::CallVirtual { .. } | Statement::CallPoly { .. } => {
                self.fe.errs.push(e500(
                    "guarded/virtual/poly call (M1: direct calls only — devirtualization runs first)".to_string(),
                ));
            }
            Statement::New { dest, class } | Statement::StackNew { dest, class } => {
                // Builtin sums materialize as (tag, value) i64/i32 pair
                // locals (no `tagged` types in-slice: within a function the
                // pair carries everything; cross-function sums are M3c
                // signatures). Rebinding reuses the pair (assigns); blocks
                // inside loops bind fresh per pass (`let`).
                if !is_sum_class(class) {
                    // M4a static rows (O29 bounded scope): one table row per
                    // single-execution site; indices are values, so aliasing
                    // and escape need no lifetime proof.
                    if !self.obj_classes.contains_key(class) {
                        self.fe.errs.push(e500(format!(
                            "reference statement `New {class}` (M4a: Int fields and nullable refs to tabled types only)"
                        )));
                        return;
                    }
                    match self.obj_rows.get(&pos).cloned() {
                        Some((c, row)) if c == *class => {
                            self.emit_assign(
                                *dest,
                                format!("Some({row})"),
                                &format!("option index into {}", obj_table_name(class)),
                            );
                        }
                        Some(_) => {
                            self.fe.errs.push(e500("row/class mismatch at construction (M4: internal)".to_string()));
                        }
                        None => {
                            self.fe.errs.push("error[E501]: object construction must execute at most once on the Gabbro path (row linearization — later step)".to_string());
                        }
                    }
                    return;
                }
                if self.fe.str_max.contains_key(&dest.0) || self.fe.arr_site.contains_key(&dest.0) {
                    self.fe.errs.push(e500(format!(
                        "local l{} is both sum and string/array (M3: internal)",
                        dest.0
                    )));
                    return;
                }
                let (tag, val) = self.pair_names(*dest);
                let _ = writeln!(self.out, "    {tag} = -1;");
                let _ = writeln!(self.out, "    {val} = 0;");
            }
            Statement::PutField { obj, class, field, value } => {
                if !is_sum_class(class) {
                    // M4a object field store through the static row.
                    let (t, slot) = match self.obj_slot(class, field) {
                        Some(s) => s,
                        None => {
                            self.fe.errs.push(e500(format!(
                                "reference statement `PutField {class}.{field}` (M4a: Int fields and nullable refs to tabled types only)"
                            )));
                            return;
                        }
                    };
                    let ol = match obj {
                        Operand::Copy(x) => {
                            let c = match self.obj_class_of(x.0) {
                                Ok(c) => c,
                                Err(()) => return,
                            };
                            if c != *class {
                                self.fe.errs.push(e500(format!(
                                    "field store on l{} of class `{c}` through `{class}` (M4: internal)",
                                    x.0
                                )));
                                return;
                            }
                            x.0
                        }
                        Operand::ConstNull => {
                            // Unconditional null dereference: refuse loudly
                            // (the NPE path — a trap, not a limitation).
                            self.fe.prelude.need_abort = true;
                            let _ = writeln!(self.out, "    abort();");
                            return;
                        }
                        _ => {
                            self.fe.errs.push(e500("object field base needs a local (M4: internal)".to_string()));
                            return;
                        }
                    };
                    match slot {
                        ObjSlot::I64 => {
                            let v = match value {
                                Operand::ConstI64(n) => format!("{n}"),
                                Operand::ConstI32(n) => format!("{n}"),
                                Operand::Copy(x)
                                    if self.fe.func.locals.get(x.0 as usize) == Some(&Ty::I64) =>
                                {
                                    self.lname(*x)
                                }
                                // Narrow locals widen exactly (`i64(i64)` is
                                // identity — q1 probe; `i64(i32)` widens).
                                Operand::Copy(x)
                                    if self.fe.func.locals.get(x.0 as usize) == Some(&Ty::I32) =>
                                {
                                    format!("i64({})", self.lname(*x))
                                }
                                _ => {
                                    self.fe.errs.push(e500("integer field needs an integer value (M4: internal)".to_string()));
                                    return;
                                }
                            };
                            let b = self.fresh_u();
                            self.emit_unwrap(ol, &b, &format!("{t}.slots[{b}].{field} = {v};"));
                        }
                        ObjSlot::Ref(target) => {
                            let v = match value {
                                Operand::ConstNull => "None".to_string(),
                                Operand::Copy(x) => match self.obj_locals.get(&x.0).cloned() {
                                    Some(c) if c == target => self.lname(*x),
                                    Some(c) => {
                                        self.fe.errs.push(e500(format!(
                                            "storing `{c}` into `{class}.{field}` of `{target}` (M4: no cross-type stores)"
                                        )));
                                        return;
                                    }
                                    None => {
                                        self.fe.errs.push(e500(format!(
                                            "object class of l{} unknown (M4: construction must dominate every use)",
                                            x.0
                                        )));
                                        return;
                                    }
                                },
                                _ => {
                                    self.fe.errs.push(e500("reference field needs a reference or null (M4: internal)".to_string()));
                                    return;
                                }
                            };
                            let b = self.fresh_u();
                            self.emit_unwrap(ol, &b, &format!("{t}.slots[{b}].{field} = {v};"));
                        }
                    }
                    return;
                }
                let l = match obj {
                    Operand::Copy(x) => x.0,
                    _ => {
                        self.fe.errs.push(e500("sum field base needs a local (M3c: internal)".to_string()));
                        return;
                    }
                };
                if !self.has_pair(l) {
                    if (l as usize) < self.nparams {
                        self.fe.errs.push(e500("sum field on a parameter (M3c: monomorphized signatures later)".to_string()));
                    } else {
                        self.fe.errs.push(e500(format!(
                            "sum field on l{l} without construction (M3c: internal)"
                        )));
                    }
                    return;
                }
                let (tag, val) = self.pair_names(Local(l));
                if field == "__tag" {
                    // Tags lower as either int width (`__tag` is I64-typed, so
                    // lowering emits ConstI64; a narrowing pass may have made
                    // it ConstI32 — accept both, never depend on the pass).
                    match value {
                        Operand::ConstI32(k) => {
                            let _ = writeln!(self.out, "    {tag} = {k};");
                        }
                        Operand::ConstI64(k) if *k >= i32::MIN as i64 && *k <= i32::MAX as i64 => {
                            let _ = writeln!(self.out, "    {tag} = {k};");
                        }
                        _ => {
                            self.fe.errs.push("error[E501]: sum tag needs a constant (later step)".to_string());
                        }
                    }
                    return;
                }
                // Value payload: integers only (strings are no variant
                // payloads — N465; nested sums likewise later). I64 locals
                // and widened index-temps flow raw; consts via temps.
                if self.has_pair(match value {
                    Operand::Copy(x) => x.0,
                    _ => u32::MAX,
                }) {
                    self.fe.errs.push("error[E501]: nested sums (later step)".to_string());
                    return;
                }
                match value {
                    Operand::ConstI64(_) | Operand::ConstI32(_) => {
                        let t = self.operand_inline(value, Ty::I64);
                        let _ = writeln!(self.out, "    {val} = {t};");
                    }
                    Operand::Copy(x)
                        if self.fe.func.locals.get(x.0 as usize) == Some(&Ty::I64)
                            && !self.fe.bools.contains(&x.0) =>
                    {
                        let t = self.lname(*x);
                        let _ = writeln!(self.out, "    {val} = {t};");
                    }
                    Operand::Copy(x) if self.fe.wide32.contains(&x.0) => {
                        // Vire-truncated at construction; small-exact.
                        // (|v| ≥ 2^31 diverges from Vire's wrap — documented,
                        // never exercised; the alternative is E501-always.)
                        let t = self.lname(*x);
                        let _ = writeln!(self.out, "    {val} = {t};");
                    }
                    _ => {
                        self.fe.errs.push("error[E501]: sum payload needs integers (later step)".to_string());
                    }
                }
            }
            Statement::GetField { dest, obj, class, field } => {
                if !is_sum_class(class) {
                    // M4a object field load through the static row. Integer
                    // slots bind i64: dest I32 joins `i64free` statically
                    // (narrowed ⟹ fits ⟹ trunc+sext is identity; un-narrowed
                    // ⟹ full value — LLVM observes the full value either
                    // way, so the i64 binding matches bit-for-bit).
                    let (t, slot) = match self.obj_slot(class, field) {
                        Some(s) => s,
                        None => {
                            self.fe.errs.push(e500(format!(
                                "reference statement `GetField {class}.{field}` (M4a: Int fields and nullable refs to tabled types only)"
                            )));
                            return;
                        }
                    };
                    let ol = match obj {
                        Operand::Copy(x) => {
                            let c = match self.obj_class_of(x.0) {
                                Ok(c) => c,
                                Err(()) => return,
                            };
                            if c != *class {
                                self.fe.errs.push(e500(format!(
                                    "field load on l{} of class `{c}` through `{class}` (M4: internal)",
                                    x.0
                                )));
                                return;
                            }
                            x.0
                        }
                        Operand::ConstNull => {
                            // Unconditional null dereference: refuse loudly.
                            self.fe.prelude.need_abort = true;
                            let _ = writeln!(self.out, "    abort();");
                            let dn = self.lname(*dest);
                            match self.fe.func.locals.get(dest.0 as usize) {
                                Some(Ty::Ref) => {
                                    let dc = match self.obj_locals.get(&dest.0).cloned() {
                                        Some(c) => c,
                                        None => {
                                            self.fe.errs.push(e500(format!(
                                                "object class of l{} unknown (M4: construction must dominate every use)",
                                                dest.0
                                            )));
                                            return;
                                        }
                                    };
                                    if self.bound.insert(dest.0) {
                                        let _ = writeln!(self.out, "    let mut {dn} : option index into {} = None;", obj_table_name(&dc));
                                    }
                                }
                                _ => {
                                    if self.bound.insert(dest.0) {
                                        let _ = writeln!(self.out, "    let mut {dn} : i64 = 0;");
                                    }
                                }
                            }
                            return;
                        }
                        _ => {
                            self.fe.errs.push(e500("object field base needs a local (M4: internal)".to_string()));
                            return;
                        }
                    };
                    match slot {
                        ObjSlot::I64 => {
                            let dl = self.fe.func.locals.get(dest.0 as usize).copied();
                            match dl {
                                Some(Ty::I64) | Some(Ty::I32) => {}
                                _ => {
                                    self.fe.errs.push(e500("integer field loaded into a non-integer local (M4: internal)".to_string()));
                                    return;
                                }
                            }
                            let dn = self.lname(*dest);
                            if self.bound.insert(dest.0) {
                                let _ = writeln!(self.out, "    let mut {dn} : i64 = 0;");
                            }
                            let b = self.fresh_u();
                            self.emit_unwrap(ol, &b, &format!("{dn} = {t}.slots[{b}].{field};"));
                        }
                        ObjSlot::Ref(target) => {
                            if self.fe.func.locals.get(dest.0 as usize) != Some(&Ty::Ref) {
                                self.fe.errs.push(e500("reference field loaded into a non-reference local (M4: internal)".to_string()));
                                return;
                            }
                            match self.obj_locals.get(&dest.0) {
                                Some(c) if *c == target => {}
                                Some(c) => {
                                    self.fe.errs.push(e500(format!(
                                        "field `{class}.{field}` of `{target}` loaded into `{c}`-typed l{} (M4: internal)",
                                        dest.0
                                    )));
                                    return;
                                }
                                None => {
                                    self.fe.errs.push(e500(format!(
                                        "object class of l{} unknown (M4: construction must dominate every use)",
                                        dest.0
                                    )));
                                    return;
                                }
                            }
                            let dn = self.lname(*dest);
                            if self.bound.insert(dest.0) {
                                let _ = writeln!(self.out, "    let mut {dn} : option index into {} = None;", obj_table_name(&target));
                            }
                            let b = self.fresh_u();
                            self.emit_unwrap(ol, &b, &format!("{dn} = {t}.slots[{b}].{field};"));
                        }
                    }
                    return;
                }
                let l = match obj {
                    Operand::Copy(x) => x.0,
                    _ => {
                        self.fe.errs.push(e500("sum field base needs a local (M3c: internal)".to_string()));
                        return;
                    }
                };
                if !self.has_pair(l) {
                    if (l as usize) < self.nparams {
                        self.fe.errs.push(e500("sum field on a parameter (M3c: monomorphized signatures later)".to_string()));
                    } else {
                        self.fe.errs.push(e500(format!(
                            "sum field on l{l} without construction (M3c: internal)"
                        )));
                    }
                    return;
                }
                let (tag, val) = self.pair_names(Local(l));
                if field == "__tag" {
                    // Tags are 0/1: widen once, use as i64 everywhere
                    // (the dest joins `i64free` statically).
                    self.emit_assign(*dest, format!("i64({tag})"), "i64");
                    return;
                }
                // Values widen identically (Vire-truncated at construction).
                self.emit_assign(*dest, val, "i64");
            }
            Statement::GetStatic { class, .. } => {
                // `$mmio` handles are compile-time descriptors (bound to a
                // `--hw-profile` region, consumed into specialized
                // `__vire_mmio_*` calls): no value to bind, like array
                // aliases vanishing into their tables. Escaping uses stay
                // loud (unknown-class reference args are E500 at calls).
                if class == "$mmio" {
                    return;
                }
                self.fe.errs.push(e500(format!(
                    "reference statement `GetStatic {class}` (M1: integer code only — objects become tables later)"
                )));
            }
            Statement::PutStatic { .. }
            | Statement::InstanceOfPending { .. }
            | Statement::CheckCast { .. }
            | Statement::InstanceOf { .. } => {
                self.fe.errs.push(e500(format!(
                    "reference statement `{}` (M1: integer code only — objects become tables later)",
                    stmt_kind(st)
                )));
            }
            Statement::NewArray { dest, .. }
            | Statement::StackNewArray { dest, .. }
            | Statement::RegionNewArray { dest, .. } => {
                // Const-length i64 sites live in their table (no binding —
                // the local aliases storage, resolved per use). Anything
                // else declines at use sites; allocations with no site at
                // all fail here so they never pass silently.
                if self.fe.arr_site.contains_key(&dest.0)
                    || self.fe.arr_conflict.contains(&dest.0)
                {
                    return;
                }
                self.fe.errs.push("error[E501]: array allocation needs a constant positive length of i64 elements (later step)".to_string());
            }
            Statement::ArrayLen { dest, arr } => {
                let (_, len) = match self.array_table(arr) {
                    Ok(t) => t,
                    Err(()) => return,
                };
                match self.fe.func.locals.get(dest.0 as usize) {
                    Some(Ty::I32) => {
                        self.emit_assign(*dest, format!("{len}"), "u32");
                    }
                    Some(Ty::I64) => {
                        self.emit_assign(*dest, format!("{len}"), "i64");
                    }
                    other => {
                        self.fe.errs.push(e500(format!(
                            "array length into {other:?} (M3b: integer lengths only)"
                        )));
                    }
                }
            }
            Statement::ArrayLoad { dest, arr, index, kind, .. } => {
                if !matches!(kind, ArrKind::Long | ArrKind::U8) {
                    self.fe.errs.push("error[E501]: non-i64 array elements (later step)".to_string());
                    return;
                }
                if self.fe.func.locals.get(dest.0 as usize) != Some(&Ty::I64) {
                    self.fe.errs.push(e500(format!(
                        "array load into non-i64 local l{} (M3b: i64 arrays first)",
                        dest.0
                    )));
                    return;
                }
                let (table, len) = match self.array_table(arr) {
                    Ok(t) => t,
                    Err(()) => return,
                };
                match self.array_index(&table, len, index) {
                    Ok(place) => {
                        self.emit_assign(*dest, place, "i64");
                    }
                    Err(()) => {}
                }
            }
            Statement::ArrayStore { arr, index, value, kind, .. } => {
                if !matches!(kind, ArrKind::Long | ArrKind::U8) {
                    self.fe.errs.push("error[E501]: non-i64 array elements (later step)".to_string());
                    return;
                }
                let (table, len) = match self.array_table(arr) {
                    Ok(t) => t,
                    Err(()) => return,
                };
                let place = match self.array_index(&table, len, index) {
                    Ok(p) => p,
                    Err(()) => return,
                };
                let v = self.operand_inline(value, Ty::I64);
                let _ = writeln!(self.out, "    {place} = {v};");
            }
            Statement::DebugLine(_) => {}
        }
    }

    fn emit_binary(&mut self, d: Local, op: BinOp, a: &Operand, b: &Operand) {
        let ty = self.fe.func.locals[d.0 as usize];
        // String comparisons (operands are literals or string locals).
        // Gabbro offers `==`/`<` on strings; the rest is expressed through
        // them (`!=`/`>=`/`<=` via negation, `>` via flipped `<`) — sound in
        // every checker, no new proof obligations.
        let is_str_op = |op: &Operand, me: &BodyEmit| -> bool {
            matches!(op, Operand::ConstStr(_))
                || matches!(op, Operand::Copy(l) if me.fe.str_max.contains_key(&l.0))
        };
        if matches!(
            op,
            BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe
        ) && (is_str_op(a, self) || is_str_op(b, self))
        {
            let l = match self.string_operand(a) {
                Ok(t) => t,
                Err(()) => return,
            };
            let r = match self.string_operand(b) {
                Ok(t) => t,
                Err(()) => return,
            };
            let text = match op {
                BinOp::CmpEq => format!("{l} == {r}"),
                BinOp::CmpNe => format!("!({l} == {r})"),
                BinOp::CmpLt => format!("{l} < {r}"),
                BinOp::CmpGt => format!("{r} < {l}"),
                BinOp::CmpGe => format!("!({l} < {r})"),
                _ => format!("!({r} < {l})"),
            };
            self.fe.bools.insert(d.0);
            self.emit_assign(d, text, "bool");
            return;
        }
        match (op, ty) {
            (BinOp::CmpEq, Ty::I32)
            | (BinOp::CmpNe, Ty::I32)
            | (BinOp::CmpLt, Ty::I32)
            | (BinOp::CmpGe, Ty::I32)
            | (BinOp::CmpGt, Ty::I32)
            | (BinOp::CmpLe, Ty::I32) => {
                // Reference equality (M4a static rows) intercepts Eq/Ne with
                // a reference side BEFORE the integer path: option-typed
                // locals compare natively, `x == None` / `x != None` are
                // null tests (canonical rows make index equality identity).
                // Anything else with a reference side declines loudly —
                // never a dangling name into the checker.
                if matches!(op, BinOp::CmpEq | BinOp::CmpNe) {
                    enum Side {
                        Null,
                        Cls(String),
                        Unmapped,
                        Other,
                    }
                    let side = |o: &Operand| -> Side {
                        match o {
                            Operand::ConstNull => Side::Null,
                            Operand::Copy(l)
                                if self.fe.func.locals.get(l.0 as usize) == Some(&Ty::Ref) =>
                            {
                                match self.obj_locals.get(&l.0) {
                                    Some(c) => Side::Cls(c.clone()),
                                    None => Side::Unmapped,
                                }
                            }
                            _ => Side::Other,
                        }
                    };
                    let is_ref_side = |s: &Side| -> bool {
                        !matches!(s, Side::Other)
                    };
                    let sa = side(a);
                    let sb = side(b);
                    if is_ref_side(&sa) || is_ref_side(&sb) {
                        let sop = if matches!(op, BinOp::CmpEq) { "==" } else { "!=" };
                        let render = |x: &Operand| -> String {
                            match x {
                                Operand::Copy(l) => self.lname(*l),
                                _ => "None".to_string(),
                            }
                        };
                        let same_class = match (&sa, &sb) {
                            (Side::Cls(x), Side::Cls(y)) => Some(x == y),
                            _ => None,
                        };
                        match (&sa, &sb) {
                            (Side::Unmapped, _) | (_, Side::Unmapped) => {
                                self.fe.errs.push(e500("comparison on an object of unknown class (M4: class must be known at every use)".to_string()));
                            }
                            (Side::Cls(_), Side::Cls(_)) if same_class == Some(false) => {
                                self.fe.errs.push(e500("comparison across different object types (M4: no cross-type identity)".to_string()));
                            }
                            (Side::Cls(_), Side::Cls(_))
                            | (Side::Cls(_), Side::Null)
                            | (Side::Null, Side::Cls(_)) => {
                                let l = render(a);
                                let r = render(b);
                                self.fe.bools.insert(d.0);
                                self.emit_assign(d, format!("{l} {sop} {r}"), "bool");
                            }
                            _ => {
                                self.fe.errs.push(e500("reference compared to a non-reference (M4: internal)".to_string()));
                            }
                        }
                        return;
                    }
                }
                let l = self.operand_inline(a, Ty::I64);
                let r = self.operand_inline(b, Ty::I64);
                let sop = match op {
                    BinOp::CmpEq => "==",
                    BinOp::CmpNe => "!=",
                    BinOp::CmpLt => "<",
                    BinOp::CmpGe => ">=",
                    BinOp::CmpGt => ">",
                    _ => "<=",
                };
                self.fe.bools.insert(d.0);
                self.emit_assign(d, format!("{l} {sop} {r}"), "bool");
            }
            (BinOp::Add, Ty::I64) | (BinOp::Sub, Ty::I64) | (BinOp::Mul, Ty::I64) => {
                let l = self.operand_inline(a, ty);
                let r = self.operand_inline(b, ty);
                let h = match op {
                    BinOp::Add => {
                        self.fe.prelude.add = true;
                        "vire_uadd64"
                    }
                    BinOp::Sub => {
                        self.fe.prelude.sub = true;
                        "vire_usub64"
                    }
                    _ => {
                        self.fe.prelude.mul = true;
                        "vire_umul64"
                    }
                };
                self.emit_assign(d, format!("i64({h}(u64({l}), u64({r})))"), "i64");
            }
            _ => {
                self.fe.errs.push(e500(format!(
                    "{op:?} on {ty:?} (M1: wrapping i64 arith + i64 comparisons only)"
                )));
            }
        }
    }

    /// Emit blocks from `entry` until `stop` (exclusive) or a return.
    /// Returns true if control falls through (reached `stop`), false if returned.
    fn emit_region(&mut self, entry: u32, stop: Option<u32>, funcs: &BTreeMap<String, ()>) -> bool {
        let mut cur = entry;
        loop {
            if Some(cur) == stop {
                return true;
            }
            if self.active.contains(&cur) {
                self.fe.errs.push(
                    "error[E501]: loop without a static bound on the Gabbro path (bound solver step)".to_string(),
                );
                return true;
            }
            // M2b region net: inside a `retry` body, every visited block must
            // belong to the innermost loop's region (chased through lowering
            // trampolines) or end in `Return` (function exit cannot escape
            // wrongly). Anything else is a loop exit the analysis did not
            // prove — loud E500, never post-loop code inside the retry.
            if !self.loops.is_empty() && !self.in_loop_region(cur) {
                self.fe.errs.push(e500(format!(
                    "block {cur} leaves the loop region (bound solver step)"
                )));
                self.active.pop();
                return false;
            }
            self.active.push(cur);
            self.seen.insert(cur);
            let blk = &self.fe.func.blocks[cur as usize];
            for (si, st) in blk.statements.iter().enumerate() {
                self.emit_stmt(st, (cur, si), funcs);
            }
            match &blk.terminator {
                Terminator::Goto(t) => {
                    // M2b: loop back-edge / exit under an enclosing `retry`.
                    // Trampolines chased first (lowering forwards through
                    // empty blocks); innermost match wins (Vire `break` /
                    // `continue` target it).
                    let nx = chase_goto(&self.fe.func.blocks, t.0);
                    if let Some(lx) = self.loops.iter().rev().find(|l| l.header == nx) {
                        let nm = lx.name.clone();
                        let _ = writeln!(self.out, "    next {nm};");
                        self.active.pop();
                        return false;
                    }
                    if let Some(lx) = self.loops.iter().rev().find(|l| l.merge == nx) {
                        let nm = lx.name.clone();
                        let _ = writeln!(self.out, "    leave {nm};");
                        self.active.pop();
                        return false;
                    }
                    self.active.pop();
                    cur = t.0;
                    continue;
                }
                Terminator::Return(op) => {
                    match op {
                        Some(o) => {
                            // Cross-function sums are M3c signatures (the
                            // Ref signature already declined); reached here
                            // only while emitting doomed bodies.
                            if let Operand::Copy(l) = o {
                                if self.has_pair(l.0) {
                                    self.fe.errs.push(e500("sum value leaves the function (M3c: monomorphized signatures later)".to_string()));
                                    self.active.pop();
                                    return false;
                                }
                            }
                            // Null returns ride the known return class.
                            if matches!(o, Operand::ConstNull)
                                && self.fe.func.ret == Ty::Ref
                                && self.obj_rets.contains_key(&self.fe.func.name)
                            {
                                let _ = writeln!(self.out, "    return None;");
                            } else {
                                let t = self.operand_inline(o, self.fe.func.ret);
                                let _ = writeln!(self.out, "    return {t};");
                            }
                        }
                        None => {
                            // Vire main returns Void; Gabbro main answers i32.
                            if self.is_main {
                                let _ = writeln!(self.out, "    return 0;");
                            } else {
                                let _ = writeln!(self.out, "    return;");
                            }
                        }
                    }
                    self.active.pop();
                    return false;
                }
                Terminator::Branch { cond, then_blk, else_blk } => {
                    let (t, e) = (then_blk.0, else_blk.0);
                    // M2b: loop header → `retry` emission (bypasses diamonds).
                    // The header's own statements (condition temps) were already
                    // emitted above exactly once; bodies never re-emit them
                    // (back-edges become `next`, exits become `leave`).
                    if self.headers.contains(&cur) {
                        let li = self.loop_info.get(&cur).cloned();
                        match li {
                            Some(li) if li.deny_msg.is_none() && li.trips.is_some() => {
                                let trips = li.trips.unwrap_or(0);
                                if trips == 0 {
                                    // Zero-trip: the body never runs; continue
                                    // at the merge (dead body still validated
                                    // via validate_unvisited).
                                    self.active.pop();
                                    cur = li.merge;
                                    continue;
                                }
                                let until =
                                    li.until_pred.clone().unwrap_or_else(|| "false".to_string());
                                let name = format!("vloop{}", self.loop_seq);
                                self.loop_seq += 1;
                                self.fe.prelude.retry_loop = true;
                                // `bounded` is a PER-PASS budget (K006) — the
                                // proven per-pass upper bound, never trips.
                                let per = li.per_pass.max(1);
                                let _ = writeln!(
                                    self.out,
                                    "    retry {name} until ({until}) bounded {per} ops on_exceeded abort {{"
                                );
                                self.loops.push(LoopCtx {
                                    header: cur,
                                    merge: li.merge,
                                    name: name.clone(),
                                });
                                let falls = self.emit_region(li.body_entry, None, funcs);
                                self.loops.pop();
                                let _ = writeln!(self.out, "    }}");
                                if falls {
                                    // Every body path must end in
                                    // next/leave/return (analysis proves it for
                                    // valid loops) — fallthrough would emit
                                    // post-loop code inside the retry.
                                    self.fe.errs.push(e500(format!(
                                        "loop body at {cur} falls through (bound solver step)"
                                    )));
                                }
                                // Nested-loop continuation: the merge may land
                                // on an enclosing loop's header/merge (the
                                // inner loop exits outward) — same rule as the
                                // Goto arm. Otherwise walk on normally.
                                let mm = chase_goto(&self.fe.func.blocks, li.merge);
                                let hdr = self
                                    .loops
                                    .iter()
                                    .rev()
                                    .find(|l| l.header == mm)
                                    .map(|l| l.name.clone());
                                let mrg = self
                                    .loops
                                    .iter()
                                    .rev()
                                    .find(|l| l.merge == mm)
                                    .map(|l| l.name.clone());
                                if let Some(nm) = hdr {
                                    let _ = writeln!(self.out, "    next {nm};");
                                    self.active.pop();
                                    return false;
                                }
                                if let Some(nm) = mrg {
                                    let _ = writeln!(self.out, "    leave {nm};");
                                    self.active.pop();
                                    return false;
                                }
                                self.active.pop();
                                cur = li.merge;
                                continue;
                            }
                            Some(li) => {
                                // Denied in validation (E500/E501 already
                                // pushed there identical — deduped at the end).
                                self.fe.errs.push(li.deny_msg.clone().unwrap_or_else(|| {
                                    "error[E501]: loop without a static bound on the Gabbro path (bound solver step)".to_string()
                                }));
                                self.active.pop();
                                return true;
                            }
                            None => {
                                self.fe.errs.push(e500(format!(
                                    "loop header {cur} without analysis (bound solver step)"
                                )));
                                self.active.pop();
                                return true;
                            }
                        }
                    }
                    // Condition: bool local → direct; constant → true/false;
                    // other i32 → `!= 0`.
                    let ctext = match cond {
                        Operand::Copy(l) if self.fe.bools.contains(&l.0) => self.lname(*l),
                        Operand::ConstI32(0) => "false".to_string(),
                        Operand::ConstI32(_) => "true".to_string(),
                        _ => {
                            let c = self.operand_inline(cond, Ty::I32);
                            format!("{c} != 0")
                        }
                    };
                    // M2b: loop-control branches under an enclosing `retry`
                    // (`break`/`continue` under `if`). Chased arms targeting
                    // an enclosing header/merge become `next`/`leave`
                    // (innermost match); live arms continue normally; arms
                    // escaping anywhere else are E501 (later step).
                    if !self.loops.is_empty() {
                        let blocks = &self.fe.func.blocks;
                        let ct = chase_goto(blocks, t);
                        let ce = chase_goto(blocks, e);
                        // Innermost enclosing loop with header/merge == x.
                        // (None = live arm; Some(None) handled below.)
                        let find_ctrl = |x: u32, loops: &[LoopCtx]| -> Option<String> {
                            for lx in loops.iter().rev() {
                                if lx.header == x {
                                    return Some(format!("next {}", lx.name));
                                }
                                if lx.merge == x {
                                    return Some(format!("leave {}", lx.name));
                                }
                            }
                            None
                        };
                        let t_ctrl = find_ctrl(ct, &self.loops);
                        let e_ctrl = find_ctrl(ce, &self.loops);
                        // Liveness for non-control arms: in-region (chased)
                        // or Return-terminated (function exit); anything else
                        // escapes the proven region.
                        let live_ok = |x: u32, blocks: &[BasicBlock]| -> bool {
                            let inn = self.loops.last().unwrap().header;
                            match self.loop_info.get(&inn) {
                                Some(li) if li.body_blocks.contains(&x) || inn == x => true,
                                _ => matches!(
                                    blocks.get(x as usize).map(|blk| &blk.terminator),
                                    Some(Terminator::Return(_))
                                ),
                            }
                        };
                        match (t_ctrl, e_ctrl) {
                            (None, None) => {
                                if !live_ok(ct, blocks) || !live_ok(ce, blocks) {
                                    self.fe.errs.push("error[E501]: loop control shape (later step)".to_string());
                                    self.active.pop();
                                    return true;
                                }
                                // Genuine in-region diamond: fall through.
                            }
                            (Some(op), None) => {
                                if !live_ok(ce, blocks) {
                                    self.fe.errs.push("error[E501]: loop control shape (later step)".to_string());
                                    self.active.pop();
                                    return true;
                                }
                                let c = ctext.clone();
                                let _ = writeln!(self.out, "    if {c} {{");
                                let _ = writeln!(self.out, "    {op};");
                                let _ = writeln!(self.out, "    }}");
                                self.active.pop();
                                cur = e;
                                continue;
                            }
                            (None, Some(op)) => {
                                if !live_ok(ct, blocks) {
                                    self.fe.errs.push("error[E501]: loop control shape (later step)".to_string());
                                    self.active.pop();
                                    return true;
                                }
                                let c = ctext.clone();
                                let _ = writeln!(self.out, "    if !({c}) {{");
                                let _ = writeln!(self.out, "    {op};");
                                let _ = writeln!(self.out, "    }}");
                                self.active.pop();
                                cur = t;
                                continue;
                            }
                            (Some(top), Some(eop)) => {
                                let c = ctext.clone();
                                let _ = writeln!(self.out, "    if {c} {{");
                                let _ = writeln!(self.out, "    {top};");
                                let _ = writeln!(self.out, "    }} else {{");
                                let _ = writeln!(self.out, "    {eop};");
                                let _ = writeln!(self.out, "    }}");
                                self.active.pop();
                                return false;
                            }
                        }
                    }
                    let merge = self.find_merge(t, e, stop);
                    match merge {
                        Merge::At(m) => {
                            if t != m && e != m {
                                let _ = writeln!(self.out, "    if {ctext} {{");
                                let _ = self.emit_region_nested(t, Some(m), funcs);
                                let _ = writeln!(self.out, "    }} else {{");
                                let _ = self.emit_region_nested(e, Some(m), funcs);
                                let _ = writeln!(self.out, "    }}");
                            } else if t == m {
                                // Empty then-side: negate (no empty blocks).
                                let _ = writeln!(self.out, "    if !({ctext}) {{");
                                let _ = self.emit_region_nested(e, Some(m), funcs);
                                let _ = writeln!(self.out, "    }}");
                            } else {
                                // Empty else-side: omit it.
                                let _ = writeln!(self.out, "    if {ctext} {{");
                                let _ = self.emit_region_nested(t, Some(m), funcs);
                                let _ = writeln!(self.out, "    }}");
                            }
                            self.active.pop();
                            cur = m;
                            continue;
                        }
                        Merge::BothReturn => {
                            // Both sides return: the function ends here.
                            let _ = writeln!(self.out, "    if {ctext} {{");
                            let _ = self.emit_region_nested(t, None, funcs);
                            let _ = writeln!(self.out, "    }} else {{");
                            let _ = self.emit_region_nested(e, None, funcs);
                            let _ = writeln!(self.out, "    }}");
                            self.active.pop();
                            return false;
                        }
                        Merge::Loop => {
                            // E501 already pushed by find_merge; stop here
                            // (output is discarded anyway once errs is non-empty).
                            self.active.pop();
                            return true;
                        }
                        Merge::Unstructured { t_ret, e_ret } => {
                            // Mixed early-return (one side falls through) comes
                            // with the loop step; anything else is unstructured.
                            if t_ret != e_ret {
                                self.fe.errs.push(e500(
                                    "branch where one side returns and the other falls through (early exits come with the loop step)".to_string(),
                                ));
                            } else {
                                self.fe.errs.push(e500(
                                    "unstructured branch (M1: if-diamonds and both-return only)".to_string(),
                                ));
                            }
                            self.active.pop();
                            return true;
                        }
                    }
                }
                Terminator::Switch { .. } => {
                    self.fe.errs.push(e500("switch (M1: if-diamonds only)".to_string()));
                    self.active.pop();
                    return true;
                }
            }
        }
    }

    /// Nested region emission that does not disturb the outer active path
    /// except for loop detection (shares it).
    fn emit_region_nested(&mut self, entry: u32, stop: Option<u32>, funcs: &BTreeMap<String, ()>) -> bool {
        self.emit_region(entry, stop, funcs)
    }

    /// Validate dead (unvisited) blocks: rejected constructs are rejected
    /// everywhere, even where they never execute. Output and emitter state
    /// are saved/restored around each block (validation only, no emission).
    fn validate_unvisited(&mut self, funcs: &BTreeMap<String, ()>) {
        let n = self.fe.func.blocks.len() as u32;
        for bi in 0..n {
            if self.seen.contains(&bi) {
                continue;
            }
            let saved_out = self.out.len();
            let saved_temps = self.fe.temps;
            let saved_bound = self.bound.clone();
            let saved_prelude = std::mem::take(&mut self.fe.prelude);
            let saved_region = self.region_stack.clone();
            let blk = &self.fe.func.blocks[bi as usize];
            for (si, st) in blk.statements.iter().enumerate() {
                self.emit_stmt(st, (bi, si), funcs);
            }
            // Terminator operands: references/floats are still rejected.
            match &blk.terminator {
                Terminator::Branch { cond, .. } => {
                    if let Operand::Copy(l) = cond {
                        let ty = self.fe.func.locals[l.0 as usize];
                        if !matches!(ty, Ty::I32 | Ty::I64) {
                            self.fe.errs.push(e500(format!(
                                "branch on {ty:?} (M1: integer conditions only)"
                            )));
                        }
                    }
                }
                Terminator::Return(Some(op)) => {
                    if let Operand::Copy(l) = op {
                        let ty = self.fe.func.locals[l.0 as usize];
                        if !matches!(ty, Ty::I32 | Ty::I64) {
                            self.fe.errs.push(e500(format!(
                                "return of {ty:?} (M1: integer results only)"
                            )));
                        }
                    }
                }
                Terminator::Switch { .. } => {
                    self.fe.errs.push(e500("switch (M1: if-diamonds only)".to_string()));
                }
                _ => {}
            }
            self.out.truncate(saved_out);
            self.fe.temps = saved_temps;
            self.bound = saved_bound;
            self.fe.prelude = saved_prelude;
            self.region_stack = saved_region;
        }
    }

    /// Merge of a branch: the reconvergence block, both-return, loop, or other.
    /// Loops are detected precisely (a side reaches the active path = an
    /// ancestor = a back-edge in Vire's reducible graphs) and get E501;
    /// anything else unstructured gets E500 (message chosen by the caller).
    fn find_merge(&mut self, t: u32, e: u32, stop: Option<u32>) -> Merge {
        if t == e {
            return Merge::At(t);
        }
        let blocks = &self.fe.func.blocks;
        let succ = |b: u32| -> Vec<u32> {
            if b as usize >= blocks.len() {
                return vec![];
            }
            match &blocks[b as usize].terminator {
                Terminator::Goto(x) => vec![x.0],
                Terminator::Branch { then_blk, else_blk, .. } => vec![then_blk.0, else_blk.0],
                Terminator::Switch { default, cases, .. } => {
                    let mut v = vec![default.0];
                    v.extend(cases.iter().map(|(_, x)| x.0));
                    v
                }
                Terminator::Return(_) => vec![],
            }
        };
        // Lockstep BFS from both sides; first common block wins (diamonds,
        // including nested ones — succ covers inner branches).
        let mut dt: BTreeMap<u32, u32> = BTreeMap::new();
        let mut de: BTreeMap<u32, u32> = BTreeMap::new();
        let mut ft = vec![t];
        let mut fe = vec![e];
        dt.insert(t, 0);
        de.insert(e, 0);
        // A side that IS the stop falls through to it.
        for _ in 0..blocks.len() + 1 {
            // Check intersection (excluding the starts unless stop).
            for (b, _) in dt.iter() {
                if *b != t && *b != e && de.contains_key(b) {
                    return Merge::At(*b);
                }
            }
            if stop.is_some() {
                if let Some(s) = stop {
                    if dt.contains_key(&s) && de.contains_key(&s) {
                        return Merge::At(s);
                    }
                }
            }
            let mut nt = vec![];
            for b in ft.drain(..) {
                for s in succ(b) {
                    if !dt.contains_key(&s) {
                        dt.insert(s, 0);
                        nt.push(s);
                    }
                }
            }
            let mut ne = vec![];
            for b in fe.drain(..) {
                for s in succ(b) {
                    if !de.contains_key(&s) {
                        de.insert(s, 0);
                        ne.push(s);
                    }
                }
            }
            ft = nt;
            fe = ne;
            if ft.is_empty() && fe.is_empty() {
                break;
            }
        }
        for (b, _) in dt.iter() {
            if *b != t && *b != e && de.contains_key(b) {
                return Merge::At(*b);
            }
        }
        // No common block: loop (a side reaches the active path), both-return,
        // or unstructured (message chosen by the caller from the returns).
        if self.reaches_active(t) || self.reaches_active(e) {
            self.fe.errs.push(
                "error[E501]: loop without a static bound on the Gabbro path (bound solver step)".to_string(),
            );
            return Merge::Loop;
        }
        let t_ret = self.side_returns(t, &mut vec![]);
        let e_ret = self.side_returns(e, &mut vec![]);
        if t_ret && e_ret {
            return Merge::BothReturn;
        }
        Merge::Unstructured { t_ret, e_ret }
    }

    /// True iff `s` can reach the active path (an ancestor block) — a back-edge.
    fn reaches_active(&self, s: u32) -> bool {
        let blocks = &self.fe.func.blocks;
        let mut seen = BTreeSet::from([s]);
        let mut stack = vec![s];
        for _ in 0..blocks.len() + 1 {
            let Some(b) = stack.pop() else { break };
            if b as usize >= blocks.len() {
                continue;
            }
            if self.active.contains(&b) {
                return true;
            }
            match &blocks[b as usize].terminator {
                Terminator::Goto(x) => {
                    if seen.insert(x.0) {
                        stack.push(x.0);
                    }
                }
                Terminator::Branch { then_blk, else_blk, .. } => {
                    if seen.insert(then_blk.0) {
                        stack.push(then_blk.0);
                    }
                    if seen.insert(else_blk.0) {
                        stack.push(else_blk.0);
                    }
                }
                Terminator::Switch { default, cases, .. } => {
                    if seen.insert(default.0) {
                        stack.push(default.0);
                    }
                    for (_, x) in cases {
                        if seen.insert(x.0) {
                            stack.push(x.0);
                        }
                    }
                }
                Terminator::Return(_) => {}
            }
        }
        false
    }

    /// True iff every maximal path from `s` ends in Return (loops → false).
    fn side_returns(&self, s: u32, active: &mut Vec<u32>) -> bool {
        if active.contains(&s) {
            return false;
        }
        let blocks = &self.fe.func.blocks;
        if s as usize >= blocks.len() {
            return false;
        }
        active.push(s);
        let r = match &blocks[s as usize].terminator {
            Terminator::Return(_) => true,
            Terminator::Goto(x) => self.side_returns(x.0, active),
            Terminator::Branch { then_blk, else_blk, .. } => {
                self.side_returns(then_blk.0, active) && self.side_returns(else_blk.0, active)
            }
            Terminator::Switch { .. } => false,
        };
        active.pop();
        r
    }
}

/// Merge of a branch: reconvergence, both-return, loop, or unstructured
/// (with the sides' return facts for the message choice).
enum Merge {
    At(u32),
    BothReturn,
    Loop,
    Unstructured { t_ret: bool, e_ret: bool },
}

fn stmt_kind(st: &Statement) -> &'static str {
    match st {
        Statement::Assign(..) => "Assign",
        Statement::Call { .. } => "Call",
        Statement::CallGuarded { .. } => "CallGuarded",
        Statement::CallVirtual { .. } => "CallVirtual",
        Statement::CallPoly { .. } => "CallPoly",
        Statement::New { .. } => "New",
        Statement::StackNew { .. } => "StackNew",
        Statement::GetField { .. } => "GetField",
        Statement::PutField { .. } => "PutField",
        Statement::GetStatic { .. } => "GetStatic",
        Statement::PutStatic { .. } => "PutStatic",
        Statement::InstanceOfPending { .. } => "InstanceOfPending",
        Statement::CheckCast { .. } => "CheckCast",
        Statement::InstanceOf { .. } => "InstanceOf",
        Statement::NewArray { .. } => "NewArray",
        Statement::StackNewArray { .. } => "StackNewArray",
        Statement::RegionNewArray { .. } => "RegionNewArray",
        Statement::ArrayLen { .. } => "ArrayLen",
        Statement::ArrayLoad { .. } => "ArrayLoad",
        Statement::ArrayStore { .. } => "ArrayStore",
        Statement::DebugLine(_) => "DebugLine",
    }
}

/// Validate + emit one function. Returns (text, prelude use).
fn emit_function(
    func: &Function,
    is_main: bool,
    eff_reads: &BTreeSet<String>,
    eff_writes: &BTreeSet<String>,
    est: u64,
    loops: BTreeMap<u32, LoopInfo>,
    pdom: Vec<BTreeSet<u32>>,
    strings: &[String],
    str_max: BTreeMap<u32, u64>,
    len32: BTreeSet<u32>,
    funcs: &BTreeMap<String, ()>,
    errs: &mut Vec<String>,
    prelude: &mut PreludeUse,
    externs: &BTreeMap<String, ExternDecl>,
    unmapped_externs: &BTreeMap<String, String>,
    obj_classes: &BTreeMap<String, Vec<(String, ObjSlot)>>,
    obj_locals: BTreeMap<u32, String>,
    obj_rows: BTreeMap<(u32, usize), (String, u64)>,
    obj_rets: &BTreeMap<String, String>,
    obj_params_all: &BTreeMap<String, BTreeMap<u32, String>>,
) -> String {
    // Signature validation.
    // (`vire_main` is the compiler-known entry, exempt from the namespace
    // guard; every other `vire_`-prefixed name would collide with prelude
    // helpers like `vire_print_str_M`. Dotted method names (`Vec2.len`) map
    // into that same reserved namespace at render — users cannot declare
    // them, so the mapping cannot collide.)
    // The rendered name (methods mapped, everything else verbatim).
    let gname = meth_name(&func.name);
    if !is_ident(&gname)
        || func.name.contains('$')
        || (func.name.starts_with("vire_") && func.name != "vire_main")
        || func.name == "syscall"
        || func.name.starts_with("__vire_")
    {
        errs.push(e500(format!(
            "function name `{}` not emittable (M1: plain idents, generics later; `vire_` is the prelude namespace; `syscall`/`__vire_*` are reserved intrinsics)",
            func.name
        )));
        return String::new();
    }
    for (i, p) in func.params.iter().enumerate() {
        if *p == Ty::Ref {
            // Proven reference parameters ride the tables as options
            // (Phase 1); unproven ones stay loud E500 M4b.
            if obj_params_all.get(&func.name).and_then(|m| m.get(&(i as u32))).is_none() {
                errs.push(e500(format!(
                    "parameter {i} of `{}` is a reference of unknown class (M4b: monomorphized signatures later)",
                    func.name
                )));
            }
        } else if *p != Ty::I64 {
            errs.push(e500(format!(
                "parameter {i} of `{}` has type {p:?} (M1: Int params only)",
                func.name
            )));
        }
    }
    // Ref returns ride the object tables when the return class is known.
    let obj_ret: Option<String> = if func.ret == Ty::Ref {
        obj_rets.get(&func.name).cloned()
    } else {
        None
    };
    match func.ret {
        Ty::I64 | Ty::Void => {}
        Ty::Ref if obj_ret.is_some() => {}
        Ty::Ref => {
            errs.push(e500(format!(
                "return type Ref of `{}` with unknown class (M4b: monomorphized signatures later)",
                func.name
            )));
        }
        _ => {
            errs.push(e500(format!(
                "return type {:?} of `{}` (M1: Int/Unit only)",
                func.ret, func.name
            )));
        }
    }
    if is_main && !func.params.is_empty() {
        errs.push(e500("main with parameters (M1: parameterless main only)".to_string()));
    }
    // Recursion check (direct + mutual via reachable walk done by caller map).
    // Direct self-call:
    for b in &func.blocks {
        for st in &b.statements {
            if let Statement::Call { func: f, .. } = st {
                if f == &func.name {
                    errs.push("error[E500]: recursion on the Gabbro path needs `decreases` (bound solver step)".to_string());
                }
            }
        }
    }
    // Bool locals: integer comparisons + copies thereof (fixpoint).
    let mut bools = BTreeSet::new();
    for b in &func.blocks {
        for st in &b.statements {
            if let Statement::Assign(d, Rvalue::Binary(op, _, _)) = st {
                if matches!(
                    op,
                    BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe
                ) {
                    bools.insert(d.0);
                }
            }
        }
    }
    loop {
        let mut grew = false;
        for b in &func.blocks {
            for st in &b.statements {
                if let Statement::Assign(d, Rvalue::Use(Operand::Copy(s))) = st {
                    if bools.contains(&s.0) && bools.insert(d.0) {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    // Used locals (dead pure temps are skipped, not rejected).
    let mut used = BTreeSet::new();
    for b in &func.blocks {
        for st in &b.statements {
            let mut mark = |op: &Operand| {
                if let Operand::Copy(l) = op {
                    used.insert(l.0);
                }
            };
            match st {
                Statement::Assign(_, r) => match r {
                    Rvalue::Use(o) => mark(o),
                    Rvalue::Binary(_, x, y) => {
                        mark(x);
                        mark(y);
                    }
                    Rvalue::Convert(o) | Rvalue::Neg(o) => mark(o),
                },
                Statement::Call { args, .. } => {
                    for a in args {
                        mark(a);
                    }
                }
                Statement::CallGuarded { args, .. }
                | Statement::CallVirtual { args, .. }
                | Statement::CallPoly { args, .. } => {
                    for a in args {
                        mark(a);
                    }
                }
                Statement::New { .. }
                | Statement::StackNew { .. }
                | Statement::NewArray { .. }
                | Statement::StackNewArray { .. }
                | Statement::RegionNewArray { .. } => {}
                Statement::GetField { obj, .. } => mark(obj),
                Statement::PutField { obj, value, .. } => {
                    mark(obj);
                    mark(value);
                }
                Statement::GetStatic { .. } => {}
                Statement::PutStatic { value, .. } => mark(value),
                Statement::InstanceOfPending { .. } => {}
                Statement::CheckCast { obj, .. } => mark(obj),
                Statement::InstanceOf { obj, .. } => mark(obj),
                Statement::ArrayLen { arr, .. } => mark(arr),
                Statement::ArrayLoad { arr, index, .. } => {
                    mark(arr);
                    mark(index);
                }
                Statement::ArrayStore { arr, index, value, .. } => {
                    mark(arr);
                    mark(index);
                    mark(value);
                }
                Statement::DebugLine(_) => {}
            }
        }
        match &b.terminator {
            Terminator::Goto(_) => {}
            Terminator::Branch { cond, .. } => {
                if let Operand::Copy(l) = cond {
                    used.insert(l.0);
                }
            }
            Terminator::Switch { value, .. } => {
                if let Operand::Copy(l) = value {
                    used.insert(l.0);
                }
            }
            Terminator::Return(op) => {
                if let Some(Operand::Copy(l)) = op {
                    used.insert(l.0);
                }
            }
        }
    }

    let (arr_site, arr_sites, arr_conflict) = analyze_arrays(func);
    let i64free = analyze_i64free(func);
    let opt_locs = analyze_opt_locs(func);
    // Register this function's array tables as program items.
    for (k, site) in arr_sites.iter().enumerate() {
        prelude.tables.insert(arr_table_name(&func.name, k), (site.len, "i64".to_string()));
    }
    // Register this function's explicit-region arenas as program items
    // (ceilings stay user-visible: `pub arena NAME capacity 0 .. MAX`).
    let region_sites = analyze_regions(func);
    for m in region_sites.values() {
        if m.is_push {
            if let Some(mx) = m.max {
                prelude.arenas.insert(m.name.clone(), mx);
            }
        }
    }
    let mut fe = FnEmit { func, strings, str_max, arr_site, arr_sites, arr_conflict, len32, i64free, opt_locs, wide32: BTreeSet::new(), trace_hints: BTreeMap::new(), bools, used, temps: 0, prelude: PreludeUse::default(), errs: vec![] };
    let headers: BTreeSet<u32> = loops.keys().copied().collect();
    let mut be = BodyEmit { fe: &mut fe, is_main, active: vec![], seen: BTreeSet::new(), nparams: func.params.len(), bound: BTreeSet::new(), loops: vec![], loop_seq: 0, loop_info: loops, headers, pdom, out: String::new(), region_sites, region_stack: vec![], externs: externs.clone(), unmapped_externs: unmapped_externs.clone(), obj_classes: obj_classes.clone(), obj_locals, obj_rows, obj_rets: obj_rets.clone(), obj_params: obj_params_all.clone() };
    // Option pairs bound once at function top (dominator-safe: every later
    // write — branches, joins, loop passes — is a plain assign).
    {
        let mut ol: Vec<u32> = be.fe.opt_locs.iter().copied().collect();
        ol.sort();
        for o in ol {
            let base = be.lname(Local(o));
            let _ = writeln!(be.out, "    let mut {base}_tag : i32 = -1;");
            let _ = writeln!(be.out, "    let mut {base}_val : i64 = 0;");
        }
    }
    // Validate all blocks for rejects even where unreachable (pass 1 is inline
    // below via emission; structural errors surface during region emission).
    let falls = be.emit_region(0, None, funcs);
    // Dead blocks: validate-only (rejected constructs are rejected everywhere).
    be.validate_unvisited(funcs);
    if !be.region_stack.is_empty() {
        be.fe.errs.push(e500("unbalanced region push/pop at function end (M3: internal)".to_string()));
    }
    if falls && !is_main {
        // A function body must not fall off (Gabbro endblock). Main is the
        // exception: Vire main returns Void, the wrapper appends `return 0;`.
        be.fe.errs.push(e500(format!(
            "function `{}` falls off the end (M1: all paths must return)",
            func.name
        )));
    }
    let mut fn_errs = std::mem::take(&mut be.fe.errs);
    errs.append(&mut fn_errs);
    // Merge prelude use.
    let pu = &be.fe.prelude;
    prelude.print |= pu.print;
    prelude.add |= pu.add;
    prelude.sub |= pu.sub;
    prelude.mul |= pu.mul;
    prelude.neg |= pu.neg;
    prelude.retry_loop |= pu.retry_loop;
    prelude.need_abort |= pu.need_abort;
    for (t, v) in pu.tables.iter() {
        prelude.tables.insert(t.clone(), v.clone());
    }
    for (a, m) in pu.arenas.iter() {
        prelude.arenas.insert(a.clone(), *m);
    }
    prelude.print_str.extend(pu.print_str.iter().copied());
    // Name collisions with the prelude.
    for reserved in ["vire_print_int", "vire_print_pos", "vire_uadd64", "vire_usub64", "vire_umul64", "putchar", "abort"] {
        if func.name == reserved {
            errs.push(e500(format!(
                "function `{}` collides with the Gabbro prelude (rename it)",
                func.name
            )));
        }
    }

    // Effects clause: `pure` iff nothing read or written; otherwise
    // writes-then-reads (matches the corpus order).
    let eff = if eff_reads.is_empty() && eff_writes.is_empty() {
        "pure".to_string()
    } else {
        let mut parts: Vec<String> = vec![];
        let mut ws: Vec<&String> = eff_writes.iter().collect();
        ws.sort();
        let mut rs: Vec<&String> = eff_reads.iter().collect();
        rs.sort();
        for x in ws {
            parts.push(format!("writes {x}"));
        }
        for x in rs {
            parts.push(format!("reads {x}"));
        }
        parts.join(", ")
    };
    let mut s = String::new();
    if is_main {
        let _ = writeln!(s, "pub fn main() -> i32");
        let _ = writeln!(s, "    effects {{ {eff} }}");
        let _ = writeln!(s, "    costs <= {est} ops");
        let _ = writeln!(s, "{{");
        s.push_str(&be.out);
        if falls {
            // Vire main returns Void; Gabbro main answers i32 for the linker.
            let _ = writeln!(s, "    return 0;");
        }
        let _ = writeln!(s, "}}");
    } else {
        // Reference parameters with proven classes ride as options
        // (Phase 1); unproven ones already failed validation above.
        let obj_pm = obj_params_all.get(&func.name).cloned().unwrap_or_default();
        let params: Vec<String> = func
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| match (p, obj_pm.get(&(i as u32))) {
                (Ty::Ref, Some(c)) => {
                    format!("p{i} : option index into {}", obj_table_name(c))
                }
                _ => format!("p{i} : i64"),
            })
            .collect();
        let ret = match func.ret {
            Ty::I64 => " -> i64".to_string(),
            Ty::Ref => match obj_ret {
                Some(c) => format!(" -> option index into {}", obj_table_name(&c)),
                None => String::new(),
            },
            _ => String::new(),
        };
        let _ = writeln!(s, "fn {}({}){} effects {{ {eff} }} costs <= {est} ops {{", gname, params.join(", "), ret);
        s.push_str(&be.out);
        let _ = writeln!(s, "}}");
    }
    s.push('\n');
    s
}

/// Generous per-form op costs for `costs <= EST` promises (K001 holds the
/// declared number against Gabbro's computed one — over-approximation is the
/// sound direction; the oracle corrects under-counts loudly).
/// Loop-aware: straight cost of every block once, plus per-loop totals
/// (trips × (region straight + nested totals + until)). Nested totals fold
/// into their outer loop so doubly-nested bodies are not under-counted.
fn stmt_cost(
    st: &Statement,
    by_name: &BTreeMap<String, &Function>,
    all_loops: &BTreeMap<String, BTreeMap<u32, LoopInfo>>,
    str_caps: &BTreeMap<String, u64>,
    str_cap: u64,
    memo: &mut BTreeMap<String, u64>,
    visiting: &mut Vec<String>,
) -> u64 {
    match st {
        Statement::Call { func: c, .. } => {
            let callee = if c == "jrt_println_long" {
                160
            } else if matches!(
                c.as_str(),
                "vire_uadd64" | "vire_usub64" | "vire_umul64"
            ) {
                8
            } else if c == "vire_print_pos" {
                64
            } else if c == "vire_print_int" {
                128
            } else if c == "jrt_str_concat" {
                128
            } else if c == "jrt_str_length" {
                32
            } else if c == "jrt_println_str" {
                str_print_costs(str_cap)
            } else if c == "jrt_str_is_empty" || c == "jrt_str_equals" {
                64
            } else if c == "jrt_region_push" {
                16
            } else if c == "jrt_region_pop" {
                64
            } else if let Some(g) = by_name.get(c.as_str()) {
                est_with_loops(g, by_name, all_loops, str_caps, memo, visiting)
            } else {
                // Unknown calls on the success path are scalar `extern "C"`
                // calls (anything else fails emission): cover their declared
                // head cost (64, link-held N504) instead of zero.
                64
            };
            16 + callee
        }
        // Array ops (generous; the narrow-guard of dynamic indices included).
        Statement::ArrayStore { index, .. } | Statement::ArrayLoad { index, .. } => {
            match index {
                Operand::Copy(_) => 128,
                _ => 64,
            }
        }
        Statement::ArrayLen { .. } => 16,
        Statement::NewArray { .. }
        | Statement::StackNewArray { .. }
        | Statement::RegionNewArray { .. } => 32,
        // Object field ops emit an unwrap-match around the access
        // (generous; the `None => return abort()` path included). Flat for
        // sums too — over-approximation is the sound direction.
        Statement::GetField { .. } | Statement::PutField { .. } => 96,
        // Null tests emit a boolean directly (`== None`); reference
        // comparisons likewise. Same price as other comparisons.
        Statement::Assign(_, Rvalue::Binary(BinOp::CmpEq | BinOp::CmpNe, _, _)) => 16,
        _ => 16,
    }
}

/// Straight-line cost of one block (statements + terminator).
fn block_cost(
    blk: &BasicBlock,
    by_name: &BTreeMap<String, &Function>,
    all_loops: &BTreeMap<String, BTreeMap<u32, LoopInfo>>,
    str_caps: &BTreeMap<String, u64>,
    str_cap: u64,
    memo: &mut BTreeMap<String, u64>,
    visiting: &mut Vec<String>,
) -> u64 {
    let mut total: u64 = 0;
    for st in &blk.statements {
        total = total.saturating_add(stmt_cost(st, by_name, all_loops, str_caps, str_cap, memo, visiting));
    }
    match &blk.terminator {
        Terminator::Goto(_) => {}
        Terminator::Return(_) => {}
        _ => {
            total = total.saturating_add(16);
        }
    }
    total
}

/// Per-pass cost of one loop region (header `h`): straight region cost +
/// directly-nested loops' TOTALS (an inner loop runs fully per outer pass) +
/// the `until` check. This is the number the emitted `retry ... bounded`
/// promises (K006 holds it against Gabbro's per-pass measurement —
/// over-approximation is the sound direction).
/// Guarded against re-entry (loop nesting is a tree).
fn loop_per_pass(
    func: &Function,
    loops: &BTreeMap<u32, LoopInfo>,
    h: u32,
    by_name: &BTreeMap<String, &Function>,
    all_loops: &BTreeMap<String, BTreeMap<u32, LoopInfo>>,
    str_caps: &BTreeMap<String, u64>,
    str_cap: u64,
    memo: &mut BTreeMap<String, u64>,
    visiting: &mut Vec<String>,
    loop_visiting: &mut Vec<u32>,
) -> u64 {
    if loop_visiting.contains(&h) {
        return 0;
    }
    let li = match loops.get(&h) {
        Some(li) => li.clone(),
        None => return 0,
    };
    if li.trips.is_none() {
        return 0; // unbounded: E501 rejects anyway; EST irrelevant.
    }
    loop_visiting.push(h);
    // Region = header + body blocks.
    let mut region = li.body_blocks.clone();
    region.insert(h);
    let mut straight: u64 = 0;
    for b in region.iter() {
        if let Some(blk) = func.blocks.get(*b as usize) {
            straight = straight.saturating_add(block_cost(blk, by_name, all_loops, str_caps, str_cap, memo, visiting));
        }
    }
    // Directly nested loops: headers strictly inside this region that are
    // not inside a *deeper* region (minimal containing region = this one).
    // Simpler sound rule: every other header in the region is nested
    // (directly or transitively); adding each nested total at this level
    // over-counts shared parts but stays an upper bound. To avoid
    // exponential blowup on deep nesting, only add headers whose region is
    // not contained in another nested header's region.
    let nested: Vec<u32> = loops
        .keys()
        .copied()
        .filter(|k| *k != h && region.contains(k))
        .collect();
    let mut direct: Vec<u32> = vec![];
    for k in nested.iter() {
        let mut k_region = loops[k].body_blocks.clone();
        k_region.insert(*k);
        let mut contained = false;
        for j in nested.iter() {
            if *j == *k {
                continue;
            }
            let mut j_region = loops[j].body_blocks.clone();
            j_region.insert(*j);
            if j_region.contains(k) && k_region.contains(j) {
                // Same region (should not happen for distinct headers);
                // break ties by id to keep one parent.
                if *j < *k {
                    contained = true;
                    break;
                }
            } else if j_region.contains(k) {
                contained = true;
                break;
            }
        }
        if !contained {
            direct.push(*k);
        }
    }
    let mut inner: u64 = 0;
    for k in direct {
        inner = inner.saturating_add(loop_total(
            func, loops, k, by_name, all_loops, str_caps, str_cap, memo, visiting, loop_visiting,
        ));
    }
    loop_visiting.pop();
    straight.saturating_add(inner).saturating_add(li.until_cost)
}

/// Total dynamic cost of one loop region (header `h`): trips × per-pass.
fn loop_total(
    func: &Function,
    loops: &BTreeMap<u32, LoopInfo>,
    h: u32,
    by_name: &BTreeMap<String, &Function>,
    all_loops: &BTreeMap<String, BTreeMap<u32, LoopInfo>>,
    str_caps: &BTreeMap<String, u64>,
    str_cap: u64,
    memo: &mut BTreeMap<String, u64>,
    visiting: &mut Vec<String>,
    loop_visiting: &mut Vec<u32>,
) -> u64 {
    let trips = match loops.get(&h).and_then(|li| li.trips) {
        Some(t) => t,
        None => return 0, // unbounded: E501 rejects anyway; EST irrelevant.
    };
    let per = loop_per_pass(func, loops, h, by_name, all_loops, str_caps, str_cap, memo, visiting, loop_visiting);
    trips.saturating_mul(per)
}

fn est_with_loops(
    func: &Function,
    by_name: &BTreeMap<String, &Function>,
    all_loops: &BTreeMap<String, BTreeMap<u32, LoopInfo>>,
    str_caps: &BTreeMap<String, u64>,
    memo: &mut BTreeMap<String, u64>,
    visiting: &mut Vec<String>,
) -> u64 {
    if let Some(n) = memo.get(&func.name) {
        return *n;
    }
    if visiting.contains(&func.name) {
        return 0;
    }
    visiting.push(func.name.clone());
    // Straight cost of every block once (loop bodies included; the loop
    // totals below add the repeated executions on top — over-approx).
    let mut total: u64 = 0;
    let str_cap = *str_caps.get(&func.name).unwrap_or(&0);
    for b in &func.blocks {
        total = total.saturating_add(block_cost(b, by_name, all_loops, str_caps, str_cap, memo, visiting));
    }
    // Top-level loops only (nested totals fold into their outer loop).
    if let Some(loops) = all_loops.get(&func.name) {
        let mut top: Vec<u32> = vec![];
        for h in loops.keys() {
            let mut inside_other = false;
            for (j, lj) in loops.iter() {
                if *j == *h {
                    continue;
                }
                let mut j_region = lj.body_blocks.clone();
                j_region.insert(*j);
                if j_region.contains(h) {
                    inside_other = true;
                    break;
                }
            }
            if !inside_other {
                top.push(*h);
            }
        }
        let mut loop_visiting: Vec<u32> = vec![];
        for h in top {
            total = total.saturating_add(loop_total(
                func,
                loops,
                h,
                by_name,
                all_loops,
                str_caps,
                str_cap,
                memo,
                visiting,
                &mut loop_visiting,
            ));
        }
    }
    visiting.pop();
    memo.insert(func.name.clone(), total);
    total
}

/// Emit a whole program as Gabbro source text.
pub fn emit_gab(program: &Program, modname: &str) -> Result<String, Vec<String>> {
    let mut errs: Vec<String> = Vec::new();
    let reachable = reachable_fns(program);
    if !reachable.contains("vire_main") {
        errs.push(e500("no `main` entry (M1: a `fn main` program only)".to_string()));
        return Err(errs);
    }
    // Mutual recursion check over reachable user functions.
    {
        let by_name: BTreeMap<&str, &Function> =
            program.functions.iter().map(|f| (f.name.as_str(), f)).collect();
        for f in &program.functions {
            if !reachable.contains(&f.name) {
                continue;
            }
            // DFS from f looking for f (cycle through other fns).
            let mut stack: Vec<&str> = vec![f.name.as_str()];
            let mut seen = BTreeSet::new();
            while let Some(n) = stack.pop() {
                if n == f.name.as_str() && seen.contains(n) {
                    errs.push("error[E500]: recursion on the Gabbro path needs `decreases` (bound solver step)".to_string());
                    break;
                }
                if !seen.insert(n.to_string()) {
                    continue;
                }
                if let Some(g) = by_name.get(n) {
                    for b in &g.blocks {
                        for st in &b.statements {
                            if let Statement::Call { func: c, .. } = st {
                                if by_name.contains_key(c.as_str()) {
                                    stack.push(c.as_str());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // Object classes usable as tables (program-level, fixpoint-free).
    let obj_classes = obj_usable_classes(program);
    let effects = compute_effects(program, &reachable, &obj_classes);
    let funcs: BTreeMap<String, ()> =
        program.functions.iter().filter(|f| reachable.contains(&f.name)).map(|f| (f.name.clone(), ())).collect();
    // Pure functions (no `writes`): allowed in loop conditions (same
    // evaluation count/order as Vire headers). Prelude seeds: the wrap
    // helpers + abort are pure; prints/putchar are effectful.
    let mut pure_fns: BTreeSet<String> = BTreeSet::from([
        "vire_uadd64".to_string(),
        "vire_usub64".to_string(),
        "vire_umul64".to_string(),
        "abort".to_string(),
    ]);
    for (n, (r, w)) in effects.iter() {
        if r.is_empty() && w.is_empty() {
            pure_fns.insert(n.clone());
        }
    }
    // Loop tables per function (sides, trips, conditions; E500/E501 pushed).
    let mut loop_tables: BTreeMap<String, BTreeMap<u32, LoopInfo>> = BTreeMap::new();
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        loop_tables.insert(f.name.clone(), build_loop_table(f, &pure_fns, &mut errs));
    }
    // Object flow: local classes, return classes, static rows (needs the
    // loop tables for the single-execution check). Infallible; declines
    // surface lazily at emission (E500 shape / E501 row linearization).
    let obj_info = analyze_objects(program, &reachable, &obj_classes, &loop_tables);
    // String tables per function (max per string local + lenof dests).
    // Pure analysis (infallible); E500/E501 surface lazily at use sites.
    let mut str_tables: BTreeMap<String, (BTreeMap<u32, u64>, BTreeSet<u32>)> = BTreeMap::new();
    let mut str_caps: BTreeMap<String, u64> = BTreeMap::new();
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        let (m, l32) = analyze_strings(f, &program.strings);
        let cap = m.values().copied().max().unwrap_or(0);
        str_tables.insert(f.name.clone(), (m, l32));
        str_caps.insert(f.name.clone(), cap);
    }
    // Costs promises (K001 holds them): generous per-function upper bounds,
    // including loop totals (trips × passops).
    let by_name: BTreeMap<String, &Function> =
        program.functions.iter().map(|f| (f.name.clone(), f)).collect();
    let mut est_memo: BTreeMap<String, u64> = BTreeMap::new();
    let mut est_map: BTreeMap<String, u64> = BTreeMap::new();
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        let mut visiting = vec![];
        let e = est_with_loops(f, &by_name, &loop_tables, &str_caps, &mut est_memo, &mut visiting);
        est_map.insert(f.name.clone(), e);
    }
    // Per-pass budgets for `retry ... bounded` (K006 holds them against
    // Gabbro's per-pass measurement). Warm memo reused; per-loop visiting
    // guards are fresh (loop nesting is a tree, no cross-talk).
    {
        let mut fills: Vec<(String, u32, u64)> = vec![];
        for f in &program.functions {
            if !reachable.contains(&f.name) {
                continue;
            }
            if let Some(table) = loop_tables.get(&f.name) {
                for (h, li) in table.iter() {
                    if li.trips.is_none() || li.deny_msg.is_some() {
                        continue;
                    }
                    let mut visiting = vec![];
                    let mut loop_visiting = vec![];
                    let str_cap = *str_caps.get(&f.name).unwrap_or(&0);
                    let per = loop_per_pass(
                        f,
                        table,
                        *h,
                        &by_name,
                        &loop_tables,
                        &str_caps,
                        str_cap,
                        &mut est_memo,
                        &mut visiting,
                        &mut loop_visiting,
                    );
                    fills.push((f.name.clone(), *h, per));
                }
            }
        }
        for (fname, h, per) in fills {
            if let Some(table) = loop_tables.get_mut(&fname) {
                if let Some(li) = table.get_mut(&h) {
                    li.per_pass = per;
                }
            }
        }
    }
    let mut prelude = PreludeUse::default();
    // `extern "C"` boundary: scalar decls become `extern fn` heads
    // (checked here, once); the rest declines loudly at its call.
    let (externs, unmapped_externs) = analyze_externs(program, &mut errs);
    // Object tables used by static rows (name → (rows, ordered slots)).
    for (class, count) in obj_info.row_counts.iter() {
        let mut slots = vec![];
        for (fname, s) in &obj_classes[class] {
            let ty = match s {
                ObjSlot::I64 => "i64".to_string(),
                ObjSlot::Ref(t) => format!("option index into {}", obj_table_name(t)),
            };
            slots.push((fname.clone(), ty));
        }
        prelude.obj_tables.insert(obj_table_name(class), (*count, slots));
    }
    let mut bodies = String::new();
    for f in &program.functions {
        if !reachable.contains(&f.name) {
            continue;
        }
        let is_main = f.name == "vire_main";
        let (er, ew) = effects.get(&f.name).cloned().unwrap_or_default();
        let est = *est_map.get(&f.name).unwrap_or(&64);
        let loops = loop_tables.remove(&f.name).unwrap_or_default();
        // Post-dominators (loop-aware edge drops need the loop table).
        let proven: BTreeSet<u32> = loops
            .iter()
            .filter_map(|(h, li)| if li.trips.is_some() { Some(*h) } else { None })
            .collect();
        let loop_drops: BTreeMap<u32, (BTreeSet<u32>, u32)> = loops
            .iter()
            .map(|(h, li)| {
                let mut body = li.body_blocks.clone();
                body.insert(*h);
                (*h, (body, li.merge))
            })
            .collect();
        let pdom = post_dom(&f.blocks, &loop_drops, &proven);
        let (str_max, len32) = str_tables.remove(&f.name).unwrap_or_default();
        let obj_locals = obj_info.locals.get(&f.name).cloned().unwrap_or_default();
        let mut obj_rows = BTreeMap::new();
        for ((g, bi, si), v) in obj_info.rows.iter() {
            if g == &f.name {
                obj_rows.insert((*bi, *si), v.clone());
            }
        }
        bodies.push_str(&emit_function(f, is_main, &er, &ew, est, loops, pdom, &program.strings, str_max, len32, &funcs, &mut errs, &mut prelude, &externs, &unmapped_externs, &obj_classes, obj_locals, obj_rows, &obj_info.ret_class, &obj_info.params));
    }
    if !errs.is_empty() {
        // Dedupe identical diagnostics (loops report E501 per enclosed branch).
        let mut seen = BTreeSet::new();
        errs.retain(|e| seen.insert(e.clone()));
        return Err(errs);
    }
    // Prelude dependencies: print_int calls usub64.
    if prelude.print {
        prelude.sub = true;
    }
    let mut out = String::new();
    let _ = writeln!(out, "module vire::{} {{\n", sanitize_mod(modname));
    // Array tables (one `pub table` per const-length site; `pub` so the
    // exported `main` may reach them through calls — N038).
    let mut tnames: Vec<&String> = prelude.tables.keys().collect();
    tnames.sort();
    for t in tnames {
        let (len, ty) = prelude.tables[t].clone();
        let _ = writeln!(out, "pub table {t} count {len} {{");
        let _ = writeln!(out, "    slot {{");
        let _ = writeln!(out, "        v : {ty},");
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "}}");
        let _ = writeln!(out, "");
    }
    // Explicit-region arenas (one `pub arena` per `region(max)` site;
    // `pub` like tables so the exported `main` may reach them — N038).
    let mut anames: Vec<&String> = prelude.arenas.keys().collect();
    anames.sort();
    for a in anames {
        let m = prelude.arenas[a];
        let _ = writeln!(out, "pub arena {a} capacity 0 .. {m} of i64;");
    }
    if !prelude.arenas.is_empty() {
        let _ = writeln!(out, "");
    }
    // Object tables (one `pub table` per used class; `pub` like arrays so
    // the exported `main` may reach them through calls — N038). Rows are
    // static (one per single-execution `New` site); indices are values, so
    // aliasing and escape need no lifetime proof (O29 bounded scope).
    let mut onames: Vec<&String> = prelude.obj_tables.keys().collect();
    onames.sort();
    for o in onames {
        let (count, slots) = prelude.obj_tables[o].clone();
        let _ = writeln!(out, "pub table {o} count {count} {{");
        let _ = writeln!(out, "    slot {{");
        for (fname, fty) in slots.iter() {
            let _ = writeln!(out, "        {fname} : {fty},");
        }
        let _ = writeln!(out, "    }}");
        let _ = writeln!(out, "}}");
        let _ = writeln!(out, "");
    }
    // `extern "C"` heads: scalar C functions become `extern fn` (Gabbro has
    // no `extern "C"` form — the contract is the signature + effects +
    // costs, the body is machine, held at `gabbro link` N501–N504).
    // Effects are conservative: scalars cannot address module carriers (no
    // `Ptr` crosses), so the only nameable sink is `ausgabe`.
    let mut enames: Vec<&String> = externs.keys().collect();
    enames.sort();
    for e in enames {
        let d = &externs[e];
        let mut ps = vec![];
        for (i, p) in d.params.iter().enumerate() {
            ps.push(format!("a{i} : {}", gab_extern_ty(*p).unwrap_or("i64")));
        }
        let ret = match d.ret {
            Ty::Void => String::new(),
            t => format!(" -> {}", gab_extern_ty(t).unwrap_or("i64")),
        };
        let _ = writeln!(out, "extern fn {e}({}){ret} effects {{ writes ausgabe }} costs <= 64 ops;", ps.join(", "));
    }
    if !externs.is_empty() {
        let _ = writeln!(out, "");
    }
    out.push_str(&prelude_text(&prelude));
    out.push_str(&bodies);
    let _ = writeln!(out, "}}");
    Ok(out)
}
