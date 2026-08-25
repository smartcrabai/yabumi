//! Chains lex→parse→module_resolve→check→effects→lint→(fmt|eval|doctest) and decides the exit
//! code (ARCHITECTURE.md §4).
//!
//! `ybm <file>` (Run) **does not include lint**, per the table in SPEC §1 (it runs only after
//! type checking succeeds). The diagram in ARCHITECTURE.md §4.1 depicts the 6-phase pipeline
//! shared by all 3 subcommands as including EffectCheck/Lint, but all 14 cases under
//! `samples/err/runtime/` use `cmd = "run"` and involve an unused variable (e.g. `oob_value` in
//! `entry_list_index_oob.ybm`); if Lint were also run, E4001 would be reported before the
//! E6xxx the case is actually meant to exercise, which is a contradiction. This file treats the
//! table in SPEC §1 (`ybm <file>` is type-checking only) as canonical and limits Run's static
//! phases to the 4 stages Lex→Parse→ModuleResolve→TypeCheck (judgment call made in this file).
//! `check`/`test` are, in fact, the only commands where `samples/err/lint/*` and
//! `samples/err/static/8_effect_errors` verify EffectCheck/Lint — and only via `cmd = "check"` —
//! so these two commands run the full 6-stage set.

use crate::ast::{Block, DocFence, FunctionDecl, Item, NodeId, Stmt, TypeAnn, TypeAnnKind};
use crate::cli::Subcommand;
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, FileId, Position, SourceMap, Span};
use crate::doctest;
use crate::effects;
use crate::eval;
use crate::eval::Abort;
use crate::eval::env::{Environment, Program};
use crate::fmt;
use crate::lexer::{FStringPart, Lexer, Token, TokenKind};
use crate::lint;
use crate::module_resolve;
use crate::parser::comment_attach::attach_comments;
use crate::parser::{parse_module, parse_module_with_offset};
use crate::stdlib;
use crate::types::Resolutions;
use crate::types::check_decl::{check_program, check_top_level_stmts};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

