//! Type checking of declarations (def/struct/enum/const) (ARCHITECTURE.md §2.2).

use crate::ast::{EnumDecl, FunctionDecl, Stmt, StmtKind, StructDecl};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode};
use crate::eval::env::Program;
use crate::types::check_stmt::{block_diverges, check_stmt};
use crate::types::env::TypeEnv;
use crate::types::generics::ty_from_ann;
use crate::types::{EffectSet, Ty};
use std::sync::Arc;

/// Checks every declaration body held by the `Program` skeleton (only declarations
/// registered so far). The main body of the ARCHITECTURE.md §4.2 TypeCheck phase --
/// judgment call made in this file: assumes driver.rs (Unit17, unimplemented as of this
/// writing) will call this function followed by [`check_top_level_stmts`].
pub fn check_all_decls(program: &mut Program, diagnostics: &mut DiagnosticBag) {
    let structs: Vec<Arc<StructDecl>> = program.structs.values().cloned().collect();
    for s in &structs {
        check_struct_decl(s, program, diagnostics);
    }
    let enums: Vec<Arc<EnumDecl>> = program.enums.values().cloned().collect();
    for e in &enums {
        check_enum_decl(e, program, diagnostics);
    }
    let funcs: Vec<Arc<FunctionDecl>> = program.functions.values().cloned().collect();
    for f in &funcs {
        check_function_decl(f, &[], None, program, diagnostics);
    }
}

/// Checks the entry file's top-level executable statements (`Item::Stmt`, non-
/// declarations) in source order (D-SYN-08: declarations are already hoisted, but non-
/// declaration statements are checked in source order). Since the top level is outside
/// any function boundary, `ret_ctx` is always `None` (`return`/`?` are not allowed -- the
/// grammatical constraint is Unit4/Unit8's responsibility; from the type-checking
/// viewpoint, this merely represents "no expected return type").
pub fn check_top_level_stmts(
    stmts: &[Stmt],
    program: &mut Program,
    diagnostics: &mut DiagnosticBag,
) {
    let mut env = TypeEnv::root();
    let mut effects = EffectSet::empty();
    for s in stmts {
        check_stmt(s, None, &mut env, program, &mut effects, diagnostics);
    }
}

/// Convenience function that runs [`check_all_decls`] followed by
/// [`check_top_level_stmts`].
pub fn check_program(
    program: &mut Program,
    entry_top_level_stmts: &[Stmt],
    diagnostics: &mut DiagnosticBag,
) {
    check_all_decls(program, diagnostics);
    check_top_level_stmts(entry_top_level_stmts, program, diagnostics);
}

fn contains_nested_void(ty: &Ty) -> bool {
    match ty {
        Ty::List(element) | Ty::Set(element) => contains_void(element),
        Ty::Dict(key, value) => contains_void(key) || contains_void(value),
        Ty::Tuple(items) | Ty::Named { args: items, .. } => items.iter().any(contains_void),
        Ty::Function { params, ret, .. } => params.iter().any(contains_void) || contains_void(ret),
        _ => false,
    }
}

fn contains_void(ty: &Ty) -> bool {
    matches!(ty, Ty::Void) || contains_nested_void(ty)
}

