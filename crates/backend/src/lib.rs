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
    ("jrt_idiv", "i32 (i32, i32)"),
    ("jrt_irem", "i32 (i32, i32)"),
    ("jrt_alloc", "ptr (i64)"),
    ("jrt_null_check", "void (ptr)"),
];

/// Klassen-Kontext: Layouts und Vtables, aus `Program::classes` berechnet.
struct Ctx<'a> {
    program: &'a Program,
}

impl<'a> Ctx<'a> {
    fn class(&self, name: &str) -> Option<&'a ClassInfo> {
        self.program.class(name)
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

    /// GEP-Index (+1 für den Vtable-Header) und Typ eines Felds, aufgelöst
    /// ab `class` die Superkette hoch.
    fn field_slot(&self, class: &str, field: &str) -> Option<(String, usize, Ty)> {
        let (owner, ty) = self.program.resolve_field(class, field)?;
        let owner = owner.to_string();
        let flat = self.flatten_fields(&owner);
        let idx = flat.iter().position(|(o, n, _)| *o == owner && n == field)?;
        Some((owner, idx + 1, ty))
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

    fn vtable_index(&self, class: &str, name: &str, desc: &str) -> Option<usize> {
        self.vtable_slots(class)
            .iter()
            .position(|(n, d, _)| n == name && d == desc)
    }
}

pub fn emit(program: &Program) -> String {
    let mut out = String::new();
    let w = &mut out;
    let ctx = Ctx { program };

    writeln!(w, "; erzeugt von fastllvm (naive Absenkung, siehe DESIGN.md)").unwrap();

    // String-Literale als { i64 len, [n x i8] }.
    for (i, s) in program.strings.iter().enumerate() {
        let bytes = s.as_bytes();
        writeln!(
            w,
            "@jstr.{i} = private unnamed_addr constant {{ i64, [{n} x i8] }} {{ i64 {n}, [{n} x i8] c\"{esc}\" }}",
            n = bytes.len(),
            esc = escape_ll(bytes),
        )
        .unwrap();
    }
    writeln!(w).unwrap();

    // Struct-Typen für alle Klassen.
    for c in &program.classes {
        let mut parts = vec!["ptr".to_string()];
        parts.extend(ctx.flatten_fields(&c.name).iter().map(|(_, _, t)| llty(*t).to_string()));
        writeln!(w, "{} = type {{ {} }}", ctx.struct_name(&c.name), parts.join(", ")).unwrap();
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
            Statement::New { class, .. } => Some(class.as_str()),
            _ => None,
        })
        .collect();
    for class in &instantiated {
        let slots = ctx.vtable_slots(class);
        let entries: Vec<String> = slots
            .iter()
            .map(|(_, _, sym)| match sym {
                Some(s) if defined.contains(s.as_str()) => format!("ptr @{s}"),
                _ => "ptr null".to_string(),
            })
            .collect();
        writeln!(
            w,
            "@vt.{} = internal unnamed_addr constant [{} x ptr] [{}]",
            sanitize(class),
            slots.len().max(1),
            if entries.is_empty() { "ptr null".to_string() } else { entries.join(", ") },
        )
        .unwrap();
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
        writeln!(w, "  call void @java_main()").unwrap();
        writeln!(w, "  ret i32 0").unwrap();
        writeln!(w, "}}").unwrap();
    }

    out
}

fn operand_ty(f: &Function, op: &Operand) -> Ty {
    match op {
        Operand::Copy(l) => f.locals[l.0 as usize],
        Operand::ConstI32(_) => Ty::I32,
        Operand::ConstI64(_) => Ty::I64,
        Operand::ConstStr(_) | Operand::ConstNull => Ty::Ref,
    }
}

struct FnEmitter<'a> {
    f: &'a Function,
    tmp: u32,
}

