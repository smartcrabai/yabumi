//! print/eprint/assert (STDLIB.md §13, ARCHITECTURE.md §2.1). No effect -- overload special
//! case (D-STDPOL-01) limited to the 4 primitive types (str/int/float/bool). struct/enum are
//! never implicitly stringified (D-STDPOL-02). Helpers for constructing `Result`/`Option`/
//! `Error` values from the Rust side live in `stdlib/mod.rs` (`ok_value`/`err_value`/
//! `some_value`/`none_value`/`error_value`).

use crate::diagnostics::Span;
use crate::eval::Abort;
use crate::eval::value::Value;
/// `print(value: str|int|float|bool): void`. Writes to stdout, emitting one trailing newline
/// per value.
pub fn print(value: &Value) {
    println!("{}", display_primitive(value));
}

/// `eprint(value: str|int|float|bool): void`. Writes to stderr.
pub fn eprint(value: &Value) {
    eprintln!("{}", display_primitive(value));
}

/// Thanks to the 4-type overload restriction of D-STDPOL-01, `value` has already been
/// type-checked and is always one of str/int/float/bool (struct/enum are never implicitly
/// stringified, D-STDPOL-02). Float display follows the same convention as `str(x: float)` in
/// D-TYPE-14 (shortest round-trip representation, always including a decimal point) -- since
/// Rust's `f64` Display omits the decimal point for integral values (`1.0` becomes `"1"`), we
/// append `.0` only when the string contains neither a decimal point nor exponent notation.
fn display_primitive(value: &Value) -> String {
    match value {
        Value::Str(s) => s.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(x) => format_float(*x),
        Value::Bool(b) => b.to_string(),
        _ => {
            unreachable!(
                "type-checked already, so print/eprint arguments are always str/int/float/bool (D-STDPOL-01)"
            )
        }
    }
}

fn format_float(x: f64) -> String {
    let s = x.to_string();
    let already_has_point_or_exponent = s.contains('.') || s.contains('e') || s.contains('E');
    let is_plain_integer_digits = s.chars().all(|c| c.is_ascii_digit() || c == '-');
    if !already_has_point_or_exponent && is_plain_integer_digits {
        format!("{s}.0")
    } else {
        s
    }
}

/// `assert(cond: bool): void`. Exits 1 on failure. The message automatically shows the source
/// text of the condition expression (`source_text` is passed by the Driver via
/// `SourceMap::slice`).
pub fn assert_bare(cond: bool, source_text: &str, span: Span) -> Result<Value, Abort> {
    if cond {
        Ok(Value::Void)
    } else {
        Err(crate::eval::panic::assert_failed(span, source_text))
    }
}

/// `assert(cond: bool, msg: str): void`. Exits 1 on failure, showing msg.
pub fn assert_with_message(cond: bool, msg: &str, span: Span) -> Result<Value, Abort> {
    if cond {
        Ok(Value::Void)
    } else {
        Err(crate::eval::panic::assert_failed(span, msg))
    }
}

/// A test-only harness shared by fs.rs/http.rs/proc.rs/time.rs/rand.rs/builtins.rs for running
/// a real file under samples/ok/ through the full lex -> parse -> module_resolve -> typecheck ->
/// effects check -> eval pipeline. It lives under `#[cfg(test)]` but is `pub(crate)` so each
/// file's own `mod tests` can reference it as
/// `crate::stdlib::builtins::test_pipeline::run_ok_sample(..)` (a decision made in this file, so
/// that test-only code shared across stdlib files isn't duplicated per file). `driver::
/// run_pipeline` is a CLI-facing entry point that returns an `ExitCode` from a `Subcommand`,
/// which doesn't suit this test's need to directly assert on the `Result<(), Abort>` or
/// individual diagnostics mid-phase; so each phase is wired up manually here, following the same
/// steps as the existing test helper `parse_and_check` in `eval/mod.rs`.
#[cfg(test)]
pub(crate) mod test_pipeline {
    use crate::ast::{Item, Stmt};
    use crate::diagnostics::{DiagnosticBag, SourceMap};
    use crate::eval::env::Environment;
    use crate::eval::{Abort, run_top_level};
    use crate::lexer::Lexer;
    use crate::module_resolve::build_program_skeleton;
    use crate::parser::parse_module;
    use crate::types::check_decl::check_program;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn sample_entry_path(rel_dir: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("ok")
            .join(rel_dir)
            .join("entry_main.ybm")
    }

