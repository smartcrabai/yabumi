//! Shared construction helpers for the builtin Option[T]/Result[T,E]/Error values used across
//! stdlib (ARCHITECTURE.md §2.1), plus the `BuiltinFnId` type.

pub mod builtins;
pub mod codec;
pub mod collections;
pub mod envns;
pub mod fs;
pub mod http;
pub mod math;
pub mod prelude;
pub mod primitives;
pub mod proc;
pub mod rand;
pub mod regexns;
pub mod result_option;
pub mod time;
pub mod value_type;

use crate::eval::value::{EnumInstance, StructInstance, Value};
use crate::types::NamespaceId;
use std::sync::Arc;

/// An identifier for holding a stdlib function as a value (used when a namespace function is
/// treated as a closure, e.g. `x |> json.encode`, via `eval::value::CallTarget::Builtin`).
/// Uniquely determined by the pair of namespace (or, if absent, a primitive-type method or a
/// flat-namespace builtin like `print`) + function name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinFnId {
    pub namespace: Option<NamespaceId>,
    pub name: &'static str,
}

// ---------------------------------------------------------------------------
// Construction helpers for Option[T]/Result[T,E]/Error (shared by the stdlib modules:
// result_option.rs/value_type.rs/collections.rs/primitives.rs/math.rs/regexns.rs, the codec
// modules, and the fs/http/env/proc/time/rand effect modules. Since D-TYPE-09/D-TYPE-10
// represent Result/Option/Value as ordinary builtin enums (`eval::value::Value::Enum`),
// consolidating them here in one place, rather than having each file assemble its own
// `EnumInstance`, helps avoid mixing up variant numbers -- which must match D-TYPE-07's
// declaration order and `eval/call.rs`'s `builtin_variant_info` -- a decision made in this
// file).
// ---------------------------------------------------------------------------

/// `Some(v)` (Option, variant_index=0).
#[must_use]
pub(crate) fn some_value(v: Value) -> Value {
    Value::Enum(Arc::new(EnumInstance {
        type_name: Arc::from("Option"),
        variant_index: 0,
        variant_name: Arc::from("Some"),
        fields: vec![v],
    }))
}

/// `None` (Option, variant_index=1; the same shape as the `None` that `eval_ident` in
/// `eval/expr.rs` constructs).
#[must_use]
pub(crate) fn none_value() -> Value {
    Value::Enum(Arc::new(EnumInstance {
        type_name: Arc::from("Option"),
        variant_index: 1,
        variant_name: Arc::from("None"),
        fields: Vec::new(),
    }))
}

/// `Ok(v)` (Result, variant_index=0).
#[must_use]
pub(crate) fn ok_value(v: Value) -> Value {
    Value::Enum(Arc::new(EnumInstance {
        type_name: Arc::from("Result"),
        variant_index: 0,
        variant_name: Arc::from("Ok"),
        fields: vec![v],
    }))
}

/// `Err(e)` (Result, variant_index=1).
#[must_use]
pub(crate) fn err_value(v: Value) -> Value {
    Value::Enum(Arc::new(EnumInstance {
        type_name: Arc::from("Result"),
        variant_index: 1,
        variant_name: Arc::from("Err"),
        fields: vec![v],
    }))
}

/// `Error(kind: kind, message: message, cause: None)` (STDLIB.md §3.3, D-STDPOL-05: make even
/// the absence of a cause explicit). Shared by every place where a stdlib function returns a
/// failure as `Err(Error{..})`.
#[must_use]
pub(crate) fn error_value(kind: &str, message: impl Into<String>) -> Value {
    Value::Struct(Arc::new(StructInstance {
        type_name: Arc::from("Error"),
        fields: vec![
            Value::Str(Arc::from(kind)),
            Value::Str(Arc::from(message.into())),
            none_value(),
        ],
    }))
}

#[cfg(test)]
mod tests {
    use super::{err_value, error_value, none_value, ok_value, some_value};
    use crate::eval::value::Value;

