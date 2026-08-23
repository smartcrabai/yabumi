//! E4005 naming convention (D-LINT-05).
//!
//! Variables, functions, struct fields, and function parameters must be
//! `^[a-z][a-z0-9_]*$` (snake_case). struct/enum type names and enum variant names must be
//! `^[A-Z][A-Za-z0-9]*$` (PascalCase). Generic type variables (`[T]`) are exempt from this
//! convention (naturally excluded by never consulting `decl.generics`).
//!
//! **On top-level variables**: `samples/err/lint/e4005_naming_convention` includes a
//! snake_case violation in the top-level variable `BadVariable`. Per the
//! `crate::effects::ENTRY_POINT_NAME` convention (see the comment at the top of
//! `src/effects/mod.rs`; needs adjustment for driver.rs = Unit17), if the synthesized
//! `FunctionDecl` for that name exists in `program.functions`, local variable names inside
//! its body (the top-level executable statements) are checked through the same path as an
//! ordinary function -- except the synthesized function's own name (`"$entry"`) is not
//! user code, so it is excluded from naming-convention checking.

use crate::ast::{Expr, ExprKind, FunctionDecl, Pattern, Stmt, StmtKind, SubPattern};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::eval::env::Program;
use crate::types::BareIdentKind;
use std::sync::Arc;

use super::{Visitor, walk_expr, walk_pattern, walk_stmt};

fn check_snake(name: &Arc<str>, span: Span, diagnostics: &mut DiagnosticBag) {
    if !regex::Regex::new(r"^[a-z][a-z0-9_]*$").is_ok_and(|re| re.is_match(name)) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::NamingConvention,
            span,
            message: format!("'{name}' is not snake_case (D-LINT-05)"),
        });
    }
}

fn check_pascal(name: &Arc<str>, span: Span, diagnostics: &mut DiagnosticBag) {
    if !regex::Regex::new(r"^[A-Z][A-Za-z0-9]*$").is_ok_and(|re| re.is_match(name)) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::NamingConvention,
            span,
            message: format!("'{name}' is not PascalCase (D-LINT-05)"),
        });
    }
}

/// Variables, functions, struct fields, and function parameters must be `snake_case`.
/// struct/enum type names and enum variant names must be `PascalCase`. Generic type
/// variables (`[T]`) are exempt.
pub fn check(program: &Program, diagnostics: &mut DiagnosticBag) {
    for s in program.structs.values() {
        check_pascal(&s.name, s.span, diagnostics);
        for f in &s.fields {
            check_snake(&f.name, f.span, diagnostics);
        }
        for m in &s.methods {
            check_function(m, true, program, diagnostics);
        }
    }
    for e in program.enums.values() {
        check_pascal(&e.name, e.span, diagnostics);
        for v in &e.variants {
            check_pascal(&v.name, v.span, diagnostics);
        }
    }
    for f in program.functions.values() {
        if crate::stdlib::prelude::is_builtin_function(f) {
            continue;
        }
        let is_entry = f.name.as_ref() == crate::effects::ENTRY_POINT_NAME;
        check_function(f, !is_entry, program, diagnostics);
    }
}

fn check_function(
    decl: &FunctionDecl,
    check_own_name: bool,
    program: &Program,
    diagnostics: &mut DiagnosticBag,
) {
    if check_own_name {
        check_snake(&decl.name, decl.span, diagnostics);
    }
    for p in &decl.params {
        check_snake(&p.name, p.span, diagnostics);
    }
    NamingVisitor {
        program,
        diagnostics,
    }
    .visit_block(&decl.body);
}

/// Checks only the name-binding leaf positions (a `VarDecl`/`NameAssign` name, lambda
/// parameters, and match-arm bindings); the recursion itself is the shared [`Visitor`]
/// walker in `lint/mod.rs`.
struct NamingVisitor<'a> {
    program: &'a Program,
    diagnostics: &'a mut DiagnosticBag,
}

impl NamingVisitor<'_> {
    fn is_binding(&self, node_id: crate::ast::NodeId) -> bool {
        self.program.resolutions.bare_ident_kind.get(&node_id) == Some(&BareIdentKind::Binding)
    }
}

impl Visitor for NamingVisitor<'_> {
    fn visit_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::VarDecl { name, value, .. } | StmtKind::NameAssign { name, value, .. } => {
                check_snake(name, stmt.span, self.diagnostics);
                self.visit_expr(value);
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        if let ExprKind::Lambda { params, body } = &expr.kind {
            for p in params {
                check_snake(&p.name, p.span, self.diagnostics);
            }
            self.visit_expr(body);
            return;
        }
        walk_expr(self, expr);
    }

    fn visit_pattern(&mut self, pattern: &Pattern) {
        if let Pattern::BareIdent(name, node_id, span) = pattern
            && self.is_binding(*node_id)
        {
            check_snake(name, *span, self.diagnostics);
        }
        walk_pattern(self, pattern);
    }

    fn visit_subpattern(&mut self, sub: &SubPattern) {
        if let SubPattern::BareIdent(name, node_id, span) = sub
            && self.is_binding(*node_id)
        {
            check_snake(name, *span, self.diagnostics);
        }
    }
}
