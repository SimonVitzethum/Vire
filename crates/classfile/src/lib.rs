//! Parser for Java class files (JVMS ch. 4).
//!
//! Deliberately minimal: constant pool in full (needed for all references),
//! methods with a Code attribute, bytecode decoder for the supported
//! subset. StackMapTable, annotations, etc. are skipped over.

mod opcode;
pub use opcode::{decode_code, ArrTy, Cond, Instr};

use std::fmt;

#[derive(Debug)]
pub enum ParseError {
    Eof,
    BadMagic(u32),
    UnsupportedConstTag(u8),
    UnsupportedOpcode(u8, usize),
    BadUtf8,
    BadIndex(u16),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Eof => write!(f, "unerwartetes Dateiende"),
            ParseError::BadMagic(m) => write!(f, "kein Classfile (magic {m:#x})"),
            ParseError::UnsupportedConstTag(t) => write!(f, "constant-pool tag {t} not supported"),
            ParseError::UnsupportedOpcode(op, pc) => write!(f, "opcode {op:#04x} at pc={pc} not supported"),
            ParseError::BadUtf8 => write!(f, "invalid Modified-UTF-8"),
            ParseError::BadIndex(i) => write!(f, "invalid constant-pool index {i}"),
        }
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

/// An entry in the constant pool. Long/Double occupy two slots (JVMS 4.4.5);
/// the second slot is stored as `Unusable` so that indices stay correct.
#[derive(Debug, Clone)]
pub enum Const {
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class { name: u16 },
    String { utf8: u16 },
    FieldRef { class: u16, name_and_type: u16 },
    MethodRef { class: u16, name_and_type: u16 },
    InterfaceMethodRef { class: u16, name_and_type: u16 },
    NameAndType { name: u16, descriptor: u16 },
    MethodHandle { reference_kind: u8, reference_index: u16 },
    MethodType { descriptor: u16 },
    InvokeDynamic { bootstrap_method_attr_index: u16, name_and_type: u16 },
    Unusable,
}

/// Entry in the BootstrapMethods attribute (JVMS 4.7.23).
#[derive(Debug, Clone)]
pub struct BootstrapMethod {
    /// CP index of a MethodHandle constant.
    pub method_ref: u16,
    /// CP indices of the static bootstrap arguments.
    pub args: Vec<u16>,
}

#[derive(Debug)]
pub struct Method {
    pub access_flags: u16,
    pub name: String,
    pub descriptor: String,
    /// None for abstract/native.
    pub code: Option<Code>,
}

impl Method {
    pub fn is_static(&self) -> bool {
        self.access_flags & 0x0008 != 0
    }
}

/// Entry in the exception table (JVMS 4.7.3): the range [start_pc, end_pc)
/// is caught by handler_pc if the thrown type matches catch_type
/// (catch_type 0 = finally / catches everything).
#[derive(Debug, Clone)]
pub struct ExceptionEntry {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    pub catch_type: u16,
}

#[derive(Debug)]
pub struct Code {
    pub max_stack: u16,
    pub max_locals: u16,
    pub bytecode: Vec<u8>,
    pub exceptions: Vec<ExceptionEntry>,
}

#[derive(Debug)]
pub struct Field {
    pub access_flags: u16,
    pub name: String,
    pub descriptor: String,
    /// CP index of the ConstantValue attribute (static finals with a
    /// compile-time constant), if present.
    pub constant_value: Option<u16>,
}

impl Field {
    pub fn is_static(&self) -> bool {
        self.access_flags & 0x0008 != 0
    }
}

#[derive(Debug)]
pub struct ClassFile {
    pub constant_pool: Vec<Const>,
    pub access_flags: u16,
    pub this_class: String,
    pub super_class: Option<String>,
    pub interfaces: Vec<String>,
    pub fields: Vec<Field>,
    pub methods: Vec<Method>,
    pub bootstrap_methods: Vec<BootstrapMethod>,
}

