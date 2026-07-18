//! Vire — Front-End (Lexer, Parser, AST). Absenkung nach `crates/ir` folgt
//! (sprache/FRONTEND-PLAN.md). Backend/Solver bleiben unverändert.

pub mod ast;
pub mod diag;
pub mod expand;
pub mod infer;
pub mod inline;
pub mod lexer;
pub mod lower;
pub mod parser;
pub mod syntax;

pub use diag::Diag;
pub use expand::expand_macros;
pub use infer::infer_module;
pub use inline::inline_recursion;
pub use lower::lower_module;
pub use parser::{parse, parse_with_syntax};
pub use syntax::Syntax;
