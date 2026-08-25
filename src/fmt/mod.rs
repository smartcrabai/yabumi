//! Driver that regenerates canonical-form text from the AST (ARCHITECTURE.md §5.9).
//!
//! Policy: AST regeneration rather than token-stream preservation. Idempotency
//! (fmt(fmt(x)) = fmt(x)) holds because "every normalization decision is uniquely determined by
//! its input." fmt needs no type checking and is self-contained using only syntactic
//! information, so it can be implemented and tested independently of type checking, effect
//! checking, and lint.

pub mod doc_fence;
pub mod printer;

use crate::ast::Module;

/// Called by `ybm check`; the caller writes the formatted result only when `--apply` is set.
/// Returns the formatted result.
pub fn format_module(module: &Module) -> String {
    printer::print_module(module)
}
