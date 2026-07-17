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

use fastllvm_classfile::{ArrTy, ClassFile, Cond, Const, Instr};
use fastllvm_ir::*;

/// Array-Elementtyp des Classfile-Decoders → IR-Typ.
fn arrty_ty(t: ArrTy) -> Ty {
    match t {
        ArrTy::Int => Ty::I32,
        ArrTy::Long => Ty::I64,
        ArrTy::Float => Ty::F32,
        ArrTy::Double => Ty::F64,
        ArrTy::Ref => Ty::Ref,
    }
}

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

/// Referenzierter Zieltyp eines Feld-Deskriptors (für die Azyklizitäts-
/// Analyse): `LNode;`/`[LNode;` → `Node`; Primitive und Primitiv-Arrays
/// (`I`, `[I`) → `None` (referenzieren nichts, keine Zyklus-Kante).
fn ref_target_of(desc: &str) -> Option<String> {
    let elem = desc.trim_start_matches('[');
    elem.strip_prefix('L').map(|s| s.trim_end_matches(';').to_string())
}

/// Phase 1: Klassenmodell registrieren (vor dem Absenken aller Methoden,
/// damit Feld-/Methodenauflösung über Klassengrenzen funktioniert).
pub fn register_class(cf: &ClassFile, program: &mut Program) -> Result<()> {
    let field_ty_of = |desc: &str| -> Result<Ty> {
        let mut chars = desc.chars().peekable();
        let c = chars.next().ok_or_else(|| FrontendError::Unsupported("leerer Felddeskriptor".into()))?;
        field_ty(c, &mut chars, desc)
    };

    let mut fields = Vec::new();
    let mut static_fields = Vec::new();
    for f in &cf.fields {
        let ty = field_ty_of(&f.descriptor)?;
        if f.is_static() {
            // ConstantValue → Compile-Zeit-Initialwert (statische finals).
            let init = match f.constant_value {
                Some(idx) => Some(match cf.constant_pool.get(idx as usize) {
                    Some(Const::Integer(v)) => ConstInit::I32(*v),
                    Some(Const::Long(v)) => ConstInit::I64(*v),
                    Some(Const::Double(v)) => ConstInit::F64(*v),
                    Some(Const::Float(v)) => ConstInit::F64(*v as f64),
                    Some(Const::String { utf8 }) => ConstInit::Str(program.intern_string(cf.utf8(*utf8)?)),
                    _ => return Err(FrontendError::Unsupported(format!("ConstantValue von {}.{}", cf.this_class, f.name))),
                }),
                None => None,
            };
            static_fields.push(StaticFieldInfo { name: f.name.clone(), ty, init });
        } else {
            fields.push(FieldInfo { name: f.name.clone(), ty, ref_target: ref_target_of(&f.descriptor) });
        }
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
    let has_clinit = cf.methods.iter().any(|m| m.name == "<clinit>" && m.code.is_some());
    program.classes.push(ClassInfo {
        name: cf.this_class.clone(),
        super_name: cf.super_class.clone().filter(|s| s != "java/lang/Object"),
        is_interface: cf.access_flags & 0x0200 != 0,
        interfaces: cf.interfaces.clone(),
        fields,
        static_fields,
        methods,
        has_clinit,
    });
    Ok(())
}

/// Registriert die eingebauten Klassen, deren Methoden am virtuellen
/// Dispatch teilnehmen. Aktuell `java/lang/String` mit `equals`/`hashCode`/
/// `toString` (Runtime-Implementierungen `jrt_str_*`), damit ein
/// `Object`-typisierter Aufruf `obj.equals(x)` auf einen String korrekt
/// dispatcht (Grundlage für equals-basierte Collections).
pub fn register_builtins(program: &mut Program) {
    if program.class("java/lang/String").is_some() {
        return;
    }
    let m = |name: &str, desc: &str, mangled: &str| MethodInfo {
        name: name.to_string(),
        desc: desc.to_string(),
        is_static: false,
        has_body: true,
        mangled: mangled.to_string(),
    };
    let builtin = |name: &str, prefix: &str| ClassInfo {
        name: name.to_string(),
        super_name: None,
        is_interface: false,
        interfaces: vec!["java/lang/Comparable".to_string()],
        fields: Vec::new(),
        static_fields: Vec::new(),
        methods: vec![
            m("equals", "(Ljava/lang/Object;)Z", &format!("jrt_{prefix}_equals")),
            m("hashCode", "()I", &format!("jrt_{prefix}_hashcode")),
            m("toString", "()Ljava/lang/String;", &format!("jrt_{prefix}_tostring")),
            m("compareTo", "(Ljava/lang/Object;)I", &format!("jrt_{prefix}_compareto")),
        ],
        has_clinit: false,
    };
    // java.lang.Comparable: Interface mit compareTo (globaler Vtable-Slot), das
    // die Wrapper und String implementieren (für generische Comparable-Bounds).
    program.classes.push(ClassInfo {
        name: "java/lang/Comparable".to_string(),
        super_name: None,
        is_interface: true,
        interfaces: Vec::new(),
        fields: Vec::new(),
        static_fields: Vec::new(),
        methods: vec![MethodInfo {
            name: "compareTo".into(),
            desc: "(Ljava/lang/Object;)I".into(),
            is_static: false,
            has_body: false,
            mangled: mangle("java/lang/Comparable", "compareTo", "(Ljava/lang/Object;)I"),
        }],
        has_clinit: false,
    });
    program.classes.push(builtin("java/lang/String", "str"));
    program.classes.push(builtin("java/lang/Integer", "integer"));
    program.classes.push(builtin("java/lang/Long", "long"));
    program.classes.push(builtin("java/lang/Boolean", "boolean"));
    program.classes.push(builtin("java/lang/Double", "double"));
    program.classes.push(builtin("java/lang/Character", "character"));
    program.classes.push(builtin("java/lang/Float", "float"));
    register_enum(program);
    register_throwables(program);
    register_concurrency(program);
}

/// java.lang.Runnable (Funktionsinterface) + java.lang.Thread. Thread hält den
/// Runnable und ein natives Handle; `start()`/`join()` sind Frontend-Intrinsics
/// (jrt_thread_start/join). `run()` wird von der Runtime-Trampoline über den
/// globalen Runnable-Vtable-Slot aufgerufen.
fn register_concurrency(program: &mut Program) {
    program.classes.push(ClassInfo {
        name: "java/lang/Runnable".to_string(),
        super_name: None,
        is_interface: true,
        interfaces: Vec::new(),
        fields: Vec::new(),
        static_fields: Vec::new(),
        methods: vec![MethodInfo {
            name: "run".into(),
            desc: "()V".into(),
            is_static: false,
            has_body: false,
            mangled: mangle("java/lang/Runnable", "run", "()V"),
        }],
        has_clinit: false,
    });
    let init = mangle("java/lang/Thread", "<init>", "(Ljava/lang/Runnable;)V");
    program.classes.push(ClassInfo {
        name: "java/lang/Thread".to_string(),
        super_name: None,
        is_interface: false,
        interfaces: Vec::new(),
        fields: vec![
            FieldInfo { name: "$runnable".to_string(), ty: Ty::Ref, ref_target: Some("java/lang/Runnable".to_string()) },
            FieldInfo { name: "$handle".to_string(), ty: Ty::I64, ref_target: None },
        ],
        static_fields: Vec::new(),
        methods: vec![MethodInfo {
            name: "<init>".into(),
            desc: "(Ljava/lang/Runnable;)V".into(),
            is_static: false,
            has_body: true,
            mangled: init.clone(),
        }],
        has_clinit: false,
    });
    // Thread.<init>(runnable): this.$runnable = runnable.
    program.functions.push(Function {
        name: init,
        params: vec![Ty::Ref, Ty::Ref],
        ret: Ty::Void,
        locals: vec![Ty::Ref, Ty::Ref],
        blocks: vec![BasicBlock {
            statements: vec![Statement::PutField {
                obj: Operand::Copy(Local(0)),
                class: "java/lang/Thread".to_string(),
                field: "$runnable".into(),
                value: Operand::Copy(Local(1)),
            }],
            terminator: Terminator::Return(None),
        }],
    });
}

/// Throwable/Exception/RuntimeException als eingebaute Basisklassen: Throwable
/// hält das `$message`-Feld, alle drei bekommen `<init>()V` und
/// `<init>(String)V` (setzen die Message). `getMessage()` wird im Frontend als
/// Intrinsic abgefangen. Damit funktionieren `new RuntimeException("…")` und
/// benutzerdefinierte Exceptions mit Message; für das *catch* bleiben diese
/// drei Basistypen catch-all (Sentinels tragen keinen Type-Descriptor).
fn register_throwables(program: &mut Program) {
    // (Klasse, Superklasse)
    let chain = [
        ("java/lang/Throwable", None),
        ("java/lang/Exception", Some("java/lang/Throwable")),
        ("java/lang/RuntimeException", Some("java/lang/Exception")),
    ];
    for (cls, sup) in chain {
        let init0 = mangle(cls, "<init>", "()V");
        let init1 = mangle(cls, "<init>", "(Ljava/lang/String;)V");
        let fields = if cls == "java/lang/Throwable" {
            vec![FieldInfo { name: "$message".to_string(), ty: Ty::Ref, ref_target: Some("java/lang/String".to_string()) }]
        } else {
            Vec::new()
        };
        program.classes.push(ClassInfo {
            name: cls.to_string(),
            super_name: sup.map(str::to_string),
            is_interface: false,
            interfaces: Vec::new(),
            fields,
            static_fields: Vec::new(),
            methods: vec![
                MethodInfo { name: "<init>".into(), desc: "()V".into(), is_static: false, has_body: true, mangled: init0.clone() },
                MethodInfo { name: "<init>".into(), desc: "(Ljava/lang/String;)V".into(), is_static: false, has_body: true, mangled: init1.clone() },
            ],
            has_clinit: false,
        });
        // <init>(): this.$message = null
        program.functions.push(Function {
            name: init0,
            params: vec![Ty::Ref],
            ret: Ty::Void,
            locals: vec![Ty::Ref],
            blocks: vec![BasicBlock {
                statements: vec![Statement::PutField {
                    obj: Operand::Copy(Local(0)),
                    class: "java/lang/Throwable".to_string(),
                    field: "$message".into(),
                    value: Operand::ConstNull,
                }],
                terminator: Terminator::Return(None),
            }],
        });
        // <init>(String): this.$message = msg
        program.functions.push(Function {
            name: init1,
            params: vec![Ty::Ref, Ty::Ref],
            ret: Ty::Void,
            locals: vec![Ty::Ref, Ty::Ref],
            blocks: vec![BasicBlock {
                statements: vec![Statement::PutField {
                    obj: Operand::Copy(Local(0)),
                    class: "java/lang/Throwable".to_string(),
                    field: "$message".into(),
                    value: Operand::Copy(Local(1)),
                }],
                terminator: Terminator::Return(None),
            }],
        });
    }
    // java.lang.MatchException (exhaustive pattern-switch-Fallback) extends
    // RuntimeException; <init>(String, Throwable) setzt $message (Cause ignoriert).
    let me_init = mangle("java/lang/MatchException", "<init>", "(Ljava/lang/String;Ljava/lang/Throwable;)V");
    program.classes.push(ClassInfo {
        name: "java/lang/MatchException".to_string(),
        super_name: Some("java/lang/RuntimeException".to_string()),
        is_interface: false,
        interfaces: Vec::new(),
        fields: Vec::new(),
        static_fields: Vec::new(),
        methods: vec![MethodInfo {
            name: "<init>".into(),
            desc: "(Ljava/lang/String;Ljava/lang/Throwable;)V".into(),
            is_static: false,
            has_body: true,
            mangled: me_init.clone(),
        }],
        has_clinit: false,
    });
    program.functions.push(Function {
        name: me_init,
        params: vec![Ty::Ref, Ty::Ref, Ty::Ref],
        ret: Ty::Void,
        locals: vec![Ty::Ref, Ty::Ref, Ty::Ref],
        blocks: vec![BasicBlock {
            statements: vec![Statement::PutField {
                obj: Operand::Copy(Local(0)),
                class: "java/lang/Throwable".to_string(),
                field: "$message".into(),
                value: Operand::Copy(Local(1)),
            }],
            terminator: Terminator::Return(None),
        }],
    });
}

/// java.lang.Enum als Basisklasse aller enums: hält name (String) und
/// ordinal (int); name()/ordinal()/toString() lesen die Felder, der
/// Konstruktor <init>(String,int) setzt sie. Methodenrümpfe werden direkt
/// als IR erzeugt.
fn register_enum(program: &mut Program) {
    let cls = "java/lang/Enum";
    let name_m = mangle(cls, "name", "()Ljava/lang/String;");
    let ord_m = mangle(cls, "ordinal", "()I");
    let tostr_m = mangle(cls, "toString", "()Ljava/lang/String;");
    let init_m = mangle(cls, "<init>", "(Ljava/lang/String;I)V");

    program.classes.push(ClassInfo {
        name: cls.to_string(),
        super_name: None,
        is_interface: false,
        interfaces: Vec::new(),
        fields: vec![
            FieldInfo { name: "$name".to_string(), ty: Ty::Ref, ref_target: Some("java/lang/String".to_string()) },
            FieldInfo { name: "$ordinal".to_string(), ty: Ty::I32, ref_target: None },
        ],
        static_fields: Vec::new(),
        methods: vec![
            MethodInfo { name: "name".into(), desc: "()Ljava/lang/String;".into(), is_static: false, has_body: true, mangled: name_m.clone() },
            MethodInfo { name: "ordinal".into(), desc: "()I".into(), is_static: false, has_body: true, mangled: ord_m.clone() },
            MethodInfo { name: "toString".into(), desc: "()Ljava/lang/String;".into(), is_static: false, has_body: true, mangled: tostr_m.clone() },
            MethodInfo { name: "<init>".into(), desc: "(Ljava/lang/String;I)V".into(), is_static: false, has_body: true, mangled: init_m.clone() },
        ],
        has_clinit: false,
    });

    // name()/toString(): return this.$name
    let getter_name = |mangled: String| Function {
        name: mangled,
        params: vec![Ty::Ref],
        ret: Ty::Ref,
        locals: vec![Ty::Ref, Ty::Ref],
        blocks: vec![BasicBlock {
            statements: vec![Statement::GetField {
                dest: Local(1),
                obj: Operand::Copy(Local(0)),
                class: cls.to_string(),
                field: "$name".to_string(),
            }],
            terminator: Terminator::Return(Some(Operand::Copy(Local(1)))),
        }],
    };
    program.functions.push(getter_name(name_m));
    program.functions.push(getter_name(tostr_m));
    // ordinal(): return this.$ordinal
    program.functions.push(Function {
        name: ord_m,
        params: vec![Ty::Ref],
        ret: Ty::I32,
        locals: vec![Ty::Ref, Ty::I32],
        blocks: vec![BasicBlock {
            statements: vec![Statement::GetField {
                dest: Local(1),
                obj: Operand::Copy(Local(0)),
                class: cls.to_string(),
                field: "$ordinal".to_string(),
            }],
            terminator: Terminator::Return(Some(Operand::Copy(Local(1)))),
        }],
    });
    // <init>(name, ordinal): this.$name = name; this.$ordinal = ordinal
    program.functions.push(Function {
        name: init_m,
        params: vec![Ty::Ref, Ty::Ref, Ty::I32],
        ret: Ty::Void,
        locals: vec![Ty::Ref, Ty::Ref, Ty::I32],
        blocks: vec![BasicBlock {
            statements: vec![
                Statement::PutField { obj: Operand::Copy(Local(0)), class: cls.to_string(), field: "$name".into(), value: Operand::Copy(Local(1)) },
                Statement::PutField { obj: Operand::Copy(Local(0)), class: cls.to_string(), field: "$ordinal".into(), value: Operand::Copy(Local(2)) },
            ],
            terminator: Terminator::Return(None),
        }],
    });
}

/// Phase 2: alle Methodenrümpfe absenken.
pub fn lower_class(cf: &ClassFile, program: &mut Program) -> Result<()> {
    for m in &cf.methods {
        let Some(code) = &m.code else { continue };
        let f = lower_method(cf, m, code, program)?;
        program.functions.push(f);
    }
    Ok(())
}

pub use fastllvm_ir::{clinit_symbol as clinit_name, mangle};

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

/// Roh-Parameter-Deskriptoren eines Methodendeskriptors (z. B.
/// `["I", "Ljava/lang/String;"]`) — für die String-Konkatenation, die je
/// nach Typ eine andere `to_str`-Konvertierung braucht.
/// Details eines Lambda-Callsites (aus dem invokedynamic + LambdaMetafactory).
struct LambdaInfo {
    iface: String,       // Funktionsinterface (Rückgabetyp des indy)
    sam_method: String,  // Name der Interface-Methode (z. B. "apply")
    sam_desc: String,    // Deskriptor der Interface-Methode
    kind: u8,            // MethodHandle-Referenzart (5=virtual, 6=static, …)
    impl_class: String,  // Klasse der Ziel-/Rumpf-Methode
    impl_name: String,   // lambda$… oder referenzierte Methode
    impl_desc: String,   // Deskriptor der Ziel-Methode
    captures: Vec<Ty>,   // eingefangene Variablen (indy-Parameter)
}

/// Registriert eine synthetische Klasse, die das Funktionsinterface
/// implementiert und die SAM-Methode an die Lambda-Rumpf-Methode
/// weiterleitet (captures aus Feldern + eigene Argumente). Liefert den
/// Klassennamen (idempotent pro Rumpf-Methode).
fn register_lambda(program: &mut Program, info: &LambdaInfo) -> Result<String> {
    let class_name = format!(
        "$lambda${}${}${}${}",
        info.iface, info.sam_method, info.impl_class, info.impl_name
    );
    if program.class(&class_name).is_some() {
        return Ok(class_name);
    }
    // Capture-Felder cap0.. mit den eingefangenen Typen.
    let fields: Vec<FieldInfo> = info
        .captures
        .iter()
        .enumerate()
        // Eingefangene Ref-Variablen konservativ als Object-Referenz werten
        // (breite Zyklus-Kante — sound für die Azyklizitäts-Analyse).
        .map(|(i, &ty)| FieldInfo {
            name: format!("cap{i}"),
            ty,
            ref_target: (ty == Ty::Ref).then(|| "java/lang/Object".to_string()),
        })
        .collect();

    let (sam_params, sam_ret) = parse_descriptor(&info.sam_desc)?;
    let sam_mangled = mangle(&class_name, &info.sam_method, &info.sam_desc);

    program.classes.push(ClassInfo {
        name: class_name.clone(),
        super_name: None,
        is_interface: false,
        interfaces: vec![info.iface.clone()],
        fields,
        static_fields: Vec::new(),
        methods: vec![MethodInfo {
            name: info.sam_method.clone(),
            desc: info.sam_desc.clone(),
            is_static: false,
            has_body: true,
            mangled: sam_mangled.clone(),
        }],
        has_clinit: false,
    });

    // Rumpf der SAM-Methode: Captures aus Feldern laden, dann die
    // Lambda-Rumpf-Methode mit (captures…, sam-args…) aufrufen.
    let mut locals = vec![Ty::Ref]; // Local 0 = this
    locals.extend(sam_params.iter().copied());
    let n_sam = sam_params.len();

    let mut stmts = Vec::new();
    let mut impl_args = Vec::new();
    for (i, &cty) in info.captures.iter().enumerate() {
        locals.push(cty);
        let cap_local = Local((locals.len() - 1) as u32);
        stmts.push(Statement::GetField {
            dest: cap_local,
            obj: Operand::Copy(Local(0)),
            class: class_name.clone(),
            field: format!("cap{i}"),
        });
        impl_args.push(Operand::Copy(cap_local));
    }
    for k in 0..n_sam {
        impl_args.push(Operand::Copy(Local((1 + k) as u32)));
    }

    // Argument-Unboxing an der SAM-Grenze: erwartet die Zielmethode ein
    // Primitiv, während das Interface Object übergibt (z. B. F.apply(Integer)
    // → static int-Methode), wird via Wrapper-<prim>Value entpackt.
    // impl_param_descs sind die Rohtypen der Ziel-Parameter; bei virtuellen
    // Aufrufen ist der Receiver (Position 0) vorangestellt und bleibt Ref.
    let mut impl_param_descs = descriptor_params(&info.impl_desc)?;
    if matches!(info.kind, 5 | 9) {
        impl_param_descs.insert(0, "Ljava/lang/Object;".to_string());
    }
    let arg_types: Vec<Ty> = info.captures.iter().copied().chain(sam_params.iter().copied()).collect();
    for (i, pd) in impl_param_descs.iter().enumerate() {
        let pc = pd.chars().next().unwrap();
        let is_prim = matches!(pc, 'I' | 'S' | 'B' | 'C' | 'Z' | 'J' | 'F' | 'D');
        if is_prim && arg_types.get(i) == Some(&Ty::Ref) {
            let (unbox_fn, unbox_ty) = match pc {
                'J' => ("jrt_long_longvalue", Ty::I64),
                'F' => ("jrt_float_floatvalue", Ty::F32),
                'D' => ("jrt_double_doublevalue", Ty::F64),
                'C' => ("jrt_character_charvalue", Ty::I32),
                'Z' => ("jrt_boolean_booleanvalue", Ty::I32),
                _ => ("jrt_integer_intvalue", Ty::I32),
            };
            locals.push(unbox_ty);
            let unboxed = Local((locals.len() - 1) as u32);
            stmts.push(Statement::Call {
                dest: Some(unboxed),
                func: unbox_fn.to_string(),
                args: vec![impl_args[i].clone()],
            });
            impl_args[i] = Operand::Copy(unboxed);
        }
    }

    // Intrinsic-gestützte Ziele (z. B. String::length) direkt aufrufen —
    // sie haben keinen Vtable-Slot.
    let intrinsic = match (info.impl_class.as_str(), info.impl_name.as_str(), info.impl_desc.as_str()) {
        ("java/lang/String", "length", "()I") => Some("jrt_str_length"),
        ("java/lang/String", "isEmpty", "()Z") => Some("jrt_str_is_empty"),
        ("java/lang/String", "charAt", "(I)C") => Some("jrt_str_char_at"),
        ("java/lang/String", "hashCode", "()I") => Some("jrt_str_hashcode"),
        _ => None,
    };

    // Roher Rückgabetyp der Ziel-Methode (vor Adaption an den SAM-Typ).
    let impl_ret_char = info.impl_desc.rsplit_once(')').map(|(_, r)| r.chars().next()).flatten();
    let impl_ret = if info.kind == 8 {
        Ty::Ref // Konstruktor liefert ein Objekt
    } else {
        parse_descriptor(&info.impl_desc)?.1
    };
    // Roh-Ergebnis-Local (Typ der Ziel-Methode).
    let raw = if impl_ret == Ty::Void {
        None
    } else {
        locals.push(impl_ret);
        Some(Local((locals.len() - 1) as u32))
    };

    if let Some(f) = intrinsic {
        stmts.push(Statement::Call { dest: raw, func: f.to_string(), args: impl_args });
    } else {
        match info.kind {
            5 | 9 => {
                let (mut mparams, mret) = parse_descriptor(&info.impl_desc)?;
                mparams.insert(0, Ty::Ref);
                stmts.push(Statement::CallVirtual {
                    dest: raw,
                    class: info.impl_class.clone(),
                    name: info.impl_name.clone(),
                    desc: info.impl_desc.clone(),
                    params: mparams,
                    ret: mret,
                    args: impl_args,
                });
            }
            8 => {
                let obj = raw.expect("Konstruktor-Referenz muss ein Objekt liefern");
                stmts.push(Statement::New { dest: obj, class: info.impl_class.clone() });
                let mut ctor_args = vec![Operand::Copy(obj)];
                ctor_args.extend(impl_args);
                stmts.push(Statement::Call {
                    dest: None,
                    func: mangle(&info.impl_class, "<init>", &info.impl_desc),
                    args: ctor_args,
                });
            }
            _ => {
                stmts.push(Statement::Call {
                    dest: raw,
                    func: mangle(&info.impl_class, &info.impl_name, &info.impl_desc),
                    args: impl_args,
                });
            }
        }
    }

    // Rückgabe an den SAM-Typ anpassen: primitives Ergebnis → Wrapper boxen,
    // wenn das Interface Object erwartet (LambdaMetafactory-Adaption).
    let result = match (raw, sam_ret) {
        (Some(r), Ty::Ref) if impl_ret != Ty::Ref => {
            let box_fn = match impl_ret_char {
                Some('J') => "jrt_long_valueof",
                Some('F') => "jrt_float_valueof",
                Some('D') => "jrt_double_valueof",
                Some('C') => "jrt_character_valueof",
                Some('Z') => "jrt_boolean_valueof",
                _ => "jrt_integer_valueof",
            };
            locals.push(Ty::Ref);
            let boxed = Local((locals.len() - 1) as u32);
            stmts.push(Statement::Call { dest: Some(boxed), func: box_fn.to_string(), args: vec![Operand::Copy(r)] });
            Some(boxed)
        }
        (r, _) => r,
    };
    let terminator = Terminator::Return(result.map(Operand::Copy));

    program.functions.push(Function {
        name: sam_mangled,
        params: locals[..=n_sam].to_vec(),
        ret: sam_ret,
        locals,
        blocks: vec![BasicBlock { statements: stmts, terminator }],
    });

    Ok(class_name)
}

/// Fügt einen Konvertierungs-Call (`jrt_*_to_str`) ein und liefert das
/// String-Ergebnis-Local als Operand.
fn str_conv(ml: &mut MethodLowering, stmts: &mut Vec<Statement>, func: &str, val: Local) -> Operand {
    let l = ml.fresh(Ty::Ref);
    stmts.push(Statement::Call {
        dest: Some(l),
        func: func.to_string(),
        args: vec![Operand::Copy(val)],
    });
    Operand::Copy(l)
}

/// Bytegröße eines Ref-/Primitiv-Felds (für Record-memcmp-equals).
fn ty_size(t: Ty) -> i64 {
    match t {
        Ty::I64 | Ty::F64 | Ty::Ref => 8,
        _ => 4,
    }
}

/// Feldwert → String (für Record-toString).
fn record_val_str(ml: &mut MethodLowering, stmts: &mut Vec<Statement>, ty: Ty, val: Local) -> Operand {
    match ty {
        Ty::I32 => str_conv(ml, stmts, "jrt_int_to_str", val),
        Ty::I64 => str_conv(ml, stmts, "jrt_long_to_str", val),
        Ty::F64 => str_conv(ml, stmts, "jrt_double_to_str", val),
        Ty::F32 => str_conv(ml, stmts, "jrt_float_to_str", val),
        Ty::Ref => {
            let l = ml.fresh(Ty::Ref);
            stmts.push(Statement::CallVirtual {
                dest: Some(l),
                class: "java/lang/Object".to_string(),
                name: "toString".to_string(),
                desc: "()Ljava/lang/String;".to_string(),
                params: vec![Ty::Ref],
                ret: Ty::Ref,
                args: vec![Operand::Copy(val)],
            });
            Operand::Copy(l)
        }
        Ty::Void => Operand::ConstNull,
    }
}

/// Feldwert → i32-Hash (für Record-hashCode; muss nur konsistent/≠0 sein).
fn record_val_hash(ml: &mut MethodLowering, stmts: &mut Vec<Statement>, ty: Ty, val: Local) -> Operand {
    match ty {
        Ty::I32 => Operand::Copy(val),
        Ty::I64 => {
            let l = ml.fresh(Ty::I32);
            stmts.push(Statement::Assign(l, Rvalue::Convert(Operand::Copy(val))));
            Operand::Copy(l)
        }
        // Float/Double: fester (konsistenter) Beitrag — gleiche Records hashen
        // gleich; die Verteilung ist gröber, aber der Kontrakt bleibt gewahrt.
        Ty::F32 | Ty::F64 => Operand::ConstI32(1),
        Ty::Ref => {
            let l = ml.fresh(Ty::I32);
            stmts.push(Statement::CallVirtual {
                dest: Some(l),
                class: "java/lang/Object".to_string(),
                name: "hashCode".to_string(),
                desc: "()I".to_string(),
                params: vec![Ty::Ref],
                ret: Ty::I32,
                args: vec![Operand::Copy(val)],
            });
            Operand::Copy(l)
        }
        Ty::Void => Operand::ConstI32(0),
    }
}

/// Schiebt angesammelte Literalzeichen als String-Konstante in die Teileliste.
fn flush_lit(lit: &mut String, parts: &mut Vec<Operand>, program: &mut Program) {
    if !lit.is_empty() {
        let sid = program.intern_string(lit);
        parts.push(Operand::ConstStr(sid));
        lit.clear();
    }
}

fn descriptor_params(desc: &str) -> Result<Vec<String>> {
    let inner = desc
        .strip_prefix('(')
        .and_then(|s| s.split_once(')'))
        .ok_or_else(|| FrontendError::Unsupported(format!("Deskriptor {desc}")))?
        .0;
    let mut out = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        let mut s = String::from(c);
        match c {
            'L' => {
                for c2 in chars.by_ref() {
                    s.push(c2);
                    if c2 == ';' {
                        break;
                    }
                }
            }
            '[' => {
                return Err(FrontendError::Unsupported(format!("Array-Argument in {desc}")));
            }
            _ => {}
        }
        out.push(s);
    }
    Ok(out)
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
        'F' => Ok(Ty::F32),
        'D' => Ok(Ty::F64),
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

/// Woher der Wert eines Locals zuletzt kam — lokale Konstantenpropagation
/// für die statische Reflection-Auflösung (DESIGN.md §1.3). javac legt
/// `ldc`-Argumente direkt vor dem Aufruf ab, daher reicht der Blick in den
/// aktuellen Block.
enum Origin<'a> {
    Op(&'a Operand),
    New(&'a str),
    Opaque,
}

fn origin_of<'a>(stmts: &'a [Statement], l: Local) -> Origin<'a> {
    origin_from(stmts, stmts.len(), l, 8)
}