    /// Runs `samples/ok/<rel_dir>/entry_main.ybm` through all 6 phases (Lex/Parse/
    /// ModuleResolve/TypeCheck/EffectCheck/Eval). A thin wrapper that just passes the file's
    /// contents to `run_ok_source` (below).
    pub(crate) fn run_ok_sample(rel_dir: &str) -> Result<(), Abort> {
        let path = sample_entry_path(rel_dir);
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => panic!("sample file read failed: {path:?}: {e}"),
        };
        run_ok_source(rel_dir, &path, &src)
    }

    /// Runs `src` (the full source text of a single Yabumi file) through all 6 phases (Lex/
    /// Parse/ModuleResolve/TypeCheck/EffectCheck/Eval). `label` is a string included in
    /// diagnostic messages for identification purposes (the sample directory name, or any name
    /// chosen by the caller); `path` is a dummy file path registered with `SourceMap`. If any of
    /// lex/parse/module_resolve/typecheck/effect check produces even one diagnostic, the test
    /// itself fails via `panic!` (`.unwrap()`/`.expect()` are unused because clippy denies them,
    /// the same policy as the existing test helper in `eval/mod.rs`). The eval result
    /// (`Ok(())` = all `assert`s succeeded, `Err(Abort)` = a panic or `assert` failure) is
    /// returned as-is to the caller for it to judge. This is made `pub(crate)` so that, besides
    /// being the shared implementation behind `run_ok_sample`, it can also be reused -- without
    /// having to add a sample under `samples/` (since `samples/**` outside one's assigned files
    /// cannot be modified) -- by other units' tests (e.g. `stdlib::collections`) that want to
    /// verify wiring such as dict/set higher-order methods through the full pipeline.
    pub(crate) fn run_ok_source(
        label: &str,
        path: &std::path::Path,
        src: &str,
    ) -> Result<(), Abort> {
        let mut sources = SourceMap::new();
        let file = sources.add(path.to_path_buf(), src.to_owned());
        let (tokens, _comments, lex_diags) = Lexer::new(src, file).tokenize();
        assert!(
            !lex_diags.has_any(),
            "lex errors in {label}: {:?}",
            lex_diags.into_sorted(&sources)
        );

        let (mut module, parse_diags) = parse_module(&tokens, file);
        assert!(
            !parse_diags.has_any(),
            "parse errors in {label}: {:?}",
            parse_diags.into_sorted(&sources)
        );

        // build_program_skeleton doesn't register Item::Stmt and discards it (see
        // module_resolve/mod.rs), so the entry's executable statements must be set aside first
        // (the same procedure as the existing test helper in eval/mod.rs).
        let all_items = std::mem::take(&mut module.items);
        let mut entry_stmts: Vec<Stmt> = Vec::new();
        let mut decl_items = Vec::new();
        for item in all_items {
            match item {
                Item::Stmt(s) => entry_stmts.push(s),
                decl @ Item::Decl(_) => decl_items.push(decl),
            }
        }
        module.items = decl_items;

        let sources = Arc::new(sources);
        let mut resolve_diags = DiagnosticBag::new();
        let mut program =
            build_program_skeleton(vec![module], Arc::clone(&sources), &mut resolve_diags);
        assert!(
            !resolve_diags.has_any(),
            "module resolve errors in {label}: {:?}",
            resolve_diags.into_sorted(&sources)
        );

        // Register the builtin enum/struct/conversion functions (Result/Option/Value/Error/
        // Response/HttpOptions/ProcOutput/int/float/str/print/eprint/assert/set) after the
        // user declarations. `prelude::install` only calls `.insert()`, so it's safe as long as
        // it doesn't collide with user declarations (the same call order that `driver.rs`'s
        // `run_pipeline` takes; this is test-only wiring -- a decision made in this file).
        crate::stdlib::prelude::install(&mut program);

        let mut type_diags = DiagnosticBag::new();
        check_program(&mut program, &entry_stmts, &mut type_diags);
        assert!(
            !type_diags.has_any(),
            "type errors in {label}: {:?}",
            type_diags.into_sorted(&sources)
        );

        let mut effect_diags = DiagnosticBag::new();
        crate::effects::check_program_effects(&mut program, &mut effect_diags);
        assert!(
            !effect_diags.has_any(),
            "effect errors in {label}: {:?}",
            effect_diags.into_sorted(&sources)
        );

        let items: Vec<Item> = entry_stmts.into_iter().map(Item::Stmt).collect();
        let program = Arc::new(program);
        let mut env = Environment::with_frame(HashMap::new());
        run_top_level(&items, &mut env, &program)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{FileId, Position};

    fn test_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    #[test]
    fn format_float_appends_point_zero_for_whole_numbers() {
        assert_eq!(format_float(3.0), "3.0");
        assert_eq!(format_float(-3.0), "-3.0");
    }

    #[test]
    fn format_float_keeps_existing_fraction() {
        assert_eq!(format_float(3.5), "3.5");
        assert_eq!(format_float(1.5), "1.5");
    }

    #[test]
    fn assert_bare_ok_when_true() {
        let span = test_span();
        let result = assert_bare(true, "x == 1", span);
        assert!(matches!(result, Ok(Value::Void)));
    }

    #[test]
    fn assert_bare_aborts_with_source_text_when_false() {
        let span = test_span();
        match assert_bare(false, "x == 1", span) {
            Err(abort) => {
                assert_eq!(abort.0.code, crate::diagnostics::ErrorCode::AssertFailed);
                assert!(abort.0.message.contains("x == 1"));
            }
            Ok(_) => panic!("a false condition should produce an Abort"),
        }
    }

    #[test]
    fn assert_with_message_aborts_with_custom_message() {
        let span = test_span();
        match assert_with_message(false, "custom message", span) {
            Err(abort) => {
                assert_eq!(abort.0.code, crate::diagnostics::ErrorCode::AssertFailed);
                assert!(abort.0.message.contains("custom message"));
            }
            Ok(_) => panic!("a false condition should produce an Abort"),
        }
    }

    /// SPEC §11.3 / STDLIB.md §13: verifies the 4-type overload of `print`/`eprint` through the
    /// full pipeline. The exact byte-for-byte stdout/stderr match required by
    /// `samples/ok/11-3_builtins_print_eprint_assert/expected.toml` needs to capture the
    /// standard output of a separate child process (since `print`/`eprint` call the real
    /// `println!`/`eprintln!` directly, they can't be intercepted from a test in the same
    /// process), so that responsibility belongs to `tests/samples.rs` (the acceptance test
    /// harness). Here we verify what can be substituted with an in-process run, i.e. that "lex/
    /// parse/module_resolve/typecheck/effect check all pass with zero diagnostics, and all 4
    /// type calls to print/eprint complete without panicking or aborting".
    #[test]
    fn sample_builtins_print_eprint_assert_runs_end_to_end() {
        let result = test_pipeline::run_ok_sample("11-3_builtins_print_eprint_assert");
        assert!(
            result.is_ok(),
            "sample should run without Abort: {result:?}"
        );
    }
}