/// The entire §4.1-4.4 pipeline. Running it on a dedicated-stack-size thread is §5.7/main.rs's
/// responsibility.
///
/// The internal implementation (`run_pipeline_impl`) returns a `bool` (true = exit 0) —
/// `std::process::ExitCode` is deliberately an opaque type with no stable API to read the
/// number back out, so to let the in-process tests (the `tests` module) verify the result, the
/// real logic converts to `bool` here before wrapping it in `ExitCode` (judgment call made in
/// this file).
#[must_use]
pub fn run_pipeline(subcommand: &Subcommand) -> ExitCode {
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    if run_pipeline_impl(subcommand, &mut stdout, &mut stderr) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_pipeline_impl(subcommand: &Subcommand, out: &mut dyn Write, err: &mut dyn Write) -> bool {
    match subcommand {
        Subcommand::Run { file } => run_command(file, err),
        Subcommand::Check { file, apply_fmt } => check_command(file, *apply_fmt, out, err),
        Subcommand::Test { file } => test_command(file, out, err),
    }
}

/// The front end shared by all 3 subcommands (ARCHITECTURE.md §4.1/§4.2): runs
/// Lex→Parse→ModuleResolve→TypeCheck and assembles the `Program` skeleton, the entry's
/// top-level executable statements, and (when requested) doc fences.
struct FrontEnd {
    program: Program,
    sources: Arc<SourceMap>,
    /// The list of the entry file plus its sibling-directory modules. `[0]` is always the entry.
    files: Vec<(PathBuf, FileId)>,
    entry_stmts: Vec<Stmt>,
    /// Empty when `need_doc_fences` is false.
    fences: Vec<DocFence>,
    /// The set of names `stdlib::prelude::install` newly added to `program.functions` (removed
    /// only by `run_effect_and_lint`, right before Lint — see the comment near the top of this
    /// file).
    prelude_function_names: std::collections::HashSet<Arc<str>>,
}

fn run_command(file: &Path, err: &mut dyn Write) -> bool {
    let Ok(front) = run_front_end(file, false, err) else {
        return false;
    };
    let FrontEnd {
        mut program,
        sources,
        files,
        entry_stmts,
        ..
    } = front;
    register_entry_point(&mut program, front_entry_file(&files), entry_stmts);
    if !run_effect_check(&mut program, &sources, err) {
        return false;
    }

    let items = take_registered_entry_items(&mut program);
    let program = Arc::new(program);
    let mut env = Environment::with_frame(HashMap::new());
    match eval::run_top_level(&items, &mut env, &program) {
        Ok(()) => true,
        Err(Abort(diag)) => {
            let _ = writeln!(err, "{}", diag.render(&sources));
            false
        }
    }
}

fn check_command(
    file: &Path,
    apply_formatting: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    let Ok(front) = run_front_end(file, true, err) else {
        return false;
    };
    let FrontEnd {
        mut program,
        sources,
        files,
        entry_stmts,
        fences,
        prelude_function_names,
    } = front;
    let entry_file = front_entry_file(&files);
    register_entry_point(&mut program, entry_file, entry_stmts);

    if !run_effect_and_lint(&mut program, &sources, &prelude_function_names, err) {
        return false;
    }

    let mut fence_bag = DiagnosticBag::new();
    typecheck_fences_only(&fences, &program, &mut fence_bag);
    if fence_bag.has_any() {
        report_diagnostics(err, fence_bag, &sources);
        return false;
    }

    apply_fmt(&files, &sources, apply_formatting, out, err)
}

fn test_command(file: &Path, out: &mut dyn Write, err: &mut dyn Write) -> bool {
    let Ok(front) = run_front_end(file, true, err) else {
        return false;
    };
    let FrontEnd {
        mut program,
        sources,
        files,
        entry_stmts,
        fences,
        prelude_function_names,
    } = front;
    let entry_file = front_entry_file(&files);
    register_entry_point(&mut program, entry_file, entry_stmts);

    if !run_effect_and_lint(&mut program, &sources, &prelude_function_names, err) {
        return false;
    }

    let results = doctest::run_all_fences(&fences, &program);
    let mut fail_count = 0usize;
    for result in &results {
        if let doctest::Outcome::Fail(diag) = &result.outcome {
            fail_count += 1;
            let _ = writeln!(err, "{}", diag.render(&sources));
        }
    }
    let pass_count = results.len() - fail_count;
    let _ = writeln!(out, "doctest: {pass_count} passed, {fail_count} failed");
    fail_count == 0
}

fn front_entry_file(files: &[(PathBuf, FileId)]) -> FileId {
    files[0].1
}

/// EffectCheck→Lint (shared by `check`/`test`; `run` does not execute this — see the judgment
/// call at the top of this file). If either produces diagnostics, reports them to stderr and
/// returns `false`.
fn run_effect_check(program: &mut Program, sources: &SourceMap, err: &mut dyn Write) -> bool {
    let mut diagnostics = DiagnosticBag::new();
    effects::check_program_effects(program, &mut diagnostics);
    if diagnostics.has_any() {
        report_diagnostics(err, diagnostics, sources);
        false
    } else {
        true
    }
}

fn run_effect_and_lint(
    program: &mut Program,
    sources: &SourceMap,
    prelude_function_names: &std::collections::HashSet<Arc<str>>,
    err: &mut dyn Write,
) -> bool {
    if !run_effect_check(program, sources, err) {
        return false;
    }

    // Lint (`src/lint/**`, outside this scope) unconditionally scans `program.functions` and
    // mistakes the built-in function placeholders (int/float/str/print/eprint/assert/set)
    // registered by `stdlib::prelude` for "declarations belonging to the entry file itself",
    // falsely reporting E4001/E4002 (see the comment near the `prelude::install` call in
    // `run_front_end`). They were kept around this far because TypeCheck/EffectCheck needed
    // them to resolve callees for pipes like `x |> str`, but they are no longer needed — and
    // are actively harmful — for Lint, so they are removed right before it.
    program
        .functions
        .retain(|name, _| !prelude_function_names.contains(name));

    let mut lint_bag = DiagnosticBag::new();
    lint::check_all(program, &mut lint_bag);
    if lint_bag.has_any() {
        report_diagnostics(err, lint_bag, sources);
        return false;
    }
    true
}

/// D-CLI-04: extension and existence check. Returns the file contents on success.
fn check_entry_path_and_read(entry_path: &Path, err: &mut dyn Write) -> Result<String, ()> {
    if entry_path.extension().and_then(|e| e.to_str()) != Some("ybm") {
        report_cli_io_error(
            err,
            entry_path,
            ErrorCode::InvalidExtension,
            "the extension must be .ybm",
        );
        return Err(());
    }
    read_source_file(entry_path, err)
}

fn read_source_file(path: &Path, err: &mut dyn Write) -> Result<String, ()> {
    fs::read_to_string(path).map_err(|error| {
        let (code, message) = if error.kind() == std::io::ErrorKind::NotFound {
            (ErrorCode::FileNotFound, "file not found".to_owned())
        } else {
            (
                ErrorCode::FileReadFailure,
                format!("cannot read file: {error}"),
            )
        };
        report_cli_io_error(err, path, code, &message);
    })
}

fn report_cli_io_error(err: &mut dyn Write, path: &Path, code: ErrorCode, message: &str) {
    let _ = writeln!(err, "{}:1:1 [{code}] {message}", path.display());
}

fn report_diagnostics(err: &mut dyn Write, bag: DiagnosticBag, sources: &SourceMap) {
    for d in bag.into_sorted(sources) {
        let _ = writeln!(err, "{}", d.render(sources));
    }
}

/// Loads the entry file plus its sibling-directory modules and runs
/// Lex→Parse→ModuleResolve→TypeCheck (ARCHITECTURE.md §4.1/§4.2). Between phases, the next
/// phase does not run unless the previous one produced zero diagnostics (D-CLI-03,
/// ARCHITECTURE.md §4.1).
///
/// `need_doc_fences` is true only for `check`/`test` — `run` never touches doc fences at all
/// (the table in SPEC §1 makes no mention of doc tests for it, ARCHITECTURE.md §4.3).
fn run_front_end(
    entry_path: &Path,
    need_doc_fences: bool,
    err: &mut dyn Write,
) -> Result<FrontEnd, ()> {
    let entry_text = check_entry_path_and_read(entry_path, err)?;

    // `Path::parent()` returns `Some("")` rather than `None` for a bare filename with no
    // directory component (e.g. `ybm entry.ybm`, an entirely normal way to invoke it — just an
    // extension-bearing filename relative to the current directory). `discover_sibling_modules`
    // (module_resolve/mod.rs, outside this scope) passes this `parent()` result straight to
    // `std::fs::read_dir`, so reading an empty-string path fails and no sibling modules are
    // found at all (reported as needing adjustment). Making the path absolute with
    // `canonicalize` before passing it in ensures `parent()` always has a proper directory
    // component, so this call form makes `discover_sibling_modules` reliably work from within
    // driver.rs too. Since `check_entry_path_and_read` has already confirmed the read
    // succeeded, `canonicalize` can only fail on a rare TOCTOU race — in that case we fall back
    // to the original path (this only leaves us with the pre-existing "no sibling found"
    // behavior, causing no new harm).
    let discovery_path = fs::canonicalize(entry_path).unwrap_or_else(|_| entry_path.to_path_buf());
    let mut sibling_paths = module_resolve::discover_sibling_modules(&discovery_path);
    let mut paths = vec![entry_path.to_path_buf()];
    paths.append(&mut sibling_paths);

    let mut sources = SourceMap::new();
    let mut file_ids = Vec::with_capacity(paths.len());
    file_ids.push(sources.add(paths[0].clone(), entry_text));
    for path in &paths[1..] {
        let text = read_source_file(path, err)?;
        file_ids.push(sources.add(path.clone(), text));
    }

    let Some((tokens_per_file, comments_per_file)) = lex_all_files(&file_ids, &sources, err) else {
        return Err(());
    };

    let Some(mut modules) =
        parse_all_files(&file_ids, tokens_per_file, comments_per_file, err, &sources)
    else {
        return Err(());
    };

    let fences = if need_doc_fences {
        doctest::collect_fences(&modules)
    } else {
        Vec::new()
    };
    let pseudo_consts = if need_doc_fences {
        doctest::collect_doctest_pseudo_consts(&modules)
    } else {
        Vec::new()
    };

    let entry_stmts = take_entry_top_level_stmts(&mut modules[0]);

    let sources = Arc::new(sources);
    let mut resolve_bag = DiagnosticBag::new();
    let mut program =
        module_resolve::build_program_skeleton(modules, Arc::clone(&sources), &mut resolve_bag);
    if resolve_bag.has_any() {
        report_diagnostics(err, resolve_bag, &sources);
        return Err(());
    }

    let prelude_function_names: std::collections::HashSet<Arc<str>> =
        stdlib::prelude::BUILTIN_FUNCTION_NAMES
            .iter()
            .map(|name| Arc::from(*name))
            .collect();

    for (name, value) in pseudo_consts {
        program.consts.entry(name).or_insert(value);
    }

    let mut type_bag = DiagnosticBag::new();
    check_program(&mut program, &entry_stmts, &mut type_bag);
    if type_bag.has_any() {
        report_diagnostics(err, type_bag, &sources);
        return Err(());
    }

    let files: Vec<(PathBuf, FileId)> = paths.into_iter().zip(file_ids).collect();

    Ok(FrontEnd {
        program,
        sources,
        files,
        entry_stmts,
        fences,
        prelude_function_names,
    })
}

type LexedFile = (Vec<Token>, crate::lexer::comments::CommentStream);

/// Lex phase: tokenizes every file, and if there is even a single diagnostic, reports it and
/// returns `None` (D-CLI-03: gating between phases).
fn lex_all_files(
    file_ids: &[FileId],
    sources: &SourceMap,
    err: &mut dyn Write,
) -> Option<(Vec<Vec<Token>>, Vec<crate::lexer::comments::CommentStream>)> {
    let mut lex_bag = DiagnosticBag::new();
    let mut tokens_per_file = Vec::with_capacity(file_ids.len());
    let mut comments_per_file = Vec::with_capacity(file_ids.len());
    for &fid in file_ids {
        let text = sources.file(fid).text();
        let (tokens, comments, diag) = Lexer::new(text, fid).tokenize();
        for d in diag.into_vec() {
            lex_bag.push(d);
        }
        tokens_per_file.push(tokens);
        comments_per_file.push(comments);
    }
    if lex_bag.has_any() {
        report_diagnostics(err, lex_bag, sources);
        return None;
    }
    Some((tokens_per_file, comments_per_file))
}

/// Parse phase: reached only when Lex produced zero diagnostics across all files.
fn parse_all_files(
    file_ids: &[FileId],
    tokens_per_file: Vec<Vec<Token>>,
    comments_per_file: Vec<crate::lexer::comments::CommentStream>,
    err: &mut dyn Write,
    sources: &SourceMap,
) -> Option<Vec<crate::ast::Module>> {
    let mut parse_bag = DiagnosticBag::new();
    let mut modules = Vec::with_capacity(file_ids.len());
    let mut next_node_id = 0;
    for ((&fid, tokens), comments) in file_ids.iter().zip(tokens_per_file).zip(comments_per_file) {
        let (mut module, diag, next_id) = parse_module_with_offset(&tokens, fid, next_node_id);
        next_node_id = next_id;
        for d in diag.into_vec() {
            parse_bag.push(d);
        }
        attach_comments(&mut module, comments);
        modules.push(module);
    }
    if parse_bag.has_any() {
        report_diagnostics(err, parse_bag, sources);
        return None;
    }
    Some(modules)
}

/// Extracts the entry file's `Item::Stmt` (top-level executable statements).
/// `build_program_skeleton` (`module_resolve/mod.rs`, outside this scope) discards `Item::Stmt`
/// entirely rather than keeping any of it in the `Program` skeleton, so it must be saved off
fn take_entry_top_level_stmts(entry_module: &mut crate::ast::Module) -> Vec<Stmt> {
    // D-MOD-01: when the entry file's effective first line is a module directive (an edge case,
    // samples/err/static/10d_entry_self_module_directive), D-MOD-02 disallows any top-level
    // executable statement at all — `Item::Stmt` must not be taken as something to execute
    // here, because `build_program_skeleton` (module_resolve/mod.rs) needs to see
    // `module.is_module_directive` to report E5002 (taking it away would erase the evidence and
    // E5002 would never be reported). In this case it is correct for `entry_stmts` (the
    // top-level statements to execute) to be empty — since only statements that aren't allowed
    // to execute are present in the first place, having nothing to execute is the right
    // outcome.
    if entry_module.is_module_directive {
        return Vec::new();
    }
    let items = std::mem::take(&mut entry_module.items);
    let mut stmts = Vec::new();
    let mut decls = Vec::new();
    for item in items {
        match item {
            Item::Stmt(s) => stmts.push(s),
            decl @ Item::Decl(_) => decls.push(decl),
        }
    }
    entry_module.items = decls;
    stmts
}

/// The `crate::effects::ENTRY_POINT_NAME` convention (see the comment at the top of
/// effects/mod.rs). Registers a synthetic `FunctionDecl`, whose body is the entry's top-level
/// executable statements, into `program.functions` — this is the only way EffectCheck/Lint can
/// determine D-LINT-02/03/05 reachability from the top level.
fn register_entry_point(program: &mut Program, entry_file: FileId, entry_stmts: Vec<Stmt>) {
    let dummy = Span {
        file: entry_file,
        start: Position { line: 0, col: 0 },
        end: Position { line: 0, col: 0 },
    };
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
            stmts: entry_stmts,
            span: dummy,
        },
        leading_comments: Vec::new(),
        doc_comment: None,
        span: dummy,
    };
    program
        .functions
        .insert(Arc::from(effects::ENTRY_POINT_NAME), Arc::new(entry_decl));
}

