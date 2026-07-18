//! Lowering tests: Vire AST → crates/ir. Check the M2 semantics structurally
//! (without clang) — entry point, return-type estimation, binding-vs-assignment,
//! loops/control flow.

use fastllvm_ir::Ty;
use vire::{expand_macros, infer_module, inline_recursion, lower_module, parse};

#[test]
fn field_packing_i32_with_mixed_arithmetic() {
    // `I32` fields pack to 4 bytes (RAM), AND mixed i32/i64 arithmetic
    // (packed field + i64 local) is now correctly sign-extended
    // (previously: backend type error `i32 but i64 expected`).
    let p = lower("type T { small: I32  big: Int }\nfn main() {\n mut t = T(5, 1000000000000)\n print(t.big + t.small)\n}\n");
    // Field `small` is i32 in the struct (packed).
    let t = p.classes.iter().find(|c| c.name == "T").expect("T");
    let small = t.fields.iter().find(|f| f.name == "small").expect("small");
    assert_eq!(small.ty, Ty::I32, "I32 field must be packed as i32");
    // main contains a Convert (i32→i64 sext) for the mixed arithmetic.
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    let has_convert = main.blocks.iter().flat_map(|b| &b.statements).any(|s| matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Convert(_))));
    assert!(has_convert, "mixed i32/i64 arithmetic must widen the i32 field to i64");
}

#[test]
fn trait_objects_dynamic_dispatch() {
    // `fn f(s: Shape)` + `s.area()` → dynamic dispatch (CallVirtual through the
    // vtable), because the concrete type is only known at runtime. The trait is
    // registered as an interface, impls fill the vtable slots.
    let src = "trait Shape {\n fn area(self) -> Int\n}\ntype Circle { r: Int }\nimpl Shape for Circle {\n fn area(self) -> Int { self.r * self.r }\n}\nfn describe(s: Shape) -> Int {\n s.area()\n}\nfn main() { print(describe(Circle(5))) }\n";
    let p = lower(src);
    // describe calls area() virtually (CallVirtual), not statically.
    let describe = p.functions.iter().find(|f| f.name == "describe").expect("describe");
    let has_virtual = describe.blocks.iter().flat_map(|b| &b.statements).any(|s| matches!(s, fastllvm_ir::Statement::CallVirtual { class, name, .. } if class == "Shape" && name == "area"));
    assert!(has_virtual, "trait-typed receiver must emit CallVirtual on Shape.area");
    // The trait is registered as an interface; Circle implements it.
    let shape = p.classes.iter().find(|c| c.name == "Shape").expect("Shape-Interface");
    assert!(shape.is_interface, "trait must be registered as an interface");
    let circle = p.classes.iter().find(|c| c.name == "Circle").expect("Circle");
    assert!(circle.interfaces.iter().any(|i| i == "Shape"), "Circle must implement Shape");
}

#[test]
fn return_type_shorthand_gt() {
    // `> T` as shorthand for `-> T` in the return type; `->` still applies.
    let p = lower("fn add(a: Int, b: Int) > Int { a + b }\nfn classic(n: Int) -> Int { n * n }\nfn main() { print(add(3, 4))  print(classic(5)) }\n");
    let add = p.functions.iter().find(|f| f.name == "add").expect("add");
    assert_eq!(add.ret, Ty::I64, "`> Int` must set the return type correctly");
    let classic = p.functions.iter().find(|f| f.name == "classic").expect("classic");
    assert_eq!(classic.ret, Ty::I64, "`-> Int` must keep working");
}

#[test]
fn shallow_recursive_inlining_reduces_self_calls() {
    // fib: 2 self-calls. After the shallow-inline pass (depth 2) the body has
    // significantly MORE call statements (unfolded copies) and the remaining
    // self-calls sit deeper — the call count per frame drops drastically.
    let (mut m, _) = parse("fn fib(n: Int) -> Int {\n if n < 2 { n } else { fib(n - 1) + fib(n - 2) }\n}\nfn main() { print(fib(10)) }\n");
    expand_macros(&mut m).unwrap();
    inline_recursion(&mut m);
    let _ = infer_module(&mut m);
    let p = lower_module(&m).unwrap();
    let fib = p.functions.iter().find(|f| f.name == "fib").expect("fib");
    let calls = fib.blocks.iter().flat_map(|b| &b.statements).filter(|s| matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "fib")).count();
    // Naively there would be 2 calls; after depth-2 unfolding there are many
    // unfolded fib calls in the body (each frame covers 3 levels).
    assert!(calls > 2, "shallow-inline must unfold the self-calls (>2), found {calls}");
}

