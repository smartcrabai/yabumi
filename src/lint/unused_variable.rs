//! E4001 unused variable (D-LINT-01).
//!
//! **On top-level executable statements**»
//! Targets local bindings of `x = ...`/`var x = ...`, function parameters, and match-arm
//! bound variables. An identifier starting with `_` is excluded from checking.
//!
//! **On top-level executable statements**
//! **On top-level executable statements**: `Program` does not retain the entry file's
//! top-level executable statements (ModuleResolve drops `Item::Stmt` wholesale, see
//! `module_resolve/mod.rs`). Since D-LINT-01 itself targets "local bindings" in general
//! with no top-level-specific exclusion rule, this file follows the
//! `crate::effects::ENTRY_POINT_NAME` convention (needs adjustment for driver.rs =
//! Unit17; see the comment at the top of `src/effects/mod.rs`) -- if the synthesized
//! `FunctionDecl` for that name exists in `program.functions`, its body (the top-level
//! executable statements) also becomes subject to unused-variable checking through
//! exactly the same path as any other function (no special-casing at all is needed, since
//! its parameter list is empty).
//!
//! **Fix for a known bug (judgment call made in this file)**: D-LINT-01 defines its
//! report of "unused" as something "never called, directly or indirectly, from a top-
//! level executable statement **or a doctest block**" (the counterpart rule to
//! unused_function.rs's D-LINT-02). A module-level const (an `x = ...` directly under the
//! entry file; held, under the `ENTRY_POINT_NAME` convention above, as a `Stmt` inside the
//! synthesized `FunctionDecl.body`) is syntactically just a top-level `NameAssign` and
//! becomes subject to E4001 determination through the same path as an ordinary local
//! binding, but until now its usage from its own `##` doc comment (a self-reference such
//! as `assert(pi_approx > 3.0)`) was never considered at all, causing false positives.
//! Using the same filter as `doc_fence::collect_doc_fence_names_from_top_level_consts`
//! (shared with `unused_function.rs`) -- only a `StmtKind::NameAssign` directly under the
//! `ENTRY_POINT_NAME` function body, matching the criterion `doctest::collect_fences`
//! actually uses to decide doctest targets, so an unrelated `Stmt.doc_comment` nested
//! inside an ordinary function body is not mistakenly treated as used -- right after
//! `check_function_body` finishes walking the body, the doc fence's referenced names are
//! `mark_used` (`mark_module_level_const_doc_fence_usages`).

use crate::ast::{
    ElseBranch, Expr, ExprKind, FunctionDecl, IfExpr, MatchArmBody, Pattern, Stmt, StmtKind,
    SubPattern,
};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::eval::env::Program;
use crate::types::BareIdentKind;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::doc_fence::collect_doc_fence_names_from_top_level_consts;
use super::{Visitor, walk_expr, walk_pattern, walk_stmt};

struct VarEntry {
    span: Span,
    used: bool,
    /// Excluded from D-LINT-01 (a lambda parameter; judgment call made at the top of this
    /// file -- a lambda that intentionally ignores its callback argument, as in
    /// `unwrap_or_else((e) => 0)` from
    /// `samples/ok/3-3_stdlib_types/entry_full_method_coverage_and_cause_chain.ybm`, is a
    /// common idiom, and D-LINT-01's "function parameter" is interpreted, following
    /// DECISIONS's overall terminology -- D-FUNC-01/02 clearly distinguishes `def` from a
    /// lambda -- as referring only to a `def`/method parameter). When `false`, `pop` does
    /// not report it as unused -- it is still registered in the scope, so name resolution
    /// for references inside the lambda body still works as usual.
    reportable: bool,
}

/// A scope stack within a single function/method body (a lightweight lint-only
/// reimplementation of the same design as `types::env::TypeEnv`, "push/pop a
/// Vec<HashMap> at if/match branch and lambda body boundaries" -- since the original
/// `TypeEnv` no longer exists once TypeCheck completes, this file rebuilds it itself).
struct Scopes(Vec<HashMap<Arc<str>, VarEntry>>);

impl Scopes {
    fn new() -> Self {
        Self(vec![HashMap::new()])
    }

