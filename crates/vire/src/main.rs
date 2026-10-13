//! `vire` — compiler driver.
//! Usage: `vire parse DATEI.vr` | `vire lex DATEI.vr` |
//!         `vire build [-o BIN] [--emit-ir|--emit-llvm] DATEI.vr` |
//!         `vire run DATEI.vr`.
//! `build`/`run` lower the AST to `crates/ir` and use the same
//! solver + LLVM backend + runtime as the Java driver (fastjavac).

use std::path::PathBuf;
use std::process::{exit, Command};

mod cverify;

// The minimal C support source (crates/runtime/runtime.c) — entry point,
// print helpers. No GC, no RC, no collector (AGENTS.md §2).
const RUNTIME_C: &str = include_str!("../../runtime/runtime.c");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: vire (parse|lex|build|run) FILE.vr");
        exit(2);
    }
    if args[0] == "build" || args[0] == "run" {
        build_or_run(&args);
        return;
    }
    if args[0] == "bindgen" {
        bindgen(&args[1..]);
        return;
    }
    if args[0] == "audit" {
        audit(&args[1..]);
        return;
    }
    if args[0] == "check" {
        check(&args[1..]);
        return;
    }
    if args[0] == "fmt" {
        fmt_cmd(&args[1..]);
        return;
    }
    if args.len() < 2 {
        eprintln!("Usage: vire (parse|lex) FILE.vr");
        exit(2);
    }
    let cmd = &args[0];
    let path = &args[1];
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            exit(1);
        }
    };

    match cmd.as_str() {
        "lex" => {
            let (toks, diags) = vire::lexer::lex(&src);
            for d in &diags {
                eprintln!("{}", d.render(&src));
            }
            for t in &toks {
                println!("{:?}", t.tok);
            }
            if !diags.is_empty() {
                exit(1);
            }
        }
        "parse" => {
            let (module, diags) = vire::parse(&src);
            for d in &diags {
                eprintln!("{}", d.render(&src));
            }
            println!("{:#?}", module);
            eprintln!(
                "{} item(s), {} diagnostic(s)",
                module.items.len(),
                diags.len()
            );
            if !diags.is_empty() {
                exit(1);
            }
        }
        "infer" => {
            // Dump the typed AST: the inferred type of every expression, keyed by
            // source span (Phase 1 of the compile-time programming layer). Proves
            // that per-expression types survive inference as a persisted table.
            let (mut module, diags) = vire::parse(&src);
            for d in &diags {
                eprintln!("{}", d.render(&src));
            }
            if !diags.is_empty() {
                exit(1);
            }
            if let Err(es) = vire::expand_macros(&mut module) {
                for e in es {
                    eprintln!("macro: {e}");
                }
                exit(1);
            }
            let (conflicts, types) = vire::infer_module_typed(&mut module);
            for c in &conflicts {
                eprintln!("{}", c.render(&src));
            }
            let mut rows: Vec<(vire::diag::Span, vire::InferTy)> = types.into_iter().collect();
            rows.sort_by_key(|(s, _)| *s);
            for (span, ty) in rows {
                let (line, col) = vire::diag::line_col(&src, span.0);
                let snippet = src.get(span.0..span.1).unwrap_or("").replace('\n', " ");
                let snippet: String = snippet.chars().take(32).collect();
                println!("{line}:{col}\t{}\t{snippet}", ty.name());
            }
        }
        "types" => {
            // Introspect the persisted, source-level type graph (the foundation of
            // the compile-time programming layer). Runs the front-end up to and
            // including inference, then prints the structural view of all types,
            // traits, impls, and functions.
            let (mut module, diags) = vire::parse(&src);
            for d in &diags {
                eprintln!("{}", d.render(&src));
            }
            if !diags.is_empty() {
                exit(1);
            }
            for e in vire::expand_item_macros(&mut module) {
                eprintln!("{}", e.render(&src));
            }
            if let Err(es) = vire::expand_macros(&mut module) {
                for e in es {
                    eprintln!("macro: {e}");
                }
                exit(1);
            }
            for e in vire::derive_expand(&mut module) {
                eprintln!("derive: {e}");
            }
            vire::infer_module(&mut module);
            let graph = vire::TypeGraph::build(&module);
            print!("{}", graph.dump());
        }
        other => {
            eprintln!("unknown command: {other} (parse|lex|types|infer|build|run)");
            exit(2);
        }
    }
}

/// Map a Vire `@assume:` name to the CSolver assumption flag it authorizes. These are
/// the irreducible hardware/framework invariants no software proof can discharge; each
/// is unsound in general and is why it must be written down and audited.
fn assume_flag(name: &str) -> Option<&'static str> {
    match name {
        "mmio" => Some("--assume-valid-mmio"),
        "field_invariants" | "field" => Some("--assume-field-invariants"),
        "valid_returns" | "returns" => Some("--assume-valid-returns"),
        "loop_ptrs" | "loop" => Some("--assume-valid-loop-ptrs"),
        "struct_tail" => Some("--assume-struct-tail"),
        _ => None,
    }
}

/// Parse `@assume: <name> [justification]` directives out of an inline block's code
/// (they are ordinary C/asm comments, copied verbatim into the generated block).
/// Returns (name, justification) pairs in source order.
fn parse_assumes(code: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in code.lines() {
        if let Some(rest) = line.split("@assume:").nth(1) {
            // strip a trailing `*/` if it was a block comment.
            let rest = rest.trim().trim_end_matches("*/").trim();
            let mut it = rest.splitn(2, char::is_whitespace);
            let name = it.next().unwrap_or("").trim().to_string();
            let just = it.next().unwrap_or("").trim().trim_matches('"').to_string();
            if !name.is_empty() {
                out.push((name, just));
            }
        }
    }
    out
}

/// Auto-synthesize a CSolver `--pre` contract from the C block's function signatures:
/// an adjacent `(T* ptr, intN len)` parameter pair (the C `(buf, len)` idiom) becomes
/// `<fn> <ptr#> elements <len#> <elem-bytes>` — a PROVEN bound, not a blanket trust.
/// This is the contract a typed Vire caller supplies (a Vire array is a proven
/// (ptr, len)). Returns the contract text (one line per pair) or None. Heuristic +
/// prove-only (refutable=false), so it never introduces a false FAIL; the remaining
/// pointers stay under `assume_valid_params`.
fn synthesize_pre(code: &str) -> Option<String> {
    fn elem_bytes(ptr_ty: &str) -> u32 {
        let t = ptr_ty.replace('*', " ");
        let t = t.split_whitespace().filter(|w| *w != "const").collect::<Vec<_>>().join(" ");
        if t.contains("char") || t.contains("int8") || t.contains("uint8") {
            1
        } else if t.contains("short") || t.contains("int16") {
            2
        } else if t.contains("long long") || t.contains("int64") || t.contains("double") || t.contains("size_t") {
            8
        } else if t.contains("long") {
            8
        } else {
            4 // int / float / default
        }
    }
    fn is_int_param(p: &str) -> bool {
        !p.contains('*')
            && ["int", "long", "short", "size_t", "unsigned", "int32", "int64", "size"].iter().any(|k| p.contains(k))
    }
    let mut lines = Vec::new();
    let bytes = code.as_bytes();
    let mut idx = 0;
    while let Some(open) = code[idx..].find('(') {
        let open = idx + open;
        // Function name = identifier immediately before '('.
        let mut ns = open;
        while ns > 0 && (bytes[ns - 1].is_ascii_alphanumeric() || bytes[ns - 1] == b'_') {
            ns -= 1;
        }
        let name = &code[ns..open];
        let Some(close_rel) = code[open..].find(')') else { break };
        let close = open + close_rel;
        // Only real definitions: a '{' should follow the ')' (skip whitespace).
        let after = code[close + 1..].trim_start();
        idx = close + 1;
        if name.is_empty() || !after.starts_with('{') {
            continue;
        }
        let params: Vec<&str> = code[open + 1..close].split(',').map(|s| s.trim()).filter(|s| !s.is_empty() && *s != "void").collect();
        for (i, p) in params.iter().enumerate() {
            if p.contains('*') && i + 1 < params.len() && is_int_param(params[i + 1]) {
                lines.push(format!("{name} {i} elements {} {}", i + 1, elem_bytes(p)));
            }
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n") + "\n")
    }
}

/// Parse the `@assume:` directives of a block into the CSolver assumption set. An
/// unknown name is an error (no silent trust).
fn assume_set(code: &str) -> Result<cverify::Assume, String> {
    let mut a = cverify::Assume::default();
    for (name, _just) in parse_assumes(code) {
        match name.as_str() {
            "mmio" => a.valid_mmio = true,
            "field_invariants" | "field" => a.field_invariants = true,
            "valid_returns" | "returns" => a.valid_returns = true,
            "loop_ptrs" | "loop" => a.valid_loop_ptrs = true,
            "struct_tail" => a.struct_tail = true,
            other => {
                return Err(format!(
                    "    unknown @assume `{other}` (known: mmio, field_invariants, valid_returns, loop_ptrs, struct_tail)"
                ))
            }
        }
    }
    Ok(a)
}

/// Gate a `native "c"`/`native "asm"` block through the CSolver memory-safety verifier.
/// C is compiled to LLVM IR (`clang -O0 -emit-llvm`); assembly (`ext == "s"`) is verified
/// directly (CSolver decodes x86-64/AArch64). Returns Ok only on a proven-safe verdict
/// (exit 0 = PASS); otherwise Err with the residual obligations / counterexample.
/// Content-addressed verification cache. A block's proof is expensive (CDCL SAT +
/// symbolic execution); an unchanged block need not be re-verified. Key = hash of the
/// block kind + its exact source (the contract/assumptions are derived from it). Only
/// PASS verdicts are cached (a rejection fails the build anyway). Bump VERIFY_CACHE_TAG
/// when the verifier changes so stale PASSes are not reused.
const VERIFY_CACHE_TAG: &str = "v1";

fn cache_key(ext: &str, code: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    VERIFY_CACHE_TAG.hash(&mut h);
    ext.hash(&mut h);
    code.hash(&mut h);
    h.finish()
}

fn cache_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("vire");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("verify-cache"))
}