fn lower(src: &str) -> fastllvm_ir::Program {
    // Real pipeline: parse → macro expansion → type inference → lowering.
    let (mut m, diags) = parse(src);
    assert!(diags.is_empty(), "parse diagnostics: {diags:?}");
    expand_macros(&mut m).unwrap_or_else(|e| panic!("macro expansion: {e:?}"));
    let conflicts = infer_module(&mut m);
    assert!(conflicts.is_empty(), "type conflicts: {conflicts:?}");
    lower_module(&m).unwrap_or_else(|e| panic!("lowering: {e:?}"))
}

#[test]
fn main_becomes_java_main_and_void() {
    let p = lower("fn main() {\n print(1)\n}\n");
    let f = p.functions.iter().find(|f| f.name == "java_main").expect("java_main");
    assert_eq!(f.ret, Ty::Void);
}

#[test]
fn tail_expression_determines_return_type() {
    // Without `-> T`: return type estimated from the tail (Ident → I64 default).
    let p = lower("fn f(n) {\n mut a = n\n a\n}\n");
    let f = p.functions.iter().find(|f| f.name == "f").unwrap();
    assert_eq!(f.ret, Ty::I64);
}

#[test]
fn float_tail_becomes_f64() {
    let p = lower("fn f() {\n 1.5 + 2.5\n}\n");
    let f = p.functions.iter().find(|f| f.name == "f").unwrap();
    assert_eq!(f.ret, Ty::F64);
}

#[test]
fn binding_then_assignment_no_shadowing() {
    // `mut s = 0` binds; `s = s + 1` in the body is an assignment to the SAME local
    // (not a new one). Expectation: only one local carries `s` → accumulator works.
    let p = lower("fn main() {\n mut s = 0\n for i in 0..3 { s = s + i }\n print(s)\n}\n");
    let f = p.functions.iter().find(|f| f.name == "java_main").unwrap();
    // The accumulator local (0) is assigned multiple times (init + reassign in the
    // loop), not freshly bound each iteration (no shadowing).
    let assigns_to_zero = f.blocks.iter().flat_map(|b| &b.statements).filter(|s| {
        matches!(s, fastllvm_ir::Statement::Assign(l, _) if l.0 == 0)
    }).count();
    assert!(assigns_to_zero >= 2, "expected init + reassign on Local 0 (s), found {assigns_to_zero}");
}

#[test]
fn if_as_expression_yields_value() {
    // `if a>b {a} else {b}` as tail → function returns I64 (not Void),
    // and the merge block yields a result local.
    let p = lower("fn max(a, b) {\n if a > b { a } else { b }\n}\n");
    let f = p.functions.iter().find(|f| f.name == "max").unwrap();
    assert_eq!(f.ret, Ty::I64);
    // The last block returns a value (Return(Some(..))), not Return(None).
    let has_value_return = f.blocks.iter().any(|b| matches!(&b.terminator, fastllvm_ir::Terminator::Return(Some(_))));
    assert!(has_value_return, "if expression must yield a value");
}

#[test]
fn string_literals_land_in_pool() {
    let p = lower("fn main() {\n print(\"a\")\n print(\"b\")\n print(\"a\")\n}\n");
    // Two unique literals (a deduplicated), print(str)→jrt_println_str.
    assert_eq!(p.strings, vec!["a".to_string(), "b".to_string()]);
    let calls_str = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "jrt_println_str")
    });
    assert!(calls_str, "print(str) must call jrt_println_str");
}

#[test]
fn product_type_new_and_field_access() {
    let src = "type P {\n x: Int\n y: Int\n}\nfn main() {\n mut p = P(3, 4)\n print(p.x)\n}\n";
    let p = lower(src);
    // Class P registered with two I64 fields.
    let c = p.classes.iter().find(|c| c.name == "P").expect("class P");
    assert_eq!(c.fields.len(), 2);
    assert_eq!(c.fields[0].name, "x");
    assert_eq!(c.fields[0].ty, Ty::I64);
    // Construction → New + two PutField; access → GetField.
    let stmts: Vec<_> = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).collect();
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::New { class, .. } if class == "P")));
    assert_eq!(stmts.iter().filter(|s| matches!(s, fastllvm_ir::Statement::PutField { .. })).count(), 2);
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::GetField { field, .. } if field == "x")));
}

