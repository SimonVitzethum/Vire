//! Decoder for the supported bytecode subset (JVMS ch. 6):
//! int arithmetic, control flow, static calls, getstatic/invokevirtual
//! (for the println intrinsic), string/int constants.

use crate::ParseError;

/// Comparison condition for branch instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    Eq,
    Ne,
    Lt,
    Ge,
    Gt,
    Le,
}

/// Array element type. The stack/value type is int for Bool/Byte/Char/Short,
/// but the storage width is 1/2 byte (narrow arrays, Rust memory profile).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrTy {
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

/// A decoded instruction. `pc` values in branch targets are absolute
/// bytecode offsets (already computed from the relative offsets).
#[derive(Debug, Clone)]
pub enum Instr {
    Nop,
    IConst(i32),
    LConst(i64),
    FConst(f32),
    DConst(f64),
    /// ldc2_w: long/double from the constant pool.
    Ldc2W(u16),
    LdcString(u16),
    LdcInt(i32),
    ILoad(u16),
    IStore(u16),
    LLoad(u16),
    LStore(u16),
    FLoad(u16),
    FStore(u16),
    DLoad(u16),
    DStore(u16),
    ALoad(u16),
    AStore(u16),
    IInc(u16, i32),
    IAdd,
    ISub,
    IMul,
    IDiv,
    IRem,
    INeg,
    IShl,
    IShr,
    IUShr,
    IAnd,
    IOr,
    IXor,
    LAdd,
    LSub,
    LMul,
    LDiv,
    LRem,
    LNeg,
    LShl,
    LShr,
    LUShr,
    LAnd,
    LOr,
    LXor,
    LCmp,
    DAdd,
    DSub,
    DMul,
    DDiv,
    DRem,
    DNeg,
    DCmpL,
    DCmpG,
    FAdd,
    FSub,
    FMul,
    FDiv,
    FRem,
    FNeg,
    FCmpL,
    FCmpG,
    I2L,
    I2D,
    L2I,
    L2D,
    D2I,
    D2L,
    I2F,
    L2F,
    F2I,
    F2L,
    F2D,
    D2F,
    Pop,
    Pop2,
    Dup,
    Dup2,
    /// if_icmp<cond>: compares two stack values.
    IfICmp(Cond, usize),
    /// if<cond>: compares a stack value with 0.
    IfZero(Cond, usize),
    Goto(usize),
    /// tableswitch/lookupswitch: (default target, [(key, target)]) with
    /// absolute bytecode offsets.
    Switch(usize, Vec<(i32, usize)>),
    IReturn,
    LReturn,
    FReturn,
    DReturn,
    AReturn,
    Return,
    GetStatic(u16),
    PutStatic(u16),
    GetField(u16),
    PutField(u16),
    InvokeVirtual(u16),
    InvokeSpecial(u16),
    InvokeStatic(u16),
    InvokeInterface(u16),
    InvokeDynamic(u16),
    New(u16),
    CheckCast(u16),
    InstanceOf(u16),
    /// newarray with primitive element type. byte/boolean/char/short are kept as
    /// an int array (4 bytes) — javac truncates the values before the store
    /// (i2b/i2c/i2s), so it is value-correct.
    NewArrayPrim(ArrTy),
    /// anewarray: array of references (class index, ignored here).
    NewArrayRef(u16),
    ArrayLength,
    AThrow,
    /// monitorenter/monitorexit — pops objectref, calls the runtime lock
    /// (real under --threads, otherwise a no-op).
    MonitorEnter,
    MonitorExit,
    /// Array load/store with element type (byte/char/short → int).
    ArrLoad(ArrTy),
    ArrStore(ArrTy),
    AConstNull,
    /// ifnull (Eq) / ifnonnull (Ne)
    IfRefNull(Cond, usize),
    /// if_acmpeq / if_acmpne
    IfACmp(Cond, usize),
}