fn cache_has(key: u64) -> bool {
    let Some(f) = cache_file() else { return false };
    let hex = format!("{key:016x}");
    std::fs::read_to_string(f).map(|s| s.lines().any(|l| l == hex)).unwrap_or(false)
}

fn cache_add(key: u64) {
    if let Some(f) = cache_file() {
        use std::io::Write;
        if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
            let _ = writeln!(fh, "{key:016x}");
        }
    }
}

/// Precompile `runtime.c` to LLVM bitcode once per (content, `-D` flags, target,
/// clang version) and cache it. The runtime is identical every build, yet its
/// `-O2 -flto -c` bitcode generation is ~80% of a small build's wall time. The LTO
/// link consumes this cached bitcode EXACTLY as if it had compiled `runtime.c`
/// inline — same bitcode in, same link-time optimization out — so this is lossless.
/// Returns the cached object, or `None` to fall back to compiling from source (any
/// error, e.g. a stale/unwritable cache, is silently non-fatal).
fn cached_runtime_object(threads: bool, backtrace: bool, no_cycles: bool, no_rc: bool, thin_lto: bool, target: Option<&str>) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("vire");
    std::fs::create_dir_all(&dir).ok()?;
    // A clang upgrade must not reuse stale bitcode → fold its version into the key.
    let clangver = Command::new("clang")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string())
        .unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "rtbc-v1".hash(&mut h);
    RUNTIME_C.hash(&mut h);
    (threads, backtrace, no_cycles, no_rc, thin_lto).hash(&mut h);
    target.unwrap_or("native").hash(&mut h);
    clangver.hash(&mut h);
    let key = h.finish();
    let obj = dir.join(format!("runtime-{key:016x}.o"));
    if obj.exists() {
        return Some(obj);
    }
    // Cache miss: compile runtime.c → bitcode object with the matching flags.
    let rt_src = dir.join("runtime-src.c");
    std::fs::write(&rt_src, RUNTIME_C).ok()?;
    let mut c = Command::new("clang");
    c.arg("-O2").arg(if thin_lto { "-flto=thin" } else { "-flto" }).arg("-c").arg(&rt_src);
    c.args(["-ffunction-sections", "-fdata-sections"]);
    if threads {
        c.arg("-DFASTLLVM_THREADS").arg("-pthread");
    }
    if backtrace {
        c.arg("-DFASTLLVM_BACKTRACE");
    }
    if no_cycles {
        c.arg("-DFASTLLVM_NO_CYCLES");
    }
    if no_rc {
        c.arg("-DFASTLLVM_NO_RC");
    }
    if let Some(t) = target {
        c.arg("-target").arg(t);
    }
    // Atomic publish: compile to a temp then rename, so a concurrent build never
    // observes a half-written object.
    let tmp = dir.join(format!("runtime-{key:016x}.{}.tmp.o", std::process::id()));
    let ok = c.arg("-o").arg(&tmp).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    std::fs::rename(&tmp, &obj).ok()?;
    Some(obj)
}

fn verify_native_block(path: &std::path::Path, code: &str, ext: &str, build_dir: &std::path::Path, i: usize) -> Result<bool, String> {
    let assume = assume_set(code)?;
    let key = cache_key(ext, code);
    if cache_has(key) {
        return Ok(true); // cached PASS — skip re-verification
    }
    let outcome = if ext == "s" {
        // Assembly is CSolver's native input — verify the .s text directly.
        let src = std::fs::read_to_string(path).map_err(|e| format!("reading asm block: {e}"))?;
        cverify::verify_asm(&src, &assume)
    } else {
        // C → LLVM IR via clang, then verify the IR. Auto-synthesized (ptr,len)→elements
        // contracts give PROVEN buffer bounds; assume_valid_params covers the rest.
        let ll = build_dir.join(format!("native_{i}.verify.ll"));
        let clang = Command::new("clang")
            .args(["-O0", "-S", "-emit-llvm", "-g", "-o"])
            .arg(&ll)
            .arg(path)
            .output()
            .map_err(|e| format!("clang not runnable for verification: {e}"))?;
        if !clang.status.success() {
            return Err(format!("the C block does not compile:\n{}", String::from_utf8_lossy(&clang.stderr)));
        }
        let src = std::fs::read_to_string(&ll).map_err(|e| format!("reading lowered IR: {e}"))?;
        let pre = synthesize_pre(code);
        cverify::verify_llvm(&src, &format!("native_{i}"), pre.as_deref(), &assume)
    };
    match outcome {
        cverify::Outcome::Pass => {
            cache_add(key);
            Ok(false)
        }
        cverify::Outcome::Rejected(report) => Err(report),
    }
}

/// Verify all native `@c`/`@asm` blocks, in PARALLEL when there is more than one.
/// Each proof is independent — a separate `clang -emit-llvm` on its own
/// `native_{i}.verify.ll` plus a functional CSolver run, with only the atomic,
/// content-addressed PASS cache shared — so they run on a bounded worker pool
/// (≤ CPU count). Results are reported in block order and the build exits on the
/// FIRST rejection, so the diagnostics are deterministic regardless of scheduling.
fn verify_blocks_parallel(jobs: &[(usize, PathBuf, String, &'static str, String)], build_dir: &std::path::Path) {
    if jobs.is_empty() {
        return;
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    let n = jobs.len();
    let results: Vec<Mutex<Option<Result<bool, String>>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let cursor = AtomicUsize::new(0);
    let nthreads = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4).min(n);
    std::thread::scope(|s| {
        for _ in 0..nthreads {
            s.spawn(|| loop {
                let idx = cursor.fetch_add(1, Ordering::Relaxed);
                if idx >= n {
                    break;
                }
                let (i, path, code, ext, _abi) = &jobs[idx];
                let r = verify_native_block(path, code, ext, build_dir, *i);
                *results[idx].lock().unwrap() = Some(r);
            });
        }
    });
    for (idx, (i, _p, _c, _e, abi)) in jobs.iter().enumerate() {
        match results[idx].lock().unwrap().take().expect("verify job not run") {
            Ok(cached) => eprintln!(
                "verify: native \"{abi}\" block {i}: PASS (proven memory-safe{})",
                if cached { ", cached" } else { "" }
            ),
            Err(report) => {
                eprintln!(
                    "error: native \"{abi}\" block {i} is not provably memory-safe \
                     (rejected instead of trusted like `unsafe`):\n{report}\n       \
                     Close the proof with a contract/`@assume` (E410: there is no opt-out)."
                );
                exit(1);
            }
        }
    }
}

/// Python include path + lib name via `python3`/sysconfig (for `native "python"`).
fn python_config() -> Option<(String, String)> {
    let out = Command::new("python3")
        .args(["-c", "import sysconfig;print(sysconfig.get_config_var('INCLUDEPY'));print(sysconfig.get_config_var('LDVERSION'))"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let inc = lines.next()?.trim().to_string();
    let ver = lines.next()?.trim().to_string();
    Some((inc, format!("python{ver}")))
}

/// Loads a `vire.syntax` file next to the source (if present) → user-
/// defined keyword spellings. If it is missing, the default syntax applies.
/// Resolve a custom-syntax config **only when explicitly requested** — never the old
/// silent folder-wide auto-load (which broke every standard-keyword `.vr` that merely
/// sat next to a remapping `vire.syntax`). Two opt-ins, in priority order:
///   1. `--syntax FILE` on the command line (`explicit`), resolved relative to the cwd;
///   2. an in-file directive on an early comment line: `//!syntax: FILE` (or
///      `// syntax: FILE`), resolved relative to the source file's directory.
/// Absent both → default (standard) keywords. A requested-but-unreadable/invalid config
/// is a hard error (never a silent fallback).
/// `vire check FILE.vr` — front-end only (parse → desugar → infer → lower),
/// printing each diagnostic as `FILE:line:col: severity: message` for editor
/// integration. Lex/parse diagnostics carry precise spans; stage errors without a
/// span are reported at 1:1. Prints nothing and exits 0 when the file is clean.
/// `vire fmt FILE.vr` — print the canonically-formatted source to stdout.
/// `vire fmt -i FILE.vr` rewrites the file in place. A parse error is fatal (a
/// malformed file is left untouched). The formatter round-trips: `fmt(fmt(x))`
/// equals `fmt(x)`, so it doubles as parser-fuzz insurance.
fn fmt_cmd(args: &[String]) {
    let in_place = args.iter().any(|a| a == "-i" || a == "--in-place");
    let Some(path) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("Usage: vire fmt [-i] FILE.vr");
        exit(2);
    };
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            exit(1);
        }
    };
    let syntax = load_syntax(&src, path, None);
    let (module, diags) = vire::parse_with_syntax(&src, syntax);
    let errs: Vec<_> = diags.iter().filter(|d| d.level == vire::diag::Level::Error).collect();
    if !errs.is_empty() {
        for d in &errs {
            eprintln!("{}", d.render(&src));
        }
        eprintln!("fmt: not formatting (parse errors above)");
        exit(1);
    }
    let out = vire::format_module(&module);
    if in_place {
        if let Err(e) = std::fs::write(path, &out) {
            eprintln!("{path}: {e}");
            exit(1);
        }
    } else {
        print!("{out}");
    }
}

fn check(args: &[String]) {
    // Optional `--hw-profile FILE` (same as build): without it every
    // `Mmio.bind` reports E400 (addresses never come from user integers).
    let hw_profile: Vec<(String, u64, u64)> = args
        .iter()
        .position(|a| a == "--hw-profile")
        .and_then(|i| args.get(i + 1))
        .map(|f| load_hw_profile(f))
        .unwrap_or_default();
    let Some(path) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("Usage: vire check FILE.vr");
        exit(2);
    };
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            println!("{path}:1:1: error: {e}");
            exit(1);
        }
    };
    // `--json`: emit `{diagnostics, symbols}` for the editor (same output as the
    // wasm frontend). Always exit 0 so the editor reads the payload.
    if args.iter().any(|a| a == "--json") {
        println!("{}", vire::analyze::analyze_json(&src, path));
        return;
    }
    let emit_span = |level: &vire::diag::Level, span: vire::diag::Span, msg: &str| {
        let (line, col) = vire::diag::line_col(&src, span.0);
        let sev = match level {
            vire::diag::Level::Error => "error",
            vire::diag::Level::Warning => "warning",
        };
        println!("{path}:{line}:{col}: {sev}: {msg}");
    };
    let emit_plain = |msg: &str| println!("{path}:1:1: error: {msg}");
    let mut had_error = false;

    // Parse (default or in-file `//!syntax:` grammar).
    let syntax = load_syntax(&src, path, None);
    let (mut module, diags) = vire::parse_with_syntax(&src, syntax);
    for d in &diags {
        if d.level == vire::diag::Level::Error {
            had_error = true;
        }
        emit_span(&d.level, d.span, &d.msg);
    }
    if had_error {
        exit(1);
    }
    // Desugars + expansions, in the same order as `build`.
    let os = vire::platform::target_os(None);
    for e in vire::apply_platform_cfg(&mut module, os) {
        had_error = true;
        emit_plain(&e);
    }
    for e in vire::desugar_cblocks(&mut module) {
        had_error = true;
        emit_plain(&e);
    }
    let (spawn_errs, _) = vire::desugar_spawn(&mut module);
    for e in spawn_errs {
        had_error = true;
        emit_plain(&e);
    }
    for e in vire::expand_item_macros(&mut module) {
        had_error = true;
        emit_span(&e.level, e.span, &e.msg);
    }
    if let Err(errs) = vire::expand_macros(&mut module) {
        for e in errs {
            had_error = true;
            emit_plain(&e);
        }
    }
    for e in vire::derive_expand(&mut module) {
        had_error = true;
        emit_plain(&e);
    }
    if had_error {
        exit(1);
    }
    // Inference → comptime → lowering (the errors editors most want to see).
    for e in vire::infer_module(&mut module) {
        had_error = true;
        emit_span(&e.level, e.span, &e.msg);
    }
    for e in vire::eval_comptime(&mut module) {
        had_error = true;
        emit_plain(&e);
    }
    if let Err(errs) = vire::lower_module_src(&module, "", &hw_profile) {
        for e in errs {
            had_error = true;
            emit_plain(&e);
        }
    }
    if had_error {
        exit(1);
    }
}

