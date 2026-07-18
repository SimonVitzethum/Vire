//! Absenkungs-Tests: Vire-AST → crates/ir. Prüfen die M2-Semantik strukturell
//! (ohne clang) — Einstiegspunkt, Rückgabetyp-Schätzung, Binding-vs-Zuweisung,
//! Schleifen/Kontrollfluss.

use fastllvm_ir::Ty;
use vire::{expand_macros, infer_module, inline_recursion, lower_module, parse};

#[test]
fn shallow_recursive_inlining_reduziert_selbstaufrufe() {
    // fib: 2 Selbstaufrufe. Nach dem shallow-inline-Pass (Tiefe 2) hat der Rumpf
    // deutlich MEHR Aufruf-Statements (aufgefaltete Kopien) und die verbleibenden
    // Selbstaufrufe liegen tiefer — die Call-Zahl je Frame sinkt drastisch.
    let (mut m, _) = parse("fn fib(n: Int) -> Int {\n if n < 2 { n } else { fib(n - 1) + fib(n - 2) }\n}\nfn main() { print(fib(10)) }\n");
    expand_macros(&mut m).unwrap();
    inline_recursion(&mut m);
    let _ = infer_module(&mut m);
    let p = lower_module(&m).unwrap();
    let fib = p.functions.iter().find(|f| f.name == "fib").expect("fib");
    let calls = fib.blocks.iter().flat_map(|b| &b.statements).filter(|s| matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "fib")).count();
    // Naiv wären es 2 Calls; nach Tiefe-2-Entfaltung stehen viele aufgefaltete
    // fib-Calls im Rumpf (jeder Frame deckt 3 Ebenen ab).
    assert!(calls > 2, "shallow-inline muss die Selbstaufrufe auffalten (>2), fand {calls}");
}

fn lower(src: &str) -> fastllvm_ir::Program {
    // Reale Pipeline: parsen → Makro-Expansion → Typinferenz → absenken.
    let (mut m, diags) = parse(src);
    assert!(diags.is_empty(), "Parse-Diagnosen: {diags:?}");
    expand_macros(&mut m).unwrap_or_else(|e| panic!("Makro-Expansion: {e:?}"));
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

#[test]
fn match_erschoepfung_ist_pflicht() {
    // Nicht-erschöpfendes match = HARTER FEHLER (kein stiller Default mehr).
    let (mut m, _) = parse("type T {\n A(x: Int)\n B\n}\nfn f(t: T) -> Int {\n match t {\n A(x) -> x\n }\n}\n");
    let _ = infer_module(&mut m);
    let errs = lower_module(&m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("erschöpf")), "nicht-erschöpfendes match muss Fehler sein: {errs:?}");
}

#[test]
fn match_verschachtelt_bindet_korrekt() {
    // Verschachteltes Muster B(A(y)) bindet y (kein stilles Ignorieren mehr).
    let src = "type T {\n A(x: Int)\n B(i: T)\n C\n}\nfn f(t: T) -> Int {\n match t {\n B(A(y)) -> y\n A(x) -> x\n B(z) -> 0\n C -> 0\n }\n}\nfn main() {\n print(f(C))\n}\n";
    let p = lower(src); // kompiliert = erschöpfend + verschachtelt akzeptiert
    assert!(p.functions.iter().any(|f| f.name == "f"));
}

#[test]
fn string_concat_und_auto_konvert() {
    let p = lower("fn main() {\n mut n = 42\n print(\"n=\" + n)\n}\n");
    let calls: Vec<&str> = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .filter_map(|s| if let fastllvm_ir::Statement::Call { func, .. } = s { Some(func.as_str()) } else { None }).collect();
    assert!(calls.contains(&"jrt_str_concat"), "String-+ muss jrt_str_concat rufen");
    assert!(calls.contains(&"jrt_long_to_str"), "Int im +-String muss konvertiert werden");
}

#[test]
fn generics_monomorphisieren_pro_typ() {
    // id[T] wird pro Aufruf-Typ instanziiert: id$Int, id$Float.
    let p = lower("fn id[T](x: T) -> T { x }\nfn main() {\n print(id(1))\n print(id(2.5))\n}\n");
    let names: Vec<&str> = p.functions.iter().map(|f| f.name.as_str()).collect();
    assert!(names.iter().any(|n| n.starts_with("id$Int")), "id$Int-Instanz fehlt: {names:?}");
    assert!(names.iter().any(|n| n.starts_with("id$Float")), "id$Float-Instanz fehlt: {names:?}");
}

