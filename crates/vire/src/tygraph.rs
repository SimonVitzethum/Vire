//! Persisted type graph — the source-level, structural view of a program's
//! types that survives past inference.
//!
//! This is the foundation of the compile-time programming layer (see TODO.md
//! "Compile-time programming layer"). Unlike the transient, IR-erased maps built
//! inside `lower.rs` (where a user type collapses to `Ty::Ref` + a class name and
//! generic parameters are monomorphized away), this graph keeps the *structural*
//! picture — generic parameters, nested type applications (`Box[List[Int]]`),
//! variants, trait method signatures, impls — exactly what reflection
//! (`@typeinfo`/`@derive`) and typed macros need to read.
//!
//! It is built purely from the (post-inference) AST, so it is decoupled from
//! lowering: `lower.rs` is untouched and keeps building its own IR-oriented maps.
//! Later phases migrate consumers onto this graph.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::ast::{Item, Module, Type};

/// A structural reference to a type as written in source: `Int`, `&T`,
/// `Box[List[Int]]`. Preserves generic application and borrow markers (which the
/// IR lattice erases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    pub name: String,
    pub args: Vec<TypeRef>,
    pub borrowed: bool,
}

impl TypeRef {
    fn of(t: &Type) -> Self {
        TypeRef { name: t.name.clone(), args: t.args.iter().map(TypeRef::of).collect(), borrowed: t.borrowed }
    }
}

impl std::fmt::Display for TypeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.borrowed {
            write!(f, "&")?;
        }
        write!(f, "{}", self.name)?;
        if !self.args.is_empty() {
            let inner: Vec<String> = self.args.iter().map(|a| a.to_string()).collect();
            write!(f, "[{}]", inner.join(", "))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeRef,
}

#[derive(Debug, Clone)]
pub struct VariantDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    pub positional: bool,
}

#[derive(Debug, Clone)]
pub enum TypeKind {
    Product { fields: Vec<FieldDecl> },
    Sum { variants: Vec<VariantDecl> },
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub name: String,
    /// Generic parameter names (`["T"]` for `Box[T]`), empty for a concrete type.
    pub generics: Vec<String>,
    pub kind: TypeKind,
    /// Inherent (type-inline) method names.
    pub methods: Vec<String>,
    /// Traits implemented for this type (from `impl Trait for T`).
    pub traits: Vec<String>,
    /// True for the compiler-provided `Option`/`Result` (not user-written).
    pub builtin: bool,
}

#[derive(Debug, Clone)]
pub struct ParamDecl {
    pub name: String,
    pub ty: Option<TypeRef>,
}

#[derive(Debug, Clone)]
pub struct MethodSig {
    pub name: String,
    pub generics: Vec<String>,
    pub params: Vec<ParamDecl>,
    pub ret: Option<TypeRef>,
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: String,
    pub methods: Vec<MethodSig>,
}

#[derive(Debug, Clone)]
pub struct ImplDecl {
    /// `None` for an inherent `impl T { … }`, `Some("Show")` for `impl Show for T`.
    pub trait_name: Option<String>,
    pub for_type: TypeRef,
    pub methods: Vec<String>,
}

/// The persisted, source-level type graph of a module.
#[derive(Debug, Clone, Default)]
pub struct TypeGraph {
    pub types: BTreeMap<String, TypeDecl>,
    pub traits: BTreeMap<String, TraitDecl>,
    pub impls: Vec<ImplDecl>,
    pub funcs: BTreeMap<String, MethodSig>,
}

fn method_sig(sig: &crate::ast::FnSig) -> MethodSig {
    MethodSig {
        name: sig.name.clone(),
        generics: sig.generics.iter().map(|g| g.name.clone()).collect(),
        params: sig
            .params
            .iter()
            .map(|p| ParamDecl { name: p.name.clone(), ty: p.ty.as_ref().map(TypeRef::of) })
            .collect(),
        ret: sig.ret.as_ref().map(TypeRef::of),
    }
}

fn fields_of(fs: &[crate::ast::Field]) -> Vec<FieldDecl> {
    fs.iter().map(|f| FieldDecl { name: f.name.clone(), ty: TypeRef::of(&f.ty) }).collect()
}

