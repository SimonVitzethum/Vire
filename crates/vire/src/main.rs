//! `vire` — Compiler-Treiber (Front-End-Stand: lexen + parsen + AST-Dump).
//! Aufruf: `vire parse DATEI.vr` | `vire lex DATEI.vr`.

use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
            eprintln!("unbekannter Befehl: {other} (parse|lex)");
            exit(2);
        }
    }
}