fn take_registered_entry_items(program: &mut Program) -> Vec<Item> {
    let decl = program
        .functions
        .remove(effects::ENTRY_POINT_NAME)
        .unwrap_or_else(|| unreachable!("the driver just registered the synthetic entry point"));
    let decl = Arc::try_unwrap(decl)
        .unwrap_or_else(|_| unreachable!("static phases do not retain declaration references"));
    decl.body.stmts.into_iter().map(Item::Stmt).collect()
}

fn typecheck_fences_only(fences: &[DocFence], program: &Program, diagnostics: &mut DiagnosticBag) {
    for fence in fences {
        if let Some(diagnostic) = doctest::typecheck_fence_only(fence, program) {
            diagnostics.push(diagnostic);
        }
    }
}

// ---------------------------------------------------------------------------
// fmt (`ybm check`'s read-only default / `--apply` rewrite, ARCHITECTURE.md §4.3, D-MOD-04).
// ---------------------------------------------------------------------------

/// Splits a shebang line (starting with `#!`) from the rest. Same split rule as
/// `format_file_text` in the `fmt/printer.rs` test module (see the handoff note from Unit10
/// fmt).
fn split_shebang(src: &str) -> (Option<&str>, &str) {
    if src.starts_with("#!") {
        if let Some(nl) = src.find('\n') {
            return (Some(&src[..=nl]), &src[nl + 1..]);
        }
        return (Some(src), "");
    }
    (None, src)
}

