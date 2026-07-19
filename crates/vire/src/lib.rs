//! Vire — front-end (lexer, parser, AST). Lowering to `crates/ir` follows
//! (language/FRONTEND-PLAN.md). Backend/solver remain unchanged.

pub mod ast;
pub mod cblock;
pub mod diag;
pub mod expand;
pub mod infer;
pub mod inline;
pub mod lexer;
pub mod lower;
pub mod parser;
pub mod spawn;
pub mod syntax;

pub use cblock::desugar_cblocks;
pub use diag::Diag;
pub use expand::expand_macros;
pub use spawn::desugar_spawn;
pub use infer::infer_module;
pub use inline::inline_recursion;
pub use lower::{lower_module, lower_module_src};
pub use parser::{parse, parse_with_syntax};
pub use syntax::Syntax;
