//! Nutzer-konfigurierbare Schlüsselwort-Schreibweisen.

use vire::ast::Item;
use vire::{parse_with_syntax, Syntax};

#[test]
fn renamed_keywords_parse() {
    let cfg = "fn = funktion\nreturn = gib\nwhile = solange\nmut = veraenderlich\n";
    let syn = Syntax::parse(cfg).expect("config");
    let src = "funktion f(n) {\n veraenderlich s = 0\n solange s < n { s = s + 1 }\n gib s\n}\n";
    let (m, diags) = parse_with_syntax(src, syn);
    assert!(diags.is_empty(), "{diags:?}");
    let Item::Fn(f) = &m.items[0] else { panic!("erwarte Fn") };
    assert_eq!(f.sig.name, "f");
}

#[test]
fn default_keyword_is_free_after_rename() {
    // Nach `fn = funktion` ist `fn` ein normaler Bezeichner, `funktion` das Keyword.
    let syn = Syntax::parse("fn = funktion\n").unwrap();
    let (m, diags) = parse_with_syntax("funktion fn() {\n print(1)\n}\n", syn);
    assert!(diags.is_empty(), "{diags:?}");
    let Item::Fn(f) = &m.items[0] else { panic!() };
    assert_eq!(f.sig.name, "fn"); // `fn` jetzt als Name nutzbar
}

#[test]
fn collision_is_rejected() {
    // Zwei Schlüsselwörter auf dieselbe Schreibweise → Fehler.
    let err = Syntax::parse("fn = x\ntype = x\n").unwrap_err();
    assert!(err.iter().any(|e| e.contains("schon")));
}

#[test]
fn unknown_keyword_is_rejected() {
    let err = Syntax::parse("funktion = f\n").unwrap_err();
    assert!(err.iter().any(|e| e.contains("unbekannt")));
}

#[test]
fn top_level_statements_become_main() {
    // Skript-Stil: Top-Level-Anweisungen → implizites fn main().
    let (m, diags) = parse_with_syntax("mut s = 0\nfor i in 0..3 { s = s + i }\nprint(s)\n", Syntax::default());
    assert!(diags.is_empty(), "{diags:?}");
    let has_main = m.items.iter().any(|it| matches!(it, Item::Fn(f) if f.sig.name == "main"));
    assert!(has_main, "Top-Level-Anweisungen müssen ein main erzeugen");
}

#[test]
fn top_level_and_explicit_main_conflict() {
    let (_, diags) = parse_with_syntax("print(1)\nfn main() { print(2) }\n", Syntax::default());
    assert!(!diags.is_empty(), "beides zugleich muss ein Fehler sein");
}