/// `lexer::Lexer::strip_shebang` (outside this scope, does not reset line numbers) merely skips
/// over the shebang line's `\n`, so the information that a blank line immediately follows the
/// shebang never shows up in `fmt::format_module`'s output. Restored here (handoff note from
/// Unit10 fmt; `format_file_text` in `fmt/printer.rs` is the reference implementation).
fn reattach_shebang(original_text: &str, formatted_body: &str) -> String {
    let (shebang, rest) = split_shebang(original_text);
    match shebang {
        Some(sb) => {
            let first_line = rest.split('\n').next().unwrap_or("");
            if !rest.is_empty() && first_line.trim().is_empty() {
                format!("{sb}\n{formatted_body}")
            } else {
                format!("{sb}{formatted_body}")
            }
        }
        None => formatted_body.to_owned(),
    }
}

/// Computes the formatting result for one file (including reattaching the shebang). By the
/// time this is called, the file has already passed Lex/Parse with zero diagnostics, so
/// re-lexing/re-parsing here always succeeds (`build_program_skeleton` has already consumed
/// `Item::Decl`, so the same text is lexed/parsed again solely for fmt — since `ast` nodes do
/// not implement `Clone`, the driver follows the same existing pattern test code (e.g.
/// `stdlib/mod.rs`) uses: "parse multiple times and reuse it per ownership-demanding
/// consumer").
fn format_file_text(original: &str, file: FileId) -> String {
    let (tokens, comments, _lex_diag) = Lexer::new(original, file).tokenize();
    let (mut module, _parse_diag) = parse_module(&tokens, file);
    attach_comments(&mut module, comments);
    let formatted_body = fmt::format_module(&module);
    reattach_shebang(original, &formatted_body)
}

/// D-MOD-04: fmt applies to all files — the entry plus its sibling-directory modules. When
/// `apply_fmt` is false, no write happens; instead it only determines whether a diff exists and
/// prints the diff content to stdout (D-CLI-01).
fn apply_fmt(
    files: &[(PathBuf, FileId)],
    sources: &SourceMap,
    apply_fmt: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    let mut outputs: Vec<(PathBuf, String, String)> = Vec::with_capacity(files.len());
    for (path, fid) in files {
        let original = sources.file(*fid).text().to_owned();
        let formatted = format_file_text(&original, *fid);
        outputs.push((path.clone(), original, formatted));
    }

    if !apply_fmt {
        let mut any_diff = false;
        for (path, original, formatted) in &outputs {
            if original != formatted {
                any_diff = true;
                let _ = writeln!(out, "--- {}", path.display());
                let _ = writeln!(out, "-- before --");
                let _ = writeln!(out, "{original}");
                let _ = writeln!(out, "-- after --");
                let _ = writeln!(out, "{formatted}");
            }
        }
        return !any_diff;
    }

    let mut pending = Vec::new();
    for (index, (path, original, formatted)) in outputs.iter().enumerate() {
        if original == formatted {
            continue;
        }
        match stage_format(path, formatted, index) {
            Ok(staged) => pending.push(staged),
            Err(error) => {
                for staged in &pending {
                    let _ = fs::remove_file(&staged.temp);
                }
                report_fmt_error(err, path, &error);
                return false;
            }
        }
    }
    commit_formats(&pending, err)
}

struct StagedFormat {
    path: PathBuf,
    temp: PathBuf,
    backup: PathBuf,
}

fn stage_format(path: &Path, formatted: &str, index: usize) -> std::io::Result<StagedFormat> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to format a symbolic link",
        ));
    }

    let temp = format_sibling_path(path, "fmt", index);
    let backup = format_sibling_path(path, "backup", index);
    if fs::symlink_metadata(&backup).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("temporary backup already exists: {}", backup.display()),
        ));
    }

    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.set_permissions(metadata.permissions())?;
        file.write_all(formatted.as_bytes())?;
        file.sync_all()
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }

    Ok(StagedFormat {
        path: path.to_path_buf(),
        temp,
        backup,
    })
}

fn format_sibling_path(path: &Path, kind: &str, index: usize) -> PathBuf {
    let mut name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("ybm"))
        .to_os_string();
    name.push(format!(".{kind}-{}-{index}", std::process::id()));
    path.with_file_name(name)
}