impl<'a> FnEmitter<'a> {
    fn fresh(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
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
            // ConstI64(0) dient dem Frontend als Ref-Dummy (System.out);
            // in Ref-Kontexten muss daraus null werden.
            Operand::ConstI64(0) => "null".to_string(),
            Operand::ConstI64(v) => v.to_string(),
            Operand::ConstStr(i) => format!("@jstr.{i}"),
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
    for (i, ty) in f.params.iter().enumerate() {
        writeln!(w, "  store {} %p{i}, ptr %l{i}", llty(*ty)).unwrap();
    }
    writeln!(w, "  br label %bb0").unwrap();

    let mut e = FnEmitter { f, tmp: 0 };

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
            Terminator::Return(None) => writeln!(w, "  ret void").unwrap(),
            Terminator::Return(Some(op)) => {
                let ty = llty(operand_ty(f, op));
                let v = e.operand(w, op);
                writeln!(w, "  ret {ty} {v}").unwrap();
            }
        }
    }
    writeln!(w, "}}\n").unwrap();
}

fn emit_statement(w: &mut String, ctx: &Ctx, e: &mut FnEmitter, st: &Statement) {
    match st {
        Statement::Assign(dest, rv) => {
            let dty = llty(e.f.locals[dest.0 as usize]);
            let val = match rv {
                Rvalue::Use(op) => e.operand(w, op),
                Rvalue::Neg(op) => {
                    let v = e.operand(w, op);
                    let t = e.fresh();
                    writeln!(w, "  {t} = sub i32 0, {v}").unwrap();
                    t
                }
                Rvalue::Binary(op, a, b) => {
                    let aty = operand_ty(e.f, a);
                    let av = e.operand(w, a);
                    let bv = e.operand(w, b);
                    emit_binop(w, e, *op, aty, &av, &bv)
                }
            };
            writeln!(w, "  store {dty} {val}, ptr %l{}", dest.0).unwrap();
        }
        Statement::Call { dest, func, args } => {
            let avs = call_args(w, e, args);
            match dest {
                None => writeln!(w, "  call void @{func}({avs})").unwrap(),
                Some(d) => {
                    let rty = llty(e.f.locals[d.0 as usize]);
                    let t = e.fresh();
                    writeln!(w, "  {t} = call {rty} @{func}({avs})").unwrap();
                    writeln!(w, "  store {rty} {t}, ptr %l{}", d.0).unwrap();
                }
            }
        }
        Statement::CallVirtual { dest, class, name, desc, params, ret, args } => {
            let slot = ctx
                .vtable_index(class, name, desc)
                .unwrap_or_else(|| panic!("Vtable-Slot {class}.{name}{desc} fehlt"));
            let recv = e.operand(w, &args[0]);
            writeln!(w, "  call void @jrt_null_check(ptr {recv})").unwrap();
            let vt = e.fresh();
            writeln!(w, "  {vt} = load ptr, ptr {recv}").unwrap();
            let slotp = e.fresh();
            writeln!(w, "  {slotp} = getelementptr ptr, ptr {vt}, i64 {slot}").unwrap();
            let fnp = e.fresh();
            writeln!(w, "  {fnp} = load ptr, ptr {slotp}").unwrap();
            // Receiver wurde schon materialisiert; restliche Argumente jetzt.
            let mut avs = vec![format!("ptr {recv}")];
            for a in &args[1..] {
                let ty = llty(operand_ty(e.f, a));
                let v = e.operand(w, a);
                avs.push(format!("{ty} {v}"));
            }
            let _ = params;
            match dest {
                None => writeln!(w, "  call {} {fnp}({})", llty(*ret), avs.join(", ")).unwrap(),
                Some(d) => {
                    let t = e.fresh();
                    writeln!(w, "  {t} = call {} {fnp}({})", llty(*ret), avs.join(", ")).unwrap();
                    writeln!(w, "  store {} {t}, ptr %l{}", llty(*ret), d.0).unwrap();
                }
            }
        }
        Statement::New { dest, class } => {
            let sn = ctx.struct_name(class);
            let t = e.fresh();
            // sizeof über GEP-Konstante; jrt_alloc nullt (Java-Defaultwerte).
            writeln!(
                w,
                "  {t} = call ptr @jrt_alloc(i64 ptrtoint (ptr getelementptr ({sn}, ptr null, i32 1) to i64))"
            )
            .unwrap();
            writeln!(w, "  store ptr @vt.{}, ptr {t}", sanitize(class)).unwrap();
            writeln!(w, "  store ptr {t}, ptr %l{}", dest.0).unwrap();
        }
        Statement::GetField { dest, obj, class, field } => {
            let (owner, idx, ty) = ctx
                .field_slot(class, field)
                .unwrap_or_else(|| panic!("Feld {class}.{field} fehlt"));
            let o = e.operand(w, obj);
            writeln!(w, "  call void @jrt_null_check(ptr {o})").unwrap();
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr {}, ptr {o}, i32 0, i32 {idx}", ctx.struct_name(&owner)).unwrap();
            let t = e.fresh();
            writeln!(w, "  {t} = load {}, ptr {p}", llty(ty)).unwrap();
            writeln!(w, "  store {} {t}, ptr %l{}", llty(ty), dest.0).unwrap();
        }
        Statement::PutField { obj, class, field, value } => {
            let (owner, idx, ty) = ctx
                .field_slot(class, field)
                .unwrap_or_else(|| panic!("Feld {class}.{field} fehlt"));
            let o = e.operand(w, obj);
            writeln!(w, "  call void @jrt_null_check(ptr {o})").unwrap();
            let v = e.operand(w, value);
            let p = e.fresh();
            writeln!(w, "  {p} = getelementptr {}, ptr {o}, i32 0, i32 {idx}", ctx.struct_name(&owner)).unwrap();
            writeln!(w, "  store {} {v}, ptr {p}", llty(ty)).unwrap();
        }
    }
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
    // Shift-Beträge maskieren (JLS 15.19): b & 31 vor shl/ashr/lshr,
    // sonst wäre der LLVM-Wert bei b >= 32 poison.
    let masked = |w: &mut String, e: &mut FnEmitter, b: &str| -> String {
        let m = e.fresh();
        writeln!(w, "  {m} = and i32 {b}, 31").unwrap();
        m
    };
    let t = e.fresh();
    match op {
        BinOp::Add => writeln!(w, "  {t} = add i32 {a}, {b}").unwrap(),
        BinOp::Sub => writeln!(w, "  {t} = sub i32 {a}, {b}").unwrap(),
        BinOp::Mul => writeln!(w, "  {t} = mul i32 {a}, {b}").unwrap(),
        BinOp::Div => writeln!(w, "  {t} = call i32 @jrt_idiv(i32 {a}, i32 {b})").unwrap(),
        BinOp::Rem => writeln!(w, "  {t} = call i32 @jrt_irem(i32 {a}, i32 {b})").unwrap(),
        BinOp::And => writeln!(w, "  {t} = and i32 {a}, {b}").unwrap(),
        BinOp::Or => writeln!(w, "  {t} = or i32 {a}, {b}").unwrap(),
        BinOp::Xor => writeln!(w, "  {t} = xor i32 {a}, {b}").unwrap(),
        BinOp::Shl => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = shl i32 {a}, {m}").unwrap();
        }
        BinOp::Shr => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = ashr i32 {a}, {m}").unwrap();
        }
        BinOp::UShr => {
            let m = masked(w, e, b);
            writeln!(w, "  {t} = lshr i32 {a}, {m}").unwrap();
        }
        BinOp::CmpEq | BinOp::CmpNe | BinOp::CmpLt | BinOp::CmpGe | BinOp::CmpGt | BinOp::CmpLe => {
            let cc = match op {
                BinOp::CmpEq => "eq",
                BinOp::CmpNe => "ne",
                BinOp::CmpLt => "slt",
                BinOp::CmpGe => "sge",
                BinOp::CmpGt => "sgt",
                _ => "sle",
            };
            let c = e.fresh();
            // Ref-Vergleiche (ifnull, if_acmpeq) laufen über ptr.
            writeln!(w, "  {c} = icmp {cc} {} {a}, {b}", llty(aty)).unwrap();
            writeln!(w, "  {t} = zext i1 {c} to i32").unwrap();
        }
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
