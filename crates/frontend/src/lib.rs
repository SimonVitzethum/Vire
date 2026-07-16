//! Bytecode → Mittel-IR.
//!
//! Standardverfahren: Basic-Block-Aufteilung an Branch-Zielen, dann
//! abstrakte Stack-Simulation — jeder Operandenstack-Slot wird auf ein
//! IR-Local abgebildet (Schlüssel: Tiefe × Typ; der Verifier garantiert
//! typkonsistente Stacks an Join-Punkten, JVMS 4.10).
//!
//! `System.out.println` wird als Intrinsic auf `jrt_println_*` der
//! Mini-Runtime abgebildet (DESIGN.md §6).

use std::collections::HashMap;

use fastllvm_classfile::{ClassFile, Cond, Const, Instr};
use fastllvm_ir::*;

#[derive(Debug)]
pub enum FrontendError {
    Parse(fastllvm_classfile::ParseError),
    Unsupported(String),
}

impl std::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrontendError::Parse(e) => write!(f, "{e}"),
            FrontendError::Unsupported(s) => write!(f, "nicht unterstützt: {s}"),
        }
    }
}

impl std::error::Error for FrontendError {}

impl From<fastllvm_classfile::ParseError> for FrontendError {
    fn from(e: fastllvm_classfile::ParseError) -> Self {
        FrontendError::Parse(e)
    }
}

type Result<T> = std::result::Result<T, FrontendError>;

/// Phase 1: Klassenmodell registrieren (vor dem Absenken aller Methoden,
/// damit Feld-/Methodenauflösung über Klassengrenzen funktioniert).
pub fn register_class(cf: &ClassFile, program: &mut Program) -> Result<()> {
    let mut fields = Vec::new();
    for f in &cf.fields {
        if f.is_static() {
            return Err(FrontendError::Unsupported(format!(
                "statisches Feld {}.{}",
                cf.this_class, f.name
            )));
        }
        let mut chars = f.descriptor.chars().peekable();
        let c = chars.next().ok_or_else(|| FrontendError::Unsupported("leerer Felddeskriptor".into()))?;
        fields.push(FieldInfo { name: f.name.clone(), ty: field_ty(c, &mut chars, &f.descriptor)? });
    }
    let methods = cf
        .methods
        .iter()
        .filter(|m| m.name != "<clinit>")
        .map(|m| MethodInfo {
            name: m.name.clone(),
            desc: m.descriptor.clone(),
            is_static: m.is_static(),
            has_body: m.code.is_some(),
            mangled: mangle(&cf.this_class, &m.name, &m.descriptor),
        })
        .collect();
    program.classes.push(ClassInfo {
        name: cf.this_class.clone(),
        super_name: cf.super_class.clone().filter(|s| s != "java/lang/Object"),
        fields,
        methods,
    });
    Ok(())
}

/// Phase 2: alle Methodenrümpfe absenken.
pub fn lower_class(cf: &ClassFile, program: &mut Program) -> Result<()> {
    for m in &cf.methods {
        if m.name == "<clinit>" {
            // Statische Initialisierer: außerhalb der Teilmenge (keine
            // statischen Felder); javac erzeugt sie hier nicht.
            continue;
        }
        let Some(code) = &m.code else { continue };
        let f = lower_method(cf, m, code, program)?;
        program.functions.push(f);
    }
    Ok(())
}

pub fn mangle(class: &str, name: &str, descriptor: &str) -> String {
    if name == "main" && descriptor == "([Ljava/lang/String;)V" {
        return "java_main".to_string();
    }
    format!("J_{}_{}_{}", sanitize(class), sanitize(name), sanitize(descriptor))
}

/// Parst einen Methodendeskriptor zu (Parametertypen, Rückgabetyp).
fn parse_descriptor(desc: &str) -> Result<(Vec<Ty>, Ty)> {
    let inner = desc
        .strip_prefix('(')
        .and_then(|s| s.split_once(')'))
        .ok_or_else(|| FrontendError::Unsupported(format!("Deskriptor {desc}")))?;
    let (params_s, ret_s) = inner;
    let mut params = Vec::new();
    let mut chars = params_s.chars().peekable();
    while let Some(c) = chars.next() {
        params.push(field_ty(c, &mut chars, desc)?);
    }
    let mut rc = ret_s.chars();
    let ret = match rc.next() {
        Some('V') => Ty::Void,
        Some(c) => field_ty(c, &mut rc.peekable(), desc)?,
        None => return Err(FrontendError::Unsupported(format!("Deskriptor {desc}"))),
    };
    Ok((params, ret))
}

