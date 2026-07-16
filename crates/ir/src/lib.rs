//! Mittel-IR nach rustc-MIR-Vorbild (siehe DESIGN.md §2): Funktionen aus
//! Basic Blocks, Locals statt Operandenstack, expliziter Terminator pro
//! Block. Auf dieser IR laufen später Devirtualisierung, Escape-Analyse,
//! Inlining und guarded speculation — vor der LLVM-Absenkung.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    I32,
    I64,
    F64,
    /// Referenztyp; vorerst opak (Zeiger). Für String-Literale genutzt.
    Ref,
    Void,
}

/// Index eines Locals in `Function::locals`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Local(pub u32);

/// Index eines Basic Blocks in `Function::blocks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Block(pub u32);

#[derive(Debug, Clone)]
pub enum Operand {
    Copy(Local),
    ConstI32(i32),
    ConstI64(i64),
    ConstF64(f64),
    /// Verweis auf ein String-Literal in `Program::strings`.
    ConstStr(u32),
    /// Class-Objekt einer zur Compile-Zeit aufgelösten Klasse (Reflection,
    /// DESIGN.md §1.3). Singleton pro Klasse → `==` ist Pointer-Gleichheit,
    /// wie in Java.
    ConstClass(String),
    ConstNull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    /// Java-Semantik: wirft bei Divisor 0; Absenkung fügt den Check ein.
    Div,
    Rem,
    Shl,
    Shr,
    UShr,
    And,
    Or,
    Xor,
    CmpEq,
    CmpNe,
    CmpLt,
    CmpGe,
    CmpGt,
    CmpLe,
}

#[derive(Debug, Clone)]
pub enum Rvalue {
    Use(Operand),
    Binary(BinOp, Operand, Operand),
    Neg(Operand),
    /// Numerische Konvertierung; Quelltyp = Typ des Operanden, Zieltyp =
    /// Typ des Ziel-Locals. Nur verlustfreie/definierte Fälle inline
    /// (i2l/i2d/l2d/l2i); saturating d2i/d2l laufen über Runtime-Calls.
    Convert(Operand),
}

#[derive(Debug, Clone)]
pub enum Statement {
    Assign(Local, Rvalue),
    /// Direkter Aufruf (statisch, devirtualisiert oder Runtime-Intrinsic).
    Call {
        dest: Option<Local>,
        func: String,
        args: Vec<Operand>,
    },
    /// Virtueller Aufruf über die Vtable; `args[0]` ist der Receiver.
    /// `class` ist der statische Typ des Call-Sites; der Solver ersetzt
    /// monomorphe Sites durch `Call` (CHA-Devirtualisierung).
    CallVirtual {
        dest: Option<Local>,
        class: String,
        name: String,
        desc: String,
        params: Vec<Ty>,
        ret: Ty,
        args: Vec<Operand>,
    },
    /// Objektallokation; Felder sind genullt (Java-Default), Header gesetzt.
    New { dest: Local, class: String },
    /// Stack-Allokation: von der Escape-Analyse bewiesen, dass das Objekt
    /// die Funktion nie verlässt (Ownership light, DESIGN.md §6a) —
    /// Lebenszeit = Stack-Frame, wie ein Rust-Wert ohne Box.
    StackNew { dest: Local, class: String },
    GetField { dest: Local, obj: Operand, class: String, field: String },
    PutField { obj: Operand, class: String, field: String, value: Operand },
    GetStatic { dest: Local, class: String, field: String },
    PutStatic { class: String, field: String, value: Operand },
    /// Array-Allokation der Länge `len`; `elem` ist I32 oder Ref.
    NewArray { dest: Local, elem: Ty, len: Operand },
    ArrayLen { dest: Local, arr: Operand },
    /// `dest = arr[index]`; bounds-gecheckt.
    ArrayLoad { dest: Local, arr: Operand, index: Operand, elem: Ty },
    /// `arr[index] = value`; bounds-gecheckt.
    ArrayStore { arr: Operand, index: Operand, value: Operand, elem: Ty },
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Goto(Block),
    /// if op != 0 → then_blk sonst else_blk (Vergleiche liefern 0/1).
    Branch {
        cond: Operand,
        then_blk: Block,
        else_blk: Block,
    },
    Return(Option<Operand>),
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub statements: Vec<Statement>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone)]
pub struct Function {
    /// Gemangelter, linkbarer Name (z. B. `J_Hello_main_...`).
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Locals[0..params.len()] sind die Parameter.
    pub locals: Vec<Ty>,
    pub blocks: Vec<BasicBlock>,
}

