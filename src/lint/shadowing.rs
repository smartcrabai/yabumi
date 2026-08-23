//! E4003 shadowing (D-LINT-03).
//!
//! Always warns when an inner scope (if/match/a def body/lambda parameters/a match arm)
//! newly creates a binding with the same name as an existing name in an outer scope
//! (including a variable, function, or struct/enum name). A function boundary is also
//! treated as an "inner scope" (applies even when a function parameter shares a name with
//! an outer variable).
//!
//! **On top-level variables**: D-LINT-03's sample (`samples/err/lint/e4003_shadowing`)
//! explicitly includes the case "a function parameter shares a name with a top-level
//! variable". Since `Program` does not retain the entry's top-level executable
//! statements, per the `crate::effects::ENTRY_POINT_NAME` convention (see the comment at
//! the top of `src/effects/mod.rs`; needs adjustment for driver.rs = Unit17), if the
//! synthesized `FunctionDecl` for that name exists, this adds its body's **outermost**
//! (non-nested) bound names to the bottom layer of every other function/method's scope,
//! as "existing names in an outer scope" (this layer is not added when checking the entry
//! itself -- treating its own declarations as shadowing against itself would be
//! meaningless).

use crate::ast::{
    ElseBranch, Expr, ExprKind, FunctionDecl, IfExpr, MatchArmBody, Pattern, Stmt, StmtKind,
    SubPattern,
};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::eval::env::Program;
use crate::types::BareIdentKind;
use std::collections::HashSet;
use std::sync::Arc;

use super::{Visitor, walk_expr, walk_pattern, walk_stmt};

/// A stack of scopes. Each element is the set of known names in one scope. index 0 is
/// the bottom layer common to every function: "the flat namespace, plus (unless this is
/// itself) the top-level variables".
struct Scopes(Vec<HashSet<Arc<str>>>);

impl Scopes {
    fn push(&mut self) {
        self.0.push(HashSet::new());
    }

    fn pop(&mut self) {
        self.0.pop();
    }

    /// Whether this name exists in any scope other than the innermost one (including the
    /// global layer).
    fn is_bound_outer(&self, name: &str) -> bool {
        let last = self.0.len().saturating_sub(1);
        self.0[..last].iter().any(|s| s.contains(name))
    }

    /// Whether this name already exists somewhere in the currently visible scopes
    /// (including the innermost) (the same visibility as the D-MUT family's new-binding/
    /// reassignment determination, aligned with check_stmt.rs's check_name_assign).
    fn is_bound_anywhere(&self, name: &str) -> bool {
        self.0.iter().any(|s| s.contains(name))
    }

    fn declare_current(&mut self, name: Arc<str>) {
        if let Some(scope) = self.0.last_mut() {
            scope.insert(name);
        }
    }
}

fn check_and_declare(
    scopes: &mut Scopes,
    name: Arc<str>,
    span: Span,
    diagnostics: &mut DiagnosticBag,
) {
    if scopes.is_bound_outer(&name) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::Shadowing,
            span,
            message: format!("'{name}' shadows an existing name in an outer scope (D-LINT-03)"),
        });
    }
    scopes.declare_current(name);
}

fn collect_flat_namespace_names(program: &Program) -> HashSet<Arc<str>> {
    let mut names: HashSet<Arc<str>> = HashSet::new();
    names.extend(
        program
            .functions
            .keys()
            .filter(|n| n.as_ref() != crate::effects::ENTRY_POINT_NAME)
            .cloned(),
    );
    names.extend(program.structs.keys().cloned());
    for e in program.enums.values() {
        names.insert(Arc::clone(&e.name));
        for v in &e.variants {
            names.insert(Arc::clone(&v.name));
        }
    }
    names.extend(program.consts.keys().cloned());
    names
}

/// Collects only the names of the outermost (non-nested) `VarDecl`/`NameAssign` in
/// `decl`'s body (a binding inside a top-level if/match/lambda does not "leak" into other
/// declarations -- judgment call made in this file).
fn collect_outermost_names(decl: &FunctionDecl) -> HashSet<Arc<str>> {
    let mut names = HashSet::new();
    for stmt in &decl.body.stmts {
        match &stmt.kind {
            StmtKind::VarDecl { name, .. } | StmtKind::NameAssign { name, .. } => {
                names.insert(Arc::clone(name));
            }
            _ => {}
        }
    }
    names
}