fn field_ty(
    c: char,
    rest: &mut std::iter::Peekable<impl Iterator<Item = char>>,
    desc: &str,
) -> Result<Ty> {
    match c {
        // boolean/byte/short/char sind auf Stack und in Locals int (JVMS 2.11.1).
        'I' | 'Z' | 'B' | 'S' | 'C' => Ok(Ty::I32),
        'J' => Ok(Ty::I64),
        'L' => {
            for c in rest.by_ref() {
                if c == ';' {
                    return Ok(Ty::Ref);
                }
            }
            Err(FrontendError::Unsupported(format!("Deskriptor {desc}")))
        }
        '[' => {
            // Array-Deskriptor konsumieren; Elementtyp egal, Wert ist Ref.
            let n = rest.next().ok_or_else(|| FrontendError::Unsupported(format!("Deskriptor {desc}")))?;
            field_ty(n, rest, desc).map(|_| Ty::Ref)
        }
        _ => Err(FrontendError::Unsupported(format!("Typ {c} in {desc}"))),
    }
}

struct MethodLowering<'a> {
    cf: &'a ClassFile,
    locals: Vec<Ty>,
    /// (JVM-Local-Slot, Typ) → IR-Local. Slots sind untypisiert wiederverwendbar.
    slot_map: HashMap<(u16, Ty), Local>,
    /// (Stack-Tiefe, Typ) → IR-Local.
    stack_map: HashMap<(usize, Ty), Local>,
}

impl<'a> MethodLowering<'a> {
    fn fresh(&mut self, ty: Ty) -> Local {
        self.locals.push(ty);
        Local((self.locals.len() - 1) as u32)
    }

    fn jvm_slot(&mut self, slot: u16, ty: Ty) -> Local {
        if let Some(&l) = self.slot_map.get(&(slot, ty)) {
            return l;
        }
        let l = self.fresh(ty);
        self.slot_map.insert((slot, ty), l);
        l
    }

    fn stack_slot(&mut self, depth: usize, ty: Ty) -> Local {
        if let Some(&l) = self.stack_map.get(&(depth, ty)) {
            return l;
        }
        let l = self.fresh(ty);
        self.stack_map.insert((depth, ty), l);
        l
    }
}

