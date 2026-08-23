//! Statements and blocks (ARCHITECTURE.md §3.4).

use super::decl::{DocComment, LeadingComment};
use super::expr::Expr;
use super::ty_ann::TypeAnn;
use crate::diagnostics::Span;
use std::sync::Arc;

pub struct Block {
    /// Only when this Block is the body of an if/match branch does a trailing ExprStmt become
    /// the value of the whole block, per D-SYN-11. When used as `FunctionDecl.body`, a
    /// different rule applies instead (§5.6 "function body value rule") -- D-SYN-11 is not
    /// generalized to FunctionDecl.body (the VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT
    /// decision, §8). Block itself does not know which rule applies (the same syntactic type
    /// is simply used in two contexts, and the caller -- if/match checking in check_stmt.rs,
    /// or function-body checking in check_decl.rs -- picks the rule).
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
    /// The doctest target for D-DOC-03 (a `##` fence). Can actually attach only to
    /// `StmtKind::NameAssign` (module-level constants, and top-level assignments in an entry
    /// file that look the same syntactically -- the DOC-COMMENT-MISSING-ON-STMT-LEVEL-CONST
    /// decision, §8). If a `##` is written before another StmtKind, the parser attaches it the
    /// same way, but doctest collection targets only NameAssign.
    pub doc_comment: Option<DocComment>,
    /// For fmt's general-comment preservation only (§5.9). Independent of doc_comment above
    /// (both are often None).
    pub leading_comments: Vec<LeadingComment>,
    pub trailing_comment: Option<String>,
}

pub enum StmtKind {
    /// `var x = expr` / `var x: T = expr`. Always a new mutable binding in the current scope.
    VarDecl {
        name: Arc<str>,
        ty: Option<TypeAnn>,
        value: Expr,
    },
    /// `x = expr` / `x: T = expr` (assignment to a bare identifier). A single syntactic form,
    /// but settled into one of the following three cases during the name-resolution phase of
    /// type checking (a design decision made here -- the parser cannot decide this because it
    /// does not know the fact needed for the decision, namely "does `x` already exist in the
    /// current scope"):
    ///   1. `x` does not exist in the current scope -> a new immutable binding
    ///   2. `x` exists in the current scope as a `var` binding -> reassignment (only type
    ///      match is checked, not subject to E3001)
    ///   3. `x` exists in the current scope as an immutable binding -> E3001
    NameAssign {
        name: Arc<str>,
        ty: Option<TypeAnn>,
        value: Expr,
    },
    /// `target.field = expr`. Always a write to an existing path (D-MUT-03: recursively
    /// tracks the root variable).
    FieldAssign {
        target: Expr,
        field: Arc<str>,
        value: Expr,
    },
    /// `target[index] = expr` (list/dict only, D-COL-02).
    IndexAssign {
        target: Expr,
        index: Expr,
        value: Expr,
    },
    /// `_ = expr` (explicit discard of an unused Result, D-ERR-03).
    Discard(Expr),
    Return(Option<Expr>),
    /// An expression statement. If its type is Result, it is subject to D-ERR-03's
    /// unused-value check.
    ExprStmt(Expr),
}
