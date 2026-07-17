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
    let mut freestanding = false;
    let mut threads = false;
    let mut main_override: Option<String> = None;
    let mut raw_inputs: Vec<PathBuf> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => match args.next() {
                Some(p) => out = Some(PathBuf::from(p)),
                None => die("-o braucht ein Argument"),
            },
            "--main" => match args.next() {
                Some(c) => main_override = Some(c.replace('.', "/")),
                None => die("--main braucht einen Klassennamen"),
            },
            "--emit-ir" => emit_ir = true,
            "--emit-llvm" => emit_llvm = true,
            "--stats" => stats = true,
            "--no-solver" => no_solver = true,
            "--freestanding" => freestanding = true,
            "--threads" => threads = true,
            "-h" | "--help" => {
                println!("Aufruf: fastjavac [-o BIN] [--main KLASSE] [--emit-ir] [--emit-llvm] [--stats] [--no-solver] [--freestanding] (KLASSE.class | LIB.jar) ...");
                return;
            }
            _ => raw_inputs.push(PathBuf::from(a)),
        }
    }
    if raw_inputs.is_empty() {
        die("keine Eingabedateien (erwartet .class oder .jar)");
    }

    // JARs entpacken (Closed-World-Sammlung): jede .class-Datei wird Input,
    // die Manifest-`Main-Class` bestimmt den Einstiegspunkt. Das Extraktions-
    // verzeichnis muss bis zum Ende der Kompilierung leben.
    let extract_root = std::env::temp_dir().join(format!("fastjavac-jars-{}", std::process::id()));
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut manifest_main: Option<String> = None;
    for path in &raw_inputs {
        if path.extension().and_then(|e| e.to_str()) == Some("jar") {
            match unpack_jar(path, &extract_root) {
                Ok((classes, main)) => {
                    inputs.extend(classes);
                    if manifest_main.is_none() {
                        manifest_main = main;
                    }
                }
                Err(e) => return die(&format!("{}: {e}", path.display())),
            }
        } else {
            inputs.push(path.clone());
        }
    }
    if inputs.is_empty() {
        die("keine .class-Dateien gefunden (leeres JAR?)");
    }
    let main_class = main_override.or(manifest_main);

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
    // Klassendaten sind eingelesen — das JAR-Extraktionsverzeichnis kann weg.
    let _ = std::fs::remove_dir_all(&extract_root);

    let mut program = fastllvm_ir::Program::default();
    program.main_class = main_class;
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

    // Azyklisch beweisbar → Zyklen-Collector entfällt (nur wenn der Solver
    // lief; ohne ihn konservativ Collector behalten).
    let mut acyclic = false;
    if !no_solver {
        let mut s = fastllvm_solver::run(&mut program);
        s.inlined_calls = fastllvm_solver::inline_program(&mut program);
        s.stack_allocated = fastllvm_solver::stack_allocate(&mut program);
        acyclic = s.acyclic;
        if stats {
            eprintln!(
                "solver: {} Klassen instanziiert, {} Funktionen erreichbar ({} entfernt), \
                 {} virtuelle Sites, {} devirtualisiert, {} bikonditional, {} Calls geinlinet, \
                 {} Allokationen auf den Stack, Zyklen-Collector: {}",
                s.instantiated_classes,
                s.reachable_functions,
                s.pruned_functions,
                s.virtual_sites,
                s.devirtualized,
                s.poly_devirtualized,
                s.inlined_calls,
                s.stack_allocated,
                if s.acyclic { "entfällt (azyklisch)" } else { "nötig" },
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

    // Freestanding (seL4): keine libc, kein Startup. Ergebnis ist ein
    // relozierbares Objekt (`clang -r`), das die Zielumgebung mit ihrem
    // _start und den schwachen Hooks (jrt_debug_putchar/jrt_platform_halt)
    // zusammenlinkt.
    let mut cmd = Command::new("clang");
    cmd.arg("-O2").arg(&ll_path).arg(&rt_path);
    // Phase 2: jede Funktion/Datenobjekt in eigene Section, ungenutzte beim
    // Linken strippen — so zieht z.B. `Hello` nur die tatsächlich gerufenen
    // jrt_-Funktionen statt der ganzen Runtime.
    cmd.args(["-ffunction-sections", "-fdata-sections"]);
    if !freestanding {
        cmd.arg("-Wl,--gc-sections");
    }
    if threads {
        cmd.args(["-DFASTLLVM_THREADS", "-pthread"]);
    }
    if acyclic {
        // Phase 1: kein Typ zyklenfähig → Zyklen-Collector wird nicht mitgelinkt,
        // retain/release werden farb-/pufferfrei.
        cmd.arg("-DFASTLLVM_NO_CYCLES");
    }
    if freestanding {
        cmd.args([
            "-r",
            "-nostdlib",
            "-ffreestanding",
            "-fno-stack-protector",
            "-DFASTLLVM_FREESTANDING",
        ]);
    }
    let status = cmd.arg("-o").arg(&out).status();
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

/// Entpackt ein JAR (ZIP) nach `<root>/<jar-stem>/` und liefert die Liste der
/// enthaltenen `.class`-Dateien sowie die `Main-Class` aus dem Manifest.
/// Nutzt `unzip` bzw. das JDK-`jar` (beide bei einer Java-Toolchain vorhanden);
/// so bleibt der Compiler dependency-frei und die Runtime unberührt.
fn unpack_jar(jar: &std::path::Path, root: &std::path::Path) -> std::io::Result<(Vec<PathBuf>, Option<String>)> {
    let stem = jar.file_stem().and_then(|s| s.to_str()).unwrap_or("jar");
    let dir = root.join(stem);
    std::fs::create_dir_all(&dir)?;
    let jar_abs = std::fs::canonicalize(jar)?;
    // Bevorzugt `unzip`, sonst `jar xf` (im Zielverzeichnis ausgeführt).
    let ok = Command::new("unzip")
        .args(["-oq"])
        .arg(&jar_abs)
        .arg("-d")
        .arg(&dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        || Command::new("jar")
            .arg("xf")
            .arg(&jar_abs)
            .current_dir(&dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    if !ok {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Entpacken fehlgeschlagen (weder `unzip` noch `jar` verfügbar)",
        ));
    }
    // .class-Dateien rekursiv einsammeln.
    let mut classes = Vec::new();
    collect_classes(&dir, &mut classes)?;
    // Main-Class aus dem Manifest (dotted → intern mit '/').
    let manifest = dir.join("META-INF").join("MANIFEST.MF");
    let main = std::fs::read_to_string(&manifest).ok().and_then(|txt| {
        txt.lines()
            .find_map(|l| l.strip_prefix("Main-Class:"))
            .map(|v| v.trim().replace('.', "/"))
    });
    Ok((classes, main))
}

fn collect_classes(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            collect_classes(&p, out)?;
        } else if p.extension().and_then(|e| e.to_str()) == Some("class") {
            out.push(p);
        }
    }
    Ok(())
}