impl ClassFile {
    pub fn parse(data: &[u8]) -> Result<ClassFile> {
        let mut r = Reader { data, pos: 0 };
        let magic = r.u32()?;
        if magic != 0xCAFEBABE {
            return Err(ParseError::BadMagic(magic));
        }
        let _minor = r.u16()?;
        let _major = r.u16()?;

        let cp_count = r.u16()?;
        // Index 0 is unused per the specification.
        let mut constant_pool = vec![Const::Unusable];
        let mut i = 1;
        while i < cp_count {
            let tag = r.u8()?;
            let entry = match tag {
                1 => {
                    let len = r.u16()? as usize;
                    let bytes = r.bytes(len)?;
                    Const::Utf8(decode_modified_utf8(bytes)?)
                }
                3 => Const::Integer(r.u32()? as i32),
                4 => Const::Float(f32::from_bits(r.u32()?)),
                5 => Const::Long(r.u64()? as i64),
                6 => Const::Double(f64::from_bits(r.u64()?)),
                7 => Const::Class { name: r.u16()? },
                8 => Const::String { utf8: r.u16()? },
                9 => Const::FieldRef { class: r.u16()?, name_and_type: r.u16()? },
                10 => Const::MethodRef { class: r.u16()?, name_and_type: r.u16()? },
                11 => Const::InterfaceMethodRef { class: r.u16()?, name_and_type: r.u16()? },
                12 => Const::NameAndType { name: r.u16()?, descriptor: r.u16()? },
                15 => Const::MethodHandle { reference_kind: r.u8()?, reference_index: r.u16()? },
                16 => Const::MethodType { descriptor: r.u16()? },
                18 => Const::InvokeDynamic { bootstrap_method_attr_index: r.u16()?, name_and_type: r.u16()? },
                // Dynamic (condy)/Module/Package: skipped over.
                17 => { r.bytes(4)?; Const::Unusable }
                19 | 20 => { r.bytes(2)?; Const::Unusable }
                t => return Err(ParseError::UnsupportedConstTag(t)),
            };
            let is_wide = matches!(entry, Const::Long(_) | Const::Double(_));
            constant_pool.push(entry);
            if is_wide {
                constant_pool.push(Const::Unusable);
                i += 2;
            } else {
                i += 1;
            }
        }

        let access_flags = r.u16()?;
        let this_idx = r.u16()?;
        let super_idx = r.u16()?;

        let interfaces_count = r.u16()?;
        let mut interface_idxs = Vec::with_capacity(interfaces_count as usize);
        for _ in 0..interfaces_count {
            interface_idxs.push(r.u16()?);
        }

        let fields_count = r.u16()?;
        let mut fields = Vec::with_capacity(fields_count as usize);
        for _ in 0..fields_count {
            let access_flags = r.u16()?;
            let name_idx = r.u16()?;
            let desc_idx = r.u16()?;
            let mut constant_value = None;
            let attr_count = r.u16()?;
            for _ in 0..attr_count {
                let attr_name_idx = r.u16()?;
                let attr_len = r.u32()? as usize;
                if utf8_at(&constant_pool, attr_name_idx)? == "ConstantValue" {
                    let mut cr = Reader { data: r.bytes(attr_len)?, pos: 0 };
                    constant_value = Some(cr.u16()?);
                } else {
                    r.bytes(attr_len)?;
                }
            }
            fields.push(Field {
                access_flags,
                name: utf8_at(&constant_pool, name_idx)?.to_string(),
                descriptor: utf8_at(&constant_pool, desc_idx)?.to_string(),
                constant_value,
            });
        }

        let methods_count = r.u16()?;
        let mut methods = Vec::with_capacity(methods_count as usize);
        for _ in 0..methods_count {
            let access_flags = r.u16()?;
            let name_idx = r.u16()?;
            let desc_idx = r.u16()?;
            let name = utf8_at(&constant_pool, name_idx)?.to_string();
            let descriptor = utf8_at(&constant_pool, desc_idx)?.to_string();
            let mut code = None;
            let attr_count = r.u16()?;
            for _ in 0..attr_count {
                let attr_name_idx = r.u16()?;
                let attr_len = r.u32()? as usize;
                if utf8_at(&constant_pool, attr_name_idx)? == "Code" {
                    let mut cr = Reader { data: r.bytes(attr_len)?, pos: 0 };
                    let max_stack = cr.u16()?;
                    let max_locals = cr.u16()?;
                    let code_len = cr.u32()? as usize;
                    let bytecode = cr.bytes(code_len)?.to_vec();
                    let ex_count = cr.u16()?;
                    let mut exceptions = Vec::with_capacity(ex_count as usize);
                    for _ in 0..ex_count {
                        exceptions.push(ExceptionEntry {
                            start_pc: cr.u16()?,
                            end_pc: cr.u16()?,
                            handler_pc: cr.u16()?,
                            catch_type: cr.u16()?,
                        });
                    }
                    // Ignore sub-attributes (LineNumberTable, StackMapTable, …).
                    code = Some(Code { max_stack, max_locals, bytecode, exceptions });
                } else {
                    r.bytes(attr_len)?;
                }
            }
            methods.push(Method { access_flags, name, descriptor, code });
        }

        // Class attributes: collect BootstrapMethods (for invokedynamic),
        // skip over the rest.
        let mut bootstrap_methods = Vec::new();
        let attr_count = r.u16()?;
        for _ in 0..attr_count {
            let attr_name_idx = r.u16()?;
            let attr_len = r.u32()? as usize;
            if utf8_at(&constant_pool, attr_name_idx)? == "BootstrapMethods" {
                let mut br = Reader { data: r.bytes(attr_len)?, pos: 0 };
                let num = br.u16()?;
                for _ in 0..num {
                    let method_ref = br.u16()?;
                    let num_args = br.u16()?;
                    let mut args = Vec::with_capacity(num_args as usize);
                    for _ in 0..num_args {
                        args.push(br.u16()?);
                    }
                    bootstrap_methods.push(BootstrapMethod { method_ref, args });
                }
            } else {
                r.bytes(attr_len)?;
            }
        }

        let this_class = class_name_at(&constant_pool, this_idx)?.to_string();
        let super_class = if super_idx == 0 {
            None
        } else {
            Some(class_name_at(&constant_pool, super_idx)?.to_string())
        };
        let interfaces = interface_idxs
            .into_iter()
            .map(|i| class_name_at(&constant_pool, i).map(str::to_string))
            .collect::<Result<Vec<_>>>()?;

        Ok(ClassFile {
            constant_pool,
            access_flags,
            this_class,
            super_class,
            interfaces,
            fields,
            methods,
            bootstrap_methods,
        })
    }