    fn push(&mut self) {
        self.0.push(HashMap::new());
    }

    /// Closes the scope and reports any binding left unused (excluding `_`-prefixed ones)
    /// as E4001.
    fn pop(&mut self, diagnostics: &mut DiagnosticBag) {
        let Some(scope) = self.0.pop() else { return };
        let mut entries: Vec<(Arc<str>, VarEntry)> = scope.into_iter().collect();
        entries.sort_by_key(|(_, entry)| (entry.span.start.line, entry.span.start.col));
        for (name, entry) in entries {
            if entry.reportable && !entry.used && !name.starts_with('_') {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::UnusedVariable,
                    span: entry.span,
                    message: format!("variable '{name}' is unused (D-LINT-01)"),
                });
            }
        }
    }

    fn declare(&mut self, name: Arc<str>, span: Span) {
        if let Some(scope) = self.0.last_mut() {
            scope.insert(
                name,
                VarEntry {
                    span,
                    used: false,
                    reportable: true,
                },
            );
        }
    }

    /// For lambda parameters: registers in the scope but does not report it as unused
    /// (judgment call made at the top of this file).
    fn declare_unreportable(&mut self, name: Arc<str>, span: Span) {
        if let Some(scope) = self.0.last_mut() {
            scope.insert(
                name,
                VarEntry {
                    span,
                    used: false,
                    reportable: false,
                },
            );
        }
    }

    fn is_bound_anywhere(&self, name: &str) -> bool {
        self.0.iter().any(|s| s.contains_key(name))
    }

    fn mark_used(&mut self, name: &str) {
        for scope in self.0.iter_mut().rev() {
            if let Some(entry) = scope.get_mut(name) {
                entry.used = true;
                return;
            }
        }
    }
}

/// Targets local bindings of `x = ...`/`var x = ...`, function parameters, and match-arm
/// bound variables. An identifier starting with `_` is excluded from checking.
pub fn check(program: &Program, diagnostics: &mut DiagnosticBag) {
    for f in program.functions.values() {
        if crate::stdlib::prelude::is_builtin_function(f) {
            continue;
        }
        check_function_body(f, program, diagnostics);
    }
    for s in program.structs.values() {
        for m in &s.methods {
            check_function_body(m, program, diagnostics);
        }
    }
}

fn check_function_body(decl: &FunctionDecl, program: &Program, diagnostics: &mut DiagnosticBag) {
    let mut scopes = Scopes::new();
    for p in &decl.params {
        scopes.declare(Arc::clone(&p.name), p.span);
    }
    let mut visitor = UnusedVarVisitor {
        scopes,
        program,
        diagnostics,
    };
    visitor.visit_block(&decl.body);
    if decl.name.as_ref() == crate::effects::ENTRY_POINT_NAME {
        mark_module_level_const_doc_fence_usages(decl, &mut visitor.scopes);
    }
    let UnusedVarVisitor {
        mut scopes,
        diagnostics,
        ..
    } = visitor;
    scopes.pop(diagnostics);
}

/// Among the entry's top-level executable statements (the `ENTRY_POINT_NAME` convention,
/// see "fix for a known bug" at the top of this file), scans the `##` doc comment of a
/// statement that syntactically looks like a NameAssign for a module-level const, and
/// marks the names referenced from its doctest fence as used in the current scope. Called
/// only when `decl` is `ENTRY_POINT_NAME` (so it does not also pick up an unrelated
/// `Stmt.doc_comment` nested inside an ordinary `def`/method body -- kept safe together
/// with `doc_fence::collect_doc_fence_names_from_top_level_consts`'s design of looking
/// only directly under `decl.body`).
fn mark_module_level_const_doc_fence_usages(decl: &FunctionDecl, scopes: &mut Scopes) {
    let mut referenced = HashSet::new();
    collect_doc_fence_names_from_top_level_consts(&decl.body, &mut referenced);
    for name in &referenced {
        scopes.mark_used(name);
    }
}

/// Owns the scope stack while walking one function/method body. The recursion itself is
/// the shared [`Visitor`] walker in `lint/mod.rs`; the overrides below only handle the
/// scope-boundary positions (a new binding, a use of a name, a lambda/match-arm/if-branch
/// scope).
struct UnusedVarVisitor<'a> {
    scopes: Scopes,
    program: &'a Program,
    diagnostics: &'a mut DiagnosticBag,
}