    #[test]
    fn some_none_round_trip_variant_shape() {
        let Value::Enum(inst) = some_value(Value::Int(1)) else {
            panic!("some_value must build an Enum")
        };
        assert_eq!(inst.type_name.as_ref(), "Option");
        assert_eq!(inst.variant_name.as_ref(), "Some");
        assert_eq!(inst.variant_index, 0);
        assert_eq!(inst.fields, vec![Value::Int(1)]);

        let Value::Enum(inst) = none_value() else {
            panic!("none_value must build an Enum")
        };
        assert_eq!(inst.variant_name.as_ref(), "None");
        assert_eq!(inst.variant_index, 1);
        assert!(inst.fields.is_empty());
    }

    #[test]
    fn ok_err_round_trip_variant_shape() {
        let Value::Enum(inst) = ok_value(Value::Int(1)) else {
            panic!("ok_value must build an Enum")
        };
        assert_eq!(inst.type_name.as_ref(), "Result");
        assert_eq!(inst.variant_name.as_ref(), "Ok");
        assert_eq!(inst.variant_index, 0);

        let Value::Enum(inst) = err_value(Value::Int(2)) else {
            panic!("err_value must build an Enum")
        };
        assert_eq!(inst.variant_name.as_ref(), "Err");
        assert_eq!(inst.variant_index, 1);
    }

    #[test]
    fn error_value_has_kind_message_none_cause() {
        let Value::Struct(inst) = error_value("decode", "bad input".to_owned()) else {
            panic!("error_value must build a Struct")
        };
        assert_eq!(inst.type_name.as_ref(), "Error");
        assert_eq!(inst.fields[0], Value::Str(std::sync::Arc::from("decode")));
        assert_eq!(
            inst.fields[1],
            Value::Str(std::sync::Arc::from("bad input"))
        );
        let Value::Enum(cause) = &inst.fields[2] else {
            panic!("Error.cause must be an Option")
        };
        assert_eq!(cause.variant_name.as_ref(), "None");
    }

