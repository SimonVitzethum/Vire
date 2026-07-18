//! AOT-Hotpath: die statische Schleifen-Schätzung setzt `!prof`-Branch-Weights.
//! Eine Funktion mit einer Schleife (Header-Branch: Körper vs. Ausgang) muss im
//! emittierten LLVM ein `!prof`-Tag am Schleifen-Branch + die branch_weights-
//! Metadatenknoten tragen. Eine Funktion ohne Schleife tut das nicht.

use fastllvm_ir::{BasicBlock, Block, Function, Local, Operand, Program, Rvalue, Statement, Terminator, Ty, BinOp};

fn looping_fn() -> Function {
    // bb0: i=0; goto bb1
    // bb1 (header): c = i < 10; branch c -> bb2(body) : bb3(exit)
    // bb2 (body): i = i+1; goto bb1  (Rückwärtskante bb2->bb1)
    // bb3 (exit): return
    let i = Local(0);
    let c = Local(1);
    Function {
        name: "loopy".into(),
        params: vec![],
        ret: Ty::Void,
        locals: vec![Ty::I64, Ty::I32],
        blocks: vec![
            BasicBlock {
                statements: vec![Statement::Assign(i, Rvalue::Use(Operand::ConstI64(0)))],
                terminator: Terminator::Goto(Block(1)),
            },
            BasicBlock {
                statements: vec![Statement::Assign(c, Rvalue::Binary(BinOp::CmpLt, Operand::Copy(i), Operand::ConstI64(10)))],
                terminator: Terminator::Branch { cond: Operand::Copy(c), then_blk: Block(2), else_blk: Block(3) },
            },
            BasicBlock {
                statements: vec![Statement::Assign(i, Rvalue::Binary(BinOp::Add, Operand::Copy(i), Operand::ConstI64(1)))],
                terminator: Terminator::Goto(Block(1)),
            },
            BasicBlock { statements: vec![], terminator: Terminator::Return(None) },
        ],
        receiver_nonnull: false,
    }
}

#[test]
fn schleifen_branch_bekommt_prof_weights() {
    let mut prog = Program::default();
    prog.functions.push(looping_fn());
    let ll = fastllvm_backend::emit(&prog);
    assert!(ll.contains("!prof"), "Schleifen-Branch muss ein !prof-Tag tragen:\n{ll}");
    assert!(ll.contains("branch_weights"), "branch_weights-Metadatenknoten fehlt");
    // Der Header-Branch (bb1) gewichtet den Körper (then=bb2) heiß.
    assert!(ll.contains("label %bb2, label %bb3, !prof"), "Header-Branch muss gewichtet sein:\n{ll}");
}
