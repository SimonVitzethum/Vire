//! Frontend-only analysis CLI for wasm (WASI) — the portable half of the Vire
//! compiler bundled in the VS Code extension. Reads the source from **stdin**,
//! takes the display filename as `argv[1]`, and prints the JSON analysis
//! (`{ diagnostics, symbols }`) — identical to the native `vire check --json`,
//! since both call `vire::analyze::analyze_json`. No LLVM backend, no CSolver,
//! no external tools, so it runs identically on Windows/macOS/Linux via Node's
//! built-in WASI.
//!
//! With no `--json` flag it prints plain `FILE:line:col: severity: message` lines
//! (like `vire check`) and exits 1 on error — handy for a CLI smoke test.

use std::io::Read;

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

    let out = vire::analyze::analyze_json(&src, &file);
    if json {
        println!("{out}");
        return;
    }
    // Plain-text mode: re-render the JSON payload as `FILE:line:col: sev: msg`.
    // (Cheap manual scan — avoids pulling in a JSON dependency.)
    let mut any_err = false;
    let diag_section = out.split("\"symbols\"").next().unwrap_or(&out);
    for entry in diag_section.split("{\"line\":").skip(1) {
        let field = |key: &str| -> Option<&str> {
            entry.split(&format!("\"{key}\":")).nth(1)
        };
        let line = entry.split(',').next().unwrap_or("1").trim();
        let col = field("col").and_then(|s| s.split(',').next()).unwrap_or("1").trim();
        let sev = field("severity").and_then(|s| s.split('"').nth(1)).unwrap_or("error");
        if sev == "error" {
            any_err = true;
        }
        if let Some(msg) = field("message").and_then(|s| s.split('"').nth(1)) {
            println!("{file}:{line}:{col}: {sev}: {msg}");
        }
    }
    std::process::exit(if any_err { 1 } else { 0 });
}
