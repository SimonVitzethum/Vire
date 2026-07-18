//! `vire` — Compiler-Treiber.
//! Aufruf: `vire parse DATEI.vr` | `vire lex DATEI.vr` |
//!         `vire build [-o BIN] [--emit-ir|--emit-llvm] DATEI.vr` |
//!         `vire run DATEI.vr`.
//! `build`/`run` senken den AST nach `crates/ir` ab und nutzen denselben
//! Solver + LLVM-Backend + Runtime wie der Java-Treiber (fastjavac).

use std::path::PathBuf;
use std::process::{exit, Command};

// Dieselbe Runtime wie der Java-Treiber (crates/driver/src/runtime.c) — ein
// gemeinsamer `main`→`java_main`-Einstieg, dieselben jrt_-Helfer.
const RUNTIME_C: &str = include_str!("../../driver/src/runtime.c");
// Eingebaute Python-Brücke: erlaubt Python-Libs aus reinem Vire (kein Nutzer-C).
const PYBRIDGE_C: &str = include_str!("pybridge.c");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Aufruf: vire (parse|lex|build|run) DATEI.vr");
        exit(2);
    }
    if args[0] == "build" || args[0] == "run" {
        build_or_run(&args);
        return;
    }
    if args.len() < 2 {
        eprintln!("Aufruf: vire (parse|lex) DATEI.vr");
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
                "{} Item(s), {} Diagnose(n)",
                module.items.len(),
                diags.len()
            );
            if !diags.is_empty() {
                exit(1);
            }
        }
        other => {
            eprintln!("unbekannter Befehl: {other} (parse|lex|build|run)");
            exit(2);
        }
    }
}

/// Python-Include-Pfad + Lib-Name via `python3`/sysconfig (für `native "python"`).
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

/// Lädt eine `vire.syntax`-Datei neben der Quelle (falls vorhanden) → nutzer-
/// definierte Schlüsselwort-Schreibweisen. Fehlt sie, gilt die Standard-Syntax.
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
            eprintln!("vire: Syntax-Konfig {} geladen", cfg.display());
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

