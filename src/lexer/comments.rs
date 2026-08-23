//! Collection of the comment/doc-comment side stream (input shared by fmt and doctest
//! extraction, ARCHITECTURE.md §2.1). Lexing collects comments (a standalone `#` line, a
//! trailing comment, an ordinary non-doc comment) into a side stream instead of discarding
//! them. The parser (parser/comment_attach.rs) looks at line numbers and attaches them to AST
//! nodes.

use crate::diagnostics::Span;

/// A single `#` or `##` comment. `is_doc` says whether it is `##` (a doc comment, D-DOC-01).
pub struct RawComment {
    pub text: String,
    pub is_doc: bool,
    /// Whether another token existed on this physical line (before the comment) -- used by
    /// the parser to decide between `trailing_comment` (a trailing comment) and
    /// `leading_comments` (a standalone-line comment).
    pub is_trailing: bool,
    pub span: Span,
}

/// The full sequence of comments collected during lexing (in source order).
pub type CommentStream = Vec<RawComment>;
