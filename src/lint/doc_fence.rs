//! Common helper that collects referenced names from doctest fences (untagged `##`
//! fences) (shared by `unused_function.rs`/`unused_variable.rs`, D-LINT-01/D-LINT-02).
//!
//! Both D-LINT-01 (unused variable) and D-LINT-02 (unused function) define their warning
//! as applying only to something "never called, directly or indirectly, from a top-level
//! executable statement **or a doctest block**". Both files commonly need the operation
//! of independently lexing/parsing the fence body of a `##` doc comment placed
//! immediately before each `def`/struct/enum/module-level const and collecting the names
//! that appear as identifiers -- unused_function.rs uses the result as roots for call-
//! graph reachability, and unused_variable.rs uses it as usage flags in the top-level
//! scope.
//!
//! **Determination method** (carries forward, unchanged, the original judgment call moved
//! over from unused_function.rs): rather than limiting the "called, directly or
//! indirectly" determination to the syntactic position of a call (`Call`), it is relaxed
//! all the way to "does that name appear anywhere in the body as an identifier" -- even
//! syntax that passes a function name as-is to a stdlib higher-order method (such as
//! `xs.sort_by(compare)`) is a reference as a value rather than a call, so a strict
//! "call graph" alone cannot capture it; this rule therefore also applies the same
//! "prioritize zero false positives" policy as D-LINT-04 (DECISIONS D-LINT-04), using
//! the looser "is it referenced" as the reachability criterion. A fence that fails to
//! parse (diagnostics for the doctest phase itself are Unit15's responsibility, a later
//! stage than Lint) is silently ignored.

use crate::ast::{Block, DocComment, Expr, ExprKind, Item, StmtKind};
use crate::diagnostics::FileId;
use std::collections::HashSet;
use std::sync::Arc;

use super::{Visitor, walk_expr};

/// Collects every name that appears as an identifier (not limited to call positions; see
/// the judgment call at the top of this file). The recursion itself is the shared
/// [`Visitor`] walker in `lint/mod.rs`; this struct only adds the `Ident` leaf action.
struct NameCollector<'a> {
    out: &'a mut HashSet<Arc<str>>,
}

impl Visitor for NameCollector<'_> {
    fn visit_expr(&mut self, expr: &Expr) {
        if let ExprKind::Ident(name) = &expr.kind {
            self.out.insert(Arc::clone(name));
        }
        walk_expr(self, expr);
    }
}

/// Collects every name that appears as an identifier within `block`'s body (not limited
/// to call positions; see the judgment call at the top of this file).
pub(super) fn collect_referenced_names(block: &Block, out: &mut HashSet<Arc<str>>) {
    NameCollector { out }.visit_block(block);
}

/// Independently lexes/parses a doctest block (an untagged fence, D-DOC-01) and collects
/// referenced names. A fence that fails to parse (it may not have been validated yet --
/// type checking/execution of doctests is Unit15's responsibility, a later stage than
/// Lint) is silently ignored.
pub(super) fn collect_doc_fence_names(doc: &DocComment, out: &mut HashSet<Arc<str>>) {
    for fence in &doc.fences {
        if fence.lang_tag.as_deref().is_some_and(|tag| !tag.is_empty()) {
            continue;
        }
        let file = FileId(0);
        let (tokens, _comments, lex_diag) =
            crate::lexer::Lexer::new(&fence.raw_text, file).tokenize();
        if !lex_diag.is_empty() {
            continue;
        }
        let (module, parse_diag) = crate::parser::parse_module(&tokens, file);
        if !parse_diag.is_empty() {
            continue;
        }
        for item in &module.items {
            if let Item::Stmt(stmt) = item {
                NameCollector { out: &mut *out }.visit_stmt(stmt);
            }
        }
    }
}

/// Collects referenced names from the doctest fences of the `##` doc comment attached to
/// each statement directly under `block` (only 1 level, not recursive). By applying the
/// same filter as the criterion `doctest::collect_fences` (`src/doctest/mod.rs`) actually
/// uses to decide doctest targets -- only a `StmtKind::NameAssign` directly under
/// `module.items` (a module-level const, and a top-level assignment in the entry file
/// that looks the same on its surface; of D-DOC-03's 4 kinds of declaration, only the
/// const falls under this path) -- this avoids mistakenly including a `Stmt.doc_comment`
/// nested inside a function body (which is never actually a doctest target) as a root for
/// referenced names.
///
/// Under the `ENTRY_POINT_NAME` convention (see the comment at the top of
/// `src/effects/mod.rs`), the entry's top-level executable statements are held as-is
/// (`doc_comment` included) as `Stmt`s inside the synthesized `FunctionDecl.body` --
/// `Program` itself (per the `Item::Stmt(_) => {}` in `module_resolve/mod.rs`) does not
/// retain the entry's top-level executable statements, and this file can only access
/// statement-level `doc_comment`s through this synthesized `FunctionDecl.body`.
pub(super) fn collect_doc_fence_names_from_top_level_consts(
    block: &Block,
    out: &mut HashSet<Arc<str>>,
) {
    for stmt in &block.stmts {
        if !matches!(stmt.kind, StmtKind::NameAssign { .. }) {
            continue;
        }
        if let Some(doc) = &stmt.doc_comment {
            collect_doc_fence_names(doc, out);
        }
    }
}
