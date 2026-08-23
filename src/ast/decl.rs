//! Declarations (ARCHITECTURE.md §3.4). Holds `def`/`struct`/`enum` declarations, module
//! directives, and doc comments/fences.
//!
//! `Decl` has no `Const` variant -- a module-level constant is syntactically identical to an
//! ordinary top-level assignment in an entry file (`Ident (":" TypeAnn)? "=" Expr`), and which
//! meaning it has is decided after the fact by module_resolve inspecting `Item::Stmt(NameAssign)`
//! (§4.2). To avoid requiring the parser to know "is this an entry or a module" and branch the
//! syntax tree's type on that, both are built as the same `Item::Stmt(Stmt)` from the start
//! (the DOC-COMMENT-MISSING-ON-STMT-LEVEL-CONST decision, §8).

use super::NodeId;
use super::stmt::{Block, Stmt};
use super::ty_ann::TypeAnn;
use crate::diagnostics::{FileId, Span};
use std::sync::Arc;

pub struct Module {
    pub file: FileId,
    /// Whether the effective first line, after shebang removal, was `module` (D-LEX-08/09).
    /// If true, the module_resolve phase checks "declarations only" (D-MOD-02).
    pub is_module_directive: bool,
    /// Kept in the exact order they appear in the source. module_resolve hoists and registers
    /// declarations (`Item::Decl`), but this Vec's own order is never changed (D-SYN-08:
    /// hoisting is scope construction at load time, and does not break the principle that
    /// execution order matches visual order).
    pub items: Vec<Item>,
    /// Comments after the final item, retained for lossless formatting.
    pub trailing_comments: Vec<LeadingComment>,
}

pub enum Item {
    Decl(Decl),
    Stmt(Stmt),
}

pub enum Decl {
    Function(FunctionDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
}

pub struct FunctionDecl {
    pub id: NodeId,
    pub name: Arc<str>,
    /// `[T, U]`.
    pub generics: Vec<Arc<str>>,
    /// Some only for struct methods. Enums have no methods (SPEC §3.5's grammar examples and
    /// DECISIONS as a whole never mention enum method syntax, so enum declarations get no
    /// method slot).
    pub self_param: Option<SelfParam>,
    pub params: Vec<Param>,
    pub ret: TypeAnn,
    /// `uses {..}` (empty means pure).
    pub effects: Vec<Arc<str>>,
    pub body: Block,
    /// For fmt's general-comment preservation only (§5.9). Holds unmarked `#` comments that
    /// appear immediately before `doc_comment` (a `##` fence), or immediately before the
    /// declaration itself when there is no `doc_comment` (so they are reproduced rather than
    /// dropped).
    pub leading_comments: Vec<LeadingComment>,
    pub doc_comment: Option<DocComment>,
    pub span: Span,
}

pub struct SelfParam {
    /// `var self` or `self` (D-MUT-01).
    pub mutable: bool,
    pub span: Span,
}

pub struct Param {
    pub name: Arc<str>,
    pub ty: TypeAnn,
    pub span: Span,
}

pub struct StructDecl {
    pub id: NodeId,
    pub name: Arc<str>,
    pub generics: Vec<Arc<str>>,
    /// Reuses Param (name: ty). Declaration order is the field index.
    pub fields: Vec<Param>,
    /// Per-field comments, parallel to `fields`.
    pub field_leading_comments: Vec<Vec<LeadingComment>>,
    pub field_trailing_comments: Vec<Option<String>>,
    /// self_param is always Some.
    pub methods: Vec<FunctionDecl>,
    /// For fmt's general-comment preservation only (§5.9). Handled the same way as
    /// FunctionDecl.
    pub leading_comments: Vec<LeadingComment>,
    pub doc_comment: Option<DocComment>,
    pub span: Span,
}

pub struct EnumDecl {
    pub id: NodeId,
    pub name: Arc<str>,
    pub generics: Vec<Arc<str>>,
    pub variants: Vec<EnumVariant>,
    /// For fmt's general-comment preservation only (§5.9). Handled the same way as
    /// FunctionDecl.
    pub leading_comments: Vec<LeadingComment>,
    pub doc_comment: Option<DocComment>,
    pub span: Span,
}

pub struct EnumVariant {
    pub name: Arc<str>,
    /// Empty means a unit variant. Per D-SYN-07, construction and destructuring are always
    /// positional.
    pub fields: Vec<TypeAnn>,
    /// The names written on each field at declaration time (if any; same length as `fields`).
    /// Kept only so fmt can reproduce readability names such as `Circle(radius: float)` from
    /// SPEC §3.5 -- D-SYN-07 only decided that construction/destructuring are positional, not
    /// that declaration field names may be discarded. Type checking (src/types/**) refers
    /// only to `fields` and never looks at this field.
    pub field_names: Vec<Option<Arc<str>>>,
    /// For fmt's general-comment preservation only (ARCHITECTURE.md §5.9). Not a DocComment --
    /// D-DOC-03 does not make individual variants a doctest target.
    pub leading_comments: Vec<LeadingComment>,
    pub trailing_comment: Option<String>,
    pub span: Span,
}

/// One line kept for fmt's general-comment preservation (§5.9). `text` is the content with the
/// single space right after `#` stripped (the inverse of D-FMT-03's processing). `line` is the
/// actual line number in the original source -- used by fmt to restore blank lines (D-SYN-02)
/// found inside a multi-line comment block and between it and the code that follows (a
/// concretization of ARCHITECTURE.md §5.9's "attach comments based on line number" mechanism,
/// shaped so the same per-line actual line number can also be used to restore blank lines).
pub struct LeadingComment {
    pub text: String,
    pub line: u32,
}

/// Original ordering of prose and fences inside a doc comment.
pub enum DocPart {
    Prose(usize),
    Fence(usize),
}

/// A `##` doc comment.
pub struct DocComment {
    pub prose_lines: Vec<String>,
    pub fences: Vec<DocFence>,
    pub parts: Vec<DocPart>,
    pub span: Span,
}

pub struct DocFence {
    /// The tag right after ` ``` `. None or an empty string means it is a test target
    /// (D-DOC-01). A non-empty tag such as `json` is ignored.
    pub lang_tag: Option<String>,
    /// The **actual file** line number of the fence's first inner line (D-DOC-05).
    pub body_start_line: u32,
    /// The fence's inner text, verbatim (not an fmt target, D-FMT-06; so it is kept as raw
    /// text rather than as a parse result of its own, and the doctest phase parses it with a
    /// separate Lexer/Parser invocation).
    pub raw_text: String,
    pub span: Span,
}
