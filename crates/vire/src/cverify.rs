//! Library-based memory-safety verification of inline C/asm blocks via the vendored
//! CSolver verifier — structured verdicts, no subprocess. Called by the driver's
//! `native "c"`/`native "asm"` gate.

use csolver_core::Verdict;
use csolver_ir::{Frontend, Module};
use csolver_verifier::{verify_module, Config};

/// The `@assume`-authorized CSolver assumptions for one block.
#[derive(Default)]
pub struct Assume {
    pub valid_mmio: bool,
    pub field_invariants: bool,
    pub valid_returns: bool,
    pub valid_loop_ptrs: bool,
    pub struct_tail: bool,
}

/// Verdict of a block: proven safe, or rejected with the residual obligations.
pub enum Outcome {
    Pass,
    Rejected(String),
}

fn config(assume: &Assume) -> Config {
    Config {
        // The typed Vire caller supplies valid, sized pointer parameters (a Vire array
        // is a proven (ptr, len)); precise `elements` contracts refine this per buffer.
        assume_valid_params: true,
        assume_valid_mmio: assume.valid_mmio,
        assume_field_invariants: assume.field_invariants,
        assume_valid_returns: assume.valid_returns,
        assume_valid_loop_ptrs: assume.valid_loop_ptrs,
        assume_struct_tail: assume.struct_tail,
        ..Config::default()
    }
}

fn finish(mut module: Module, pre_text: Option<&str>, assume: &Assume) -> Outcome {
    if let Some(text) = pre_text {
        if let Ok(pre) = csolver_verifier::precond::parse(text) {
            let _ = csolver_verifier::precond::apply(&mut module, &pre);
        }
    }
    let report = verify_module(&module, &config(assume));
    match report.verdict {
        Verdict::Pass => Outcome::Pass,
        _ => {
            let text = csolver_report::render_text(&report);
            let msg: String = text
                .lines()
                .filter(|l| {
                    let t = l.trim_start();
                    t.starts_with("FAIL")
                        || t.starts_with("UNKNOWN")
                        || t.contains("residual")
                        || t.contains("counterexample")
                })
                .take(12)
                .map(|l| format!("    {}", l.trim()))
                .collect::<Vec<_>>()
                .join("\n");
            Outcome::Rejected(if msg.is_empty() {
                format!("    verdict: {:?}", report.verdict)
            } else {
                msg
            })
        }
    }
}

/// Verify a textual `.ll` module (a lowered inline-C block).
pub fn verify_llvm(source: &str, name: &str, pre_text: Option<&str>, assume: &Assume) -> Outcome {
    match csolver_llvm::LlvmFrontend.lower(csolver_llvm::LlvmInput { source: source.to_string(), name: name.to_string() }) {
        Ok(m) => finish(m, pre_text, assume),
        Err(e) => Outcome::Rejected(format!("    the C block does not lower to IR: {e}")),
    }
}

/// Verify an x86-64 AT&T assembly block directly (CSolver's native input).
pub fn verify_asm(source: &str, assume: &Assume) -> Outcome {
    match csolver_asm::AsmFrontend.lower(csolver_asm::AsmInput {
        source: source.to_string(),
        arch: csolver_asm::Architecture::X86_64,
        syntax: csolver_asm::Syntax::Att,
    }) {
        Ok(m) => finish(m, None, assume),
        Err(e) => Outcome::Rejected(format!("    the asm block does not lower to IR: {e}")),
    }
}