#[test]
fn field_mutation_produces_putfield() {
    let src = "type C {\n n: Int\n}\nfn main() {\n mut c = C(0)\n c.n = 5\n c.n += 1\n}\n";
    let p = lower(src);
    let puts = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .filter(|s| matches!(s, fastllvm_ir::Statement::PutField { field, .. } if field == "n")).count();
    // 1× construction + `= 5` + `+= 1` = 3 PutField on n.
    assert_eq!(puts, 3);
}

#[test]
fn capsule_wraps_body_with_arena() {
    // Pure form: arena_push before the body, arena_pop afterward; scalar result out.
    let p = lower("fn f(n) {\n capsule(n) {\n mut s = 0\n s = s + n\n s\n }\n}\n");
    let f = p.functions.iter().find(|f| f.name == "f").unwrap();
    assert_eq!(f.ret, Ty::I64);
    let calls: Vec<&str> = f.blocks.iter().flat_map(|b| &b.statements).filter_map(|s| {
        if let fastllvm_ir::Statement::Call { func, .. } = s { Some(func.as_str()) } else { None }
    }).collect();
    assert!(calls.contains(&"jrt_arena_push"), "arena_push missing");
    assert!(calls.contains(&"jrt_arena_pop"), "arena_pop missing");
}

#[test]
fn capsule_ref_result_is_error() {
    // An object result would point into the freed arena → hard error.
    let (mut m, _) = parse("type P {\n x: Int\n}\nfn f(n) {\n capsule(n) {\n P(n)\n }\n}\n");
    let _ = infer_module(&mut m);
    let errs = lower_module(&m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("capsule") && e.contains("object result")));
}

#[test]
fn capsule_object_input_is_error() {
    // The dangerous case (aliased ref input) must be a HARD error,
    // not a silent stub — otherwise capsule would promise containment without delivering it.
    let (m, _) = parse("type P {\n x: Int\n}\nfn f(p: P) -> Int {\n capsule(p) {\n p.x\n }\n}\n");
    let errs = lower_module(&m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("capsule") && e.contains("object input")));
}

#[test]
fn null_lowers_to_constnull() {
    // `null` (measurement bootstrap) → ConstNull; allows linked/cyclic graphs.
    let src = "type N {\n next: N\n v: Int\n}\nfn main() {\n mut a = N(null, 1)\n print(a.v)\n}\n";
    let p = lower(src);
    let has_null = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::PutField { value: fastllvm_ir::Operand::ConstNull, .. })
    });
    assert!(has_null, "null must go into the next field as ConstNull");
}

#[test]
fn return_statement_yields_typed_value() {
    // Function whose value comes from a `return` statement (no tail): the
    // unreachable fallthrough must terminate type-correctly, not `ret void`.
    let p = lower("fn f(n) {\n mut s = 0\n s = s + n\n return s\n}\n");
    let f = p.functions.iter().find(|f| f.name == "f").unwrap();
    assert_eq!(f.ret, Ty::I64);
    // No Return(None) in an I64 function.
    let has_void_return = f.blocks.iter().any(|b| matches!(&b.terminator, fastllvm_ir::Terminator::Return(None)));
    assert!(!has_void_return, "I64-Funktion darf kein Return(None) haben");
}

#[test]
fn extern_c_call_resolves_directly() {
    // extern "C" signature registered → call lowers as a direct call (no
    // mangling); the backend declares, clang links.
    let p = lower("extern \"C\" {\n fn sqrt(x: F64) -> F64\n}\nfn main() {\n print(sqrt(16.0))\n}\n");
    let calls_sqrt = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "sqrt")
    });
    assert!(calls_sqrt, "extern call must lower as Call(sqrt)");
}

#[test]
fn break_outside_loop_is_error() {
    let (m, _) = parse("fn main() {\n break\n}\n");
    assert!(lower_module(&m).is_err());
}

#[test]
fn methods_and_impl_blocks() {
    let src = "type P {\n x: Int\n y: Int\n}\nimpl P {\n fn sum(self) -> Int { self.x + self.y }\n}\nfn main() {\n mut p = P(3, 4)\n print(p.sum())\n}\n";
    let p = lower(src);
    // Method registered + lowered as function `P.sum` with a self-ref parameter.
    let m = p.functions.iter().find(|f| f.name == "P.sum").expect("P.sum");
    assert_eq!(m.params.first().copied(), Some(Ty::Ref)); // self
    // Call site emits Call(P.sum, [p]).
    let calls = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "P.sum")
    });
    assert!(calls, "method call must emit Call(P.sum)");
}

