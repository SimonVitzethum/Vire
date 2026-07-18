//! Absenkungs-Tests: Vire-AST → crates/ir. Prüfen die M2-Semantik strukturell
//! (ohne clang) — Einstiegspunkt, Rückgabetyp-Schätzung, Binding-vs-Zuweisung,
//! Schleifen/Kontrollfluss.

use fastllvm_ir::Ty;
use vire::{infer_module, lower_module, parse};

fn lower(src: &str) -> fastllvm_ir::Program {
    // Reale Pipeline: parsen → Typinferenz → absenken.
    let (mut m, diags) = parse(src);
    assert!(diags.is_empty(), "Parse-Diagnosen: {diags:?}");
    let conflicts = infer_module(&mut m);
    assert!(conflicts.is_empty(), "Typkonflikte: {conflicts:?}");
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
fn feldmutation_erzeugt_putfield() {
    let src = "type C {\n n: Int\n}\nfn main() {\n mut c = C(0)\n c.n = 5\n c.n += 1\n}\n";
    let p = lower(src);
    let puts = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .filter(|s| matches!(s, fastllvm_ir::Statement::PutField { field, .. } if field == "n")).count();
    // 1× Konstruktion + `= 5` + `+= 1` = 3 PutField auf n.
    assert_eq!(puts, 3);
}

#[test]
fn capsule_umklammert_rumpf_mit_arena() {
    // Reine Form: arena_push vor dem Rumpf, arena_pop danach; Skalar-Ergebnis raus.
    let p = lower("fn f(n) {\n capsule(n) {\n mut s = 0\n s = s + n\n s\n }\n}\n");
    let f = p.functions.iter().find(|f| f.name == "f").unwrap();
    assert_eq!(f.ret, Ty::I64);
    let calls: Vec<&str> = f.blocks.iter().flat_map(|b| &b.statements).filter_map(|s| {
        if let fastllvm_ir::Statement::Call { func, .. } = s { Some(func.as_str()) } else { None }
    }).collect();
    assert!(calls.contains(&"jrt_arena_push"), "arena_push fehlt");
    assert!(calls.contains(&"jrt_arena_pop"), "arena_pop fehlt");
}

#[test]
fn capsule_ref_ergebnis_ist_fehler() {
    // Objekt-Ergebnis würde in die freigegebene Arena zeigen → harter Fehler.
    let (mut m, _) = parse("type P {\n x: Int\n}\nfn f(n) {\n capsule(n) {\n P(n)\n }\n}\n");
    let _ = infer_module(&mut m);
    let errs = lower_module(&m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("capsule") && e.contains("Objekt-Ergebnis")));
}

#[test]
fn capsule_objekt_eingabe_ist_fehler() {
    // Der gefährliche Fall (aliasierte Ref-Eingabe) muss ein HARTER Fehler sein,
    // kein stiller Stub — sonst verspräche capsule Containment ohne es zu liefern.
    let (m, _) = parse("type P {\n x: Int\n}\nfn f(p: P) -> Int {\n capsule(p) {\n p.x\n }\n}\n");
    let errs = lower_module(&m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("capsule") && e.contains("Objekt-Eingabe")));
}

#[test]
fn null_senkt_zu_constnull() {
    // `null` (Mess-Bootstrap) → ConstNull; erlaubt verkettete/zyklische Graphen.
    let src = "type N {\n next: N\n v: Int\n}\nfn main() {\n mut a = N(null, 1)\n print(a.v)\n}\n";
    let p = lower(src);
    let has_null = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::PutField { value: fastllvm_ir::Operand::ConstNull, .. })
    });
    assert!(has_null, "null muss als ConstNull ins next-Feld");
}

#[test]
fn return_statement_liefert_typisierten_wert() {
    // Funktion, deren Wert aus einem `return`-Statement kommt (kein Tail): der
    // unerreichbare Fallthrough muss typkorrekt terminieren, nicht `ret void`.
    let p = lower("fn f(n) {\n mut s = 0\n s = s + n\n return s\n}\n");
    let f = p.functions.iter().find(|f| f.name == "f").unwrap();
    assert_eq!(f.ret, Ty::I64);
    // Kein Return(None) in einer I64-Funktion.
    let has_void_return = f.blocks.iter().any(|b| matches!(&b.terminator, fastllvm_ir::Terminator::Return(None)));
    assert!(!has_void_return, "I64-Funktion darf kein Return(None) haben");
}

#[test]
fn extern_c_aufruf_loest_direkt_auf() {
    // extern "C"-Signatur registriert → Aufruf lowert als direkter Call (kein
    // Mangling); das Backend deklariert, clang linkt.
    let p = lower("extern \"C\" {\n fn sqrt(x: F64) -> F64\n}\nfn main() {\n print(sqrt(16.0))\n}\n");
    let calls_sqrt = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "sqrt")
    });
    assert!(calls_sqrt, "extern-Aufruf muss als Call(sqrt) lowern");
}

#[test]
fn break_ausserhalb_schleife_ist_fehler() {
    let (m, _) = parse("fn main() {\n break\n}\n");
    assert!(lower_module(&m).is_err());
}

#[test]
fn methoden_und_impl_bloecke() {
    let src = "type P {\n x: Int\n y: Int\n}\nimpl P {\n fn sum(self) -> Int { self.x + self.y }\n}\nfn main() {\n mut p = P(3, 4)\n print(p.sum())\n}\n";
    let p = lower(src);
    // Methode als Funktion `P.sum` mit self-Ref-Parameter registriert + abgesenkt.
    let m = p.functions.iter().find(|f| f.name == "P.sum").expect("P.sum");
    assert_eq!(m.params.first().copied(), Some(Ty::Ref)); // self
    // Aufrufstelle emittiert Call(P.sum, [p]).
    let calls = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "P.sum")
    });
    assert!(calls, "Methodenaufruf muss Call(P.sum) emittieren");
}

#[test]
fn summentyp_und_match() {
    let src = "type Sh {\n Circle(r: Int)\n Rect(w: Int, h: Int)\n Empty\n}\nfn area(s: Sh) -> Int {\n match s {\n Circle(r) -> r * r\n Rect(w, h) -> w * h\n Empty -> 0\n }\n}\nfn main() {\n print(area(Circle(3)))\n}\n";
    let p = lower(src);
    // Getaggte Klasse Sh mit __tag als erstem Feld.
    let c = p.classes.iter().find(|c| c.name == "Sh").expect("Sh");
    assert_eq!(c.fields[0].name, "__tag");
    // Konstruktion setzt __tag; match liest __tag (GetField __tag).
    let reads_tag = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::GetField { field, .. } if field == "__tag")
    });
    assert!(reads_tag, "match muss __tag lesen");
}

#[test]
fn listen_und_comprehensions() {
    // List-Literal → NewArray+ArrayStore; Comprehension mit Filter → NewArray + Loop.
    let p = lower("fn main() {\n mut xs = [1, 2, 3]\n mut ys = [x * x for x in xs if x > 1]\n print(ys.len())\n}\n");
    let stmts: Vec<_> = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).collect();
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::NewArray { .. })), "List/Comprehension braucht NewArray");
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::ArrayLoad { .. })), "Comprehension iteriert (ArrayLoad)");
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::ArrayLen { .. })), ".len()/Iteration braucht ArrayLen");
}
