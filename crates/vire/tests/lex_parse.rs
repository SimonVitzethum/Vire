use vire::ast::{Expr, Item, Stmt};
use vire::lexer::{lex, Kw, Tok};

#[test]
fn lex_basics() {
    let (toks, diags): (Vec<_>, _) = lex("fn add(a, b) = a + b");
    assert!(diags.is_empty(), "{:?}", diags);
    let kinds: Vec<_> = toks.iter().map(|t| t.tok.clone()).collect();
    assert_eq!(kinds[0], Tok::Kw(Kw::Fn));
    assert_eq!(kinds[1], Tok::Ident("add".into()));
    assert_eq!(kinds[2], Tok::LParen);
    assert!(matches!(kinds.last(), Some(Tok::Eof)));
}

#[test]
fn lex_numbers_and_ops() {
    let (toks, diags) = lex("x = 0xFF + 1_000 * 3.5 +% 2");
    assert!(diags.is_empty());
    assert!(toks.iter().any(|t| t.tok == Tok::Int(255)));
    assert!(toks.iter().any(|t| t.tok == Tok::Int(1000)));
    assert!(toks.iter().any(|t| matches!(t.tok, Tok::Float(f) if (f - 3.5).abs() < 1e-9)));
    assert!(toks.iter().any(|t| t.tok == Tok::PlusPct));
}

#[test]
fn newline_is_soft_terminator() {
    // Newline after `b` (ends statement) → terminator; after `+` (operator) it does not.
    let (toks, _) = lex("a + b\nc");
    let nl = toks.iter().filter(|t| t.tok == Tok::Newline).count();
    assert_eq!(nl, 1, "genau ein Terminator-Newline erwartet");
    let (toks2, _) = lex("a +\nb");
    assert_eq!(toks2.iter().filter(|t| t.tok == Tok::Newline).count(), 0);
}

#[test]
fn nested_block_comment() {
    let (toks, diags) = lex("a /* x /* y */ z */ b");
    assert!(diags.is_empty());
    let idents: Vec<_> = toks.iter().filter_map(|t| match &t.tok {
        Tok::Ident(s) => Some(s.clone()),
        _ => None,
    }).collect();
    assert_eq!(idents, vec!["a", "b"]);
}

#[test]
fn parse_expr_fn() {
    let (m, diags) = vire::parse("fn add(a, b) = a + b\n");
    assert!(diags.is_empty(), "{:?}", diags);
    assert_eq!(m.items.len(), 1);
    match &m.items[0] {
        Item::Fn(f) => {
            assert_eq!(f.sig.name, "add");
            assert_eq!(f.sig.params.len(), 2);
            assert!(f.body.is_some());
        }
        _ => panic!("erwartete Fn"),
    }
}

#[test]
fn parse_precedence() {
    // 1 + 2 * 3  →  Add(1, Mul(2,3))
    let (m, diags) = vire::parse("fn f() = 1 + 2 * 3\n");
    assert!(diags.is_empty(), "{:?}", diags);
    let Item::Fn(f) = &m.items[0] else { panic!() };
    let tail = f.body.as_ref().unwrap().tail.as_ref().unwrap();
    match tail.as_ref() {
        Expr::Binary { op, rhs, .. } => {
            assert!(matches!(op, vire::ast::BinOp::Add));
            assert!(matches!(rhs.as_ref(), Expr::Binary { op: vire::ast::BinOp::Mul, .. }));
        }
        e => panic!("erwartete Binary Add, fand {e:?}"),
    }
}

#[test]
fn parse_type_sum() {
    let src = "type Shape {\n Circle(radius: Float)\n Rect(w: Float, h: Float)\n Empty\n}\n";
    let (m, diags) = vire::parse(src);
    assert!(diags.is_empty(), "{:?}", diags);
    let Item::Type(t) = &m.items[0] else { panic!("erwartete Type") };
    assert_eq!(t.name, "Shape");
    assert_eq!(t.variants.len(), 3);
    assert_eq!(t.variants[0].name, "Circle");
}

#[test]
fn parse_match_and_for() {
    let src = "fn area(s) {\n match s {\n Circle(r) -> 3.14 * r * r\n Rect(w, h) -> w * h\n }\n}\n";
    let (m, diags) = vire::parse(src);
    assert!(diags.is_empty(), "{:?}", diags);
    let Item::Fn(f) = &m.items[0] else { panic!() };
    let tail = f.body.as_ref().unwrap().tail.as_ref().unwrap();
    assert!(matches!(tail.as_ref(), Expr::Match { .. }));
}

#[test]
fn parse_method_chain_multiline() {
    // Leading-dot chain across lines
    let src = "fn f(xs) = xs.map(g)\n  .filter(h)\n  .len()\n";
    let (m, diags) = vire::parse(src);
    assert!(diags.is_empty(), "{:?}", diags);
    let Item::Fn(f) = &m.items[0] else { panic!() };
    // outermost expression is a Call (.len())
    assert!(matches!(f.body.as_ref().unwrap().tail.as_ref().unwrap().as_ref(), Expr::Call { .. }));
}

#[test]
fn parse_let_and_while() {
    let src = "fn main() {\n mut i = 0\n while i < 10 {\n i = i + 1\n }\n}\n";
    let (m, diags) = vire::parse(src);
    assert!(diags.is_empty(), "{:?}", diags);
    let Item::Fn(f) = &m.items[0] else { panic!() };
    let b = f.body.as_ref().unwrap();
    assert!(matches!(b.stmts[0], Stmt::Let { mutable: true, .. }));
    assert!(matches!(b.stmts[1], Stmt::While { .. }));
}

#[test]
fn parse_capsule() {
    let src = "fn f(x) = capsule(x) {\n mut g = build(x)\n step(g)\n g\n}\n";
    let (m, diags) = vire::parse(src);
    assert!(diags.is_empty(), "{:?}", diags);
    let Item::Fn(f) = &m.items[0] else { panic!() };
    assert!(matches!(f.body.as_ref().unwrap().tail.as_ref().unwrap().as_ref(), Expr::Capsule { .. }));
}
