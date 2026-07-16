//! fastjavac — Java-Classfiles → natives Binary.
//!
//! Pipeline (DESIGN.md §2, Stufe 1 der Priorisierung §7):
//!   .class → Parser → Mittel-IR → textuelles LLVM-IR → clang → Binary
//!
//! Aufruf: fastjavac [-o BIN] [--emit-ir] [--emit-llvm] KLASSE.class ...

use std::path::PathBuf;
use std::process::{exit, Command};

const RUNTIME_C: &str = include_str!("runtime.c");

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut emit_ir = false;
    let mut emit_llvm = false;
    let mut stats = false;
    let mut no_solver = false;
    let mut inputs: Vec<PathBuf> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => match args.next() {
                Some(p) => out = Some(PathBuf::from(p)),
                None => die("-o braucht ein Argument"),
            },
            "--emit-ir" => emit_ir = true,
            "--emit-llvm" => emit_llvm = true,
            "--stats" => stats = true,
            "--no-solver" => no_solver = true,
            "-h" | "--help" => {
                println!("Aufruf: fastjavac [-o BIN] [--emit-ir] [--emit-llvm] [--stats] [--no-solver] KLASSE.class ...");
                return;
            }
            _ => inputs.push(PathBuf::from(a)),
        }
    }
    if inputs.is_empty() {
        die("keine Eingabedateien (erwartet .class)");
    }

    // Zweiphasig: erst alle Klassen registrieren (Closed World), dann
    // absenken — Feld-/Methodenauflösung geht über Klassengrenzen.
    let mut classfiles = Vec::new();
    for path in &inputs {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => return die(&format!("{}: {e}", path.display())),
        };
        match fastllvm_classfile::ClassFile::parse(&data) {
            Ok(cf) => classfiles.push((path, cf)),
            Err(e) => return die(&format!("{}: {e}", path.display())),
        }
    }
    let mut program = fastllvm_ir::Program::default();
    for (path, cf) in &classfiles {
        if let Err(e) = fastllvm_frontend::register_class(cf, &mut program) {
            return die(&format!("{}: {e}", path.display()));
        }
    }
    fastllvm_frontend::register_builtins(&mut program);
    for (path, cf) in &classfiles {
        if let Err(e) = fastllvm_frontend::lower_class(cf, &mut program) {
            return die(&format!("{}: {e}", path.display()));
        }
    }

    if !no_solver {
        let mut s = fastllvm_solver::run(&mut program);
        s.inlined_calls = fastllvm_solver::inline_program(&mut program);
        s.stack_allocated = fastllvm_solver::stack_allocate(&mut program);
        if stats {
            eprintln!(
                "solver: {} Klassen instanziiert, {} Funktionen erreichbar ({} entfernt), \
                 {} virtuelle Sites, {} devirtualisiert, {} bikonditional, {} Calls geinlinet, \
                 {} Allokationen auf den Stack",
                s.instantiated_classes,
                s.reachable_functions,
                s.pruned_functions,
                s.virtual_sites,
                s.devirtualized,
                s.poly_devirtualized,
                s.inlined_calls,
                s.stack_allocated,
            );
        }
    }

    if emit_ir {
        print!("{program}");
        return;
    }

    let ll = fastllvm_backend::emit(&program);
    if emit_llvm {
        print!("{ll}");
        return;
    }

    let out = out.unwrap_or_else(|| PathBuf::from("a.out"));
    let build_dir = std::env::temp_dir().join(format!("fastjavac-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&build_dir) {
        return die(&format!("Build-Verzeichnis: {e}"));
    }
    let ll_path = build_dir.join("program.ll");
    let rt_path = build_dir.join("runtime.c");
    if let Err(e) = std::fs::write(&ll_path, &ll).and_then(|_| std::fs::write(&rt_path, RUNTIME_C)) {
        return die(&format!("Schreiben nach {}: {e}", build_dir.display()));
    }

    let status = Command::new("clang")
        .arg("-O2")
        .arg(&ll_path)
        .arg(&rt_path)
        .arg("-o")
        .arg(&out)
        .status();
    match status {
        Ok(s) if s.success() => {
            let _ = std::fs::remove_dir_all(&build_dir);
        }
        Ok(s) => die(&format!("clang schlug fehl ({s}); Zwischendateien in {}", build_dir.display())),
        Err(e) => die(&format!("clang nicht ausführbar: {e}")),
    }
}

fn die(msg: &str) {
    eprintln!("fastjavac: {msg}");
    exit(1);
}