    /// Shared helper: `types/check_expr.rs` has its own stdlib signature tables
    /// (`str_method_sig`/`list_method_sig`/`dict_method_sig`/`set_method_sig`/
    /// `result_method_sig`/`option_method_sig`/`value_method_sig`, all non-`pub` private
    /// functions), which duplicates the signatures this file implements. Since those functions
    /// on the `types/check_expr.rs` side are non-`pub`, a test can't call them directly --
    /// instead, each of the `*_type_table_matches_implementation` tests below cross-checks a
    /// "list of method names from the type-check table" (transcribed by hand from reading those
    /// tables directly -- see the corresponding function in `src/types/check_expr.rs`) against
    /// a "list of method names this file provides" (the Rust functions implemented by
    /// `collections.rs`/`result_option.rs`/`value_type.rs`/`primitives.rs`, plus
    /// `par_map`/`par_each` (concurrency.rs), `shuffle` (rand.rs), and `count`/`is_empty`
    /// (known aliases that call.rs delegates to / inlines from an existing function of the same
    /// meaning, called out explicitly in each test's comment) -- to detect any difference
    /// (known limitation: full automation isn't possible unless the functions on the
    /// `check_expr.rs` side are made `pub(crate)`, so these tests are limited to comparing two
    /// hand-transcribed lists -- note the risk that a transcription mistake would let the test
    /// itself pass incorrectly).
    fn method_name_set(names: &[&'static str]) -> std::collections::BTreeSet<&'static str> {
        names.iter().copied().collect()
    }

    #[test]
    fn str_type_table_matches_implementation() {
        use std::collections::BTreeSet;
        // `str_method_sig` in `types/check_expr.rs` (transcribed as of 2026).
        let type_check_str = method_name_set(&[
            "len",
            "count",
            "get",
            "bytes",
            "trim",
            "trim_start",
            "trim_end",
            "to_upper",
            "to_lower",
            "to_str",
            "contains",
            "starts_with",
            "ends_with",
            "replace",
            "repeat",
            "is_empty",
            "find",
            "slice",
            "parse_int",
            "parse_float",
            "map",
            "filter",
            "fold",
            "find_by",
            "any",
            "all",
            "enumerate",
            "zip",
            "rev",
            "chars",
            "take",
            "skip",
            "flat_map",
            "sort_by",
            "split",
            "chain",
        ]);
        // The str-specific methods primitives.rs implements directly (the 15 iterator-family
        // methods don't appear here since, per STDLIB.md and the comment at the top of
        // primitives.rs, they're delegated to the generic list implementation via str_chars --
        // this verifies that the 21 remaining after subtracting those 15 iterator-family
        // methods from `str_method_sig`'s 36 entries matches what primitives.rs implements).
        let implemented_str_direct = method_name_set(&[
            "len",
            "count",
            "get",
            "bytes",
            "trim",
            "trim_start",
            "trim_end",
            "to_upper",
            "to_lower",
            "to_str",
            "contains",
            "starts_with",
            "ends_with",
            "replace",
            "repeat",
            "is_empty",
            "find",
            "slice",
            "parse_int",
            "parse_float",
            "split",
        ]);
        let str_delegated_to_list_generic = method_name_set(&[
            "map",
            "filter",
            "fold",
            "find_by",
            "any",
            "all",
            "enumerate",
            "zip",
            "rev",
            "chars",
            "take",
            "skip",
            "flat_map",
            "sort_by",
            "chain",
        ]);
        let implemented_str: BTreeSet<&str> = implemented_str_direct
            .union(&str_delegated_to_list_generic)
            .copied()
            .collect();
        assert_eq!(
            type_check_str, implemented_str,
            "str: type-check table vs implemented methods differ"
        );
    }

    #[test]
    fn list_type_table_matches_implementation() {
        use std::collections::BTreeSet;
        // `list_method_sig` in `types/check_expr.rs`.
        let type_check_list = method_name_set(&[
            "map",
            "par_map",
            "filter",
            "fold",
            "find",
            "any",
            "all",
            "count",
            "len",
            "sum",
            "enumerate",
            "zip",
            "rev",
            "take",
            "skip",
            "flat_map",
            "sort_by",
            "chain",
            "get",
            "is_empty",
            "contains",
            "first",
            "last",
            "join",
            "slice",
            "to_set",
            "each",
            "par_each",
            "push",
            "pop",
            "insert",
            "remove",
            "extend",
            "clear",
            "shuffle",
        ]);
        // The list methods collections.rs implements directly.
        let implemented_list_collections = method_name_set(&[
            "map",
            "filter",
            "fold",
            "find",
            "any",
            "all",
            "count",
            "len",
            "sum",
            "enumerate",
            "zip",
            "rev",
            "take",
            "skip",
            "flat_map",
            "sort_by",
            "chain",
            "get",
            "is_empty",
            "contains",
            "first",
            "last",
            "join",
            "slice",
            "to_set",
            "each",
            "push",
            "pop",
            "insert",
            "remove",
            "extend",
            "clear",
        ]);
        // Known aliases / implemented in another file (each confirmed against the actual
        // branches of list_method_readonly/list_mutate in `eval/call.rs`): par_map/par_each
        // have their real implementation in concurrency.rs (`crate::concurrency::eval_par_map`),
        // and shuffle in rand.rs (`crate::stdlib::rand::shuffle`) -- neither exists in
        // collections.rs.
        let list_implemented_elsewhere = method_name_set(&["par_map", "par_each", "shuffle"]);
        let implemented_list: BTreeSet<&str> = implemented_list_collections
            .union(&list_implemented_elsewhere)
            .copied()
            .collect();
        assert_eq!(
            type_check_list, implemented_list,
            "list: type-check table vs implemented methods differ"
        );
    }

    #[test]
    fn dict_type_table_matches_implementation() {
        use std::collections::BTreeSet;
        // `dict_method_sig` in `types/check_expr.rs`.
        let type_check_dict = method_name_set(&[
            "get",
            "contains_key",
            "keys",
            "values",
            "entries",
            "len",
            "is_empty",
            "map",
            "filter",
            "any",
            "all",
            "find",
            "fold",
            "each",
            "insert",
            "remove",
            "clear",
        ]);
        let implemented_dict_collections = method_name_set(&[
            "get",
            "contains_key",
            "keys",
            "values",
            "entries",
            "len",
            "map",
            "filter",
            "any",
            "all",
            "find",
            "fold",
            "each",
            "insert",
            "remove",
            "clear",
        ]);
        // `is_empty` is inline-implemented on the spot by dict_method_readonly in
        // `eval/call.rs` as `Value::Bool(m.is_empty())`, with no corresponding `dict_is_empty`
        // function in collections.rs (unlike list/set, this is the existing style on the
        // call.rs side, confirmed in this file).
        let dict_implemented_elsewhere = method_name_set(&["is_empty"]);
        let implemented_dict: BTreeSet<&str> = implemented_dict_collections
            .union(&dict_implemented_elsewhere)
            .copied()
            .collect();
        assert_eq!(
            type_check_dict, implemented_dict,
            "dict: type-check table vs implemented methods differ"
        );
    }

    #[test]
    fn set_type_table_matches_implementation() {
        use std::collections::BTreeSet;
        // `set_method_sig` in `types/check_expr.rs`.
        let type_check_set = method_name_set(&[
            "contains",
            "len",
            "count",
            "is_empty",
            "union",
            "intersection",
            "difference",
            "to_list",
            "map",
            "filter",
            "any",
            "all",
            "find",
            "fold",
            "sum",
            "each",
            "insert",
            "remove",
            "clear",
        ]);
        let implemented_set_collections = method_name_set(&[
            "contains",
            "len",
            "union",
            "intersection",
            "difference",
            "to_list",
            "map",
            "filter",
            "any",
            "all",
            "find",
            "fold",
            "sum",
            "each",
            "insert",
            "remove",
            "clear",
        ]);
        // `count` is aliased to len by set_method_readonly in `eval/call.rs` as
        // `"len" | "count" => col::set_len(s)`, and `is_empty` is inline-implemented like dict
        // (confirmed in this file).
        let set_implemented_elsewhere = method_name_set(&["count", "is_empty"]);
        let implemented_set: BTreeSet<&str> = implemented_set_collections
            .union(&set_implemented_elsewhere)
            .copied()
            .collect();
        assert_eq!(
            type_check_set, implemented_set,
            "set: type-check table vs implemented methods differ"
        );
    }

    #[test]
    fn result_type_table_matches_implementation() {
        // `result_method_sig` in `types/check_expr.rs`.
        let type_check_result = method_name_set(&[
            "is_ok",
            "is_err",
            "ok",
            "err",
            "unwrap",
            "unwrap_or",
            "unwrap_or_else",
            "map",
            "map_err",
            "and_then",
        ]);
        // The pub function names result_option.rs implements (stripping the `result_` prefix
        // matches STDLIB.md's method names; `is_ok`/`is_err` have no prefix).
        let implemented_result = method_name_set(&[
            "is_ok",
            "is_err",
            "ok",
            "err",
            "unwrap",
            "unwrap_or",
            "unwrap_or_else",
            "map",
            "map_err",
            "and_then",
        ]);
        assert_eq!(
            type_check_result, implemented_result,
            "Result: type-check table vs implemented methods differ"
        );
    }

    #[test]
    fn option_type_table_matches_implementation() {
        // `option_method_sig` in `types/check_expr.rs`.
        let type_check_option = method_name_set(&[
            "is_some",
            "is_none",
            "unwrap",
            "unwrap_or",
            "unwrap_or_else",
            "map",
            "and_then",
            "filter",
            "ok_or",
        ]);
        let implemented_option = method_name_set(&[
            "is_some",
            "is_none",
            "unwrap",
            "unwrap_or",
            "unwrap_or_else",
            "map",
            "and_then",
            "filter",
            "ok_or",
        ]);
        assert_eq!(
            type_check_option, implemented_option,
            "Option: type-check table vs implemented methods differ"
        );
    }

    #[test]
    fn value_type_table_matches_implementation() {
        // `value_method_sig` in `types/check_expr.rs`.
        let type_check_value = method_name_set(&[
            "as_int", "as_float", "as_str", "as_bool", "as_list", "as_dict", "is_null", "get", "at",
        ]);
        // value_type.rs's value_get/value_at correspond to STDLIB.md's get/at (the prefix
        // avoids a Rust-side name collision with dict_get etc.; the public method name itself
        // is get/at).
        let implemented_value = method_name_set(&[
            "as_int", "as_float", "as_str", "as_bool", "as_list", "as_dict", "is_null", "get", "at",
        ]);
        assert_eq!(
            type_check_value, implemented_value,
            "Value: type-check table vs implemented methods differ"
        );
    }
}

/// Verifies real files under samples/ok/**, samples/err/runtime/** through the full lex ->
/// parse -> module_resolve -> typecheck -> effectcheck -> lint -> eval pipeline
/// (ARCHITECTURE.md §4.1). `driver.rs::run_pipeline` is a CLI-facing entry point that returns
/// an `ExitCode` from a `Subcommand`, which doesn't suit this test's need to directly assert on
/// per-phase diagnostics, so the 6-phase call sequence is assembled directly here (following the
/// same pattern as `tests::parse_and_check` in `eval/mod.rs` and `tests::run_effect_check` in
/// `effects/mod.rs` -- in particular, the `ENTRY_POINT_NAME` convention (see the comment at the
/// top of `effects/mod.rs`: after TypeCheck completes and before EffectCheck/Lint run, driver.rs
/// registers a synthetic `FunctionDecl` whose `body` holds the entry's top-level executable
/// statements into `program.functions`) is a convention that lint's E4002 (unused function)
/// determination depends on, and omitting it would cause functions that call each other within
/// the entry, like `first`/`empty_list`, to be incorrectly flagged as "unused" -- a decision made
/// in this file). A byte-for-byte comparison of what `print` writes to the real process's stdout
/// is the responsibility of `tests/samples.rs` (the acceptance test harness); here we verify only
/// that "all 6 phases produce zero diagnostics" and "eval succeeds as expected, or if it panics,
/// the ErrorCode matches" (a decision made in this file -- since `tests/samples.rs` verifies the
/// byte-exact `stdout`/`stderr` and every field of `expected.toml`, this module deliberately
/// stays at a coarser diagnostic-level check and doesn't try to consolidate the duplication).
#[cfg(test)]
mod samples_pipeline_tests {
    use crate::ast::{Block, FunctionDecl, Item, NodeId, Stmt, TypeAnn, TypeAnnKind};
    use crate::diagnostics::{DiagnosticBag, ErrorCode, Position, SourceMap, Span};
    use crate::eval::env::{Environment, Program};
    use crate::eval::{Abort, run_top_level};
    use crate::lexer::Lexer;
    use crate::module_resolve::build_program_skeleton;
    use crate::parser::parse_module;
    use crate::types::check_decl::check_program;
    use crate::{effects, lint};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn read_sample(rel_path: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_path);
        match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => panic!("sample file read failed: {path:?}: {e}"),
        }
    }

