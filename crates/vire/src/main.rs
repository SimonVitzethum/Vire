//! `vire` — compiler driver.
//! Usage: `vire parse DATEI.vr` | `vire lex DATEI.vr` |
//!         `vire build [-o BIN] [--emit-ir|--emit-llvm] DATEI.vr` |
//!         `vire run DATEI.vr`.
//! `build`/`run` lower the AST to `crates/ir` and use the same
//! solver + LLVM backend + runtime as the Java driver (fastjavac).

use std::path::PathBuf;
use std::process::{exit, Command};

// The same runtime as the Java driver (crates/driver/src/runtime.c) — a
// shared `main`→`java_main` entry point, the same jrt_ helpers.
const RUNTIME_C: &str = include_str!("../../driver/src/runtime.c");
// Built-in Python bridge: allows Python libs from pure Vire (no user C).
const PYBRIDGE_C: &str = include_str!("pybridge.c");

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
        other => {
            eprintln!("unknown command: {other} (parse|lex|build|run)");
            exit(2);
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
fn load_syntax(src_path: &str) -> vire::Syntax {
    let cfg = std::path::Path::new(src_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("vire.syntax");
    let Ok(text) = std::fs::read_to_string(&cfg) else {
        return vire::Syntax::default();
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
            "--emit-ir" => emit_ir = true,
            "--emit-llvm" => emit_llvm = true,
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

    // Front end: lex/parse (with optional user-defined syntax).
    let syntax = load_syntax(&path);
    let (mut module, diags) = vire::parse_with_syntax(&src, syntax);
    if !diags.is_empty() {
        for d in &diags {
            eprintln!("{}", d.render(&src));
        }
        exit(1);
    }
    // `extern "C" header "h.h"` → generate signatures at compile time from the C header
    // (auto-bindgen) and fill the extern block with them.
    let src_dir = std::path::Path::new(&path).parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for it in module.items.iter_mut() {
        if let vire::ast::Item::Extern { items, header: Some(h), .. } = it {
            let hpath = src_dir.join(&*h);
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
    // Does the program use the built-in Python bridge (`vire_py_*`)? Then
    // pybridge.c is compiled in automatically + libpython linked — NO user C.
    let mut want_py_bridge = false;
    for it in &module.items {
        match it {
            vire::ast::Item::Extern { items, links, .. } => {
                link_libs.extend(links.iter().cloned());
                if items.iter().any(|s| s.name.starts_with("vire_py_") || s.name.starts_with("py_")) {
                    want_py_bridge = true;
                }
            }
            vire::ast::Item::Native { abi, code, links, .. } => {
                link_libs.extend(links.iter().cloned());
                native_blocks.push((abi.clone(), code.clone()));
            }
            _ => {}
        }
    }

    // Hygienic macros: AST→AST expansion BEFORE type inference.
    if let Err(errs) = vire::expand_macros(&mut module) {
        for e in &errs {
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
            eprintln!("error: {c}");
        }
        exit(1);
    }
    // Lowering to crates/ir.
    let mut program = match vire::lower_module(&module) {
        Ok(p) => p,
        Err(errs) => {
            for e in &errs {
                eprintln!("lowering: {e}");
            }
            exit(1);
        }
    };
    if !program.functions.iter().any(|f| f.name == "java_main") {
        eprintln!("no entry point: expected `fn main()`");
        exit(1);
    }
    // Also trigger the Python bridge when `py_*` is used WITHOUT an extern block
    // (the signatures are built in) — detectable on the lowered program.
    if !want_py_bridge {
        want_py_bridge = program.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|st| {
            matches!(st, fastllvm_ir::Statement::Call { func, .. } if func.starts_with("py_") || func.starts_with("vire_py_"))
        });
    }

    // Solver + optimization passes — identical to the Java driver.
    let mut s = fastllvm_solver::run(&mut program);
    let _ = fastllvm_solver::elide_redundant_ref_copies(&mut program);
    let _ = fastllvm_solver::fuse_long_compares(&mut program);
    let _ = fastllvm_solver::elide_bounds(&mut program);
    let _ = fastllvm_solver::elide_pending_checks(&mut program);
    s.inlined_calls = fastllvm_solver::inline_program(&mut program);
    s.stack_allocated = fastllvm_solver::stack_allocate(&mut program);
    // Field auto-narrowing (value-range analysis): narrow `Int` fields whose values
    // provably fit in i32 to 4 bytes (RAM). Sound (widening).
    let _narrowed = fastllvm_solver::narrow_fields(&mut program);
    let acyclic = s.acyclic;

    if emit_ir {
        print!("{program}");
        return;
    }
    let ll = fastllvm_backend::emit(&program);
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
    if let Err(e) = std::fs::write(&ll_path, &ll).and_then(|_| std::fs::write(&rt_path, RUNTIME_C)) {
        eprintln!("writing to {}: {e}", build_dir.display());
        exit(1);
    }
    // Enqueue the built-in Python bridge as a (Python) native block → it goes through
    // the same auto-include/link path as a user `native "python"` block.
    if want_py_bridge {
        native_blocks.push(("python".into(), PYBRIDGE_C.to_string()));
    }
    // Write embedded native blocks to files (extension by ABI) and
    // add compile/link flags for C++/Python automatically.
    let mut native_paths: Vec<PathBuf> = Vec::new();
    let mut want_cpp = false;
    let mut want_python = false;
    for (i, (abi, code)) in native_blocks.iter().enumerate() {
        let a = abi.to_ascii_lowercase();
        let ext = if a == "c++" || a == "cpp" || a == "cxx" { want_cpp = true; "cpp" } else { "c" };
        if a == "python" || a == "py" {
            want_python = true;
        }
        let p = build_dir.join(format!("native_{i}.{ext}"));
        if let Err(e) = std::fs::write(&p, code) {
            eprintln!("writing native block: {e}");
            exit(1);
        }
        native_paths.push(p);
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
    let mut cmd = Command::new("clang");
    if opt0 {
        // Measurement mode: no optimization, no LTO → allocations remain in place.
        // Section GC stays (strips only unused whole functions such as the
        // threads path, not the alloc calls in used functions).
        cmd.arg("-O0").arg(&ll_path).arg(&rt_path);
        cmd.args(["-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections"]);
    } else {
        cmd.arg("-O2").arg(&ll_path).arg(&rt_path);
        cmd.args(["-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections"]);
        // ThinLTO (parallel, low memory) for huge programs; otherwise full LTO.
        cmd.arg(if thin_lto { "-flto=thin" } else { "-flto" });
        // `-march=native` only without cross-compile (otherwise the host CPU does not
        // match the target triple). Pass the triple through when cross-compiling.
        match &target {
            Some(t) => {
                cmd.arg("-target").arg(t);
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