/// Load a hardware profile for `Mmio.bind` (language/HW-SAFE.md §2): lines
/// `name base len` (`#` comments, decimal or `0x`-hex). A malformed line is a
/// hard error — silently skipping it would turn an address typo into E400
/// confusion (or worse, a wrong mapping).
fn load_hw_profile(path: &str) -> Vec<(String, u64, u64)> {
    fn num(s: &str) -> Option<u64> {
        let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).map(|h| (h, 16)).unwrap_or((s, 10));
        u64::from_str_radix(s.0, s.1).ok()
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("--hw-profile {path}: {e}");
            exit(1);
        }
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            [name, base, len] => match (num(base), num(len)) {
                (Some(b), Some(l)) if l > 0 => out.push((name.to_string(), b, l)),
                _ => {
                    eprintln!("--hw-profile {path}:{}: bad base/len in `{line}` (need `name base len`, numbers decimal or 0x-hex, len > 0)", i + 1);
                    exit(1);
                }
            },
            _ => {
                eprintln!("--hw-profile {path}:{}: bad line `{line}` (need `name base len`)", i + 1);
                exit(1);
            }
        }
    }
    out
}

fn load_syntax(src: &str, src_path: &str, explicit: Option<&str>) -> vire::Syntax {
    let src_dir = std::path::Path::new(src_path).parent().unwrap_or_else(|| std::path::Path::new("."));
    let cfg = if let Some(f) = explicit {
        std::path::PathBuf::from(f)
    } else if let Some(rel) = syntax_directive(src) {
        src_dir.join(rel)
    } else {
        return vire::Syntax::default();
    };
    let text = match std::fs::read_to_string(&cfg) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("syntax config {}: {e}", cfg.display());
            exit(1);
        }
    };
    match vire::Syntax::parse(&text) {
        Ok(s) => {
            eprintln!("vire: loaded syntax config {}", cfg.display());
            s
        }
        Err(errs) => {
            for e in &errs {
                eprintln!("{}: {e}", cfg.display());
            }
            exit(1);
        }
    }
}

/// Scan the first few comment lines for an opt-in syntax directive
/// `//!syntax: FILE` / `// syntax: FILE`, returning the FILE. Only comment lines and
/// blank lines are scanned (the directive must precede real code), so it never triggers
/// on incidental text.
fn syntax_directive(src: &str) -> Option<String> {
    for line in src.lines().take(10) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix("//") {
            let rest = rest.trim_start_matches('!').trim();
            if let Some(file) = rest.strip_prefix("syntax:") {
                return Some(file.trim().trim_matches('"').to_string());
            }
            continue; // other comment → keep scanning
        }
        break; // first non-comment, non-blank line → stop
    }
    None
}

/// `vire audit FILE.vr`: list every `@assume` in the program's inline C/asm blocks —
/// the complete, named trust boundary. Everything NOT listed here is machine-proven;
/// each entry is a hardware/framework invariant the proof rests on but cannot discharge.
fn audit(args: &[String]) {
    let Some(path) = args.first() else {
        eprintln!("Usage: vire audit FILE.vr");
        exit(2);
    };
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            exit(1);
        }
    };
    let (mut module, _diags) = vire::parse(&src);
    let _ = vire::desugar_cblocks(&mut module);
    let mut total_blocks = 0usize;
    let mut total_assumes = 0usize;
    println!("Trust audit — {path}");
    println!("Every inline block below is memory-verified; the @assume lines are the ONLY");
    println!("facts trusted without proof.\n");
    for it in &module.items {
        if let vire::ast::Item::Native { abi, code, .. } = it {
            let a = abi.to_ascii_lowercase();
            if a != "c" && a != "asm" && a != "s" && a != "assembly" {
                continue;
            }
            total_blocks += 1;
            let assumes = parse_assumes(code);
            let label = code.lines().find(|l| l.contains("__cblock")).map(|l| l.trim()).unwrap_or("<inline block>");
            if assumes.is_empty() {
                println!("  ✓ {label}\n      fully proven — no assumptions");
            } else {
                println!("  ⚠ {label}");
                for (name, just) in &assumes {
                    total_assumes += 1;
                    let flag = assume_flag(name).unwrap_or("<UNKNOWN>");
                    let why = if just.is_empty() { "(no justification given)" } else { just.as_str() };
                    println!("      @assume {name} [{flag}]  —  {why}");
                }
            }
        }
    }
    println!("\n{total_blocks} inline block(s), {total_assumes} assumption(s) at the trust boundary.");
    if total_assumes == 0 && total_blocks > 0 {
        println!("No unproven assumptions — every inline block is fully machine-verified.");
    }
}