    pub fn utf8(&self, idx: u16) -> Result<&str> {
        utf8_at(&self.constant_pool, idx)
    }

    /// Resolves an invokedynamic call site. Returns (name, descriptor,
    /// bootstrap method name, bootstrap argument CP indices). The
    /// bootstrap method name lets the frontend recognize `makeConcatWithConstants`
    /// and resolve it statically (DESIGN.md §1.3).
    pub fn invoke_dynamic(&self, idx: u16) -> Result<(&str, &str, &str, &[u16])> {
        let (bsm_idx, nat) = match self.constant_pool.get(idx as usize) {
            Some(Const::InvokeDynamic { bootstrap_method_attr_index, name_and_type }) => {
                (*bootstrap_method_attr_index, *name_and_type)
            }
            _ => return Err(ParseError::BadIndex(idx)),
        };
        let (name, desc) = match self.constant_pool.get(nat as usize) {
            Some(Const::NameAndType { name, descriptor }) => (*name, *descriptor),
            _ => return Err(ParseError::BadIndex(nat)),
        };
        let bsm = self
            .bootstrap_methods
            .get(bsm_idx as usize)
            .ok_or(ParseError::BadIndex(bsm_idx))?;
        // Bootstrap MethodHandle → referenced method → its name.
        let bsm_name = match self.constant_pool.get(bsm.method_ref as usize) {
            Some(Const::MethodHandle { reference_index, .. }) => {
                self.member_ref(*reference_index)?.1
            }
            _ => return Err(ParseError::BadIndex(bsm.method_ref)),
        };
        Ok((
            utf8_at(&self.constant_pool, name)?,
            utf8_at(&self.constant_pool, desc)?,
            bsm_name,
            &bsm.args,
        ))
    }

    /// Resolves a MethodHandle constant to (reference_kind, class, name,
    /// descriptor) of the referenced method.
    pub fn method_handle(&self, idx: u16) -> Result<(u8, &str, &str, &str)> {
        let (kind, ref_idx) = match self.constant_pool.get(idx as usize) {
            Some(Const::MethodHandle { reference_kind, reference_index }) => {
                (*reference_kind, *reference_index)
            }
            _ => return Err(ParseError::BadIndex(idx)),
        };
        let (class, name, desc) = self.member_ref(ref_idx)?;
        Ok((kind, class, name, desc))
    }

