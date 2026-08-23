//! Entry point for the 5 rules, common walker (ARCHITECTURE.md §2.1). All of them depend
//! on resolved name information available only after type checking and effect checking,
//! so they run only after both succeed (§4.2).

mod doc_fence;
pub mod naming;
pub mod shadowing;
pub mod unreachable;
pub mod unused_function;
pub mod unused_variable;

use crate::ast::{
    Block, ElseBranch, Expr, ExprKind, FStringSegment, IfExpr, MatchArmBody, Pattern, PipeCallee,
    PipeExpr, Stmt, StmtKind, SubPattern,
};
use crate::diagnostics::DiagnosticBag;
use crate::eval::env::Program;

/// Runs all 5 rules (unused variable / unused function / shadowing / unreachable code / naming convention).
pub fn check_all(program: &Program, diagnostics: &mut DiagnosticBag) {
    unused_variable::check(program, diagnostics);
    unused_function::check(program, diagnostics);
    shadowing::check(program, diagnostics);
    unreachable::check(program, diagnostics);
    naming::check(program, diagnostics);
}

/// A visitor over the statement/expression tree, shared by the lint rules. Every rule
/// walks the same AST shape, so the recursion itself lives here once; a rule implements
/// this trait and overrides only the hooks where it has a leaf action (e.g. recording an
/// `Ident`, pushing a scope at a lambda boundary). The default implementations recurse
/// into children via the `walk_*` free functions below -- an override that still wants
/// the default recursion for the uninteresting kinds calls the corresponding `walk_*`
/// function in its fallthrough arm.
pub(crate) trait Visitor {
    fn visit_block(&mut self, block: &Block) {
        walk_block(self, block);
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &Expr) {
        walk_expr(self, expr);
    }

    fn visit_pipe(&mut self, pipe: &PipeExpr) {
        walk_pipe(self, pipe);
    }

    fn visit_if(&mut self, if_expr: &IfExpr) {
        walk_if(self, if_expr);
    }

    fn visit_pattern(&mut self, pattern: &Pattern) {
        walk_pattern(self, pattern);
    }

    fn visit_subpattern(&mut self, sub: &SubPattern) {
        walk_subpattern(self, sub);
    }
}

/// The default recursion behind [`Visitor::visit_block`]: visits every statement in order.
pub(crate) fn walk_block<V: Visitor + ?Sized>(v: &mut V, block: &Block) {
    for stmt in &block.stmts {
        v.visit_stmt(stmt);
    }
}

/// The default recursion behind [`Visitor::visit_stmt`]: visits every child expression.
pub(crate) fn walk_stmt<V: Visitor + ?Sized>(v: &mut V, stmt: &Stmt) {
    match &stmt.kind {
        StmtKind::VarDecl { value, .. } | StmtKind::NameAssign { value, .. } => {
            v.visit_expr(value);
        }
        StmtKind::FieldAssign { target, value, .. } => {
            v.visit_expr(target);
            v.visit_expr(value);
        }
        StmtKind::IndexAssign {
            target,
            index,
            value,
        } => {
            v.visit_expr(target);
            v.visit_expr(index);
            v.visit_expr(value);
        }
        StmtKind::Discard(e) | StmtKind::ExprStmt(e) | StmtKind::Return(Some(e)) => {
            v.visit_expr(e);
        }
        StmtKind::Return(None) => {}
    }
}