/// `vire build`/`run`: .vr → AST → IR (lowering) → solver → LLVM → clang → binary.
/// `run` executes the binary afterwards and passes through its exit code.
fn build_or_run(args: &[String]) {
    let is_run = args[0] == "run";
    let mut out: Option<PathBuf> = None;
    let mut emit_ir = false;
    let mut emit_llvm = false;
    // -O0: clang optimization/LTO off. For honest RC/heap MEASUREMENTS — otherwise
    // `-O2 -flto` eliminates dead allocation/release pairs (the objects are
    // optimized away, the runtime counters stay 0). The solver always runs.
    let mut opt0 = false;
    let mut force_no_cycles = false;
    let mut force_no_rc = false;
    // PGO (Profile-Guided Optimization): the honest addition to static AOT for
    // data-dependent hotness that the estimate does not see. `--pgo-gen` builds an
    // instrumented binary (writes a profile at run time), `--pgo-use DIR` builds
    // with the collected profile. Two phases: gen → representative run → use.
    let mut pgo_gen = false;
    let mut pgo_use: Option<String> = None;
    // Cross-compile: `--target <triple>` passes `-target` through to clang (the emitted
    // IR is triple-agnostic → portable). Linux/BSD/macOS = POSIX runtime directly;
    // Windows needs the runtime shims (aligned_alloc/pthread, see runtime.c).
    let mut target: Option<String> = None;
    // Scaling large programs: ThinLTO instead of full LTO (parallel, far less
    // memory/time at millions of lines — full LTO is the whole-program bottleneck).
    let mut thin_lto = false;
    // FFI: additional libraries (`-l NAME`) and objects/sources (`--obj FILE`,
    // .c/.cpp/.o/.a) to link — for C/C++/Python interop.
    let mut link_libs: Vec<String> = Vec::new();
    let mut link_objs: Vec<String> = Vec::new();
    let mut path: Option<String> = None;
    // Memory-safety verification of `native "c"`/`native "asm"` blocks is
    // MANDATORY (the sound alternative to a blind `unsafe`): each block is proven safe by
    // the vendored CSolver verifier (linked in — no external binary) or it is a compile
    // error (E410). There is no opt-out (AGENTS.md §3).
    let mut threads_flag = false;
    let mut backtrace_flag = false;
    let mut debug_flag = false;
    // Build-system interop (Meson / C-ABI object files). `--emit=obj` lowers the whole
    // `.vr` program to ONE relocatable object exposing C-ABI symbols (the runtime `main`
    // included) that Meson/ld links with other objects; `--emit=staticlib` wraps it in a
    // `.a`; `--emit=asm` emits the program IR as assembly. `--deps FILE` writes a
    // Makefile/Ninja depfile (Meson `depfile:`); `-I DIR` forwards include paths;
    // `--pkg NAME` pulls cflags+libs from pkg-config (first-class dependency consumption).
    let mut emit_obj = false;
    let mut emit_asm = false;
    let mut emit_staticlib = false;
    let mut emit_module = false;
    let mut deps_file: Option<String> = None;
    let mut include_dirs: Vec<String> = Vec::new();
    let mut pkgs: Vec<String> = Vec::new();
    // Header files pulled in by `extern "C" header "…"` — recorded as build dependencies
    // for the `--deps` depfile (so Meson/Ninja rebuild when a header changes).
    let mut header_deps: Vec<String> = Vec::new();
    // Custom keyword syntax is opt-in only (`--syntax FILE`), never folder-auto-loaded.
    let mut syntax_file: Option<String> = None;
    let mut hw_profile_file: Option<String> = None;
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => match it.next() {
                Some(p) => out = Some(PathBuf::from(p)),
                None => {
                    eprintln!("-o needs an argument");
                    exit(2);
                }
            },
            // REMOVED (AGENTS.md §3, E410): `--noverify`/`--unsafe-c`/`--no-verify-c`
            // disabled the memory-safety proof for native blocks. There is no opt-out;
            // use a contract/`@assume` or remove the block.
            "--noverify" | "--unsafe-c" | "--no-verify-c" => {
                eprintln!("error[E410]: `--noverify`/`--unsafe-c` is removed — native blocks must verify (AGENTS.md §3). Close the proof with a contract/`@assume`.");
                exit(2);
            }
            // Force the threads runtime even without `spawn` (atomic RC + pthreads).
            // `spawn` enables it automatically; this is for explicit control.
            "--threads" => threads_flag = true,
            // Debug: print a native backtrace on an uncaught exception / hard crash
            // (off by default → zero overhead). Needs -rdynamic for symbol names.
            "--backtrace" => backtrace_flag = true,
            // Emit DWARF debug info (DISubprogram/DILocation) mapping to the .vr
            // source, and build at -O0 so gdb/lldb/addr2line resolve file:line.
            "--debug" | "-g" => debug_flag = true,
            // Obsolete: the verifier is now linked in (vendored). Accept + ignore the
            // old `--verify-c <path>` form so existing invocations keep working.
            "--verify-c" => {
                let _ = it.next();
            }
            "--emit-ir" => emit_ir = true,
            "--emit-llvm" => emit_llvm = true,
            // Dynamic module (M1, DYNAMIC-VIRE-PLAN.md P1): a PIC `.so` exporting a scalar
            // C-ABI entry `vire_module_main(i64)->i64` (from `fn module_main(x: Int) -> Int`)
            // + a `vire_module_abi` version constant, loadable at runtime via `load_module`.
            "--emit-module" => emit_module = true,
            // Unified emit selector (Meson-style): --emit=obj|asm|llvm|ir|staticlib|exe.
            a if a.starts_with("--emit=") => match &a[7..] {
                "ir" => emit_ir = true,
                "llvm" => emit_llvm = true,
                "obj" | "object" => emit_obj = true,
                "asm" | "assembly" => emit_asm = true,
                "staticlib" | "lib" | "static" => emit_staticlib = true,
                "exe" | "bin" | "executable" => {}
                k => {
                    eprintln!("--emit: unknown kind '{k}' (obj|asm|llvm|ir|staticlib|exe)");
                    exit(2);
                }
            },
            "--deps" => match it.next() {
                Some(f) => deps_file = Some(f.clone()),
                None => {
                    eprintln!("--deps needs a file path");
                    exit(2);
                }
            },
            "-I" => match it.next() {
                Some(d) => include_dirs.push(d.clone()),
                None => {
                    eprintln!("-I needs a directory");
                    exit(2);
                }
            },
            a if a.starts_with("-I") && a.len() > 2 => include_dirs.push(a[2..].to_string()),
            "--pkg" => match it.next() {
                Some(n) => pkgs.push(n.clone()),
                None => {
                    eprintln!("--pkg needs a package name (pkg-config)");
                    exit(2);
                }
            },
            // Explicit custom-keyword syntax config (opt-in; replaces the old silent
            // folder-wide auto-load). Also settable via an in-file `//!syntax:` directive.
            "--syntax" => match it.next() {
                Some(f) => syntax_file = Some(f.clone()),
                None => {
                    eprintln!("--syntax needs a config file");
                    exit(2);
                }
            },
            // Hardware profile for `Mmio.bind("name")` (language/HW-SAFE.md §2):
            // lines `name base len` (`#` comments, decimal or 0x-hex). Without it
            // every bind is E400 — addresses never come from user integers.
            "--hw-profile" => match it.next() {
                Some(f) => hw_profile_file = Some(f.clone()),
                None => {
                    eprintln!("--hw-profile needs a profile file");
                    exit(2);
                }
            },
            // Build-time minimum log level (debug|info|warn|error|off); default info.
            // Levels below it lower to nothing (zero instructions). Read in lower.rs.
            "--log-level" => match it.next() {
                Some(l) if matches!(l.as_str(), "debug" | "info" | "warn" | "error" | "off" | "none") => {
                    std::env::set_var("FASTLLVM_LOG_LEVEL", l);
                }
                _ => {
                    eprintln!("--log-level needs one of: debug info warn error off");
                    exit(2);
                }
            },
            // Build-time console sink: color the level tag by severity (ANSI).
            // Opt-in; without it the output is plain (a JSON/file sink is future work).
            "--log-color" => std::env::set_var("FASTLLVM_LOG_COLOR", "1"),
            "-O0" => opt0 = true,
            // MEASUREMENT: cycle collector forced OFF (even for cyclic types).
            // Unsound (leaks cycles), but isolates the collector cost against the
            // pure RC path — the middle column of the M0.1 three-way.
            "--no-cycles" => force_no_cycles = true,
            // MEASUREMENT (oracle): RC entirely off (retain/release no-op) — the ceiling
            // of an ideal region inference on the stable set. Implies
            // --no-cycles. Unsound (leaks), only for ceiling timing.
            "--no-rc" => {
                force_no_rc = true;
                force_no_cycles = true;
            }
            "--target" => match it.next() {
                Some(t) => target = Some(t.clone()),
                None => {
                    eprintln!("--target needs a triple (e.g. x86_64-pc-windows-gnu)");
                    exit(2);
                }
            },
            "--thin-lto" => thin_lto = true,
            "--pgo-gen" => pgo_gen = true,
            "--pgo-use" => match it.next() {
                Some(d) => pgo_use = Some(d.clone()),
                None => {
                    eprintln!("--pgo-use needs a profile directory");
                    exit(2);
                }
            },
            "-l" => match it.next() {
                Some(l) => link_libs.push(l.clone()),
                None => {
                    eprintln!("-l needs a library name");
                    exit(2);
                }
            },
            "--obj" => match it.next() {
                Some(o) => link_objs.push(o.clone()),
                None => {
                    eprintln!("--obj needs a file");
                    exit(2);
                }
            },
            a if a.starts_with("-l") && a.len() > 2 => link_libs.push(a[2..].to_string()),
            other => path = Some(other.to_string()),
        }
    }
    let path = path.unwrap_or_else(|| {
        eprintln!("no input file (.vr)");
        exit(2);
    });
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            exit(1);
        }
    };

    // First-class dependency consumption: `--pkg NAME` resolves compile/link flags via
    // pkg-config (`--cflags`/`--libs`), so a Vire project links system libraries the same
    // way a Meson/C project does. cflags feed both the native-block compile and the final
    // link; libs feed the link. A missing package is a hard error (fail early, clearly).
    let mut pkg_cflags: Vec<String> = Vec::new();
    let mut pkg_libs: Vec<String> = Vec::new();
    for name in &pkgs {
        let cflags = pkg_config_query("--cflags", name);
        let libs = pkg_config_query("--libs", name);
        match (cflags, libs) {
            (Some(cf), Some(lf)) => {
                pkg_cflags.extend(cf.split_whitespace().map(String::from));
                pkg_libs.extend(lf.split_whitespace().map(String::from));
            }
            _ => {
                eprintln!("--pkg: pkg-config has no package '{name}' (or pkg-config not installed)");
                exit(1);
            }
        }
    }

    // Front end: lex/parse (with optional user-defined syntax).
    let syntax = load_syntax(&src, &path, syntax_file.as_deref());
    let (mut module, diags) = vire::parse_with_syntax(&src, syntax);
    if !diags.is_empty() {
        for d in &diags {
            eprintln!("{}", d.render(&src));
        }
        exit(1);
    }
    // Platform conditional compilation: drop `@when(os)` items not for this target
    // (before any other pass sees them — so per-platform same-named fns don't clash).
    let os = vire::platform::target_os(target.as_deref());
    let cfg_errs = vire::apply_platform_cfg(&mut module, os);
    if !cfg_errs.is_empty() {
        for e in &cfg_errs {
            eprintln!("error: {e}");
        }
        exit(1);
    }
    // `extern "C" header "h.h"` → generate signatures at compile time from the C header
    // (auto-bindgen) and fill the extern block with them.
    let src_dir = std::path::Path::new(&path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for it in module.items.iter_mut() {
        if let vire::ast::Item::Extern { items, header: Some(h), .. } = it {
            let hpath = src_dir.join(&*h);
            header_deps.push(hpath.to_string_lossy().into_owned());
            let htext = match std::fs::read_to_string(&hpath) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("header {}: {e}", hpath.display());
                    exit(1);
                }
            };
            let (extern_text, _skipped) = header_to_extern(&htext, None);
            let (gen, gdiags) = vire::parse(&extern_text);
            if !gdiags.is_empty() {
                eprintln!("bindgen({}): generated bindings are invalid", hpath.display());
                exit(1);
            }
            if let Some(vire::ast::Item::Extern { items: gitems, .. }) = gen.items.into_iter().next() {
                *items = gitems; // fill the extern block with the generated signatures
            }
        }
    }

    // First-class inline blocks: `@c(""" … """, caps)` / `@asm(…)` → a generated
    // `native "c"`/`native "asm"` function + an `extern "C"` decl + a call at the
    // site. Runs before the native-block collection below so the generated blocks are
    // compiled and verified like any other native block.
    let cblock_errs = vire::desugar_cblocks(&mut module);
    if !cblock_errs.is_empty() {
        for e in &cblock_errs {
            eprintln!("error: {e}");
        }
        exit(1);
    }
    // `spawn f(arg)` → generated worker shim + `jrt_spawn`. Any spawn forces the
    // threads runtime (atomic RC + pthreads), enabled automatically below.
    let (spawn_errs, spawn_workers) = vire::desugar_spawn(&mut module);
    if !spawn_errs.is_empty() {
        for e in &spawn_errs {
            eprintln!("error: {e}");
        }
        exit(1);
    }
    let want_threads = !spawn_workers.is_empty();

    // C++ bridge generator: `cxx { fn sig = "c++ body" }` → generate an
    // `extern "C"` trampoline per fn (compiled via the native "c++" path) and
    // replace the item with an `extern` item so that infer/lower see the
    // signatures. Saves the hand-written facade.
    let mut cxx_native: Vec<(String, String)> = Vec::new();
    for it in &mut module.items {
        if let vire::ast::Item::Cxx { links, preamble, fns, span } = it {
            let mut src = String::from("// generated by `cxx {}` (bridge generator)\n");
            src.push_str(preamble);
            src.push('\n');
            for (sig, body) in fns.iter() {
                src.push_str(&gen_cxx_trampoline(sig, body));
            }
            cxx_native.push(("c++".into(), src));
            link_libs.extend(links.iter().cloned());
            let sigs: Vec<vire::ast::FnSig> = fns.iter().map(|(s, _)| s.clone()).collect();
            *it = vire::ast::Item::Extern { abi: "C".into(), items: sigs, links: links.clone(), header: None, span: *span };
        }
    }

    // FFI from the source: `extern "…" link "lib"` + `native "…" … """code"""`.
    // Link libs → clang `-l`; native blocks → compiled in automatically.
    let mut native_blocks: Vec<(String, String)> = cxx_native;
    for it in &module.items {
        match it {
            vire::ast::Item::Extern { links, .. } => {
                link_libs.extend(links.iter().cloned());
            }
            vire::ast::Item::Native { abi, code, links, .. } => {
                link_libs.extend(links.iter().cloned());
                native_blocks.push((abi.clone(), code.clone()));
            }
            _ => {}
        }
    }

    // Hygienic macros: AST→AST expansion BEFORE type inference.
    // Hygienic item macros (`name!(...)` → declarations) expand first, so any
    // expression macros / derives inside the generated items are then handled.
    let item_macro_errs = vire::expand_item_macros(&mut module);
    if !item_macro_errs.is_empty() {
        for e in &item_macro_errs {
            eprintln!("{}", e.render(&src));
        }
        exit(1);
    }
    if let Err(errs) = vire::expand_macros(&mut module) {
        for e in &errs {
            eprintln!("error: {e}");
        }
        exit(1);
    }
    // `@derive(...)` reflection: synthesize methods (Eq/Show) from type structure.
    // Runs before inference so generated methods are inferred + lowered normally.
    let derive_errs = vire::derive_expand(&mut module);
    if !derive_errs.is_empty() {
        for e in &derive_errs {
            eprintln!("error: {e}");
        }
        exit(1);
    }
    // Shallow self-recursive inlining (small, pure, tail-shaped recursion →
    // 1–2 levels self-inlined; LLVM CSE captures the branching win). Before infer.
    vire::inline_recursion(&mut module);
    // Type inference (F5 core): fill in un-annotated parameter types. Detected
    // type conflicts are genuine errors → reject (do not silently default to I64).
    let type_conflicts = vire::infer_module(&mut module);
    if !type_conflicts.is_empty() {
        for c in &type_conflicts {
            eprintln!("{}", c.render(&src));
        }
        exit(1);
    }
    // Compile-time evaluation pass (after inference): resolve `const` references and
    // fold `comptime`/`comptime if` on the AST before lowering sees them.
    let comptime_errs = vire::eval_comptime(&mut module);
    if !comptime_errs.is_empty() {
        for e in &comptime_errs {
            eprintln!("error: {e}");
        }
        exit(1);
    }
    // Lowering to crates/ir.
    // Debug builds thread the source so lowering emits per-statement DebugLine
    // markers; non-debug builds pass no source (no markers → no interference with
    // the optimizing passes, IR byte-for-byte as before).
    let lower_src = if debug_flag { src.as_str() } else { "" };
    // Hardware profile for `Mmio.bind` (language/HW-SAFE.md §2).
    let hw_profile: Vec<(String, u64, u64)> = hw_profile_file
        .as_deref()
        .map(load_hw_profile)
        .unwrap_or_default();
    let mut program = match vire::lower_module_src(&module, lower_src, &hw_profile) {
        Ok(p) => p,
        Err(errs) => {
            for e in &errs {
                eprintln!("lowering: {e}");
            }
            exit(1);
        }
    };
    // A dynamic module has no `main` — its entry is `fn module_main(x: Int) -> Int`
    // (exported by the shim as `vire_module_main`). Everything else needs `fn main`.
    if emit_module {
        // A module exports `fn module_main(x: Int) -> Int` (a scalar entry) and/or any
        // scalar `fn NAME(x: Int) -> Int` as an override — it needs at least one.
        let has_scalar = program
            .functions
            .iter()
            .any(|f| f.params == [fastllvm_ir::Ty::I64] && f.ret == fastllvm_ir::Ty::I64);
        // Step 2b: an object entry `fn module_obj(o: T) -> T` is an entry too — a module
        // may cross the boundary with objects only and never take a scalar at all.
        let has_obj = program
            .functions
            .iter()
            .any(|f| f.name == "module_obj" && f.params == [fastllvm_ir::Ty::Ref] && f.ret == fastllvm_ir::Ty::Ref);
        if !has_scalar && !has_obj {
            eprintln!("--emit-module: expected a scalar `fn NAME(x: Int) -> Int` (entry `module_main` and/or an override) or the object entry `fn module_obj(o: T) -> T`");
            exit(1);
        }
    } else if !program.functions.iter().any(|f| f.name == "vire_main") {
        eprintln!("no entry point: expected `fn main()`");
        exit(1);
    }
    // Spawn workers are called only from their generated C shim (invisible to the
    // solver's RTA) → keep them as reachability roots so they are not pruned.
    program.exported = spawn_workers;

    // Solver + optimization passes — identical to the Java driver.
    let _tdbg = std::env::var_os("FASTLLVM_TIME_PASSES").is_some();
    macro_rules! timed { ($name:expr, $e:expr) => {{
        if _tdbg { let __t = std::time::Instant::now(); let __r = $e; eprintln!("[time] {:<28} {:>7.1} ms", $name, __t.elapsed().as_secs_f64()*1000.0); __r }
        else { $e } }} }
    let mut s = timed!("solver::run", fastllvm_solver::run(&mut program));
    let _ = timed!("elide_redundant_ref_copies", fastllvm_solver::elide_redundant_ref_copies(&mut program));
    // Constant-propagate entry-block scalar constants first: a divisor that was a
    // `mut n = <const>` local becomes a literal → native srem/magic-multiply
    // instead of a jrt_lrem call, and constant sizes/bounds help the passes below.
    let _ = timed!("propagate_const_scalars", fastllvm_solver::propagate_const_scalars(&mut program));
    let _ = timed!("elide_bounds", fastllvm_solver::elide_bounds(&mut program));
    s.inlined_calls = timed!("inline_program", fastllvm_solver::inline_program(&mut program));
    // Bounds again, AFTER inlining. The first run above sees `fn f(a: Array[Int])` with
    // the array as a parameter: no allocation site, hence no length, hence
    // `provably_in_bounds` bails out on its first line and NOTHING is elided — a
    // function that takes an array as a parameter got no elision at all. Once the callee
    // is inlined, the array and its `array(n)` are in the same function and every path
    // (constant length, loop guard, affine index) can finally see them. Cheap: the pass
    // is linear in the IR and only re-marks, it never un-elides.
    let _ = timed!("elide_bounds_post_inline", fastllvm_solver::elide_bounds(&mut program));
    s.stack_allocated = timed!("stack_allocate", fastllvm_solver::stack_allocate(&mut program));
    // Field auto-narrowing (value-range analysis): narrow `Int` fields whose values
    // provably fit in i32 to 4 bytes (RAM). Sound (widening).
    //
    // …but it is a WHOLE-PROGRAM decision, and step 2b's shared types have two programs.
    // The narrowing depends on the ranges each side happens to observe: a host storing
    // `Pair(3, 4)` narrows both fields to i32, while the module, which only sees them
    // through a parameter, keeps i64 — the same source declaration, two layouts, and the
    // module would read the host's objects at the wrong offsets. So a program that lets
    // objects cross the boundary keeps its layouts FROZEN (the "frozen layouts" of P1
    // step 2b). Deliberately whole-program rather than per-type: the RAM this gives up is
    // a small constant, and a per-type rule would have to track exactly which types can
    // reach the boundary — a soundness argument in exchange for a few bytes.
    // The SCALAR boundary (`module_call`) is unaffected; no object crosses there.
    let obj_boundary = (emit_module
        && program.functions.iter().any(|f| f.name == "module_obj" && f.params == [fastllvm_ir::Ty::Ref] && f.ret == fastllvm_ir::Ty::Ref))
        || program.functions.iter().any(|f| {
            f.blocks.iter().any(|b| {
                b.statements
                    .iter()
                    .any(|s| matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "jrt_module_call_obj"))
            })
        });
    let _narrowed = if obj_boundary { 0 } else { timed!("narrow_fields", fastllvm_solver::narrow_fields(&mut program)) };
    // Solver-side ownership classification (borrowck offload, AGENTS.md §2):
    // analysis-only report behind FASTLLVM_DUMP_OWN=1 — measures the provable
    // fraction (Unique/Borrow/Region vs Shared). No codegen effect yet.
    // FASTLLVM_WARN_DYNAMIC=1 previews E300 (AGENTS.md §2.4, OWNERSHIP-STATIC.md):
    // every Shared heap allocation site is a program point that would need a
    // ceiling (`region(max)`) or a kernel `Rc` once E300 is enforced. Warning
    // only — measures the blast radius without breaking the corpus.
    if std::env::var_os("FASTLLVM_DUMP_OWN").is_some() || std::env::var_os("FASTLLVM_WARN_DYNAMIC").is_some() {
        let rep = fastllvm_solver::classify_ownership(&program);
        if std::env::var_os("FASTLLVM_DUMP_OWN").is_some() {
            eprintln!(
                "[own] ref={} unique={} borrow={} region={} shared={} shared_new_sites={}",
                rep.n_ref, rep.n_unique, rep.n_borrow, rep.n_region, rep.n_shared,
                rep.shared_news.len()
            );
            for (f, i) in &rep.shared_news {
                eprintln!("[own] shared-new {f} stmt#{i}");
            }
        }
        if std::env::var_os("FASTLLVM_WARN_DYNAMIC").is_some() {
            for (f, i) in &rep.shared_news {
                eprintln!("warning[E300-preview]: heap allocation in `{f}` (site #{i}) has dynamic lifetime — will need `region(max)` or kernel `Rc` when E300 is enforced (AGENTS.md §2.4)");
            }
        }
    }
    let acyclic = s.acyclic;

    if emit_ir {
        print!("{program}");
        return;
    }
    // Debug info (DWARF) mapping to the .vr source for `--debug`/`-g`. Debug builds
    // compile at -O0 (below), so the partial `!dbg` (calls/returns) is valid — the
    // inliner, which would demand it on every call, does not run. `--backtrace`
    // stays independent (symbol-level at -O2); combine `--debug --backtrace` for a
    // file:line backtrace.
    let ll = if debug_flag {
        let p = std::path::Path::new(&path);
        let fname = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        let dir = p.parent().and_then(|d| d.canonicalize().ok()).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        timed!("backend::emit_debug", fastllvm_backend::emit_debug(&program, Some((&fname, &dir))))
    } else {
        timed!("backend::emit", fastllvm_backend::emit(&program))
    };
    // Debug builds compile at -O0 (no LTO/inlining) so the line info stays precise
    // and partial !dbg is accepted.
    if debug_flag {
        opt0 = true;
    }
    if emit_llvm {
        print!("{ll}");
        return;
    }

    // Output path: -o, otherwise the file stem (a temp binary for `run`).
    let out = out.unwrap_or_else(|| {
        if is_run {
            std::env::temp_dir().join(format!("vire-run-{}", std::process::id()))
        } else {
            PathBuf::from(PathBuf::from(&path).file_stem().and_then(|s| s.to_str()).unwrap_or("a"))
        }
    });
    let build_dir = std::env::temp_dir().join(format!("vire-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&build_dir) {
        eprintln!("build directory: {e}");
        exit(1);
    }
    let ll_path = build_dir.join("program.ll");
    let rt_path = build_dir.join("runtime.c");
    // `--emit-module`: append the C-ABI export shim (a scalar module entry + the frozen
    // ABI version) so the runtime loader can dlsym + version-check + call it. Requires the
    // module to define `fn module_main(x: Int) -> Int` (symbol `@module_main`).
    let ll = if emit_module {
        use fastllvm_ir::Ty;
        let mut shim = String::from("\n@vire_module_abi = constant i64 1\n");
        // Optional scalar entry `fn module_main(x: Int) -> Int` → `vire_module_main`.
        if program.functions.iter().any(|f| f.name == "module_main") {
            shim.push_str("define i64 @vire_module_main(i64 %x) {\n  %r = call i64 @module_main(i64 %x)\n  ret i64 %r\n}\n");
        }
        // Every scalar `fn NAME(x: Int) -> Int` is also exported as `vire_override_NAME`,
        // so a host with a matching `dynamic fn NAME` can install it via `install_override`.
        for f in &program.functions {
            if f.name != "module_main" && f.params == [Ty::I64] && f.ret == Ty::I64 {
                shim.push_str(&format!(
                    "define i64 @vire_override_{0}(i64 %x) {{\n  %r = call i64 @{0}(i64 %x)\n  ret i64 %r\n}}\n",
                    f.name
                ));
            }
        }
        // Step 2b — the OBJECT entry: `fn module_obj(o: T) -> T` → `vire_module_obj`.
        // Boundary RC convention, identical to an ordinary Vire call: the argument is
        // BORROWED (+0, the host keeps its count) and the result is OWNED (+1, handed to
        // the host). The module allocates and retains through the *host's* runtime — it
        // links no `runtime.c` of its own and resolves `jrt_*` from the executable — so
        // there is exactly one heap and one set of counts across the boundary, which is
        // what makes a two-module program's 0-live claim mean anything.
        if program.functions.iter().any(|f| f.name == "module_obj" && f.params == [Ty::Ref] && f.ret == Ty::Ref) {
            shim.push_str("define ptr @vire_module_obj(ptr %o) {\n  %r = call ptr @module_obj(ptr %o)\n  ret ptr %r\n}\n");
        }
        // The shared-type ABI table the host's load verifier checks (layout hash +
        // ownership edges). Distinct symbol name from the host's on purpose — see
        // `emit_typetab`.
        shim.push_str(&fastllvm_backend::emit_typetab(&program, "vire_module_types"));
        format!("{ll}{shim}")
    } else if uses_module_loader(&program) {
        // The host side of the same table: the verifier needs the host's own layouts and
        // type graph to compare against. Only emitted for a program that actually loads
        // modules — otherwise it is dead weight that `--gc-sections` would strip anyway.
        let tab = fastllvm_backend::emit_typetab(&program, "vire_host_types");
        format!("{ll}{tab}")
    } else {
        ll
    };
    if let Err(e) = std::fs::write(&ll_path, &ll).and_then(|_| std::fs::write(&rt_path, RUNTIME_C)) {
        eprintln!("writing to {}: {e}", build_dir.display());
        exit(1);
    }
    // Does the program load native modules at runtime (`load_module`)? Decides
    // the shared-type ABI table + `-rdynamic` (see below).
    fn uses_module_loader(program: &fastllvm_ir::Program) -> bool {
        program.functions.iter().any(|f| {
            f.blocks.iter().any(|b| {
                b.statements
                    .iter()
                    .any(|s| matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "jrt_load_module"))
            })
        })
    }

    // Write embedded native blocks to files (extension by ABI) and
    // add compile/link flags for C++/Python automatically.
    let mut native_paths: Vec<PathBuf> = Vec::new();
    let mut want_cpp = false;
    let mut want_python = false;
    // Blocks that need the memory-safety proof: (block index, path, code, ext, abi).
    // Collected here, then verified in PARALLEL below (each proof is an independent
    // clang `-emit-llvm` + a functional CSolver run over its own `.verify.ll`).
    let mut verify_jobs: Vec<(usize, PathBuf, String, &'static str, String)> = Vec::new();
    for (i, (abi, code)) in native_blocks.iter().enumerate() {
        let a = abi.to_ascii_lowercase();
        // Compiler-generated glue (`c-glue`, e.g. the `spawn` shim): compiled as C
        // but exempt from the verification gate — it is a trusted runtime handoff,
        // not user `unsafe`.
        let glue = a == "c-glue";
        let ext = if a == "c++" || a == "cpp" || a == "cxx" {
            want_cpp = true;
            "cpp"
        } else if a == "asm" || a == "s" || a == "assembly" {
            "s"
        } else if a == "python" || a == "py" {
            want_python = true;
            "py"
        } else {
            "c"
        };
        let p = build_dir.join(format!("native_{i}.{ext}"));
        if let Err(e) = std::fs::write(&p, code) {
            eprintln!("writing native block: {e}");
            exit(1);
        }
        // Verification gate (MANDATORY): a `native "c"`/`native "asm"` block is
        // accepted only when the vendored CSolver memory-safety verifier PROVES it safe
        // (called as a library — structured verdicts, no subprocess) — the sound
        // replacement for a blind `unsafe`. A block that cannot be proven safe is a
        // compile error (E410), not a runtime hazard. No opt-out.
        if !glue && (ext == "c" || ext == "s") {
            verify_jobs.push((i, p.clone(), code.clone(), ext, abi.clone()));
        }
        native_paths.push(p);
    }
    verify_blocks_parallel(&verify_jobs, &build_dir);
    // Raw port I/O helpers (language/HW-SAFE.md §2): x86 `in`/`out` have no
    // alternative encoding, so the compiler generates this tiny TU into the
    // PROGRAM's own build (never a shared runtime, never user-written asm).
    // Counted toolchain-side glue like the `spawn` shim. Only when used.
    {
        let mut want_port = false;
        for f in &program.functions {
            for bb in &f.blocks {
                for st in &bb.statements {
                    if let fastllvm_ir::Statement::Call { func, .. } = st {
                        if func == "__vire_port_outb" || func == "__vire_port_inb" {
                            want_port = true;
                        }
                    }
                }
            }
        }
        if want_port {
            let code = "/* Generated per program (language/HW-SAFE.md H2): raw x86 port I/O.\n * The ONLY toolchain-side asm in Vire output; user code can never emit asm.\n * `asm volatile` + memory clobber: the compiler may neither reorder nor drop it. */\n#include <stdint.h>\nvoid __vire_port_outb(int64_t port, int64_t val) { __asm__ volatile (\"outb %0, %1\" :: \"a\"((uint8_t)val), \"Nd\"((uint16_t)port) : \"memory\"); }\nint64_t __vire_port_inb(int64_t port) { uint8_t v; __asm__ volatile (\"inb %1, %0\" : \"=a\"(v) : \"Nd\"((uint16_t)port) : \"memory\"); return (int64_t)v; }\n";
            let p = build_dir.join("vire_port.c");
            if let Err(e) = std::fs::write(&p, code) {
                eprintln!("writing port helpers: {e}");
                exit(1);
            }
            native_paths.push(p);
        }
    }
    // Python: include path + libpython automatically (from python3/sysconfig).
    let mut py_include: Option<String> = None;
    if want_python {
        match python_config() {
            Some((inc, lib)) => {
                py_include = Some(inc);
                link_libs.push(lib);
            }
            None => {
                eprintln!("native \"python\": python3/sysconfig not found");
                exit(1);
            }
        }
    }
    // === Object / assembly / static-library emit (Meson & C/C++/Rust interop) ===
    // A whole `.vr` program lowers to ONE relocatable object exposing C-ABI symbols
    // (the runtime `main` included) that Meson/ld links with other objects. We must NOT
    // use `-flto` here: `ld -r`/`ar` merge real ELF objects, not LTO bitcode (the exe
    // path below keeps full LTO). The same `-D` defines as the exe path keep the runtime
    // ABI identical, so the emitted object behaves exactly like a `vire build` binary.
    if emit_obj || emit_asm || emit_staticlib {
        let mut cg: Vec<String> = Vec::new();
        cg.push(if opt0 { "-O0".into() } else { "-O2".into() });
        match &target {
            Some(t) => {
                cg.push("-target".into());
                cg.push(t.clone());
            }
            None => cg.push("-march=native".into()),
        }
        if want_threads || threads_flag {
            cg.push("-DFASTLLVM_THREADS".into());
        }
        if backtrace_flag {
            cg.push("-DFASTLLVM_BACKTRACE".into());
        }
        if debug_flag {
            cg.push("-g".into());
            cg.push("-fno-omit-frame-pointer".into());
        }
        if acyclic || force_no_cycles {
            cg.push("-DFASTLLVM_NO_CYCLES".into());
        }
        if force_no_rc {
            cg.push("-DFASTLLVM_NO_RC".into());
        }
        for inc in &include_dirs {
            cg.push(format!("-I{inc}"));
        }
        if let Some(inc) = &py_include {
            cg.push(format!("-I{inc}"));
        }
        cg.extend(pkg_cflags.iter().cloned());

        let default_ext = if emit_asm {
            "s"
        } else if emit_staticlib {
            "a"
        } else {
            "o"
        };
        // `out` is already resolved (the exe path defaulted it to the file stem). Give it
        // the kind's extension when none was supplied (`-o prog` → `prog.o`).
        let out = if out.extension().is_some() { out.clone() } else { out.with_extension(default_ext) };
        // Ninja/Makefile depfile (Meson `depfile:`): the object depends on the .vr source
        // and every C header it pulls in, so a header edit triggers a rebuild.
        if let Some(df) = &deps_file {
            write_depfile(df, &out, &path, &header_deps);
        }

        if emit_asm {
            // Assembly of the program IR — the inspectable artifact (runtime excluded).
            let st = Command::new("clang").args(&cg).arg("-S").arg(&ll_path).arg("-o").arg(&out).status();
            match st {
                Ok(s) if s.success() => {}
                _ => {
                    eprintln!("clang -S (assembly emit) failed");
                    exit(1);
                }
            }
            let _ = std::fs::remove_dir_all(&build_dir);
            eprintln!("vire: wrote {}", out.display());
            return;
        }

        // Compile each input to a real ELF object, then partial-link into a single .o.
        let mut objs: Vec<PathBuf> = Vec::new();
        let compile = |src: &std::path::Path, tag: &str| -> PathBuf {
            let o = build_dir.join(format!("{tag}.o"));
            let st = Command::new("clang").args(&cg).arg("-c").arg(src).arg("-o").arg(&o).status();
            match st {
                Ok(s) if s.success() => o,
                _ => {
                    eprintln!("clang -c failed for {}", src.display());
                    exit(1);
                }
            }
        };
        objs.push(compile(&ll_path, "program"));
        objs.push(compile(&rt_path, "runtime"));
        for (i, p) in native_paths.iter().enumerate() {
            // Only compilable native sources land in the object (skip e.g. python .py,
            // which is loaded at runtime via libpython, not linked).
            let compilable = matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("c" | "cc" | "cpp" | "cxx" | "s" | "S")
            );
            if compilable {
                objs.push(compile(p, &format!("native_{i}")));
            }
        }
        // Relocatable partial link → one merged .o.
        let merged = if emit_staticlib { build_dir.join("merged.o") } else { out.clone() };
        let mut r = Command::new("clang");
        r.arg("-r").arg("-nostdlib");
        for o in &objs {
            r.arg(o);
        }
        let st = r.arg("-o").arg(&merged).status();
        match st {
            Ok(s) if s.success() => {}
            _ => {
                eprintln!("clang -r (object merge) failed");
                exit(1);
            }
        }
        if emit_staticlib {
            let st = Command::new("ar").arg("rcs").arg(&out).arg(&merged).status();
            match st {
                Ok(s) if s.success() => {}
                _ => {
                    eprintln!("ar (static library) failed");
                    exit(1);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&build_dir);
        eprintln!("vire: wrote {}", out.display());
        return;
    }

    let mut cmd = Command::new("clang");
    if opt0 {
        // Measurement mode: no optimization, no LTO → allocations remain in place.
        // Section GC stays (strips only unused whole functions such as the
        // threads path, not the alloc calls in used functions).
        cmd.arg("-O0").arg(&ll_path).arg(&rt_path);
        cmd.args(["-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections"]);
    } else {
        // Runtime bitcode cache: `runtime.c` is identical every build, so reuse its
        // cached `-flto` bitcode instead of recompiling it (~80% of a small build's
        // time). Lossless — the LTO link optimizes it exactly as if from source.
        // Skipped under PGO (the runtime must share the program's instrumentation).
        let rt_cached = if pgo_gen || pgo_use.is_some() || emit_module {
            // A module links `-shared`, so the runtime must be PIC — the cached object is
            // not; compile runtime.c from source (with the `-fPIC` added below) instead.
            None
        } else {
            cached_runtime_object(want_threads || threads_flag, backtrace_flag, acyclic || force_no_cycles, force_no_rc, thin_lto, target.as_deref())
        };
        let rt_input = rt_cached.as_deref().unwrap_or_else(|| rt_path.as_path());
        cmd.arg("-O2").arg(&ll_path);
        // A scalar module (`--emit-module`) is self-contained (pure scalar arithmetic, no
        // jrt_ calls) and must NOT pull in runtime.c — doing so would drag in the runtime's
        // `jrt_dynslot` reference, which a module (with no `dynamic fn`s) does not define,
        // making the `.so` fail to `dlopen` (undefined symbol). Host binaries link it.
        if !emit_module {
            cmd.arg(rt_input);
        }
        cmd.args(["-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections"]);
        // ThinLTO (parallel, low memory) for huge programs; otherwise full LTO.
        cmd.arg(if thin_lto { "-flto=thin" } else { "-flto" });
        // `-march=native` only without cross-compile (otherwise the host CPU does not
        // match the target triple). Pass the triple through when cross-compiling.
        match &target {
            Some(t) => {
                cmd.arg("-target").arg(t);
                // Cross-compiling with `-flto`: the target's default linker (e.g.
                // MinGW's `ld`) can't consume LLVM bitcode. LLD handles LTO for
                // ELF/PE/Mach-O, so use it for every cross target.
                cmd.arg("-fuse-ld=lld");
            }
            None => {
                cmd.arg("-march=native");
            }
        }
        // PGO: instrumentation (gen) or profile use (use). LTO stays on —
        // clang combines `-fprofile-use` with `-flto` (the hot paths are
        // inlined/unrolled/laid out more aggressively).
        if pgo_gen {
            cmd.arg("-fprofile-generate");
        } else if let Some(dir) = &pgo_use {
            cmd.arg(format!("-fprofile-use={dir}"));
            // if a site is missing from the profile, that is not an error (just uninstrumented codegen).
            cmd.arg("-Wno-profile-instr-unprofiled").arg("-Wno-profile-instr-out-of-date");
        }
    }
    // Dynamic loading: link libdl so the runtime's `jrt_load_module` (dlopen) resolves.
    // Hosted only; harmless when `load_module` is unused (the loader is section-stripped,
    // so libdl stays unreferenced). Cross targets get their own loader story later.
    if target.is_none() {
        cmd.arg("-ldl");
    }
    // A loaded module carries no runtime of its own (that would give it a second heap and
    // a second set of reference counts — the boundary RC would then be a fiction). It
    // resolves `jrt_alloc`/`jrt_retain`/`jrt_release`/… against the host executable at
    // `dlopen` time, which only works if those symbols are in the host's dynamic symbol
    // table. Costs nothing but a larger `.dynsym` in a program that loads modules anyway.
    if !emit_module && uses_module_loader(&program) {
        cmd.arg("-rdynamic");
    }
    // A dynamic module (`--emit-module`) is a position-independent shared object exporting
    // the scalar C-ABI entry, not an executable.
    if emit_module {
        cmd.arg("-shared").arg("-fPIC");
    }
    // Threads: enabled automatically when the program uses `spawn` (or explicitly
    // via `--threads`). Switches on atomic reference counting + pthreads. Note the
    // incremental cycle collector is disabled under threads (documented limit in
    // runtime.c), so it composes with the acyclic/NO_CYCLES flags below.
    if want_threads || threads_flag {
        cmd.arg("-DFASTLLVM_THREADS").arg("-pthread");
    }
    // Debug backtraces: define the runtime hook and export symbols so the native
    // backtrace resolves Vire function names.
    if backtrace_flag {
        cmd.arg("-DFASTLLVM_BACKTRACE").arg("-rdynamic").arg("-funwind-tables");
    }
    // Debug (DWARF) build: keep the metadata (`-g`) and disable PIE so the runtime
    // addresses in a backtrace equal the static addresses addr2line/gdb expect.
    if debug_flag {
        cmd.arg("-g").arg("-no-pie").arg("-fno-omit-frame-pointer");
    }
    if acyclic || force_no_cycles {
        cmd.arg("-DFASTLLVM_NO_CYCLES");
    }
    if force_no_rc {
        cmd.arg("-DFASTLLVM_NO_RC");
    }
    // Embedded native sources + include/stdlib flags.
    if let Some(inc) = &py_include {
        cmd.arg(format!("-I{inc}"));
    }
    // User include paths (`-I`) and pkg-config cflags — for `native "c"` blocks / headers.
    for inc in &include_dirs {
        cmd.arg(format!("-I{inc}"));
    }
    cmd.args(&pkg_cflags);
    for p in &native_paths {
        cmd.arg(p);
    }
    if want_cpp {
        link_libs.push("stdc++".into()); // C++ blocks need the C++ stdlib
    }
    // FFI linking: user objects/sources first, then libraries. libm always
    // (math intrinsics via extern "C"). clang compiles supplied .c/.cpp directly.
    for o in &link_objs {
        cmd.arg(o);
    }
    cmd.arg("-lm");
    for l in &link_libs {
        cmd.arg(format!("-l{l}"));
    }
    // pkg-config libs (`--libs`: -L/-l/other) resolved from `--pkg`.
    cmd.args(&pkg_libs);
    let status = cmd.arg("-o").arg(&out).status();
    match status {
        Ok(st) if st.success() => {
            let _ = std::fs::remove_dir_all(&build_dir);
        }
        Ok(st) => {
            eprintln!("clang failed ({st}); intermediate files in {}", build_dir.display());
            exit(1);
        }
        Err(e) => {
            eprintln!("clang not executable: {e}");
            exit(1);
        }
    }

    if is_run {
        let st = Command::new(&out).status();
        let _ = std::fs::remove_file(&out);
        match st {
            Ok(st) => exit(st.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("execution failed: {e}");
                exit(1);
            }
        }
    }
}

/// Query pkg-config for a package's `--cflags` or `--libs`. Returns the trimmed output,
/// or `None` if pkg-config is missing or the package is unknown (non-zero exit).
fn pkg_config_query(flag: &str, name: &str) -> Option<String> {
    let out = Command::new("pkg-config").arg(flag).arg(name).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Write a Makefile/Ninja depfile (`out: src header…`) for Meson's `depfile:` — Ninja
/// rebuilds the object when the `.vr` source or any pulled-in C header changes. Paths
/// with spaces are escaped `\ ` per the Make depfile convention.
fn write_depfile(path: &str, out: &std::path::Path, src: &str, headers: &[String]) {
    let esc = |s: &str| s.replace(' ', "\\ ");
    let mut line = format!("{}:", esc(&out.to_string_lossy()));
    line.push(' ');
    line.push_str(&esc(src));
    for h in headers {
        line.push(' ');
        line.push_str(&esc(h));
    }
    line.push('\n');
    if let Err(e) = std::fs::write(path, line) {
        eprintln!("warning: could not write depfile {path}: {e}");
    }
}

/// `vire bindgen HEADER.h [-l lib] [-o OUT.vr]` — generates an `extern "C"` block
/// from C function declarations so signatures need not be typed by hand.
/// Dependency-free heuristic parser: covers scalar + pointer APIs
/// (the 80% case). Struct-by-value/function pointers/varargs are skipped with a
/// note (not cleanly mappable to the C ABI).
fn bindgen(args: &[String]) {
    let mut header: Option<String> = None;
    let mut lib: Option<String> = None;
    let mut out: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-l" => lib = it.next().cloned(),
            "-o" => out = it.next().cloned(),
            other => header = Some(other.to_string()),
        }
    }
    let header = header.unwrap_or_else(|| {
        eprintln!("Usage: vire bindgen HEADER.h [-l lib] [-o OUT.vr]");
        exit(2);
    });
    let text = std::fs::read_to_string(&header).unwrap_or_else(|e| {
        eprintln!("{header}: {e}");
        exit(1);
    });
    let (s, skipped) = header_to_extern(&text, lib.as_deref());
    let nfns = s.matches("\n    fn ").count();
    match out {
        Some(o) => {
            std::fs::write(&o, &s).unwrap_or_else(|e| {
                eprintln!("{o}: {e}");
                exit(1);
            });
            eprintln!("vire bindgen: {nfns} function(s) → {o} ({skipped} skipped)");
        }
        None => print!("{s}"),
    }
}

