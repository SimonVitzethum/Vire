//! Decoder für die unterstützte Bytecode-Teilmenge (JVMS Kap. 6):
//! int-Arithmetik, Kontrollfluss, statische Aufrufe, getstatic/invokevirtual
//! (für das println-Intrinsic), String-/int-Konstanten.

use crate::ParseError;

/// Vergleichsbedingung für Branch-Instruktionen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    Eq,
    Ne,
    Lt,
    Ge,
    Gt,
    Le,
}

/// Eine dekodierte Instruktion. `pc`-Angaben in Branch-Zielen sind absolute
/// Bytecode-Offsets (bereits aus den relativen Offsets berechnet).
#[derive(Debug, Clone)]
pub enum Instr {
    Nop,
    IConst(i32),
    LConst(i64),
    DConst(f64),
    /// ldc2_w: long/double aus dem Constant Pool.
    Ldc2W(u16),
    LdcString(u16),
    LdcInt(i32),
    ILoad(u16),
    IStore(u16),
    LLoad(u16),
    LStore(u16),
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
    I2L,
    I2D,
    L2I,
    L2D,
    D2I,
    D2L,
    Pop,
    Pop2,
    Dup,
    Dup2,
    /// if_icmp<cond>: vergleicht zwei Stack-Werte.
    IfICmp(Cond, usize),
    /// if<cond>: vergleicht einen Stack-Wert mit 0.
    IfZero(Cond, usize),
    Goto(usize),
    IReturn,
    LReturn,
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
    /// newarray mit primitivem Elementtyp (atype-Code, hier nur int=10).
    NewArrayInt,
    /// anewarray: Array von Referenzen (Klassenindex, hier ignoriert).
    NewArrayRef(u16),
    ArrayLength,
    AThrow,
    IaLoad,
    IaStore,
    AaLoad,
    AaStore,
    AConstNull,
    /// ifnull (Eq) / ifnonnull (Ne)
    IfRefNull(Cond, usize),
    /// if_acmpeq / if_acmpne
    IfACmp(Cond, usize),
}

/// Dekodiert das Code-Array einer Methode zu `(pc, Instr)`-Paaren.
/// `ldc`-Auflösung braucht den Constant Pool, deshalb der Callback:
/// er liefert für einen CP-Index `Some(int)` bzw. `None` für String.
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
            0x18 => (Instr::DLoad(u8_at(pc + 1)? as u16), 2),
            0x26..=0x29 => (Instr::DLoad(op as u16 - 0x26), 1),
            0x19 => (Instr::ALoad(u8_at(pc + 1)? as u16), 2),
            0x2A..=0x2D => (Instr::ALoad(op as u16 - 0x2A), 1),
            0x36 => (Instr::IStore(u8_at(pc + 1)? as u16), 2),
            0x3B..=0x3E => (Instr::IStore(op as u16 - 0x3B), 1),
            0x37 => (Instr::LStore(u8_at(pc + 1)? as u16), 2),
            0x3F..=0x42 => (Instr::LStore(op as u16 - 0x3F), 1),
            0x39 => (Instr::DStore(u8_at(pc + 1)? as u16), 2),
            0x47..=0x4A => (Instr::DStore(op as u16 - 0x47), 1),
            0x3A => (Instr::AStore(u8_at(pc + 1)? as u16), 2),
            0x4B..=0x4E => (Instr::AStore(op as u16 - 0x4B), 1),
            0x2E => (Instr::IaLoad, 1),
            0x32 => (Instr::AaLoad, 1),
            0x4F => (Instr::IaStore, 1),
            0x53 => (Instr::AaStore, 1),
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
            0x85 => (Instr::I2L, 1),
            0x87 => (Instr::I2D, 1),
            0x88 => (Instr::L2I, 1),
            0x8A => (Instr::L2D, 1),
            0x8E => (Instr::D2I, 1),
            0x8F => (Instr::D2L, 1),
            0x94 => (Instr::LCmp, 1),
            0x97 => (Instr::DCmpL, 1),
            0x98 => (Instr::DCmpG, 1),
            0x84 => (Instr::IInc(u8_at(pc + 1)? as u16, u8_at(pc + 2)? as i8 as i32), 3),
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
            0xAC => (Instr::IReturn, 1),
            0xAD => (Instr::LReturn, 1),
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
            // invokeinterface: index (u2), count (u1), 0 (u1) — 5 Bytes.
            0xB9 => (Instr::InvokeInterface(u16_at(pc + 1)?), 5),
            0xBA => (Instr::InvokeDynamic(u16_at(pc + 1)?), 5),
            0xBB => (Instr::New(u16_at(pc + 1)?), 3),
            0xBC => {
                // newarray: atype 10 = T_INT (aktuell einzig unterstützt).
                let atype = u8_at(pc + 1)?;
                if atype != 10 {
                    return Err(ParseError::UnsupportedOpcode(op, pc));
                }
                (Instr::NewArrayInt, 2)
            }
            0xBD => (Instr::NewArrayRef(u16_at(pc + 1)?), 3),
            0xBE => (Instr::ArrayLength, 1),
            0xBF => (Instr::AThrow, 1),
            0xC0 => (Instr::CheckCast(u16_at(pc + 1)?), 3),
            0xC1 => (Instr::InstanceOf(u16_at(pc + 1)?), 3),
            0xC6 => (Instr::IfRefNull(Cond::Eq, branch(pc + 1)?), 3),
            0xC7 => (Instr::IfRefNull(Cond::Ne, branch(pc + 1)?), 3),
            _ => return Err(ParseError::UnsupportedOpcode(op, pc)),
        };
        out.push((start, instr));
        pc += len;
    }
    Ok(out)
}