/// Type-checks a function/method body. Applies §5.6 "the function body value rule"
/// (the void/non-void branch, divergence determination via `check_stmt::block_diverges`).
///
/// `extra_generics` is (for a method) the type parameters of the enclosing struct/enum
/// declaration itself (D-FUNC-04, brought into scope alongside `decl.generics`).
/// `self_info` is `Some((self_ty, mutable))` -- `self_ty` is
/// `Ty::Named{struct name, the struct's own TypeVar list}`, and `mutable` is whether it is
/// `var self` (D-MUT-01). Both are empty/`None` for a top-level function.
#[expect(
    clippy::too_many_lines,
    reason = "function declaration checking keeps one shared environment and return context"
)]
pub fn check_function_decl(
    decl: &FunctionDecl,
    extra_generics: &[Arc<str>],
    self_info: Option<(&Ty, bool)>,
    program: &mut Program,
    diagnostics: &mut DiagnosticBag,
) {
    let combined_generics: Vec<Arc<str>> = extra_generics
        .iter()
        .cloned()
        .chain(decl.generics.iter().cloned())
        .collect();
    let mut env = TypeEnv::for_function(combined_generics.clone());
    if let Some((self_ty, mutable)) = self_info {
        env.bind(Arc::from("self"), self_ty.clone(), mutable);
    }
    for parameter in &decl.params {
        match ty_from_ann(&parameter.ty, &combined_generics, program) {
            Some(ty) if contains_void(&ty) => {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span: parameter.span,
                    message: "void cannot appear in a parameter type".to_owned(),
                });
                env.bind(Arc::clone(&parameter.name), Ty::Unknown, false);
            }
            Some(ty) => env.bind(Arc::clone(&parameter.name), ty, false),
            None => {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::MissingParamAnnotation,
                    span: parameter.span,
                    message: format!(
                        "parameter '{}' has no valid type annotation (D-TYPE-11)",
                        parameter.name
                    ),
                });
                env.bind(Arc::clone(&parameter.name), Ty::Unknown, false);
            }
        }
    }
    let mut ret_ty = ty_from_ann(&decl.ret, &combined_generics, program).unwrap_or(Ty::Unknown);
    if contains_nested_void(&ret_ty) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: decl.ret.span,
            message: "void cannot appear inside a return type".to_owned(),
        });
        ret_ty = Ty::Unknown;
    }
    let mut effects = EffectSet::empty();

    if block_diverges(&decl.body) {
        for s in &decl.body.stmts {
            check_stmt(
                s,
                Some(&ret_ty),
                &mut env,
                program,
                &mut effects,
                diagnostics,
            );
        }
        return;
    }

    if matches!(ret_ty, Ty::Void) {
        for s in &decl.body.stmts {
            check_stmt(
                s,
                Some(&ret_ty),
                &mut env,
                program,
                &mut effects,
                diagnostics,
            );
        }
        return;
    }

    let Some((last, rest)) = decl.body.stmts.split_last() else {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: decl.span,
            message: "the body of a function whose return type is not void cannot be empty (D-SYN-11/§5.6)".to_owned(),
        });
        return;
    };
    for s in rest {
        check_stmt(
            s,
            Some(&ret_ty),
            &mut env,
            program,
            &mut effects,
            diagnostics,
        );
    }
    check_stmt(
        last,
        Some(&ret_ty),
        &mut env,
        program,
        &mut effects,
        diagnostics,
    );
    if !matches!(&last.kind, StmtKind::Return(Some(_))) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: last.span,
            message: "a non-diverging function body must end with an explicit return (§5.6)"
                .to_owned(),
        });
    }
}

/// Validity checking of struct field types (D-TYPE-08: void cannot be placed in a type-
/// argument position), and the [`check_function_decl`] call for each method (D-FUNC-01: a
/// method's `uses` is written individually -- this function does not validate a method's
/// `uses` itself; that is EffectCheck = Unit8's responsibility).
pub fn check_struct_decl(
    decl: &StructDecl,
    program: &mut Program,
    diagnostics: &mut DiagnosticBag,
) {
    for f in &decl.fields {
        check_field_or_variant_field_ty(&f.ty, &decl.generics, f.span, program, diagnostics);
    }
    let self_ty = Ty::Named {
        name: Arc::clone(&decl.name),
        args: decl
            .generics
            .iter()
            .map(|g| Ty::TypeVar(Arc::clone(g)))
            .collect(),
    };
    for m in &decl.methods {
        let mutable = m.self_param.as_ref().is_some_and(|sp| sp.mutable);
        check_function_decl(
            m,
            &decl.generics,
            Some((&self_ty, mutable)),
            program,
            diagnostics,
        );
    }
}

/// Checks enum variant field types, and D-TYPE-08's prohibition on void in a type-
/// argument position.
pub fn check_enum_decl(decl: &EnumDecl, program: &mut Program, diagnostics: &mut DiagnosticBag) {
    for v in &decl.variants {
        for f in &v.fields {
            check_field_or_variant_field_ty(f, &decl.generics, v.span, program, diagnostics);
        }
    }
}

