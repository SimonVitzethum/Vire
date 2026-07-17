//! Absenkungs-Tests: Vire-AST → crates/ir. Prüfen die M2-Semantik strukturell
//! (ohne clang) — Einstiegspunkt, Rückgabetyp-Schätzung, Binding-vs-Zuweisung,
//! Schleifen/Kontrollfluss.

use fastllvm_ir::Ty;
use vire::{lower_module, parse};

fn lower(src: &str) -> fastllvm_ir::Program {
    let (m, diags) = parse(src);
    assert!(diags.is_empty(), "Parse-Diagnosen: {diags:?}");
    lower_module(&m).unwrap_or_else(|e| panic!("Absenkung: {e:?}"))
}

#[test]
fn main_wird_java_main_und_void() {
    let p = lower("fn main() {\n print(1)\n}\n");
    let f = p.functions.iter().find(|f| f.name == "java_main").expect("java_main");
    assert_eq!(f.ret, Ty::Void);
}

#[test]
fn tail_ausdruck_bestimmt_rueckgabetyp() {
    // Ohne `-> T`: Rückgabetyp aus dem Tail geschätzt (Ident → I64-Default).
    let p = lower("fn f(n) {\n mut a = n\n a\n}\n");
    let f = p.functions.iter().find(|f| f.name == "f").unwrap();
    assert_eq!(f.ret, Ty::I64);
}

#[test]
fn float_tail_wird_f64() {
    let p = lower("fn f() {\n 1.5 + 2.5\n}\n");
    let f = p.functions.iter().find(|f| f.name == "f").unwrap();
    assert_eq!(f.ret, Ty::F64);
}

#[test]
fn binding_dann_zuweisung_kein_shadowing() {
    // `mut s = 0` bindet; `s = s + 1` im Rumpf ist Zuweisung auf DENSELBEN Local
    // (kein neuer). Erwartung: nur ein Local trägt `s` → Akku funktioniert.
    let p = lower("fn main() {\n mut s = 0\n for i in 0..3 { s = s + i }\n print(s)\n}\n");
    let f = p.functions.iter().find(|f| f.name == "java_main").unwrap();
    // Der Akku-Local (0) wird mehrfach zugewiesen (Init + Reassign in der
    // Schleife), nicht je Iteration frisch gebunden (kein Shadowing).
    let assigns_to_zero = f.blocks.iter().flat_map(|b| &b.statements).filter(|s| {
        matches!(s, fastllvm_ir::Statement::Assign(l, _) if l.0 == 0)
    }).count();
    assert!(assigns_to_zero >= 2, "erwarte Init + Reassign auf Local 0 (s), fand {assigns_to_zero}");
}

#[test]
fn if_als_ausdruck_liefert_wert() {
    // `if a>b {a} else {b}` als Tail → Funktion gibt I64 zurück (nicht Void),
    // und der merge-Block liefert ein Ergebnis-Local.
    let p = lower("fn max(a, b) {\n if a > b { a } else { b }\n}\n");
    let f = p.functions.iter().find(|f| f.name == "max").unwrap();
    assert_eq!(f.ret, Ty::I64);
    // Der letzte Block gibt einen Wert zurück (Return(Some(..))), kein Return(None).
    let has_value_return = f.blocks.iter().any(|b| matches!(&b.terminator, fastllvm_ir::Terminator::Return(Some(_))));
    assert!(has_value_return, "if-Ausdruck muss einen Wert zurückgeben");
}

#[test]
fn string_literale_landen_im_pool() {
    let p = lower("fn main() {\n print(\"a\")\n print(\"b\")\n print(\"a\")\n}\n");
    // Zwei eindeutige Literale (a dedupliziert), print(str)→jrt_println_str.
    assert_eq!(p.strings, vec!["a".to_string(), "b".to_string()]);
    let calls_str = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "jrt_println_str")
    });
    assert!(calls_str, "print(str) muss jrt_println_str rufen");
}

#[test]
fn produkttyp_new_und_feldzugriff() {
    let src = "type P {\n x: Int\n y: Int\n}\nfn main() {\n mut p = P(3, 4)\n print(p.x)\n}\n";
    let p = lower(src);
    // Klasse P registriert mit zwei I64-Feldern.
    let c = p.classes.iter().find(|c| c.name == "P").expect("Klasse P");
    assert_eq!(c.fields.len(), 2);
    assert_eq!(c.fields[0].name, "x");
    assert_eq!(c.fields[0].ty, Ty::I64);
    // Konstruktion → New + zwei PutField; Zugriff → GetField.
    let stmts: Vec<_> = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).collect();
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::New { class, .. } if class == "P")));
    assert_eq!(stmts.iter().filter(|s| matches!(s, fastllvm_ir::Statement::PutField { .. })).count(), 2);
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::GetField { field, .. } if field == "x")));
}

#[test]
fn break_ausserhalb_schleife_ist_fehler() {
    let (m, _) = parse("fn main() {\n break\n}\n");
    assert!(lower_module(&m).is_err());
}
