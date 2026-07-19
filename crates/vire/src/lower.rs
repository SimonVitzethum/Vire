//! Lowering Vire AST → `crates/ir` (SSA-like, no slot reuse).
//! Covers the M2 core: functions, arithmetic, control flow (if/while/
//! for-over-Range), `print`, calls to own functions. Generics/traits/
//! closures/capsule follow (FRONTEND-PLAN F5–F8).

use std::collections::HashMap;

use fastllvm_ir::{ArrKind, BasicBlock, BinOp as IB, Block, Function, Local, Operand, Program, Rvalue, Statement, Terminator, Ty};

use crate::ast::*;

/// Field layout of a user type: (field name, IR type, ref target class).
type Layout = Vec<(String, Ty, Option<String>)>;

/// Variant of a sum type: (sum type name, tag, fields as (flattened name, type, ref class)).
type VariantInfo = (String, i64, Vec<(String, Ty, Option<String>)>);

/// Info about a generic function for monomorphization at call sites.
#[derive(Clone)]
struct GInfo {
    /// Type parameter names, e.g. `["T"]` for `fn f[T](…)`.
    tparams: Vec<String>,
    /// Per generic parameter: is it a comptime VALUE param (`[comptime N: Int]`)?
    /// Parallel to `tparams`. Value params bind to a literal, not a type.
    comptime: Vec<bool>,
    /// Parameter type annotations (with T placeholders), for binding the type arguments.
    param_tys: Vec<Option<Type>>,
    /// Return annotation (with T).
    ret: Option<Type>,
}

/// Symbol name of a monomorph. instance: `f$Int$Point`.
fn mono_sym(name: &str, targs: &[String]) -> String {
    format!("{name}${}", targs.join("$"))
}

/// Concrete type name of an argument (for type argument binding).
fn concrete_tyname(ty: Ty, class: Option<&String>) -> String {
    match ty {
        Ty::F64 => "Float".into(),
        Ty::F32 => "F32".into(),
        Ty::I32 => "I32".into(),
        Ty::Ref => class.cloned().unwrap_or_else(|| "Str".into()),
        _ => "Int".into(),
    }
}

/// Replaces type parameter names in a `Type` with concrete types.
fn subst_type(t: &Type, bind: &HashMap<String, String>) -> Type {
    let name = bind.get(&t.name).cloned().unwrap_or_else(|| t.name.clone());
    Type { name, args: t.args.iter().map(|a| subst_type(a, bind)).collect(), borrowed: t.borrowed, span: t.span }
}

/// Clones a generic FnDef and substitutes the type parameters in the signature +
/// body annotations (Let/Cast). The rest of the body goes through inference.
fn subst_fndef(f: &FnDef, bind: &HashMap<String, String>) -> FnDef {
    let mut nf = f.clone();
    nf.sig.generics = vec![]; // instance is no longer generic
    for p in &mut nf.sig.params {
        if let Some(t) = &p.ty {
            p.ty = Some(subst_type(t, bind));
        }
    }
    if let Some(t) = &nf.sig.ret {
        nf.sig.ret = Some(subst_type(t, bind));
    }
    if let Some(b) = &mut nf.body {
        subst_block(b, bind);
    }
    nf
}

fn subst_block(b: &mut crate::ast::Block, bind: &HashMap<String, String>) {
    for s in &mut b.stmts {
        subst_stmt(s, bind);
    }
    // Tail expressions rarely contain type annotations; casts within them via subst_expr.
    if let Some(t) = &mut b.tail {
        subst_expr(t, bind);
    }
}

fn subst_stmt(s: &mut Stmt, bind: &HashMap<String, String>) {
    match s {
        Stmt::Let { value: Some(v), .. } => subst_expr(v, bind),
        Stmt::Expr(e) | Stmt::Return(Some(e), _) => subst_expr(e, bind),
        Stmt::Assign { target, value, .. } => {
            subst_expr(target, bind);
            subst_expr(value, bind);
        }
        Stmt::While { cond, body, .. } => {
            subst_expr(cond, bind);
            subst_block(body, bind);
        }
        Stmt::For { iter, body, .. } => {
            subst_expr(iter, bind);
            subst_block(body, bind);
        }
        _ => {}
    }
}

fn subst_expr(e: &mut Expr, bind: &HashMap<String, String>) {
    match e {
        // A comptime value parameter (`N`) bound to a literal → inline the literal.
        // Value bindings are numeric strings; type bindings (e.g. "Int") never
        // appear in value position, so this only fires for value generics.
        Expr::Ident(n, sp) => {
            if let Some(v) = bind.get(n).and_then(|s| s.parse::<i128>().ok()) {
                *e = Expr::Int(v, *sp);
            }
        }
        Expr::Cast { ty, inner, .. } => {
            *ty = subst_type(ty, bind);
            subst_expr(inner, bind);
        }
        Expr::Binary { lhs, rhs, .. } => {
            subst_expr(lhs, bind);
            subst_expr(rhs, bind);
        }
        Expr::Unary { rhs, .. } => subst_expr(rhs, bind),
        Expr::Call { callee, args, .. } => {
            subst_expr(callee, bind);
            for a in args {
                subst_expr(a, bind);
            }
        }
        Expr::TurboCall { targs, args, .. } => {
            for t in targs {
                subst_expr(t, bind);
            }
            for a in args {
                subst_expr(a, bind);
            }
        }
        Expr::Index { base, index, .. } => {
            subst_expr(base, bind);
            subst_expr(index, bind);
        }
        Expr::Range { start, end, .. } => {
            subst_expr(start, bind);
            subst_expr(end, bind);
        }
        Expr::If { cond, then, elifs, els, .. } => {
            subst_expr(cond, bind);
            subst_block(then, bind);
            for (c, b) in elifs {
                subst_expr(c, bind);
                subst_block(b, bind);
            }
            if let Some(b) = els {
                subst_block(b, bind);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            subst_expr(scrutinee, bind);
            for (_, g, b) in arms {
                if let Some(g) = g {
                    subst_expr(g, bind);
                }
                subst_expr(b, bind);
            }
        }
        Expr::List(xs, _) => xs.iter_mut().for_each(|x| subst_expr(x, bind)),
        Expr::MapLit(kvs, _) => {
            for (k, v) in kvs {
                subst_expr(k, bind);
                subst_expr(v, bind);
            }
        }
        Expr::Block(b) => subst_block(b, bind),
        Expr::Field { base, .. } | Expr::Try { inner: base, .. } | Expr::Comptime { inner: base, .. } => subst_expr(base, bind),
        _ => {}
    }
}

/// Built-in FFI/Python bridge signatures (Ptr = i64). Always available, so that
/// Python is usable from pure Vire without an `extern` block.
fn builtin_ffi_sigs() -> Vec<(&'static str, Vec<Ty>, Ty)> {
    use Ty::*;
    vec![
        ("py_import", vec![I64], I64),
        ("py_getattr", vec![I64, I64], I64),
        ("py_call_f", vec![I64, F64], I64),
        ("py_call_ff", vec![I64, F64, F64], I64),
        ("py_call_i", vec![I64, I64], I64),
        ("py_call_s", vec![I64, I64], I64),
        ("py_float", vec![F64], I64),
        ("py_int", vec![I64], I64),
        ("py_str", vec![I64], I64),
        ("py_asfloat", vec![I64], F64),
        ("py_asint", vec![I64], I64),
        ("py_getitem_i", vec![I64, I64], I64),
        ("vire_py_eval_f", vec![Ref, F64], F64),
        ("vire_py_eval_i", vec![Ref, I64], I64),
    ]
}

/// All methods (type-inline + `impl` blocks) as (class name, method).
fn collect_methods(m: &Module) -> Vec<(String, &FnDef)> {
    let mut out = Vec::new();
    for it in &m.items {
        match it {
            Item::Type(t) => out.extend(t.methods.iter().map(|meth| (t.name.clone(), meth))),
            Item::Impl(im) => out.extend(im.methods.iter().map(|meth| (im.for_type.name.clone(), meth))),
            _ => {}
        }
    }
    out
}

/// Call signature of a function: parameter types, return type, return class
/// (for object returns the class name — for field access on the result).
struct Sig {
    params: Vec<Ty>,
    ret: Ty,
    ret_class: Option<String>,
}

pub fn lower_module(m: &Module) -> Result<Program, Vec<String>> {
    lower_module_src(m, "")
}

/// Byte offset → 1-based source line (for debug info); 0 if unknown.
fn line_of(line_starts: &[usize], byte: usize) -> u32 {
    if line_starts.is_empty() {
        return 0;
    }
    line_starts.partition_point(|&s| s <= byte) as u32
}

pub fn lower_module_src(m: &Module, src: &str) -> Result<Program, Vec<String>> {
    let line_starts: Vec<usize> = if src.is_empty() {
        Vec::new()
    } else {
        std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect()
    };
    let ls = &line_starts[..];
    let mut prog = Program::default();
    let mut errs = Vec::new();

    // Product types → classes. Sum types → ONE tagged class: field `__tag`
    // (I64) + all variant fields flattened (`Variant_field`). Match dispatches
    // via `__tag`. (Space = sum of all variants; simple, fits the flat
    // class model. A more compact union follows later.)
    let mut types: HashMap<String, Layout> = HashMap::new();
    let mut variants: HashMap<String, VariantInfo> = HashMap::new();
    // Generic product types (`type Box[T] { value: T }`): do NOT register
    // directly as a class (fields reference T). Instead monomorphized per used
    // type argument combination (`Box$Float`) — like generic
    // functions. Name → (type parameters, fields).
    let mut generic_ptypes: HashMap<String, (Vec<String>, Vec<Field>)> = HashMap::new();
    // Generic sum types (`type Maybe[T] { Some2(T) | Nothing }` + the built-in
    // Option[T]): monomorphized per type argument (`Option$Float`), so that Float
    // payloads are type-correct (no i64 erasure). Name → (type parameter, [(variant,
    // [(field name, type name)])]). Single-parameter; Result stays i64-erased.
    let mut generic_stypes: HashMap<String, (Vec<String>, Vec<(String, Vec<(String, String)>)>)> = HashMap::new();
    let mut variant_owner_g: HashMap<String, String> = HashMap::new();
    for it in &m.items {
        if let Item::Type(t) = it {
            if !t.generics.is_empty() && t.variants.is_empty() {
                generic_ptypes.insert(
                    t.name.clone(),
                    (t.generics.iter().map(|g| g.name.clone()).collect(), t.fields.clone()),
                );
                continue;
            }
            if !t.generics.is_empty() {
                // Generic sum type.
                let tparams: Vec<String> = t.generics.iter().map(|g| g.name.clone()).collect();
                let variants_g: Vec<(String, Vec<(String, String)>)> = t
                    .variants
                    .iter()
                    .map(|v| {
                        let vf = v
                            .fields
                            .iter()
                            .enumerate()
                            .map(|(i, f)| {
                                let fname = if f.name.is_empty() { format!("{}_{i}", v.name) } else { format!("{}_{}", v.name, f.name) };
                                (fname, f.ty.name.clone())
                            })
                            .collect();
                        variant_owner_g.insert(v.name.clone(), t.name.clone());
                        (v.name.clone(), vf)
                    })
                    .collect();
                generic_stypes.insert(t.name.clone(), (tparams, variants_g));
                continue;
            }
            if t.variants.is_empty() {
                let layout: Layout = t
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), ty_of(Some(&f.ty)), class_of(Some(&f.ty))))
                    .collect();
                types.insert(t.name.clone(), layout);
            } else {
                let mut layout: Layout = vec![("__tag".into(), Ty::I64, None)];
                for (tag, v) in t.variants.iter().enumerate() {
                    let vfields: Vec<(String, Ty, Option<String>)> = v
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(i, f)| {
                            let fname = if f.name.is_empty() { format!("{}_{i}", v.name) } else { format!("{}_{}", v.name, f.name) };
                            (fname, ty_of(Some(&f.ty)), class_of(Some(&f.ty)))
                        })
                        .collect();
                    layout.extend(vfields.iter().cloned());
                    variants.insert(v.name.clone(), (t.name.clone(), tag as i64, vfields));
                }
                types.insert(t.name.clone(), layout);
            }
        }
    }
    // Built-in sum types Option/Result (if not defined by the user).
    // Payload is currently i64-wide (Int/pointer); typed/Float payloads
    // need generic types (next step).
    if !types.contains_key("Option") {
        types.insert("Option".into(), vec![("__tag".into(), Ty::I64, None), ("Some_value".into(), Ty::I64, None)]);
        variants.insert("Some".into(), ("Option".into(), 0, vec![("Some_value".into(), Ty::I64, None)]));
        variants.insert("None".into(), ("Option".into(), 1, vec![]));
    }
    if !types.contains_key("Result") {
        types.insert("Result".into(), vec![("__tag".into(), Ty::I64, None), ("Ok_value".into(), Ty::I64, None), ("Err_error".into(), Ty::I64, None)]);
        variants.insert("Ok".into(), ("Result".into(), 0, vec![("Ok_value".into(), Ty::I64, None)]));
        variants.insert("Err".into(), ("Result".into(), 1, vec![("Err_error".into(), Ty::I64, None)]));
    }
    // Built-in generic Option[T]: `Some(x)` is monomorphized type-correctly
    // (`Option$Float` carries F64), `None` stays over the erased Option (only __tag).
    if !generic_stypes.contains_key("Option") {
        generic_stypes.insert(
            "Option".into(),
            (vec!["T".into()], vec![("Some".into(), vec![("Some_value".into(), "T".into())]), ("None".into(), vec![])]),
        );
        variant_owner_g.entry("Some".into()).or_insert("Option".into());
        variant_owner_g.entry("None".into()).or_insert("Option".into());
    }
    // TRAIT OBJECTS (dynamic dispatch): traits are registered as interfaces,
    // `impl Trait for Typ` adds the methods into the type vtable at
    // consistent global slots (the backend's existing interface/CallVirtual
    // machinery). Trait → [(method, descriptor, param types incl. self, return)].
    let mut trait_methods: HashMap<String, Vec<(String, String, Vec<Ty>, Ty)>> = HashMap::new();
    for it in &m.items {
        if let Item::Trait(tr) = it {
            let ms = tr
                .methods
                .iter()
                .map(|meth| {
                    let params: Vec<Ty> = meth.sig.params.iter().map(|p| if p.name == "self" { Ty::Ref } else { ty_of(p.ty.as_ref()) }).collect();
                    (meth.sig.name.clone(), method_desc(&params), params, guess_ret_ty(meth))
                })
                .collect();
            trait_methods.insert(tr.name.clone(), ms);
        }
    }
    // Type → implemented traits (from `impl Trait for Typ`).
    let mut type_traits: HashMap<String, Vec<String>> = HashMap::new();
    for it in &m.items {
        if let Item::Impl(im) = it {
            if let Some(tn) = &im.trait_name {
                if trait_methods.contains_key(tn) {
                    type_traits.entry(im.for_type.name.clone()).or_default().push(tn.clone());
                }
            }
        }
    }
    // Register ClassInfo per type (user + built-in).
    let mut all_type_names: Vec<String> = m
        .items
        .iter()
        .filter_map(|it| if let Item::Type(t) = it { Some(t.name.clone()) } else { None })
        .filter(|n| types.contains_key(n))
        .collect();
    for bi in ["Option", "Result"] {
        if types.contains_key(bi) && !all_type_names.iter().any(|n| n == bi) {
            all_type_names.push(bi.into());
        }
    }
    for tname in &all_type_names {
        let fields = types[tname]
            .iter()
            .map(|(n, ty, rt)| fastllvm_ir::FieldInfo { name: n.clone(), ty: *ty, ref_target: rt.clone() })
            .collect();
        // Implemented traits → interfaces + the impl methods into the vtable
        // (mangled = `Typ.methode`, as collect_methods lowers them).
        let ifaces = type_traits.get(tname).cloned().unwrap_or_default();
        let mut methods = Vec::new();
        for tn in &ifaces {
            if let Some(tms) = trait_methods.get(tn) {
                for (mn, d, _, _) in tms {
                    methods.push(fastllvm_ir::MethodInfo {
                        name: mn.clone(),
                        desc: d.clone(),
                        is_static: false,
                        has_body: true,
                        mangled: format!("{tname}.{mn}"),
                    });
                }
            }
        }
        prog.classes.push(fastllvm_ir::ClassInfo {
            name: tname.clone(),
            super_name: Some("java/lang/Object".to_string()),
            is_interface: false,
            interfaces: ifaces,
            fields,
            static_fields: vec![],
            methods,
            has_clinit: false,
        });
    }
    // Traits as interface ClassInfos (abstract methods → global vtable slots).
    for (tname, ms) in &trait_methods {
        let methods = ms
            .iter()
            .map(|(mn, d, _, _)| fastllvm_ir::MethodInfo { name: mn.clone(), desc: d.clone(), is_static: false, has_body: false, mangled: String::new() })
            .collect();
        prog.classes.push(fastllvm_ir::ClassInfo {
            name: tname.clone(),
            super_name: Some("java/lang/Object".to_string()),
            is_interface: true,
            interfaces: vec![],
            fields: vec![],
            static_fields: vec![],
            methods,
            has_clinit: false,
        });
    }

    // Signature table (name → (param types, return type, return class)) for calls.
    let mut sigs: HashMap<String, Sig> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            let ps = f.sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
            sigs.insert(f.sig.name.clone(), Sig { params: ps, ret: guess_ret_ty(f), ret_class: class_of_ann(f.sig.ret.as_ref(), &generic_ptypes, &generic_stypes) });
        }
        // extern "C" { fn name(...) -> T }: C-ABI function, directly under its
        // name (no mangling). Calls resolve through this; the backend
        // declares the called-but-undefined function, clang links it
        // (libc/libm/-lstdc++ / linked objects).
        if let Item::Extern { items, .. } = it {
            for sig in items {
                let ps = sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
                sigs.insert(sig.name.clone(), Sig { params: ps, ret: ret_ty(sig), ret_class: class_of(sig.ret.as_ref()) });
            }
        }
    }
    // Built-in Python bridge: always register the signatures, so that `py_import`
    // & co. are callable from pure Vire WITHOUT an `extern` block (the lowering
    // emits calls, the backend declares, the driver links the bridge).
    for (name, params, ret) in builtin_ffi_sigs() {
        sigs.entry(name.to_string()).or_insert(Sig { params, ret, ret_class: None });
    }
    // Methods (type-inline + impl blocks) → symbol `Class.method`, self = Ref.
    let methods = collect_methods(m);
    // Trait/impl coherence: a type must not define the same method name more than
    // once (across inline methods and `impl` blocks). Both would mangle to
    // `Type.method` and one definition would silently shadow the other — reject it
    // instead, so overlapping/duplicate impls are a compile error, not a surprise.
    {
        let mut counts: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
        for (class, meth) in &methods {
            *counts.entry((class.clone(), meth.sig.name.clone())).or_insert(0) += 1;
        }
        let mut dups: Vec<(String, String, usize)> =
            counts.into_iter().filter(|(_, n)| *n > 1).map(|((c, m), n)| (c, m, n)).collect();
        dups.sort();
        for (class, mname, n) in dups {
            errs.push(format!(
                "coherence: method `{mname}` is defined {n} times for type `{class}` (conflicting or overlapping impls)"
            ));
        }
    }
    for (class, meth) in &methods {
        let ps = meth
            .sig
            .params
            .iter()
            .map(|p| if p.name == "self" { Ty::Ref } else { ty_of(p.ty.as_ref()) })
            .collect();
        let sym = format!("{class}.{}", meth.sig.name);
        sigs.insert(sym, Sig { params: ps, ret: guess_ret_ty(meth), ret_class: class_of_ann(meth.sig.ret.as_ref(), &generic_ptypes, &generic_stypes) });
    }
    // Collect generic functions (do NOT lower directly — one monomorph.
    // instance per call type argument). Trait bounds are parsed, but not yet
    // resolved (trait solving/coherence is the open hard half).
    let mut generics: HashMap<String, GInfo> = HashMap::new();
    let mut generic_defs: HashMap<String, FnDef> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if !f.sig.generics.is_empty() {
                generics.insert(
                    f.sig.name.clone(),
                    GInfo {
                        tparams: f.sig.generics.iter().map(|g| g.name.clone()).collect(),
                        comptime: f.sig.generics.iter().map(|g| g.is_comptime).collect(),
                        param_tys: f.sig.params.iter().map(|p| p.ty.clone()).collect(),
                        ret: f.sig.ret.clone(),
                    },
                );
                generic_defs.insert(f.sig.name.clone(), f.clone());
            }
        }
    }
    // Non-generic function ASTs for higher-order inlining.
    let mut fn_defs: HashMap<String, FnDef> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if f.sig.generics.is_empty() && f.body.is_some() {
                fn_defs.insert(f.sig.name.clone(), f.clone());
            }
        }
    }
    // Pre-pass: instantiate annotation-driven generic instances (`-> Option[Float]`,
    // `b: Box[Int]`) module-wide, so that call sites/matches see the
    // concrete instance (layout + variants) — even in functions that only
    // use the type but do not annotate it.
    let mut shared_inst: HashMap<String, Layout> = HashMap::new();
    let mut shared_svars: HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>> = HashMap::new();
    {
        let mut seed = |t: &Type| {
            if t.args.is_empty() {
                return;
            }
            let targs: Vec<String> = t.args.iter().map(|a| a.name.clone()).collect();
            if let Some((tps, vars)) = generic_stypes.get(&t.name) {
                inst_stype(&t.name, tps, vars, &targs, &mut shared_inst, &mut shared_svars);
            } else if let Some((tps, fields)) = generic_ptypes.get(&t.name) {
                inst_ptype(&t.name, tps, fields, &targs, &mut shared_inst);
            }
        };
        for it in &m.items {
            if let Item::Fn(f) = it {
                for p in &f.sig.params {
                    if let Some(t) = &p.ty {
                        seed(t);
                    }
                }
                if let Some(t) = &f.sig.ret {
                    seed(t);
                }
            }
        }
        for (_, meth) in &methods {
            for p in &meth.sig.params {
                if let Some(t) = &p.ty {
                    seed(t);
                }
            }
            if let Some(t) = &meth.sig.ret {
                seed(t);
            }
        }
    }
    let mut mono_queue: Vec<(String, Vec<String>)> = Vec::new();
    // Instantiated generic types (mangled class → layout), collected from all
    // functions and afterwards registered as classes for the backend.
    let mut all_insts: HashMap<String, Layout> = HashMap::new();
    let mut str_index: HashMap<String, u32> = HashMap::new();
    for it in &m.items {
        if let Item::Fn(f) = it {
            if !f.sig.generics.is_empty() {
                continue; // generic → only instantiated on demand
            }
            if is_higher_order(f) {
                continue; // higher-order template → only inline (defunctionalization)
            }
            match lower_fn(f, &sigs, &types, &variants, &generics, &trait_methods, &fn_defs, &generic_ptypes, &generic_stypes, &variant_owner_g, &shared_inst, &shared_svars, &mut prog.strings, &mut str_index, None, None, line_of(ls, f.sig.span.0), ls) {
                Ok((func, mono, insts)) => {
                    prog.functions.push(func);
                    mono_queue.extend(mono);
                    all_insts.extend(insts);
                }
                Err(mut e) => errs.append(&mut e),
            }
        }
        // trait/const/use: skipped here (trait dispatch open)
    }
    for (class, meth) in &methods {
        let sym = format!("{class}.{}", meth.sig.name);
        match lower_fn(meth, &sigs, &types, &variants, &generics, &trait_methods, &fn_defs, &generic_ptypes, &generic_stypes, &variant_owner_g, &shared_inst, &shared_svars, &mut prog.strings, &mut str_index, Some(class), Some(&sym), line_of(ls, meth.sig.span.0), ls) {
            Ok((func, mono, insts)) => {
                prog.functions.push(func);
                mono_queue.extend(mono);
                all_insts.extend(insts);
            }
            Err(mut e) => errs.append(&mut e),
        }
    }
    // Monomorphization worklist: substitute + lower each requested instance
    // (may request further instances), until fixpoint.
    let mut mono_done: std::collections::HashSet<String> = std::collections::HashSet::new();
    while let Some((gname, targs)) = mono_queue.pop() {
        let sym = mono_sym(&gname, &targs);
        if !mono_done.insert(sym.clone()) {
            continue;
        }
        let Some(gdef) = generic_defs.get(&gname) else { continue };
        // Enforce declared trait bounds (`[T: Shape]`): every type argument must
        // implement each bound of its parameter. Checked here, at the concrete
        // instantiation, with a precise message pointing at the boundary — rather
        // than letting it surface downstream as a cryptic "type has no method X".
        // Only user (nominal) types can carry impls, so a primitive/instance name
        // absent from `type_traits` fails a non-empty bound. Skip lowering the
        // ill-typed instance (the build already fails on the error) to avoid a
        // duplicate downstream diagnostic.
        let mut bound_violation = false;
        for (gp, concrete) in gdef.sig.generics.iter().zip(targs.iter()) {
            for bound in &gp.bounds {
                let satisfied = type_traits.get(concrete).is_some_and(|ts| ts.iter().any(|t| t == bound));
                if !satisfied {
                    errs.push(format!(
                        "trait bound not satisfied: `{gname}` requires `{}: {}`, but the type argument `{concrete}` does not implement `{bound}`",
                        gp.name,
                        gp.bounds.join(" + "),
                    ));
                    bound_violation = true;
                }
            }
        }
        if bound_violation {
            continue;
        }
        let bind: HashMap<String, String> = gdef.sig.generics.iter().map(|g| g.name.clone()).zip(targs.iter().cloned()).collect();
        let inst = subst_fndef(gdef, &bind);
        // Register instance signature (for recursion/mutual calls).
        let ps = inst.sig.params.iter().map(|p| ty_of(p.ty.as_ref())).collect();
        sigs.insert(sym.clone(), Sig { params: ps, ret: guess_ret_ty(&inst), ret_class: class_of_ann(inst.sig.ret.as_ref(), &generic_ptypes, &generic_stypes) });
        match lower_fn(&inst, &sigs, &types, &variants, &generics, &trait_methods, &fn_defs, &generic_ptypes, &generic_stypes, &variant_owner_g, &shared_inst, &shared_svars, &mut prog.strings, &mut str_index, None, Some(&sym), line_of(ls, inst.sig.span.0), ls) {
            Ok((func, mono, insts)) => {
                prog.functions.push(func);
                mono_queue.extend(mono);
                all_insts.extend(insts);
            }
            Err(mut e) => errs.append(&mut e),
        }
    }
    // Register instantiated generic types as classes (backend layout).
    // Merge annotation-driven (shared) + payload-driven (all_insts).
    for (k, v) in &shared_inst {
        all_insts.entry(k.clone()).or_insert_with(|| v.clone());
    }
    for (mangled, layout) in &all_insts {
        let fields = layout
            .iter()
            .map(|(n, ty, rt)| fastllvm_ir::FieldInfo { name: n.clone(), ty: *ty, ref_target: rt.clone() })
            .collect();
        prog.classes.push(fastllvm_ir::ClassInfo {
            name: mangled.clone(),
            super_name: Some("java/lang/Object".to_string()),
            is_interface: false,
            interfaces: vec![],
            fields,
            static_fields: vec![],
            methods: vec![],
            has_clinit: false,
        });
    }
    if errs.is_empty() {
        Ok(prog)
    } else {
        Err(errs)
    }
}