/// The default recursion behind [`Visitor::visit_expr`]: visits every child node
/// (including the patterns of match arms -- a pattern contains no expressions, so this
/// is a no-op unless the visitor overrides the pattern hooks).
pub(crate) fn walk_expr<V: Visitor + ?Sized>(v: &mut V, expr: &Expr) {
    match &expr.kind {
        ExprKind::Ident(_)
        | ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::StringLit(_) => {}
        ExprKind::FString(segments) => {
            for seg in segments {
                if let FStringSegment::Expr(e) = seg {
                    v.visit_expr(e);
                }
            }
        }
        ExprKind::ListLit { elements, .. }
        | ExprKind::SetLit { elements, .. }
        | ExprKind::TupleLit { elements, .. }
        | ExprKind::Par { elements, .. } => {
            for e in elements {
                v.visit_expr(e);
            }
        }
        ExprKind::DictLit { entries, .. } => {
            for (k, val) in entries {
                v.visit_expr(k);
                v.visit_expr(val);
            }
        }
        ExprKind::Unary { operand, .. } => v.visit_expr(operand),
        ExprKind::Binary { lhs, rhs, .. } => {
            v.visit_expr(lhs);
            v.visit_expr(rhs);
        }
        ExprKind::Call { callee, args, .. } => {
            v.visit_expr(callee);
            for a in args {
                v.visit_expr(&a.value);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            v.visit_expr(receiver);
            for a in args {
                v.visit_expr(&a.value);
            }
        }
        ExprKind::FieldAccess { target, .. } | ExprKind::TupleIndex { target, .. } => {
            v.visit_expr(target);
        }
        ExprKind::Index { target, index } => {
            v.visit_expr(target);
            v.visit_expr(index);
        }
        ExprKind::Question { target } => v.visit_expr(target),
        ExprKind::Pipe(pipe) => v.visit_pipe(pipe),
        ExprKind::Lambda { body, .. } => v.visit_expr(body),
        ExprKind::If(if_expr) => v.visit_if(if_expr),
        ExprKind::Match { scrutinee, arms } => {
            v.visit_expr(scrutinee);
            for arm in arms {
                v.visit_pattern(&arm.pattern);
                match &arm.body {
                    MatchArmBody::Expr(e) => v.visit_expr(e),
                    MatchArmBody::Block(b) => v.visit_block(b),
                }
            }
        }
        ExprKind::Grouping(inner) => v.visit_expr(inner),
    }
}

/// The default recursion behind [`Visitor::visit_pipe`]: visits the source and every
/// stage callee/argument (placeholders excluded).
pub(crate) fn walk_pipe<V: Visitor + ?Sized>(v: &mut V, pipe: &PipeExpr) {
    v.visit_expr(&pipe.source);
    for stage in &pipe.stages {
        match &stage.callee {
            PipeCallee::Bare(e) => v.visit_expr(e),
            PipeCallee::WithArgs { callee, args } => {
                v.visit_expr(callee);
                for a in args {
                    if !a.is_placeholder {
                        v.visit_expr(&a.value);
                    }
                }
            }
        }
    }
}

/// The default recursion behind [`Visitor::visit_if`]: visits the condition and both
/// branches.
pub(crate) fn walk_if<V: Visitor + ?Sized>(v: &mut V, if_expr: &IfExpr) {
    v.visit_expr(&if_expr.cond);
    v.visit_block(&if_expr.then_branch);
    match &if_expr.else_branch {
        ElseBranch::Block(b) => v.visit_block(b),
        ElseBranch::ElseIf(inner) => v.visit_if(inner),
    }
}

/// The default recursion behind [`Visitor::visit_pattern`]: visits every nested
/// subpattern.
pub(crate) fn walk_pattern<V: Visitor + ?Sized>(v: &mut V, pattern: &Pattern) {
    match pattern {
        Pattern::Variant { fields, .. } => {
            for f in fields {
                v.visit_subpattern(f);
            }
        }
        Pattern::Tuple { elements, .. } => {
            for f in elements {
                v.visit_subpattern(f);
            }
        }
        Pattern::Literal(..) | Pattern::BareIdent(..) | Pattern::Wildcard(_) => {}
    }
}

/// The default recursion behind [`Visitor::visit_subpattern`]: a no-op, because
/// SubPattern has no children (D-SYN-06 forbids nesting; see ast/pattern.rs).
pub(crate) fn walk_subpattern<V: Visitor + ?Sized>(_v: &mut V, _sub: &SubPattern) {}