// --- Klassenmodell (Closed World: alle Klassen sind zur Build-Zeit bekannt) ---

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub ty: Ty,
}

/// Compile-Zeit-Initialwert eines statischen Feldes (ConstantValue).
#[derive(Debug, Clone)]
pub enum ConstInit {
    I32(i32),
    I64(i64),
    F64(f64),
    Str(u32),
}

#[derive(Debug, Clone)]
pub struct StaticFieldInfo {
    pub name: String,
    pub ty: Ty,
    pub init: Option<ConstInit>,
}

#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub desc: String,
    pub is_static: bool,
    /// false bei abstract (kein Code-Attribut).
    pub has_body: bool,
    /// Gemangelter Funktionsname der Definition in dieser Klasse.
    pub mangled: String,
}

impl MethodInfo {
    /// Virtuell = nimmt am Dispatch teil (Instanzmethode, kein Konstruktor).
    pub fn is_virtual(&self) -> bool {
        !self.is_static && self.name != "<init>"
    }
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    /// Interner JVM-Name (z. B. `pkg/Foo`).
    pub name: String,
    /// None nur bei java/lang/Object (implizit, nicht registriert).
    pub super_name: Option<String>,
    pub is_interface: bool,
    /// Direkt implementierte/erweiterte Interfaces (JVM-Namen).
    pub interfaces: Vec<String>,
    /// Nur deklarierte Instanzfelder; Superklassen-Felder über die Kette.
    pub fields: Vec<FieldInfo>,
    pub static_fields: Vec<StaticFieldInfo>,
    pub methods: Vec<MethodInfo>,
    /// Hat die Klasse einen statischen Initialisierer (<clinit>)?
    pub has_clinit: bool,
}

#[derive(Debug, Default)]
pub struct Program {
    pub functions: Vec<Function>,
    pub classes: Vec<ClassInfo>,
    /// String-Literal-Pool; `Operand::ConstStr` indiziert hierher.
    pub strings: Vec<String>,
    /// Klassen mit Class-Objekt (durch Reflection berührt):
    /// Klassenname → String-Index des gepunkteten Namens (für getName).
    pub class_objects: Vec<(String, u32)>,
}