/// Sucht die letzte Zuweisung an `l` vor Index `upto` und verfolgt
/// Copy-Ketten (astore/aload legt Werte über JVM-Slot-Locals um).
fn origin_from<'a>(stmts: &'a [Statement], upto: usize, l: Local, depth: u32) -> Origin<'a> {
    if depth == 0 {
        return Origin::Opaque;
    }
    for i in (0..upto).rev() {
        match &stmts[i] {
            Statement::Assign(d, rv) if *d == l => {
                return match rv {
                    Rvalue::Use(Operand::Copy(src)) => origin_from(stmts, i, *src, depth - 1),
                    Rvalue::Use(op) => Origin::Op(op),
                    _ => Origin::Opaque,
                };
            }
            Statement::New { dest, class } if *dest == l => return Origin::New(class),
            Statement::Call { dest: Some(d), .. }
            | Statement::CallVirtual { dest: Some(d), .. }
            | Statement::GetField { dest: d, .. }
                if *d == l =>
            {
                return Origin::Opaque;
            }
            _ => {}
        }
    }
    Origin::Opaque
}

struct MethodLowering<'a> {
    cf: &'a ClassFile,
    locals: Vec<Ty>,
    /// (JVM-Local-Slot, Typ) → IR-Local. Slots sind untypisiert wiederverwendbar.
    slot_map: HashMap<(u16, Ty), Local>,
    /// (Stack-Tiefe, Typ) → IR-Local.
    stack_map: HashMap<(usize, Ty), Local>,
    /// IR-Local → bekannter ConstClass-Wert (Reflection). Wird über
    /// Kopien (aload/astore) propagiert, damit `getName`/`newInstance` das
    /// Class-Objekt auch blockübergreifend statisch auflösen (die
    /// Origin-Analyse ist nur block-lokal, seit invokestatic splittet).
    class_const: HashMap<Local, String>,
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
    // Nur die (ggf. per Manifest gewählte) Einstiegsklasse liefert java_main;
    // sonst jede main (Einzeldatei-Modus). So kollidieren mehrere main-Methoden
    // in einem JAR nicht.
    let is_main = m.name == "main"
        && m.descriptor == "([Ljava/lang/String;)V"
        && match &program.main_class {
            Some(mc) => cf.this_class == *mc,
            None => true,
        };
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
            Instr::IfICmp(_, t)
            | Instr::IfZero(_, t)
            | Instr::IfACmp(_, t)
            | Instr::IfRefNull(_, t) => {
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
            Instr::Switch(default, cases) => {
                leaders.push(*default);
                for (_, t) in cases {
                    leaders.push(*t);
                }
                if let Some((next_pc, _)) = instrs.get(i + 1) {
                    leaders.push(*next_pc);
                }
            }
            Instr::Return | Instr::IReturn | Instr::AReturn | Instr::LReturn | Instr::DReturn
            | Instr::FReturn | Instr::AThrow => {
                if let Some((next_pc, _)) = instrs.get(i + 1) {
                    leaders.push(*next_pc);
                }
            }
            // Werfende Operationen beenden den Block (danach folgt der
            // Exception-Check). invokedynamic (Konkatenation) wirft nicht;
            // Division wirft ArithmeticException.
            Instr::InvokeStatic(_) | Instr::InvokeVirtual(_) | Instr::InvokeSpecial(_)
            | Instr::InvokeInterface(_)
            | Instr::IDiv | Instr::IRem | Instr::LDiv | Instr::LRem
            | Instr::ArrLoad(_) | Instr::ArrStore(_)
            | Instr::ArrayLength | Instr::GetField(_) | Instr::PutField(_) => {
                if let Some((next_pc, _)) = instrs.get(i + 1) {
                    leaders.push(*next_pc);
                }
            }
            _ => {}
        }
    }
    // Exception-Handler-Einstiege sind Leader.
    for e in &code.exceptions {
        leaders.push(e.handler_pc as usize);
    }
    leaders.sort_unstable();
    leaders.dedup();
    let block_of_pc: HashMap<usize, Block> =
        leaders.iter().enumerate().map(|(i, pc)| (*pc, Block(i as u32))).collect();

    // Synthetischer Propagate-Block (letzter Block): wird angesprungen, wenn
    // eine Exception aus dieser Methode heraus propagiert. Er läuft ins
    // Funktions-Cleanup (Backend released die Locals; die Exception bleibt in
    // jrt_pending) und returnt einen Dummy — der Aufrufer prüft pending.
    let propagate_block = Block(leaders.len() as u32);

    // Handler-Blöcke und, pro werfender Instruktion, das Sprungziel im
    // Ausnahmefall (innerster umschließender Handler oder Propagate-Block).
    let handler_blocks: std::collections::HashSet<Block> = code
        .exceptions
        .iter()
        .map(|e| block_of_pc[&(e.handler_pc as usize)])
        .collect();
    // Alle Handler, die einen pc abdecken (in Table-Reihenfolge). catch_type 0
    // oder eine nicht modellierte Klasse (java/lang/Exception, …) wirkt als
    // catch-all (None); modellierte Klassen als echter instanceof-Typ.
    let handlers_of_pc = |pc: usize| -> Vec<(Option<String>, Block)> {
        code.exceptions
            .iter()
            .filter(|e| (e.start_pc as usize) <= pc && pc < (e.end_pc as usize))
            .map(|e| {
                let cc = if e.catch_type == 0 {
                    None
                } else {
                    match cf.class_name(e.catch_type) {
                        // Die eingebauten Basis-Throwables bleiben catch-all:
                        // Laufzeit-Sentinels (Arith/NPE/Bounds) tragen keinen
                        // Type-Descriptor und würden einen typisierten
                        // instanceof gegen RuntimeException sonst verfehlen.
                        Ok("java/lang/Throwable")
                        | Ok("java/lang/Exception")
                        | Ok("java/lang/RuntimeException") => None,
                        Ok(c) if program.class(c).is_some() => Some(c.to_string()),
                        _ => None,
                    }
                };
                (cc, block_of_pc[&(e.handler_pc as usize)])
            })
            .collect()
    };
    // Länge der Dispatch-Kette: bis einschließlich des ersten catch-all.
    let chain_len = |list: &[(Option<String>, Block)]| -> usize {
        list.iter().position(|(cc, _)| cc.is_none()).map(|i| i + 1).unwrap_or(list.len())
    };

    // Für jeden werfenden pc das Exception-Ziel: direkter Handler (einzelner
    // catch-all), Kette (typspezifisch) oder Propagate-Block. Ketten werden
    // nach Handler-Liste dedupliziert und synthetische Blöcke ab hinter dem
    // Propagate-Block angesiedelt.
    let mut chain_entry: HashMap<Vec<(Option<String>, Block)>, Block> = HashMap::new();
    let mut chains: Vec<(Block, Vec<(Option<String>, Block)>)> = Vec::new();
    let mut next_synth = propagate_block.0 + 1;
    let mut exc_target_of_pc: HashMap<usize, Block> = HashMap::new();
    for (pc, _) in &instrs {
        let list = handlers_of_pc(*pc);
        let target = if list.is_empty() {
            propagate_block
        } else if list[0].0.is_none() {
            list[0].1 // erster Handler fängt alles → direkt
        } else {
            *chain_entry.entry(list.clone()).or_insert_with(|| {
                let entry = Block(next_synth);
                next_synth += chain_len(&list) as u32;
                chains.push((entry, list.clone()));
                entry
            })
        };
        exc_target_of_pc.insert(*pc, target);
    }

    let mut ml = MethodLowering {
        cf,
        locals: Vec::new(),
        slot_map: HashMap::new(),
        stack_map: HashMap::new(),
        class_const: HashMap::new(),
    };

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
    // Handler betreten mit genau der Exception auf dem Stack (JVMS 4.10.1).
    for &hb in &handler_blocks {
        block_entry_stack.insert(hb, vec![Ty::Ref]);
    }
    let mut done: Vec<Option<BasicBlock>> = vec![None; leaders.len()];
    // Handler sind eigene Einstiegspunkte: die Dispatch-Ketten springen sie
    // an, nicht die werfenden Blöcke direkt.
    let mut worklist = vec![Block(0)];
    for &hb in &handler_blocks {
        worklist.push(hb);
    }

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
            handler_blocks.contains(&blk),
            &exc_target_of_pc,
        )?;
        for (succ, stack) in succs {
            // Propagate- und Dispatch-Ketten-Blöcke sind synthetisch (Index
            // ab propagate_block) und werden separat generiert.
            if succ.0 >= propagate_block.0 {
                continue;
            }
            // Handler-Eintrittsstacks sind fest [Ref] und werden nicht vom
            // Vorgänger überschrieben (der Ausnahme-Zweig leert den Stack).
            if handler_blocks.contains(&succ) {
                worklist.push(succ);
                continue;
            }
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
    let mut blocks: Vec<BasicBlock> = done
        .into_iter()
        .map(|b| b.unwrap_or(BasicBlock { statements: Vec::new(), terminator: Terminator::Return(None) }))
        .collect();

    // Propagate-Block anhängen: Return eines Dummy passend zum Rückgabetyp
    // (der Wert wird nie benutzt — der Aufrufer sieht die pending exception).
    let dummy = match ret {
        Ty::Void => None,
        Ty::I32 => Some(Operand::ConstI32(0)),
        Ty::I64 => Some(Operand::ConstI64(0)),
        Ty::F32 => Some(Operand::ConstF32(0.0)),
        Ty::F64 => Some(Operand::ConstF64(0.0)),
        Ty::Ref => Some(Operand::ConstNull),
    };
    blocks.push(BasicBlock { statements: Vec::new(), terminator: Terminator::Return(dummy) });

    // Dispatch-Ketten der typspezifischen catch-Blöcke anhängen. Reihenfolge
    // = Zuweisungsreihenfolge, damit die vorab vergebenen Indizes stimmen.
    for (entry, list) in &chains {
        let n = chain_len(list);
        for i in 0..n {
            let (cc, handler) = &list[i];
            let block = match cc {
                None => BasicBlock { statements: Vec::new(), terminator: Terminator::Goto(*handler) },
                Some(class) => {
                    let c = ml.fresh(Ty::I32);
                    let next = if i + 1 < n { Block(entry.0 + (i + 1) as u32) } else { propagate_block };
                    BasicBlock {
                        statements: vec![Statement::InstanceOfPending { dest: c, class: class.clone() }],
                        terminator: Terminator::Branch { cond: Operand::Copy(c), then_blk: *handler, else_blk: next },
                    }
                }
            };
            debug_assert_eq!(blocks.len() as u32, entry.0 + i as u32);
            blocks.push(block);
        }
    }

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
    is_handler: bool,
    exc_target_of_pc: &HashMap<usize, Block>,
) -> Result<(BasicBlock, Vec<(Block, Vec<Ty>)>)> {
    // Stack als Liste von Typen; Wert der Tiefe d liegt im Local stack_slot(d, ty).
    let mut stack: Vec<Ty> = entry_stack;
    let mut stmts: Vec<Statement> = Vec::new();

    // Handler betreten mit der Exception auf dem Stack: aus jrt_pending holen.
    if is_handler {
        let l = ml.stack_slot(0, Ty::Ref);
        stmts.push(Statement::Call {
            dest: Some(l),
            func: "jrt_take_pending".to_string(),
            args: Vec::new(),
        });
    }
    // Werfender Aufruf am Blockende → Terminator prüft die pending exception.
    let mut throw_after: Option<usize> = None;

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
        if terminator.is_some() {
            // Darf nie passieren: hieße, die Leader-Berechnung hat einen
            // Terminator-Opcode nicht als Blockende erkannt.
            return Err(FrontendError::Unsupported(format!(
                "interner Fehler: Instruktion nach Terminator bei pc={pc}"
            )));
        }
        match instr {
            Instr::Nop => {}
            Instr::IConst(v) | Instr::LdcInt(v) => {
                push!(Ty::I32, Rvalue::Use(Operand::ConstI32(*v)));
            }
            Instr::LdcString(idx) => {
                match ml.cf.constant_pool.get(*idx as usize) {
                    Some(Const::String { utf8 }) => {
                        let sid = program.intern_string(ml.cf.utf8(*utf8)?);
                        push!(Ty::Ref, Rvalue::Use(Operand::ConstStr(sid)));
                    }
                    Some(Const::Float(v)) => {
                        push!(Ty::F32, Rvalue::Use(Operand::ConstF32(*v)));
                    }
                    // ldc einer Klassenkonstante (`Widget.class`).
                    Some(Const::Class { .. }) => {
                        let class = ml.cf.class_name(*idx)?.to_string();
                        if program.class(&class).is_none() {
                            return Err(FrontendError::Unsupported(format!(
                                "{class}.class (Klasse nicht im Closed-World-Input)"
                            )));
                        }
                        program.intern_class_object(&class);
                        let l = push!(Ty::Ref, Rvalue::Use(Operand::ConstClass(class.clone())));
                        ml.class_const.insert(l, class);
                    }
                    _ => return Err(FrontendError::Unsupported(format!("ldc auf CP-Index {idx}"))),
                }
            }
            Instr::LConst(v) => {
                push!(Ty::I64, Rvalue::Use(Operand::ConstI64(*v)));
            }
            Instr::FConst(v) => {
                push!(Ty::F32, Rvalue::Use(Operand::ConstF32(*v)));
            }
            Instr::DConst(v) => {
                push!(Ty::F64, Rvalue::Use(Operand::ConstF64(*v)));
            }
            Instr::Ldc2W(idx) => match ml.cf.constant_pool.get(*idx as usize) {
                Some(Const::Long(v)) => {
                    push!(Ty::I64, Rvalue::Use(Operand::ConstI64(*v)));
                }
                Some(Const::Double(v)) => {
                    push!(Ty::F64, Rvalue::Use(Operand::ConstF64(*v)));
                }
                _ => return Err(FrontendError::Unsupported(format!("ldc2_w auf CP-Index {idx}"))),
            },
            Instr::ILoad(slot) => {
                let l = ml.jvm_slot(*slot, Ty::I32);
                push!(Ty::I32, Rvalue::Use(Operand::Copy(l)));
            }
            Instr::LLoad(slot) => {
                let l = ml.jvm_slot(*slot, Ty::I64);
                push!(Ty::I64, Rvalue::Use(Operand::Copy(l)));
            }
            Instr::FLoad(slot) => {
                let l = ml.jvm_slot(*slot, Ty::F32);
                push!(Ty::F32, Rvalue::Use(Operand::Copy(l)));
            }
            Instr::DLoad(slot) => {
                let l = ml.jvm_slot(*slot, Ty::F64);
                push!(Ty::F64, Rvalue::Use(Operand::Copy(l)));
            }
            Instr::LStore(slot) => {
                let v = pop!();
                let l = ml.jvm_slot(*slot, Ty::I64);
                stmts.push(Statement::Assign(l, Rvalue::Use(Operand::Copy(v))));
            }
            Instr::FStore(slot) => {
                let v = pop!();
                let l = ml.jvm_slot(*slot, Ty::F32);
                stmts.push(Statement::Assign(l, Rvalue::Use(Operand::Copy(v))));
            }
            Instr::DStore(slot) => {
                let v = pop!();
                let l = ml.jvm_slot(*slot, Ty::F64);
                stmts.push(Statement::Assign(l, Rvalue::Use(Operand::Copy(v))));
            }
            Instr::ALoad(slot) => {
                let l = ml.jvm_slot(*slot, Ty::Ref);
                let dest = push!(Ty::Ref, Rvalue::Use(Operand::Copy(l)));
                if let Some(c) = ml.class_const.get(&l).cloned() {
                    ml.class_const.insert(dest, c); // ConstClass über Kopie propagieren
                }
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
                match ml.class_const.get(&v).cloned() {
                    Some(c) => { ml.class_const.insert(l, c); }
                    None => { ml.class_const.remove(&l); } // Slot überschrieben
                }
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
            Instr::LNeg => {
                let v = pop!();
                push!(Ty::I64, Rvalue::Neg(Operand::Copy(v)));
            }
            Instr::DNeg => {
                let v = pop!();
                push!(Ty::F64, Rvalue::Neg(Operand::Copy(v)));
            }
            Instr::FNeg => {
                let v = pop!();
                push!(Ty::F32, Rvalue::Neg(Operand::Copy(v)));
            }
            Instr::FAdd | Instr::FSub | Instr::FMul | Instr::FDiv | Instr::FRem => {
                let op = match instr {
                    Instr::FAdd => BinOp::Add,
                    Instr::FSub => BinOp::Sub,
                    Instr::FMul => BinOp::Mul,
                    Instr::FDiv => BinOp::Div,
                    _ => BinOp::Rem,
                };
                let b = pop!();
                let a = pop!();
                push!(Ty::F32, Rvalue::Binary(op, Operand::Copy(a), Operand::Copy(b)));
            }
            Instr::LAdd | Instr::LSub | Instr::LMul | Instr::LAnd | Instr::LOr | Instr::LXor
            | Instr::LShl | Instr::LShr | Instr::LUShr => {
                let op = match instr {
                    Instr::LAdd => BinOp::Add,
                    Instr::LSub => BinOp::Sub,
                    Instr::LMul => BinOp::Mul,
                    Instr::LAnd => BinOp::And,
                    Instr::LOr => BinOp::Or,
                    Instr::LXor => BinOp::Xor,
                    Instr::LShl => BinOp::Shl,
                    Instr::LShr => BinOp::Shr,
                    _ => BinOp::UShr,
                };
                let b = pop!();
                let a = pop!();
                push!(Ty::I64, Rvalue::Binary(op, Operand::Copy(a), Operand::Copy(b)));
            }
            Instr::DAdd | Instr::DSub | Instr::DMul | Instr::DDiv | Instr::DRem => {
                let op = match instr {
                    Instr::DAdd => BinOp::Add,
                    Instr::DSub => BinOp::Sub,
                    Instr::DMul => BinOp::Mul,
                    Instr::DDiv => BinOp::Div,
                    _ => BinOp::Rem,
                };
                let b = pop!();
                let a = pop!();
                push!(Ty::F64, Rvalue::Binary(op, Operand::Copy(a), Operand::Copy(b)));
            }
            // long-Division/Rest über Runtime (Java: /0 wirft, MIN/-1 definiert).
            Instr::LDiv | Instr::LRem => {
                let func = if matches!(instr, Instr::LDiv) { "jrt_ldiv" } else { "jrt_lrem" };
                let b = pop!();
                let a = pop!();
                let l = ml.stack_slot(stack.len(), Ty::I64);
                stmts.push(Statement::Call {
                    dest: Some(l),
                    func: func.to_string(),
                    args: vec![Operand::Copy(a), Operand::Copy(b)],
                });
                stack.push(Ty::I64);
                throw_after = Some(*pc);
            }
            Instr::LCmp | Instr::DCmpL | Instr::DCmpG | Instr::FCmpL | Instr::FCmpG => {
                let func = match instr {
                    Instr::LCmp => "jrt_lcmp",
                    Instr::DCmpL => "jrt_dcmpl",
                    Instr::DCmpG => "jrt_dcmpg",
                    Instr::FCmpL => "jrt_fcmpl",
                    _ => "jrt_fcmpg",
                };
                let b = pop!();
                let a = pop!();
                let l = ml.stack_slot(stack.len(), Ty::I32);
                stmts.push(Statement::Call {
                    dest: Some(l),
                    func: func.to_string(),
                    args: vec![Operand::Copy(a), Operand::Copy(b)],
                });
                stack.push(Ty::I32);
            }
            Instr::I2L => {
                let v = pop!();
                push!(Ty::I64, Rvalue::Convert(Operand::Copy(v)));
            }
            Instr::I2D => {
                let v = pop!();
                push!(Ty::F64, Rvalue::Convert(Operand::Copy(v)));
            }
            Instr::L2I => {
                let v = pop!();
                push!(Ty::I32, Rvalue::Convert(Operand::Copy(v)));
            }
            Instr::L2D => {
                let v = pop!();
                push!(Ty::F64, Rvalue::Convert(Operand::Copy(v)));
            }
            Instr::I2F => {
                let v = pop!();
                push!(Ty::F32, Rvalue::Convert(Operand::Copy(v)));
            }
            Instr::L2F => {
                let v = pop!();
                push!(Ty::F32, Rvalue::Convert(Operand::Copy(v)));
            }
            Instr::F2D => {
                let v = pop!();
                push!(Ty::F64, Rvalue::Convert(Operand::Copy(v)));
            }
            Instr::D2F => {
                let v = pop!();
                push!(Ty::F32, Rvalue::Convert(Operand::Copy(v)));
            }
            // d2i/d2l/f2i/f2l saturieren (Java-Semantik) → Runtime.
            Instr::D2I | Instr::D2L | Instr::F2I | Instr::F2L => {
                let (func, ty) = match instr {
                    Instr::D2I => ("jrt_d2i", Ty::I32),
                    Instr::D2L => ("jrt_d2l", Ty::I64),
                    Instr::F2I => ("jrt_f2i", Ty::I32),
                    _ => ("jrt_f2l", Ty::I64),
                };
                let v = pop!();
                let l = ml.stack_slot(stack.len(), ty);
                stmts.push(Statement::Call {
                    dest: Some(l),
                    func: func.to_string(),
                    args: vec![Operand::Copy(v)],
                });
                stack.push(ty);
            }
            Instr::IAdd | Instr::ISub | Instr::IMul
            | Instr::IShl | Instr::IShr | Instr::IUShr | Instr::IAnd | Instr::IOr | Instr::IXor => {
                let op = match instr {
                    Instr::IAdd => BinOp::Add,
                    Instr::ISub => BinOp::Sub,
                    Instr::IMul => BinOp::Mul,
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
            // Division/Rest werfen ArithmeticException → werfender Runtime-Call.
            Instr::IDiv | Instr::IRem => {
                let func = if matches!(instr, Instr::IDiv) { "jrt_idiv" } else { "jrt_irem" };
                let b = pop!();
                let a = pop!();
                let l = ml.stack_slot(stack.len(), Ty::I32);
                stmts.push(Statement::Call {
                    dest: Some(l),
                    func: func.to_string(),
                    args: vec![Operand::Copy(a), Operand::Copy(b)],
                });
                stack.push(Ty::I32);
                throw_after = Some(*pc);
            }
            Instr::Pop => {
                pop!();
            }
            // monitorenter/monitorexit → Runtime-Sperre (rekursiver globaler
            // Mutex unter --threads, sonst No-Op). objectref ist geborgt.
            Instr::MonitorEnter => {
                let obj = pop!();
                stmts.push(Statement::Call {
                    dest: None,
                    func: "jrt_monitor_enter".to_string(),
                    args: vec![Operand::Copy(obj)],
                });
            }
            Instr::MonitorExit => {
                let obj = pop!();
                stmts.push(Statement::Call {
                    dest: None,
                    func: "jrt_monitor_exit".to_string(),
                    args: vec![Operand::Copy(obj)],
                });
            }
            Instr::Pop2 => {
                // Kategorie-2 (long/double) belegt einen Stack-Eintrag; zwei
                // Kategorie-1-Werte zwei.
                let top = *stack.last().ok_or_else(|| {
                    FrontendError::Unsupported("pop2 auf leerem Stack".into())
                })?;
                pop!();
                if top != Ty::I64 && top != Ty::F64 {
                    pop!();
                }
            }
            Instr::Dup => {
                let ty = *stack.last().ok_or_else(|| {
                    FrontendError::Unsupported("dup auf leerem Stack".into())
                })?;
                let src = ml.stack_slot(stack.len() - 1, ty);
                push!(ty, Rvalue::Use(Operand::Copy(src)));
            }
            Instr::Dup2 => {
                let top = *stack.last().ok_or_else(|| {
                    FrontendError::Unsupported("dup2 auf leerem Stack".into())
                })?;
                if top == Ty::I64 || top == Ty::F64 {
                    let src = ml.stack_slot(stack.len() - 1, top);
                    push!(top, Rvalue::Use(Operand::Copy(src)));
                } else {
                    let t_lo = stack[stack.len() - 2];
                    let t_hi = stack[stack.len() - 1];
                    let s_lo = ml.stack_slot(stack.len() - 2, t_lo);
                    let s_hi = ml.stack_slot(stack.len() - 1, t_hi);
                    push!(t_lo, Rvalue::Use(Operand::Copy(s_lo)));
                    push!(t_hi, Rvalue::Use(Operand::Copy(s_hi)));
                }
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
            Instr::Switch(default, cases) => {
                let value = pop!();
                let default_blk = block_of_pc[default];
                succs.push((default_blk, stack.clone()));
                let case_blks: Vec<(i32, Block)> = cases
                    .iter()
                    .map(|(k, t)| {
                        let b = block_of_pc[t];
                        succs.push((b, stack.clone()));
                        (*k, b)
                    })
                    .collect();
                terminator = Some(Terminator::Switch {
                    value: Operand::Copy(value),
                    default: default_blk,
                    cases: case_blks,
                });
            }
            Instr::Return => terminator = Some(Terminator::Return(None)),
            Instr::IReturn | Instr::AReturn | Instr::LReturn | Instr::DReturn | Instr::FReturn => {
                let v = pop!();
                terminator = Some(Terminator::Return(Some(Operand::Copy(v))));
            }
            Instr::AThrow => {
                let obj = pop!();
                stmts.push(Statement::Call {
                    dest: None,
                    func: "jrt_throw".to_string(),
                    args: vec![Operand::Copy(obj)],
                });
                let target = exc_target_of_pc[pc];
                succs.push((target, stack.clone()));
                terminator = Some(Terminator::Goto(target));
            }
            Instr::AConstNull => {
                push!(Ty::Ref, Rvalue::Use(Operand::ConstNull));
            }
            Instr::NewArrayPrim(_) | Instr::NewArrayRef(_) => {
                let elem = match instr {
                    Instr::NewArrayPrim(t) => arrty_ty(*t),
                    _ => Ty::Ref,
                };
                let len = pop!();
                let l = ml.stack_slot(stack.len(), Ty::Ref);
                stmts.push(Statement::NewArray { dest: l, elem, len: Operand::Copy(len) });
                stack.push(Ty::Ref);
            }
            Instr::ArrayLength => {
                let arr = pop!();
                let dest = ml.stack_slot(stack.len(), Ty::I32);
                stmts.push(Statement::ArrayLen { dest, arr: Operand::Copy(arr) });
                stack.push(Ty::I32);
                throw_after = Some(*pc); // NPE bei null-Array
            }
            Instr::ArrLoad(t) => {
                let elem = arrty_ty(*t);
                let index = pop!();
                let arr = pop!();
                let l = ml.stack_slot(stack.len(), elem);
                stmts.push(Statement::ArrayLoad {
                    dest: l,
                    arr: Operand::Copy(arr),
                    index: Operand::Copy(index),
                    elem,
                });
                stack.push(elem);
                throw_after = Some(*pc); // NPE / ArrayIndexOutOfBounds
            }
            Instr::ArrStore(t) => {
                let elem = arrty_ty(*t);
                let value = pop!();
                let index = pop!();
                let arr = pop!();
                stmts.push(Statement::ArrayStore {
                    arr: Operand::Copy(arr),
                    index: Operand::Copy(index),
                    value: Operand::Copy(value),
                    elem,
                });
                throw_after = Some(*pc); // NPE / ArrayIndexOutOfBounds
            }
            Instr::New(idx) => {
                let class = ml.cf.class_name(*idx)?.to_string();
                // StringBuilder ist runtime-gestützt: new → jrt_sb_new.
                if class == "java/lang/StringBuilder" {
                    let l = ml.stack_slot(stack.len(), Ty::Ref);
                    stmts.push(Statement::Call { dest: Some(l), func: "jrt_sb_new".to_string(), args: vec![] });
                    stack.push(Ty::Ref);
                    continue;
                }
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
                throw_after = Some(*pc); // NPE bei null-Objekt
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
                throw_after = Some(*pc); // NPE bei null-Objekt
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
                // StringBuilder-Konstruktor (Objekt kam schon von jrt_sb_new):
                // ()V ist ein No-Op, (String) hängt den String an.
                if class == "java/lang/StringBuilder" && name == "<init>" {
                    // args = [receiver, string?] (schon eingesammelt, reversed).
                    if desc == "(Ljava/lang/String;)V" {
                        stmts.push(Statement::Call {
                            dest: None,
                            func: "jrt_sb_init_str".to_string(),
                            args,
                        });
                    }
                    continue;
                }
                if name == "<init>" && program.class(&class).is_none() {
                    // Konstruktor einer nicht modellierten Basisklasse
                    // (Object, Throwable, RuntimeException, …): entfällt.
                    // Argumente wurden bereits gepoppt.
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
                throw_after = Some(*pc);
            }
            Instr::GetStatic(idx) => {
                let (class, name, _) = ml.cf.member_ref(*idx)?;
                if class == "java/lang/System" && (name == "out" || name == "err") {
                    // Receiver-Dummy; das println-Intrinsic ignoriert ihn.
                    push!(Ty::Ref, Rvalue::Use(Operand::ConstNull));
                } else {
                    let (class, field) = (class.to_string(), name.to_string());
                    let (_, ty) = program.resolve_static_field(&class, &field).ok_or_else(|| {
                        FrontendError::Unsupported(format!("getstatic {class}.{field}"))
                    })?;
                    let l = ml.stack_slot(stack.len(), ty);
                    stmts.push(Statement::GetStatic { dest: l, class, field });
                    stack.push(ty);
                }
            }
            Instr::PutStatic(idx) => {
                let (class, field, _) = ml.cf.member_ref(*idx)?;
                let (class, field) = (class.to_string(), field.to_string());
                if program.resolve_static_field(&class, &field).is_none() {
                    return Err(FrontendError::Unsupported(format!("putstatic {class}.{field}")));
                }
                let value = pop!();
                stmts.push(Statement::PutStatic { class, field, value: Operand::Copy(value) });
            }
            Instr::InvokeVirtual(idx) => {
                let (class, name, desc) = ml.cf.member_ref(*idx)?;
                let intrinsic = match (class, name, desc) {
                    ("java/io/PrintStream", "println", "(Ljava/lang/String;)V") => Some("jrt_println_str"),
                    ("java/io/PrintStream", "println", "(I)V") => Some("jrt_println_int"),
                    ("java/io/PrintStream", "println", "()V") => Some("jrt_println_ln"),
                    ("java/io/PrintStream", "print", "(Ljava/lang/String;)V") => Some("jrt_print_str"),
                    ("java/io/PrintStream", "print", "(I)V") => Some("jrt_print_int"),
                    ("java/io/PrintStream", "println", "(C)V") => Some("jrt_println_char"),
                    ("java/io/PrintStream", "print", "(C)V") => Some("jrt_print_char"),
                    ("java/io/PrintStream", "println", "(J)V") => Some("jrt_println_long"),
                    ("java/io/PrintStream", "print", "(J)V") => Some("jrt_print_long"),
                    ("java/io/PrintStream", "println", "(D)V") => Some("jrt_println_double"),
                    ("java/io/PrintStream", "print", "(D)V") => Some("jrt_print_double"),
                    ("java/io/PrintStream", "println", "(F)V") => Some("jrt_println_float"),
                    ("java/io/PrintStream", "print", "(F)V") => Some("jrt_print_float"),
                    ("java/io/PrintStream", "println", "(Z)V") => Some("jrt_println_bool"),
                    ("java/io/PrintStream", "print", "(Z)V") => Some("jrt_print_bool"),
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
                // System.out.printf(fmt, Object[]) → formatieren + ausgeben,
                // gibt den Stream (Dummy) zurück.
                if class == "java/io/PrintStream"
                    && name == "printf"
                    && desc == "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/io/PrintStream;"
                {
                    let array = pop!();
                    let fmt = pop!();
                    pop!(); // Receiver-Dummy
                    let s = ml.fresh(Ty::Ref);
                    stmts.push(Statement::Call {
                        dest: Some(s),
                        func: "jrt_str_format".to_string(),
                        args: vec![Operand::Copy(fmt), Operand::Copy(array)],
                    });
                    stmts.push(Statement::Call {
                        dest: None,
                        func: "jrt_print_str".to_string(),
                        args: vec![Operand::Copy(s)],
                    });
                    push!(Ty::Ref, Rvalue::Use(Operand::ConstNull));
                    continue;
                }
                // print(ln)(Object): virtueller toString, dann als String
                // ausgeben.
                if class == "java/io/PrintStream"
                    && (name == "println" || name == "print")
                    && desc == "(Ljava/lang/Object;)V"
                {
                    let arg = pop!();
                    pop!(); // Receiver-Dummy
                    let s = ml.fresh(Ty::Ref);
                    stmts.push(Statement::CallVirtual {
                        dest: Some(s),
                        class: "java/lang/Object".to_string(),
                        name: "toString".to_string(),
                        desc: "()Ljava/lang/String;".to_string(),
                        params: vec![Ty::Ref],
                        ret: Ty::Ref,
                        args: vec![Operand::Copy(arg)],
                    });
                    let f = if name == "println" { "jrt_println_str" } else { "jrt_print_str" };
                    stmts.push(Statement::Call { dest: None, func: f.to_string(), args: vec![Operand::Copy(s)] });
                    continue;
                }
                // StringBuilder-Methoden (runtime-gestützt). append gibt
                // this zurück (Verkettung), toString einen neuen String.
                if class == "java/lang/StringBuilder" {
                    let (func, rty) = match (name, desc) {
                        ("append", "(Ljava/lang/String;)Ljava/lang/StringBuilder;") => ("jrt_sb_append_str", Ty::Ref),
                        ("append", "(I)Ljava/lang/StringBuilder;") => ("jrt_sb_append_int", Ty::Ref),
                        ("append", "(C)Ljava/lang/StringBuilder;") => ("jrt_sb_append_char", Ty::Ref),
                        ("append", "(J)Ljava/lang/StringBuilder;") => ("jrt_sb_append_long", Ty::Ref),
                        ("append", "(D)Ljava/lang/StringBuilder;") => ("jrt_sb_append_double", Ty::Ref),
                        ("append", "(Z)Ljava/lang/StringBuilder;") => ("jrt_sb_append_bool", Ty::Ref),
                        ("toString", "()Ljava/lang/String;") => ("jrt_sb_tostring", Ty::Ref),
                        ("length", "()I") => ("jrt_sb_length", Ty::I32),
                        _ => {
                            return Err(FrontendError::Unsupported(format!(
                                "StringBuilder.{name}{desc}"
                            )))
                        }
                    };
                    let (ptys, _) = parse_descriptor(desc)?;
                    let mut args = Vec::new();
                    for _ in &ptys {
                        args.push(Operand::Copy(pop!()));
                    }
                    let recv = pop!();
                    args.push(Operand::Copy(recv));
                    args.reverse();
                    let l = ml.stack_slot(stack.len(), rty);
                    stack.push(rty);
                    stmts.push(Statement::Call { dest: Some(l), func: func.to_string(), args });
                    continue;
                }
                // Unboxing: Wrapper.<prim>Value() → eingepackter Wert.
                let unbox = match (class, name, desc) {
                    ("java/lang/Integer", "intValue", "()I") => Some(("jrt_integer_intvalue", Ty::I32)),
                    ("java/lang/Long", "longValue", "()J") => Some(("jrt_long_longvalue", Ty::I64)),
                    ("java/lang/Boolean", "booleanValue", "()Z") => Some(("jrt_boolean_booleanvalue", Ty::I32)),
                    ("java/lang/Double", "doubleValue", "()D") => Some(("jrt_double_doublevalue", Ty::F64)),
                    ("java/lang/Character", "charValue", "()C") => Some(("jrt_character_charvalue", Ty::I32)),
                    ("java/lang/Float", "floatValue", "()F") => Some(("jrt_float_floatvalue", Ty::F32)),
                    _ => None,
                };
                if let Some((f, rty)) = unbox {
                    let recv = pop!();
                    let l = ml.stack_slot(stack.len(), rty);
                    stmts.push(Statement::Call {
                        dest: Some(l),
                        func: f.to_string(),
                        args: vec![Operand::Copy(recv)],
                    });
                    stack.push(rty);
                    continue;
                }
                // String-Methoden als Runtime-Intrinsics (Receiver ist ein
                // echtes Argument, kein Dummy). UTF-8/Byte-Semantik: charAt
                // liefert das Byte — für ASCII korrekt (Java: UTF-16-Einheit).
                if class == "java/lang/String" {
                    let (func, rty) = match (name, desc) {
                        ("length", "()I") => ("jrt_str_length", Ty::I32),
                        ("charAt", "(I)C") => ("jrt_str_char_at", Ty::I32),
                        ("equals", "(Ljava/lang/Object;)Z") => ("jrt_str_equals", Ty::I32),
                        ("isEmpty", "()Z") => ("jrt_str_is_empty", Ty::I32),
                        ("hashCode", "()I") => ("jrt_str_hashcode", Ty::I32),
                        ("indexOf", "(Ljava/lang/String;)I") => ("jrt_str_indexof", Ty::I32),
                        ("startsWith", "(Ljava/lang/String;)Z") => ("jrt_str_startswith", Ty::I32),
                        ("endsWith", "(Ljava/lang/String;)Z") => ("jrt_str_endswith", Ty::I32),
                        ("compareTo", "(Ljava/lang/String;)I") => ("jrt_str_compareto", Ty::I32),
                        ("substring", "(I)Ljava/lang/String;") => ("jrt_str_substring1", Ty::Ref),
                        ("substring", "(II)Ljava/lang/String;") => ("jrt_str_substring2", Ty::Ref),
                        ("concat", "(Ljava/lang/String;)Ljava/lang/String;") => ("jrt_str_concat", Ty::Ref),
                        ("trim", "()Ljava/lang/String;") => ("jrt_str_trim", Ty::Ref),
                        _ => {
                            return Err(FrontendError::Unsupported(format!(
                                "String.{name}{desc} (Teilmenge: length, charAt, equals, isEmpty, \
                                 hashCode, indexOf, startsWith, endsWith, compareTo, substring, concat, trim)"
                            )))
                        }
                    };
                    let (ptys, _) = parse_descriptor(&desc)?;
                    let mut args = Vec::new();
                    for _ in &ptys {
                        args.push(Operand::Copy(pop!()));
                    }
                    let recv = pop!();
                    args.push(Operand::Copy(recv));
                    args.reverse();
                    let l = ml.stack_slot(stack.len(), rty);
                    stack.push(rty);
                    stmts.push(Statement::Call { dest: Some(l), func: func.to_string(), args });
                    // Receiver-null/OOB werfen NPE/StringIndexOutOfBounds →
                    // abfangbar (equals/compareTo sind null-tolerant genug).
                    if func != "jrt_str_equals" {
                        throw_after = Some(*pc);
                    }
                    continue;
                }
                // Throwable.addSuppressed (von try-with-resources erzeugt):
                // unterdrückte Exceptions sind rein diagnostisch → no-op.
                if name == "addSuppressed" && desc == "(Ljava/lang/Throwable;)V" {
                    pop!(); // suppressed throwable
                    pop!(); // receiver
                    continue;
                }
                // Thread.start()/join(): Runtime übernimmt (pthread bzw.
                // synchroner Lauf ohne --threads). objectref geborgt.
                if class == "java/lang/Thread" && (name == "start" || name == "join") && desc == "()V" {
                    let recv = pop!();
                    let func = if name == "start" { "jrt_thread_start" } else { "jrt_thread_join" };
                    stmts.push(Statement::Call {
                        dest: None,
                        func: func.to_string(),
                        args: vec![Operand::Copy(recv)],
                    });
                    continue;
                }
                // Object.getClass(): Class-Singleton über den Type-Descriptor
                // (Laufzeit-Reflection der Objektidentität).
                if name == "getClass" && desc == "()Ljava/lang/Class;" {
                    let recv = pop!();
                    let l = ml.stack_slot(stack.len(), Ty::Ref);
                    stack.push(Ty::Ref);
                    stmts.push(Statement::Call {
                        dest: Some(l),
                        func: "jrt_get_class".to_string(),
                        args: vec![Operand::Copy(recv)],
                    });
                    continue;
                }
                // Throwable.getMessage(): $message-Feld lesen (Sentinel-sicher
                // über die Runtime, die den Type-Descriptor prüft).
                if name == "getMessage" && desc == "()Ljava/lang/String;" {
                    let recv = pop!();
                    let l = ml.stack_slot(stack.len(), Ty::Ref);
                    stack.push(Ty::Ref);
                    stmts.push(Statement::Call {
                        dest: Some(l),
                        func: "jrt_throwable_message".to_string(),
                        args: vec![Operand::Copy(recv)],
                    });
                    continue;
                }
                // Array-clone() (u.a. von enum values() erzeugt): flache
                // Kopie mit retain der Ref-Elemente in der Runtime.
                if class.starts_with('[') && name == "clone" {
                    let arr = pop!();
                    let (elem_size, is_ref) = match class.as_bytes().get(1) {
                        Some(b'L') | Some(b'[') => (8, 1),
                        Some(b'J') | Some(b'D') => (8, 0),
                        Some(b'I') | Some(b'F') => (4, 0),
                        Some(b'S') | Some(b'C') => (2, 0),
                        Some(b'Z') | Some(b'B') => (1, 0),
                        _ => (8, 0),
                    };
                    let l = ml.stack_slot(stack.len(), Ty::Ref);
                    stack.push(Ty::Ref);
                    stmts.push(Statement::Call {
                        dest: Some(l),
                        func: "jrt_array_clone".to_string(),
                        args: vec![
                            Operand::Copy(arr),
                            Operand::ConstI64(elem_size),
                            Operand::ConstI32(is_ref),
                        ],
                    });
                    continue;
                }
                // Reflection auf einem Class-Objekt.
                if class == "java/lang/Class" {
                    // getName/getSimpleName laufen auf JEDEM Class-Wert (statisch
                    // via ConstClass oder Laufzeit via getClass()) → das @jclass
                    // trägt die Namens-Strings, Zugriff über die Runtime.
                    if desc == "()Ljava/lang/String;"
                        && (name == "getName" || name == "getSimpleName")
                    {
                        let recv = pop!();
                        let func = if name == "getName" {
                            "jrt_class_getname"
                        } else {
                            "jrt_class_getsimplename"
                        };
                        let l = ml.stack_slot(stack.len(), Ty::Ref);
                        stack.push(Ty::Ref);
                        stmts.push(Statement::Call {
                            dest: Some(l),
                            func: func.to_string(),
                            args: vec![Operand::Copy(recv)],
                        });
                        continue;
                    }
                    // newInstance u.ä. brauchen den statisch bekannten Zieltyp.
                    let recv = pop!();
                    let target = match origin_of(&stmts, recv) {
                        Origin::Op(Operand::ConstClass(c)) => c.clone(),
                        // Blockübergreifend über die ConstClass-Verfolgung.
                        _ => match ml.class_const.get(&recv).cloned() {
                            Some(c) => c,
                            None => {
                                return Err(FrontendError::Unsupported(format!(
                                    "Class.{name} auf nicht statisch bekanntem Class-Objekt \
                                     (Closed World: Reflection muss statisch auflösbar sein)"
                                )));
                            }
                        },
                    };
                    match (name, desc) {
                        ("newInstance", "()Ljava/lang/Object;") => {
                            let ctor = program
                                .resolve_method(&target, "<init>", "()V")
                                .map(|(_, mi)| mi.mangled.clone())
                                .ok_or_else(|| {
                                    FrontendError::Unsupported(format!(
                                        "{target}.newInstance(): kein parameterloser Konstruktor"
                                    ))
                                })?;
                            let l = ml.stack_slot(stack.len(), Ty::Ref);
                            stmts.push(Statement::New { dest: l, class: target });
                            stmts.push(Statement::Call {
                                dest: None,
                                func: ctor,
                                args: vec![Operand::Copy(l)],
                            });
                            stack.push(Ty::Ref);
                        }
                        _ => {
                            return Err(FrontendError::Unsupported(format!(
                                "Class.{name}{desc} (Reflection-Teilmenge: forName, getName, newInstance)"
                            )));
                        }
                    }
                    continue;
                }
                let (class, name, desc) = (class.to_string(), name.to_string(), desc.to_string());
                // java/lang/Object-Wurzelmethoden (equals/hashCode/toString)
                // dispatchen global über die Vtable jeder Klasse.
                let is_object_root = class == "java/lang/Object"
                    && matches!(
                        (name.as_str(), desc.as_str()),
                        ("equals", "(Ljava/lang/Object;)Z")
                            | ("hashCode", "()I")
                            | ("toString", "()Ljava/lang/String;")
                    );
                if program.class(&class).is_none() && !is_object_root {
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
                throw_after = Some(*pc);
            }
            Instr::InvokeDynamic(idx) => {
                // Statisch aufgelöst (Closed World, DESIGN.md §1.3):
                // String-Konkatenation und Lambdas (LambdaMetafactory).
                let (dname, ddesc, bsm_name, bsm_args) = ml.cf.invoke_dynamic(*idx)?;

                // --- Lambda (LambdaMetafactory.metafactory) ---
                if bsm_name == "metafactory" || bsm_name == "altMetafactory" {
                    let iface = match parse_descriptor(ddesc)?.1 {
                        Ty::Ref => {
                            // Rückgabetyp des indy = "L…;" → Interface-Name.
                            let d = ddesc.rsplit_once(')').unwrap().1;
                            d.trim_start_matches('L').trim_end_matches(';').to_string()
                        }
                        _ => return Err(FrontendError::Unsupported("Lambda ohne Interface-Rückgabe".into())),
                    };
                    let sam_desc = ml.cf.method_type(bsm_args[0])?.to_string();
                    let (kind, impl_class, impl_name, impl_desc) = ml.cf.method_handle(bsm_args[1])?;
                    let info = LambdaInfo {
                        iface,
                        sam_method: dname.to_string(),
                        sam_desc,
                        kind,
                        impl_class: impl_class.to_string(),
                        impl_name: impl_name.to_string(),
                        impl_desc: impl_desc.to_string(),
                        captures: descriptor_params(ddesc)?
                            .iter()
                            .map(|p| {
                                let mut c = p.chars().peekable();
                                let f = c.next().unwrap();
                                field_ty(f, &mut c, p)
                            })
                            .collect::<Result<Vec<_>>>()?,
                    };
                    let lambda_class = register_lambda(program, &info)?;
                    // Eingefangene Argumente vom Stack holen (in Reihenfolge).
                    let n = info.captures.len();
                    let mut caps = Vec::with_capacity(n);
                    for _ in 0..n {
                        caps.push(pop!());
                    }
                    caps.reverse();
                    // Lambda-Objekt erzeugen und Capture-Felder setzen.
                    let obj = ml.stack_slot(stack.len(), Ty::Ref);
                    stmts.push(Statement::New { dest: obj, class: lambda_class.clone() });
                    for (i, cap) in caps.into_iter().enumerate() {
                        stmts.push(Statement::PutField {
                            obj: Operand::Copy(obj),
                            class: lambda_class.clone(),
                            field: format!("cap{i}"),
                            value: Operand::Copy(cap),
                        });
                    }
                    stack.push(Ty::Ref);
                    continue;
                }

                // --- Records (java/lang/runtime/ObjectMethods.bootstrap) ---
                // toString/hashCode/equals werden feldweise erzeugt. Feldnamen
                // aus bsm_args[1] ("f1;f2"), Typen via resolve_field.
                if bsm_name == "bootstrap"
                    && (dname == "toString" || dname == "hashCode" || dname == "equals")
                {
                    // Empfängertyp = erster Parameter des indy-Deskriptors.
                    let rec_class = descriptor_params(ddesc)?
                        .first()
                        .and_then(|p| p.strip_prefix('L').map(|s| s.trim_end_matches(';').to_string()))
                        .ok_or_else(|| FrontendError::Unsupported("Record-Empfängertyp".into()))?;
                    let names = ml.cf.const_string(bsm_args[1])?;
                    let field_names: Vec<String> = if names.is_empty() {
                        Vec::new()
                    } else {
                        names.split(';').map(str::to_string).collect()
                    };
                    let fields: Vec<(String, Ty)> = field_names
                        .iter()
                        .map(|n| {
                            let ty = program.resolve_field(&rec_class, n).map(|(_, t)| t).unwrap_or(Ty::I32);
                            (n.clone(), ty)
                        })
                        .collect();
                    match dname {
                        "toString" => {
                            let this = pop!();
                            let simple = rec_class.rsplit(['/', '$']).next().unwrap_or(&rec_class);
                            // Teile: "Simple[", "f=", <wert>, ", g=", <wert>, "]"
                            let mut acc = {
                                let sid = program.intern_string(&format!("{simple}["));
                                let l = ml.fresh(Ty::Ref);
                                stmts.push(Statement::Assign(l, Rvalue::Use(Operand::ConstStr(sid))));
                                Operand::Copy(l)
                            };
                            let cat = |ml: &mut MethodLowering, stmts: &mut Vec<Statement>, a: Operand, b: Operand| {
                                let l = ml.fresh(Ty::Ref);
                                stmts.push(Statement::Call { dest: Some(l), func: "jrt_str_concat".into(), args: vec![a, b] });
                                Operand::Copy(l)
                            };
                            for (i, (fname, fty)) in fields.iter().enumerate() {
                                let prefix = if i == 0 { format!("{fname}=") } else { format!(", {fname}=") };
                                let pid = program.intern_string(&prefix);
                                let pl = ml.fresh(Ty::Ref);
                                stmts.push(Statement::Assign(pl, Rvalue::Use(Operand::ConstStr(pid))));
                                acc = cat(ml, &mut stmts, acc, Operand::Copy(pl));
                                // Feldwert → String.
                                let fv = ml.fresh(*fty);
                                stmts.push(Statement::GetField { dest: fv, obj: Operand::Copy(this), class: rec_class.clone(), field: fname.clone() });
                                let vs = record_val_str(ml, &mut stmts, *fty, fv);
                                acc = cat(ml, &mut stmts, acc, vs);
                            }
                            let cl = program.intern_string("]");
                            let cll = ml.fresh(Ty::Ref);
                            stmts.push(Statement::Assign(cll, Rvalue::Use(Operand::ConstStr(cl))));
                            acc = cat(ml, &mut stmts, acc, Operand::Copy(cll));
                            push!(Ty::Ref, Rvalue::Use(acc));
                            continue;
                        }
                        "hashCode" => {
                            let this = pop!();
                            // h = 0; für jedes Feld: h = h*31 + feldhash.
                            let h = ml.fresh(Ty::I32);
                            stmts.push(Statement::Assign(h, Rvalue::Use(Operand::ConstI32(0))));
                            for (fname, fty) in &fields {
                                let fv = ml.fresh(*fty);
                                stmts.push(Statement::GetField { dest: fv, obj: Operand::Copy(this), class: rec_class.clone(), field: fname.clone() });
                                let fh = record_val_hash(ml, &mut stmts, *fty, fv);
                                let h31 = ml.fresh(Ty::I32);
                                stmts.push(Statement::Assign(h31, Rvalue::Binary(BinOp::Mul, Operand::Copy(h), Operand::ConstI32(31))));
                                stmts.push(Statement::Assign(h, Rvalue::Binary(BinOp::Add, Operand::Copy(h31), fh)));
                            }
                            push!(Ty::I32, Rvalue::Use(Operand::Copy(h)));
                            continue;
                        }
                        _ => {
                            // equals(this, other): instanceof + memcmp der Felder.
                            let other = pop!();
                            let this = pop!();
                            let fb: i64 = fields.iter().map(|(_, t)| ty_size(*t)).sum();
                            let inst = ml.fresh(Ty::I32);
                            stmts.push(Statement::InstanceOf { dest: inst, obj: Operand::Copy(other), class: rec_class.clone() });
                            let l = ml.fresh(Ty::I32);
                            stmts.push(Statement::Call {
                                dest: Some(l),
                                func: "jrt_record_memeq".into(),
                                args: vec![Operand::Copy(this), Operand::Copy(other), Operand::Copy(inst), Operand::ConstI64(fb)],
                            });
                            push!(Ty::I32, Rvalue::Use(Operand::Copy(l)));
                            continue;
                        }
                    }
                }

                // --- Pattern-Switch (SwitchBootstraps.typeSwitch) ---
                // Liefert den Index des ersten passenden Typ-Labels (−1 bei null,
                // N bei keinem Treffer); ein nachfolgendes lookupswitch verzweigt.
                // Branch-frei für disjunkte Labels (sealed): idx = Σ k·(o instof
                // Lk) + (1−Σ)·N − (o==null)·(N+1).
                if bsm_name == "typeSwitch" && dname == "typeSwitch" {
                    let labels: Vec<String> = bsm_args
                        .iter()
                        .map(|&i| ml.cf.class_name(i).map(str::to_string))
                        .collect::<std::result::Result<_, _>>()
                        .map_err(|_| FrontendError::Unsupported(
                            "typeSwitch mit nicht-Klassen-Label (guarded/constant pattern)".into(),
                        ))?;
                    let n = labels.len() as i32;
                    let _restart = pop!(); // Restart-Index (0 bei einfachen Mustern)
                    let obj = pop!();
                    let isnull = ml.fresh(Ty::I32);
                    stmts.push(Statement::Assign(isnull, Rvalue::Binary(BinOp::CmpEq, Operand::Copy(obj), Operand::ConstNull)));
                    let matched = ml.fresh(Ty::I32);
                    stmts.push(Statement::Assign(matched, Rvalue::Use(Operand::ConstI32(0))));
                    let idxsum = ml.fresh(Ty::I32);
                    stmts.push(Statement::Assign(idxsum, Rvalue::Use(Operand::ConstI32(0))));
                    for (k, label) in labels.iter().enumerate() {
                        let inst = ml.fresh(Ty::I32);
                        if program.class(label).is_some() {
                            stmts.push(Statement::InstanceOf { dest: inst, obj: Operand::Copy(obj), class: label.clone() });
                        } else {
                            stmts.push(Statement::Assign(inst, Rvalue::Use(Operand::ConstI32(0))));
                        }
                        let nm = ml.fresh(Ty::I32);
                        stmts.push(Statement::Assign(nm, Rvalue::Binary(BinOp::Add, Operand::Copy(matched), Operand::Copy(inst))));
                        stmts.push(Statement::Assign(matched, Rvalue::Use(Operand::Copy(nm))));
                        if k > 0 {
                            let ki = ml.fresh(Ty::I32);
                            stmts.push(Statement::Assign(ki, Rvalue::Binary(BinOp::Mul, Operand::Copy(inst), Operand::ConstI32(k as i32))));
                            let ns = ml.fresh(Ty::I32);
                            stmts.push(Statement::Assign(ns, Rvalue::Binary(BinOp::Add, Operand::Copy(idxsum), Operand::Copy(ki))));
                            stmts.push(Statement::Assign(idxsum, Rvalue::Use(Operand::Copy(ns))));
                        }
                    }
                    // notmatched = 1 - matched
                    let notm = ml.fresh(Ty::I32);
                    stmts.push(Statement::Assign(notm, Rvalue::Binary(BinOp::Sub, Operand::ConstI32(1), Operand::Copy(matched))));
                    let nmN = ml.fresh(Ty::I32);
                    stmts.push(Statement::Assign(nmN, Rvalue::Binary(BinOp::Mul, Operand::Copy(notm), Operand::ConstI32(n))));
                    let r1 = ml.fresh(Ty::I32);
                    stmts.push(Statement::Assign(r1, Rvalue::Binary(BinOp::Add, Operand::Copy(idxsum), Operand::Copy(nmN))));
                    let nullpen = ml.fresh(Ty::I32);
                    stmts.push(Statement::Assign(nullpen, Rvalue::Binary(BinOp::Mul, Operand::Copy(isnull), Operand::ConstI32(n + 1))));
                    let res = ml.fresh(Ty::I32);
                    stmts.push(Statement::Assign(res, Rvalue::Binary(BinOp::Sub, Operand::Copy(r1), Operand::Copy(nullpen))));
                    push!(Ty::I32, Rvalue::Use(Operand::Copy(res)));
                    continue;
                }

                if dname != "makeConcatWithConstants" && dname != "makeConcat" {
                    return Err(FrontendError::Unsupported(format!(
                        "invokedynamic {dname} (unterstützt: String-Konkatenation, Lambda, Record, Pattern-Switch)"
                    )));
                }
                let with_constants = dname == "makeConcatWithConstants";
                let param_descs = descriptor_params(ddesc)?;
                let recipe: String = if with_constants {
                    ml.cf.const_string(bsm_args[0])?.to_string()
                } else {
                    "\u{1}".repeat(param_descs.len())
                };
                // Konstante Bootstrap-Argumente (ab Index 1) vorab als Strings.
                let const_strings: Vec<String> = if with_constants {
                    bsm_args[1..]
                        .iter()
                        .map(|&i| ml.cf.const_string(i).map(str::to_string))
                        .collect::<std::result::Result<_, _>>()?
                } else {
                    Vec::new()
                };

                // Dynamische Argumente vom Stack holen (in umgekehrter
                // Reihenfolge) und zu String-Operanden konvertieren.
                let mut arg_parts: Vec<Operand> = vec![Operand::ConstNull; param_descs.len()];
                for k in (0..param_descs.len()).rev() {
                    let val = pop!();
                    let pd = param_descs[k].as_str();
                    let part = match pd {
                        "Ljava/lang/String;" => Operand::Copy(val),
                        "I" | "S" | "B" => str_conv(ml, &mut stmts, "jrt_int_to_str", val),
                        "C" => str_conv(ml, &mut stmts, "jrt_char_to_str", val),
                        "Z" => str_conv(ml, &mut stmts, "jrt_bool_to_str", val),
                        "J" => str_conv(ml, &mut stmts, "jrt_long_to_str", val),
                        "D" => str_conv(ml, &mut stmts, "jrt_double_to_str", val),
                        "F" => str_conv(ml, &mut stmts, "jrt_float_to_str", val),
                        // Beliebiges Objekt (Wrapper, user-Klasse) → virtueller
                        // toString. (null-Argument → NPE statt "null"; der
                        // StringConcatFactory-Sonderfall ist nicht abgebildet.)
                        _ if pd.starts_with('L') => {
                            let l = ml.fresh(Ty::Ref);
                            stmts.push(Statement::CallVirtual {
                                dest: Some(l),
                                class: "java/lang/Object".to_string(),
                                name: "toString".to_string(),
                                desc: "()Ljava/lang/String;".to_string(),
                                params: vec![Ty::Ref],
                                ret: Ty::Ref,
                                args: vec![Operand::Copy(val)],
                            });
                            Operand::Copy(l)
                        }
                        _ => {
                            return Err(FrontendError::Unsupported(format!(
                                "Konkatenation von Argument-Typ {pd}"
                            )))
                        }
                    };
                    arg_parts[k] = part;
                }

                // Recipe in Teile zerlegen:  = Argument,  =
                // Konstante, sonst Literalzeichen.
                let mut parts: Vec<Operand> = Vec::new();
                let mut lit = String::new();
                let mut ai = 0;
                let mut ci = 0;
                for ch in recipe.chars() {
                    match ch {
                        '\u{1}' => {
                            flush_lit(&mut lit, &mut parts, program);
                            parts.push(arg_parts[ai].clone());
                            ai += 1;
                        }
                        '\u{2}' => {
                            flush_lit(&mut lit, &mut parts, program);
                            let sid = program.intern_string(&const_strings[ci]);
                            parts.push(Operand::ConstStr(sid));
                            ci += 1;
                        }
                        c => lit.push(c),
                    }
                }
                flush_lit(&mut lit, &mut parts, program);

                // Teile mit jrt_str_concat falten.
                let result = if parts.is_empty() {
                    Operand::ConstStr(program.intern_string(""))
                } else {
                    let mut acc = parts[0].clone();
                    for p in &parts[1..] {
                        let l = ml.fresh(Ty::Ref);
                        stmts.push(Statement::Call {
                            dest: Some(l),
                            func: "jrt_str_concat".to_string(),
                            args: vec![acc, p.clone()],
                        });
                        acc = Operand::Copy(l);
                    }
                    acc
                };
                push!(Ty::Ref, Rvalue::Use(result));
            }
            Instr::CheckCast(idx) => {
                // Closed World: der Cast muss statisch beweisbar sein, sonst
                // Build-Fehler. Ein Laufzeit-Typtest käme mit instanceof
                // (Klassen-Metadaten im Header) in einer späteren Stufe.
                let target = ml.cf.class_name(*idx)?.to_string();
                let top_ty = *stack.last().ok_or_else(|| {
                    FrontendError::Unsupported("checkcast auf leerem Stack".into())
                })?;
                let top = ml.stack_slot(stack.len() - 1, top_ty);
                let provable = match origin_of(&stmts, top) {
                    Origin::New(c) => program.is_subclass(c, &target),
                    Origin::Op(Operand::ConstNull) => true,
                    Origin::Op(Operand::ConstStr(_)) => target == "java/lang/String",
                    Origin::Op(Operand::ConstClass(_)) => target == "java/lang/Class",
                    _ => false,
                };
                if provable {
                    // Statisch bewiesen → kein Code.
                } else if program.class(&target).is_some() {
                    // Modellierte Zielklasse → Laufzeit-Check.
                    stmts.push(Statement::CheckCast { obj: Operand::Copy(top), class: target });
                }
                // Nicht modellierte Zielklasse (String, java/lang/*): Cast
                // durchreichen (catch-all-Prinzip wie bei catch-Typen).
            }
            Instr::InvokeInterface(idx) => {
                let (class, name, desc) = ml.cf.member_ref(*idx)?;
                let (class, name, desc) = (class.to_string(), name.to_string(), desc.to_string());
                if program.class(&class).is_none() {
                    return Err(FrontendError::Unsupported(format!(
                        "invokeinterface {class}.{name}{desc} (Interface nicht im Input)"
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
                throw_after = Some(*pc);
            }
            Instr::InstanceOf(idx) => {
                let target = ml.cf.class_name(*idx)?.to_string();
                let obj = pop!();
                let l = ml.stack_slot(stack.len(), Ty::I32);
                if program.class(&target).is_some() {
                    stmts.push(Statement::InstanceOf { dest: l, obj: Operand::Copy(obj), class: target });
                } else {
                    // Nicht modellierte Zielklasse → konservativ false.
                    stmts.push(Statement::Assign(l, Rvalue::Use(Operand::ConstI32(0))));
                }
                stack.push(Ty::I32);
            }
            Instr::InvokeStatic(idx) => {
                let (class, name, desc) = ml.cf.member_ref(*idx)?;
                // Reflection: Class.forName mit konstantem Argument wird zur
                // Compile-Zeit aufgelöst — statisch bekanntes "dynamisches"
                // Klassenladen im Sinne von DESIGN.md §1.3.
                if class == "java/lang/Class" && name == "forName" {
                    if desc != "(Ljava/lang/String;)Ljava/lang/Class;" {
                        return Err(FrontendError::Unsupported(format!("Class.forName{desc}")));
                    }
                    let arg = pop!();
                    let sid = match origin_of(&stmts, arg) {
                        Origin::Op(Operand::ConstStr(s)) => *s,
                        _ => {
                            return Err(FrontendError::Unsupported(
                                "Class.forName mit nicht-konstantem Argument (Closed World: \
                                 Reflection muss statisch auflösbar sein)"
                                    .into(),
                            ))
                        }
                    };
                    let dotted = program.strings[sid as usize].clone();
                    let target = dotted.replace('.', "/");
                    if program.class(&target).is_none() {
                        return Err(FrontendError::Unsupported(format!(
                            "Class.forName(\"{dotted}\"): Klasse nicht im Closed-World-Input"
                        )));
                    }
                    program.intern_class_object(&target);
                    let l = push!(Ty::Ref, Rvalue::Use(Operand::ConstClass(target.clone())));
                    ml.class_const.insert(l, target);
                    continue;
                }
                // Enum.valueOf(Class, name): über die values() des statisch
                // bekannten enum iterieren und per $name-Feld vergleichen.
                if class == "java/lang/Enum"
                    && name == "valueOf"
                    && desc == "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;"
                {
                    let name_arg = pop!();
                    let cls_arg = pop!();
                    let target = match origin_of(&stmts, cls_arg) {
                        Origin::Op(Operand::ConstClass(c)) => c.clone(),
                        _ => match ml.class_const.get(&cls_arg).cloned() {
                            Some(c) => c,
                            None => return Err(FrontendError::Unsupported(
                                "Enum.valueOf mit nicht statisch bekanntem Class-Objekt".into(),
                            )),
                        },
                    };
                    let values = program
                        .resolve_method(&target, "values", &format!("()[L{target};"))
                        .map(|(_, mi)| mi.mangled.clone())
                        .ok_or_else(|| FrontendError::Unsupported(format!("{target}.values()")))?;
                    let arr = ml.fresh(Ty::Ref);
                    stmts.push(Statement::Call { dest: Some(arr), func: values, args: vec![] });
                    let l = ml.stack_slot(stack.len(), Ty::Ref);
                    stack.push(Ty::Ref);
                    stmts.push(Statement::Call {
                        dest: Some(l),
                        func: "jrt_enum_valueof".to_string(),
                        args: vec![Operand::Copy(arr), Operand::Copy(name_arg)],
                    });
                    continue;
                }
                // String.format(fmt, Object[]) → Runtime-Formatter.
                if class == "java/lang/String"
                    && name == "format"
                    && desc == "(Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/String;"
                {
                    let array = pop!();
                    let fmt = pop!();
                    let l = ml.stack_slot(stack.len(), Ty::Ref);
                    stmts.push(Statement::Call {
                        dest: Some(l),
                        func: "jrt_str_format".to_string(),
                        args: vec![Operand::Copy(fmt), Operand::Copy(array)],
                    });
                    stack.push(Ty::Ref);
                    continue;
                }
                // Autoboxing: Wrapper.valueOf(primitive) → Runtime-Box.
                let box_fn = match (class, name, desc) {
                    ("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;") => Some("jrt_integer_valueof"),
                    ("java/lang/Long", "valueOf", "(J)Ljava/lang/Long;") => Some("jrt_long_valueof"),
                    ("java/lang/Boolean", "valueOf", "(Z)Ljava/lang/Boolean;") => Some("jrt_boolean_valueof"),
                    ("java/lang/Double", "valueOf", "(D)Ljava/lang/Double;") => Some("jrt_double_valueof"),
                    ("java/lang/Character", "valueOf", "(C)Ljava/lang/Character;") => Some("jrt_character_valueof"),
                    ("java/lang/Float", "valueOf", "(F)Ljava/lang/Float;") => Some("jrt_float_valueof"),
                    _ => None,
                };
                if let Some(f) = box_fn {
                    let arg = pop!();
                    let l = ml.stack_slot(stack.len(), Ty::Ref);
                    stmts.push(Statement::Call {
                        dest: Some(l),
                        func: f.to_string(),
                        args: vec![Operand::Copy(arg)],
                    });
                    stack.push(Ty::Ref);
                    continue;
                }
                // String.valueOf(x): primitive → *_to_str, Objekt → toString.
                if class == "java/lang/String" && name == "valueOf" {
                    let arg = pop!();
                    let part = match desc {
                        "(I)Ljava/lang/String;" | "(S)Ljava/lang/String;" | "(B)Ljava/lang/String;" => {
                            str_conv(ml, &mut stmts, "jrt_int_to_str", arg)
                        }
                        "(C)Ljava/lang/String;" => str_conv(ml, &mut stmts, "jrt_char_to_str", arg),
                        "(Z)Ljava/lang/String;" => str_conv(ml, &mut stmts, "jrt_bool_to_str", arg),
                        "(J)Ljava/lang/String;" => str_conv(ml, &mut stmts, "jrt_long_to_str", arg),
                        "(D)Ljava/lang/String;" => str_conv(ml, &mut stmts, "jrt_double_to_str", arg),
                        "(F)Ljava/lang/String;" => str_conv(ml, &mut stmts, "jrt_float_to_str", arg),
                        "(Ljava/lang/Object;)Ljava/lang/String;"
                        | "(Ljava/lang/String;)Ljava/lang/String;" => {
                            let l = ml.fresh(Ty::Ref);
                            stmts.push(Statement::CallVirtual {
                                dest: Some(l),
                                class: "java/lang/Object".to_string(),
                                name: "toString".to_string(),
                                desc: "()Ljava/lang/String;".to_string(),
                                params: vec![Ty::Ref],
                                ret: Ty::Ref,
                                args: vec![Operand::Copy(arg)],
                            });
                            Operand::Copy(l)
                        }
                        _ => return Err(FrontendError::Unsupported(format!("String.valueOf{desc}"))),
                    };
                    push!(Ty::Ref, Rvalue::Use(part));
                    continue;
                }
                // Objects.requireNonNull(x[, msg]) → x (NPE bei null). javac
                // fügt es u.a. beim Zugriff auf die äußere Instanz innerer
                // Klassen ein. Der Message-Overload verwirft das zweite Argument.
                if class == "java/util/Objects" && name == "requireNonNull" {
                    if desc == "(Ljava/lang/Object;Ljava/lang/String;)Ljava/lang/Object;" {
                        pop!(); // Message
                    }
                    let obj = pop!();
                    stmts.push(Statement::Call {
                        dest: None,
                        func: "jrt_null_check".to_string(),
                        args: vec![Operand::Copy(obj)],
                    });
                    push!(Ty::Ref, Rvalue::Use(Operand::Copy(obj)));
                    continue;
                }
                // System.arraycopy: flache Kopie über die Runtime (bei
                // NPE/Bounds/Store-Mismatch bricht sie ab — nicht abfangbar).
                if class == "java/lang/System"
                    && name == "arraycopy"
                    && desc == "(Ljava/lang/Object;ILjava/lang/Object;II)V"
                {
                    let len = pop!();
                    let dstpos = pop!();
                    let dst = pop!();
                    let srcpos = pop!();
                    let src = pop!();
                    stmts.push(Statement::Call {
                        dest: None,
                        func: "jrt_arraycopy".to_string(),
                        args: vec![
                            Operand::Copy(src),
                            Operand::Copy(srcpos),
                            Operand::Copy(dst),
                            Operand::Copy(dstpos),
                            Operand::Copy(len),
                        ],
                    });
                    continue;
                }
                // Wertliefernde Runtime-Intrinsics (parse/Math/Zeit). clang -O2
                // inlinet sie (gemeinsame Übersetzungseinheit mit runtime.c).
                let simple: Option<(&str, Ty)> = match (class, name, desc) {
                    ("java/lang/Integer", "parseInt", "(Ljava/lang/String;)I") => Some(("jrt_parse_int", Ty::I32)),
                    ("java/lang/Long", "parseLong", "(Ljava/lang/String;)J") => Some(("jrt_parse_long", Ty::I64)),
                    ("java/lang/Math", "abs", "(I)I") => Some(("jrt_math_abs_i", Ty::I32)),
                    ("java/lang/Math", "abs", "(J)J") => Some(("jrt_math_abs_l", Ty::I64)),
                    ("java/lang/Math", "abs", "(D)D") => Some(("jrt_math_abs_d", Ty::F64)),
                    ("java/lang/Math", "abs", "(F)F") => Some(("jrt_math_abs_f", Ty::F32)),
                    ("java/lang/Math", "max", "(II)I") => Some(("jrt_math_max_i", Ty::I32)),
                    ("java/lang/Math", "min", "(II)I") => Some(("jrt_math_min_i", Ty::I32)),
                    ("java/lang/Math", "max", "(JJ)J") => Some(("jrt_math_max_l", Ty::I64)),
                    ("java/lang/Math", "min", "(JJ)J") => Some(("jrt_math_min_l", Ty::I64)),
                    ("java/lang/Math", "max", "(DD)D") => Some(("jrt_math_max_d", Ty::F64)),
                    ("java/lang/Math", "min", "(DD)D") => Some(("jrt_math_min_d", Ty::F64)),
                    ("java/lang/Math", "sqrt", "(D)D") => Some(("jrt_math_sqrt", Ty::F64)),
                    ("java/lang/System", "currentTimeMillis", "()J") => Some(("jrt_current_time_millis", Ty::I64)),
                    ("java/lang/System", "nanoTime", "()J") => Some(("jrt_nano_time", Ty::I64)),
                    _ => None,
                };
                if let Some((func, rty)) = simple {
                    let (ptys, _) = parse_descriptor(desc)?;
                    let mut args = Vec::new();
                    for _ in &ptys {
                        args.push(Operand::Copy(pop!()));
                    }
                    args.reverse();
                    let dest = push!(rty, Rvalue::Use(Operand::ConstI32(0)));
                    stmts.pop(); // Platzhalter
                    stmts.push(Statement::Call { dest: Some(dest), func: func.to_string(), args });
                    continue;
                }
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
                throw_after = Some(*pc);
            }
        }
    }

    // Werfender Aufruf am Blockende: pending prüfen → Handler/Propagation
    // oder normal weiter.
    if terminator.is_none() {
        if let Some(throw_pc) = throw_after {
            let target = exc_target_of_pc[&throw_pc];
            let cont = fallthrough.ok_or_else(|| {
                FrontendError::Unsupported(format!("werfender Aufruf ohne Folgeblock bei pc={throw_pc}"))
            })?;
            let c = ml.fresh(Ty::I32);
            stmts.push(Statement::Call {
                dest: Some(c),
                func: "jrt_pending_set".to_string(),
                args: Vec::new(),
            });
            succs.push((target, stack.clone()));
            succs.push((cont, stack.clone()));
            terminator = Some(Terminator::Branch {
                cond: Operand::Copy(c),
                then_blk: target,
                else_blk: cont,
            });
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
