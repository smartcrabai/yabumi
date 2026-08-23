//! Complete type definitions for `Token`/`TokenKind` (ARCHITECTURE.md §3.3).

use crate::diagnostics::Span;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    IntLiteral(i64),
    FloatLiteral(f64),
    /// Already has escapes resolved.
    StringLiteral(String),
    FString(Vec<FStringPart>),
    True,
    False,

    Ident(Arc<str>),

    // Reserved words (D-LEX-01). `Ok`/`Err`/`Some`/`None`/`int`/`float`/`str` are not
    // reserved words -- they are generated as ordinary Ident tokens and treated as
    // pre-registered identifiers on the flat-namespace side.
    Def,
    Struct,
    Enum,
    If,
    Else,
    Match,
    Return,
    Var,
    Uses,
    Par,
    And,
    Or,
    Not,
    In,
    Underscore,
    Module,
    Void,
    KwSelf,

    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    /// `=` (assignment is statement-only, not an expression).
    Eq,
    /// `->` (function types inside a type annotation only).
    Arrow,
    /// `=>`.
    FatArrow,
    /// `|>`.
    PipeOp,
    Question,
    Dot,
    Comma,
    Colon,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,

    Newline,
    Indent,
    Dedent,
    Eof,
}

/// An f-string's `{expr}` portion is cut out by the outer scan (brace depth, §5.2) and then
/// lexed by recursively invoking the same Lexer. Inside expr, scanning uses a special mode
/// that disallows starting a string literal (`"`) (D-LEX-07).
#[derive(Debug, Clone, PartialEq)]
pub enum FStringPart {
    /// `{{` -> `{`, `}}` -> `}`; the literal portion with escapes already resolved.
    Text(String),
    /// A token sequence already lexed by the recursive call (does not include the terminal Eof).
    Expr(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