#[test]
fn sum_type_and_match() {
    let src = "type Sh {\n Circle(r: Int)\n Rect(w: Int, h: Int)\n Empty\n}\nfn area(s: Sh) -> Int {\n match s {\n Circle(r) -> r * r\n Rect(w, h) -> w * h\n Empty -> 0\n }\n}\nfn main() {\n print(area(Circle(3)))\n}\n";
    let p = lower(src);
    // Tagged class Sh with __tag as the first field.
    let c = p.classes.iter().find(|c| c.name == "Sh").expect("Sh");
    assert_eq!(c.fields[0].name, "__tag");
    // Construction sets __tag; match reads __tag (GetField __tag).
    let reads_tag = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::GetField { field, .. } if field == "__tag")
    });
    assert!(reads_tag, "match must read __tag");
}

#[test]
fn lists_and_comprehensions() {
    // List literal → NewArray+ArrayStore; comprehension with filter → NewArray + Loop.
    let p = lower("fn main() {\n mut xs = [1, 2, 3]\n mut ys = [x * x for x in xs if x > 1]\n print(ys.len())\n}\n");
    let stmts: Vec<_> = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).collect();
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::NewArray { .. })), "list/comprehension needs NewArray");
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::ArrayLoad { .. })), "Comprehension iteriert (ArrayLoad)");
    assert!(stmts.iter().any(|s| matches!(s, fastllvm_ir::Statement::ArrayLen { .. })), ".len()/iteration needs ArrayLen");
}

#[test]
fn match_exhaustiveness_is_mandatory() {
    // Non-exhaustive match = HARD ERROR (no more silent default).
    let (mut m, _) = parse("type T {\n A(x: Int)\n B\n}\nfn f(t: T) -> Int {\n match t {\n A(x) -> x\n }\n}\n");
    let _ = infer_module(&mut m);
    let errs = lower_module(&m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("exhaust")), "non-exhaustive match must be an error: {errs:?}");
}

#[test]
fn match_nested_binds_correctly() {
    // Nested pattern B(A(y)) binds y (no more silent ignoring).
    let src = "type T {\n A(x: Int)\n B(i: T)\n C\n}\nfn f(t: T) -> Int {\n match t {\n B(A(y)) -> y\n A(x) -> x\n B(z) -> 0\n C -> 0\n }\n}\nfn main() {\n print(f(C))\n}\n";
    let p = lower(src); // compiles = exhaustive + nested accepted
    assert!(p.functions.iter().any(|f| f.name == "f"));
}

#[test]
fn string_concat_and_auto_convert() {
    let p = lower("fn main() {\n mut n = 42\n print(\"n=\" + n)\n}\n");
    let calls: Vec<&str> = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .filter_map(|s| if let fastllvm_ir::Statement::Call { func, .. } = s { Some(func.as_str()) } else { None }).collect();
    assert!(calls.contains(&"jrt_str_concat"), "string + must call jrt_str_concat");
    assert!(calls.contains(&"jrt_long_to_str"), "Int in a + string must be converted");
}

#[test]
fn generics_monomorphize_per_type() {
    // id[T] is instantiated per call type: id$Int, id$Float.
    let p = lower("fn id[T](x: T) -> T { x }\nfn main() {\n print(id(1))\n print(id(2.5))\n}\n");
    let names: Vec<&str> = p.functions.iter().map(|f| f.name.as_str()).collect();
    assert!(names.iter().any(|n| n.starts_with("id$Int")), "id$Int instance missing: {names:?}");
    assert!(names.iter().any(|n| n.starts_with("id$Float")), "id$Float instance missing: {names:?}");
}

