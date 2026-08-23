//! Match patterns (D-SYN-06). Nesting constraints are enforced by the type system
//! (ARCHITECTURE.md §3.5).
//!
//! D-SYN-06 states: "inside an enum-variant destructure or tuple destructure, only the three
//! kinds literal, simple binding, and wildcard may be nested -- recursively nesting another
//! variant/tuple pattern is forbidden." This is guaranteed not by having "the parser check
//! nesting depth at runtime" but by **using a type that cannot syntactically represent
//! nesting in the first place**.

use super::NodeId;
use crate::diagnostics::Span;
use std::sync::Arc;

pub enum Pattern {
    Literal(LiteralPat, Span),
    /// A bare identifier without parentheses. The parser **does not** decide whether this is
    /// a unit variant name or a new binding variable (that needs the scrutinee's type --
    /// D-SYN-06 "name resolution of bare identifiers"). The type-checking phase settles it
    /// and records it into `Resolutions::bare_ident_kind` via the `NodeId`.
    BareIdent(Arc<str>, NodeId, Span),
    Wildcard(Span),
    /// `Circle(r)` (D-SYN-07: positional).
    Variant {
        name: Arc<str>,
        fields: Vec<SubPattern>,
        span: Span,
    },
    /// `(a, b)` (positional, D-TYPE-06).
    Tuple {
        elements: Vec<SubPattern>,
        span: Span,
    },
}

/// Only these three kinds may occupy an element position of a Variant/Tuple. Because
/// SubPattern has no Variant/Tuple variant, a syntax tree that "nests another Variant/Tuple
/// pattern" cannot be constructed in the first place -- this expresses D-SYN-06's prohibition
/// rule as a Rust type rather than as a runtime check in the parser.
pub enum SubPattern {
    Literal(LiteralPat, Span),
    BareIdent(Arc<str>, NodeId, Span),
    Wildcard(Span),
}

pub enum LiteralPat {
    /// Includes the unary-minus form too (the parser folds D-LEX-04's special-casing down
    /// to the equivalent of a single token).
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}