impl UnusedVarVisitor<'_> {
    fn is_binding(&self, node_id: crate::ast::NodeId) -> bool {
        self.program.resolutions.bare_ident_kind.get(&node_id) == Some(&BareIdentKind::Binding)
    }
}

impl Visitor for UnusedVarVisitor<'_> {
    fn visit_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::VarDecl { name, value, .. } => {
                self.visit_expr(value);
                self.scopes.declare(Arc::clone(name), stmt.span);
            }
            StmtKind::NameAssign { name, value, .. } => {
                self.visit_expr(value);
                // If a binding already exists in a visible scope, this is a reassignment
                // (the D-MUT family; E3001 is TypeCheck's responsibility) rather than a
                // new binding, so it is not added to D-LINT-01's tracking (judgment call
                // made in this file -- the same visibility determination as
                // check_stmt.rs's check_name_assign).
                if !self.scopes.is_bound_anywhere(name) {
                    self.scopes.declare(Arc::clone(name), stmt.span);
                }
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Ident(name) => self.scopes.mark_used(name),
            ExprKind::Lambda { params, body } => {
                self.scopes.push();
                for p in params {
                    self.scopes
                        .declare_unreportable(Arc::clone(&p.name), p.span);
                }
                self.visit_expr(body);
                self.scopes.pop(self.diagnostics);
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
                    self.scopes.pop(self.diagnostics);
                }
            }
            _ => walk_expr(self, expr),
        }
    }

    fn visit_if(&mut self, if_expr: &IfExpr) {
        self.visit_expr(&if_expr.cond);
        self.scopes.push();
        self.visit_block(&if_expr.then_branch);
        self.scopes.pop(self.diagnostics);
        match &if_expr.else_branch {
            ElseBranch::Block(b) => {
                self.scopes.push();
                self.visit_block(b);
                self.scopes.pop(self.diagnostics);
            }
            ElseBranch::ElseIf(inner) => self.visit_if(inner),
        }
    }

    fn visit_pattern(&mut self, pattern: &Pattern) {
        if let Pattern::BareIdent(name, node_id, span) = pattern
            && self.is_binding(*node_id)
        {
            self.scopes.declare(Arc::clone(name), *span);
        }
        walk_pattern(self, pattern);
    }

    fn visit_subpattern(&mut self, sub: &SubPattern) {
        if let SubPattern::BareIdent(name, node_id, span) = sub
            && self.is_binding(*node_id)
        {
            self.scopes.declare(Arc::clone(name), *span);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Block, FunctionDecl, Item, NodeId, TypeAnn, TypeAnnKind};
    use crate::diagnostics::{Diagnostic, FileId, Position, SourceMap};
    use crate::effects::{ENTRY_POINT_NAME, check_program_effects};
    use crate::lexer::Lexer;
    use crate::module_resolve::{build_program_skeleton, discover_sibling_modules};
    use crate::types::check_decl::check_program;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn sample_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn dummy_span(file: FileId) -> Span {
        Span {
            file,
            start: Position { line: 0, col: 0 },
            end: Position { line: 0, col: 0 },
        }
    }

    /// Runs `entry_path` (plus sibling modules) all the way through lex/parse/
    /// module_resolve/TypeCheck/EffectCheck, registers the entry's top-level executable
    /// statements into `program.functions` as a synthesized `FunctionDecl` per the
    /// [`ENTRY_POINT_NAME`] convention, and then runs `lint::check_all` (reproduces for
    /// testing purposes the wiring driver.rs = Unit17 should ultimately perform -- see the
    /// comment at the top of this file, and "adjustment needed in a file outside this
    /// Unit's scope" at the top of `src/effects/mod.rs`).
    pub(super) fn run_lint_check_all(entry_path: &Path) -> (Vec<Diagnostic>, Arc<SourceMap>) {
        let mut sibling_paths = discover_sibling_modules(entry_path);
        let mut all_paths = vec![entry_path.to_path_buf()];
        all_paths.append(&mut sibling_paths);

        let mut sources = SourceMap::new();
        let mut modules = Vec::new();
        for path in &all_paths {
            let text = fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            let file = sources.add(path.clone(), text.clone());
            let (tokens, _comments, lex_diag) = Lexer::new(&text, file).tokenize();
            assert!(
                lex_diag.is_empty(),
                "{}: unexpected lex error",
                path.display()
            );
            let (module, parse_diag) = crate::parser::parse_module(&tokens, file);
            assert!(
                parse_diag.is_empty(),
                "{}: unexpected parse error",
                path.display()
            );
            modules.push(module);
        }

        let entry_text = fs::read_to_string(entry_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", entry_path.display()));
        let mut entry_sources = SourceMap::new();
        let entry_file = entry_sources.add(entry_path.to_path_buf(), entry_text.clone());
        let (entry_tokens, _c, entry_lex_diag) = Lexer::new(&entry_text, entry_file).tokenize();
        assert!(
            entry_lex_diag.is_empty(),
            "lex error while re-parsing the entry"
        );
        let (entry_module, entry_parse_diag) =
            crate::parser::parse_module(&entry_tokens, entry_file);
        assert!(
            entry_parse_diag.is_empty(),
            "parse error while re-parsing the entry"
        );
        let entry_stmts: Vec<Stmt> = entry_module
            .items
            .into_iter()
            .filter_map(|item| match item {
                Item::Stmt(s) => Some(s),
                Item::Decl(_) => None,
            })
            .collect();

        let mut diagnostics = DiagnosticBag::new();
        let sources_arc = Arc::new(sources);
        let mut program =
            build_program_skeleton(modules, Arc::clone(&sources_arc), &mut diagnostics);
        check_program(&mut program, &entry_stmts, &mut diagnostics);

        let dummy = dummy_span(entry_file);
        let entry_decl = FunctionDecl {
            id: NodeId(u32::MAX),
            name: Arc::from(ENTRY_POINT_NAME),
            generics: Vec::new(),
            self_param: None,
            params: Vec::new(),
            ret: TypeAnn {
                kind: TypeAnnKind::Void,
                span: dummy,
            },
            effects: Vec::new(),
            body: Block {
                stmts: entry_stmts,
                span: dummy,
            },
            leading_comments: Vec::new(),
            doc_comment: None,
            span: dummy,
        };
        program
            .functions
            .insert(Arc::from(ENTRY_POINT_NAME), Arc::new(entry_decl));

        check_program_effects(&mut program, &mut diagnostics);
        crate::lint::check_all(&program, &mut diagnostics);

        let final_sources = Arc::clone(&program.sources);
        let sorted = diagnostics.into_sorted(&final_sources);
        (sorted, final_sources)
    }

    fn e4xxx_codes(diags: &[Diagnostic]) -> Vec<String> {
        let mut codes: Vec<String> = diags
            .iter()
            .filter(|d| d.code.numeric() / 1000 == 4)
            .map(|d| d.code.to_string())
            .collect();
        codes.sort();
        codes
    }

    fn subdirs(rel: &str) -> Vec<PathBuf> {
        let base = sample_path(rel);
        let Ok(entries) = fs::read_dir(&base) else {
            panic!("cannot read {}", base.display());
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs
    }

    #[test]
    fn e4001_unused_variable_sample_matches() {
        let dir = sample_path("samples/err/lint/e4001_unused_variable");
        let (diags, sources) = run_lint_check_all(&dir.join("entry_unused_local.ybm"));
        assert_eq!(
            e4xxx_codes(&diags),
            vec!["E4001".to_owned()],
            "all: {:?}",
            diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn e4002_unused_function_sample_matches() {
        let dir = sample_path("samples/err/lint/e4002_unused_function");
        let (diags, sources) = run_lint_check_all(&dir.join("entry_with_dead_function.ybm"));
        assert_eq!(
            e4xxx_codes(&diags),
            vec!["E4002".to_owned()],
            "all: {:?}",
            diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
        );
    }

    /// Verifies that E4003 is reported exactly once for each of the 4 inner scopes
    /// D-LINT-03 enumerates (function boundary/if/match/lambda parameter), with no other
    /// E4xxx/pre-lint diagnostics mixed in. Case (2)'s if case uses an explicit new
    /// binding via `var` (see the comment in the sample itself), so it does not get
    /// mistaken for a bare reassignment and turned into E3001, and purely triggers E4003
    /// alone.
    #[test]
    fn e4003_shadowing_sample_matches() {
        let dir = sample_path("samples/err/lint/e4003_shadowing");
        let (diags, sources) = run_lint_check_all(&dir.join("entry_shadowing_various_scopes.ybm"));
        assert!(
            !has_pre_lint_diagnostics(&diags),
            "no type-check/mutability diagnostics such as E3001 should be mixed in: {:?}",
            diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
        );
        assert_eq!(
            e4xxx_codes(&diags),
            vec![
                "E4003".to_owned(),
                "E4003".to_owned(),
                "E4003".to_owned(),
                "E4003".to_owned(),
            ],
            "all: {:?}",
            diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn e4004_unreachable_code_sample_matches() {
        let dir = sample_path("samples/err/lint/e4004_unreachable_code");
        let (diags, sources) = run_lint_check_all(&dir.join("entry_code_after_return.ybm"));
        assert_eq!(
            e4xxx_codes(&diags),
            vec!["E4004".to_owned()],
            "all: {:?}",
            diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn e4005_naming_convention_sample_matches() {
        let dir = sample_path("samples/err/lint/e4005_naming_convention");
        let (diags, sources) = run_lint_check_all(&dir.join("entry_naming_violations.ybm"));
        assert_eq!(
            e4xxx_codes(&diags),
            vec![
                "E4005".to_owned(),
                "E4005".to_owned(),
                "E4005".to_owned(),
                "E4005".to_owned(),
                "E4005".to_owned(),
                "E4005".to_owned(),
            ],
            "all: {:?}",
            diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
        );
    }

    /// Whether this `entry_*.ybm` has even a single TypeCheck/EffectCheck diagnostic
    /// (E1xxx/E2xxx/E3xxx). Per ARCHITECTURE.md §4.2 "a phase does not run the next phase
    /// unless the previous one produced zero diagnostics", the real driver.rs pipeline
    /// would never reach Lint in that case -- since this test is a simplified harness that
    /// calls each phase individually (without gating), it reproduces this determination on
    /// its own (because a deliberate contrast sample containing E1020 on purpose, such as
    /// samples/ok/5b_return_implicit_ok_some_wrap/entry_type_mismatch.ybm, is mixed in
    /// under `ok/`).
    fn has_pre_lint_diagnostics(diags: &[Diagnostic]) -> bool {
        diags.iter().any(|d| {
            let n = d.code.numeric();
            (1000..3000).contains(&n) || n == 3001
        })
    }

    /// Unit9 task instruction: verifies that lint reports zero warnings across every
    /// directory under `samples/ok/` (the simple norm "passing check == clean", SPEC
    /// §12). No exclusion list is maintained -- the known inconsistencies have already
    /// been resolved via the D-LINT-02 revision (module `def`s excluded from the warning)
    /// and sample-side fixes to `6-3_pipe_operator`/`e4003_shadowing`. Only files where an
    /// earlier phase (TypeCheck/EffectCheck) already produced a nonzero count (deliberate
    /// contrast samples that would never reach Lint in the real pipeline) are excluded.
    #[test]
    fn all_ok_samples_have_zero_e4xxx() {
        let mut failures = Vec::new();
        for dir in subdirs("samples/ok") {
            let Ok(read_dir) = fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<PathBuf> = read_dir
                .filter_map(std::result::Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ybm"))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("entry_"))
                })
                .collect();
            entries.sort();
            for entry_path in entries {
                let (diags, sources) = run_lint_check_all(&entry_path);
                if has_pre_lint_diagnostics(&diags) {
                    continue;
                }
                let codes = e4xxx_codes(&diags);
                if !codes.is_empty() {
                    failures.push(format!(
                        "{}: unexpected E4xxx: {:?}\n  all: {:?}",
                        entry_path.display(),
                        codes,
                        diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n---\n"));
    }
}