impl TypeGraph {
    /// Build the graph from a (post-inference) module. Pure over the AST.
    pub fn build(m: &Module) -> Self {
        let mut g = TypeGraph::default();

        for it in &m.items {
            match it {
                Item::Type(t) => {
                    let generics = t.generics.iter().map(|g| g.name.clone()).collect();
                    let kind = if t.variants.is_empty() {
                        TypeKind::Product { fields: fields_of(&t.fields) }
                    } else {
                        TypeKind::Sum {
                            variants: t
                                .variants
                                .iter()
                                .map(|v| VariantDecl {
                                    name: v.name.clone(),
                                    fields: fields_of(&v.fields),
                                    positional: v.positional,
                                })
                                .collect(),
                        }
                    };
                    let methods = t.methods.iter().map(|md| md.sig.name.clone()).collect();
                    g.types.insert(
                        t.name.clone(),
                        TypeDecl { name: t.name.clone(), generics, kind, methods, traits: Vec::new(), builtin: false },
                    );
                }
                Item::Trait(tr) => {
                    g.traits.insert(
                        tr.name.clone(),
                        TraitDecl { name: tr.name.clone(), methods: tr.methods.iter().map(|md| method_sig(&md.sig)).collect() },
                    );
                }
                Item::Impl(im) => {
                    g.impls.push(ImplDecl {
                        trait_name: im.trait_name.clone(),
                        for_type: TypeRef::of(&im.for_type),
                        methods: im.methods.iter().map(|md| md.sig.name.clone()).collect(),
                    });
                }
                Item::Fn(f) => {
                    g.funcs.insert(f.sig.name.clone(), method_sig(&f.sig));
                }
                _ => {}
            }
        }

        // Compiler-provided sum types (only if the user did not shadow them).
        for (name, variants) in builtin_sum_types() {
            g.types.entry(name.to_string()).or_insert_with(|| TypeDecl {
                name: name.to_string(),
                generics: builtin_generics(name),
                kind: TypeKind::Sum { variants },
                methods: Vec::new(),
                traits: Vec::new(),
                builtin: true,
            });
        }

        // Back-fill each type's implemented traits + impl methods from the impls.
        for im in &g.impls {
            if let Some(td) = g.types.get_mut(&im.for_type.name) {
                if let Some(tr) = &im.trait_name {
                    if !td.traits.contains(tr) {
                        td.traits.push(tr.clone());
                    }
                }
                for mn in &im.methods {
                    if !td.methods.contains(mn) {
                        td.methods.push(mn.clone());
                    }
                }
            }
        }

        g
    }

    /// Human-readable dump for the `vire types` introspection command and tests.
    pub fn dump(&self) -> String {
        let mut s = String::new();
        for td in self.types.values() {
            let gp = if td.generics.is_empty() { String::new() } else { format!("[{}]", td.generics.join(", ")) };
            let tag = if td.builtin { " (builtin)" } else { "" };
            match &td.kind {
                TypeKind::Product { fields } => {
                    writeln!(s, "type {}{}{}", td.name, gp, tag).unwrap();
                    for f in fields {
                        writeln!(s, "  field {}: {}", f.name, f.ty).unwrap();
                    }
                }
                TypeKind::Sum { variants } => {
                    writeln!(s, "enum {}{}{}", td.name, gp, tag).unwrap();
                    for v in variants {
                        let fs: Vec<String> = v.fields.iter().map(|f| f.ty.to_string()).collect();
                        if fs.is_empty() {
                            writeln!(s, "  variant {}", v.name).unwrap();
                        } else {
                            writeln!(s, "  variant {}({})", v.name, fs.join(", ")).unwrap();
                        }
                    }
                }
            }
            for tr in &td.traits {
                writeln!(s, "  impl {tr}").unwrap();
            }
            for m in &td.methods {
                writeln!(s, "  method {m}").unwrap();
            }
        }
        for tr in self.traits.values() {
            writeln!(s, "trait {}", tr.name).unwrap();
            for m in &tr.methods {
                writeln!(s, "  method {}{}{}", m.name, gen_suffix(m), sig_suffix(m)).unwrap();
            }
        }
        for f in self.funcs.values() {
            writeln!(s, "fn {}{}{}", f.name, gen_suffix(f), sig_suffix(f)).unwrap();
        }
        s
    }
}

fn gen_suffix(m: &MethodSig) -> String {
    if m.generics.is_empty() {
        String::new()
    } else {
        format!("[{}]", m.generics.join(", "))
    }
}

fn sig_suffix(m: &MethodSig) -> String {
    let ps: Vec<String> = m
        .params
        .iter()
        .map(|p| match &p.ty {
            Some(t) => format!("{}: {}", p.name, t),
            None => p.name.clone(),
        })
        .collect();
    let ret = m.ret.as_ref().map(|r| format!(" -> {r}")).unwrap_or_default();
    format!("({}){}", ps.join(", "), ret)
}

/// The compiler-provided generic sum types, mirrored from `lower.rs` so the graph
/// reflects what is actually compiled when the user does not define them.
fn builtin_sum_types() -> Vec<(&'static str, Vec<VariantDecl>)> {
    let t = |n: &str| TypeRef { name: n.into(), args: vec![], borrowed: false };
    vec![
        (
            "Option",
            vec![
                VariantDecl { name: "Some".into(), fields: vec![FieldDecl { name: "value".into(), ty: t("T") }], positional: true },
                VariantDecl { name: "None".into(), fields: vec![], positional: false },
            ],
        ),
        (
            "Result",
            vec![
                VariantDecl { name: "Ok".into(), fields: vec![FieldDecl { name: "value".into(), ty: t("T") }], positional: true },
                VariantDecl { name: "Err".into(), fields: vec![FieldDecl { name: "error".into(), ty: t("E") }], positional: true },
            ],
        ),
    ]
}

/// Generic parameter lists for the builtin sum types (`Result` has two).
fn builtin_generics(name: &str) -> Vec<String> {
    match name {
        "Result" => vec!["T".into(), "E".into()],
        _ => vec!["T".into()],
    }
}