/// Decodes a method's code array into `(pc, Instr)` pairs.
/// `ldc` resolution needs the constant pool, hence the callback:
/// it returns `Some(int)` for a CP index, or `None` for a string.
pub fn decode_code(
    code: &[u8],
    resolve_ldc: impl Fn(u16) -> Option<i32>,
) -> Result<Vec<(usize, Instr)>, ParseError> {
    let mut out = Vec::new();
    let mut pc = 0usize;
    while pc < code.len() {
        let op = code[pc];
        let start = pc;
        let u8_at = |i: usize| -> Result<u8, ParseError> {
            code.get(i).copied().ok_or(ParseError::Eof)
        };
        let u16_at = |i: usize| -> Result<u16, ParseError> {
            Ok(u16::from_be_bytes([u8_at(i)?, u8_at(i + 1)?]))
        };
        let branch = |i: usize| -> Result<usize, ParseError> {
            let off = u16_at(i)? as i16 as isize;
            Ok((start as isize + off) as usize)
        };

        let (instr, len) = match op {
            0x00 => (Instr::Nop, 1),
            0x01 => (Instr::AConstNull, 1),
            0x09 => (Instr::LConst(0), 1),
            0x0A => (Instr::LConst(1), 1),
            0x0B => (Instr::FConst(0.0), 1),
            0x0C => (Instr::FConst(1.0), 1),
            0x0D => (Instr::FConst(2.0), 1),
            0x0E => (Instr::DConst(0.0), 1),
            0x0F => (Instr::DConst(1.0), 1),
            0x14 => (Instr::Ldc2W(u16_at(pc + 1)?), 3),
            0x02..=0x08 => (Instr::IConst(op as i32 - 0x03), 1),
            0x10 => (Instr::IConst(u8_at(pc + 1)? as i8 as i32), 2),
            0x11 => (Instr::IConst(u16_at(pc + 1)? as i16 as i32), 3),
            0x12 => {
                let idx = u8_at(pc + 1)? as u16;
                let i = match resolve_ldc(idx) {
                    Some(v) => Instr::LdcInt(v),
                    None => Instr::LdcString(idx),
                };
                (i, 2)
            }
            0x13 => {
                let idx = u16_at(pc + 1)?;
                let i = match resolve_ldc(idx) {
                    Some(v) => Instr::LdcInt(v),
                    None => Instr::LdcString(idx),
                };
                (i, 3)
            }
            0x15 => (Instr::ILoad(u8_at(pc + 1)? as u16), 2),
            0x1A..=0x1D => (Instr::ILoad(op as u16 - 0x1A), 1),
            0x16 => (Instr::LLoad(u8_at(pc + 1)? as u16), 2),
            0x1E..=0x21 => (Instr::LLoad(op as u16 - 0x1E), 1),
            0x17 => (Instr::FLoad(u8_at(pc + 1)? as u16), 2),
            0x22..=0x25 => (Instr::FLoad(op as u16 - 0x22), 1),
            0x18 => (Instr::DLoad(u8_at(pc + 1)? as u16), 2),
            0x26..=0x29 => (Instr::DLoad(op as u16 - 0x26), 1),
            0x19 => (Instr::ALoad(u8_at(pc + 1)? as u16), 2),
            0x2A..=0x2D => (Instr::ALoad(op as u16 - 0x2A), 1),
            0x36 => (Instr::IStore(u8_at(pc + 1)? as u16), 2),
            0x3B..=0x3E => (Instr::IStore(op as u16 - 0x3B), 1),
            0x37 => (Instr::LStore(u8_at(pc + 1)? as u16), 2),
            0x3F..=0x42 => (Instr::LStore(op as u16 - 0x3F), 1),
            0x38 => (Instr::FStore(u8_at(pc + 1)? as u16), 2),
            0x43..=0x46 => (Instr::FStore(op as u16 - 0x43), 1),
            0x39 => (Instr::DStore(u8_at(pc + 1)? as u16), 2),
            0x47..=0x4A => (Instr::DStore(op as u16 - 0x47), 1),
            0x3A => (Instr::AStore(u8_at(pc + 1)? as u16), 2),
            0x4B..=0x4E => (Instr::AStore(op as u16 - 0x4B), 1),
            // Array load (baload covers byte AND boolean).
            0x2E => (Instr::ArrLoad(ArrTy::Int), 1),
            0x2F => (Instr::ArrLoad(ArrTy::Long), 1),
            0x30 => (Instr::ArrLoad(ArrTy::Float), 1),
            0x31 => (Instr::ArrLoad(ArrTy::Double), 1),
            0x32 => (Instr::ArrLoad(ArrTy::Ref), 1),
            0x33 => (Instr::ArrLoad(ArrTy::Byte), 1),
            0x34 => (Instr::ArrLoad(ArrTy::Char), 1),
            0x35 => (Instr::ArrLoad(ArrTy::Short), 1),
            // Array store.
            0x4F => (Instr::ArrStore(ArrTy::Int), 1),
            0x50 => (Instr::ArrStore(ArrTy::Long), 1),
            0x51 => (Instr::ArrStore(ArrTy::Float), 1),
            0x52 => (Instr::ArrStore(ArrTy::Double), 1),
            0x53 => (Instr::ArrStore(ArrTy::Ref), 1),
            0x54 => (Instr::ArrStore(ArrTy::Byte), 1),
            0x55 => (Instr::ArrStore(ArrTy::Char), 1),
            0x56 => (Instr::ArrStore(ArrTy::Short), 1),
            0x57 => (Instr::Pop, 1),
            0x58 => (Instr::Pop2, 1),
            0x59 => (Instr::Dup, 1),
            0x5C => (Instr::Dup2, 1),
            0x60 => (Instr::IAdd, 1),
            0x64 => (Instr::ISub, 1),
            0x68 => (Instr::IMul, 1),
            0x6C => (Instr::IDiv, 1),
            0x70 => (Instr::IRem, 1),
            0x74 => (Instr::INeg, 1),
            0x78 => (Instr::IShl, 1),
            0x7A => (Instr::IShr, 1),
            0x7C => (Instr::IUShr, 1),
            0x7E => (Instr::IAnd, 1),
            0x80 => (Instr::IOr, 1),
            0x82 => (Instr::IXor, 1),
            0x61 => (Instr::LAdd, 1),
            0x65 => (Instr::LSub, 1),
            0x69 => (Instr::LMul, 1),
            0x6D => (Instr::LDiv, 1),
            0x71 => (Instr::LRem, 1),
            0x75 => (Instr::LNeg, 1),
            0x79 => (Instr::LShl, 1),
            0x7B => (Instr::LShr, 1),
            0x7D => (Instr::LUShr, 1),
            0x7F => (Instr::LAnd, 1),
            0x81 => (Instr::LOr, 1),
            0x83 => (Instr::LXor, 1),
            0x63 => (Instr::DAdd, 1),
            0x67 => (Instr::DSub, 1),
            0x6B => (Instr::DMul, 1),
            0x6F => (Instr::DDiv, 1),
            0x73 => (Instr::DRem, 1),
            0x77 => (Instr::DNeg, 1),
            0x62 => (Instr::FAdd, 1),
            0x66 => (Instr::FSub, 1),
            0x6A => (Instr::FMul, 1),
            0x6E => (Instr::FDiv, 1),
            0x72 => (Instr::FRem, 1),
            0x76 => (Instr::FNeg, 1),
            0x85 => (Instr::I2L, 1),
            0x86 => (Instr::I2F, 1),
            0x87 => (Instr::I2D, 1),
            0x88 => (Instr::L2I, 1),
            0x89 => (Instr::L2F, 1),
            0x8A => (Instr::L2D, 1),
            0x8B => (Instr::F2I, 1),
            0x8C => (Instr::F2L, 1),
            0x8D => (Instr::F2D, 1),
            0x8E => (Instr::D2I, 1),
            0x8F => (Instr::D2L, 1),
            0x90 => (Instr::D2F, 1),
            0x94 => (Instr::LCmp, 1),
            0x95 => (Instr::FCmpL, 1),
            0x96 => (Instr::FCmpG, 1),
            0x97 => (Instr::DCmpL, 1),
            0x98 => (Instr::DCmpG, 1),
            0x84 => (Instr::IInc(u8_at(pc + 1)? as u16, u8_at(pc + 2)? as i8 as i32), 3),
            // `wide`: widens the index (and, for iinc, the constant) to 16 bits.
            // Format: 0xc4 <op> <index:u16> [<const:i16> only for iinc].
            0xc4 => {
                let sub = u8_at(pc + 1)?;
                let idx = u16_at(pc + 2)?;
                match sub {
                    0x15 => (Instr::ILoad(idx), 4),
                    0x16 => (Instr::LLoad(idx), 4),
                    0x17 => (Instr::FLoad(idx), 4),
                    0x18 => (Instr::DLoad(idx), 4),
                    0x19 => (Instr::ALoad(idx), 4),
                    0x36 => (Instr::IStore(idx), 4),
                    0x37 => (Instr::LStore(idx), 4),
                    0x38 => (Instr::FStore(idx), 4),
                    0x39 => (Instr::DStore(idx), 4),
                    0x3A => (Instr::AStore(idx), 4),
                    0x84 => (Instr::IInc(idx, u16_at(pc + 4)? as i16 as i32), 6),
                    _ => return Err(ParseError::UnsupportedOpcode(sub, pc)),
                }
            }
            0x99 => (Instr::IfZero(Cond::Eq, branch(pc + 1)?), 3),
            0x9A => (Instr::IfZero(Cond::Ne, branch(pc + 1)?), 3),
            0x9B => (Instr::IfZero(Cond::Lt, branch(pc + 1)?), 3),
            0x9C => (Instr::IfZero(Cond::Ge, branch(pc + 1)?), 3),
            0x9D => (Instr::IfZero(Cond::Gt, branch(pc + 1)?), 3),
            0x9E => (Instr::IfZero(Cond::Le, branch(pc + 1)?), 3),
            0x9F => (Instr::IfICmp(Cond::Eq, branch(pc + 1)?), 3),
            0xA0 => (Instr::IfICmp(Cond::Ne, branch(pc + 1)?), 3),
            0xA1 => (Instr::IfICmp(Cond::Lt, branch(pc + 1)?), 3),
            0xA2 => (Instr::IfICmp(Cond::Ge, branch(pc + 1)?), 3),
            0xA3 => (Instr::IfICmp(Cond::Gt, branch(pc + 1)?), 3),
            0xA4 => (Instr::IfICmp(Cond::Le, branch(pc + 1)?), 3),
            0xA5 => (Instr::IfACmp(Cond::Eq, branch(pc + 1)?), 3),
            0xA6 => (Instr::IfACmp(Cond::Ne, branch(pc + 1)?), 3),
            0xA7 => (Instr::Goto(branch(pc + 1)?), 3),
            // tableswitch: padding to a 4-byte boundary, then default/low/high
            // and (high-low+1) offsets (all relative to the opcode start).
            0xAA => {
                let base = pc + 1 + ((4 - ((pc + 1) % 4)) % 4);
                let u32_at = |i: usize| -> Result<u32, ParseError> {
                    Ok(u32::from_be_bytes([u8_at(i)?, u8_at(i + 1)?, u8_at(i + 2)?, u8_at(i + 3)?]))
                };
                let default = (start as i64 + u32_at(base)? as i32 as i64) as usize;
                let low = u32_at(base + 4)? as i32;
                let high = u32_at(base + 8)? as i32;
                let count = (high - low + 1) as usize;
                let mut cases = Vec::with_capacity(count);
                for k in 0..count {
                    let off = u32_at(base + 12 + k * 4)? as i32 as i64;
                    cases.push((low + k as i32, (start as i64 + off) as usize));
                }
                (Instr::Switch(default, cases), base + 12 + count * 4 - pc)
            }
            // lookupswitch: padding, then default/npairs and npairs (key, offset).
            0xAB => {
                let base = pc + 1 + ((4 - ((pc + 1) % 4)) % 4);
                let u32_at = |i: usize| -> Result<u32, ParseError> {
                    Ok(u32::from_be_bytes([u8_at(i)?, u8_at(i + 1)?, u8_at(i + 2)?, u8_at(i + 3)?]))
                };
                let default = (start as i64 + u32_at(base)? as i32 as i64) as usize;
                let npairs = u32_at(base + 4)? as usize;
                let mut cases = Vec::with_capacity(npairs);
                for k in 0..npairs {
                    let key = u32_at(base + 8 + k * 8)? as i32;
                    let off = u32_at(base + 12 + k * 8)? as i32 as i64;
                    cases.push((key, (start as i64 + off) as usize));
                }
                (Instr::Switch(default, cases), base + 8 + npairs * 8 - pc)
            }
            0xAC => (Instr::IReturn, 1),
            0xAD => (Instr::LReturn, 1),
            0xAE => (Instr::FReturn, 1),
            0xAF => (Instr::DReturn, 1),
            0xB0 => (Instr::AReturn, 1),
            0xB1 => (Instr::Return, 1),
            0xB2 => (Instr::GetStatic(u16_at(pc + 1)?), 3),
            0xB3 => (Instr::PutStatic(u16_at(pc + 1)?), 3),
            0xB4 => (Instr::GetField(u16_at(pc + 1)?), 3),
            0xB5 => (Instr::PutField(u16_at(pc + 1)?), 3),
            0xB6 => (Instr::InvokeVirtual(u16_at(pc + 1)?), 3),
            0xB7 => (Instr::InvokeSpecial(u16_at(pc + 1)?), 3),
            0xB8 => (Instr::InvokeStatic(u16_at(pc + 1)?), 3),
            // invokeinterface: index (u2), count (u1), 0 (u1) — 5 bytes.
            0xB9 => (Instr::InvokeInterface(u16_at(pc + 1)?), 5),
            0xBA => (Instr::InvokeDynamic(u16_at(pc + 1)?), 5),
            0xBB => (Instr::New(u16_at(pc + 1)?), 3),
            0xBC => {
                // newarray: atype → element type. bool/byte/char/short/int → int
                // (4-byte, value-correct), long/float/double typed.
                let elem = match u8_at(pc + 1)? {
                    4 => ArrTy::Bool,
                    5 => ArrTy::Char,
                    6 => ArrTy::Float,
                    7 => ArrTy::Double,
                    8 => ArrTy::Byte,
                    9 => ArrTy::Short,
                    10 => ArrTy::Int,
                    11 => ArrTy::Long,
                    _ => return Err(ParseError::UnsupportedOpcode(op, pc)),
                };
                (Instr::NewArrayPrim(elem), 2)
            }
            0xBD => (Instr::NewArrayRef(u16_at(pc + 1)?), 3),
            0xBE => (Instr::ArrayLength, 1),
            0xBF => (Instr::AThrow, 1),
            0xC0 => (Instr::CheckCast(u16_at(pc + 1)?), 3),
            0xC1 => (Instr::InstanceOf(u16_at(pc + 1)?), 3),
            0xC2 => (Instr::MonitorEnter, 1),
            0xC3 => (Instr::MonitorExit, 1),
            0xC6 => (Instr::IfRefNull(Cond::Eq, branch(pc + 1)?), 3),
            0xC7 => (Instr::IfRefNull(Cond::Ne, branch(pc + 1)?), 3),
            _ => return Err(ParseError::UnsupportedOpcode(op, pc)),
        };
        out.push((start, instr));
        pc += len;
    }
    Ok(out)
}