#[test]
fn auto_arena_promoviert_nicht_entkommende_schleife() {
    // Schleife, die eine temporäre Struktur alloziert, einen Skalar reduziert und
    // sie verwirft → per-Iteration-Arena (jrt_arena_push/pop im Rumpf).
    let src = "type Tree { l: Tree  r: Tree }\nfn make(d: Int) -> Tree {\n if d == 0 { Tree(null, null) } else { Tree(make(d - 1), make(d - 1)) }\n}\nfn check(t: Tree, d: Int) -> Int {\n if d == 0 { 1 } else { 1 + check(t.l, d - 1) + check(t.r, d - 1) }\n}\nfn main() {\n mut s = 0\n mut n = 0\n while n < 10 {\n s = s + check(make(5), 5)\n n = n + 1\n }\n print(s)\n}\n";
    let p = lower(src);
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    let calls: Vec<&str> = main.blocks.iter().flat_map(|b| &b.statements).filter_map(|s| if let fastllvm_ir::Statement::Call { func, .. } = s { Some(func.as_str()) } else { None }).collect();
    assert!(calls.contains(&"jrt_arena_push"), "nicht-entkommende Alloc-Schleife muss Auto-Arena bekommen: {calls:?}");
    assert!(calls.contains(&"jrt_arena_pop"), "Auto-Arena braucht pop");
}

#[test]
fn auto_arena_meidet_entkommende_schleife() {
    // Schleife, die eine Liste BAUT (frische Node fließt in die äußere `head`,
    // wird nach der Schleife genutzt) → darf NICHT arena-promotet werden
    // (sonst dangling). `head = Node(head, i)` ist ein Let einer äußeren Ref.
    let src = "type Node { next: Node  v: Int }\nfn main() {\n mut head = null\n mut i = 0\n while i < 100 {\n head = Node(head, i)\n i = i + 1\n }\n mut s = 0\n mut cur = head\n while cur != null {\n s = s + cur.v\n cur = cur.next\n }\n print(s)\n}\n";
    let p = lower(src);
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    let has_arena = main.blocks.iter().flat_map(|b| &b.statements).any(|s| matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "jrt_arena_push"));
    assert!(!has_arena, "entkommende (Listen-baue) Schleife darf KEINE Auto-Arena bekommen");
}

#[test]
fn makro_expandiert_und_ist_hygienisch() {
    // add_one(x) führt ein lokales `tmp` ein. Aufruf mit einem Argument, das
    // ebenfalls `tmp` heißt: das makro-lokale `tmp` wird gensym-umbenannt, fängt
    // das Argument NICHT ein. Ergebnis 11 (10+1), nicht 2 (tmp+tmp).
    let src = "macro add_one(x) = { mut tmp = 1\n x + tmp }\nfn main() {\n mut tmp = 10\n print(add_one(tmp))\n}\n";
    let p = lower(src);
    // Makro-Definition ist verschwunden; nur main bleibt.
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    // Das eingeführte `tmp` wurde umbenannt → im IR steht eine Addition mit dem
    // Argument (Local des Aufrufer-tmp) + der lokalen 1, kein Doppel-Argument.
    let adds = main.blocks.iter().flat_map(|b| &b.statements).filter(|s| {
        matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Binary(fastllvm_ir::BinOp::Add, ..)))
    }).count();
    assert!(adds >= 1, "Makro-Rumpf x+tmp muss als Add erscheinen");
}

#[test]
fn makro_aritaetskonflikt_ist_fehler() {
    let (mut m, _) = parse("macro pair(a, b) = a + b\nfn main() {\n print(pair(1))\n}\n");
    let errs = expand_macros(&mut m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("Makro")), "Aritätskonflikt muss Fehler sein: {errs:?}");
}

#[test]
fn higher_order_inline_defunktionalisiert() {
    // apply(f, x) mit Lambda-Argument → an der Aufrufstelle inline expandiert
    // (kein Funktionszeiger); Capture über den Scope; das Template selbst wird
    // NICHT als eigenständige Funktion emittiert.
    let src = "fn apply(f, x) -> Int {\n f(x)\n}\nfn main() {\n mut c = 10\n print(apply(y -> y + c, 5))\n}\n";
    let p = lower(src);
    assert!(!p.functions.iter().any(|f| f.name == "apply"), "Higher-Order-Template darf nicht eigenständig emittiert werden");
    // Der Lambda-Rumpf (y + c) ist in main inline → ein Add mit der Capture c.
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    let has_add = main.blocks.iter().flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Binary(fastllvm_ir::BinOp::Add, ..)))
    });
    assert!(has_add, "Lambda-Rumpf y+c muss inline (Add) in main stehen");
}

#[test]
fn generische_produkttypen() {
    // type Box[T] pro Typargument monomorphisiert: Box$Int, Box$Float —
    // je mit dem korrekten Feldtyp (I64 vs F64).
    let src = "type Box[T] {\n value: T\n}\nfn main() {\n mut a = Box(42)\n mut b = Box(3.5)\n print(a.value)\n print(b.value)\n}\n";
    let p = lower(src);
    let bi = p.classes.iter().find(|c| c.name == "Box$Int").expect("Box$Int fehlt");
    assert_eq!(bi.fields[0].ty, fastllvm_ir::Ty::I64);
    let bf = p.classes.iter().find(|c| c.name == "Box$Float").expect("Box$Float fehlt");
    assert_eq!(bf.fields[0].ty, fastllvm_ir::Ty::F64, "Float-Payload muss F64 sein (kein i64-Erasen)");
}

