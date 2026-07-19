//! First-class inline blocks: `@c(""" …C… """, cap1, cap2)` and `@asm(""" …asm… """,
//! cap)`. Each is desugared into a generated `native "c"` / `native "asm"` function
//! plus an `extern "C"` declaration, and the call site is replaced by a call to it.
//!
//! The captured Vire values (currently scalar parameters of the enclosing function)
//! become the block's parameters, so the C/asm reads them by name / by the SysV
//! register they land in. The default memory-safety verification gate then PROVES the
//! generated block safe (see language/VERIFIED-C-ASM.md). Buffer/array capture (the
//! (ptr,len) ABI + a call-site `elements` contract) is the next step.

use crate::ast::*;
use crate::diag::Span;
use std::collections::HashMap;

/// Rewrite every `@c`/`@asm` call in the module and append the generated foreign
/// items. Returns any diagnostics (unsupported captures, etc.).
pub fn desugar_cblocks(m: &mut Module) -> Vec<String> {
    let mut generated: Vec<Item> = Vec::new();
    let mut counter = 0u32;
    let mut errs: Vec<String> = Vec::new();
    for item in &mut m.items {
        if let Item::Fn(f) = item {
            let ptypes: HashMap<String, String> = f
                .sig
                .params
                .iter()
                .filter_map(|p| p.ty.as_ref().map(|t| (p.name.clone(), t.name.clone())))
                .collect();
            if let Some(body) = &mut f.body {
                let mut d = Desugar { ptypes: &ptypes, generated: &mut generated, counter: &mut counter, errs: &mut errs };
                d.block(body);
            }
        }
    }
    m.items.extend(generated);
    errs
}

struct Desugar<'a> {
    ptypes: &'a HashMap<String, String>,
    generated: &'a mut Vec<Item>,
    counter: &'a mut u32,
    errs: &'a mut Vec<String>,
}

impl Desugar<'_> {
    fn block(&mut self, b: &mut Block) {
        for s in &mut b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &mut b.tail {
            self.expr(t);
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        match s {
            Stmt::Let { value: Some(e), .. } => self.expr(e),
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return(Some(e), _) => self.expr(e),
            Stmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::For { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        self.children(e);
        if let Expr::Call { callee, args, span } = e {
            if let Expr::Ident(name, _) = callee.as_ref() {
                let is_asm = name == "@asm";
                if (name == "@c" || is_asm) && !args.is_empty() {
                    if let Some(rep) = self.build(args, is_asm, *span) {
                        *e = rep;
                    }
                }
            }
        }
    }

    /// Recurse into the sub-expressions that can contain a nested `@c`/`@asm`.
    fn children(&mut self, e: &mut Expr) {
        match e {
            Expr::Unary { rhs, .. } => self.expr(rhs),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Call { callee, args, .. } => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Field { base, .. } => self.expr(base),
            Expr::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            Expr::If { cond, then, elifs, els, .. } => {
                self.expr(cond);
                self.block(then);
                for (c, b) in elifs {
                    self.expr(c);
                    self.block(b);
                }
                if let Some(b) = els {
                    self.block(b);
                }
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.expr(scrutinee);
                for (_, g, b) in arms {
                    if let Some(g) = g {
                        self.expr(g);
                    }
                    self.expr(b);
                }
            }
            Expr::Block(b) => self.block(b),
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::List(xs, _) => {
                for x in xs {
                    self.expr(x);
                }
            }
            Expr::MapLit(kvs, _) => {
                for (k, v) in kvs {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::Try { inner, .. } | Expr::Cast { inner, .. } | Expr::Comptime { inner, .. } => self.expr(inner),
            _ => {}
        }
    }

    /// Build the generated foreign item + the replacement call. `args[0]` is the code
    /// string; the rest are capture identifiers.
    fn build(&mut self, args: &[Expr], is_asm: bool, span: Span) -> Option<Expr> {
        let code = match args.first() {
            Some(Expr::Str(s, _)) => s.clone(),
            _ => {
                self.errs.push("@c/@asm: the first argument must be the code as a \"\"\"…\"\"\" string".into());
                return None;
            }
        };
        // captures: (name, vire type name, C type)
        let mut caps: Vec<(String, String)> = Vec::new();
        for a in &args[1..] {
            let nm = match a {
                Expr::Ident(n, _) => n.clone(),
                _ => {
                    self.errs.push("@c/@asm: captures must be plain variable names".into());
                    return None;
                }
            };
            let vty = match self.ptypes.get(&nm) {
                Some(t) => t.clone(),
                None => {
                    self.errs
                        .push(format!("@c/@asm: capture `{nm}` must be a parameter of the enclosing function (buffer capture not yet supported)"));
                    return None;
                }
            };
            let cty = match vty.as_str() {
                "Int" => "long",
                "Float" => "double",
                "Bool" => "long",
                other => {
                    self.errs
                        .push(format!("@c/@asm: capture `{nm}` has unsupported type `{other}` (scalar Int/Float/Bool only for now)"));
                    return None;
                }
            };
            caps.push((nm, cty.to_string()));
        }
        let n = *self.counter;
        *self.counter += 1;
        let fname = format!("__cblock_{n}");
        let sig = if caps.is_empty() {
            "void".to_string()
        } else {
            caps.iter().map(|(nm, cty)| format!("{cty} {nm}")).collect::<Vec<_>>().join(", ")
        };
        let (abi, gen_code) = if is_asm {
            ("asm".to_string(), format!(".globl {fname}\n{fname}:\n{code}\n"))
        } else {
            ("c".to_string(), format!("long {fname}({sig}) {{\n{code}\n}}\n"))
        };
        self.generated.push(Item::Native { abi, code: gen_code, links: vec![], span });
        // extern "C" declaration so Vire can call the generated function. It returns
        // Int; scalar captures keep their Vire types.
        let ty = |name: &str| Type { name: name.to_string(), args: vec![], borrowed: false, span };
        let ext_params: Vec<Param> = args[1..]
            .iter()
            .filter_map(|a| if let Expr::Ident(nm, _) = a { Some(nm.clone()) } else { None })
            .map(|nm| {
                let vty = self.ptypes.get(&nm).cloned().unwrap_or_else(|| "Int".into());
                Param { name: nm, ty: Some(ty(&vty)), default: None }
            })
            .collect();
        let ext_sig = FnSig { name: fname.clone(), generics: vec![], params: ext_params, ret: Some(ty("Int")), span };
        self.generated.push(Item::Extern { abi: "C".into(), items: vec![ext_sig], links: vec![], header: None, span });
        // Replacement: a plain call to the generated function with the captures.
        let call_args: Vec<Expr> = args[1..].to_vec();
        Some(Expr::Call { callee: Box::new(Expr::Ident(fname, span)), args: call_args, span })
    }
}
