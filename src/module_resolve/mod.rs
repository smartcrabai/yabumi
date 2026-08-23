//! Enumeration of sibling `.ybm` files, module directive detection, and `Program` skeleton
//! construction (ARCHITECTURE.md §2.1).

pub mod flat_namespace;
pub mod module_grammar;

use crate::ast::{Decl, Item, Module};
use crate::diagnostics::{DiagnosticBag, SourceMap};
use crate::eval::env::Program;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Enumerates the paths of `.ybm` files carrying a `module` directive that exist in the same
/// directory as the entry file (immediate directory only, subdirectories are not scanned,
/// D-MOD-01); the entry file itself is not included.
///
/// Deciding whether a file "carries a module directive" does not require compliance with
/// D-LEX-08 (a bare keyword with no name attached) -- a file with a syntactically malformed
/// directive (e.g. `module foo`) is still included in the set of "candidates scanned for
/// auto-import" (samples/err/static/10b_module_directive_malformed: this verifies that E5001
/// is reported as a result of `mod_bad_directive.ybm`, which has a malformed directive, being
/// auto-imported -- if the import decision itself excluded it, E5001 would never occur, which
/// would be a contradiction). So the sole condition checked is "does the effective first line,
/// after shebang removal, start with a `module` token", and what follows it (whether it is
/// well-formed) is not asked. This check reuses Unit2's already-implemented `lexer::Lexer` as
/// is (rather than reimplementing shebang removal and tokenization behavior here, so that this
/// always guarantees the same behavior as the lexer body itself with respect to D-LEX-09's
/// spec of "skip only the shebang, do not skip comment lines etc.").
pub fn discover_sibling_modules(entry_path: &Path) -> Vec<PathBuf> {
    let dir = entry_path.parent().unwrap_or_else(|| Path::new("."));
    let entry_file_name = entry_path.file_name();

    let Ok(dir_entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir_entry in dir_entries {
        let Ok(dir_entry) = dir_entry else { continue };
        let Ok(file_type) = dir_entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = dir_entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("ybm") {
            continue;
        }
        if path.file_name() == entry_file_name {
            // The entry file itself is never subject to auto-import (D-MOD-01).
            continue;
        }
        if file_starts_with_module_keyword(&path) {
            candidates.push(path);
        }
    }
    // The discovery order (an OS-dependent directory enumeration order) can affect D-MOD-05's
    // "position of the second definition" check, so it is pinned to a deterministic order
    // (lexicographic order of the path string) -- a decision made in this file.
    candidates.sort();
    candidates
}

/// Reads the contents of `path` and determines whether the leading token of the effective
/// first line, after shebang removal, is `TokenKind::Module` (D-LEX-08/09). If reading fails,
/// returns false on the safe side (excluded from import).
fn file_starts_with_module_keyword(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    crate::lexer::text_starts_with_module_keyword(&text)
}

/// Builds the `Program` skeleton (only declarations registered; the contents of function
/// bodies etc. are not yet checked) from all already-lexed and parsed `Module`s (the entry
/// file plus same-directory modules), by running each of the `register_flat_namespace` and
/// `module_grammar` checks (§4.2).
///
/// Call order (a decision made in this file): (1) for files where `is_module_directive` is
/// set, the D-MOD-02 top-level grammar check (`check_module_toplevel_grammar`, E5002), (2)
/// flat namespace construction across all files (`register_flat_namespace`, E1001 +
/// finalizing `Program.consts`), in this order. (The D-LEX-08/09 syntax check for the module
/// directive itself, E5001, is already completed on the `parser::parse_module` side, so this
/// phase does not redo it.) Per D-CLI-03 (collect everything), steps (1)-(2) are all run
/// regardless of whether diagnostics occur, and every diagnostic produced is gathered into a
/// single `DiagnosticBag`.
/// Finally, since `register_flat_namespace` can only borrow (`&[Module]`) and cannot move
/// ownership of `Item::Decl`, this function moves the actual `FunctionDecl`/`StructDecl`/
/// `EnumDecl` values it owns from `modules` into `program.functions`/`structs`/`enums` (any
/// second-or-later entry with the same name is ignored via `or_insert_with` -- since the
/// diagnostic for it was already reported in step (2), the skeleton's own content carries no
/// meaning beyond "the first definition wins").
pub fn build_program_skeleton(
    modules: Vec<Module>,
    sources: Arc<SourceMap>,
    diagnostics: &mut DiagnosticBag,
) -> Program {
    let mut program = Program::new(sources);
    crate::stdlib::prelude::install(&mut program);

    for module in &modules {
        if module.is_module_directive {
            module_grammar::check_module_toplevel_grammar(module, diagnostics);
        }
    }

    flat_namespace::register_flat_namespace(&modules, &mut program, diagnostics);

    for module in modules {
        for item in module.items {
            match item {
                Item::Decl(Decl::Function(f)) => {
                    program
                        .functions
                        .entry(f.name.clone())
                        .or_insert_with(|| Arc::new(f));
                }
                Item::Decl(Decl::Struct(s)) => {
                    program
                        .structs
                        .entry(s.name.clone())
                        .or_insert_with(|| Arc::new(s));
                }
                Item::Decl(Decl::Enum(e)) => {
                    program
                        .enums
                        .entry(e.name.clone())
                        .or_insert_with(|| Arc::new(e));
                }
                Item::Stmt(_) => {
                    // An entry file's executable statement (or an invalid module statement,
                    // for which E5002 was already reported). Do nothing, since the Program
                    // skeleton holds only declarations (§4.2, "only declarations registered").
                }
            }
        }
    }

    program
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::ErrorCode;
    use crate::lexer::Lexer;
    use std::fs;

    fn sample_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn file_names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| {
                p.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // discover_sibling_modules: verifies the 4 patterns from SAMPLES_PLAN.md §2 with real
    // files.
    // ------------------------------------------------------------------

    #[test]
    fn pattern1_shared_module_is_discovered_by_both_independent_entries() {
        // samples/ok/10a_module_shared_by_two_entries (pattern 1).
        let dir = sample_path("samples/ok/10a_module_shared_by_two_entries");
        let area_report = discover_sibling_modules(&dir.join("entry_area_report.ybm"));
        let shape_filter = discover_sibling_modules(&dir.join("entry_shape_filter.ybm"));
        assert_eq!(file_names(&area_report), vec!["mod_shapes.ybm".to_owned()]);
        assert_eq!(file_names(&shape_filter), vec!["mod_shapes.ybm".to_owned()]);
    }

    #[test]
    fn pattern2_independent_entries_do_not_discover_each_other() {
        // samples/ok/10b_independent_entries_same_directory (pattern 2): since there is no
        // file carrying a module directive, neither entry imports the other.
        let dir = sample_path("samples/ok/10b_independent_entries_same_directory");
        let alpha = discover_sibling_modules(&dir.join("entry_alpha.ybm"));
        let beta = discover_sibling_modules(&dir.join("entry_beta.ybm"));
        assert_eq!(alpha, Vec::<PathBuf>::new());
        assert_eq!(beta, Vec::<PathBuf>::new());
    }

    #[test]
    fn pattern3_shared_module_discovered_by_typecheck_only_and_runnable_entries() {
        // samples/ok/15_end_to_end_showcase (pattern 3): even with two entries that have
        // different effect profiles, the same shared module is auto-imported by both.
        let dir = sample_path("samples/ok/15_end_to_end_showcase");
        let typecheck_only =
            discover_sibling_modules(&dir.join("entry_showcase_typecheck_only.ybm"));
        let runnable = discover_sibling_modules(&dir.join("entry_showcase_runnable.ybm"));
        assert_eq!(file_names(&typecheck_only), vec!["mod_repo.ybm".to_owned()]);
        assert_eq!(file_names(&runnable), vec!["mod_repo.ybm".to_owned()]);
    }

    #[test]
    fn pattern4_broken_shared_module_is_discovered_by_both_cascading_entries() {
        // samples/err/static/10c_module_toplevel_statement_cascade (pattern 4): a broken
        // shared module gets auto-imported by both entries, becoming the basis on which both
        // fail in a cascading way.
        let dir = sample_path("samples/err/static/10c_module_toplevel_statement_cascade");
        let alpha = discover_sibling_modules(&dir.join("entry_alpha.ybm"));
        let beta = discover_sibling_modules(&dir.join("entry_beta.ybm"));
        assert_eq!(file_names(&alpha), vec!["mod_broken.ybm".to_owned()]);
        assert_eq!(file_names(&beta), vec!["mod_broken.ybm".to_owned()]);
    }

    #[test]
    fn discover_sorts_multiple_sibling_modules_deterministically() {
        // samples/ok/10c_module_constants_and_cross_reference: the two same-directory
        // modules mod_constants.ybm and mod_helpers.ybm are enumerated in a deterministic
        // (lexicographic) order.
        let dir = sample_path("samples/ok/10c_module_constants_and_cross_reference");
        let siblings = discover_sibling_modules(&dir.join("entry_main.ybm"));
        assert_eq!(
            file_names(&siblings),
            vec!["mod_constants.ybm".to_owned(), "mod_helpers.ybm".to_owned()]
        );
    }

    #[test]
    fn discover_finds_malformed_directive_module_for_inclusion() {
        // samples/err/static/10b_module_directive_malformed: a file with a syntactically
        // malformed `module foo` is still included among the candidates scanned for
        // auto-import (the import decision itself does not ask whether the syntax is
        // well-formed).
        let dir = sample_path("samples/err/static/10b_module_directive_malformed");
        let siblings = discover_sibling_modules(&dir.join("entry_probe.ybm"));
        assert_eq!(
            file_names(&siblings),
            vec!["mod_bad_directive.ybm".to_owned()]
        );
    }

    #[test]
    fn discover_returns_empty_when_entry_has_no_siblings() {
        // samples/err/static/10d_entry_self_module_directive: there is no other .ybm file in
        // the same directory.
        let dir = sample_path("samples/err/static/10d_entry_self_module_directive");
        let siblings = discover_sibling_modules(&dir.join("entry_with_module_directive.ybm"));
        assert_eq!(siblings, Vec::<PathBuf>::new());
    }

    #[test]
    fn discover_finds_collision_module_for_10a_err_case() {
        let dir = sample_path("samples/err/static/10a_module_name_collision");
        let siblings = discover_sibling_modules(&dir.join("entry_main.ybm"));
        assert_eq!(file_names(&siblings), vec!["mod_util.ybm".to_owned()]);
    }

    // ------------------------------------------------------------------
    // build_program_skeleton: verifies the entire module resolution phase after lexing and
    // parsing real files (an integration test that uses discover_sibling_modules's result
    // as is).
    // ------------------------------------------------------------------

    /// Lexes and parses `entry_path` and all of its same-directory modules, returning
    /// `(modules, lex_parse_diag, sources)` -- a simplified reproduction, for testing, of the
    /// actual Lex/Parse phase that driver.rs (Unit17) is responsible for (structured the same
    /// way as Unit4's `parser::mod.rs` tests).
    fn load_entry_and_siblings(entry_path: &Path) -> (Vec<Module>, Vec<String>, Arc<SourceMap>) {
        let mut sibling_paths = discover_sibling_modules(entry_path);
        let mut all_paths = vec![entry_path.to_path_buf()];
        all_paths.append(&mut sibling_paths);

        let mut sources = SourceMap::new();
        let mut modules = Vec::new();
        let mut diag_summaries = Vec::new();

        for path in all_paths {
            let text = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => panic!("failed to read {}: {e}", path.display()),
            };
            let file = sources.add(path.clone(), text);
            let src = sources.file(file).text().to_owned();
            let (tokens, _comments, lex_diag) = Lexer::new(&src, file).tokenize();
            let (module, parse_diag) = crate::parser::parse_module(&tokens, file);
            for d in lex_diag.into_vec().into_iter().chain(parse_diag.into_vec()) {
                diag_summaries.push(format!("{}", d.code));
            }
            modules.push(module);
        }

        (modules, diag_summaries, Arc::new(sources))
    }

    #[test]
    fn ok_10a_both_entries_build_zero_diagnostic_skeleton_sharing_mod_shapes() {
        let dir = sample_path("samples/ok/10a_module_shared_by_two_entries");
        for entry_name in ["entry_area_report.ybm", "entry_shape_filter.ybm"] {
            let (modules, lex_parse_diag, sources) = load_entry_and_siblings(&dir.join(entry_name));
            assert!(
                lex_parse_diag.is_empty(),
                "{entry_name}: {lex_parse_diag:?}"
            );
            let mut diagnostics = DiagnosticBag::new();
            let program = build_program_skeleton(modules, sources, &mut diagnostics);
            assert!(
                diagnostics.is_empty(),
                "{entry_name}: {:?}",
                diagnostics.into_vec()
            );
            assert!(program.structs.contains_key("NamedShape"));
            assert!(program.enums.contains_key("Shape"));
            let shape = &program.enums["Shape"];
            let variant_names: Vec<&str> = shape.variants.iter().map(|v| v.name.as_ref()).collect();
            assert_eq!(variant_names, vec!["Circle", "Rect"]);
            assert!(program.functions.contains_key("area"));
        }
    }

    #[test]
    fn ok_10b_independent_entries_each_build_zero_diagnostic_skeleton_without_collision() {
        let dir = sample_path("samples/ok/10b_independent_entries_same_directory");
        for entry_name in ["entry_alpha.ybm", "entry_beta.ybm"] {
            let (modules, lex_parse_diag, sources) = load_entry_and_siblings(&dir.join(entry_name));
            assert!(
                lex_parse_diag.is_empty(),
                "{entry_name}: {lex_parse_diag:?}"
            );
            let mut diagnostics = DiagnosticBag::new();
            let program = build_program_skeleton(modules, sources, &mut diagnostics);
            assert!(
                diagnostics.is_empty(),
                "{entry_name}: {:?}",
                diagnostics.into_vec()
            );
            assert!(program.functions.contains_key("helper"));
        }
    }

    #[test]
    fn ok_10c_module_constants_and_cross_reference_builds_expected_skeleton() {
        let dir = sample_path("samples/ok/10c_module_constants_and_cross_reference");
        let (modules, lex_parse_diag, sources) =
            load_entry_and_siblings(&dir.join("entry_main.ybm"));
        assert!(lex_parse_diag.is_empty(), "{lex_parse_diag:?}");
        let mut diagnostics = DiagnosticBag::new();
        let program = build_program_skeleton(modules, sources, &mut diagnostics);
        assert!(diagnostics.is_empty(), "{:?}", diagnostics.into_vec());

        assert_eq!(
            program.consts.get("max_retries"),
            Some(&crate::eval::value::Value::Int(3))
        );
        assert_eq!(
            program.consts.get("default_timeout_ms"),
            Some(&crate::eval::value::Value::Int(5000))
        );
        assert!(program.functions.contains_key("retries_exhausted"));
        assert!(program.functions.contains_key("greeting"));
        assert!(program.functions.contains_key("total_delay_ms"));
    }

    #[test]
    fn ok_pattern3_15_end_to_end_showcase_builds_zero_diagnostic_skeleton() {
        let dir = sample_path("samples/ok/15_end_to_end_showcase");
        for entry_name in [
            "entry_showcase_typecheck_only.ybm",
            "entry_showcase_runnable.ybm",
        ] {
            let (modules, lex_parse_diag, sources) = load_entry_and_siblings(&dir.join(entry_name));
            assert!(
                lex_parse_diag.is_empty(),
                "{entry_name}: {lex_parse_diag:?}"
            );
            let mut diagnostics = DiagnosticBag::new();
            let program = build_program_skeleton(modules, sources, &mut diagnostics);
            assert!(
                diagnostics.is_empty(),
                "{entry_name}: {:?}",
                diagnostics.into_vec()
            );
            assert!(program.structs.contains_key("Repo"));
            assert!(program.functions.contains_key("fetch_repos"));
        }
    }

    #[test]
    fn err_10a_module_name_collision_reports_e1001_only() {
        let dir = sample_path("samples/err/static/10a_module_name_collision");
        let (modules, lex_parse_diag, sources) =
            load_entry_and_siblings(&dir.join("entry_main.ybm"));
        assert!(lex_parse_diag.is_empty(), "{lex_parse_diag:?}");
        let mut diagnostics = DiagnosticBag::new();
        build_program_skeleton(modules, sources, &mut diagnostics);
        let diags = diagnostics.into_vec();
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, ErrorCode::DuplicateName);
    }

    #[test]
    fn err_10b_module_directive_malformed_reports_e5001_only() {
        // E5001 is reported directly by parser::parse_module (Unit4) at the point of lexing
        // and parsing, so the module_resolve phase (build_program_skeleton) should produce no
        // additional diagnostics -- verify that the total diagnostics is exactly the one
        // E5001 (checking this matches expected.toml's diagnostics = ["E5001"]).
        let dir = sample_path("samples/err/static/10b_module_directive_malformed");
        let (modules, lex_parse_diag, sources) =
            load_entry_and_siblings(&dir.join("entry_probe.ybm"));
        assert_eq!(
            lex_parse_diag,
            vec!["E5001".to_owned()],
            "{lex_parse_diag:?}"
        );
        let mut diagnostics = DiagnosticBag::new();
        build_program_skeleton(modules, sources, &mut diagnostics);
        assert!(
            diagnostics.is_empty(),
            "module_resolve itself should produce no additional diagnostics: {:?}",
            diagnostics.into_vec()
        );
    }

    #[test]
    fn err_10c_toplevel_statement_cascade_reports_e5002_for_each_entry() {
        let dir = sample_path("samples/err/static/10c_module_toplevel_statement_cascade");
        for entry_name in ["entry_alpha.ybm", "entry_beta.ybm"] {
            let (modules, lex_parse_diag, sources) = load_entry_and_siblings(&dir.join(entry_name));
            assert!(
                lex_parse_diag.is_empty(),
                "{entry_name}: {lex_parse_diag:?}"
            );
            let mut diagnostics = DiagnosticBag::new();
            build_program_skeleton(modules, sources, &mut diagnostics);
            let diags = diagnostics.into_vec();
            assert_eq!(diags.len(), 1, "{entry_name}: {diags:?}");
            assert_eq!(diags[0].code, ErrorCode::ModuleTopLevelStatement);
        }
    }

    #[test]
    fn err_10d_entry_self_module_directive_reports_e5002() {
        let dir = sample_path("samples/err/static/10d_entry_self_module_directive");
        let (modules, lex_parse_diag, sources) =
            load_entry_and_siblings(&dir.join("entry_with_module_directive.ybm"));
        assert!(lex_parse_diag.is_empty(), "{lex_parse_diag:?}");
        let mut diagnostics = DiagnosticBag::new();
        build_program_skeleton(modules, sources, &mut diagnostics);
        let diags = diagnostics.into_vec();
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, ErrorCode::ModuleTopLevelStatement);
    }
}