    fn parse_ok(
        src: &str,
        sources: &mut SourceMap,
    ) -> (crate::ast::Module, crate::diagnostics::FileId) {
        let file = sources.add(PathBuf::from("entry_main.ybm"), src.to_owned());
        let (tokens, _comments, lex_diags) = Lexer::new(src, file).tokenize();
        assert!(!lex_diags.has_any(), "lex errors: {lex_diags:?}");
        let (module, parse_diags) = parse_module(&tokens, file);
        assert!(!parse_diags.has_any(), "parse errors: {parse_diags:?}");
        (module, file)
    }

    fn entry_stmts_of(module: crate::ast::Module) -> Vec<Stmt> {
        module
            .items
            .into_iter()
            .filter_map(|item| match item {
                Item::Stmt(s) => Some(s),
                Item::Decl(_) => None,
            })
            .collect()
    }

    fn dummy_span(file: crate::diagnostics::FileId) -> Span {
        Span {
            file,
            start: Position { line: 0, col: 0 },
            end: Position { line: 0, col: 0 },
        }
    }

    /// The 6 stages Lex -> Parse -> ModuleResolve -> TypeCheck -> EffectCheck -> Lint
    /// (ARCHITECTURE.md §4.1, excluding Eval). If any phase produces a diagnostic, `panic!`s
    /// with that phase's name and the diagnostic list included in the message (mirroring the
    /// real pipeline's actual stop rule: if the previous stage isn't clean, the next stage
    /// doesn't run). The source text is parsed 3 times (for building the skeleton, for
    /// check_program + the ENTRY_POINT_NAME synthetic function, and for eval's `items`) --
    /// since `ast` nodes don't implement `Clone` (see the comment at the top of
    /// `module_resolve/flat_namespace.rs`), a single parse result can't be reused across
    /// multiple ownership-taking destinations. The 3x parsing in test code is accepted since its
    /// runtime-speed cost is negligible (a decision made in this file).
    fn run_static_phases(src: &str) -> (Arc<Program>, Vec<Item>) {
        let mut sources = SourceMap::new();
        let (module_for_skeleton, _file) = parse_ok(src, &mut sources);
        let (module_for_check, entry_file) = parse_ok(src, &mut sources);
        let (module_for_eval, _file2) = parse_ok(src, &mut sources);

        let entry_stmts_for_check = entry_stmts_of(module_for_check);
        let entry_stmts_for_eval = entry_stmts_of(module_for_eval);

        let sources = Arc::new(sources);
        let mut resolve_diags = DiagnosticBag::new();
        let mut program = build_program_skeleton(
            vec![module_for_skeleton],
            Arc::clone(&sources),
            &mut resolve_diags,
        );
        assert!(
            !resolve_diags.has_any(),
            "module resolve errors: {:?}",
            resolve_diags.into_sorted(&sources)
        );

        let mut type_diags = DiagnosticBag::new();
        check_program(&mut program, &entry_stmts_for_check, &mut type_diags);
        assert!(
            !type_diags.has_any(),
            "type errors: {:?}",
            type_diags.into_sorted(&sources)
        );

        // The ENTRY_POINT_NAME convention (see the comment at the top of effects/mod.rs): since
        // EffectCheck/Lint assume this wiring, which driver.rs normally performs, it must be
        // reproduced here as well.
        let dummy = dummy_span(entry_file);
        let entry_decl = FunctionDecl {
            id: NodeId(u32::MAX),
            name: Arc::from(effects::ENTRY_POINT_NAME),
            generics: Vec::new(),
            self_param: None,
            params: Vec::new(),
            ret: TypeAnn {
                kind: TypeAnnKind::Void,
                span: dummy,
            },
            effects: Vec::new(),
            body: Block {
                stmts: entry_stmts_for_check,
                span: dummy,
            },
            leading_comments: Vec::new(),
            doc_comment: None,
            span: dummy,
        };
        program
            .functions
            .insert(Arc::from(effects::ENTRY_POINT_NAME), Arc::new(entry_decl));

        let mut effect_diags = DiagnosticBag::new();
        effects::check_program_effects(&mut program, &mut effect_diags);
        assert!(
            !effect_diags.has_any(),
            "effect errors: {:?}",
            effect_diags.into_sorted(&sources)
        );

        let mut lint_diags = DiagnosticBag::new();
        lint::check_all(&program, &mut lint_diags);
        assert!(
            !lint_diags.has_any(),
            "lint errors: {:?}",
            lint_diags.into_sorted(&sources)
        );

        let items: Vec<Item> = entry_stmts_for_eval.into_iter().map(Item::Stmt).collect();
        (Arc::new(program), items)
    }

