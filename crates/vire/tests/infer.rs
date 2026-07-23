//! Type-inference tests (F5 core): un-annotated parameter/return types are
//! derived from usage + call sites and written back into the AST.

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
fn float_param_from_arithmetic_and_call() {
    // avg(a,b) with `(a+b)/2.0` and a call with float args → parameter Float.
    let src = "fn avg(a, b) {\n (a + b) / 2.0\n}\nfn main() {\n print(avg(10.0, 20.0))\n}\n";
    assert_eq!(ty_of_param(src, "avg", 0).as_deref(), Some("Float"));
    assert_eq!(ty_of_param(src, "avg", 1).as_deref(), Some("Float"));
    assert_eq!(ret_of(src, "avg").as_deref(), Some("Float"));
}

#[test]
fn int_param_stays_int() {
    let src = "fn dbl(x) {\n x * 2\n}\nfn main() {\n print(dbl(21))\n}\n";
    // Int is written back as "Int" (ty_of → I64).
    assert_eq!(ty_of_param(src, "dbl", 0).as_deref(), Some("Int"));
}

#[test]
fn type_conflict_is_reported_not_swallowed() {
    // A genuine type conflict must be reported, not silently defaulted (which would
    // miscompile instead of reject). `x` used as Int (`x + 1`) AND as a String
    // (`use_str(x)`) → Int vs Ref, unresolvable. (Int/Float is NOT a conflict — it's
    // numeric promotion — so the conflict here is Int-vs-object, which promotion leaves
    // alone.)
    let (mut m, diags) = parse(
        "fn use_str(s: Str) -> Int {\n 0\n}\nfn bad(x) {\n mut a = x + 1\n mut b = use_str(x)\n a\n}\n",
    );
    assert!(diags.is_empty());
    let conflicts = infer_module(&mut m);
    assert!(!conflicts.is_empty(), "a real Int/object type conflict must be reported");
}

#[test]
fn int_float_arithmetic_promotes_not_conflicts() {
    // Mixing a concrete Int with a Float in arithmetic promotes the Int to Float — no
    // conflict — so `i * 2.0` on an Int local (and `i < 2.5`, `i / n * pi`) needs no cast.
    let (mut m, diags) = parse(
        "fn main() {\n mut i = 3\n mut n = 5\n print(i * 2.0)\n print(i < 2.5)\n print(i * 1.0 / (n * 1.0))\n}\n",
    );
    assert!(diags.is_empty());
    let conflicts = infer_module(&mut m);
    assert!(conflicts.is_empty(), "concrete Int*Float should promote, not conflict: {conflicts:?}");
}

#[test]
fn main_stays_without_return_type() {
    let src = "fn main() {\n print(1)\n}\n";
    assert_eq!(ret_of(src, "main"), None);
}