/// `vire build`/`run`: .vr → AST → IR (Absenkung) → Solver → LLVM → clang → Binary.
/// `run` führt das Binary danach aus und reicht dessen Exit-Code durch.
fn build_or_run(args: &[String]) {
    let is_run = args[0] == "run";
    let mut out: Option<PathBuf> = None;
    let mut emit_ir = false;
    let mut emit_llvm = false;
    // -O0: clang-Optimierung/LTO aus. Für ehrliche RC-/Heap-MESSUNGEN — sonst
    // eliminiert `-O2 -flto` tote Allokations-/Release-Paare (die Objekte werden
    // wegoptimiert, die Laufzeitzähler bleiben 0). Der Solver läuft immer.
    let mut opt0 = false;
    let mut force_no_cycles = false;
    let mut force_no_rc = false;
    // FFI: zusätzliche Bibliotheken (`-l NAME`) und Objekte/Quellen (`--obj FILE`,
    // .c/.cpp/.o/.a) zum Linken — für C/C++/Python-Interop.
    let mut link_libs: Vec<String> = Vec::new();
    let mut link_objs: Vec<String> = Vec::new();
    let mut path: Option<String> = None;
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => match it.next() {
                Some(p) => out = Some(PathBuf::from(p)),
                None => {
                    eprintln!("-o braucht ein Argument");
                    exit(2);
                }
            },
            "--emit-ir" => emit_ir = true,
            "--emit-llvm" => emit_llvm = true,
            "-O0" => opt0 = true,
            // MESSUNG: Zyklen-Kollektor erzwungen AUS (auch bei zyklischen Typen).
            // Unsound (leckt Zyklen), aber isoliert die Kollektor-Kosten gegen den
            // reinen RC-Pfad — die mittlere Spalte des M0.1-Dreiwegs.
            "--no-cycles" => force_no_cycles = true,
            // MESSUNG (Orakel): RC komplett aus (retain/release No-Op) — die Decke
            // einer idealen Region-Inferenz auf der stabilen Menge. Impliziert
            // --no-cycles. Unsound (leckt), nur für Ceiling-Timing.
            "--no-rc" => {
                force_no_rc = true;
                force_no_cycles = true;
            }
            "-l" => match it.next() {
                Some(l) => link_libs.push(l.clone()),
                None => {
                    eprintln!("-l braucht einen Bibliotheksnamen");
                    exit(2);
                }
            },
            "--obj" => match it.next() {
                Some(o) => link_objs.push(o.clone()),
                None => {
                    eprintln!("--obj braucht eine Datei");
                    exit(2);
                }
            },
            a if a.starts_with("-l") && a.len() > 2 => link_libs.push(a[2..].to_string()),
            other => path = Some(other.to_string()),
        }
    }
    let path = path.unwrap_or_else(|| {
        eprintln!("keine Eingabedatei (.vr)");
        exit(2);
    });
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            exit(1);
        }
    };

    // Front-End: lexen/parsen (mit optionaler nutzerdefinierter Syntax).
    let syntax = load_syntax(&path);
    let (mut module, diags) = vire::parse_with_syntax(&src, syntax);
    if !diags.is_empty() {
        for d in &diags {
            eprintln!("{}", d.render(&src));
        }
        exit(1);
    }
    // FFI aus der Quelle: `extern "…" link "lib"` + `native "…" … """code"""`.
    // Link-Libs → clang `-l`; native-Blöcke → automatisch mitkompiliert.
    let mut native_blocks: Vec<(String, String)> = Vec::new();
    // Nutzt das Programm die eingebaute Python-Brücke (`vire_py_*`)? Dann wird
    // pybridge.c automatisch mitkompiliert + libpython gelinkt — KEIN Nutzer-C.
    let mut want_py_bridge = false;
    for it in &module.items {
        match it {
            vire::ast::Item::Extern { items, links, .. } => {
                link_libs.extend(links.iter().cloned());
                if items.iter().any(|s| s.name.starts_with("vire_py_")) {
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

    // Typinferenz (F5-Kern): un-annotierte Parametertypen ausfüllen. Erkannte
    // Typkonflikte sind echte Fehler → ablehnen (nicht still auf I64 defaulten).
    let type_conflicts = vire::infer_module(&mut module);
    if !type_conflicts.is_empty() {
        for c in &type_conflicts {
            eprintln!("Fehler: {c}");
        }
        exit(1);
    }
    // Absenkung nach crates/ir.
    let mut program = match vire::lower_module(&module) {
        Ok(p) => p,
        Err(errs) => {
            for e in &errs {
                eprintln!("Absenkung: {e}");
            }
            exit(1);
        }
    };
    if !program.functions.iter().any(|f| f.name == "java_main") {
        eprintln!("kein Einstiegspunkt: erwarte `fn main()`");
        exit(1);
    }

    // Solver + Optimierungspässe — identisch zum Java-Treiber.
    let mut s = fastllvm_solver::run(&mut program);
    let _ = fastllvm_solver::elide_redundant_ref_copies(&mut program);
    let _ = fastllvm_solver::fuse_long_compares(&mut program);
    let _ = fastllvm_solver::elide_bounds(&mut program);
    let _ = fastllvm_solver::elide_pending_checks(&mut program);
    s.inlined_calls = fastllvm_solver::inline_program(&mut program);
    s.stack_allocated = fastllvm_solver::stack_allocate(&mut program);
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

    // Ausgabe-Pfad: -o, sonst Dateistamm (bei `run` ein Temp-Binary).
    let out = out.unwrap_or_else(|| {
        if is_run {
            std::env::temp_dir().join(format!("vire-run-{}", std::process::id()))
        } else {
            PathBuf::from(PathBuf::from(&path).file_stem().and_then(|s| s.to_str()).unwrap_or("a"))
        }
    });
    let build_dir = std::env::temp_dir().join(format!("vire-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&build_dir) {
        eprintln!("Build-Verzeichnis: {e}");
        exit(1);
    }
    let ll_path = build_dir.join("program.ll");
    let rt_path = build_dir.join("runtime.c");
    if let Err(e) = std::fs::write(&ll_path, &ll).and_then(|_| std::fs::write(&rt_path, RUNTIME_C)) {
        eprintln!("Schreiben nach {}: {e}", build_dir.display());
        exit(1);
    }
    // Eingebaute Python-Brücke als (Python-)native-Block einreihen → sie durchläuft
    // denselben Auto-Include/-Link-Pfad wie ein Nutzer-`native "python"`-Block.
    if want_py_bridge {
        native_blocks.push(("python".into(), PYBRIDGE_C.to_string()));
    }
    // Eingebettete native-Blöcke in Dateien schreiben (Endung nach ABI) und
    // Kompilier-/Link-Flags für C++/Python automatisch ergänzen.
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
            eprintln!("native-Block schreiben: {e}");
            exit(1);
        }
        native_paths.push(p);
    }
    // Python: Include-Pfad + libpython automatisch (aus python3/sysconfig).
    let mut py_include: Option<String> = None;
    if want_python {
        match python_config() {
            Some((inc, lib)) => {
                py_include = Some(inc);
                link_libs.push(lib);
            }
            None => {
                eprintln!("native \"python\": python3/sysconfig nicht gefunden");
                exit(1);
            }
        }
    }
    let mut cmd = Command::new("clang");
    if opt0 {
        // Messmodus: keine Optimierung, kein LTO → Allokationen bleiben stehen.
        // Section-GC bleibt (strippt nur ungenutzte ganze Funktionen wie den
        // Threads-Pfad, nicht die Allok-Calls in genutzten Funktionen).
        cmd.arg("-O0").arg(&ll_path).arg(&rt_path);
        cmd.args(["-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections"]);
    } else {
        cmd.arg("-O2").arg(&ll_path).arg(&rt_path);
        cmd.args(["-ffunction-sections", "-fdata-sections", "-flto", "-Wl,--gc-sections", "-march=native"]);
    }
    if acyclic || force_no_cycles {
        cmd.arg("-DFASTLLVM_NO_CYCLES");
    }
    if force_no_rc {
        cmd.arg("-DFASTLLVM_NO_RC");
    }
    // Eingebettete native-Quellen + Include-/Stdlib-Flags.
    if let Some(inc) = &py_include {
        cmd.arg(format!("-I{inc}"));
    }
    for p in &native_paths {
        cmd.arg(p);
    }
    if want_cpp {
        link_libs.push("stdc++".into()); // C++-Blöcke brauchen die C++-Stdlib
    }
    // FFI-Linken: Nutzer-Objekte/-Quellen zuerst, dann Bibliotheken. libm immer
    // (Mathe-Intrinsics via extern "C"). clang übersetzt mitgegebene .c/.cpp direkt.
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
            eprintln!("clang schlug fehl ({st}); Zwischendateien in {}", build_dir.display());
            exit(1);
        }
        Err(e) => {
            eprintln!("clang nicht ausführbar: {e}");
            exit(1);
        }
    }

    if is_run {
        let st = Command::new(&out).status();
        let _ = std::fs::remove_file(&out);
        match st {
            Ok(st) => exit(st.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("Ausführen fehlgeschlagen: {e}");
                exit(1);
            }
        }
    }
}