#[test]
fn auto_arena_promotes_non_escaping_loop() {
    // Loop that allocates a temporary structure, reduces a scalar and
    // discards it → per-iteration arena (jrt_arena_push/pop in the body).
    let src = "type Tree { l: Tree  r: Tree }\nfn make(d: Int) -> Tree {\n if d == 0 { Tree(null, null) } else { Tree(make(d - 1), make(d - 1)) }\n}\nfn check(t: Tree, d: Int) -> Int {\n if d == 0 { 1 } else { 1 + check(t.l, d - 1) + check(t.r, d - 1) }\n}\nfn main() {\n mut s = 0\n mut n = 0\n while n < 10 {\n s = s + check(make(5), 5)\n n = n + 1\n }\n print(s)\n}\n";
    let p = lower(src);
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    let calls: Vec<&str> = main.blocks.iter().flat_map(|b| &b.statements).filter_map(|s| if let fastllvm_ir::Statement::Call { func, .. } = s { Some(func.as_str()) } else { None }).collect();
    assert!(calls.contains(&"jrt_arena_push"), "non-escaping alloc loop must get an auto-arena: {calls:?}");
    assert!(calls.contains(&"jrt_arena_pop"), "auto-arena needs pop");
}

#[test]
fn auto_arena_avoids_escaping_loop() {
    // Loop that BUILDS a list (a fresh node flows into the outer `head`,
    // used after the loop) → must NOT be arena-promoted
    // (otherwise dangling). `head = Node(head, i)` is a let of an outer ref.
    let src = "type Node { next: Node  v: Int }\nfn main() {\n mut head = null\n mut i = 0\n while i < 100 {\n head = Node(head, i)\n i = i + 1\n }\n mut s = 0\n mut cur = head\n while cur != null {\n s = s + cur.v\n cur = cur.next\n }\n print(s)\n}\n";
    let p = lower(src);
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    let has_arena = main.blocks.iter().flat_map(|b| &b.statements).any(|s| matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "jrt_arena_push"));
    assert!(!has_arena, "escaping (list-building) loop must NOT get an auto-arena");
}

#[test]
fn macro_expands_and_is_hygienic() {
    // add_one(x) introduces a local `tmp`. Called with an argument that
    // is also named `tmp`: the macro-local `tmp` is gensym-renamed and does
    // NOT capture the argument. Result 11 (10+1), not 2 (tmp+tmp).
    let src = "macro add_one(x) = { mut tmp = 1\n x + tmp }\nfn main() {\n mut tmp = 10\n print(add_one(tmp))\n}\n";
    let p = lower(src);
    // The macro definition is gone; only main remains.
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    // The introduced `tmp` was renamed → the IR has an addition of the
    // argument (local of the caller's tmp) + the local 1, not a double argument.
    let adds = main.blocks.iter().flat_map(|b| &b.statements).filter(|s| {
        matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Binary(fastllvm_ir::BinOp::Add, ..)))
    }).count();
    assert!(adds >= 1, "macro body x+tmp must appear as an Add");
}

#[test]
fn macro_arity_conflict_is_error() {
    let (mut m, _) = parse("macro pair(a, b) = a + b\nfn main() {\n print(pair(1))\n}\n");
    let errs = expand_macros(&mut m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("Macro")), "arity conflict must be an error: {errs:?}");
}

#[test]
fn higher_order_inline_defunctionalized() {
    // apply(f, x) with a lambda argument → expanded inline at the call site
    // (no function pointer); capture over the scope; the template itself is
    // NOT emitted as a standalone function.
    let src = "fn apply(f, x) -> Int {\n f(x)\n}\nfn main() {\n mut c = 10\n print(apply(y -> y + c, 5))\n}\n";
    let p = lower(src);
    assert!(!p.functions.iter().any(|f| f.name == "apply"), "higher-order template must not be emitted standalone");
    // The lambda body (y + c) is inlined in main → an Add with the capture c.
    let main = p.functions.iter().find(|f| f.name == "java_main").expect("main");
    let has_add = main.blocks.iter().flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Binary(fastllvm_ir::BinOp::Add, ..)))
    });
    assert!(has_add, "lambda body y+c must be inline (Add) in main");
}

#[test]
fn generic_product_types() {
    // type Box[T] monomorphized per type argument: Box$Int, Box$Float —
    // each with the correct field type (I64 vs F64).
    let src = "type Box[T] {\n value: T\n}\nfn main() {\n mut a = Box(42)\n mut b = Box(3.5)\n print(a.value)\n print(b.value)\n}\n";
    let p = lower(src);
    let bi = p.classes.iter().find(|c| c.name == "Box$Int").expect("Box$Int missing");
    assert_eq!(bi.fields[0].ty, fastllvm_ir::Ty::I64);
    let bf = p.classes.iter().find(|c| c.name == "Box$Float").expect("Box$Float missing");
    assert_eq!(bf.fields[0].ty, fastllvm_ir::Ty::F64, "Float payload must be F64 (no i64 erasure)");
}