    fn run_sample_ok(rel_path: &str) {
        let src = read_sample(rel_path);
        let (program, items) = run_static_phases(&src);
        let mut env = Environment::with_frame(HashMap::new());
        if let Err(Abort(diag)) = run_top_level(&items, &mut env, &program) {
            panic!("{rel_path}: unexpected abort {diag:?}");
        }
    }

    /// Only Lex -> Parse -> ModuleResolve -> TypeCheck (skipping EffectCheck/Lint). The
    /// panic-demonstration samples under `samples/err/runtime/**` (e6001/e6007 etc.) all share
    /// an intentional minimal-writing style, per STDLIB.md's correspondence table, of "assign
    /// into a variable meant for an out-of-range access and then never reference it again in a
    /// later statement before triggering the panic" (e.g. `oob_value = xs[idx]`, where that
    /// value is never used again) -- common to every e6001 through e6008 file. Strictly applying
    /// D-LINT-01 (unused variables; the scope is "local bindings", with no top-level-specific
    /// exemption, see the comment at the top of `lint/unused_variable.rs`) under the
    /// ENTRY_POINT_NAME convention would make every one of these `err/runtime` samples also
    /// report E4001 at the same time, and under the actual phase gating (ARCHITECTURE.md §4.1:
    /// "if the previous stage isn't clean, the next stage doesn't run"), execution would then
    /// stop at Lint and never reach Eval, making it impossible to reproduce the very E6xxx panic
    /// these samples are meant to verify (`samples/err/runtime/**/expected.toml` all expect only
    /// E6xxx in `diagnostics`, never including E4001). This inconsistency sits unreconciled
    /// between the samples side (whose "discard the value" assignment style may predate
    /// D-LINT-01 being established) and lint's specification, and cannot be resolved
    /// unilaterally from either this file or the lint side (`samples/**` cannot be modified, and
    /// changing lint's specification is outside this file's scope); so this follows the same
    /// reduced pipeline (skipping EffectCheck/Lint) as the existing `tests::sample_e6001_*` in
    /// `eval/mod.rs`, verifying only that "eval panics with the expected ErrorCode" (a known
    /// limitation).
    fn run_typecheck_only_phases(src: &str) -> (Arc<Program>, Vec<Item>) {
        let mut sources = SourceMap::new();
        let (module_for_skeleton, _file) = parse_ok(src, &mut sources);
        let (module_for_check, _entry_file) = parse_ok(src, &mut sources);
        let (module_for_eval, _file2) = parse_ok(src, &mut sources);

        let entry_stmts_for_check = entry_stmts_of(module_for_check);
        let entry_stmts_for_eval = entry_stmts_of(module_for_eval);

        let sources = Arc::new(sources);
        let mut resolve_diags = DiagnosticBag::new();
        let mut program = build_program_skeleton(
            vec![module_for_skeleton],
            Arc::clone(&sources),
            &mut resolve_diags,
        );
        assert!(
            !resolve_diags.has_any(),
            "module resolve errors: {:?}",
            resolve_diags.into_sorted(&sources)
        );

        let mut type_diags = DiagnosticBag::new();
        check_program(&mut program, &entry_stmts_for_check, &mut type_diags);
        assert!(
            !type_diags.has_any(),
            "type errors: {:?}",
            type_diags.into_sorted(&sources)
        );

        let items: Vec<Item> = entry_stmts_for_eval.into_iter().map(Item::Stmt).collect();
        (Arc::new(program), items)
    }

