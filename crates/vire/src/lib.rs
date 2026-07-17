//! Vire — Front-End (Lexer, Parser, AST). Absenkung nach `crates/ir` folgt
//! (sprache/FRONTEND-PLAN.md). Backend/Solver bleiben unverändert.

pub mod ast;
pub mod diag;
pub mod lexer;
pub mod parser;

pub use diag::Diag;
pub use parser::parse;