#[test]
fn generische_summentypen_typkorrekt() {
    // Option[Float]: Some(3.5) trägt F64 (kein i64-Erasen → kein Truncation-Bug).
    let src = "fn g() -> Option[Float] {\n Some(3.5)\n}\nfn main() {\n match g() {\n Some(x) -> print(x)\n None -> print(0.0)\n }\n}\n";
    let p = lower(src);
    let of = p.classes.iter().find(|c| c.name == "Option$Float").expect("Option$Float fehlt");
    let some_v = of.fields.iter().find(|f| f.name == "Some_value").expect("Some_value fehlt");
    assert_eq!(some_v.ty, fastllvm_ir::Ty::F64, "Some_value in Option$Float muss F64 sein");
}

#[test]
fn generische_summen_erschoepfung_pflicht() {
    // Nicht-erschöpfendes match auf typisierte Option = HARTER FEHLER (kein Loch
    // durch die Instanz-Klasse Option$Float).
    let (mut m, _) = parse("fn g() -> Option[Float] {\n Some(1.5)\n}\nfn main() {\n match g() {\n Some(x) -> print(x)\n }\n}\n");
    let _ = infer_module(&mut m);
    let errs = lower_module(&m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("erschöpf")), "typisierte Option nicht-erschöpfend muss Fehler sein: {errs:?}");
}

#[test]
fn option_result_und_try() {
    // Eingebaute Summentypen + `?`-Propagation.
    let src = "fn d(a: Int, b: Int) -> Result {\n if b == 0 { Err(1) } else { Ok(a / b) }\n}\nfn c(a: Int, b: Int) -> Result {\n mut q = d(a, b)?\n Ok(q + 1)\n}\nfn main() {\n print(1)\n}\n";
    let p = lower(src);
    // Result-Klasse registriert; `?` liest __tag + Ok_value.
    assert!(p.classes.iter().any(|c| c.name == "Result"));
    let reads_ok = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::GetField { field, .. } if field == "Ok_value")
    });
    assert!(reads_ok, "`?` muss Ok_value extrahieren");
}

#[test]
fn wachsende_liste_und_map() {
    let p = lower("fn main() {\n mut xs = list()\n xs.push(1)\n print(xs.len())\n mut m = [1: 2]\n print(m.get(1))\n}\n");
    let calls: Vec<&str> = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .filter_map(|s| if let fastllvm_ir::Statement::Call { func, .. } = s { Some(func.as_str()) } else { None }).collect();
    assert!(calls.contains(&"vire_list_new") && calls.contains(&"vire_list_push"));
    assert!(calls.contains(&"vire_map_new") && calls.contains(&"vire_map_put"));
}

#[test]
fn lambda_inline_mit_capture() {
    // `mut f = x -> x*k` fängt k; f(5) wird inline expandiert.
    let src = "fn main() {\n mut k = 10\n mut f = x -> x * k\n print(f(5))\n}\n";
    let p = lower(src);
    // Inline: die Multiplikation landet im main-Body (kein separater Call an f).
    let has_mul = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .any(|s| matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Binary(fastllvm_ir::BinOp::Mul, ..))));
    assert!(has_mul, "Lambda-Rumpf muss inline expandiert werden");
}

#[test]
fn comptime_faltet_konstanten() {
    // `comptime 2 + 3 * 4` → ConstI64(14), keine Laufzeit-Arithmetik.
    let p = lower("fn main() {\n print(comptime 2 + 3 * 4)\n}\n");
    let has_arith = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .any(|s| matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Binary(..))));
    assert!(!has_arith, "comptime muss zur Compilezeit falten (keine Binary-Ops)");
}

#[test]
fn traits_statische_dispatch() {
    // `impl Show for Point` → Methode Point.show; `display[T: Show]` monomorphisiert
    // und ruft die konkrete Impl (statische Dispatch, kein vtable).
    let src = "trait Show { fn show(self) -> Int }\ntype P { x: Int }\nimpl Show for P {\n fn show(self) -> Int { self.x }\n}\nfn display[T: Show](it: T) -> Int { it.show() }\nfn main() {\n print(display(P(9)))\n}\n";
    let p = lower(src);
    assert!(p.functions.iter().any(|f| f.name == "P.show"), "impl-Methode P.show fehlt");
    // display$P-Instanz ruft P.show.
    let calls_show = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .any(|s| matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "P.show"));
    assert!(calls_show, "monomorphisierte display muss P.show rufen");
}