fn commit_formats(pending: &[StagedFormat], err: &mut dyn Write) -> bool {
    for (index, staged) in pending.iter().enumerate() {
        if let Err(error) = fs::rename(&staged.path, &staged.backup) {
            rollback_formats(&pending[..index], err);
            cleanup_staged_formats(&pending[index..]);
            report_fmt_error(err, &staged.path, &error);
            return false;
        }
        if let Err(error) = fs::rename(&staged.temp, &staged.path) {
            let restore_error = fs::rename(&staged.backup, &staged.path).err();
            let _ = fs::remove_file(&staged.temp);
            rollback_formats(&pending[..index], err);
            cleanup_staged_formats(&pending[index + 1..]);
            report_fmt_error(err, &staged.path, &error);
            if let Some(restore_error) = restore_error {
                report_fmt_error(err, &staged.path, &restore_error);
            }
            return false;
        }
    }

    let mut ok = true;
    for staged in pending {
        if let Err(error) = fs::remove_file(&staged.backup) {
            report_fmt_error(err, &staged.path, &error);
            ok = false;
        }
    }
    ok
}

fn rollback_formats(committed: &[StagedFormat], err: &mut dyn Write) {
    for staged in committed.iter().rev() {
        if let Err(error) =
            fs::remove_file(&staged.path).and_then(|()| fs::rename(&staged.backup, &staged.path))
        {
            report_fmt_error(err, &staged.path, &error);
        }
    }
}

fn cleanup_staged_formats(staged: &[StagedFormat]) {
    for staged in staged {
        let _ = fs::remove_file(&staged.temp);
    }
}

