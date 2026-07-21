//! Frontend-only analysis CLI for wasm (WASI) — the portable half of the Vire
//! compiler bundled in the VS Code extension. Reads the source from **stdin**,
//! takes the display filename as `argv[1]`. No LLVM backend, no CSolver, no
//! external tools — so it runs identically on Windows/macOS/Linux via Node's
//! built-in WASI.
//!
//! Two output modes:
//!   - default: diagnostics as `FILE:line:col: severity: message` (like
//!     `vire check`); exit 1 if any error.
//!   - `--json`: a single JSON object `{ "diagnostics": [...], "symbols": [...] }`
//!     for the editor's diagnostics + hover + go-to-definition. Always exit 0.
//!
//! Mirrors the native `vire check` pipeline (parse → desugar → infer → lower).

use std::io::Read;

use vire::ast::{Item, Type};
use vire::diag::{line_col, Level};

fn main() {
    let file = std::env::args().nth(1).unwrap_or_else(|| "<stdin>".to_string());
    let json = std::env::args().any(|a| a == "--json");
    let mut src = String::new();
    if std::io::stdin().read_to_string(&mut src).is_err() {
        if json {
            println!("{{\"diagnostics\":[{{\"line\":1,\"col\":1,\"severity\":\"error\",\"message\":\"could not read source\"}}],\"symbols\":[]}}");
        } else {
            println!("{file}:1:1: error: could not read source from stdin");
        }
        std::process::exit(if json { 0 } else { 1 });
    }

    let mut diags: Vec<(usize, usize, &'static str, String)> = Vec::new();
    let push_span = |diags: &mut Vec<_>, level: &Level, span: vire::diag::Span, msg: &str| {
        let (line, col) = line_col(&src, span.0);
        let sev = if *level == Level::Warning { "warning" } else { "error" };
        diags.push((line, col, sev, msg.to_string()));
    };
    let push_plain = |diags: &mut Vec<_>, msg: &str| diags.push((1, 1, "error", msg.to_string()));

    // Parse first — the AST (before desugaring) is the basis for symbols.
    let (mut module, pdiags) = vire::parse(&src);
    let symbols = collect_symbols(&module, &src);
    let mut fatal = false;
    for d in &pdiags {
        if d.level == Level::Error {
            fatal = true;
        }
        push_span(&mut diags, &d.level, d.span, &d.msg);
    }

    // Only run the later stages when parsing produced a usable AST.
    if !fatal {
        let os = vire::platform::target_os(None);
        for e in vire::apply_platform_cfg(&mut module, os) {
            push_plain(&mut diags, &e);
        }
        for e in vire::desugar_cblocks(&mut module) {
            push_plain(&mut diags, &e);
        }
        let (spawn_errs, _) = vire::desugar_spawn(&mut module);
        for e in spawn_errs {
            push_plain(&mut diags, &e);
        }
        for e in vire::expand_item_macros(&mut module) {
            push_plain(&mut diags, &e);
        }
        if let Err(errs) = vire::expand_macros(&mut module) {
            for e in errs {
                push_plain(&mut diags, &e);
            }
        }
        for e in vire::derive_expand(&mut module) {
            push_plain(&mut diags, &e);
        }
        // Inference/lowering only make sense once expansion succeeded.
        if diags.iter().all(|(_, _, s, _)| *s != "error") {
            for e in vire::infer_module(&mut module) {
                push_plain(&mut diags, &e);
            }
            for e in vire::eval_comptime(&mut module) {
                push_plain(&mut diags, &e);
            }
            if let Err(errs) = vire::lower_module_src(&module, "") {
                for e in errs {
                    push_plain(&mut diags, &e);
                }
            }
        }
    }

    if json {
        emit_json(&diags, &symbols);
    } else {
        for (line, col, sev, msg) in &diags {
            println!("{file}:{line}:{col}: {sev}: {msg}");
        }
        std::process::exit(if diags.iter().any(|(_, _, s, _)| *s == "error") { 1 } else { 0 });
    }
}

struct Symbol {
    name: String,
    kind: &'static str,
    line: usize,
    col: usize,
    signature: String,
}

/// Top-level definitions (functions, types, traits, consts) with their source
/// location and a display signature — the basis for go-to-definition + hover.
fn collect_symbols(module: &vire::ast::Module, src: &str) -> Vec<Symbol> {
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

fn fn_signature(sig: &vire::ast::FnSig) -> String {
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

fn emit_json(diags: &[(usize, usize, &'static str, String)], symbols: &[Symbol]) {
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
    s.push_str("]}");
    println!("{s}");
}
