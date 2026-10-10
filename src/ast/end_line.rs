//! The source line of a node's actual final token, shared by `parser/comment_attach.rs` and
//! `fmt/printer.rs` (§5.9).
//!
//! `parser/stmt.rs::parse_block` decides `Block.span.end` via `self.previous_span()` **after**
//! `self.bump()`-ing the `Dedent` token. Because the synthesized `Dedent` token's span points
//! at the next non-blank line (= the start of the next sibling element, or further still if
//! blank lines follow), the `.span.end.line` of a `Block` / the `FunctionDecl`/`StructDecl`/
//! `EnumDecl`/`MatchArm` (a block body) / `IfExpr` (when the else is a block) containing it
//! points not at "the line of the actual final token" but at "the line of the next sibling
//! element." Using it directly would insert spurious blank lines in fmt (D-SYN-02, confirmed
//! in samples/ok/7-5_assert) and attach a sibling's trailing comment to the wrong node. The
//! functions below recompute the real final line recursively. Expressions that don't pass
//! through a block (Call/MethodCall/a string literal etc.) have their span decided by an
//! actual token such as `)`, so `expr.span.end.line` is correct for them as-is.

use super::{
    Block, ElseBranch, EnumDecl, Expr, ExprKind, FunctionDecl, IfExpr, MatchArm, MatchArmBody,
    Stmt, StmtKind, StructDecl,
};

pub fn true_end_line_of_stmt(stmt: &Stmt) -> u32 {
    match &stmt.kind {
        StmtKind::VarDecl { value, .. }
        | StmtKind::NameAssign { value, .. }
        | StmtKind::FieldAssign { value, .. }
        | StmtKind::IndexAssign { value, .. }
        | StmtKind::Discard(value)
        | StmtKind::ExprStmt(value)
        | StmtKind::Return(Some(value)) => true_end_line_of_expr(value),
        StmtKind::Return(None) => stmt.span.start.line,
    }
}

pub fn true_end_line_of_expr(expr: &Expr) -> u32 {
    match &expr.kind {
        ExprKind::If(if_expr) => true_end_line_of_if(if_expr),
        ExprKind::Match { arms, .. } => arms
            .last()
            .map_or(expr.span.start.line, true_end_line_of_match_arm),
        ExprKind::Lambda { body, .. } => true_end_line_of_expr(body),
        ExprKind::Grouping(inner) => true_end_line_of_expr(inner),
        _ => expr.span.end.line,
    }
}

pub fn true_end_line_of_if(if_expr: &IfExpr) -> u32 {
    match &if_expr.else_branch {
        ElseBranch::Block(block) => true_end_line_of_block(block),
        ElseBranch::ElseIf(inner) => true_end_line_of_if(inner),
    }
}

pub fn true_end_line_of_block(block: &Block) -> u32 {
    block
        .stmts
        .last()
        .map_or(block.span.start.line, true_end_line_of_stmt)
}

pub fn true_end_line_of_match_arm(arm: &MatchArm) -> u32 {
    match &arm.body {
        MatchArmBody::Expr(e) => true_end_line_of_expr(e),
        MatchArmBody::Block(block) => true_end_line_of_block(block),
    }
}

pub fn true_end_line_of_function_decl(f: &FunctionDecl) -> u32 {
    true_end_line_of_block(&f.body)
}

pub fn true_end_line_of_struct_decl(s: &StructDecl) -> u32 {
    if let Some(m) = s.methods.last() {
        true_end_line_of_function_decl(m)
    } else if let Some(field) = s.fields.last() {
        field.span.end.line
    } else {
        s.span.start.line
    }
}

pub fn true_end_line_of_enum_decl(e: &EnumDecl) -> u32 {
    e.variants
        .last()
        .map_or(e.span.start.line, |v| v.span.end.line)
}