fn report_fmt_error(err: &mut dyn Write, path: &Path, error: &std::io::Error) {
    let _ = writeln!(
        err,
        "{}:1:1 [{}] failed to write fmt output: {error}",
        path.display(),
        ErrorCode::FileReadFailure
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // samples/ acceptance verification (in-process, calls `run_pipeline_impl` directly).
    // Verifies the driver's own implementation independently of `tests/samples.rs`, which
    // spawns a process.
    // -----------------------------------------------------------------

    #[derive(Default, Clone, Debug)]
    struct TestCase {
        id: String,
        entry: String,
        cmd: String,
        stdin_file: String,
        exit_code: i32,
        diagnostics: Vec<String>,
        fmt_result_file: String,
        stdout_mode: String,
        stdout_value: String,
        stderr_mode: String,
        stderr_value: String,
        doc_blocks: Vec<(u32, String, Option<String>)>,
        requires_env: Vec<String>,
    }

    fn unescape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some('0') => out.push('\0'),
                    Some(other) => out.push(other),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn parse_toml_string(s: &str) -> String {
        let s = s.trim();
        let inner = s
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(s);
        unescape(inner)
    }

    fn parse_string_array(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '"' {
                let mut item = String::new();
                for c2 in chars.by_ref() {
                    if c2 == '"' {
                        break;
                    }
                    item.push(c2);
                }
                out.push(unescape(&item));
            }
        }
        out
    }

    fn extract_quoted_after(s: &str, key: &str) -> Option<String> {
        let idx = s.find(key)?;
        let after = &s[idx + key.len()..];
        let eq_idx = after.find('=')?;
        let after_eq = after[eq_idx + 1..].trim_start();
        let rest = after_eq.strip_prefix('"')?;
        let mut chars = rest.chars();
        let mut out = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    Some('0') => out.push('\0'),
                    Some(other) => out.push(other),
                    None => return Some(out),
                },
                other => out.push(other),
            }
        }
        Some(out)
    }

    fn extract_int_after(s: &str, key: &str) -> Option<u32> {
        let idx = s.find(key)?;
        let after = &s[idx + key.len()..];
        let eq_idx = after.find('=')?;
        let after_eq = after[eq_idx + 1..].trim_start();
        let digits: String = after_eq.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }

    fn parse_stdio_table(s: &str) -> (String, String) {
        let mode = extract_quoted_after(s, "mode").unwrap_or_else(|| "exact".to_owned());
        let value = extract_quoted_after(s, "value").unwrap_or_default();
        (mode, value)
    }

    fn parse_doc_block_entry(line: &str) -> Option<(u32, String, Option<String>)> {
        let line = line.trim().trim_end_matches(',').trim();
        let inner = line.strip_prefix('{')?.strip_suffix('}')?;
        let line_num = extract_int_after(inner, "line")?;
        let result = extract_quoted_after(inner, "result").unwrap_or_default();
        let code = extract_quoted_after(inner, "code");
        Some((line_num, result, code))
    }

    /// A minimal TOML reader for `expected.toml`. Reads a subset of the same schema as
    /// `tests/support/toml_lite.rs` — implemented independently because tests on the `src/`
    /// side of a binary crate cannot `use` anything under `tests/` (a separate compilation unit
    /// for integration tests) (judgment call made in this file).
    fn parse_cases(text: &str) -> Vec<TestCase> {
        let mut cases = Vec::new();
        let mut current: Option<TestCase> = None;
        let mut lines = text.lines();
        while let Some(raw_line) = lines.next() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[case]]" {
                if let Some(c) = current.take() {
                    cases.push(c);
                }
                current = Some(TestCase::default());
                continue;
            }
            let Some(case) = current.as_mut() else {
                continue;
            };
            let Some((key, rest)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let rest = rest.trim();
            match key {
                "id" => case.id = parse_toml_string(rest),
                "entry" => case.entry = parse_toml_string(rest),
                "cmd" => case.cmd = parse_toml_string(rest),
                "stdin_file" => case.stdin_file = parse_toml_string(rest),
                "exit_code" => case.exit_code = rest.parse().unwrap_or(0),
                "diagnostics" => case.diagnostics = parse_string_array(rest),
                "fmt_result_file" => case.fmt_result_file = parse_toml_string(rest),
                "requires_env" => case.requires_env = parse_string_array(rest),
                "stdout" => {
                    let (m, v) = parse_stdio_table(rest);
                    case.stdout_mode = m;
                    case.stdout_value = v;
                }
                "stderr" => {
                    let (m, v) = parse_stdio_table(rest);
                    case.stderr_mode = m;
                    case.stderr_value = v;
                }
                "doc_blocks" => {
                    if rest == "[]" {
                        // Empty array.
                    } else if let Some(inline) =
                        rest.strip_prefix('[').and_then(|r| r.strip_suffix(']'))
                    {
                        case.doc_blocks = inline
                            .split("},")
                            .filter_map(|entry| parse_doc_block_entry(&format!("{entry}}}")))
                            .collect();
                    } else {
                        let mut entries = Vec::new();
                        for l in lines.by_ref() {
                            let lt = l.trim();
                            if lt == "]" {
                                break;
                            }
                            if let Some(entry) = parse_doc_block_entry(lt) {
                                entries.push(entry);
                            }
                        }
                        case.doc_blocks = entries;
                    }
                }
                _ => {}
            }
        }
        if let Some(c) = current.take() {
            cases.push(c);
        }
        cases
    }

    fn discover_sample_dirs(root: &Path) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        collect_dirs_with_expected_toml(root, &mut dirs);
        dirs.sort();
        dirs
    }

    fn collect_dirs_with_expected_toml(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut has_expected = false;
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                subdirs.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("expected.toml") {
                has_expected = true;
            }
        }
        if has_expected {
            out.push(dir.to_path_buf());
        }
        for sub in subdirs {
            collect_dirs_with_expected_toml(&sub, out);
        }
    }

    fn samples_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("samples")
    }

    fn copy_dir_recursive(src: &Path, dst: &Path) {
        let _ = fs::create_dir_all(dst);
        let Ok(entries) = fs::read_dir(src) else {
            return;
        };
        for entry in entries.flatten() {
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if src_path.is_dir() {
                copy_dir_recursive(&src_path, &dst_path);
            } else if src_path.is_file() {
                let _ = fs::copy(&src_path, &dst_path);
            }
        }
    }

    fn scratch_dir_for(dir: &Path, case_id: &str) -> PathBuf {
        let base = std::env::var("YABUMI_TEST_SCRATCH_DIR")
            .map_or_else(|_| std::env::temp_dir(), PathBuf::from);
        let unique = format!(
            "ybm_driver_test_{}_{case_id}_{:?}",
            dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("dir")
                .replace(['/', '\\'], "_"),
            std::thread::current().id()
        );
        base.join(unique)
    }

    /// To avoid the E6008 (stack overflow) sample exhausting the Rust native stack and
    /// SIGABRT-ing the whole test process, runs one case on a thread with the same dedicated
    /// 64MiB stack as `main.rs` (same reason as `run_source_big_stack` in `eval/mod.rs`).
    fn run_case_pipeline(subcommand: &Subcommand) -> (bool, String, String) {
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, || {
                    let mut out = Vec::new();
                    let mut err = Vec::new();
                    let ok = run_pipeline_impl(subcommand, &mut out, &mut err);
                    (
                        ok,
                        String::from_utf8_lossy(&out).into_owned(),
                        String::from_utf8_lossy(&err).into_owned(),
                    )
                });
            match handle {
                Ok(h) => h.join().unwrap_or((false, String::new(), String::new())),
                Err(_) => (false, String::new(), String::new()),
            }
        })
    }

    fn extract_diagnostic_codes(text: &str) -> Vec<String> {
        let mut codes = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let mut i = 0;
        while i < len {
            let is_code = i + 6 < len
                && chars[i] == '['
                && chars[i + 1] == 'E'
                && chars[i + 2].is_ascii_digit()
                && chars[i + 3].is_ascii_digit()
                && chars[i + 4].is_ascii_digit()
                && chars[i + 5].is_ascii_digit()
                && chars[i + 6] == ']';
            if is_code {
                codes.push(chars[i + 1..i + 6].iter().collect());
                i += 7;
            } else {
                i += 1;
            }
        }
        codes
    }

    fn check_stdio(mode: &str, expected: &str, actual: &str) -> bool {
        if mode == "contains" {
            actual.contains(expected)
        } else {
            actual == expected
        }
    }

    fn line_number_matches(diagnostic_line: &str, line: u32) -> bool {
        diagnostic_line
            .split(':')
            .nth(1)
            .and_then(|s| s.parse::<u32>().ok())
            .is_some_and(|n| n == line)
    }

    enum CaseOutcome {
        Passed,
        Skipped,
        Failed(String),
    }

    /// Verifies one case directly via `run_pipeline_impl` (no process spawned).
    ///
    /// Because the `print`/`eprint` builtins call the real `println!`/`eprintln!` directly
    /// (`stdlib/builtins.rs`), they cannot be intercepted in-process — so this function does
    /// not compare `run`/`test`'s stdout/stderr content (that is `tests/samples.rs`'s
    /// responsibility, which runs as a separate process; see the existing comment in that
    /// file). exit_code, diagnostics (common to all commands and always captured reliably since
    /// the driver itself emits them), `check`/`check_diff`'s stdout/stderr, and `test`'s
    /// doc_blocks can all be verified reliably, so they are verified here.
    fn case_should_skip(case: &TestCase) -> bool {
        // External dependencies (mock HTTP server, child-process fixture) are not provided by
        // the in-process harness, so always skip these (as instructed in the task).
        if !case.requires_env.is_empty() {
            return true;
        }
        // env.stdin()/env.args() read the raw process stdin/argv (`stdlib/envns.rs`), which
        // cannot be reproduced correctly in-process.
        if !case.stdin_file.is_empty() {
            return true;
        }
        // D-ERR-06/PAR-ABORT-NOT-ACTUALLY-IMMEDIATE decision (ARCHITECTURE.md §5.8): a panic
        // inside a `par` worker thread has `concurrency/mod.rs` call `std::process::exit(1)`
        // directly, terminating the whole process immediately (outside this scope, intentional
        // by design). That would take this in-process harness's own test process down with it,
        // making it impossible to report other cases' results, so this case alone cannot be
        // verified and is skipped (verification is left to the process-isolated
        // `tests/samples.rs`).
        case.id == "par_branch_panics_immediately"
    }

    fn build_subcommand(case: &TestCase, entry_path: PathBuf) -> Result<Subcommand, String> {
        match case.cmd.as_str() {
            "run" => Ok(Subcommand::Run { file: entry_path }),
            "check" => Ok(Subcommand::Check {
                file: entry_path,
                apply_fmt: true,
            }),
            "check_diff" => Ok(Subcommand::Check {
                file: entry_path,
                apply_fmt: false,
            }),
            "test" => Ok(Subcommand::Test { file: entry_path }),
            other => Err(format!("unknown cmd '{other}'")),
        }
    }

    fn check_fmt_result_file(case: &TestCase, work_dir: Option<&Path>, problems: &mut Vec<String>) {
        if case.cmd != "check" || case.fmt_result_file.is_empty() {
            return;
        }
        let Some(wd) = work_dir else { return };
        let actual = fs::read(wd.join(&case.entry));
        let expected = fs::read(wd.join(&case.fmt_result_file));
        match (actual, expected) {
            (Ok(a), Ok(e)) if a == e => {}
            (a, e) => problems.push(format!(
                "fmt_result_file mismatch: actual_ok={}, expected_ok={}",
                a.is_ok(),
                e.is_ok()
            )),
        }
    }

    fn check_doc_blocks(case: &TestCase, stderr_text: &str, problems: &mut Vec<String>) {
        if case.cmd != "test" {
            return;
        }
        for (line, result, code) in &case.doc_blocks {
            let matching: Vec<&str> = stderr_text
                .lines()
                .filter(|l| line_number_matches(l, *line))
                .collect();
            match result.as_str() {
                "fail"
                    if !matching.iter().any(|l| {
                        code.as_deref()
                            .is_none_or(|c| l.contains(&format!("[{c}]")))
                    }) =>
                {
                    problems.push(format!(
                        "doc_blocks: no fail diagnostic found on line {line}"
                    ));
                }
                "pass" if !matching.is_empty() => {
                    problems.push(format!(
                        "doc_blocks: line {line} expected pass but a diagnostic was found"
                    ));
                }
                _ => {}
            }
        }
    }

    fn check_stdio_expectations(
        case: &TestCase,
        stdout_text: &str,
        stderr_text: &str,
        problems: &mut Vec<String>,
    ) {
        if case.cmd != "check" && case.cmd != "check_diff" {
            return;
        }
        if !check_stdio(&case.stdout_mode, &case.stdout_value, stdout_text) {
            problems.push(format!(
                "stdout({}): expected {:?}, got {stdout_text:?}",
                case.stdout_mode, case.stdout_value
            ));
        }
        if !check_stdio(&case.stderr_mode, &case.stderr_value, stderr_text) {
            problems.push(format!(
                "stderr({}): expected {:?}, got {stderr_text:?}",
                case.stderr_mode, case.stderr_value
            ));
        }
    }

    /// Verifies one case directly via `run_pipeline_impl` (no process spawned).
    ///
    /// Because the `print`/`eprint` builtins call the real `println!`/`eprintln!` directly
    /// (`stdlib/builtins.rs`), they cannot be intercepted in-process — so this function does
    /// not compare `run`/`test`'s stdout/stderr content (that is `tests/samples.rs`'s
    /// responsibility, which runs as a separate process; see the existing comment in that
    /// file). exit_code, diagnostics (common to all commands and always captured reliably since
    /// the driver itself emits them), `check`/`check_diff`'s stdout/stderr, and `test`'s
    /// doc_blocks can all be verified reliably, so they are verified here.
    /// Temporarily switches the current directory (so that samples using relative paths like
    /// `fs.write("_out/...")` actually run inside that sample's own directory — the in-process
    /// equivalent of `Command::current_dir(work_dir)` in `tests/samples.rs`). The current
    /// directory is state shared by the whole process, but since this harness runs cases one at
    /// a time, sequentially, inside the single `#[test]` function `run_all_samples_in_process`
    /// (never concurrently), no conflict arises with other `#[test]`s running in parallel
    /// within the same process — as long as they, like every existing test throughout this
    /// codebase does, use only absolute paths rooted at `CARGO_MANIFEST_DIR` and do not depend
    /// on the current directory.
    struct CwdGuard {
        original: PathBuf,
    }

    impl CwdGuard {
        fn enter(dir: &Path) -> Option<Self> {
            let original = std::env::current_dir().ok()?;
            std::env::set_current_dir(dir).ok()?;
            Some(Self { original })
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn run_one_case(dir: &Path, case: &TestCase) -> CaseOutcome {
        if case_should_skip(case) {
            return CaseOutcome::Skipped;
        }

        // To protect `samples/` itself not only from `ybm check --apply`'s in-place rewrite but
        // also from side effects `run`/`test` might cause by writing under `_out/` via
        // copy into a temp directory before running, for every command (`SAMPLES_PLAN.md`
        // §1.4, same policy as `tests/samples.rs`).
        let work_dir = scratch_dir_for(dir, &case.id);
        let _ = fs::remove_dir_all(&work_dir);
        copy_dir_recursive(dir, &work_dir);
        let entry_path = work_dir.join(&case.entry);

        let subcommand = match build_subcommand(case, entry_path) {
            Ok(sub) => sub,
            Err(msg) => {
                let _ = fs::remove_dir_all(&work_dir);
                return CaseOutcome::Failed(msg);
            }
        };

        let (ok, stdout_text, stderr_text) = {
            let _cwd_guard = CwdGuard::enter(&work_dir);
            run_case_pipeline(&subcommand)
        };
        let actual_exit = i32::from(!ok);

        let mut problems = Vec::new();
        if actual_exit != case.exit_code {
            problems.push(format!(
                "exit_code: expected {}, got {actual_exit}",
                case.exit_code
            ));
        }
        // The `test` command has fail diagnostics from inside doc blocks (e.g. E6004/E6005)
        // show up in stderr, while `case.diagnostics` is dedicated to top-level
        // (Lex/Parse/ModuleResolve/TypeCheck/EffectCheck/Lint) diagnostics and is always empty
        // — so this comparison is not performed at all for `test` (same judgment as
        // `wants_top_level_diagnostics_check` in `tests/samples.rs`).
        if case.cmd != "test" {
            let actual_diags = extract_diagnostic_codes(&stderr_text);
            if actual_diags != case.diagnostics {
                problems.push(format!(
                    "diagnostics: expected {:?}, got {actual_diags:?}",
                    case.diagnostics
                ));
            }
        }
        check_stdio_expectations(case, &stdout_text, &stderr_text, &mut problems);
        check_fmt_result_file(case, Some(&work_dir), &mut problems);
        check_doc_blocks(case, &stderr_text, &mut problems);

        let _ = fs::remove_dir_all(&work_dir);

        if problems.is_empty() {
            CaseOutcome::Passed
        } else {
            CaseOutcome::Failed(problems.join(" / "))
        }
    }

    /// An in-process version that verifies the same samples/ as
    /// `tests/samples.rs::run_all_samples` (process-isolated). Included in ordinary
    /// `cargo test` since it runs fast with no process spawned — only the
    /// `par_branch_panics_immediately` case, where a `par` worker thread panic calls
    /// `std::process::exit(1)`, is reliably excluded via `case_should_skip` to prevent the test
    /// binary itself from going down with it (see that comment for details).
    #[test]
    fn run_all_samples_in_process() {
        let root = samples_root();
        let dirs = discover_sample_dirs(&root);
        assert!(
            !dirs.is_empty(),
            "no expected.toml found anywhere under samples/"
        );

        let mut passed = 0usize;
        let mut skipped = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for dir in &dirs {
            let expected_path = dir.join("expected.toml");
            let Ok(text) = fs::read_to_string(&expected_path) else {
                failures.push(format!("{}: failed to read expected.toml", dir.display()));
                continue;
            };
            for case in parse_cases(&text) {
                let label = format!("{}/{}", dir.display(), case.id);
                match run_one_case(dir, &case) {
                    CaseOutcome::Passed => passed += 1,
                    CaseOutcome::Skipped => skipped += 1,
                    CaseOutcome::Failed(msg) => failures.push(format!("{label}: {msg}")),
                }
            }
        }

        eprintln!(
            "in-process samples: {passed} passed, {skipped} skipped, {} failed",
            failures.len()
        );
        for f in &failures {
            eprintln!("  FAILED: {f}");
        }
        assert!(
            failures.is_empty(),
            "{} case(s) failed (see stderr above for details)",
            failures.len()
        );
    }

    #[test]
    fn formatter_check_is_non_mutating_and_covers_modules() {
        let dir = scratch_dir_for(Path::new("formatter"), "transaction");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("create scratch dir: {error}"));
        let entry_path = dir.join("entry.ybm");
        let module_path = dir.join("mod_values.ybm");
        let entry_text = "value=1\n";
        let module_text = "module\n\nother=2\n";
        fs::write(&entry_path, entry_text)
            .unwrap_or_else(|error| panic!("write entry fixture: {error}"));
        fs::write(&module_path, module_text)
            .unwrap_or_else(|error| panic!("write module fixture: {error}"));

        let mut sources = SourceMap::new();
        let entry_file = sources.add(entry_path.clone(), entry_text.to_owned());
        let module_file = sources.add(module_path.clone(), module_text.to_owned());
        let files = vec![
            (entry_path.clone(), entry_file),
            (module_path.clone(), module_file),
        ];
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert!(!apply_fmt(&files, &sources, false, &mut out, &mut err));
        assert!(!out.is_empty());
        assert!(err.is_empty());
        assert_eq!(
            fs::read_to_string(&entry_path).ok().as_deref(),
            Some(entry_text)
        );
        assert_eq!(
            fs::read_to_string(&module_path).ok().as_deref(),
            Some(module_text)
        );

        out.clear();
        assert!(apply_fmt(&files, &sources, true, &mut out, &mut err));
        assert!(err.is_empty());
        assert_eq!(
            fs::read_to_string(&entry_path).ok().as_deref(),
            Some("value = 1\n")
        );
        assert_eq!(
            fs::read_to_string(&module_path).ok().as_deref(),
            Some("module\n\nother = 2\n")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_command_stops_before_doctests_on_static_errors() {
        let dir = scratch_dir_for(Path::new("doctest"), "static-gate");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("create scratch dir: {error}"));
        let entry = dir.join("entry.ybm");
        fs::write(
            &entry,
            "## ```\n## assert(true)\n## ```\ndef documented(): void\n    return\n\nunused = 1\n",
        )
        .unwrap_or_else(|error| panic!("write static-gate fixture: {error}"));

        let (ok, out, err) = run_case_pipeline(&Subcommand::Test { file: entry });
        assert!(!ok);
        assert!(err.contains("[E400"));
        assert!(!out.contains("doctest:"));
        let _ = fs::remove_dir_all(dir);
    }
    #[test]
    fn check_rejects_type_invalid_doc_fence_without_execution() {
        let dir = scratch_dir_for(Path::new("doctest"), "check-invalid");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("create scratch dir: {error}"));
        let entry = dir.join("entry.ybm");
        fs::write(
            &entry,
            "## ```\n## value = 1 + \"x\"\n## ```\ndef documented(): void\n    return\n\ndocumented()\n",
        )
        .unwrap_or_else(|error| panic!("write invalid-fence fixture: {error}"));

        let (ok, out, err) = run_case_pipeline(&Subcommand::Check {
            file: entry,
            apply_fmt: false,
        });
        assert!(!ok);
        assert!(out.is_empty());
        assert!(err.contains("[E1050]") || err.contains("[E1020]"));
        let _ = fs::remove_dir_all(dir);
    }
}