/// Always warns when an inner scope (if/match/a def body/lambda parameters/a match arm)
/// newly creates a binding with the same name as an existing name in an outer scope
/// (including a variable, function, or struct/enum name).
pub fn check(program: &Program, diagnostics: &mut DiagnosticBag) {
    let flat_names = collect_flat_namespace_names(program);
    let entry_top_level_names: HashSet<Arc<str>> = program
        .functions
        .get(crate::effects::ENTRY_POINT_NAME)
        .map(|f| collect_outermost_names(f))
        .unwrap_or_default();

    for f in program.functions.values() {
        if crate::stdlib::prelude::is_builtin_function(f) {
            continue;
        }
        let is_entry = f.name.as_ref() == crate::effects::ENTRY_POINT_NAME;
        let mut base = flat_names.clone();
        if !is_entry {
            base.extend(entry_top_level_names.iter().cloned());
        }
        check_function_body(f, base, program, diagnostics);
    }
    for s in program.structs.values() {
        for m in &s.methods {
            let mut base = flat_names.clone();
            base.extend(entry_top_level_names.iter().cloned());
            check_function_body(m, base, program, diagnostics);
        }
    }
}

fn check_function_body(
    decl: &FunctionDecl,
    base: HashSet<Arc<str>>,
    program: &Program,
    diagnostics: &mut DiagnosticBag,
) {
    let mut scopes = Scopes(vec![base]);
    scopes.push();
    for p in &decl.params {
        check_and_declare(&mut scopes, Arc::clone(&p.name), p.span, diagnostics);
    }
    let mut visitor = ShadowingVisitor {
        scopes,
        program,
        diagnostics,
    };
    visitor.visit_block(&decl.body);
    visitor.scopes.pop();
}

/// Owns the scope stack while walking one function/method body. The recursion itself is
/// the shared [`Visitor`] walker in `lint/mod.rs`; the overrides below only handle the
/// scope-boundary positions (a new binding, a lambda/match-arm/if-branch scope).
struct ShadowingVisitor<'a> {
    scopes: Scopes,
    program: &'a Program,
    diagnostics: &'a mut DiagnosticBag,
}

impl ShadowingVisitor<'_> {
    fn check_and_declare(&mut self, name: Arc<str>, span: Span) {
        check_and_declare(&mut self.scopes, name, span, self.diagnostics);
    }

    fn is_binding(&self, node_id: crate::ast::NodeId) -> bool {
        self.program.resolutions.bare_ident_kind.get(&node_id) == Some(&BareIdentKind::Binding)
    }
}

impl Visitor for ShadowingVisitor<'_> {
    fn visit_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::VarDecl { name, value, .. } => {
                self.visit_expr(value);
                self.check_and_declare(Arc::clone(name), stmt.span);
            }
            StmtKind::NameAssign { name, value, .. } => {
                self.visit_expr(value);
                if !self.scopes.is_bound_anywhere(name) {
                    self.check_and_declare(Arc::clone(name), stmt.span);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Lambda { params, body } => {
                self.scopes.push();
                for p in params {
                    self.check_and_declare(Arc::clone(&p.name), p.span);
                }
                self.visit_expr(body);
                self.scopes.pop();
            }
            ExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee);
                for arm in arms {
                    self.scopes.push();
                    self.visit_pattern(&arm.pattern);
                    match &arm.body {
                        MatchArmBody::Expr(e) => self.visit_expr(e),
                        MatchArmBody::Block(b) => self.visit_block(b),
                    }
                    self.scopes.pop();
                }
            }
            _ => walk_expr(self, expr),
        }
    }

    fn visit_if(&mut self, if_expr: &IfExpr) {
        self.visit_expr(&if_expr.cond);
        self.scopes.push();
        self.visit_block(&if_expr.then_branch);
        self.scopes.pop();
        match &if_expr.else_branch {
            ElseBranch::Block(b) => {
                self.scopes.push();
                self.visit_block(b);
                self.scopes.pop();
            }
            ElseBranch::ElseIf(inner) => self.visit_if(inner),
        }
    }

    fn visit_pattern(&mut self, pattern: &Pattern) {
        if let Pattern::BareIdent(name, node_id, span) = pattern
            && self.is_binding(*node_id)
        {
            self.check_and_declare(Arc::clone(name), *span);
        }
        walk_pattern(self, pattern);
    }

    fn visit_subpattern(&mut self, sub: &SubPattern) {
        if let SubPattern::BareIdent(name, node_id, span) = sub
            && self.is_binding(*node_id)
        {
            self.check_and_declare(Arc::clone(name), *span);
        }
    }
}