/// C header text → `extern "C"` block (text) + number of skipped declarations.
/// Core used by both `vire bindgen` and the `header "…"` directive.
fn header_to_extern(text: &str, lib: Option<&str>) -> (String, usize) {
    let cleaned = strip_c(text);
    let mut lines = Vec::new();
    let mut skipped = 0usize;
    for chunk in cleaned.split(';') {
        match parse_proto(chunk) {
            Ok(Some(line)) => lines.push(line),
            Ok(None) => {}
            Err(_) => skipped += 1,
        }
    }
    let mut s = String::new();
    match lib {
        Some(l) => s.push_str(&format!("extern \"C\" link \"{l}\" {{\n")),
        None => s.push_str("extern \"C\" {\n"),
    }
    for l in &lines {
        s.push_str("    ");
        s.push_str(l);
        s.push('\n');
    }
    s.push_str("}\n");
    (s, skipped)
}

/// Remove comments + preprocessor lines (rough, for the prototype scan).
fn strip_c(text: &str) -> String {
    let mut out = String::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    // Drop preprocessor lines (#...).
    out.lines().filter(|l| !l.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n")
}

/// Parse a C function prototype from a `;`-separated chunk → Vire `fn` line.
/// `Ok(None)` = not a function prototype; `Err` = skipped (not mappable).
fn parse_proto(chunk: &str) -> Result<Option<String>, ()> {
    let c = chunk.trim();
    if c.is_empty() || c.contains('{') || c.contains('}') {
        return Ok(None);
    }
    // first '(' and matching ')'
    let open = match c.find('(') {
        Some(o) => o,
        None => return Ok(None),
    };
    let close = match c.rfind(')') {
        Some(x) if x > open => x,
        _ => return Ok(None),
    };
    let head = c[..open].trim();
    let params_s = c[open + 1..close].trim();
    // function pointers / nested parentheses in the head → skip.
    if head.contains('(') || head.contains(')') || head.is_empty() {
        return Ok(None);
    }
    // varargs → not mappable.
    if params_s.contains("...") {
        return Err(());
    }
    // name = last identifier in the head; rest = return type.
    let name_start = head.rfind(|ch: char| !(ch.is_alphanumeric() || ch == '_')).map(|p| p + 1).unwrap_or(0);
    let name = &head[name_start..];
    let ret_c = head[..name_start].trim();
    if name.is_empty() || !name.chars().next().unwrap().is_alphabetic() && name.chars().next() != Some('_') {
        return Ok(None);
    }
    // only genuine declarations (no typedef/struct/enum/union as a "return")
    if ret_c.is_empty() || ret_c.starts_with("typedef") {
        return Ok(None);
    }
    let ret_v = map_c_ty(ret_c, true)?;
    // parameters
    let mut vparams = Vec::new();
    if !params_s.is_empty() && params_s != "void" {
        for (k, p) in params_s.split(',').enumerate() {
            let p = p.trim();
            let ty = map_c_param(p)?;
            vparams.push(format!("a{k}: {ty}"));
        }
    }
    let ret_part = if ret_v == "Unit" { String::new() } else { format!(" -> {ret_v}") };
    Ok(Some(format!("fn {name}({}){ret_part}", vparams.join(", "))))
}

/// C parameter (type + optional name) → Vire type.
fn map_c_param(p: &str) -> Result<&'static str, ()> {
    // strip the name at the end (if present): last identifier without '*'.
    let t = if p.contains('*') {
        "Ptr" // any pointer
    } else {
        // split off the last identifier (param name)
        let stripped = match p.rfind(|c: char| !(c.is_alphanumeric() || c == '_')) {
            Some(pos) => p[..pos + 1].trim(),
            None => p, // just one word → type without name
        };
        let base = if stripped.is_empty() { p } else { stripped };
        return map_c_ty(base, false);
    };
    Ok(t)
}

