//! Typinferenz-Tests (F5-Kern): un-annotierte Parameter-/Rückgabetypen werden
//! aus Nutzung + Aufrufstellen erschlossen und in den AST zurückgeschrieben.

use vire::{infer_module, parse};

fn ty_of_param(src: &str, fn_name: &str, idx: usize) -> Option<String> {
    let (mut m, diags) = parse(src);
    assert!(diags.is_empty(), "{diags:?}");
    infer_module(&mut m);
    for it in &m.items {
        if let vire::ast::Item::Fn(f) = it {
            if f.sig.name == fn_name {
                return f.sig.params[idx].ty.as_ref().map(|t| t.name.clone());
            }
        }
    }
    None
}

fn ret_of(src: &str, fn_name: &str) -> Option<String> {
    let (mut m, diags) = parse(src);
    assert!(diags.is_empty(), "{diags:?}");
    infer_module(&mut m);
    for it in &m.items {
        if let vire::ast::Item::Fn(f) = it {
            if f.sig.name == fn_name {
                return f.sig.ret.as_ref().map(|t| t.name.clone());
            }
        }
    }
    None
}

#[test]
fn float_param_aus_arithmetik_und_aufruf() {
    // avg(a,b) mit `(a+b)/2.0` und Aufruf mit Float-Args → Parameter Float.
    let src = "fn avg(a, b) {\n (a + b) / 2.0\n}\nfn main() {\n print(avg(10.0, 20.0))\n}\n";
    assert_eq!(ty_of_param(src, "avg", 0).as_deref(), Some("Float"));
    assert_eq!(ty_of_param(src, "avg", 1).as_deref(), Some("Float"));
    assert_eq!(ret_of(src, "avg").as_deref(), Some("Float"));
}

#[test]
fn int_param_bleibt_int() {
    let src = "fn dbl(x) {\n x * 2\n}\nfn main() {\n print(dbl(21))\n}\n";
    // Int wird als "Int" zurückgeschrieben (ty_of → I64).
    assert_eq!(ty_of_param(src, "dbl", 0).as_deref(), Some("Int"));
}

#[test]
fn typkonflikt_wird_gemeldet_nicht_geschluckt() {
    // `x` als Int UND Float benutzt → Konflikt. Muss gemeldet werden, nicht still
    // auf I64 defaulten (sonst Miskompilat statt Ablehnung).
    let (mut m, diags) = parse("fn bad(x) {\n mut a = x + 1\n mut b = x + 2.0\n a\n}\n");
    assert!(diags.is_empty());
    let conflicts = infer_module(&mut m);
    assert!(!conflicts.is_empty(), "Typkonflikt Int/Float muss gemeldet werden");
}

#[test]
fn main_bleibt_ohne_rueckgabetyp() {
    let src = "fn main() {\n print(1)\n}\n";
    assert_eq!(ret_of(src, "main"), None);
}