fn check_field_or_variant_field_ty(
    ty_ann: &crate::ast::TypeAnn,
    generics: &[Arc<str>],
    span: crate::diagnostics::Span,
    program: &Program,
    diagnostics: &mut DiagnosticBag,
) {
    match ty_from_ann(ty_ann, generics, program) {
        Some(ty) if contains_void(&ty) => diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span,
            message: "void cannot be held as a type (D-TYPE-08)".to_owned(),
        }),
        Some(_) => {}
        None => diagnostics.push(Diagnostic {
            code: ErrorCode::MissingParamAnnotation,
            span,
            message: "cannot resolve the type annotation".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Diagnostic, ErrorCode, SourceMap};
    use crate::lexer::Lexer;
    use crate::module_resolve::{build_program_skeleton, discover_sibling_modules};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn sample_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    /// Lexes+parses `entry_path` and its sibling modules, builds the `Program` skeleton
    /// with `build_program_skeleton`, and runs all the way through `check_program`
    /// (TypeCheck, this Unit's scope). E1001/E5001/E5002 (diagnostics module_resolve
    /// detects during construction) also come back mixed into the same `DiagnosticBag` --
    /// the test side filters down to only E1xxx/E3001 for comparison (E1001 is
    /// module_resolve's responsibility, but since it is a decided code that falls within
    /// the E1xxx range, it naturally remains subject to the match check even after
    /// filtering).
    ///
    /// Since `build_program_skeleton` discards the entry file's top-level executable
    /// statements (moving only declarations into `Program`), the entry file is lexed+
    /// parsed a second time, independently, to extract them (the same "lightly re-
    /// implement driver.rs's Lex/Parse for testing purposes" approach as the existing
    /// tests in module_resolve/mod.rs. The two parse passes produce mismatched NodeId
    /// values, but that causes no problem for use as keys into `Resolutions`).
    fn typecheck_entry(entry_path: &Path) -> (Vec<Diagnostic>, std::sync::Arc<SourceMap>) {
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
                "{}: unexpected lex error: {:?}",
                path.display(),
                lex_diag.into_vec()
            );
            let (module, parse_diag) = crate::parser::parse_module(&tokens, file);
            assert!(
                parse_diag.is_empty(),
                "{}: unexpected parse error: {:?}",
                path.display(),
                parse_diag.into_vec()
            );
            modules.push(module);
        }

        let entry_text = fs::read_to_string(entry_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", entry_path.display()));
        let mut entry_sources = SourceMap::new();
        let entry_file = entry_sources.add(entry_path.to_path_buf(), entry_text.clone());
        let (entry_tokens, _entry_comments, entry_lex_diag) =
            Lexer::new(&entry_text, entry_file).tokenize();
        assert!(
            entry_lex_diag.is_empty(),
            "lex error while re-parsing the entry: {entry_lex_diag:?}"
        );
        let (entry_module_standalone, entry_parse_diag) =
            crate::parser::parse_module(&entry_tokens, entry_file);
        assert!(
            entry_parse_diag.is_empty(),
            "parse error while re-parsing the entry: {entry_parse_diag:?}"
        );
        let entry_stmts: Vec<Stmt> = entry_module_standalone
            .items
            .into_iter()
            .filter_map(|item| match item {
                crate::ast::Item::Stmt(s) => Some(s),
                crate::ast::Item::Decl(_) => None,
            })
            .collect();

        let mut diagnostics = DiagnosticBag::new();
        let sources_arc = std::sync::Arc::new(sources);
        let mut program = build_program_skeleton(
            modules,
            std::sync::Arc::clone(&sources_arc),
            &mut diagnostics,
        );
        check_program(&mut program, &entry_stmts, &mut diagnostics);
        let final_sources = std::sync::Arc::clone(&program.sources);
        let sorted = diagnostics.into_sorted(&final_sources);
        (sorted, final_sources)
    }

    /// A list of code strings filtered down to only E1xxx (1000-1999) or E3001
    /// (mutability) (sorted ascending, for comparison).
    fn e1xxx_e3001_codes(diags: &[Diagnostic]) -> Vec<String> {
        let mut codes: Vec<String> = diags
            .iter()
            .filter(|d| d.code.numeric() / 1000 == 1 || d.code == ErrorCode::ImmutableMutation)
            .map(|d| d.code.to_string())
            .collect();
        codes.sort();
        codes
    }

    /// Extracts the mapping of `[[case]] entry = "..." diagnostics = [...]` from
    /// `dir/expected.toml` via a naive line-based parse (this crate does not depend on the
    /// `toml` crate -- Cargo.toml is outside Unit7's scope and cannot be added to; since
    /// SAMPLES_PLAN.md §1.3's schema uses only a simple one-line-per-field format, this
    /// lightweight ad-hoc extraction is sufficient rather than pulling in a dedicated
    /// parser -- judgment call made in this file).
    fn read_expected_diagnostics(dir: &Path) -> Vec<(String, Vec<String>)> {
        let text = fs::read_to_string(dir.join("expected.toml"))
            .unwrap_or_else(|e| panic!("{}: failed to read expected.toml: {e}", dir.display()));
        let mut cases: Vec<(String, Vec<String>)> = Vec::new();
        let mut current_entry: Option<String> = None;
        let mut current_diags: Vec<String> = Vec::new();
        let mut in_case = false;
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line == "[[case]]" {
                if in_case {
                    cases.push((
                        current_entry.take().unwrap_or_default(),
                        std::mem::take(&mut current_diags),
                    ));
                }
                in_case = true;
                current_entry = None;
                current_diags = Vec::new();
                continue;
            }
            if !in_case {
                continue;
            }
            if let Some(rest) = line.strip_prefix("entry = ") {
                current_entry = Some(rest.trim().trim_matches('"').to_owned());
            } else if let Some(rest) = line.strip_prefix("diagnostics = ") {
                current_diags = rest
                    .trim()
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        if in_case {
            cases.push((current_entry.unwrap_or_default(), current_diags));
        }
        cases
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

    /// Verifies, for every directory under samples/ok/, that the E1xxx/E3001 diagnostics
    /// each `[[case]]` expects (normally empty -- only entry_type_mismatch of
    /// 5b_return_implicit_ok_some_wrap, an irregular D-TYPE-17 configuration, expects
    /// `["E1020"]`) match what TypeCheck actually reports.
    #[test]
    fn ok_samples_match_expected_e1xxx_e3001_diagnostics() {
        let mut failures = Vec::new();
        for dir in subdirs("samples/ok") {
            let cases = read_expected_diagnostics(&dir);
            for (entry, expected) in cases {
                if entry.is_empty() {
                    continue;
                }
                let entry_path = dir.join(&entry);
                if !entry_path.exists() {
                    continue;
                }
                let expected_e1: Vec<String> = {
                    let mut v: Vec<String> = expected
                        .iter()
                        .filter(|c| c.starts_with("E1") || c.as_str() == "E3001")
                        .cloned()
                        .collect();
                    v.sort();
                    v
                };
                let (diags, sources) = typecheck_entry(&entry_path);
                let actual_e1 = e1xxx_e3001_codes(&diags);
                if actual_e1 != expected_e1 {
                    let rendered: Vec<String> = diags.iter().map(|d| d.render(&sources)).collect();
                    failures.push(format!(
                        "{}: expected {:?}, got {:?}\n  all diagnostics: {:?}",
                        entry_path.display(),
                        expected_e1,
                        actual_e1,
                        rendered
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    /// Verifies, for every directory under samples/err/static/ that has at least one
    /// `[[case]]` expecting E1xxx/E3001, that the actual diagnostics match (cases
    /// expecting only E0xxx (lex/parse), E2xxx (effect), or E5xxx (module) are naturally
    /// excluded by this filter -- they are Unit2/4/5/8's responsibility).
    #[test]
    fn err_static_samples_match_expected_e1xxx_e3001_diagnostics() {
        let mut failures = Vec::new();
        for dir in subdirs("samples/err/static") {
            let cases = read_expected_diagnostics(&dir);
            for (entry, expected) in cases {
                if entry.is_empty() {
                    continue;
                }
                let expected_e1: Vec<String> = {
                    let mut v: Vec<String> = expected
                        .iter()
                        .filter(|c| c.starts_with("E1") || c.as_str() == "E3001")
                        .cloned()
                        .collect();
                    v.sort();
                    v
                };
                if expected_e1.is_empty() {
                    // This case expects no E1xxx/E3001 at all (only diagnostics outside Unit7's scope).
                    continue;
                }
                let entry_path = dir.join(&entry);
                if !entry_path.exists() {
                    continue;
                }
                let (diags, sources) = typecheck_entry(&entry_path);
                let actual_e1 = e1xxx_e3001_codes(&diags);
                if actual_e1 != expected_e1 {
                    let rendered: Vec<String> = diags.iter().map(|d| d.render(&sources)).collect();
                    failures.push(format!(
                        "{}: expected {:?}, got {:?}\n  all diagnostics: {:?}",
                        entry_path.display(),
                        expected_e1,
                        actual_e1,
                        rendered
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    fn assert_zero_e1xxx_e3001(entry_path: &Path) {
        let (diags, sources) = typecheck_entry(entry_path);
        let actual = e1xxx_e3001_codes(&diags);
        assert!(
            actual.is_empty(),
            "{}: unexpected E1xxx/E3001 diagnostic: {:?}\n  all: {:?}",
            entry_path.display(),
            actual,
            diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
        );
    }

    /// Individually confirms that D-TYPE-17 (implicit Ok/Some wrap) and the VOID-VALUE-
    /// AND-BLOCK-VALUE-RULE-CONFLICT decision (divergence determination) coexist without
    /// contradiction -- a pairing explicitly named in the implementation instructions.
    #[test]
    fn d_type_17_and_9_concurrency_par_are_mutually_consistent() {
        let dir5b = sample_path("samples/ok/5b_return_implicit_ok_some_wrap");
        assert_zero_e1xxx_e3001(&dir5b.join("entry_implicit_wrap.ybm"));
        assert_zero_e1xxx_e3001(&dir5b.join("entry_explicit_ok_wrap.ybm"));
        let (diags, _sources) = typecheck_entry(&dir5b.join("entry_type_mismatch.ybm"));
        assert_eq!(e1xxx_e3001_codes(&diags), vec!["E1020".to_owned()]);

        let dir9 = sample_path("samples/ok/9_concurrency_par");
        assert_zero_e1xxx_e3001(&dir9.join("entry_par_fixed_arity.ybm"));
        assert_zero_e1xxx_e3001(&dir9.join("entry_par_map_and_each.ybm"));
        assert_zero_e1xxx_e3001(&dir9.join("entry_par_nested.ybm"));
    }

    #[test]
    fn generics_sample_typechecks_cleanly() {
        let dir = sample_path("samples/ok/3-6_generics");
        assert_zero_e1xxx_e3001(&dir.join("entry_generic_struct_and_enum.ybm"));
        assert_zero_e1xxx_e3001(&dir.join("entry_main.ybm"));
    }

    #[test]
    fn match_exhaustiveness_sample_typechecks_cleanly() {
        let dir = sample_path("samples/ok/6-1_expression_oriented_if_match");
        assert_zero_e1xxx_e3001(&dir.join("entry_main.ybm"));
        assert_zero_e1xxx_e3001(&dir.join("entry_multi_statement_block_value.ybm"));
        assert_zero_e1xxx_e3001(&dir.join("entry_non_enum_match_with_wildcard_and_bool.ybm"));
    }

    #[test]
    fn mutability_sample_typechecks_cleanly() {
        let dir = sample_path("samples/ok/4_mutability");
        assert_zero_e1xxx_e3001(&dir.join("entry_main.ybm"));
    }

    #[test]
    fn value_semantics_sample_typechecks_cleanly() {
        let dir = sample_path("samples/ok/14_memory_model_value_semantics");
        assert_zero_e1xxx_e3001(&dir.join("entry_main.ybm"));
    }

    #[test]
    fn generic_operator_misuse_reports_e1013_only() {
        let dir = sample_path("samples/err/static/3-6_generic_operator_misuse");
        let (diags, _sources) =
            typecheck_entry(&dir.join("entry_unconstrained_type_param_operator.ybm"));
        assert_eq!(e1xxx_e3001_codes(&diags), vec!["E1013".to_owned()]);
    }

    #[test]
    fn mutability_errors_report_e3001_each() {
        let dir = sample_path("samples/err/static/4_mutability_errors");
        for entry in [
            "entry_reassign_immutable.ybm",
            "entry_field_write_immutable.ybm",
            "entry_var_self_required.ybm",
            "entry_nested_root_not_var.ybm",
        ] {
            let (diags, _sources) = typecheck_entry(&dir.join(entry));
            assert_eq!(
                e1xxx_e3001_codes(&diags),
                vec!["E3001".to_owned()],
                "entry: {entry}"
            );
        }
        // entry_subscript_write_immutable.ybm reports only the E3001 for the index
        // assignment. This sample used to include `print(xs)` (xs: list[int]), which also
        // co-triggered E1020 because print is an overload restricted to
        // str/int/float/bool (STDLIB.md §13 / D-STDPOL-01/02); that has since been fixed
        // to `print(xs[0])` (matching the other 4 cases), as it was an oversight on the
        // samples side that interfered with verifying E3001.
        let (diags, _sources) = typecheck_entry(&dir.join("entry_subscript_write_immutable.ybm"));
        assert_eq!(e1xxx_e3001_codes(&diags), vec!["E3001".to_owned()]);
    }

    #[test]
    fn toml_encode_list_root_remains_supported() {
        let dir = sample_path("samples/err/static/11-1_toml_encode_root_type_error");
        let (diags, _sources) = typecheck_entry(&dir.join("entry_toml_encode_list_root.ybm"));
        assert!(e1xxx_e3001_codes(&diags).is_empty());
    }

    #[test]
    fn full_diagnostic_report_ordering_reports_three_errors_in_line_order() {
        let dir = sample_path("samples/err/static/1_full_diagnostic_report_ordering");
        let (diags, _sources) = typecheck_entry(&dir.join("entry_multiple_independent_errors.ybm"));
        let codes: Vec<String> = diags.iter().map(|d| d.code.to_string()).collect();
        assert_eq!(
            codes,
            vec!["E1002".to_owned(), "E1050".to_owned(), "E1021".to_owned()]
        );
    }
}
