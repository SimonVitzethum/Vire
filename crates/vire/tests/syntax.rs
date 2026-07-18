//! User-configurable keyword spellings.

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
    // After `fn = funktion`, `fn` is a normal identifier, `funktion` the keyword.
    let syn = Syntax::parse("fn = funktion\n").unwrap();
    let (m, diags) = parse_with_syntax("funktion fn() {\n print(1)\n}\n", syn);
    assert!(diags.is_empty(), "{diags:?}");
    let Item::Fn(f) = &m.items[0] else { panic!() };
    assert_eq!(f.sig.name, "fn"); // `fn` now usable as a name
}

#[test]
fn collision_is_rejected() {
    // Two keywords on the same spelling → error.
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
    // Script style: top-level statements → implicit fn main().
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

#[test]
fn triple_quoted_raw_string() {
    // """…""" is a multi-line raw string: backslash + inner " literally.
    let src = "\"\"\"a\\n b\"\"\"".to_string(); // source: """a\n b"""
    let (toks, diags) = vire::lexer::lex(&src);
    assert!(diags.is_empty(), "{diags:?}");
    let got = toks.iter().find_map(|t| match &t.tok {
        vire::lexer::Tok::Str(s) => Some(s.clone()),
        _ => None,
    });
    assert_eq!(got.as_deref(), Some("a\\n b")); // backslash-n NOT interpreted
}

#[test]
fn native_and_link_parse() {
    let src = "native \"c++\" link \"stdc++\" \"\"\"\nextern \"C\" int f(){return 1;}\n\"\"\"\nextern \"C\" link \"m\" {\n fn f() -> I32\n}\n";
    let (m, diags) = parse_with_syntax(src, Syntax::default());
    assert!(diags.is_empty(), "{diags:?}");
    let has_native = m.items.iter().any(|it| matches!(it, vire::ast::Item::Native { .. }));
    let has_extern_link = m.items.iter().any(|it| matches!(it, vire::ast::Item::Extern { links, .. } if !links.is_empty()));
    assert!(has_native && has_extern_link);
}

#[test]
fn extern_header_directive_parses() {
    let (m, diags) = parse_with_syntax("extern \"C\" header \"geo.h\" link \"geo\"\nprint(1)\n", Syntax::default());
    assert!(diags.is_empty(), "{diags:?}");
    let has = m.items.iter().any(|it| matches!(it, vire::ast::Item::Extern { header: Some(h), .. } if h == "geo.h"));
    assert!(has, "extern header-Direktive muss parsen");
}