impl Program {
    pub fn intern_string(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return i as u32;
        }
        self.strings.push(s.to_string());
        (self.strings.len() - 1) as u32
    }

    /// Registriert das Class-Objekt einer Klasse und liefert den
    /// String-Index ihres gepunkteten Namens.
    pub fn intern_class_object(&mut self, class: &str) -> u32 {
        if let Some((_, sid)) = self.class_objects.iter().find(|(c, _)| c == class) {
            return *sid;
        }
        let dotted = class.replace('/', ".");
        let sid = self.intern_string(&dotted);
        self.class_objects.push((class.to_string(), sid));
        sid
    }

    pub fn class(&self, name: &str) -> Option<&ClassInfo> {
        self.classes.iter().find(|c| c.name == name)
    }

    /// Feld-Auflösung: läuft die Superklassen-Kette von `class` hoch bis zur
    /// deklarierenden Klasse (JVMS 5.4.3.2). Liefert (Besitzerklasse, Typ).
    pub fn resolve_field(&self, class: &str, field: &str) -> Option<(&str, Ty)> {
        let mut cur = self.class(class)?;
        loop {
            if let Some(f) = cur.fields.iter().find(|f| f.name == field) {
                return Some((cur.name.as_str(), f.ty));
            }
            cur = self.class(cur.super_name.as_deref()?)?;
        }
    }

    /// Statisches Feld auflösen (Superklassen-Kette hoch). Liefert
    /// (Besitzerklasse, Typ).
    pub fn resolve_static_field(&self, class: &str, field: &str) -> Option<(&str, Ty)> {
        let mut cur = self.class(class)?;
        loop {
            if let Some(f) = cur.static_fields.iter().find(|f| f.name == field) {
                return Some((cur.name.as_str(), f.ty));
            }
            cur = self.class(cur.super_name.as_deref()?)?;
        }
    }

    /// Methoden-Auflösung: findet die Implementierung von `name`+`desc`
    /// ab `class` aufwärts. Liefert die definierende ClassInfo + MethodInfo.
    pub fn resolve_method(&self, class: &str, name: &str, desc: &str) -> Option<(&ClassInfo, &MethodInfo)> {
        let mut cur = self.class(class)?;
        loop {
            if let Some(m) = cur.methods.iter().find(|m| m.name == name && m.desc == desc && m.has_body) {
                return Some((cur, m));
            }
            cur = self.class(cur.super_name.as_deref()?)?;
        }
    }

    /// Ist `sub` gleich `sup` oder eine (transitive) Subklasse?
    pub fn is_subclass(&self, sub: &str, sup: &str) -> bool {
        let mut cur = sub;
        loop {
            if cur == sup {
                return true;
            }
            match self.class(cur).and_then(|c| c.super_name.as_deref()) {
                Some(s) => cur = s,
                None => return false,
            }
        }
    }

    /// Alle Interfaces, die `class` (transitiv über Super-Kette und
    /// Interface-Vererbung) implementiert.
    pub fn all_interfaces(&self, class: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let mut stack = vec![class.to_string()];
        while let Some(c) = stack.pop() {
            let Some(ci) = self.class(&c) else { continue };
            for i in &ci.interfaces {
                if out.insert(i.clone()) {
                    stack.push(i.clone());
                }
            }
            if let Some(s) = &ci.super_name {
                stack.push(s.clone());
            }
        }
        out
    }

    /// Implementiert `class` das Interface `iface` (oder ist gleich)?
    pub fn implements(&self, class: &str, iface: &str) -> bool {
        class == iface || self.all_interfaces(class).contains(iface)
    }
}

/// Macht aus einem JVM-Namen einen linkbaren Bezeichner.
pub fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// Linker-Symbol einer Methode. Muss über alle Crates konsistent sein.
pub fn mangle(class: &str, name: &str, descriptor: &str) -> String {
    if name == "main" && descriptor == "([Ljava/lang/String;)V" {
        return "java_main".to_string();
    }
    format!("J_{}_{}_{}", sanitize(class), sanitize(name), sanitize(descriptor))
}

/// Symbol des statischen Initialisierers einer Klasse.
pub fn clinit_symbol(class: &str) -> String {
    mangle(class, "<clinit>", "()V")
}

// --- Textuelle Ausgabe für Debugging (`--emit-ir`) ---

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.strings.iter().enumerate() {
            writeln!(f, "str{i} = {s:?}")?;
        }
        for func in &self.functions {
            write!(f, "{func}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\nfn {}({:?}) -> {:?} {{", self.name, self.params, self.ret)?;
        for (i, ty) in self.locals.iter().enumerate() {
            writeln!(f, "  let _{i}: {ty:?};")?;
        }
        for (i, bb) in self.blocks.iter().enumerate() {
            writeln!(f, "  bb{i}:")?;
            for st in &bb.statements {
                writeln!(f, "    {st:?}")?;
            }
            writeln!(f, "    {:?}", bb.terminator)?;
        }
        writeln!(f, "}}")
    }
}