fn ty_of(t: Option<&Type>) -> Ty {
    match t.map(|t| t.name.as_str()) {
        Some("Float") | Some("F64") => Ty::F64,
        Some("F32") => Ty::F32,
        Some("Bool") => Ty::I32,
        Some("Str") => Ty::Ref,
        Some("I32") | Some("U32") => Ty::I32,
        Some("Int") | Some("I64") | Some("U64") => Ty::I64,
        // `Ptr` = opaque raw pointer (FFI): i64-wide, NO RC (not a Vire object).
        Some("Ptr") => Ty::I64,
        Some("Unit") | None => Ty::I64, // default integer when nothing is given
        // Everything else is a (user) reference type: object on the heap.
        Some(_) => Ty::Ref,
    }
}

/// Class name of a reference type annotation (for GetField/New), otherwise None.
fn class_of(t: Option<&Type>) -> Option<String> {
    let name = t?.name.as_str();
    match name {
        "Float" | "F64" | "F32" | "Bool" | "Str" | "I32" | "U32" | "Int" | "I64" | "U64" | "Unit" | "Ptr" => None,
        _ => Some(name.to_string()),
    }
}

/// Class of an annotation taking generic types into account: `Option[Float]` →
/// `Option$Float` (the monomorphized instance), otherwise like `class_of`. For
/// return classes in signatures, so that call sites see the concrete instance.
fn class_of_ann(
    t: Option<&Type>,
    gp: &HashMap<String, (Vec<String>, Vec<Field>)>,
    gs: &HashMap<String, (Vec<String>, Vec<(String, Vec<(String, String)>)>)>,
) -> Option<String> {
    let t = t?;
    if !t.args.is_empty() && (gp.contains_key(&t.name) || gs.contains_key(&t.name)) {
        let args = t.args.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join("$");
        return Some(format!("{}${args}", t.name));
    }
    class_of(Some(t))
}

/// Instantiates a generic product type `base[targs]` → mangled class
/// `base$targs`; stores the substituted layout in `layouts`.
fn inst_ptype(base: &str, tparams: &[String], fields: &[Field], targs: &[String], layouts: &mut HashMap<String, Layout>) -> String {
    let mangled = format!("{base}${}", targs.join("$"));
    if !layouts.contains_key(&mangled) {
        let tmap: HashMap<String, String> = tparams.iter().cloned().zip(targs.iter().cloned()).collect();
        let layout: Layout = fields
            .iter()
            .map(|f| {
                let cn = tmap.get(&f.ty.name).cloned().unwrap_or_else(|| f.ty.name.clone());
                let t = Type { name: cn, args: vec![], borrowed: false, span: f.ty.span };
                (f.name.clone(), ty_of(Some(&t)), class_of(Some(&t)))
            })
            .collect();
        layouts.insert(mangled.clone(), layout);
    }
    mangled
}

/// Instantiates a generic sum type `sum[targs]` → mangled class.
/// Fills `layouts` (class layout) + `svars` (variant → (tag, field layout)).
fn inst_stype(
    sum: &str,
    tparams: &[String],
    variants: &[(String, Vec<(String, String)>)],
    targs: &[String],
    layouts: &mut HashMap<String, Layout>,
    svars: &mut HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>>,
) -> String {
    let mangled = format!("{sum}${}", targs.join("$"));
    if layouts.contains_key(&mangled) {
        return mangled;
    }
    let tmap: HashMap<String, String> = tparams.iter().cloned().zip(targs.iter().cloned()).collect();
    let mut layout: Layout = vec![("__tag".into(), Ty::I64, None)];
    let mut vmap: HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)> = HashMap::new();
    for (tag, (vname, vfields)) in variants.iter().enumerate() {
        let vf: Vec<(String, Ty, Option<String>)> = vfields
            .iter()
            .map(|(fname, tyname)| {
                let cn = tmap.get(tyname).cloned().unwrap_or_else(|| tyname.clone());
                let t = Type { name: cn, args: vec![], borrowed: false, span: crate::diag::Span(0, 0) };
                (fname.clone(), ty_of(Some(&t)), class_of(Some(&t)))
            })
            .collect();
        layout.extend(vf.iter().cloned());
        vmap.insert(vname.clone(), (tag as i64, vf));
    }
    layouts.insert(mangled.clone(), layout);
    svars.insert(mangled.clone(), vmap);
    mangled
}

/// Synthetic method descriptor (for consistent vtable slot assignment
/// between trait call and impl). `self` (first param) is omitted.
fn method_desc(params: &[Ty]) -> String {
    let mut s = String::from("(");
    for t in params.iter().skip(1) {
        s.push(ty_code(*t));
    }
    s.push(')');
    s
}
fn ty_code(t: Ty) -> char {
    match t {
        Ty::F64 | Ty::F32 => 'D',
        Ty::Ref => 'L',
        Ty::I32 => 'I',
        _ => 'J',
    }
}

fn ret_ty(sig: &FnSig) -> Ty {
    match &sig.ret {
        None => Ty::Void,
        Some(t) if t.name == "Unit" => Ty::Void,
        Some(t) => ty_of(Some(t)),
    }
}

/// Return type of a function — until type inference (F5) exists.
/// With `-> T` annotation: exact. Without: estimated structurally from the tail
/// expression (no tail → Void). Used for call sites AND the function itself,
/// so that both agree.
fn guess_ret_ty(f: &FnDef) -> Ty {
    // `main` is the entry point — always Void, regardless of whether the last line
    // was parsed as a (Void) expression like `print(x)` as tail.
    if f.sig.name == "main" {
        return Ty::Void;
    }
    if f.sig.ret.is_some() {
        return ret_ty(&f.sig);
    }
    match f.body.as_ref().and_then(|b| b.tail.as_ref()) {
        Some(t) => guess_expr_ty(t),
        None => Ty::Void,
    }
}

/// Rough, annotation-free type estimate of an expression (only literals/structure).
/// Idents/calls without context → I64 (default integer). Replaces real inference.
fn guess_expr_ty(e: &Expr) -> Ty {
    match e {
        // `print(...)` returns Void (intrinsic).
        Expr::Call { callee, .. } if matches!(callee.as_ref(), Expr::Ident(n, _) if n == "print") => Ty::Void,
        Expr::Float(..) => Ty::F64,
        Expr::Bool(..) => Ty::I32,
        Expr::Str(..) => Ty::Ref,
        Expr::Int(..) => Ty::I64,
        Expr::Unary { rhs, .. } => guess_expr_ty(rhs),
        Expr::If { then, els, .. } => {
            let t = then.tail.as_ref().map(|e| guess_expr_ty(e)).unwrap_or(Ty::Void);
            if t != Ty::Void {
                t
            } else {
                els.as_ref().and_then(|b| b.tail.as_ref()).map(|e| guess_expr_ty(e)).unwrap_or(Ty::Void)
            }
        }
        Expr::Block(b) => b.tail.as_ref().map(|e| guess_expr_ty(e)).unwrap_or(Ty::Void),
        Expr::Binary { op, lhs, rhs, .. } => {
            if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                Ty::I32
            } else if guess_expr_ty(lhs) == Ty::F64 || guess_expr_ty(rhs) == Ty::F64 {
                Ty::F64
            } else {
                Ty::I64
            }
        }
        _ => Ty::I64,
    }
}

struct FnLower<'a> {
    locals: Vec<Ty>,
    blocks: Vec<BasicBlock>,
    cur: usize,
    scopes: Vec<HashMap<String, (Local, Ty)>>,
    sigs: &'a HashMap<String, Sig>,
    /// User type layouts (name → fields) for New/GetField.
    types: &'a HashMap<String, Layout>,
    /// Variant registry (variant name → info) for construction + match.
    variants: &'a HashMap<String, VariantInfo>,
    /// Generic functions (name → info) for call monomorphization.
    generics: &'a HashMap<String, GInfo>,
    /// Traits (name → methods) for dynamic dispatch (trait objects): a
    /// method call on a trait-typed receiver becomes `CallVirtual`.
    trait_methods: &'a HashMap<String, Vec<(String, String, Vec<Ty>, Ty)>>,
    /// Function ASTs (name → def) for higher-order inlining: when a lambda
    /// is passed, the called function expands inline at that spot
    /// (defunctionalization — direct, specialized code instead of a function pointer).
    fn_defs: &'a HashMap<String, FnDef>,
    /// Stack of the currently inline-expanded functions (recursion guard).
    inlining: Vec<String>,
    /// Generic product types (name → (type parameters, fields)) for `Box(x)`.
    generic_ptypes: &'a HashMap<String, (Vec<String>, Vec<Field>)>,
    /// Generic sum types (name → (type parameters, variants)) for `Some(x)`.
    generic_stypes: &'a HashMap<String, (Vec<String>, Vec<(String, Vec<(String, String)>)>)>,
    /// Variant → generic sum type (`Some` → `Option`) for construction/match.
    variant_owner_g: &'a HashMap<String, String>,
    /// Module-wide generic instances (annotation-driven): layout + variants.
    shared_inst: &'a HashMap<String, Layout>,
    shared_svars: &'a HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>>,
    /// Instantiated generic types of this function (mangled name → layout).
    /// Construction + annotated parameters fill this; the module registers
    /// the classes for the backend from it.
    local_inst: HashMap<String, Layout>,
    /// Payload-driven sum instances of this function (variant registry).
    local_svars: HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>>,
    /// Requested monomorph. instances: (generic name, concrete type arguments).
    mono: Vec<(String, Vec<String>)>,
    /// Class of a ref local (object local index → class name) for field access.
    local_class: HashMap<u32, String>,
    /// Element kind of an array/list local (for index/len/for-over-list).
    local_arr: HashMap<u32, ArrKind>,
    /// Lambda locals: `mut f = x -> …` → (parameter, body). The call `f(a)` is
    /// inline-expanded at that spot (capturing closures in the same scope for free).
    local_lambda: HashMap<u32, (Vec<String>, Expr)>,
    /// Shared string literal pool (Program::strings); `intern` returns indices.
    strings: &'a mut Vec<String>,
    /// O(1) index into the string pool (literal → index) — otherwise `intern`
    /// would be a linear search = O(n²) compile time with many literals (scaling for large prog.).
    str_idx: &'a mut HashMap<String, u32>,
    errs: Vec<String>,
    /// Target blocks of the enclosing loops: (continue → header, break → exit).
    loops: Vec<(Block, Block)>,
    /// Source line-start offsets (for debug `DebugLine` markers); empty = no debug.
    line_starts: &'a [usize],
    /// Line of the last emitted `DebugLine` marker (0 = none), to avoid repeats.
    last_dbg_line: u32,
    /// Mangled name of the function being lowered (the innermost DebugLine frame).
    fn_name: String,
}