    fn run_sample_expect_abort(rel_path: &str, expected: ErrorCode) {
        let src = read_sample(rel_path);
        let (program, items) = run_typecheck_only_phases(&src);
        let mut env = Environment::with_frame(HashMap::new());
        match run_top_level(&items, &mut env, &program) {
            Ok(()) => panic!("{rel_path}: expected Abort({expected:?}) but evaluation succeeded"),
            Err(Abort(diag)) => assert_eq!(diag.code, expected, "{rel_path}: wrong error code"),
        }
    }

    #[test]
    fn ok_3_1_primitives() {
        run_sample_ok("samples/ok/3-1_primitives/entry_main.ybm");
        run_sample_ok("samples/ok/3-1_primitives/entry_conversion_roundtrip.ybm");
    }

    #[test]
    fn ok_3_2_collections() {
        run_sample_ok("samples/ok/3-2_collections/entry_literals.ybm");
        run_sample_ok("samples/ok/3-2_collections/entry_edge_cases.ybm");
    }

    #[test]
    fn ok_3_3_stdlib_types() {
        run_sample_ok("samples/ok/3-3_stdlib_types/entry_main.ybm");
        run_sample_ok("samples/ok/3-3_stdlib_types/entry_full_method_coverage_and_cause_chain.ybm");
    }