fn lower_method(
    cf: &ClassFile,
    m: &fastllvm_classfile::Method,
    code: &fastllvm_classfile::Code,
    program: &mut Program,
) -> Result<Function> {
    let (mut params, ret) = parse_descriptor(&m.descriptor)?;
    let is_main = m.name == "main" && m.descriptor == "([Ljava/lang/String;)V";
    if is_main {
        // args-Array wird nicht durchgereicht; Slot 0 bleibt ein
        // uninitialisiertes Ref-Local (Nutzung → Linker-/Laufzeitfehler später).
        params = Vec::new();
    } else if !m.is_static() {
        // this belegt JVM-Slot 0 (JVMS 2.6.1).
        params.insert(0, Ty::Ref);
    }

    let cp = &cf.constant_pool;
    let instrs = fastllvm_classfile::decode_code(&code.bytecode, |idx| match cp.get(idx as usize) {
        Some(Const::Integer(v)) => Some(*v),
        _ => None,
    })?;
    let pc_index: HashMap<usize, usize> = instrs.iter().enumerate().map(|(i, (pc, _))| (*pc, i)).collect();

    // Block-Leader bestimmen: Einstieg, Branch-Ziele, Nachfolger von Branches.
    let mut leaders = vec![0usize];
    for (i, (_, instr)) in instrs.iter().enumerate() {
        match instr {
            Instr::IfICmp(_, t) | Instr::IfZero(_, t) => {
                leaders.push(*t);
                if let Some((next_pc, _)) = instrs.get(i + 1) {
                    leaders.push(*next_pc);
                }
            }
            Instr::Goto(t) => {
                leaders.push(*t);
                if let Some((next_pc, _)) = instrs.get(i + 1) {
                    leaders.push(*next_pc);
                }
            }
            Instr::Return | Instr::IReturn => {
                if let Some((next_pc, _)) = instrs.get(i + 1) {
                    leaders.push(*next_pc);
                }
            }
            _ => {}
        }
    }
    leaders.sort_unstable();
    leaders.dedup();
    let block_of_pc: HashMap<usize, Block> =
        leaders.iter().enumerate().map(|(i, pc)| (*pc, Block(i as u32))).collect();

    let mut ml = MethodLowering { cf, locals: Vec::new(), slot_map: HashMap::new(), stack_map: HashMap::new() };

    // Parameter belegen die ersten IR-Locals; JVM-Slot-Zählung beachtet
    // breite Typen (long/double = 2 Slots).
    let mut jvm_slot = 0u16;
    for &p in &params {
        let l = ml.fresh(p);
        ml.slot_map.insert((jvm_slot, p), l);
        jvm_slot += if p == Ty::I64 { 2 } else { 1 };
    }

    // Worklist über Blöcke; Stack-Zustand (Typen) wird an Nachfolger propagiert.
    let mut block_entry_stack: HashMap<Block, Vec<Ty>> = HashMap::new();
    block_entry_stack.insert(Block(0), Vec::new());
    let mut done: Vec<Option<BasicBlock>> = vec![None; leaders.len()];
    let mut worklist = vec![Block(0)];

    while let Some(blk) = worklist.pop() {
        if done[blk.0 as usize].is_some() {
            continue;
        }
        let entry_stack = block_entry_stack[&blk].clone();
        let start_pc = leaders[blk.0 as usize];
        let start_idx = pc_index[&start_pc];
        let end_idx = leaders
            .get(blk.0 as usize + 1)
            .map(|pc| pc_index[pc])
            .unwrap_or(instrs.len());

        let fallthrough = if blk.0 as usize + 1 < leaders.len() {
            Some(Block(blk.0 + 1))
        } else {
            None
        };
        let (bb, succs) = lower_block(
            &mut ml,
            program,
            &instrs[start_idx..end_idx],
            entry_stack,
            &block_of_pc,
            fallthrough,
        )?;
        for (succ, stack) in succs {
            match block_entry_stack.get(&succ) {
                Some(prev) => {
                    if *prev != stack {
                        return Err(FrontendError::Unsupported(format!(
                            "inkonsistenter Stack am Join bb{} in {}.{}",
                            succ.0, cf.this_class, m.name
                        )));
                    }
                }
                None => {
                    block_entry_stack.insert(succ, stack);
                }
            }
            worklist.push(succ);
        }
        done[blk.0 as usize] = Some(bb);
    }

    // Unerreichte Blöcke (z. B. nach javac totem Code) als leere Returns.
    let blocks = done
        .into_iter()
        .map(|b| b.unwrap_or(BasicBlock { statements: Vec::new(), terminator: Terminator::Return(None) }))
        .collect();

    Ok(Function {
        name: mangle(&cf.this_class, &m.name, &m.descriptor),
        params,
        ret,
        locals: ml.locals,
        blocks,
    })
}