    /// Descriptor of a MethodType constant.
    pub fn method_type(&self, idx: u16) -> Result<&str> {
        match self.constant_pool.get(idx as usize) {
            Some(Const::MethodType { descriptor }) => utf8_at(&self.constant_pool, *descriptor),
            _ => Err(ParseError::BadIndex(idx)),
        }
    }

    /// Returns a string from a String or Utf8 constant
    /// (bootstrap arguments are String constants).
    pub fn const_string(&self, idx: u16) -> Result<&str> {
        match self.constant_pool.get(idx as usize) {
            Some(Const::String { utf8 }) => utf8_at(&self.constant_pool, *utf8),
            Some(Const::Utf8(s)) => Ok(s),
            _ => Err(ParseError::BadIndex(idx)),
        }
    }

    pub fn class_name(&self, idx: u16) -> Result<&str> {
        class_name_at(&self.constant_pool, idx)
    }

    /// Resolves a method/field ref to (class, name, descriptor).
    pub fn member_ref(&self, idx: u16) -> Result<(&str, &str, &str)> {
        let (class, nat) = match self.constant_pool.get(idx as usize) {
            Some(Const::MethodRef { class, name_and_type })
            | Some(Const::InterfaceMethodRef { class, name_and_type })
            | Some(Const::FieldRef { class, name_and_type }) => (*class, *name_and_type),
            _ => return Err(ParseError::BadIndex(idx)),
        };
        let (name, desc) = match self.constant_pool.get(nat as usize) {
            Some(Const::NameAndType { name, descriptor }) => (*name, *descriptor),
            _ => return Err(ParseError::BadIndex(nat)),
        };
        Ok((
            class_name_at(&self.constant_pool, class)?,
            utf8_at(&self.constant_pool, name)?,
            utf8_at(&self.constant_pool, desc)?,
        ))
    }
}

fn utf8_at(cp: &[Const], idx: u16) -> Result<&str> {
    match cp.get(idx as usize) {
        Some(Const::Utf8(s)) => Ok(s),
        _ => Err(ParseError::BadIndex(idx)),
    }
}

fn class_name_at(cp: &[Const], idx: u16) -> Result<&str> {
    match cp.get(idx as usize) {
        Some(Const::Class { name }) => utf8_at(cp, *name),
        _ => Err(ParseError::BadIndex(idx)),
    }
}

fn skip_attributes(r: &mut Reader) -> Result<()> {
    let count = r.u16()?;
    for _ in 0..count {
        r.u16()?;
        let len = r.u32()? as usize;
        r.bytes(len)?;
    }
    Ok(())
}

/// Modified UTF-8 (JVMS 4.4.7): no 4-byte UTF-8, supplementary characters
/// as CESU-8 surrogate pairs, U+0000 as 0xC0 0x80. For the current subset,
/// decoding 1–3-byte sequences suffices.
fn decode_modified_utf8(bytes: &[u8]) -> Result<String> {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let c = if b & 0x80 == 0 {
            i += 1;
            b as u32
        } else if b & 0xE0 == 0xC0 {
            let b2 = *bytes.get(i + 1).ok_or(ParseError::BadUtf8)?;
            i += 2;
            ((b as u32 & 0x1F) << 6) | (b2 as u32 & 0x3F)
        } else if b & 0xF0 == 0xE0 {
            let b2 = *bytes.get(i + 1).ok_or(ParseError::BadUtf8)?;
            let b3 = *bytes.get(i + 2).ok_or(ParseError::BadUtf8)?;
            i += 3;
            ((b as u32 & 0x0F) << 12) | ((b2 as u32 & 0x3F) << 6) | (b3 as u32 & 0x3F)
        } else {
            return Err(ParseError::BadUtf8);
        };
        out.push(char::from_u32(c).ok_or(ParseError::BadUtf8)?);
    }
    Ok(out)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(ParseError::Eof);
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let b = self.bytes(8)?;
        Ok(u64::from_be_bytes(b.try_into().unwrap()))
    }
}