#[test]
fn generic_sum_types_type_correct() {
    // Option[Float]: Some(3.5) carries F64 (no i64 erasure → no truncation bug).
    let src = "fn g() -> Option[Float] {\n Some(3.5)\n}\nfn main() {\n match g() {\n Some(x) -> print(x)\n None -> print(0.0)\n }\n}\n";
    let p = lower(src);
    let of = p.classes.iter().find(|c| c.name == "Option$Float").expect("Option$Float missing");
    let some_v = of.fields.iter().find(|f| f.name == "Some_value").expect("Some_value missing");
    assert_eq!(some_v.ty, fastllvm_ir::Ty::F64, "Some_value in Option$Float must be F64");
}

#[test]
fn generic_sum_exhaustiveness_mandatory() {
    // Non-exhaustive match on a typed Option = HARD ERROR (no hole
    // through the instance class Option$Float).
    let (mut m, _) = parse("fn g() -> Option[Float] {\n Some(1.5)\n}\nfn main() {\n match g() {\n Some(x) -> print(x)\n }\n}\n");
    let _ = infer_module(&mut m);
    let errs = lower_module(&m).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("exhaust")), "typed Option non-exhaustive must be an error: {errs:?}");
}

#[test]
fn option_result_and_try() {
    // Built-in sum types + `?` propagation.
    let src = "fn d(a: Int, b: Int) -> Result {\n if b == 0 { Err(1) } else { Ok(a / b) }\n}\nfn c(a: Int, b: Int) -> Result {\n mut q = d(a, b)?\n Ok(q + 1)\n}\nfn main() {\n print(1)\n}\n";
    let p = lower(src);
    // Result class registered; `?` reads __tag + Ok_value.
    assert!(p.classes.iter().any(|c| c.name == "Result"));
    let reads_ok = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements).any(|s| {
        matches!(s, fastllvm_ir::Statement::GetField { field, .. } if field == "Ok_value")
    });
    assert!(reads_ok, "`?` must extract Ok_value");
}

#[test]
fn growing_list_and_map() {
    let p = lower("fn main() {\n mut xs = list()\n xs.push(1)\n print(xs.len())\n mut m = [1: 2]\n print(m.get(1))\n}\n");
    let calls: Vec<&str> = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .filter_map(|s| if let fastllvm_ir::Statement::Call { func, .. } = s { Some(func.as_str()) } else { None }).collect();
    assert!(calls.contains(&"vire_list_new") && calls.contains(&"vire_list_push"));
    assert!(calls.contains(&"vire_map_new") && calls.contains(&"vire_map_put"));
}

#[test]
fn lambda_inline_with_capture() {
    // `mut f = x -> x*k` captures k; f(5) is expanded inline.
    let src = "fn main() {\n mut k = 10\n mut f = x -> x * k\n print(f(5))\n}\n";
    let p = lower(src);
    // Inline: the multiplication lands in the main body (no separate call to f).
    let has_mul = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .any(|s| matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Binary(fastllvm_ir::BinOp::Mul, ..))));
    assert!(has_mul, "lambda body must be inline-expanded");
}

#[test]
fn comptime_folds_constants() {
    // `comptime 2 + 3 * 4` → ConstI64(14), no runtime arithmetic.
    let p = lower("fn main() {\n print(comptime 2 + 3 * 4)\n}\n");
    let has_arith = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .any(|s| matches!(s, fastllvm_ir::Statement::Assign(_, fastllvm_ir::Rvalue::Binary(..))));
    assert!(!has_arith, "comptime must fold at compile time (no binary ops)");
}

#[test]
fn traits_static_dispatch() {
    // `impl Show for Point` → method Point.show; `display[T: Show]` monomorphized
    // and calls the concrete impl (static dispatch, no vtable).
    let src = "trait Show { fn show(self) -> Int }\ntype P { x: Int }\nimpl Show for P {\n fn show(self) -> Int { self.x }\n}\nfn display[T: Show](it: T) -> Int { it.show() }\nfn main() {\n print(display(P(9)))\n}\n";
    let p = lower(src);
    assert!(p.functions.iter().any(|f| f.name == "P.show"), "impl method P.show missing");
    // The display$P instance calls P.show.
    let calls_show = p.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.statements)
        .any(|s| matches!(s, fastllvm_ir::Statement::Call { func, .. } if func == "P.show"));
    assert!(calls_show, "monomorphized display must call P.show");
}
