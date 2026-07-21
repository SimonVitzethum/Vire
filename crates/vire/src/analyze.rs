//! Editor analysis: run the frontend (parse → desugar → infer → lower) over a
//! source string and produce a JSON `{ diagnostics, symbols }` for IDE features
//! (red squiggles, hover, go-to-definition, completion). Frontend-only — no LLVM
//! backend, no CSolver — so the SAME function powers both the native
//! `vire check --json` and the wasm build bundled in the VS Code extension.

use crate::ast::{FnSig, Item, Module, Type};
use crate::diag::{line_col, Level};

/// Analyze `src` and return a single-line JSON object:
/// `{"diagnostics":[{line,col,severity,message}],"symbols":[{name,kind,line,col,signature}]}`.
pub fn analyze_json(src: &str, _file: &str) -> String {
    let mut diags: Vec<(usize, usize, &'static str, String)> = Vec::new();
    // Per-expression inferred types (start line/col, end line/col, type name).
    let mut types: Vec<(usize, usize, usize, usize, &'static str)> = Vec::new();
    let push_span = |diags: &mut Vec<_>, level: &Level, span: crate::diag::Span, msg: &str| {
        let (line, col) = line_col(src, span.0);
        let sev = if *level == Level::Warning { "warning" } else { "error" };
        diags.push((line, col, sev, msg.to_string()));
    };
    let push_plain = |diags: &mut Vec<_>, msg: &str| diags.push((1, 1, "error", msg.to_string()));

    // Parse first — the AST (pre-desugar) is the basis for symbols.
    let (mut module, pdiags) = crate::parse(src);
    let symbols = collect_symbols(&module, src);
    let mut fatal = false;
    for d in &pdiags {
        if d.level == Level::Error {
            fatal = true;
        }
        push_span(&mut diags, &d.level, d.span, &d.msg);
    }

    if !fatal {
        let os = crate::platform::target_os(None);
        for e in crate::apply_platform_cfg(&mut module, os) {
            push_plain(&mut diags, &e);
        }
        for e in crate::desugar_cblocks(&mut module) {
            push_plain(&mut diags, &e);
        }
        let (spawn_errs, _) = crate::desugar_spawn(&mut module);
        for e in spawn_errs {
            push_plain(&mut diags, &e);
        }
        for e in crate::expand_item_macros(&mut module) {
            push_plain(&mut diags, &e);
        }
        if let Err(errs) = crate::expand_macros(&mut module) {
            for e in errs {
                push_plain(&mut diags, &e);
            }
        }
        for e in crate::derive_expand(&mut module) {
            push_plain(&mut diags, &e);
        }
        // Inference/lowering only make sense once expansion succeeded.
        if diags.iter().all(|(_, _, s, _)| *s != "error") {
            // Typed inference: conflicts become diagnostics, and every expression's
            // inferred type feeds editor hover.
            let (conflicts, exprtypes) = crate::infer_module_typed(&mut module);
            for e in conflicts {
                push_plain(&mut diags, &e);
            }
            for (span, ty) in exprtypes {
                let name = ty.name();
                if name == "?" || name == "Unit" {
                    continue; // no useful hover for unknown/void
                }
                let (sl, sc) = line_col(src, span.0);
                let (el, ec) = line_col(src, span.1);
                types.push((sl, sc, el, ec, name));
            }
            for e in crate::eval_comptime(&mut module) {
                push_plain(&mut diags, &e);
            }
            if let Err(errs) = crate::lower_module_src(&module, "") {
                for e in errs {
                    push_plain(&mut diags, &e);
                }
            }
        }
    }

    emit_json(&diags, &symbols, &types)
}

/// A top-level definition: name, kind, source location, display signature.
pub struct Symbol {
    pub name: String,
    pub kind: &'static str,
    pub line: usize,
    pub col: usize,
    pub signature: String,
}

/// Top-level functions/types/traits/consts with their location + display
/// signature — the basis for go-to-definition, hover, and completion.
pub fn collect_symbols(module: &Module, src: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    for it in &module.items {
        match it {
            Item::Fn(f) => {
                let (line, col) = line_col(src, f.sig.span.0);
                out.push(Symbol { name: f.sig.name.clone(), kind: "function", line, col, signature: fn_signature(&f.sig) });
            }
            Item::Type(t) => {
                let (line, col) = line_col(src, t.span.0);
                out.push(Symbol { name: t.name.clone(), kind: "type", line, col, signature: format!("type {}", t.name) });
            }
            Item::Trait(t) => {
                let (line, col) = line_col(src, t.span.0);
                out.push(Symbol { name: t.name.clone(), kind: "trait", line, col, signature: format!("trait {}", t.name) });
            }
            Item::Const { name, span, .. } => {
                let (line, col) = line_col(src, span.0);
                out.push(Symbol { name: name.clone(), kind: "const", line, col, signature: format!("const {name}") });
            }
            _ => {}
        }
    }
    out
}

fn render_type(t: &Type) -> String {
    let mut s = String::new();
    if t.borrowed {
        s.push('&');
    }
    s.push_str(&t.name);
    if !t.args.is_empty() {
        s.push('[');
        for (i, a) in t.args.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&render_type(a));
        }
        s.push(']');
    }
    s
}

fn fn_signature(sig: &FnSig) -> String {
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|p| match &p.ty {
            Some(t) => format!("{}: {}", p.name, render_type(t)),
            None => p.name.clone(),
        })
        .collect();
    let ret = sig.ret.as_ref().map(|t| format!(" -> {}", render_type(t))).unwrap_or_default();
    format!("fn {}({}){ret}", sig.name, params.join(", "))
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn emit_json(diags: &[(usize, usize, &'static str, String)], symbols: &[Symbol], types: &[(usize, usize, usize, usize, &'static str)]) -> String {
    let mut s = String::from("{\"diagnostics\":[");
    for (i, (line, col, sev, msg)) in diags.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{{\"line\":{line},\"col\":{col},\"severity\":\"{sev}\",\"message\":\"{}\"}}", json_escape(msg)));
    }
    s.push_str("],\"symbols\":[");
    for (i, sym) in symbols.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"name\":\"{}\",\"kind\":\"{}\",\"line\":{},\"col\":{},\"signature\":\"{}\"}}",
            json_escape(&sym.name), sym.kind, sym.line, sym.col, json_escape(&sym.signature)
        ));
    }
    s.push_str("],\"types\":[");
    for (i, (sl, sc, el, ec, name)) in types.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{{\"sl\":{sl},\"sc\":{sc},\"el\":{el},\"ec\":{ec},\"type\":\"{name}\"}}"));
    }
    s.push_str("]}");
    s
}