/// C type name → Vire type. `is_ret`: void → Unit allowed.
fn map_c_ty(s: &str, is_ret: bool) -> Result<&'static str, ()> {
    let s = s.replace("const", " ").replace("volatile", " ").replace("unsigned", " ").replace("signed", " ");
    let n: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.contains('*') {
        return Ok("Ptr");
    }
    Ok(match n.as_str() {
        "void" => {
            if is_ret {
                "Unit"
            } else {
                return Err(());
            }
        }
        "double" => "F64",
        "float" => "F32",
        "int" | "int32_t" | "short" | "char" | "int16_t" | "int8_t" | "uint32_t" | "uint16_t" | "uint8_t" | "wchar_t" => "I32",
        "long" | "long long" | "long int" | "int64_t" | "uint64_t" | "size_t" | "ssize_t" | "intptr_t" | "uintptr_t" | "off_t" | "time_t" => "Int",
        "bool" | "_Bool" => "Bool",
        // Unknown non-pointer type (e.g. struct by value) → not mappable.
        _ => return Err(()),
    })
}

/// C++ bridge: Vire type name → C-ABI C++ type (for the trampoline signature).
/// Scalar + `Ptr` (opaque object handle) directly; Str/ref → `void*` (raw handle).
fn map_cxx_ty(n: &str) -> &'static str {
    match n {
        "Int" | "I64" | "U64" => "long",
        "I32" | "U32" => "int",
        "Float" | "F64" => "double",
        "F32" => "float",
        "Bool" => "int",
        _ => "void*", // Ptr / Str / ref → opaque pointer
    }
}

/// Generates the `extern "C"` trampoline for a `cxx` fn. The body is C++:
/// if it contains `;`/`return`, it is taken as a statement block; otherwise it is
/// wrapped as an expression (`return (expr);`, or `expr;` for Unit).
fn gen_cxx_trampoline(sig: &vire::ast::FnSig, body: &str) -> String {
    let ret_name = sig.ret.as_ref().map(|t| t.name.as_str());
    let cret = match ret_name {
        None | Some("Unit") => "void",
        Some(n) => map_cxx_ty(n),
    };
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|p| {
            let cty = p.ty.as_ref().map(|t| map_cxx_ty(&t.name)).unwrap_or("long");
            format!("{cty} {}", p.name)
        })
        .collect();
    let b = body.trim();
    let is_void = matches!(ret_name, None | Some("Unit"));
    let wrapped = if b.contains("return") || b.contains(';') {
        b.to_string()
    } else if is_void {
        format!("{b};")
    } else {
        format!("return ({b});")
    };
    format!("extern \"C\" {cret} {}({}) {{ {wrapped} }}\n", sig.name, params.join(", "))
}