/// Senkt einen Block ab. Liefert den fertigen Block plus die Nachfolger mit
/// ihrem Eintritts-Stack (Typen).
fn lower_block(
    ml: &mut MethodLowering,
    program: &mut Program,
    instrs: &[(usize, Instr)],
    entry_stack: Vec<Ty>,
    block_of_pc: &HashMap<usize, Block>,
    fallthrough: Option<Block>,
) -> Result<(BasicBlock, Vec<(Block, Vec<Ty>)>)> {
    // Stack als Liste von Typen; Wert der Tiefe d liegt im Local stack_slot(d, ty).
    let mut stack: Vec<Ty> = entry_stack;
    let mut stmts: Vec<Statement> = Vec::new();

    macro_rules! push {
        ($ty:expr, $rv:expr) => {{
            let ty = $ty;
            let l = ml.stack_slot(stack.len(), ty);
            stmts.push(Statement::Assign(l, $rv));
            stack.push(ty);
            l
        }};
    }
    macro_rules! pop {
        () => {{
            let ty = stack.pop().ok_or_else(|| {
                FrontendError::Unsupported("Stack-Unterlauf (Bytecode außerhalb der Teilmenge?)".into())
            })?;
            ml.stack_slot(stack.len(), ty)
        }};
    }

    let mut terminator: Option<Terminator> = None;
    let mut succs: Vec<(Block, Vec<Ty>)> = Vec::new();
    let mut last_pc_end = 0usize;

    for (pc, instr) in instrs.iter() {
        last_pc_end = *pc;
        match instr {
            Instr::Nop => {}
            Instr::IConst(v) | Instr::LdcInt(v) => {
                push!(Ty::I32, Rvalue::Use(Operand::ConstI32(*v)));
            }
            Instr::LdcString(idx) => {
                let s = match ml.cf.constant_pool.get(*idx as usize) {
                    Some(Const::String { utf8 }) => ml.cf.utf8(*utf8)?,
                    _ => return Err(FrontendError::Unsupported(format!("ldc auf CP-Index {idx}"))),
                };
                let sid = program.intern_string(s);
                push!(Ty::Ref, Rvalue::Use(Operand::ConstStr(sid)));
            }
            Instr::ILoad(slot) => {
                let l = ml.jvm_slot(*slot, Ty::I32);
                push!(Ty::I32, Rvalue::Use(Operand::Copy(l)));
            }
            Instr::ALoad(slot) => {
                let l = ml.jvm_slot(*slot, Ty::Ref);
                push!(Ty::Ref, Rvalue::Use(Operand::Copy(l)));
            }
            Instr::IStore(slot) => {
                let v = pop!();
                let l = ml.jvm_slot(*slot, Ty::I32);
                stmts.push(Statement::Assign(l, Rvalue::Use(Operand::Copy(v))));
            }
            Instr::AStore(slot) => {
                let v = pop!();
                let l = ml.jvm_slot(*slot, Ty::Ref);
                stmts.push(Statement::Assign(l, Rvalue::Use(Operand::Copy(v))));
            }
            Instr::IInc(slot, delta) => {
                let l = ml.jvm_slot(*slot, Ty::I32);
                stmts.push(Statement::Assign(
                    l,
                    Rvalue::Binary(BinOp::Add, Operand::Copy(l), Operand::ConstI32(*delta)),
                ));
            }
            Instr::INeg => {
                let v = pop!();
                push!(Ty::I32, Rvalue::Neg(Operand::Copy(v)));
            }
            Instr::IAdd | Instr::ISub | Instr::IMul | Instr::IDiv | Instr::IRem
            | Instr::IShl | Instr::IShr | Instr::IUShr | Instr::IAnd | Instr::IOr | Instr::IXor => {
                let op = match instr {
                    Instr::IAdd => BinOp::Add,
                    Instr::ISub => BinOp::Sub,
                    Instr::IMul => BinOp::Mul,
                    Instr::IDiv => BinOp::Div,
                    Instr::IRem => BinOp::Rem,
                    Instr::IShl => BinOp::Shl,
                    Instr::IShr => BinOp::Shr,
                    Instr::IUShr => BinOp::UShr,
                    Instr::IAnd => BinOp::And,
                    Instr::IOr => BinOp::Or,
                    _ => BinOp::Xor,
                };
                let b = pop!();
                let a = pop!();
                push!(Ty::I32, Rvalue::Binary(op, Operand::Copy(a), Operand::Copy(b)));
            }
            Instr::Pop => {
                pop!();
            }
            Instr::Dup => {
                let ty = *stack.last().ok_or_else(|| {
                    FrontendError::Unsupported("dup auf leerem Stack".into())
                })?;
                let src = ml.stack_slot(stack.len() - 1, ty);
                push!(ty, Rvalue::Use(Operand::Copy(src)));
            }
            Instr::IfICmp(cond, target)
            | Instr::IfZero(cond, target)
            | Instr::IfACmp(cond, target)
            | Instr::IfRefNull(cond, target) => {
                let (a, b) = match instr {
                    Instr::IfICmp(..) | Instr::IfACmp(..) => {
                        let b = pop!();
                        let a = pop!();
                        (Operand::Copy(a), Operand::Copy(b))
                    }
                    Instr::IfRefNull(..) => {
                        let a = pop!();
                        (Operand::Copy(a), Operand::ConstNull)
                    }
                    _ => {
                        let a = pop!();
                        (Operand::Copy(a), Operand::ConstI32(0))
                    }
                };
                let op = match cond {
                    Cond::Eq => BinOp::CmpEq,
                    Cond::Ne => BinOp::CmpNe,
                    Cond::Lt => BinOp::CmpLt,
                    Cond::Ge => BinOp::CmpGe,
                    Cond::Gt => BinOp::CmpGt,
                    Cond::Le => BinOp::CmpLe,
                };
                let t = ml.fresh(Ty::I32);
                stmts.push(Statement::Assign(t, Rvalue::Binary(op, a, b)));
                // Ein bedingter Branch beendet den Block; der Else-Zweig ist
                // der Fallthrough-Block direkt dahinter.
                let then_blk = block_of_pc[target];
                let else_blk = fallthrough.ok_or_else(|| {
                    FrontendError::Unsupported(format!("Branch ohne Folgeblock bei pc={pc}"))
                })?;
                succs.push((then_blk, stack.clone()));
                succs.push((else_blk, stack.clone()));
                terminator = Some(Terminator::Branch { cond: Operand::Copy(t), then_blk, else_blk });
            }
            Instr::Goto(target) => {
                let blk = block_of_pc[target];
                succs.push((blk, stack.clone()));
                terminator = Some(Terminator::Goto(blk));
            }
            Instr::Return => terminator = Some(Terminator::Return(None)),
            Instr::IReturn | Instr::AReturn => {
                let v = pop!();
                terminator = Some(Terminator::Return(Some(Operand::Copy(v))));
            }
            Instr::AConstNull => {
                push!(Ty::Ref, Rvalue::Use(Operand::ConstNull));
            }
            Instr::New(idx) => {
                let class = ml.cf.class_name(*idx)?.to_string();
                if program.class(&class).is_none() {
                    return Err(FrontendError::Unsupported(format!("new {class} (Klasse nicht im Closed-World-Input)")));
                }
                let ty = Ty::Ref;
                let l = ml.stack_slot(stack.len(), ty);
                stmts.push(Statement::New { dest: l, class });
                stack.push(ty);
            }
            Instr::GetField(idx) => {
                let (class, field, _) = ml.cf.member_ref(*idx)?;
                let (class, field) = (class.to_string(), field.to_string());
                let (_, fty) = program.resolve_field(&class, &field).ok_or_else(|| {
                    FrontendError::Unsupported(format!("getfield {class}.{field}"))
                })?;
                let obj = pop!();
                let l = ml.stack_slot(stack.len(), fty);
                stmts.push(Statement::GetField { dest: l, obj: Operand::Copy(obj), class, field });
                stack.push(fty);
            }
            Instr::PutField(idx) => {
                let (class, field, _) = ml.cf.member_ref(*idx)?;
                let (class, field) = (class.to_string(), field.to_string());
                if program.resolve_field(&class, &field).is_none() {
                    return Err(FrontendError::Unsupported(format!("putfield {class}.{field}")));
                }
                let value = pop!();
                let obj = pop!();
                stmts.push(Statement::PutField {
                    obj: Operand::Copy(obj),
                    class,
                    field,
                    value: Operand::Copy(value),
                });
            }
            Instr::InvokeSpecial(idx) => {
                let (class, name, desc) = ml.cf.member_ref(*idx)?;
                let (class, name, desc) = (class.to_string(), name.to_string(), desc.to_string());
                let (ptys, rty) = parse_descriptor(&desc)?;
                let mut args = Vec::new();
                for _ in &ptys {
                    args.push(Operand::Copy(pop!()));
                }
                let recv = pop!();
                args.push(Operand::Copy(recv));
                args.reverse();
                if class == "java/lang/Object" && name == "<init>" {
                    // Leerer Object-Konstruktor: entfällt.
                    continue;
                }
                // invokespecial dispatcht nicht: Konstruktor, super-Aufruf
                // oder private Methode → direkter Call auf die Auflösung.
                let mangled = program
                    .resolve_method(&class, &name, &desc)
                    .map(|(_, mi)| mi.mangled.clone())
                    .ok_or_else(|| {
                        FrontendError::Unsupported(format!("invokespecial {class}.{name}{desc}"))
                    })?;
                let dest = if rty == Ty::Void {
                    None
                } else {
                    let l = ml.stack_slot(stack.len(), rty);
                    stack.push(rty);
                    Some(l)
                };
                stmts.push(Statement::Call { dest, func: mangled, args });
            }
            Instr::GetStatic(idx) => {
                let (class, name, _) = ml.cf.member_ref(*idx)?;
                if class == "java/lang/System" && (name == "out" || name == "err") {
                    // Receiver-Dummy; das println-Intrinsic ignoriert ihn.
                    push!(Ty::Ref, Rvalue::Use(Operand::ConstI64(0)));
                } else {
                    return Err(FrontendError::Unsupported(format!("getstatic {class}.{name}")));
                }
            }
            Instr::InvokeVirtual(idx) => {
                let (class, name, desc) = ml.cf.member_ref(*idx)?;
                let intrinsic = match (class, name, desc) {
                    ("java/io/PrintStream", "println", "(Ljava/lang/String;)V") => Some("jrt_println_str"),
                    ("java/io/PrintStream", "println", "(I)V") => Some("jrt_println_int"),
                    ("java/io/PrintStream", "println", "()V") => Some("jrt_println_ln"),
                    ("java/io/PrintStream", "print", "(Ljava/lang/String;)V") => Some("jrt_print_str"),
                    ("java/io/PrintStream", "print", "(I)V") => Some("jrt_print_int"),
                    _ => None,
                };
                if let Some(intrinsic) = intrinsic {
                    let arg = if desc.starts_with("()") { None } else { Some(pop!()) };
                    pop!(); // Receiver (System.out-Dummy)
                    stmts.push(Statement::Call {
                        dest: None,
                        func: intrinsic.to_string(),
                        args: arg.into_iter().map(Operand::Copy).collect(),
                    });
                    continue;
                }
                let (class, name, desc) = (class.to_string(), name.to_string(), desc.to_string());
                if program.class(&class).is_none() {
                    return Err(FrontendError::Unsupported(format!(
                        "invokevirtual {class}.{name}{desc}"
                    )));
                }
                let (ptys, rty) = parse_descriptor(&desc)?;
                let mut args = Vec::new();
                for _ in &ptys {
                    args.push(Operand::Copy(pop!()));
                }
                let recv = pop!();
                args.push(Operand::Copy(recv));
                args.reverse();
                let dest = if rty == Ty::Void {
                    None
                } else {
                    let l = ml.stack_slot(stack.len(), rty);
                    stack.push(rty);
                    Some(l)
                };
                let mut params = vec![Ty::Ref];
                params.extend(ptys);
                stmts.push(Statement::CallVirtual { dest, class, name, desc, params, ret: rty, args });
            }
            Instr::InvokeStatic(idx) => {
                let (class, name, desc) = ml.cf.member_ref(*idx)?;
                let (ptys, rty) = parse_descriptor(desc)?;
                let mut args = Vec::new();
                for _ in &ptys {
                    args.push(Operand::Copy(pop!()));
                }
                args.reverse();
                let dest = if rty == Ty::Void { None } else { Some(push!(rty, Rvalue::Use(Operand::ConstI32(0)))) };
                // Der Platzhalter-Assign von push! wird durch den Call ersetzt:
                if dest.is_some() {
                    stmts.pop();
                }
                stmts.push(Statement::Call { dest, func: mangle(class, name, desc), args });
            }
        }
    }

    // Block endet ohne expliziten Sprung → Fallthrough in den Folgeblock.
    let terminator = match terminator {
        Some(t) => t,
        None => {
            let blk = fallthrough.ok_or_else(|| {
                FrontendError::Unsupported(format!("Code endet ohne Return bei pc={last_pc_end}"))
            })?;
            succs.push((blk, stack.clone()));
            Terminator::Goto(blk)
        }
    };
    Ok((BasicBlock { statements: stmts, terminator }, succs))
}