impl<'a> FnLower<'a> {
    fn new_local(&mut self, ty: Ty) -> Local {
        self.locals.push(ty);
        Local((self.locals.len() - 1) as u32)
    }
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.str_idx.get(s) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.str_idx.insert(s.to_string(), i);
        self.strings.push(s.to_string());
        i
    }
    fn new_block(&mut self) -> Block {
        self.blocks.push(BasicBlock { statements: vec![], terminator: Terminator::Return(None) });
        Block((self.blocks.len() - 1) as u32)
    }
    fn emit(&mut self, s: Statement) {
        self.blocks[self.cur].statements.push(s);
    }
    fn term(&mut self, blk: usize, t: Terminator) {
        self.blocks[blk].terminator = t;
    }
    fn lookup(&self, name: &str) -> Option<(Local, Ty)> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return Some(*v);
            }
        }
        None
    }
    fn bind(&mut self, name: &str, l: Local, t: Ty) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), (l, t));
    }
    /// Layout of a class — user type OR instantiated generic type.
    fn layout_of(&self, class: &str) -> Option<Layout> {
        self.types
            .get(class)
            .or_else(|| self.local_inst.get(class))
            .or_else(|| self.shared_inst.get(class))
            .cloned()
    }
    /// Variant info (tag, field layout) of a variant IN a concrete
    /// (possibly generic) instance class. None → not in this instance.
    fn variant_in(&self, class: &str, variant: &str) -> Option<(String, i64, Vec<(String, Ty, Option<String>)>)> {
        let m = self.local_svars.get(class).or_else(|| self.shared_svars.get(class))?;
        let (tag, vf) = m.get(variant)?;
        Some((class.to_string(), *tag, vf.clone()))
    }
    /// Concrete type name of a lowered value (for type argument inference of
    /// generic constructors): class of a ref, otherwise scalar name.
    fn ty_name(&self, op: &Operand, ty: Ty) -> String {
        if let Some(c) = self.class_of_operand(op) {
            return c;
        }
        match ty {
            Ty::F64 => "Float",
            Ty::F32 => "F32",
            Ty::I32 => "I32",
            Ty::Ref => "Ref",
            _ => "Int",
        }
        .to_string()
    }
    /// Instantiates a generic product type `base[targs]` → mangled class
    /// `base$targs`. Stores the (substituted) layout once in `local_inst`.
    fn instantiate_ptype(&mut self, base: &str, tparams: &[String], fields: &[Field], targs: &[String]) -> String {
        inst_ptype(base, tparams, fields, targs, &mut self.local_inst)
    }
    /// Class of an operand, if it is a ref local with a known class.
    fn class_of_operand(&self, op: &Operand) -> Option<String> {
        match op {
            Operand::Copy(l) => self.local_class.get(&l.0).cloned(),
            _ => None,
        }
    }
    /// Array element kind of an operand, if it is an array/list local.
    fn arr_of_operand(&self, op: &Operand) -> Option<ArrKind> {
        match op {
            Operand::Copy(l) => self.local_arr.get(&l.0).copied(),
            _ => None,
        }
    }
    /// Bring an operand to i32 (array index/length are Java `int`).
    fn to_i32(&mut self, op: Operand) -> Operand {
        match op {
            Operand::ConstI64(v) => Operand::ConstI32(v as i32),
            Operand::ConstI32(_) => op,
            other => {
                let d = self.new_local(Ty::I32);
                self.emit(Statement::Assign(d, Rvalue::Convert(other)));
                Operand::Copy(d)
            }
        }
    }
    /// Operand → String (Ref). Ref stays; scalars via jrt_*_to_str.
    fn to_str(&mut self, op: Operand, ty: Ty) -> Operand {
        if ty == Ty::Ref {
            return op;
        }
        let func = match ty {
            Ty::F64 => "jrt_double_to_str",
            Ty::F32 => "jrt_float_to_str",
            Ty::I32 => "jrt_int_to_str",
            _ => "jrt_long_to_str",
        };
        let arg = if ty == Ty::I64 { op } else if matches!(ty, Ty::F64 | Ty::F32 | Ty::I32) { op } else { to_i64(op) };
        let d = self.new_local(Ty::Ref);
        self.emit(Statement::Call { dest: Some(d), func: func.into(), args: vec![arg] });
        Operand::Copy(d)
    }
    /// ArrayLen (i32) → i64 operand for Vire (Ints are i64).
    fn array_len_i64(&mut self, arr: Operand) -> Operand {
        let li32 = self.new_local(Ty::I32);
        self.emit(Statement::ArrayLen { dest: li32, arr });
        let l64 = self.new_local(Ty::I64);
        self.emit(Statement::Assign(l64, Rvalue::Convert(Operand::Copy(li32))));
        Operand::Copy(l64)
    }

    /// i32 operand sign-extended to i64 (for mixed int arithmetic with
    /// packed `I32` fields). Constants directly, otherwise a `Convert` (sext).
    fn widen_i32(&mut self, op: Operand) -> Operand {
        if let Operand::ConstI32(v) = op {
            return Operand::ConstI64(v as i64);
        }
        let d = self.new_local(Ty::I64);
        self.emit(Statement::Assign(d, Rvalue::Convert(op)));
        Operand::Copy(d)
    }

    fn lower_block(&mut self, b: &Block2) {
        let _ = self.lower_block_val(b); // Void context: tail value discarded
    }

    /// Name whose call creates a heap object (constructor of a user/
    /// generic type, variant, or collection builtin).
    fn is_alloc_name(&self, n: &str) -> bool {
        self.types.contains_key(n)
            || self.variants.contains_key(n)
            || self.generic_ptypes.contains_key(n)
            || self.variant_owner_g.contains_key(n)
            || matches!(n, "list" | "map" | "set" | "array" | "farray")
    }

    /// AUTOMATIC LOOP ARENA (escape→arena). A `while` iteration whose
    /// allocations provably do NOT leave the iteration is placed into a
    /// per-iteration bump arena (like an automatic capsule): no
    /// malloc/free per node, en-bloc release at the end of the iteration. Hits the
    /// ONLY measured gap (allocator; btree 2.57× ceiling). Conservative —
    /// any uncertainty ⇒ do not promote (no arena ⇒ no unsoundness).
    ///
    /// Safe if the body (transitively over user callees):
    ///  - allocates (otherwise no benefit),
    ///  - writes NO field/index (mutating an existing object could
    ///    store an arena reference to the outside; constructors do NOT count as
    ///    field writes — they are calls on fresh objects),
    ///  - calls NO mutator method (push/put/set/pop/add/insert),
    ///  - contains NO `return`/`break`/`continue` (at body level),
    ///  - only calls user functions + constructors (no extern/builtin/lambda —
    ///    could capture a reference),
    ///  - assigns no ref to an outer (cross-iteration) variable.
    fn while_arena_safe(&self, body: &Block2) -> bool {
        let mut outer: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in &self.scopes {
            for k in s.keys() {
                outer.insert(k.clone());
            }
        }
        let mut seen = std::collections::HashSet::new();
        if self.region_bad_block(body, true, &outer, &mut seen) {
            return false;
        }
        seen.clear();
        self.region_allocates_block(body, &mut seen)
    }

    fn region_bad_block(&self, b: &Block2, top: bool, outer: &std::collections::HashSet<String>, seen: &mut std::collections::HashSet<String>) -> bool {
        b.stmts.iter().any(|s| self.region_bad_stmt(s, top, outer, seen))
            || b.tail.as_deref().map(|e| self.region_bad_expr(e, outer, seen)).unwrap_or(false)
    }

    fn region_bad_stmt(&self, s: &Stmt, top: bool, outer: &std::collections::HashSet<String>, seen: &mut std::collections::HashSet<String>) -> bool {
        match s {
            Stmt::Return(..) => true,
            Stmt::Break(_) | Stmt::Continue(_) => top, // leaves OUR arena iteration
            // `name = expr` is a Let (rebinding) in the Vire AST. If it re-binds an
            // OUTER variable (declared before the loop) with a ref, the ref escapes
            // beyond the iteration → forbidden. New body-local names
            // (not in `outer`) are harmless (they die with the iteration).
            Stmt::Let { name, value, .. } => {
                // Rebinding an OUTER var with a ref escapes the iteration — unless the
                // outer var is scalar-typed (no ref fits) or the value is provably scalar.
                let outer_is_ref = self.lookup(name).map(|(_, t)| t == Ty::Ref).unwrap_or(true);
                let escapes = top
                    && outer.contains(name)
                    && outer_is_ref
                    && value.as_ref().map(|v| self.expr_may_be_ref(v)).unwrap_or(false);
                escapes || value.as_ref().map(|e| self.region_bad_expr(e, outer, seen)).unwrap_or(false)
            }
            Stmt::Assign { target, value, .. } => {
                let target_bad = match target {
                    // Field/index mutation only leaks the arena if the WRITTEN VALUE can be
                    // an arena pointer. A provably-scalar store (`a[i] = int`, `p.x = i*2`)
                    // can never make an arena object outlive the arena — a scalar is not a
                    // pointer — so it is safe regardless of whether the base escapes.
                    // `expr_may_be_ref` is conservative (true when unsure), so `!…` means
                    // "definitely scalar".
                    Expr::Field { .. } | Expr::Index { .. } => self.expr_may_be_ref(value),
                    // Ref to an outer variable (compound `x op= e`) → escapes. Safe if the
                    // outer variable is scalar-typed (a ref cannot be stored into an I64/F64
                    // slot) or the written value is provably scalar.
                    Expr::Ident(n, _) => {
                        top && outer.contains(n)
                            && self.lookup(n).map(|(_, t)| t == Ty::Ref).unwrap_or(true)
                            && self.expr_may_be_ref(value)
                    }
                    _ => false,
                };
                target_bad || self.region_bad_expr(value, outer, seen)
            }
            Stmt::Expr(e) => self.region_bad_expr(e, outer, seen),
            Stmt::While { cond, body, .. } => self.region_bad_expr(cond, outer, seen) || self.region_bad_block(body, false, outer, seen),
            Stmt::For { iter, body, .. } => self.region_bad_expr(iter, outer, seen) || self.region_bad_block(body, false, outer, seen),
        }
    }

    fn region_bad_expr(&self, e: &Expr, outer: &std::collections::HashSet<String>, seen: &mut std::collections::HashSet<String>) -> bool {
        match e {
            Expr::Call { callee, args, .. } => {
                let callee_bad = match callee.as_ref() {
                    Expr::Ident(n, _) => {
                        if let Some(fd) = self.fn_defs.get(n) {
                            if seen.insert(n.clone()) {
                                match &fd.body {
                                    Some(b) => self.region_bad_block(b, false, outer, seen),
                                    None => true, // only signature → opaque
                                }
                            } else {
                                false // already being checked (recursion) → ok for the cycle
                            }
                        } else if self.is_alloc_name(n) {
                            false // constructor of a fresh object — allowed
                        } else {
                            true // builtin/extern/unknown → conservatively opaque
                        }
                    }
                    // Mutator method on a (possibly outer) object → could store.
                    _ => true,
                };
                callee_bad || args.iter().any(|a| self.region_bad_expr(a, outer, seen))
            }
            Expr::Unary { rhs, .. } => self.region_bad_expr(rhs, outer, seen),
            Expr::Binary { lhs, rhs, .. } => self.region_bad_expr(lhs, outer, seen) || self.region_bad_expr(rhs, outer, seen),
            Expr::Field { base, .. } => self.region_bad_expr(base, outer, seen),
            Expr::Index { base, index, .. } => self.region_bad_expr(base, outer, seen) || self.region_bad_expr(index, outer, seen),
            Expr::If { cond, then, elifs, els, .. } => {
                self.region_bad_expr(cond, outer, seen)
                    || self.region_bad_block(then, false, outer, seen)
                    || elifs.iter().any(|(c, b)| self.region_bad_expr(c, outer, seen) || self.region_bad_block(b, false, outer, seen))
                    || els.as_ref().map(|b| self.region_bad_block(b, false, outer, seen)).unwrap_or(false)
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.region_bad_expr(scrutinee, outer, seen)
                    || arms.iter().any(|(_, g, b)| g.as_ref().map(|g| self.region_bad_expr(g, outer, seen)).unwrap_or(false) || self.region_bad_expr(b, outer, seen))
            }
            Expr::Block(b) => self.region_bad_block(b, false, outer, seen),
            Expr::List(xs, _) => xs.iter().any(|x| self.region_bad_expr(x, outer, seen)),
            Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => self.region_bad_expr(inner, outer, seen),
            Expr::Range { start, end, .. } => self.region_bad_expr(start, outer, seen) || self.region_bad_expr(end, outer, seen),
            // Lambda/Comprehension/MapLit/Capsule: conservatively opaque (could capture/store outside).
            Expr::Lambda { .. } | Expr::Comprehension { .. } | Expr::MapLit(..) | Expr::Capsule { .. } => true,
            _ => false,
        }
    }

    fn region_allocates_block(&self, b: &Block2, seen: &mut std::collections::HashSet<String>) -> bool {
        b.stmts.iter().any(|s| self.region_allocates_stmt(s, seen)) || b.tail.as_deref().map(|e| self.region_allocates_expr(e, seen)).unwrap_or(false)
    }
    fn region_allocates_stmt(&self, s: &Stmt, seen: &mut std::collections::HashSet<String>) -> bool {
        match s {
            Stmt::Let { value, .. } => value.as_ref().map(|e| self.region_allocates_expr(e, seen)).unwrap_or(false),
            Stmt::Assign { target, value, .. } => self.region_allocates_expr(target, seen) || self.region_allocates_expr(value, seen),
            Stmt::Expr(e) | Stmt::Return(Some(e), _) => self.region_allocates_expr(e, seen),
            Stmt::While { cond, body, .. } => self.region_allocates_expr(cond, seen) || self.region_allocates_block(body, seen),
            Stmt::For { iter, body, .. } => self.region_allocates_expr(iter, seen) || self.region_allocates_block(body, seen),
            _ => false,
        }
    }
    fn region_allocates_expr(&self, e: &Expr, seen: &mut std::collections::HashSet<String>) -> bool {
        match e {
            Expr::Call { callee, args, .. } => {
                let ca = match callee.as_ref() {
                    Expr::Ident(n, _) => {
                        if self.is_alloc_name(n) {
                            true
                        } else if let Some(fd) = self.fn_defs.get(n) {
                            if seen.insert(n.clone()) {
                                fd.body.as_ref().map(|b| self.region_allocates_block(b, seen)).unwrap_or(false)
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                ca || args.iter().any(|a| self.region_allocates_expr(a, seen))
            }
            Expr::List(..) | Expr::MapLit(..) | Expr::Comprehension { .. } => true,
            Expr::Unary { rhs, .. } => self.region_allocates_expr(rhs, seen),
            Expr::Binary { lhs, rhs, .. } => self.region_allocates_expr(lhs, seen) || self.region_allocates_expr(rhs, seen),
            Expr::Field { base, .. } => self.region_allocates_expr(base, seen),
            Expr::Index { base, index, .. } => self.region_allocates_expr(base, seen) || self.region_allocates_expr(index, seen),
            Expr::If { cond, then, elifs, els, .. } => {
                self.region_allocates_expr(cond, seen)
                    || self.region_allocates_block(then, seen)
                    || elifs.iter().any(|(c, b)| self.region_allocates_expr(c, seen) || self.region_allocates_block(b, seen))
                    || els.as_ref().map(|b| self.region_allocates_block(b, seen)).unwrap_or(false)
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.region_allocates_expr(scrutinee, seen) || arms.iter().any(|(_, g, b)| g.as_ref().map(|g| self.region_allocates_expr(g, seen)).unwrap_or(false) || self.region_allocates_expr(b, seen))
            }
            Expr::Block(b) => self.region_allocates_block(b, seen),
            Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => self.region_allocates_expr(inner, seen),
            _ => false,
        }
    }

    /// Conservative: can the expression yield a reference? (For the
    /// "ref to outer variable" escape check.) Only obviously scalar → false.
    fn expr_may_be_ref(&self, e: &Expr) -> bool {
        match e {
            Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Char(..) | Expr::Unary { .. } => false,
            Expr::Binary { op, lhs, rhs, .. } => {
                // String `+` yields Ref; other arithmetic/comparison/logic = scalar.
                matches!(op, BinOp::Add) && (self.expr_may_be_ref(lhs) || self.expr_may_be_ref(rhs))
            }
            Expr::Ident(n, _) => self.lookup(n).map(|(_, t)| t == Ty::Ref).unwrap_or(true),
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::Ident(n, _) => self.is_alloc_name(n) || self.sigs.get(n).map(|s| s.ret == Ty::Ref).unwrap_or(true),
                _ => true,
            },
            _ => true,
        }
    }

    /// Like `lower_block`, but yields the tail value (for if/block expressions).
    /// Without tail → (_, Void).
    fn lower_block_val(&mut self, b: &Block2) -> (Operand, Ty) {
        self.scopes.push(HashMap::new());
        for s in &b.stmts {
            self.lower_stmt(s);
        }
        let v = match &b.tail {
            Some(t) => {
                self.mark_line(expr_span(t));
                self.lower_expr(t)
            }
            None => (Operand::ConstI64(0), Ty::Void),
        };
        self.scopes.pop();
        v
    }

    /// Debug: emit a `DebugLine` marker for `span`'s source line (only in debug
    /// builds, and only when the line changed). The backend turns it into a
    /// `!DILocation` for the instructions that follow.
    fn mark_line(&mut self, span: crate::diag::Span) {
        if self.line_starts.is_empty() {
            return;
        }
        let line = line_of(self.line_starts, span.0);
        if line != 0 && line != self.last_dbg_line {
            self.last_dbg_line = line;
            let frame = vec![(self.fn_name.clone(), line)];
            self.emit(Statement::DebugLine(frame));
        }
    }

    fn lower_stmt(&mut self, s: &Stmt) {
        self.mark_line(stmt_span(s));
        match s {
            Stmt::Let { mutable, name, value, .. } => {
                // `mut f = x -> …`: remember lambda (the call is inline-expanded).
                if let Some(Expr::Lambda { params, body, .. }) = value {
                    let l = self.new_local(Ty::I64);
                    self.local_lambda.insert(l.0, (params.clone(), (**body).clone()));
                    self.bind(name, l, Ty::I64);
                    return;
                }
                // Binding-vs-assignment (F3 replacement until Resolve exists): `mut x = …`
                // always binds anew; a plain `x = …` on an already
                // visible name is an assignment, not shadowing.
                if !mutable {
                    if let Some((l, _)) = self.lookup(name) {
                        let (op, _) = match value {
                            Some(v) => self.lower_expr(v),
                            None => (Operand::ConstI64(0), Ty::I64),
                        };
                        // Update object class on reassignment (traversal
                        // `cur = cur.next` must keep knowing cur as a Node).
                        if let Some(c) = self.class_of_operand(&op) {
                            self.local_class.insert(l.0, c);
                        }
                        self.emit(Statement::Assign(l, Rvalue::Use(op)));
                        return;
                    }
                }
                let (op, ty) = match value {
                    Some(v) => self.lower_expr(v),
                    None => (Operand::ConstI64(0), Ty::I64),
                };
                let l = self.new_local(ty);
                // Pass the object class resp. array element kind on to the new local.
                if let Some(c) = self.class_of_operand(&op) {
                    self.local_class.insert(l.0, c);
                }
                if let Some(k) = self.arr_of_operand(&op) {
                    self.local_arr.insert(l.0, k);
                }
                self.emit(Statement::Assign(l, Rvalue::Use(op)));
                self.bind(name, l, ty);
            }
            Stmt::Assign { target, op, value, .. } => match target {
                Expr::Ident(name, _) => {
                    if let Some((l, _ty)) = self.lookup(name) {
                        let (rhs, _) = self.lower_expr(value);
                        if op.is_none() {
                            if let Some(c) = self.class_of_operand(&rhs) {
                                self.local_class.insert(l.0, c);
                            }
                        }
                        let rv = match op {
                            None => Rvalue::Use(rhs),
                            Some(o) => Rvalue::Binary(map_op(*o), Operand::Copy(l), rhs),
                        };
                        self.emit(Statement::Assign(l, rv));
                    } else {
                        self.errs.push(format!("unknown variable: {name}"));
                    }
                }
                // Field mutation `p.x = v` resp. `p.x op= v` → (Get)+Binary+PutField.
                Expr::Field { base, name, .. } => {
                    let (obj, _) = self.lower_expr(base);
                    let class = match self.class_of_operand(&obj) {
                        Some(c) => c,
                        None => {
                            self.errs.push(format!("field assignment `.{name}`: type of the object unknown (annotate it)"));
                            return;
                        }
                    };
                    let fty = match self.layout_of(&class).and_then(|l| l.into_iter().find(|(n, ..)| n == name)) {
                        Some((_, ty, _)) => ty,
                        None => {
                            self.errs.push(format!("`{class}` has no field `{name}`"));
                            return;
                        }
                    };
                    let (mut v, _) = self.lower_expr(value);
                    if let Some(o) = op {
                        // compound: read old value, combine.
                        let cur = self.new_local(fty);
                        self.emit(Statement::GetField { dest: cur, obj: obj.clone(), class: class.clone(), field: name.clone() });
                        let d = self.new_local(fty);
                        self.emit(Statement::Assign(d, Rvalue::Binary(map_op(*o), Operand::Copy(cur), v)));
                        v = Operand::Copy(d);
                    }
                    if fty == Ty::I64 {
                        v = to_i64(v);
                    }
                    self.emit(Statement::PutField { obj, class, field: name.clone(), value: v });
                }
                // Index assignment `xs[i] = v` (array or growable list).
                Expr::Index { base, index, .. } => {
                    let (arr, _) = self.lower_expr(base);
                    let (idx, _) = self.lower_expr(index);
                    let (mut v, vt) = self.lower_expr(value);
                    if self.class_of_operand(&arr).as_deref() == Some("$List") {
                        self.emit(Statement::Call { dest: None, func: "vire_list_set".into(), args: vec![arr, to_i64(idx), to_i64(v)] });
                    } else if let Some(kind) = self.arr_of_operand(&arr) {
                        if kind == ArrKind::Long && vt != Ty::I64 {
                            v = to_i64(v);
                        }
                        let idx32 = self.to_i32(idx);
                        self.emit(Statement::ArrayStore { arr, index: idx32, value: v, kind, checked: true });
                    } else {
                        self.errs.push("index assignment: not an array/list".into());
                    }
                }
                _ => {
                    self.errs.push("assignment target M2: only variables and fields".into());
                }
            },
            Stmt::Expr(e) => {
                self.lower_expr(e);
            }
            Stmt::Return(e, _) => {
                let t = match e {
                    Some(e) => {
                        let (op, _) = self.lower_expr(e);
                        Terminator::Return(Some(op))
                    }
                    None => Terminator::Return(None),
                };
                let cur = self.cur;
                self.term(cur, t);
                // Rest becomes a new (unreachable) block
                let nb = self.new_block();
                self.cur = nb.0 as usize;
            }
            Stmt::Break(_) => {
                match self.loops.last() {
                    Some((_, exit)) => {
                        let cur = self.cur;
                        self.term(cur, Terminator::Goto(*exit));
                        let nb = self.new_block();
                        self.cur = nb.0 as usize;
                    }
                    None => self.errs.push("`break` outside a loop".into()),
                }
            }
            Stmt::Continue(_) => {
                match self.loops.last() {
                    Some((header, _)) => {
                        let cur = self.cur;
                        self.term(cur, Terminator::Goto(*header));
                        let nb = self.new_block();
                        self.cur = nb.0 as usize;
                    }
                    None => self.errs.push("`continue` outside a loop".into()),
                }
            }
            Stmt::While { cond, body, .. } => {
                let header = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Goto(header));
                self.cur = header.0 as usize;
                let (c, _) = self.lower_expr(cond);
                let bodyb = self.new_block();
                let exit = self.new_block();
                self.term(header.0 as usize, Terminator::Branch { cond: c, then_blk: bodyb, else_blk: exit });
                self.cur = bodyb.0 as usize;
                // AUTO-ARENA (escape→arena): place provably non-escaping alloc.
                // into a per-iteration bump arena (no malloc/free per node).
                let arena = self.while_arena_safe(body);
                let body_locals_start = self.locals.len();
                if arena {
                    self.emit(Statement::Call { dest: None, func: "jrt_arena_push".into(), args: vec![] });
                }
                self.loops.push((header, exit));
                self.lower_block(body);
                self.loops.pop();
                if arena {
                    // Ref locals created in the body point into the arena. After the pop
                    // the memory is gone → null them BEFORE the pop, otherwise the
                    // function-end release (jrt_release) reads freed memory (UAF).
                    for idx in body_locals_start..self.locals.len() {
                        if self.locals[idx] == Ty::Ref {
                            self.emit(Statement::Assign(Local(idx as u32), Rvalue::Use(Operand::ConstNull)));
                        }
                    }
                    self.emit(Statement::Call { dest: None, func: "jrt_arena_pop".into(), args: vec![] });
                }
                let end = self.cur;
                self.term(end, Terminator::Goto(header));
                self.cur = exit.0 as usize;
            }
            Stmt::For { pat, iter, body, .. } => {
                let name = match pat {
                    Pattern::Bind(n, _) => n.clone(),
                    Pattern::Wildcard(_) => "_".into(),
                    _ => {
                        self.errs.push("for pattern: only `for x in …`".into());
                        return;
                    }
                };
                // `for x in liste` (non-Range) → iterate over the array:
                // i=0; while i<len { x = arr[i]; body; i++ }.
                if !matches!(iter, Expr::Range { .. }) {
                    let (arr, _) = self.lower_expr(iter);
                    // for over a growable list ($List) → vire_list_len/get.
                    if self.class_of_operand(&arr).as_deref() == Some("$List") {
                        let len = self.new_local(Ty::I64);
                        self.emit(Statement::Call { dest: Some(len), func: "vire_list_len".into(), args: vec![arr.clone()] });
                        let ivar = self.new_local(Ty::I64);
                        self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
                        let header = self.new_block();
                        let cur = self.cur;
                        self.term(cur, Terminator::Goto(header));
                        self.cur = header.0 as usize;
                        let c = self.new_local(Ty::I32);
                        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), Operand::Copy(len))));
                        let bodyb = self.new_block();
                        let latch = self.new_block();
                        let exit = self.new_block();
                        self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(c), then_blk: bodyb, else_blk: exit });
                        self.cur = bodyb.0 as usize;
                        let elem = self.new_local(Ty::I64);
                        self.emit(Statement::Call { dest: Some(elem), func: "vire_list_get".into(), args: vec![arr.clone(), Operand::Copy(ivar)] });
                        self.scopes.push(HashMap::new());
                        self.bind(&name, elem, Ty::I64);
                        self.loops.push((latch, exit));
                        self.lower_block(body);
                        self.loops.pop();
                        self.scopes.pop();
                        let end = self.cur;
                        self.term(end, Terminator::Goto(latch));
                        self.cur = latch.0 as usize;
                        self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                        self.term(latch.0 as usize, Terminator::Goto(header));
                        self.cur = exit.0 as usize;
                        return;
                    }
                    let kind = match self.arr_of_operand(&arr) {
                        Some(k) => k,
                        None => {
                            self.errs.push("for iterator: range `a..b` or a list".into());
                            return;
                        }
                    };
                    let vty = kind.value_ty();
                    let len = self.array_len_i64(arr.clone());
                    let ivar = self.new_local(Ty::I64);
                    self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
                    let header = self.new_block();
                    let cur = self.cur;
                    self.term(cur, Terminator::Goto(header));
                    self.cur = header.0 as usize;
                    let cond = self.new_local(Ty::I32);
                    self.emit(Statement::Assign(cond, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), len)));
                    let bodyb = self.new_block();
                    let latch = self.new_block();
                    let exit = self.new_block();
                    self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(cond), then_blk: bodyb, else_blk: exit });
                    self.cur = bodyb.0 as usize;
                    let elem = self.new_local(vty);
                    let idx32 = self.to_i32(Operand::Copy(ivar));
                    self.emit(Statement::ArrayLoad { dest: elem, arr, index: idx32, kind, checked: true });
                    self.scopes.push(HashMap::new());
                    self.bind(&name, elem, vty);
                    self.loops.push((latch, exit));
                    self.lower_block(body);
                    self.loops.pop();
                    self.scopes.pop();
                    let end = self.cur;
                    self.term(end, Terminator::Goto(latch));
                    self.cur = latch.0 as usize;
                    self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                    self.term(latch.0 as usize, Terminator::Goto(header));
                    self.cur = exit.0 as usize;
                    return;
                }
                let (start, end_op, incl) = match iter {
                    Expr::Range { start, end, inclusive, .. } => {
                        let (s, _) = self.lower_expr(start);
                        let (e, _) = self.lower_expr(end);
                        (s, e, *inclusive)
                    }
                    _ => unreachable!(),
                };
                let ivar = self.new_local(Ty::I64);
                self.emit(Statement::Assign(ivar, Rvalue::Use(to_i64(start))));
                let header = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Goto(header));
                self.cur = header.0 as usize;
                let cond = self.new_local(Ty::I32);
                let cmp = if incl { IB::CmpLe } else { IB::CmpLt };
                self.emit(Statement::Assign(cond, Rvalue::Binary(cmp, Operand::Copy(ivar), to_i64(end_op))));
                let bodyb = self.new_block();
                let latch = self.new_block(); // increment block: `continue` target
                let exit = self.new_block();
                self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(cond), then_blk: bodyb, else_blk: exit });
                self.cur = bodyb.0 as usize;
                // Bind the loop variable (always scalar I64) BEFORE the arena analysis so
                // `expr_may_be_ref` can see it — otherwise `a[i] = i` reads `i` as an
                // unknown (conservatively ref) and the scalar-store relaxation never fires.
                self.scopes.push(HashMap::new());
                self.bind(&name, ivar, Ty::I64);
                // AUTO-ARENA (escape→arena), same soundness gate as the `while` case:
                // a numeric `for` iteration whose allocations provably do not escape the
                // iteration runs in a per-iteration bump arena (no malloc/free per node).
                // `while_arena_safe` forbids body-level return/break/continue, so the
                // arena_pop below is never skipped.
                let arena = self.while_arena_safe(body);
                let body_locals_start = self.locals.len();
                if arena {
                    self.emit(Statement::Call { dest: None, func: "jrt_arena_push".into(), args: vec![] });
                }
                self.loops.push((latch, exit)); // continue → latch (not header!), otherwise no increment
                self.lower_block(body);
                self.loops.pop();
                self.scopes.pop();
                if arena {
                    // Ref locals created in the body point into the arena; null them
                    // before the pop so the function-end release does not read freed
                    // memory (UAF) — identical to the `while` arena.
                    for idx in body_locals_start..self.locals.len() {
                        if self.locals[idx] == Ty::Ref {
                            self.emit(Statement::Assign(Local(idx as u32), Rvalue::Use(Operand::ConstNull)));
                        }
                    }
                    self.emit(Statement::Call { dest: None, func: "jrt_arena_pop".into(), args: vec![] });
                }
                let end = self.cur;
                self.term(end, Terminator::Goto(latch));
                self.cur = latch.0 as usize;
                self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
                self.term(latch.0 as usize, Terminator::Goto(header));
                self.cur = exit.0 as usize;
            }
        }
    }

    /// Returns (operand, type). Emits temporaries as needed.
    fn lower_expr(&mut self, e: &Expr) -> (Operand, Ty) {
        match e {
            Expr::Int(v, _) => (Operand::ConstI64(*v as i64), Ty::I64),
            Expr::Float(v, _) => (Operand::ConstF64(*v), Ty::F64),
            Expr::Bool(b, _) => (Operand::ConstI32(if *b { 1 } else { 0 }), Ty::I32),
            Expr::Str(s, _) => {
                let id = self.intern(s);
                (Operand::ConstStr(id), Ty::Ref)
            }
            // `null` — MEASUREMENT BOOTSTRAP (not the final language; that has no
            // null, but Option). Only needed to construct linked/cyclic graphs
            // and thereby enter the RC/collector path on Vire IR FOR THE FIRST
            // TIME (M0.1b-on-Vire). Will be replaced by Option[T].
            Expr::Ident(name, _) if name == "null" && self.lookup(name).is_none() => {
                (Operand::ConstNull, Ty::Ref)
            }
            // Nullary variant as expression: `Empty` → tagged instance.
            Expr::Ident(name, _) if self.variants.contains_key(name) && self.lookup(name).is_none() => {
                self.build_variant(name, &[])
            }
            Expr::Ident(name, _) => match self.lookup(name) {
                Some((l, ty)) => (Operand::Copy(l), ty),
                None => {
                    self.errs.push(format!("unknown variable: {name}"));
                    (Operand::ConstI64(0), Ty::I64)
                }
            },
            // `self` = the receiver bound as a parameter.
            Expr::SelfExpr(_) => match self.lookup("self") {
                Some((l, ty)) => (Operand::Copy(l), ty),
                None => {
                    self.errs.push("`self` outside a method".into());
                    (Operand::ConstI64(0), Ty::I64)
                }
            },
            Expr::Unary { op, rhs, .. } => {
                let (r, rt) = self.lower_expr(rhs);
                match op {
                    UnOp::Neg => {
                        let d = self.new_local(rt);
                        self.emit(Statement::Assign(d, Rvalue::Neg(r)));
                        (Operand::Copy(d), rt)
                    }
                    UnOp::Not => {
                        let d = self.new_local(Ty::I32);
                        self.emit(Statement::Assign(d, Rvalue::Binary(IB::CmpEq, r, Operand::ConstI32(0))));
                        (Operand::Copy(d), Ty::I32)
                    }
                }
            }
            Expr::Binary { .. } if const_eval(e).is_some() => {
                // General constant folding: `2 + 3`, `WIDTH * HEIGHT` etc. →
                // constant at compile time (not only under `comptime`).
                match const_eval(e).unwrap() {
                    CVal::Int(v) => (Operand::ConstI64(v), Ty::I64),
                    CVal::Float(v) => (Operand::ConstF64(v), Ty::F64),
                    CVal::Bool(b) => (Operand::ConstI32(if b { 1 } else { 0 }), Ty::I32),
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let (mut l, mut lt) = self.lower_expr(lhs);
                let (mut r, mut rt) = self.lower_expr(rhs);
                // String concatenation: `+` with at least one ref side → Concat,
                // numbers are automatically converted to strings (`"n=" + n`).
                if matches!(op, BinOp::Add) && (lt == Ty::Ref || rt == Ty::Ref) {
                    let ls = self.to_str(l, lt);
                    let rs = self.to_str(r, rt);
                    let d = self.new_local(Ty::Ref);
                    self.emit(Statement::Call { dest: Some(d), func: "jrt_str_concat".into(), args: vec![ls, rs] });
                    return (Operand::Copy(d), Ty::Ref);
                }
                // Align integer widths: if the expression mixes a narrow i32
                // (e.g. a packed `I32` field) with i64, the i32 is sign-
                // extended. Otherwise the backend would emit `add i64 %a, %i32` (type error).
                // Makes opt-in `I32` field packing fully usable (RAM savings).
                if lt == Ty::I32 && rt == Ty::I64 {
                    l = self.widen_i32(l);
                    lt = Ty::I64;
                } else if rt == Ty::I32 && lt == Ty::I64 {
                    r = self.widen_i32(r);
                    rt = Ty::I64;
                }
                let _ = rt;
                let is_cmp = matches!(
                    op,
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                );
                let ty = if is_cmp { Ty::I32 } else { lt };
                let d = self.new_local(ty);
                self.emit(Statement::Assign(d, Rvalue::Binary(map_op(*op), l, r)));
                (Operand::Copy(d), ty)
            }
            Expr::Field { base, name, .. } => {
                let (obj, _) = self.lower_expr(base);
                let class = match self.class_of_operand(&obj) {
                    Some(c) => c,
                    None => {
                        self.errs.push(format!("field access `.{name}`: type of the object unknown (annotate it)"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let (fty, rtarget) = match self.layout_of(&class).and_then(|l| l.into_iter().find(|(n, ..)| n == name)) {
                    Some((_, ty, rt)) => (ty, rt.clone()),
                    None => {
                        self.errs.push(format!("`{class}` has no field `{name}`"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let d = self.new_local(fty);
                if let Some(rt) = rtarget {
                    self.local_class.insert(d.0, rt);
                }
                self.emit(Statement::GetField { dest: d, obj, class, field: name.clone() });
                (Operand::Copy(d), fty)
            }
            Expr::Call { callee, args, .. } => self.lower_call(callee, args),
            Expr::TurboCall { callee, targs, args, .. } => self.lower_turbocall(callee, targs, args),
            Expr::If { cond, then, elifs, els, .. } => self.lower_if(cond, then, elifs, els),
            Expr::Match { scrutinee, arms, .. } => self.lower_match(scrutinee, arms),
            // `comptime <expr>` → compile-time folding of constant expressions.
            // `x as T` — numeric conversion (int↔float, widths).
            Expr::Cast { inner, ty, .. } => {
                let (op, from) = self.lower_expr(inner);
                let to = ty_of(Some(ty));
                if from == to {
                    return (op, to);
                }
                let d = self.new_local(to);
                self.emit(Statement::Assign(d, Rvalue::Convert(op)));
                (Operand::Copy(d), to)
            }
            // `comptime if COND { A } else { B }` — conditional compilation: fold
            // COND at compile time and lower ONLY the taken branch, dropping the
            // rest (so a branch may reference platform-specific / otherwise-invalid
            // code that is never compiled). Falls back to constant folding for
            // value expressions (`comptime 2 + 3`).
            Expr::Comptime { inner, .. } => {
                if let Expr::If { cond, then, elifs, els, .. } = inner.as_ref() {
                    let taken: Option<&Block2> = match const_eval(cond) {
                        Some(CVal::Bool(true)) => Some(then),
                        Some(CVal::Bool(false)) => {
                            let mut chosen = els.as_ref();
                            for (ec, eb) in elifs {
                                match const_eval(ec) {
                                    Some(CVal::Bool(true)) => {
                                        chosen = Some(eb);
                                        break;
                                    }
                                    Some(CVal::Bool(false)) => continue,
                                    _ => {
                                        chosen = None;
                                        break;
                                    }
                                }
                            }
                            chosen
                        }
                        _ => {
                            self.errs.push("comptime if: condition is not a compile-time constant bool".into());
                            None
                        }
                    };
                    return match taken {
                        Some(b) => self.lower_block_val(b),
                        None => (Operand::ConstI64(0), Ty::Void),
                    };
                }
                match const_eval(inner) {
                    Some(CVal::Int(v)) => (Operand::ConstI64(v), Ty::I64),
                    Some(CVal::Float(v)) => (Operand::ConstF64(v), Ty::F64),
                    Some(CVal::Bool(b)) => (Operand::ConstI32(if b { 1 } else { 0 }), Ty::I32),
                    None => {
                        self.errs.push("comptime: expression is not constant-foldable (only literals/arithmetic/comparisons)".into());
                        (Operand::ConstI64(0), Ty::I64)
                    }
                }
            }
            // `e?` — error propagation for Result: Ok(v) → v; Err(_) → return e.
            // (Desugared to match; the enclosing function must return Result.)
            Expr::Try { inner, .. } => {
                let (obj, _) = self.lower_expr(inner);
                let tag = self.new_local(Ty::I64);
                self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: "Result".into(), field: "__tag".into() });
                let is_ok = self.new_local(Ty::I32);
                self.emit(Statement::Assign(is_ok, Rvalue::Binary(IB::CmpEq, Operand::Copy(tag), Operand::ConstI64(0))));
                let okb = self.new_block();
                let errb = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Branch { cond: Operand::Copy(is_ok), then_blk: okb, else_blk: errb });
                // Err branch: pass the whole Result on.
                self.term(errb.0 as usize, Terminator::Return(Some(obj.clone())));
                // Ok branch: extract the value.
                self.cur = okb.0 as usize;
                let v = self.new_local(Ty::I64);
                self.emit(Statement::GetField { dest: v, obj, class: "Result".into(), field: "Ok_value".into() });
                (Operand::Copy(v), Ty::I64)
            }
            // List literal `[a, b, c]` → NewArray + ArrayStore. Element kind from the
            // first element (homogeneous). Empty list → Long (default).
            Expr::List(elems, _) => {
                let lowered: Vec<(Operand, Ty)> = elems.iter().map(|e| self.lower_expr(e)).collect();
                let kind = lowered.first().map(|(_, t)| arrkind_of(*t)).unwrap_or(ArrKind::Long);
                let arr = self.new_local(Ty::Ref);
                self.local_arr.insert(arr.0, kind);
                self.emit(Statement::NewArray { dest: arr, kind, len: Operand::ConstI32(elems.len() as i32) });
                for (i, (mut v, t)) in lowered.into_iter().enumerate() {
                    if kind == ArrKind::Long {
                        v = to_i64(v);
                    }
                    let _ = t;
                    self.emit(Statement::ArrayStore { arr: Operand::Copy(arr), index: Operand::ConstI32(i as i32), value: v, kind, checked: false });
                }
                (Operand::Copy(arr), Ty::Ref)
            }
            Expr::Comprehension { elem, var, iter, cond, .. } => self.lower_comprehension(elem, var, iter, cond.as_deref()),
            // Map literal `[k: v, …]` → map() + put per pair.
            Expr::MapLit(pairs, _) => {
                let m = self.new_local(Ty::Ref);
                self.local_class.insert(m.0, "$Map".into());
                self.emit(Statement::Call { dest: Some(m), func: "vire_map_new".into(), args: vec![] });
                for (k, v) in pairs {
                    let (ko, kt) = self.lower_expr(k);
                    let (vo, vt) = self.lower_expr(v);
                    let ko = if kt == Ty::Ref { ko } else { to_i64(ko) };
                    let vo = if vt == Ty::Ref { vo } else { to_i64(vo) };
                    self.emit(Statement::Call { dest: None, func: "vire_map_put".into(), args: vec![Operand::Copy(m), ko, vo] });
                }
                (Operand::Copy(m), Ty::Ref)
            }
            // Indexing `xs[i]` → ArrayLoad (bounds-checked) resp. vire_list_get.
            Expr::Index { base, index, .. } => {
                let (arr, _) = self.lower_expr(base);
                if self.class_of_operand(&arr).as_deref() == Some("$List") {
                    let (idx, _) = self.lower_expr(index);
                    let d = self.new_local(Ty::I64);
                    self.emit(Statement::Call { dest: Some(d), func: "vire_list_get".into(), args: vec![arr, to_i64(idx)] });
                    return (Operand::Copy(d), Ty::I64);
                }
                let kind = match self.arr_of_operand(&arr) {
                    Some(k) => k,
                    None => {
                        self.errs.push("index `[]`: unknown array (annotate it)".into());
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                };
                let (idx, _) = self.lower_expr(index);
                let idx32 = self.to_i32(idx);
                let vty = kind.value_ty();
                let d = self.new_local(vty);
                self.emit(Statement::ArrayLoad { dest: d, arr, index: idx32, kind, checked: true });
                (Operand::Copy(d), vty)
            }
            Expr::Block(b) => self.lower_block_val(b),
            // capsule (pure form, scalar-in/-out): the body runs in its
            // own arena. `jrt_arena_push` before the body routes all heap
            // allocations there (immortal → no RC/collector), `jrt_arena_pop`
            // afterwards releases the arena en bloc. ONLY scalar inputs/result
            // allowed (hard errors otherwise): values cannot alias, and no
            // object pointer survives the arena → isolation + fault containment without
            // deep copy. Object-in/-out (deep copy) remains open.
            Expr::Capsule { inputs, body, .. } => {
                for (nm, _borrowed) in inputs {
                    if let Some((_, Ty::Ref)) = self.lookup(nm) {
                        self.errs.push(format!(
                            "capsule: object input `{nm}` not yet allowed — the isolation \
                             needs deep-copy-in (not yet implemented). Until then only \
                             scalar inputs (Int/Float/Bool), otherwise the containment guarantee \
                             would be a lie."
                        ));
                    }
                }
                // `return` in the body would skip arena_pop (arena leak) →
                // forbid it. break/continue: the loop targets are saved and
                // cleared during the body (inner loops set their own).
                if body_has_return(body) {
                    self.errs.push("capsule: `return` in the body not allowed (would leak the arena) — use the block value".into());
                }
                self.emit(Statement::Call { dest: None, func: "jrt_arena_push".into(), args: vec![] });
                let saved_loops = std::mem::take(&mut self.loops);
                let body_locals_start = self.locals.len(); // from here on: body locals
                let (val, ty) = self.lower_block_val(body);
                self.loops = saved_loops;
                if ty == Ty::Ref {
                    self.errs.push(
                        "capsule: object result not yet allowed — that needs deep-copy-out \
                         (otherwise dangling into the freed arena). Until then only a scalar result."
                            .into(),
                    );
                }
                // Capture the scalar result first (register/const, survives the pop).
                let res = self.new_local(if ty == Ty::Void { Ty::I64 } else { ty });
                if ty != Ty::Void {
                    self.emit(Statement::Assign(res, Rvalue::Use(val)));
                }
                // All ref locals created in the body point into the arena. After the
                // pop the memory is gone; but the backend releases ref locals at
                // function end (reads the header → use-after-free). Therefore set them to
                // null BEFORE the pop → jrt_release(null) is a no-op.
                for idx in body_locals_start..self.locals.len() {
                    if self.locals[idx] == Ty::Ref {
                        self.emit(Statement::Assign(Local(idx as u32), Rvalue::Use(Operand::ConstNull)));
                    }
                }
                self.emit(Statement::Call { dest: None, func: "jrt_arena_pop".into(), args: vec![] });
                if ty == Ty::Void {
                    (Operand::ConstI64(0), Ty::Void)
                } else {
                    (Operand::Copy(res), ty)
                }
            }
            Expr::Range { .. } => {
                self.errs.push("range only as a for iterator (M2)".into());
                (Operand::ConstI64(0), Ty::I64)
            }
            other => {
                self.errs.push(format!("expression M2 not yet lowered: {}", expr_kind(other)));
                (Operand::ConstI64(0), Ty::I64)
            }
        }
    }

    /// `f[T, N](args)` — a generic call with EXPLICIT arguments. Type params bind
    /// to the named type; comptime value params bind to the folded literal (value
    /// generics). Remaining type params are still inferred from the arguments.
    fn lower_turbocall(&mut self, callee: &str, targs: &[Expr], call_args: &[Expr]) -> (Operand, Ty) {
        let g = match self.generics.get(callee).cloned() {
            Some(g) => g,
            None => {
                self.errs.push(format!("turbofish `{callee}[..]`: `{callee}` is not a generic function"));
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        if targs.len() > g.tparams.len() {
            self.errs.push(format!("turbofish `{callee}`: {} generic arg(s) given but only {} declared", targs.len(), g.tparams.len()));
            return (Operand::ConstI64(0), Ty::I64);
        }
        let lowered: Vec<(Operand, Ty)> = call_args.iter().map(|a| self.lower_expr(a)).collect();
        let mut bind: HashMap<String, String> = HashMap::new();
        // Explicit positional generic args.
        for (i, ta) in targs.iter().enumerate() {
            let tp = g.tparams[i].clone();
            if g.comptime.get(i).copied().unwrap_or(false) {
                match const_eval(ta) {
                    Some(CVal::Int(v)) => {
                        bind.insert(tp, v.to_string());
                    }
                    _ => {
                        self.errs.push(format!("turbofish `{callee}[{tp}]`: value generic must be a comptime Int"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                }
            } else {
                match ta {
                    Expr::Ident(n, _) => {
                        bind.insert(tp, n.clone());
                    }
                    _ => {
                        self.errs.push(format!("turbofish `{callee}[{tp}]`: type argument must be a type name"));
                        return (Operand::ConstI64(0), Ty::I64);
                    }
                }
            }
        }
        // Infer any type params not given explicitly, from the argument types.
        for (i, pty) in g.param_tys.iter().enumerate() {
            if let Some(t) = pty {
                if g.tparams.contains(&t.name) && !bind.contains_key(&t.name) {
                    if let Some((op, ty)) = lowered.get(i) {
                        let cls = self.class_of_operand(op);
                        bind.entry(t.name.clone()).or_insert_with(|| concrete_tyname(*ty, cls.as_ref()));
                    }
                }
            }
        }
        let targ_strs: Vec<String> = g.tparams.iter().map(|tp| bind.get(tp).cloned().unwrap_or_else(|| "Int".into())).collect();
        let sym = mono_sym(callee, &targ_strs);
        self.mono.push((callee.to_string(), targ_strs));
        let ret = g.ret.as_ref().map(|t| ty_of(Some(&subst_type(t, &bind)))).unwrap_or(Ty::Void);
        let ret_class = g.ret.as_ref().and_then(|t| class_of(Some(&subst_type(t, &bind))));
        let arg_ops: Vec<Operand> = lowered.into_iter().map(|(o, _)| o).collect();
        if ret == Ty::Void {
            self.emit(Statement::Call { dest: None, func: sym, args: arg_ops });
            return (Operand::ConstI64(0), Ty::Void);
        }
        let d = self.new_local(ret);
        if let Some(c) = ret_class {
            self.local_class.insert(d.0, c);
        }
        self.emit(Statement::Call { dest: Some(d), func: sym, args: arg_ops });
        (Operand::Copy(d), ret)
    }

    fn lower_call(&mut self, callee: &Expr, args: &[Expr]) -> (Operand, Ty) {
        // Method call `obj.method(args)` → direct call `Class.method(obj, args)`
        // (monomorphic, no virtual dispatch — Vire types are (still) flat).
        if let Expr::Field { base, name, span } = callee {
            // Feature 6 — `log.LEVEL(msg)` with a COMPILE-TIME level filter. Levels
            // below the threshold lower to nothing (zero instructions), exactly the
            // "disabled log calls cost nothing" property. Enabled levels prepend a
            // level tag to a literal message at compile time and print it.
            if let Expr::Ident(id, _) = base.as_ref() {
                if id == "log" {
                    // 0=debug 1=info 2=warn 3=error. Default threshold: info and up.
                    const LOG_THRESHOLD: i32 = 1;
                    let (level, tag) = match name.as_str() {
                        "debug" => (0, "[DEBUG] "),
                        "info" => (1, "[INFO] "),
                        "warn" => (2, "[WARN] "),
                        "error" => (3, "[ERROR] "),
                        _ => {
                            self.errs.push(format!("log has no level `{name}` (use debug/info/warn/error)"));
                            return (Operand::ConstI64(0), Ty::Void);
                        }
                    };
                    if level >= LOG_THRESHOLD {
                        if let Some(msg) = args.first() {
                            let printed = match msg {
                                Expr::Str(s, sp) => Expr::Str(format!("{tag}{s}"), *sp),
                                other => other.clone(),
                            };
                            let call = Expr::Call {
                                callee: Box::new(Expr::Ident("print".into(), *span)),
                                args: vec![printed],
                                span: *span,
                            };
                            self.lower_expr(&call);
                        }
                    }
                    // Below threshold: emit nothing at all.
                    return (Operand::ConstI64(0), Ty::Void);
                }
            }
            let (obj, base_ty) = self.lower_expr(base);
            // `xs.len()` on an array → ArrayLen.
            if name == "len" && args.is_empty() && self.arr_of_operand(&obj).is_some() {
                let l = self.array_len_i64(obj);
                return (l, Ty::I64);
            }
            // Methods on growing lists ($List) and maps ($Map).
            if let Some(sent) = self.class_of_operand(&obj) {
                // `$Atomic` = a locally-constructed `Atomic(..)`; `Atomic` = a value
                // arriving typed as such (e.g. a worker parameter `c: Atomic`).
                if sent == "$Atomic" || sent == "Atomic" {
                    // `a.fetch_add(d)` (returns the previous value), `a.load()`.
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "fetch_add" | "add" => ("jrt_atomic_add", Ty::I64),
                        "load" | "get" => ("jrt_atomic_get", Ty::I64),
                        _ => {
                            self.errs.push(format!("Atomic has no method `{name}` (fetch_add/load)"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Channel" || sent == "Channel" {
                    // `c.send(v)` enqueues; `c.recv()` blocks for the next value.
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "send" => ("jrt_chan_send", Ty::Void),
                        "recv" => ("jrt_chan_recv", Ty::I64),
                        _ => {
                            self.errs.push(format!("Channel has no method `{name}` (send/recv)"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Mutex" || sent == "Mutex" {
                    // `m.lock()` / `m.unlock()` around a critical section; `m.get()` /
                    // `m.set(v)` read/update the guarded cell.
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "lock" => ("jrt_mutex_lock", Ty::Void),
                        "unlock" => ("jrt_mutex_unlock", Ty::Void),
                        "get" => ("jrt_mutex_get", Ty::I64),
                        "set" => ("jrt_mutex_set", Ty::Void),
                        _ => {
                            self.errs.push(format!("Mutex has no method `{name}` (lock/unlock/get/set)"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$List" {
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "push" => ("vire_list_push", Ty::Void),
                        "pop" => ("vire_list_pop", Ty::I64),
                        "len" => ("vire_list_len", Ty::I64),
                        "get" => ("vire_list_get", Ty::I64),
                        "set" => ("vire_list_set", Ty::Void),
                        "contains" => ("vire_list_contains", Ty::I32),
                        "clear" => ("vire_list_clear", Ty::Void),
                        _ => {
                            self.errs.push(format!("List has no method `{name}`"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Map" {
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "put" => ("vire_map_put", Ty::Void),
                        "get" => ("vire_map_get", Ty::I64),
                        "has" => ("vire_map_has", Ty::I32),
                        "remove" => ("vire_map_remove", Ty::I32),
                        "len" => ("vire_map_len", Ty::I64),
                        _ => {
                            self.errs.push(format!("Map has no method `{name}`"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
                if sent == "$Set" {
                    // A hash set of Ints (backed by the map runtime). `add`/`remove`
                    // return void/bool, `contains` a bool, `len` the count.
                    let a: Vec<Operand> = args.iter().map(|e| { let (o, t) = self.lower_expr(e); if t == Ty::Ref { o } else { to_i64(o) } }).collect();
                    let (func, ret): (&str, Ty) = match name.as_str() {
                        "add" => ("vire_set_add", Ty::Void),
                        "contains" => ("vire_set_contains", Ty::I64),
                        "remove" => ("vire_set_remove", Ty::I64),
                        "len" => ("vire_set_len", Ty::I64),
                        _ => {
                            self.errs.push(format!("Set has no method `{name}` (add/contains/remove/len)"));
                            return (Operand::ConstI64(0), Ty::I64);
                        }
                    };
                    let mut all = vec![obj];
                    all.extend(a);
                    if ret == Ty::Void {
                        self.emit(Statement::Call { dest: None, func: func.into(), args: all });
                        return (Operand::ConstI64(0), Ty::Void);
                    }
                    let d = self.new_local(ret);
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
            }
            // STRING methods. A string receiver is a bare `Ty::Ref` carrying no
            // sentinel/class (a literal, a concat/`str()` result, or a `Str`-typed
            // parameter — `class_of` returns `None`/`"Str"`). Route only KNOWN
            // method names to the `jrt_str_*` runtime; anything else falls through
            // to the "annotate the receiver" error so genuine unknown-type calls
            // still fail. Arg kinds: `'i'` = index → i32, else a string ref.
            if base_ty == Ty::Ref
                && self.arr_of_operand(&obj).is_none()
                && self.class_of_operand(&obj).as_deref().map_or(true, |c| c == "Str")
            {
                // (func, Vire result type, arg kinds). The `jrt_str_*` runtime
                // returns i32 for every scalar; an `Int`-typed result (`Ty::I64`)
                // is therefore widened from the i32 the call yields, whereas a
                // `Bool` result (`Ty::I32`) is used verbatim (Vire `Bool` = i32).
                let strm: Option<(&str, Ty, &[char])> = match name.as_str() {
                    "len" | "length" => Some(("jrt_str_length", Ty::I64, &[])),
                    "charAt" | "char_at" => Some(("jrt_str_char_at", Ty::I64, &['i'])),
                    "indexOf" | "index_of" => Some(("jrt_str_indexof", Ty::I64, &['r'])),
                    "compareTo" | "compare_to" => Some(("jrt_str_compareto", Ty::I64, &['r'])),
                    "isEmpty" | "is_empty" => Some(("jrt_str_is_empty", Ty::I32, &[])),
                    "equals" => Some(("jrt_str_equals", Ty::I32, &['r'])),
                    "startsWith" | "starts_with" => Some(("jrt_str_startswith", Ty::I32, &['r'])),
                    "endsWith" | "ends_with" => Some(("jrt_str_endswith", Ty::I32, &['r'])),
                    "trim" => Some(("jrt_str_trim", Ty::Ref, &[])),
                    "lower" | "toLowerCase" | "to_lower" => Some(("jrt_str_lower", Ty::Ref, &[])),
                    "upper" | "toUpperCase" | "to_upper" => Some(("jrt_str_upper", Ty::Ref, &[])),
                    "substring" if args.len() == 1 => Some(("jrt_str_substring1", Ty::Ref, &['i'])),
                    "substring" => Some(("jrt_str_substring2", Ty::Ref, &['i', 'i'])),
                    _ => None,
                };
                if let Some((func, ret, kinds)) = strm {
                    let mut all = vec![obj];
                    for (i, e) in args.iter().enumerate() {
                        let (o, t) = self.lower_expr(e);
                        all.push(match kinds.get(i) {
                            Some('i') => self.to_i32(o),
                            _ if t == Ty::Ref => o,
                            _ => to_i64(o),
                        });
                    }
                    // Integer result: the call is i32, widen to Int (i64).
                    if ret == Ty::I64 {
                        let d = self.new_local(Ty::I32);
                        self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                        return (self.widen_i32(Operand::Copy(d)), Ty::I64);
                    }
                    let d = self.new_local(ret);
                    // A string-returning method yields another string (chainable).
                    if ret == Ty::Ref { self.local_class.insert(d.0, "Str".into()); }
                    self.emit(Statement::Call { dest: Some(d), func: func.into(), args: all });
                    return (Operand::Copy(d), ret);
                }
            }
            let class = match self.class_of_operand(&obj) {
                Some(c) => c,
                None => {
                    self.errs.push(format!("method call `.{name}()`: type of the receiver unknown (annotate it)"));
                    return (Operand::ConstI64(0), Ty::I64);
                }
            };
            // TRAIT OBJECT: if the receiver is trait-typed (`s: Show`), dispatch
            // dynamically via the vtable (CallVirtual) — the concrete type is only
            // known at runtime. Otherwise a static `Typ.methode` call.
            if let Some(tms) = self.trait_methods.get(&class) {
                if let Some((mn, desc, params, ret)) = tms.iter().find(|(n, ..)| n == name).cloned() {
                    let mut arg_ops = vec![obj];
                    for a in args {
                        arg_ops.push(self.lower_expr(a).0);
                    }
                    let dest = if ret == Ty::Void { None } else { Some(self.new_local(ret)) };
                    self.emit(Statement::CallVirtual { dest, class: class.clone(), name: mn, desc, params, ret, args: arg_ops });
                    return match dest {
                        Some(d) => (Operand::Copy(d), ret),
                        None => (Operand::ConstI64(0), Ty::Void),
                    };
                }
            }
            let sym = format!("{class}.{name}");
            let mut arg_ops = vec![obj];
            for a in args {
                arg_ops.push(self.lower_expr(a).0);
            }
            let (ret, ret_class) = self.sigs.get(&sym).map(|s| (s.ret, s.ret_class.clone())).unwrap_or_else(|| {
                self.errs.push(format!("`{class}` has no method `{name}`"));
                (Ty::I64, None)
            });
            if ret == Ty::Void {
                self.emit(Statement::Call { dest: None, func: sym, args: arg_ops });
                return (Operand::ConstI64(0), Ty::Void);
            }
            let d = self.new_local(ret);
            if let Some(c) = ret_class {
                self.local_class.insert(d.0, c);
            }
            self.emit(Statement::Call { dest: Some(d), func: sym, args: arg_ops });
            return (Operand::Copy(d), ret);
        }
        let name = match callee {
            Expr::Ident(n, _) => n.clone(),
            _ => {
                self.errs.push("call target M2: only named functions".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        // Buffer-capture intrinsics for inline C/asm blocks: `@arraydata(a)` yields the
        // raw data pointer (past the 16-byte object header), `@arraylen(a)` the element
        // count. Together they pass a Vire array to a `native "c"` block as a proven
        // (ptr, len) pair (see cblock.rs / language/VERIFIED-C-ASM.md).
        if name == "@arraydata" {
            let a = self.lower_expr(&args[0]).0;
            let d = self.new_local(Ty::Ref);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_array_data".into(), args: vec![a] });
            return (Operand::Copy(d), Ty::Ref);
        }
        if name == "@arraylen" {
            let a = self.lower_expr(&args[0]).0;
            let l = self.array_len_i64(a);
            return (l, Ty::I64);
        }
        // Call of a lambda local `f(args)` → body inline (parameters bound).
        if let Some((l, _)) = self.lookup(&name) {
            if let Some((params, body)) = self.local_lambda.get(&l.0).cloned() {
                self.scopes.push(HashMap::new());
                for (p, arg) in params.iter().zip(args) {
                    let (op, ty) = self.lower_expr(arg);
                    let d = self.new_local(ty);
                    if let Some(c) = self.class_of_operand(&op) {
                        self.local_class.insert(d.0, c);
                    }
                    self.emit(Statement::Assign(d, Rvalue::Use(op)));
                    self.bind(p, d, ty);
                }
                let r = self.lower_expr(&body);
                self.scopes.pop();
                return r;
            }
        }
        // Data-carrying variant of a generic sum type (`Some(3.5)`) →
        // monomorphized type-correctly. Data-less variants (`None`) go through
        // the erased path (only __tag; payload type irrelevant).
        if self.variant_owner_g.contains_key(&name) && !args.is_empty() {
            return self.build_generic_variant(&name, args);
        }
        // Variant constructor of a sum type: `Circle(2.0)` → tagged instance.
        if self.variants.contains_key(&name) {
            return self.build_variant(&name, args);
        }
        // Constructor of a generic product type: `Box(x)` → infer the type arguments
        // from the argument types, instantiate `Box$Float`, New + PutField.
        if let Some((tparams, fields)) = self.generic_ptypes.get(&name).cloned() {
            let lowered: Vec<(Operand, Ty)> = args.iter().map(|a| self.lower_expr(a)).collect();
            // Derive the type parameter from the first field that is exactly `T`.
            let mut tmap: HashMap<String, String> = HashMap::new();
            for (i, f) in fields.iter().enumerate() {
                if tparams.iter().any(|tp| tp == &f.ty.name) {
                    if let Some((op, ty)) = lowered.get(i) {
                        tmap.entry(f.ty.name.clone()).or_insert_with(|| self.ty_name(op, *ty));
                    }
                }
            }
            let targs: Vec<String> = tparams.iter().map(|tp| tmap.get(tp).cloned().unwrap_or_else(|| "Int".into())).collect();
            let mangled = self.instantiate_ptype(&name, &tparams, &fields, &targs);
            let layout = self.local_inst[&mangled].clone();
            let obj = self.new_local(Ty::Ref);
            self.local_class.insert(obj.0, mangled.clone());
            self.emit(Statement::New { dest: obj, class: mangled.clone() });
            if lowered.len() != layout.len() {
                self.errs.push(format!("{name}: expected {} fields, {} given", layout.len(), lowered.len()));
            }
            for ((fname, fty, _), (mut v, _)) in layout.iter().zip(lowered) {
                if *fty == Ty::I64 {
                    v = to_i64(v);
                }
                self.emit(Statement::PutField { obj: Operand::Copy(obj), class: mangled.clone(), field: fname.clone(), value: v });
            }
            return (Operand::Copy(obj), Ty::Ref);
        }
        // Constructor of a user type: `Point(x, y)` → New + PutField per field
        // (field order = declaration order).
        if let Some(layout) = self.types.get(&name).cloned() {
            let obj = self.new_local(Ty::Ref);
            self.local_class.insert(obj.0, name.clone());
            self.emit(Statement::New { dest: obj, class: name.clone() });
            if args.len() != layout.len() {
                self.errs.push(format!("{name}: expected {} fields, {} given", layout.len(), args.len()));
            }
            for ((fname, fty, _), arg) in layout.iter().zip(args) {
                let (mut v, _) = self.lower_expr(arg);
                if *fty == Ty::I64 {
                    v = to_i64(v);
                }
                self.emit(Statement::PutField {
                    obj: Operand::Copy(obj),
                    class: name.clone(),
                    field: fname.clone(),
                    value: v,
                });
            }
            return (Operand::Copy(obj), Ty::Ref);
        }
        // Higher-order: if a lambda is passed, expand the called
        // function inline at this spot (parameters bound, lambda as
        // local_lambda). BEFORE the eager argument lowering — a lambda is not a
        // value and must not be lowered directly.
        if args.iter().any(|a| matches!(a, Expr::Lambda { .. })) {
            if let Some(fdef) = self.fn_defs.get(&name).cloned() {
                if fdef.body.is_some() {
                    return self.inline_higher_order(&name, &fdef, args);
                }
            }
        }
        // DEVIRT: if a CONCRETELY-typed object is passed to a trait parameter
        // (`run(a, …)` with `a: AddOp`, param `o: Op`), expand the function
        // inline → in the body `o` is concrete → `o.apply()` becomes a STATIC
        // call (no vtable/type check). This is g++'s devirtualization gain,
        // done cleanly in the closed-world solver. Only for a small body (bloat).
        if let Some(fdef) = self.fn_defs.get(&name).cloned() {
            if fdef.body.is_some() && self.devirt_inline_ok(&fdef, args) {
                return self.inline_higher_order(&name, &fdef, args);
            }
        }
        let lowered: Vec<(Operand, Ty)> = args.iter().map(|a| self.lower_expr(a)).collect();
        // Sized typed arrays: `array(n)` (Int), `farray(n)` (Float) —
        // real bounds-checked/-elidable arrays (as opposed to the i64 list).
        if name == "array" || name == "farray" {
            let kind = if name == "farray" { ArrKind::Double } else { ArrKind::Long };
            let n = lowered.into_iter().next().map(|(o, _)| o).unwrap_or(Operand::ConstI64(0));
            let len32 = self.to_i32(n);
            let arr = self.new_local(Ty::Ref);
            self.local_arr.insert(arr.0, kind);
            self.emit(Statement::NewArray { dest: arr, kind, len: len32 });
            return (Operand::Copy(arr), Ty::Ref);
        }
        // Collection builtins: `list()` (growing list), `map()` (Int→Int),
        // `set()` (Int hash set).
        if name == "list" || name == "map" || name == "set" {
            let (func, sentinel) = match name.as_str() {
                "list" => ("vire_list_new", "$List"),
                "map" => ("vire_map_new", "$Map"),
                _ => ("vire_set_new", "$Set"),
            };
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, sentinel.into());
            self.emit(Statement::Call { dest: Some(d), func: func.into(), args: vec![] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // `Atomic(v)` → shared atomic counter (a `$Atomic` ref; immortal, thread-safe).
        if name == "Atomic" {
            let init = lowered.into_iter().next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "$Atomic".into());
            self.emit(Statement::Call { dest: Some(d), func: "jrt_atomic_new".into(), args: vec![init] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // `Mutex(v)` → a lock-guarded cell (a `$Mutex` ref; immortal).
        if name == "Mutex" {
            let init = lowered.into_iter().next().map(|(o, _)| to_i64(o)).unwrap_or(Operand::ConstI64(0));
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "$Mutex".into());
            self.emit(Statement::Call { dest: Some(d), func: "jrt_mutex_new".into(), args: vec![init] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // `Channel()` → a thread-safe FIFO queue (a `$Channel` ref; immortal).
        if name == "Channel" {
            let d = self.new_local(Ty::Ref);
            self.local_class.insert(d.0, "$Channel".into());
            self.emit(Statement::Call { dest: Some(d), func: "jrt_chan_new".into(), args: vec![] });
            return (Operand::Copy(d), Ty::Ref);
        }
        // `join(h)` → wait for the spawned thread, yield its result.
        if name == "join" {
            let h = lowered.into_iter().next().map(|(o, _)| o).unwrap_or(Operand::ConstNull);
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "jrt_join".into(), args: vec![h] });
            return (Operand::Copy(d), Ty::I64);
        }
        // Builtin `str(x)` → text representation (Ref).
        if name == "str" {
            let (op, ty) = lowered.into_iter().next().unwrap_or((Operand::ConstNull, Ty::Ref));
            return (self.to_str(op, ty), Ty::Ref);
        }
        // FFI builtin `cstr(s)` → NUL-terminated char* (as Ptr/i64).
        if name == "cstr" {
            let arg = lowered.into_iter().next().map(|(o, _)| o).unwrap_or(Operand::ConstNull);
            let d = self.new_local(Ty::I64);
            self.emit(Statement::Call { dest: Some(d), func: "vire_cstr".into(), args: vec![arg] });
            return (Operand::Copy(d), Ty::I64);
        }
        // Intrinsic `print` — multi-argument: each argument on its own line.
        if name == "print" {
            if lowered.is_empty() {
                let empty = self.intern("");
                self.emit(Statement::Call { dest: None, func: "jrt_println_str".into(), args: vec![Operand::ConstStr(empty)] });
            }
            for (op, ty) in lowered {
                let func = match ty {
                    Ty::F64 | Ty::F32 => "jrt_println_double",
                    Ty::Ref => "jrt_println_str",
                    _ => "jrt_println_long",
                };
                let arg = if matches!(ty, Ty::F64 | Ty::F32 | Ty::Ref) { op } else { to_i64(op) };
                self.emit(Statement::Call { dest: None, func: func.to_string(), args: vec![arg] });
            }
            return (Operand::ConstI64(0), Ty::Void);
        }
        // Call of a generic function → bind the type arguments from the argument
        // types, request the monomorph instance `f$T…`, call on the instance.
        if let Some(g) = self.generics.get(&name).cloned() {
            let mut bind: HashMap<String, String> = HashMap::new();
            for (i, pty) in g.param_tys.iter().enumerate() {
                if let Some(t) = pty {
                    if g.tparams.contains(&t.name) {
                        if let Some((op, ty)) = lowered.get(i) {
                            let cls = self.class_of_operand(op);
                            bind.entry(t.name.clone()).or_insert_with(|| concrete_tyname(*ty, cls.as_ref()));
                        }
                    }
                }
            }
            let targs: Vec<String> = g.tparams.iter().map(|tp| bind.get(tp).cloned().unwrap_or_else(|| "Int".into())).collect();
            let sym = mono_sym(&name, &targs);
            self.mono.push((name.clone(), targs.clone()));
            let ret = g.ret.as_ref().map(|t| ty_of(Some(&subst_type(t, &bind)))).unwrap_or(Ty::Void);
            let ret_class = g.ret.as_ref().and_then(|t| class_of(Some(&subst_type(t, &bind))));
            let arg_ops: Vec<Operand> = lowered.into_iter().map(|(o, _)| o).collect();
            if ret == Ty::Void {
                self.emit(Statement::Call { dest: None, func: sym, args: arg_ops });
                return (Operand::ConstI64(0), Ty::Void);
            }
            let d = self.new_local(ret);
            if let Some(c) = ret_class {
                self.local_class.insert(d.0, c);
            }
            self.emit(Statement::Call { dest: Some(d), func: sym, args: arg_ops });
            return (Operand::Copy(d), ret);
        }
        // Call of an own function
        let (ret, ret_class) = self.sigs.get(&name).map(|s| (s.ret, s.ret_class.clone())).unwrap_or((Ty::I64, None));
        // Convenience: for `py_*` bridge functions, string arguments are
        // automatically turned into C strings (`cstr`), so that one can write
        // `py_import("math")` instead of `py_import(cstr("math"))`.
        let auto_cstr = name.starts_with("py_");
        let arg_ops: Vec<Operand> = lowered
            .into_iter()
            .map(|(o, t)| {
                if auto_cstr && t == Ty::Ref {
                    let d = self.new_local(Ty::I64);
                    self.emit(Statement::Call { dest: Some(d), func: "vire_cstr".into(), args: vec![o] });
                    Operand::Copy(d)
                } else {
                    o
                }
            })
            .collect();
        if ret == Ty::Void {
            self.emit(Statement::Call { dest: None, func: name, args: arg_ops });
            (Operand::ConstI64(0), Ty::Void)
        } else {
            let d = self.new_local(ret);
            if let Some(c) = ret_class {
                self.local_class.insert(d.0, c); // object return: remember the class
            }
            self.emit(Statement::Call { dest: Some(d), func: name, args: arg_ops });
            (Operand::Copy(d), ret)
        }
    }

    /// Expand a higher-order function inline: `apply(x -> x+1, 5)` →
    /// body of `apply` at the call site, `f` bound as local_lambda,
    /// value parameters as locals. Fully specialized (direct code, LLVM can
    /// inline further); the lambda's captures stay visible via the scope stack.
    /// Is a devirt inline expansion worthwhile? Yes, if a CONCRETELY-typed
    /// object goes to a trait parameter (then the method call in the body becomes
    /// static) AND the body is small enough (code-bloat bound).
    fn devirt_inline_ok(&self, fdef: &FnDef, args: &[Expr]) -> bool {
        let has_devirt = fdef.sig.params.iter().zip(args).any(|(p, a)| {
            let is_trait_param = p.ty.as_ref().map(|t| self.trait_methods.contains_key(&t.name)).unwrap_or(false);
            if !is_trait_param {
                return false;
            }
            if let Expr::Ident(n, _) = a {
                if let Some((l, _)) = self.lookup(n) {
                    if let Some(c) = self.local_class.get(&l.0) {
                        // concrete class (not a trait) → the call becomes static.
                        return !self.trait_methods.contains_key(c);
                    }
                }
            }
            false
        });
        has_devirt && fdef.body.as_ref().map(|b| b.stmts.len() <= 24).unwrap_or(false)
    }

    fn inline_higher_order(&mut self, name: &str, fdef: &FnDef, args: &[Expr]) -> (Operand, Ty) {
        let body = fdef.body.as_ref().unwrap();
        if self.inlining.iter().any(|n| n == name) {
            self.errs
                .push(format!("higher-order: recursive inlining of `{name}` not supported (a lambda is not a storable function pointer)"));
            return (Operand::ConstI64(0), Ty::I64);
        }
        if body_has_return(body) {
            self.errs.push(format!(
                "higher-order: `{name}` uses `return` — inline expansion needs an expression-shaped body (tail value instead of `return`)"
            ));
            return (Operand::ConstI64(0), Ty::I64);
        }
        self.inlining.push(name.to_string());
        self.scopes.push(HashMap::new());
        for (p, arg) in fdef.sig.params.iter().zip(args) {
            match arg {
                Expr::Lambda { params, body, .. } => {
                    let l = self.new_local(Ty::Ref);
                    self.local_lambda.insert(l.0, (params.clone(), (**body).clone()));
                    self.bind(&p.name, l, Ty::Ref);
                }
                _ => {
                    let (op, ty) = self.lower_expr(arg);
                    let d = self.new_local(ty);
                    if let Some(c) = self.class_of_operand(&op) {
                        self.local_class.insert(d.0, c);
                    }
                    if let Some(k) = self.arr_of_operand(&op) {
                        self.local_arr.insert(d.0, k);
                    }
                    self.emit(Statement::Assign(d, Rvalue::Use(op)));
                    self.bind(&p.name, d, ty);
                }
            }
        }
        let r = self.lower_block_val(body);
        self.scopes.pop();
        self.inlining.pop();
        r
    }

    /// List comprehension `[elem for var in src (if cond)]` → two-pass:
    /// count (with filter) → allocate result array → fill.
    fn lower_comprehension(&mut self, elem: &Expr, var: &str, iter: &Expr, cond: Option<&Expr>) -> (Operand, Ty) {
        let (src, _) = self.lower_expr(iter);
        let src_kind = match self.arr_of_operand(&src) {
            Some(k) => k,
            None => {
                self.errs.push("comprehension: source is not a list".into());
                return (Operand::ConstI64(0), Ty::I64);
            }
        };
        let src_vty = src_kind.value_ty();
        // Element kind of the result: probe elem in a dead block.
        let elem_kind = {
            let saved = self.cur;
            let dead = self.new_block();
            self.cur = dead.0 as usize;
            self.scopes.push(HashMap::new());
            let pv = self.new_local(src_vty);
            self.bind(var, pv, src_vty);
            let (_, ety) = self.lower_expr(elem);
            self.scopes.pop();
            self.cur = saved;
            arrkind_of(ety)
        };
        // Pass 1: count.
        let count = self.new_local(Ty::I64);
        self.emit(Statement::Assign(count, Rvalue::Use(Operand::ConstI64(0))));
        self.comp_loop(src.clone(), src_kind, var, src_vty, cond, &mut |s, _elem_local| {
            s.emit(Statement::Assign(count, Rvalue::Binary(IB::Add, Operand::Copy(count), Operand::ConstI64(1))));
        });
        // Allocate the result array.
        let count32 = self.to_i32(Operand::Copy(count));
        let res = self.new_local(Ty::Ref);
        self.local_arr.insert(res.0, elem_kind);
        self.emit(Statement::NewArray { dest: res, kind: elem_kind, len: count32 });
        // Pass 2: fill (evaluate elem, write at position j).
        let j = self.new_local(Ty::I64);
        self.emit(Statement::Assign(j, Rvalue::Use(Operand::ConstI64(0))));
        let elem_c = elem.clone();
        self.comp_loop(src, src_kind, var, src_vty, cond, &mut |s, _elem_local| {
            let (mut v, _) = s.lower_expr(&elem_c);
            if elem_kind == ArrKind::Long {
                v = to_i64(v);
            }
            let j32 = s.to_i32(Operand::Copy(j));
            s.emit(Statement::ArrayStore { arr: Operand::Copy(res), index: j32, value: v, kind: elem_kind, checked: true });
            s.emit(Statement::Assign(j, Rvalue::Binary(IB::Add, Operand::Copy(j), Operand::ConstI64(1))));
        });
        (Operand::Copy(res), Ty::Ref)
    }

    /// Emits `for var in src { if cond { body } }` — loop scaffold for
    /// comprehensions. `body` is called in the body (after an optional cond filter).
    fn comp_loop(&mut self, src: Operand, kind: ArrKind, var: &str, vty: Ty, cond: Option<&Expr>, body: &mut dyn FnMut(&mut Self, Local)) {
        let len = self.array_len_i64(src.clone());
        let ivar = self.new_local(Ty::I64);
        self.emit(Statement::Assign(ivar, Rvalue::Use(Operand::ConstI64(0))));
        let header = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Goto(header));
        self.cur = header.0 as usize;
        let c = self.new_local(Ty::I32);
        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpLt, Operand::Copy(ivar), len)));
        let bodyb = self.new_block();
        let latch = self.new_block();
        let exit = self.new_block();
        self.term(header.0 as usize, Terminator::Branch { cond: Operand::Copy(c), then_blk: bodyb, else_blk: exit });
        self.cur = bodyb.0 as usize;
        let elem = self.new_local(vty);
        let idx32 = self.to_i32(Operand::Copy(ivar));
        self.emit(Statement::ArrayLoad { dest: elem, arr: src, index: idx32, kind, checked: true });
        self.scopes.push(HashMap::new());
        self.bind(var, elem, vty);
        // optional filter
        if let Some(cnd) = cond {
            let (cv, _) = self.lower_expr(cnd);
            let keep = self.new_block();
            self.term(self.cur, Terminator::Branch { cond: cv, then_blk: keep, else_blk: latch });
            self.cur = keep.0 as usize;
        }
        body(self, elem);
        self.scopes.pop();
        self.term(self.cur, Terminator::Goto(latch));
        self.cur = latch.0 as usize;
        self.emit(Statement::Assign(ivar, Rvalue::Binary(IB::Add, Operand::Copy(ivar), Operand::ConstI64(1))));
        self.term(latch.0 as usize, Terminator::Goto(header));
        self.cur = exit.0 as usize;
    }

    /// Build a tagged variant: `New Sum`, `__tag = t`, fields from the arguments.
    fn build_variant(&mut self, vname: &str, args: &[Expr]) -> (Operand, Ty) {
        let (sum, tag, vfields) = self.variants.get(vname).cloned().unwrap();
        let obj = self.new_local(Ty::Ref);
        self.local_class.insert(obj.0, sum.clone());
        self.emit(Statement::New { dest: obj, class: sum.clone() });
        self.emit(Statement::PutField { obj: Operand::Copy(obj), class: sum.clone(), field: "__tag".into(), value: Operand::ConstI64(tag) });
        if args.len() != vfields.len() {
            self.errs.push(format!("variant `{vname}`: expected {} fields, {} given", vfields.len(), args.len()));
        }
        for ((fname, fty, _), arg) in vfields.iter().zip(args) {
            let (mut v, _) = self.lower_expr(arg);
            if *fty == Ty::I64 {
                v = to_i64(v);
            }
            self.emit(Statement::PutField { obj: Operand::Copy(obj), class: sum.clone(), field: fname.clone(), value: v });
        }
        (Operand::Copy(obj), Ty::Ref)
    }

    /// Build a data-carrying variant of a generic sum type (`Some(3.5)`) type-correctly:
    /// infer the type arguments from the payload types, instantiate `Option$Float`
    /// (F64 payload), tagged instance with the instance class.
    fn build_generic_variant(&mut self, vname: &str, args: &[Expr]) -> (Operand, Ty) {
        let sum = self.variant_owner_g[vname].clone();
        let (tparams, variants) = self.generic_stypes[&sum].clone();
        let vdef = variants.iter().find(|(n, _)| n == vname).cloned().unwrap_or((vname.into(), vec![]));
        let lowered: Vec<(Operand, Ty)> = args.iter().map(|a| self.lower_expr(a)).collect();
        // Derive the type parameters from the payload fields that are exactly `T`.
        let mut tmap: HashMap<String, String> = HashMap::new();
        for (i, (_, tyname)) in vdef.1.iter().enumerate() {
            if tparams.iter().any(|tp| tp == tyname) {
                if let Some((op, ty)) = lowered.get(i) {
                    tmap.entry(tyname.clone()).or_insert_with(|| self.ty_name(op, *ty));
                }
            }
        }
        let targs: Vec<String> = tparams.iter().map(|tp| tmap.get(tp).cloned().unwrap_or_else(|| "Int".into())).collect();
        let mangled = inst_stype(&sum, &tparams, &variants, &targs, &mut self.local_inst, &mut self.local_svars);
        let (_, tag, vfields) = self.variant_in(&mangled, vname).unwrap();
        let obj = self.new_local(Ty::Ref);
        self.local_class.insert(obj.0, mangled.clone());
        self.emit(Statement::New { dest: obj, class: mangled.clone() });
        self.emit(Statement::PutField { obj: Operand::Copy(obj), class: mangled.clone(), field: "__tag".into(), value: Operand::ConstI64(tag) });
        for ((fname, fty, _), (mut v, _)) in vfields.iter().zip(lowered) {
            if *fty == Ty::I64 {
                v = to_i64(v);
            }
            self.emit(Statement::PutField { obj: Operand::Copy(obj), class: mangled.clone(), field: fname.clone(), value: v });
        }
        (Operand::Copy(obj), Ty::Ref)
    }

    /// `match s { Variant(binds) -> body … _ -> body }` → dispatch via `__tag`,
    /// field extraction per arm, phi stand-in via a result local (like lower_if).
    fn lower_match(&mut self, scrut: &Expr, arms: &[(Pattern, Option<Expr>, Expr)]) -> (Operand, Ty) {
        let (obj, oty) = self.lower_expr(scrut);
        let class = self.class_of_operand(&obj);
        // Unfold or-patterns at arm level: `A | B -> body` → two arms.
        let mut flat: Vec<(Pattern, Option<Expr>, Expr)> = Vec::new();
        for (pat, guard, body) in arms {
            match pat {
                Pattern::Or(ps, _) => {
                    for p in ps {
                        flat.push((p.clone(), guard.clone(), body.clone()));
                    }
                }
                _ => flat.push((pat.clone(), guard.clone(), body.clone())),
            }
        }
        // Exhaustiveness check (compile time): non-exhaustive = HARD ERROR.
        self.check_exhaustive(&class, &flat);
        let merge = self.new_block();
        let mut ends: Vec<(usize, Operand, Ty)> = Vec::new();
        for (pat, guard, body) in &flat {
            let fail = self.new_block();
            self.scopes.push(HashMap::new());
            self.emit_pattern_test(obj.clone(), oty, class.clone(), pat, fail);
            // Guard after a successful pattern.
            if let Some(g) = guard {
                let (gc, _) = self.lower_expr(g);
                let cont = self.new_block();
                let cur = self.cur;
                self.term(cur, Terminator::Branch { cond: gc, then_blk: cont, else_blk: fail });
                self.cur = cont.0 as usize;
            }
            let (v, t) = self.lower_expr(body);
            self.scopes.pop();
            ends.push((self.cur, v, t));
            self.cur = fail.0 as usize; // the next arm begins in the fail block
        }
        let rty = ends.iter().map(|(_, _, t)| *t).find(|t| *t != Ty::Void).unwrap_or(Ty::Void);
        let res = if rty != Ty::Void { Some(self.new_local(rty)) } else { None };
        for (end, v, _) in &ends {
            if let Some(r) = res {
                self.blocks[*end].statements.push(Statement::Assign(r, Rvalue::Use(v.clone())));
            }
            self.blocks[*end].terminator = Terminator::Goto(merge);
        }
        // Close the fallthrough (unreachable because exhaustively checked) type-correctly.
        let cur = self.cur;
        if let Some(r) = res {
            self.blocks[cur].statements.push(Statement::Assign(r, Rvalue::Use(zero_of(rty))));
        }
        self.term(cur, Terminator::Goto(merge));
        self.cur = merge.0 as usize;
        match res {
            Some(r) => (Operand::Copy(r), rty),
            None => (Operand::ConstI64(0), Ty::Void),
        }
    }

    /// Emits the tests for ONE pattern against `obj`. On non-match
    /// → `fail`; on a match `self.cur` continues (with bindings in
    /// scope). Recursive for nested patterns.
    fn emit_pattern_test(&mut self, obj: Operand, ty: Ty, class: Option<String>, pat: &Pattern, fail: Block) {
        match pat {
            Pattern::Wildcard(_) => {}
            Pattern::Bind(name, _) => {
                let l = match &obj {
                    Operand::Copy(l) => *l,
                    _ => {
                        let d = self.new_local(ty);
                        self.emit(Statement::Assign(d, Rvalue::Use(obj.clone())));
                        d
                    }
                };
                if let Some(c) = &class {
                    self.local_class.insert(l.0, c.clone());
                }
                self.bind(name, l, ty);
            }
            Pattern::Int(v, _) => self.emit_eq_test(obj, to_i64(Operand::ConstI64(*v as i64)), fail),
            Pattern::Bool(b, _) => self.emit_eq_test(obj, Operand::ConstI32(if *b { 1 } else { 0 }), fail),
            Pattern::Ctor { name, args, .. } => {
                // For a generic instance class (`Option$Float`): use the instance layout
                // (type-correct field extraction), otherwise the erased variant.
                let inst = class.as_deref().and_then(|c| self.variant_in(c, name));
                let (sum, vtag, vfields) = match inst.or_else(|| self.variants.get(name).cloned()) {
                    Some(v) => v,
                    None => {
                        self.errs.push(format!("unknown variant `{name}` in match"));
                        return;
                    }
                };
                let tag = self.new_local(Ty::I64);
                self.emit(Statement::GetField { dest: tag, obj: obj.clone(), class: sum.clone(), field: "__tag".into() });
                self.emit_eq_test(Operand::Copy(tag), Operand::ConstI64(vtag), fail);
                for (j, argpat) in args.iter().enumerate() {
                    if let Some((fname, fty, rt)) = vfields.get(j).cloned() {
                        let d = self.new_local(fty);
                        if let Some(rc) = &rt {
                            self.local_class.insert(d.0, rc.clone());
                        }
                        self.emit(Statement::GetField { dest: d, obj: obj.clone(), class: sum.clone(), field: fname });
                        self.emit_pattern_test(Operand::Copy(d), fty, rt, argpat, fail);
                    }
                }
            }
            Pattern::Or(ps, _) => {
                // Nested or: try in order; if one matches → continue.
                let matched = self.new_block();
                for p in ps {
                    let next = self.new_block();
                    self.emit_pattern_test(obj.clone(), ty, class.clone(), p, next);
                    let cur = self.cur;
                    self.term(cur, Terminator::Goto(matched));
                    self.cur = next.0 as usize;
                }
                let cur = self.cur;
                self.term(cur, Terminator::Goto(fail));
                self.cur = matched.0 as usize;
            }
            _ => self.errs.push("match pattern: tuple/string patterns not yet lowered".into()),
        }
    }

    /// `obj == val ? continue : fail`.
    fn emit_eq_test(&mut self, obj: Operand, val: Operand, fail: Block) {
        let c = self.new_local(Ty::I32);
        self.emit(Statement::Assign(c, Rvalue::Binary(IB::CmpEq, obj, val)));
        let cont = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Branch { cond: Operand::Copy(c), then_blk: cont, else_blk: fail });
        self.cur = cont.0 as usize;
    }

    /// Compile-time exhaustiveness check. Sum type: all variants or `_`/Bind.
    /// Scalar/literal: a `_`/Bind branch required. Otherwise a hard error.
    fn check_exhaustive(&mut self, class: &Option<String>, arms: &[(Pattern, Option<Expr>, Expr)]) {
        let has_catchall = arms.iter().any(|(p, g, _)| g.is_none() && matches!(p, Pattern::Wildcard(_) | Pattern::Bind(..)));
        if has_catchall {
            return;
        }
        if let Some(sum) = class {
            // Generic instance class (`Option$Float`) → variants from the
            // instance registry; otherwise the erased variants of the sum type.
            let inst_vars = self.local_svars.get(sum).or_else(|| self.shared_svars.get(sum));
            let all: Vec<(String, i64)> = if let Some(m) = inst_vars {
                m.iter().map(|(n, (t, _))| (n.clone(), *t)).collect()
            } else {
                self.variants.iter().filter(|(_, (s, _, _))| s == sum).map(|(n, (_, t, _))| (n.clone(), *t)).collect()
            };
            if all.is_empty() {
                return; // not a sum type (e.g. product type) → no check
            }
            let tag_of = |name: &str| -> Option<i64> {
                if let Some(m) = inst_vars {
                    m.get(name).map(|(t, _)| *t)
                } else {
                    self.variants.get(name).map(|(_, t, _)| *t)
                }
            };
            let mut covered = std::collections::HashSet::new();
            for (p, g, _) in arms {
                if g.is_some() {
                    continue; // guard can fail → does not reliably cover
                }
                if let Pattern::Ctor { name, args, .. } = p {
                    // covers the tag only if all arguments are irrefutable
                    if args.iter().all(is_irrefutable) {
                        if let Some(t) = tag_of(name) {
                            covered.insert(t);
                        }
                    }
                }
            }
            let missing: Vec<&str> = all.iter().filter(|(_, t)| !covered.contains(t)).map(|(n, _)| n.as_str()).collect();
            if !missing.is_empty() {
                self.errs.push(format!("non-exhaustive `match`: missing {} (or `_` arm)", missing.join(", ")));
            }
        } else {
            self.errs.push("`match` over scalar/literal needs a `_` arm (non-exhaustive)".into());
        }
    }

    fn lower_if(&mut self, cond: &Expr, then: &Block2, elifs: &[(Expr, Block2)], els: &Option<Block2>) -> (Operand, Ty) {
        let (c, _) = self.lower_expr(cond);
        let thenb = self.new_block();
        let elseb = self.new_block();
        let merge = self.new_block();
        let cur = self.cur;
        self.term(cur, Terminator::Branch { cond: c, then_blk: thenb, else_blk: elseb });
        // then branch → value + end block (not yet terminated).
        self.cur = thenb.0 as usize;
        let (tv, tty) = self.lower_block_val(then);
        let te = self.cur;
        // else branch: further `elif`s recursively, else the `else` block, else no value.
        self.cur = elseb.0 as usize;
        let (ev, ety) = if !elifs.is_empty() {
            let (ec, eb) = &elifs[0];
            let rest: Vec<(Expr, Block2)> = elifs[1..].to_vec();
            self.lower_if(ec, eb, &rest, els)
        } else if let Some(e) = els {
            self.lower_block_val(e)
        } else {
            (Operand::ConstI64(0), Ty::Void)
        };
        let ee = self.cur;
        // Result type: the non-Void branch wins (both equal for a value-if).
        let rty = if tty != Ty::Void { tty } else { ety };
        if rty != Ty::Void {
            // Phi replacement: shared result local, assigned in both end blocks.
            let res = self.new_local(rty);
            self.blocks[te].statements.push(Statement::Assign(res, Rvalue::Use(tv)));
            self.blocks[ee].statements.push(Statement::Assign(res, Rvalue::Use(ev)));
            self.term(te, Terminator::Goto(merge));
            self.term(ee, Terminator::Goto(merge));
            self.cur = merge.0 as usize;
            (Operand::Copy(res), rty)
        } else {
            self.term(te, Terminator::Goto(merge));
            self.term(ee, Terminator::Goto(merge));
            self.cur = merge.0 as usize;
            (Operand::ConstI64(0), Ty::Void)
        }
    }
}

// The AST calls it Block; here an alias to avoid a name collision with ir::Block.
use crate::ast::Block as Block2;

#[allow(clippy::too_many_arguments)]
fn lower_fn(
    f: &FnDef,
    sigs: &HashMap<String, Sig>,
    types: &HashMap<String, Layout>,
    variants: &HashMap<String, VariantInfo>,
    generics: &HashMap<String, GInfo>,
    trait_methods: &HashMap<String, Vec<(String, String, Vec<Ty>, Ty)>>,
    fn_defs: &HashMap<String, FnDef>,
    generic_ptypes: &HashMap<String, (Vec<String>, Vec<Field>)>,
    generic_stypes: &HashMap<String, (Vec<String>, Vec<(String, Vec<(String, String)>)>)>,
    variant_owner_g: &HashMap<String, String>,
    shared_inst: &HashMap<String, Layout>,
    shared_svars: &HashMap<String, HashMap<String, (i64, Vec<(String, Ty, Option<String>)>)>>,
    strings: &mut Vec<String>,
    str_idx: &mut HashMap<String, u32>,
    recv_class: Option<&str>,
    sym: Option<&str>,
    line: u32,
    line_starts: &[usize],
) -> Result<(Function, Vec<(String, Vec<String>)>, HashMap<String, Layout>), Vec<String>> {
    let ret = guess_ret_ty(f);
    let name = match sym {
        Some(s) => s.to_string(),
        None if f.sig.name == "main" => "java_main".to_string(),
        None => f.sig.name.clone(),
    };
    let mut fl = FnLower {
        locals: Vec::new(),
        blocks: Vec::new(),
        cur: 0,
        scopes: vec![HashMap::new()],
        sigs,
        types,
        variants,
        generics,
        trait_methods,
        fn_defs,
        inlining: Vec::new(),
        generic_ptypes,
        generic_stypes,
        variant_owner_g,
        shared_inst,
        shared_svars,
        local_inst: HashMap::new(),
        local_svars: HashMap::new(),
        mono: Vec::new(),
        local_class: HashMap::new(),
        local_arr: HashMap::new(),
        local_lambda: HashMap::new(),
        strings,
        str_idx,
        errs: Vec::new(),
        loops: Vec::new(),
        line_starts,
        last_dbg_line: 0,
        fn_name: name.clone(),
    };
    // Block 0
    fl.new_block();
    // Parameter → Locals 0..n
    let mut param_tys = Vec::new();
    for p in &f.sig.params {
        // `self` receiver: Ref to the method class.
        let (t, cls) = if p.name == "self" {
            (Ty::Ref, recv_class.map(|c| c.to_string()))
        } else if let Some(pt) = p.ty.as_ref().filter(|pt| !pt.args.is_empty() && fl.generic_ptypes.contains_key(&pt.name)) {
            // Annotated generic type `b: Box[Int]` → instance `Box$Int`, so that
            // field accesses in the body find the concrete layout.
            let (tparams, fields) = fl.generic_ptypes[&pt.name].clone();
            let targs: Vec<String> = pt.args.iter().map(|a| a.name.clone()).collect();
            let mangled = fl.instantiate_ptype(&pt.name, &tparams, &fields, &targs);
            (Ty::Ref, Some(mangled))
        } else {
            (ty_of(p.ty.as_ref()), class_of(p.ty.as_ref()))
        };
        param_tys.push(t);
        let l = fl.new_local(t);
        if let Some(c) = cls {
            fl.local_class.insert(l.0, c);
        }
        fl.bind(&p.name, l, t);
    }
    if let Some(body) = &f.body {
        // Statements + tail (tail = return value, if ret != Void)
        fl.scopes.push(HashMap::new());
        for s in &body.stmts {
            fl.lower_stmt(s);
        }
        let term = if let Some(t) = &body.tail {
            fl.mark_line(expr_span(t));
            let (op, _) = fl.lower_expr(t);
            if ret == Ty::Void { Terminator::Return(None) } else { Terminator::Return(Some(op)) }
        } else if ret == Ty::Void {
            Terminator::Return(None)
        } else {
            // No tail, but a typed return value: the value comes from a
            // `return` statement; this fallthrough block is unreachable. But it
            // must terminate type-correctly (otherwise `ret void` in an i64 function).
            Terminator::Return(Some(zero_of(ret)))
        };
        fl.scopes.pop();
        let cur = fl.cur;
        fl.term(cur, term);
    } else {
        let cur = fl.cur;
        fl.term(cur, Terminator::Return(None));
    }
    if !fl.errs.is_empty() {
        return Err(fl.errs);
    }
    let mono = fl.mono;
    let local_inst = fl.local_inst;
    Ok((
        Function {
            name,
            params: param_tys,
            ret,
            locals: fl.locals,
            blocks: fl.blocks,
            receiver_nonnull: false,
            line,
        },
        mono,
        local_inst,
    ))
}

/// Higher-order template? True if a parameter is used as a call target in the
/// body (`f(x)` with `f` = parameter). Such functions are ONLY
/// inline-expanded (defunctionalization) — never lowered standalone, because the
/// function parameter has no runtime value.
fn is_higher_order(f: &FnDef) -> bool {
    let params: std::collections::HashSet<&str> = f.sig.params.iter().map(|p| p.name.as_str()).collect();
    match &f.body {
        Some(b) => block_calls_param(b, &params),
        None => false,
    }
}
fn block_calls_param(b: &Block2, params: &std::collections::HashSet<&str>) -> bool {
    b.stmts.iter().any(|s| stmt_calls_param(s, params)) || b.tail.as_deref().map(|e| expr_calls_param(e, params)).unwrap_or(false)
}
fn stmt_calls_param(s: &Stmt, params: &std::collections::HashSet<&str>) -> bool {
    match s {
        Stmt::Let { value, .. } => value.as_ref().map(|e| expr_calls_param(e, params)).unwrap_or(false),
        Stmt::Assign { target, value, .. } => expr_calls_param(target, params) || expr_calls_param(value, params),
        Stmt::Expr(e) => expr_calls_param(e, params),
        Stmt::Return(v, _) => v.as_ref().map(|e| expr_calls_param(e, params)).unwrap_or(false),
        Stmt::While { cond, body, .. } => expr_calls_param(cond, params) || block_calls_param(body, params),
        Stmt::For { iter, body, .. } => expr_calls_param(iter, params) || block_calls_param(body, params),
        Stmt::Break(_) | Stmt::Continue(_) => false,
    }
}
fn expr_calls_param(e: &Expr, params: &std::collections::HashSet<&str>) -> bool {
    let sub = |e: &Expr| expr_calls_param(e, params);
    let blk = |b: &Block2| block_calls_param(b, params);
    match e {
        Expr::Call { callee, args, .. } => {
            (matches!(callee.as_ref(), Expr::Ident(n, _) if params.contains(n.as_str()))) || sub(callee) || args.iter().any(sub)
        }
        Expr::Unary { rhs, .. } => sub(rhs),
        Expr::Binary { lhs, rhs, .. } => sub(lhs) || sub(rhs),
        Expr::Field { base, .. } => sub(base),
        Expr::Index { base, index, .. } => sub(base) || sub(index),
        Expr::If { cond, then, elifs, els, .. } => {
            sub(cond) || blk(then) || elifs.iter().any(|(c, b)| sub(c) || blk(b)) || els.as_ref().map(blk).unwrap_or(false)
        }
        Expr::Match { scrutinee, arms, .. } => sub(scrutinee) || arms.iter().any(|(_, g, b)| g.as_ref().map(sub).unwrap_or(false) || sub(b)),
        Expr::Block(b) => blk(b),
        Expr::Lambda { body, .. } => sub(body),
        Expr::List(xs, _) => xs.iter().any(sub),
        Expr::Comprehension { elem, iter, cond, .. } => sub(elem) || sub(iter) || cond.as_deref().map(sub).unwrap_or(false),
        Expr::MapLit(kvs, _) => kvs.iter().any(|(k, v)| sub(k) || sub(v)),
        Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => sub(inner),
        Expr::Range { start, end, .. } => sub(start) || sub(end),
        Expr::Capsule { body, .. } => blk(body),
        _ => false,
    }
}

/// Does a block contain (recursively over nested blocks/loops/if) a
/// `return` statement? For the capsule check (return would skip arena_pop).
fn body_has_return(b: &Block2) -> bool {
    b.stmts.iter().any(stmt_has_return)
}
fn stmt_has_return(s: &Stmt) -> bool {
    match s {
        Stmt::Return(..) => true,
        Stmt::While { body, .. } | Stmt::For { body, .. } => body_has_return(body),
        _ => false,
    }
}

/// Compile-time constant value (for `comptime`).
enum CVal {
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Folds a constant expression at compile time. `None` = not constant.
fn const_eval(e: &Expr) -> Option<CVal> {
    Some(match e {
        Expr::Int(v, _) => CVal::Int(*v as i64),
        Expr::Float(v, _) => CVal::Float(*v),
        Expr::Bool(b, _) => CVal::Bool(*b),
        Expr::Comptime { inner, .. } => return const_eval(inner),
        Expr::Unary { op, rhs, .. } => match (op, const_eval(rhs)?) {
            (UnOp::Neg, CVal::Int(v)) => CVal::Int(-v),
            (UnOp::Neg, CVal::Float(v)) => CVal::Float(-v),
            (UnOp::Not, CVal::Bool(b)) => CVal::Bool(!b),
            _ => return None,
        },
        Expr::Binary { op, lhs, rhs, .. } => {
            let (l, r) = (const_eval(lhs)?, const_eval(rhs)?);
            match (l, r) {
                (CVal::Int(a), CVal::Int(b)) => match op {
                    BinOp::Add | BinOp::AddWrap => CVal::Int(a.wrapping_add(b)),
                    BinOp::Sub | BinOp::SubWrap => CVal::Int(a.wrapping_sub(b)),
                    BinOp::Mul | BinOp::MulWrap => CVal::Int(a.wrapping_mul(b)),
                    BinOp::Div if b != 0 => CVal::Int(a / b),
                    BinOp::Rem if b != 0 => CVal::Int(a % b),
                    BinOp::Eq => CVal::Bool(a == b),
                    BinOp::Ne => CVal::Bool(a != b),
                    BinOp::Lt => CVal::Bool(a < b),
                    BinOp::Le => CVal::Bool(a <= b),
                    BinOp::Gt => CVal::Bool(a > b),
                    BinOp::Ge => CVal::Bool(a >= b),
                    BinOp::BitAnd => CVal::Int(a & b),
                    BinOp::BitOr => CVal::Int(a | b),
                    BinOp::BitXor => CVal::Int(a ^ b),
                    BinOp::Shl => CVal::Int(a << b),
                    BinOp::Shr => CVal::Int(a >> b),
                    _ => return None,
                },
                (CVal::Float(a), CVal::Float(b)) => match op {
                    BinOp::Add => CVal::Float(a + b),
                    BinOp::Sub => CVal::Float(a - b),
                    BinOp::Mul => CVal::Float(a * b),
                    BinOp::Div => CVal::Float(a / b),
                    BinOp::Lt => CVal::Bool(a < b),
                    BinOp::Le => CVal::Bool(a <= b),
                    BinOp::Gt => CVal::Bool(a > b),
                    BinOp::Ge => CVal::Bool(a >= b),
                    _ => return None,
                },
                (CVal::Bool(a), CVal::Bool(b)) => match op {
                    BinOp::And => CVal::Bool(a && b),
                    BinOp::Or => CVal::Bool(a || b),
                    BinOp::Eq => CVal::Bool(a == b),
                    BinOp::Ne => CVal::Bool(a != b),
                    _ => return None,
                },
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// Pattern that always matches (covers its slot completely).
fn is_irrefutable(p: &Pattern) -> bool {
    matches!(p, Pattern::Wildcard(_) | Pattern::Bind(..))
}

/// IR value type → array element kind.
fn arrkind_of(t: Ty) -> ArrKind {
    match t {
        Ty::F64 => ArrKind::Double,
        Ty::F32 => ArrKind::Float,
        Ty::I32 => ArrKind::Int,
        Ty::Ref => ArrKind::Ref,
        _ => ArrKind::Long,
    }
}

/// Type-correct null/default operand (for unreachable typed returns).
fn zero_of(t: Ty) -> Operand {
    match t {
        Ty::F64 => Operand::ConstF64(0.0),
        Ty::F32 => Operand::ConstF32(0.0),
        Ty::I32 => Operand::ConstI32(0),
        Ty::Ref => Operand::ConstNull,
        _ => Operand::ConstI64(0),
    }
}

fn map_op(o: BinOp) -> IB {
    match o {
        BinOp::Add | BinOp::AddWrap => IB::Add,
        BinOp::Sub | BinOp::SubWrap => IB::Sub,
        BinOp::Mul | BinOp::MulWrap => IB::Mul,
        BinOp::Div => IB::Div,
        BinOp::Rem => IB::Rem,
        BinOp::Eq => IB::CmpEq,
        BinOp::Ne => IB::CmpNe,
        BinOp::Lt => IB::CmpLt,
        BinOp::Le => IB::CmpLe,
        BinOp::Gt => IB::CmpGt,
        BinOp::Ge => IB::CmpGe,
        BinOp::And | BinOp::BitAnd => IB::And,
        BinOp::Or | BinOp::BitOr => IB::Or,
        BinOp::BitXor => IB::Xor,
        BinOp::Shl => IB::Shl,
        BinOp::Shr => IB::Shr,
    }
}

/// For comparisons/range with mixed const widths: use i32 constants as i64.
/// Best-effort source span of an expression (for debug line markers).
fn expr_span(e: &Expr) -> crate::diag::Span {
    use Expr::*;
    match e {
        Int(_, s) | Float(_, s) | Str(_, s) | Char(_, s) | Bool(_, s) | Ident(_, s) | SelfExpr(s) => *s,
        Unary { span, .. } | Binary { span, .. } | Call { span, .. } | TurboCall { span, .. }
        | Field { span, .. } | Index { span, .. } | If { span, .. } | Match { span, .. }
        | Cast { span, .. } | Try { span, .. } | Range { span, .. } | Lambda { span, .. }
        | List(_, span) | MapLit(_, span) | Comptime { span, .. } | Capsule { span, .. }
        | Spawn { span, .. } => *span,
        _ => crate::diag::Span(0, 0),
    }
}

/// Best-effort source span of a statement (for debug line markers).
fn stmt_span(s: &Stmt) -> crate::diag::Span {
    match s {
        Stmt::Let { span, .. } | Stmt::Assign { span, .. } | Stmt::While { span, .. } | Stmt::For { span, .. } => *span,
        Stmt::Return(_, span) | Stmt::Break(span) | Stmt::Continue(span) => *span,
        Stmt::Expr(e) => expr_span(e),
    }
}

fn to_i64(op: Operand) -> Operand {
    match op {
        Operand::ConstI32(v) => Operand::ConstI64(v as i64),
        other => other,
    }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Str(..) => "Str", Expr::Char(..) => "Char", Expr::SelfExpr(..) => "self",
        Expr::Field { .. } => "field access", Expr::Index { .. } => "Index",
        Expr::Match { .. } => "match", Expr::Lambda { .. } => "Lambda",
        Expr::List(..) => "list", Expr::Try { .. } => "?", Expr::Cast { .. } => "as",
        Expr::Comptime { .. } => "comptime", Expr::Capsule { .. } => "capsule",
        _ => "expression",
    }
}