    #[test]
    fn ok_3_6_generics() {
        run_sample_ok("samples/ok/3-6_generics/entry_main.ybm");
        run_sample_ok("samples/ok/3-6_generics/entry_generic_struct_and_enum.ybm");
    }

    #[test]
    fn ok_6_2_iterators() {
        run_sample_ok("samples/ok/6-2_iterators/entry_main.ybm");
    }

    #[test]
    fn ok_6_4_strings() {
        run_sample_ok("samples/ok/6-4_strings/entry_main.ybm");
    }

    #[test]
    fn ok_7_4_safe_apis() {
        run_sample_ok("samples/ok/7-4_safe_apis/entry_main.ybm");
    }

    #[test]
    fn ok_11_2_math() {
        run_sample_ok("samples/ok/11-2_math/entry_main.ybm");
    }

    #[test]
    fn ok_11_2_regex() {
        run_sample_ok("samples/ok/11-2_regex/entry_main.ybm");
    }

    #[test]
    fn err_e6001_out_of_range_access() {
        run_sample_expect_abort(
            "samples/err/runtime/e6001_out_of_range_access/entry_list_index_oob.ybm",
            ErrorCode::IndexOutOfRange,
        );
        run_sample_expect_abort(
            "samples/err/runtime/e6001_out_of_range_access/entry_dict_missing_key.ybm",
            ErrorCode::IndexOutOfRange,
        );
        run_sample_expect_abort(
            "samples/err/runtime/e6001_out_of_range_access/entry_slice_out_of_range.ybm",
            ErrorCode::IndexOutOfRange,
        );
    }

    #[test]
    fn err_e6007_unwrap_failure() {
        run_sample_expect_abort(
            "samples/err/runtime/e6007_unwrap_failure/entry_result_unwrap_on_err.ybm",
            ErrorCode::UnwrapFailed,
        );
        run_sample_expect_abort(
            "samples/err/runtime/e6007_unwrap_failure/entry_option_unwrap_on_none.ybm",
            ErrorCode::UnwrapFailed,
        );
    }

    /// Confirms that an out-of-range conversion in `int(x: float)`
    /// (`primitives::int_from_float`) aborts immediately with E6003. The existing test in
    /// `eval/mod.rs` only covers `entry_arithmetic_overflow.ybm` (arithmetic overflow, ops.rs)
    /// in the same directory; `entry_float_to_int_overflow.ybm` (the `int()` conversion) wasn't
    /// yet covered by any existing test, so it's added here.
    #[test]
    fn err_e6003_float_to_int_overflow() {
        run_sample_expect_abort(
            "samples/err/runtime/e6003_integer_overflow/entry_float_to_int_overflow.ybm",
            ErrorCode::IntegerOverflow,
        );
    }
}
