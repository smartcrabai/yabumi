//! Detection of E5002 (top-level executable statement inside a
//! module) (ARCHITECTURE.md §2.1/§4.2).

use crate::ast::{Expr, ExprKind, Item, Module, Stmt, StmtKind};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode};
use std::sync::Arc;

/// D-MOD-02's restricted grammar: the right-hand side is limited to numeric/string/bool/
/// collection literals and combinations of them (including references to other constants,
/// i.e. identifiers), and forbids expressions that include a function call. A simple
/// parenthesized wrapper (`Grouping`) around `(expr)` is merely a visual difference (its
/// evaluated result is identical to the inner expression), so as a decision made here it is
/// treated transparently, recursing the same check into the inner expression.
fn is_module_const_value_expr(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::Ident(_) => true,
        ExprKind::ListLit { elements, .. } | ExprKind::SetLit { elements, .. } => {
            elements.iter().all(is_module_const_value_expr)
        }
        ExprKind::TupleLit { elements, .. } => elements.iter().all(is_module_const_value_expr),
        ExprKind::DictLit { entries, .. } => entries
            .iter()
            .all(|(k, v)| is_module_const_value_expr(k) && is_module_const_value_expr(v)),
        ExprKind::Grouping(inner) => is_module_const_value_expr(inner),
        // FString/Unary/Binary/Call/MethodCall/FieldAccess/TupleIndex/Index/Question/Pipe/
        // Lambda/If/Match/Par all carry executable-statement-like meaning equivalent to (or
        // beyond) a function call, so they are rejected as targets of D-MOD-02's
        // "expressions including a function call are forbidden" rule.
        _ => false,
    }
}

/// Returns `Some((name, value))` if `stmt` matches D-MOD-02's "module-level constant"
/// pattern (`NameAssign` whose right-hand side satisfies the restricted grammar above).
/// Reused by `register_flat_namespace` in `flat_namespace.rs` to obtain, under the same
/// criterion, the values it registers into Program.consts (a shared helper so the check's
/// criterion is not implemented twice in two places).
pub(crate) fn module_level_const(stmt: &Stmt) -> Option<(&Arc<str>, &Expr)> {
    match &stmt.kind {
        StmtKind::NameAssign { name, value, .. } if is_module_const_value_expr(value) => {
            Some((name, value))
        }
        _ => None,
    }
}

/// For a file where `Module.is_module_directive == true`, checks whether each `Item::Stmt`
/// satisfies D-MOD-02's restricted grammar (`NameAssign` whose right-hand side is only a
/// combination of literals/collection literals/constant references, with function calls
/// forbidden). If even one `Item::Stmt` fails to satisfy it, reports E5002 at that `Item`'s
/// `Span`. `Item::Decl` (def/struct/enum) is exactly the form SPEC §10 "a module is
/// declarations only" allows, so it is always permitted.
pub fn check_module_toplevel_grammar(module: &Module, diagnostics: &mut DiagnosticBag) {
    for item in &module.items {
        let Item::Stmt(stmt) = item else {
            continue;
        };
        if module_level_const(stmt).is_none() {
            diagnostics.push(Diagnostic {
                code: ErrorCode::ModuleTopLevelStatement,
                span: stmt.span,
                message: "a file with a module directive cannot contain a top-level \
                          executable statement (only declarations, or module-level \
                          constants that are literals only, are allowed; SPEC §10, D-MOD-02)"
                    .to_owned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::SourceMap;
    use crate::lexer::Lexer;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn sample_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn read_sample(rel: &str) -> String {
        let path = sample_path(rel);
        match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => panic!("failed to read sample file {}: {e}", path.display()),
        }
    }

    /// A simplified lex+parse pipeline for tests only (structured the same way as the tests
    /// in Unit4's `parser::mod.rs`).
    fn lex_and_parse(src: &str) -> Module {
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("test.ybm"), src.to_owned());
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "lex diagnostics: {lex_diag:?}");
        let (module, parse_diag) = crate::parser::parse_module(&tokens, file);
        assert!(
            parse_diag.is_empty(),
            "parse diagnostics: {:?}",
            parse_diag.into_vec()
        );
        module
    }

    #[test]
    fn module_level_const_accepts_literal_and_collection_combinations() {
        let module = lex_and_parse(
            "module\n\n\
             a = 1\n\
             b = \"s\"\n\
             c = true\n\
             d = [1, 2, a]\n\
             e = {\"k\": a, \"j\": 2}\n\
             f = (1, 2,)\n\
             g = {1, 2, a}\n",
        );
        for item in &module.items {
            let Item::Stmt(stmt) = item else {
                panic!("expected something other than a declaration");
            };
            assert!(
                module_level_const(stmt).is_some(),
                "a combination of literals/collections/constant references should be allowed"
            );
        }
    }

    #[test]
    fn module_level_const_rejects_function_call_rhs() {
        let module = lex_and_parse("module\n\nx = foo()\n");
        let Item::Stmt(stmt) = &module.items[0] else {
            panic!("expected Item::Stmt");
        };
        assert!(
            module_level_const(stmt).is_none(),
            "an expression that includes a function call is not allowed as a module-level constant (D-MOD-02)"
        );
    }

    #[test]
    fn check_module_toplevel_grammar_flags_bare_call_statement_e5002() {
        // Uses samples/err/static/10c_module_toplevel_statement_cascade/mod_broken.ybm as an
        // actual file, to verify that the top-level print statement becomes E5002.
        let src =
            read_sample("samples/err/static/10c_module_toplevel_statement_cascade/mod_broken.ybm");
        let module = lex_and_parse(&src);
        assert!(module.is_module_directive);
        let mut diagnostics = DiagnosticBag::new();
        check_module_toplevel_grammar(&module, &mut diagnostics);
        let diags = diagnostics.into_vec();
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, ErrorCode::ModuleTopLevelStatement);
    }

    #[test]
    fn check_module_toplevel_grammar_flags_only_the_offending_statement() {
        // samples/err/static/10d_entry_self_module_directive: the def declaration should be
        // allowed, and only the trailing print statement should be reported as E5002.
        let src = read_sample(
            "samples/err/static/10d_entry_self_module_directive/entry_with_module_directive.ybm",
        );
        let module = lex_and_parse(&src);
        assert!(module.is_module_directive);
        assert_eq!(
            module.items.len(),
            2,
            "expected 1 def declaration + 1 print ExprStmt"
        );
        let mut diagnostics = DiagnosticBag::new();
        check_module_toplevel_grammar(&module, &mut diagnostics);
        let diags = diagnostics.into_vec();
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, ErrorCode::ModuleTopLevelStatement);
    }

    #[test]
    fn check_module_toplevel_grammar_accepts_module_const_samples() {
        // samples/ok/10c_module_constants_and_cross_reference/mod_constants.ybm:
        // every item is a module-level constant made of literals only, so there should be
        // zero E5002s.
        let src =
            read_sample("samples/ok/10c_module_constants_and_cross_reference/mod_constants.ybm");
        let module = lex_and_parse(&src);
        assert!(module.is_module_directive);
        let mut diagnostics = DiagnosticBag::new();
        check_module_toplevel_grammar(&module, &mut diagnostics);
        assert!(diagnostics.is_empty(), "{:?}", diagnostics.into_vec());
    }
}
