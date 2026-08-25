//! Execution of `##`-fence extraction results, building an independent program, pass/fail
//! aggregation (ARCHITECTURE.md §5.10).
//!
//! The concrete means of running each `DocFence` (with no language tag) as an "independent
//! program" is to prepare one new `Environment` frame and simply share the existing
//! `Program` (global declarations) as-is — there is no need to spawn a new process or OS
//! thread.
//!
//! [`collect_fences`] takes not `Program` (post `build_program_skeleton`), but `&[Module]`
//! right after parsing, while it still holds `Item::Stmt` — because
//! `build_program_skeleton` in `module_resolve/mod.rs` discards every `Item::Stmt`
//! (module-level constants, plus the entry file's top-level assignments that look the same
//! on the surface — of D-DOC-03's four kinds of declaration, only constants take this path)
//! without leaving any in `Program`'s skeleton, `Stmt.doc_comment` cannot be recovered from
//! `Program` alone. So `driver.rs` calls `doctest::collect_fences(&modules)` **before**
//! handing `modules`'s ownership over to `build_program_skeleton(modules, ..)`.
//!
//! For the same reason, [`collect_doctest_pseudo_consts`] also requires `&[Module]`. To
//! satisfy SPEC §13's "the scope is the whole file," even a top-level assignment consisting
//! only of literals in the entry file itself (`module_resolve/flat_namespace.rs` registers
//! into `Program.consts` only the constants belonging to files where
//! `module.is_module_directive`, and D-TYPE-07 excludes the entry file's assignments of the
//! same surface shape) must be treated as a "constant" within doctest's scope too.
//! `driver.rs` merges this function's result into `program.consts` via
//! `entry(name).or_insert(value)` (not overwriting an existing module constant) after
//! running `build_program_skeleton` and before running `run_all_fences`.
//!
//! **A known limitation that has now been resolved**: [`run_fence`] parses the fence body
//! independently with its own dedicated `Lexer`/`Parser`, and if its `NodeId`s were simply
//! renumbered starting from 0 as-is, they could overwrite entries the real declarations are
//! already using in `program.resolutions` (a side table keyed by `NodeId`, shared with them,
//! `types/resolutions.rs`) at the same numeric value. Merely containing the blast radius to
//! "type-checking and running this one fence" via [`clone_program_for_fence`] was not
//! enough — if the fence's own nodes numerically collided with internal nodes of a real
//! declaration the fence itself calls, that one fence's execution could still misbehave.
//! This has now been resolved by adding `Parser::with_start_id`/
//! `parser::parse_module_with_offset` (`src/parser/mod.rs`), which lets `parser::Parser`
//! accept a starting value for `NodeId`, with [`safe_fence_id_base`] computing a safe start
//! offset from the real declarations' total node count and passing it to the fence's parse
//! (see each function's comments for details).

use crate::ast::{Decl, DocComment, DocFence, Expr, Item, Module, Stmt, StmtKind};
use crate::diagnostics::{Diagnostic, DiagnosticBag, FileId, Position, Span};
use crate::eval::env::{Environment, Program};
use crate::eval::value::Value;
use crate::eval::{Abort, run_top_level};
use crate::lexer::{FStringPart, Lexer, Token, TokenKind};
use crate::module_resolve::flat_namespace::resolve_module_const_values;
use crate::module_resolve::module_grammar::module_level_const;
use crate::parser::{parse_module, parse_module_with_offset};
use crate::types::Resolutions;
use crate::types::check_decl::{check_all_decls, check_top_level_stmts};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub struct BlockResult {
    pub line: u32,
    pub outcome: Outcome,
}

pub enum Outcome {
    Pass,
    Fail(Diagnostic),
}

/// 1. Lexes and parses `fence.raw_text` as an independent series of statements.
/// 2. Type-checks with "the entry plus every declaration in sibling modules" as scope
///    (D-MOD-04/D-DOC-03). If a diagnostic is emitted, `fail` (`code` is that diagnostic's
///    `ErrorCode`, `line` follows D-DOC-05's rule of pointing at a real file line).
/// 3. If type checking passes, executes the fence's statements in order against the same
///    `Program`, in a fresh `Environment` (D-DOC-04: each block gets its own independent
///    execution context). `fail` on an `assert` failure or a panic/`?` propagation, `pass`
///    if execution reaches the end.
#[must_use]
pub fn run_fence(fence: &DocFence, program: &Program) -> BlockResult {
    // The fence body is parsed with its own dedicated `Lexer`/`Parser`. To keep its
    // `NodeId`s from numerically colliding with the real declarations' `NodeId`s,
    // `parse_fence_body` is passed the safe start offset `safe_fence_id_base` computes
    // (`parser::parse_module_with_offset`, see each function's comments for details).
    let node_id_base = safe_fence_id_base(program, fence.span.file);
    let module = match parse_fence_body(fence, node_id_base) {
        Ok(module) => module,
        Err(diag) => return fail_result(diag),
    };

    // `program` itself is never modified at all; a single disposable `Program` is
    // assembled that clones the real declarations (functions/structs/enums/consts) and the
    // existing `resolutions` and used only here — this containment of the blast radius is
    // maintained independently of the offset assignment via `node_id_base` (so that
    // entries the fence's type checking appends never leak into another fence or the
    // caller's `program`).
    let mut fence_program = clone_program_for_fence(program);
    let stmts = match install_fence_items(module.items, &mut fence_program) {
        Ok(stmts) => stmts,
        Err(diagnostic) => return fail_result(diagnostic),
    };
    if let Some(diag) = typecheck_fence(&stmts, &mut fence_program) {
        return fail_result(diag);
    }

    // Once type checking passes, this executes the fence's statements in order against a
    // fresh `Environment` (D-DOC-04: each block gets its own independent execution
    // context, unaffected by another block's assert failure). An `assert` failure is
    // simply an `Err(Abort(..))` returned as the evaluation result of a built-in call, and
    // it goes through exactly the same path as any other panic or top-level `?` propagation
    // (`run_top_level`) (ARCHITECTURE.md §5.10) — no doctest-specific logic needs to be
    // added.
    let items: Vec<Item> = stmts.into_iter().map(Item::Stmt).collect();
    let fence_program = Arc::new(fence_program);
    let mut env = Environment::with_frame(HashMap::new());
    match run_top_level(&items, &mut env, &fence_program) {
        Ok(()) => BlockResult {
            line: fence.body_start_line,
            outcome: Outcome::Pass,
        },
        Err(Abort(diag)) => fail_result(diag),
    }
}

/// Calls `run_fence` for every `DocFence` in the entry plus sibling modules, and aggregates
/// the results. One block's `Fail` has no effect at all on another block's execution.
#[must_use]
pub fn run_all_fences(fences: &[DocFence], program: &Program) -> Vec<BlockResult> {
    fences
        .iter()
        .map(|fence| run_fence(fence, program))
        .collect()
}

pub(crate) fn typecheck_fence_only(fence: &DocFence, program: &Program) -> Option<Diagnostic> {
    let node_id_base = safe_fence_id_base(program, fence.span.file);
    let module = match parse_fence_body(fence, node_id_base) {
        Ok(module) => module,
        Err(diagnostic) => return Some(diagnostic),
    };
    let mut fence_program = clone_program_for_fence(program);
    let statements = match install_fence_items(module.items, &mut fence_program) {
        Ok(statements) => statements,
        Err(diagnostic) => return Some(diagnostic),
    };
    typecheck_fence(&statements, &mut fence_program)
}

/// D-DOC-05: the `line` in a fail report is a real file line — `diag.span` already points at
/// a real file line (either via `parse_fence_body`'s per-token shift, or via the span of the
/// fence's own AST node that a runtime Abort references), so it is used as-is. The `line` on
/// pass (`fence.body_start_line`) never goes through this function ([`run_fence`] assembles
/// `BlockResult` directly).
fn fail_result(diag: Diagnostic) -> BlockResult {
    let line = diag.span.start.line;
    BlockResult {
        line,
        outcome: Outcome::Fail(diag),
    }
}

/// The width of `## ` (two `#`s plus the conventional single space, D-FMT-03).
/// `comment_attach.rs`'s `strip_one_leading_space` strips only a single leading space from
/// the already-`##`-stripped comment body, so column 1 of the fence body (`raw_text`) always
/// corresponds to column 4 of the real file — hence an offset of 3.
const FENCE_CONTENT_COL_OFFSET: u32 = 3;

/// Lexes and parses the fence body (`fence.raw_text`) as an independent series of
/// statements.
///
/// To satisfy D-DOC-05 (the `line` in a fail report is a real file line), immediately after
/// lexing and before parsing, the token stream's `Span`s are shifted onto their real
/// position in the file — since each line of `raw_text` corresponds 1:1 to the matching
/// line in the real file (`fence.body_start_line` being the real file line number of the
/// first line), the shift amount is determined by two constant offsets: "line: add
/// `body_start_line - 1`" and "column: add [`FENCE_CONTENT_COL_OFFSET`]." This way, the
/// diagnostics that subsequent type checking (`typecheck_fence`) and evaluation
/// (`run_top_level`), fed this statement list, produce already point at a real file line
/// (and column) from the start, with no extra conversion needed.
fn parse_fence_body(fence: &DocFence, node_id_base: u32) -> Result<Module, Diagnostic> {
    let file = fence.span.file;
    let line_offset = fence.body_start_line.saturating_sub(1);

    let (raw_tokens, _comments, lex_diags) = Lexer::new(&fence.raw_text, file).tokenize();
    let lex_diags: Vec<Diagnostic> = lex_diags
        .into_vec()
        .into_iter()
        .map(|diagnostic| shift_diagnostic(diagnostic, line_offset, FENCE_CONTENT_COL_OFFSET))
        .collect();
    if let Some(diagnostic) = first_diagnostic(lex_diags) {
        return Err(diagnostic);
    }

    let tokens: Vec<Token> = raw_tokens
        .into_iter()
        .map(|token| shift_token(token, line_offset, FENCE_CONTENT_COL_OFFSET))
        .collect();
    let (module, parse_diags, _next_id) = parse_module_with_offset(&tokens, file, node_id_base);
    if let Some(diagnostic) = first_diagnostic(parse_diags.into_vec()) {
        return Err(diagnostic);
    }
    Ok(module)
}

fn install_fence_items(items: Vec<Item>, program: &mut Program) -> Result<Vec<Stmt>, Diagnostic> {
    let mut statements = Vec::new();
    for item in items {
        match item {
            Item::Stmt(statement) => statements.push(statement),
            Item::Decl(Decl::Function(declaration)) => {
                if program.functions.contains_key(declaration.name.as_ref()) {
                    return Err(Diagnostic {
                        code: crate::diagnostics::ErrorCode::DuplicateName,
                        span: declaration.span,
                        message: format!("duplicate definition of '{}'", declaration.name),
                    });
                }
                program
                    .functions
                    .insert(Arc::clone(&declaration.name), Arc::new(declaration));
            }
            Item::Decl(Decl::Struct(declaration)) => {
                if program.structs.contains_key(declaration.name.as_ref()) {
                    return Err(Diagnostic {
                        code: crate::diagnostics::ErrorCode::DuplicateName,
                        span: declaration.span,
                        message: format!("duplicate definition of '{}'", declaration.name),
                    });
                }
                program
                    .structs
                    .insert(Arc::clone(&declaration.name), Arc::new(declaration));
            }
            Item::Decl(Decl::Enum(declaration)) => {
                if program.enums.contains_key(declaration.name.as_ref()) {
                    return Err(Diagnostic {
                        code: crate::diagnostics::ErrorCode::DuplicateName,
                        span: declaration.span,
                        message: format!("duplicate definition of '{}'", declaration.name),
                    });
                }
                program
                    .enums
                    .insert(Arc::clone(&declaration.name), Arc::new(declaration));
            }
        }
    }
    Ok(statements)
}

/// Computes a safe `NodeId` start offset for parsing the fence body.
///
/// Since the fence's dedicated `Parser` always renumbers `NodeId` starting from 0 (see the
/// comment at the top of this file), the entries the fence's type checking
/// (`typecheck_fence`) writes into `fence_program.resolutions` must never numerically
/// collide with `NodeId`s already used by the internal nodes of a real declaration
/// (function/struct/enum) held in `program`, or by another declaration or top-level
/// statement in the same file this fence belongs to.
///
/// Since `NodeId` assignment is fully deterministic for a given token stream (`next_node_id`
/// in `parser/mod.rs` merely increases monotonically as parsing proceeds, with no
/// dependence on external state), independently re-lexing and re-parsing each file a real
/// declaration belongs to can accurately reproduce the total number of `NodeId`s that file
/// originally consumed (= the final counter value `parse_module_with_offset` returns),
/// without needing a hand-written new implementation of a dedicated visitor that walks the
/// full AST covering every node kind — a judgment call made in this file. When multiple
/// files are involved, their sum is used as a safe-side lower bound (since each file is
/// renumbered from 0 by its own independent `Parser` instance, strictly speaking only "the
/// largest single file among those involved" is needed, but substituting the sum is harmless
/// and merely errs on the safe side).
///
/// `fence_file` (the file this fence itself belongs to) is always included as a candidate —
/// the fence attached to a module-level constant's (`Item::Stmt(StmtKind::NameAssign)`) doc
/// comment can miss this file's own top-level statement `NodeId`s if only the real
/// declarations are enumerated, in the case where that file contains not a single other
/// function/struct/enum declaration, since the constant itself never appears in
/// `program.functions`/`structs`/`enums` (`Program.consts` holds only the already-evaluated
/// `Value`, with no AST, D-DOC-03) — always including `fence_file` as a candidate closes
/// this gap.
fn safe_fence_id_base(program: &Program, fence_file: FileId) -> u32 {
    let mut files: HashSet<FileId> = HashSet::new();
    files.insert(fence_file);
    files.extend(program.functions.values().map(|f| f.span.file));
    files.extend(program.structs.values().map(|s| s.span.file));
    files.extend(program.enums.values().map(|e| e.span.file));

    let mut total: u32 = 0;
    for file in files {
        let text = program.sources.file(file).text();
        let (tokens, _comments, _lex_diags) = Lexer::new(text, file).tokenize();
        let (_module, _diags, next_id) = parse_module_with_offset(&tokens, file, 0);
        total = total.saturating_add(next_id);
    }
    total.saturating_add(1)
}

/// Type-checks `stmts` (the fence body) as an independent virtual program whose scope is
/// "the entry plus every declaration in sibling modules." If there is even one diagnostic,
/// returns the first one (in ascending `file:line:col` order) (D-CLI-03's "collect all" is
/// the main pipeline's policy — one block's fail report is designed to hold only a single
/// `Diagnostic` in `BlockResult`, ARCHITECTURE.md §5.10).
fn typecheck_fence(stmts: &[Stmt], program: &mut Program) -> Option<Diagnostic> {
    let mut diagnostics = DiagnosticBag::new();
    check_all_decls(program, &mut diagnostics);
    check_top_level_stmts(stmts, program, &mut diagnostics);
    first_diagnostic(diagnostics.into_vec())
}

/// Picks the first diagnostic in ascending `file:line:col` order (comparing only `line`/
/// `col` suffices since this is within a single file — the path-name comparison in
/// `DiagnosticBag::into_sorted`, which spans multiple files, is unnecessary here).
fn first_diagnostic(diags: Vec<Diagnostic>) -> Option<Diagnostic> {
    diags
        .into_iter()
        .min_by_key(|d| (d.span.start.line, d.span.start.col))
}

fn shift_position(pos: Position, line_offset: u32, col_offset: u32) -> Position {
    Position {
        line: pos.line + line_offset,
        col: pos.col + col_offset,
    }
}

fn shift_span(span: Span, line_offset: u32, col_offset: u32) -> Span {
    Span {
        file: span.file,
        start: shift_position(span.start, line_offset, col_offset),
        end: shift_position(span.end, line_offset, col_offset),
    }
}

fn shift_diagnostic(diag: Diagnostic, line_offset: u32, col_offset: u32) -> Diagnostic {
    Diagnostic {
        code: diag.code,
        span: shift_span(diag.span, line_offset, col_offset),
        message: diag.message,
    }
}

/// The `{expr}` portion inside an f-string holds a recursive `Vec<Token>`
/// (`TokenKind::FString`), so shifting the entire token stream onto its real file position
/// requires following this nesting too.
fn shift_token(token: Token, line_offset: u32, col_offset: u32) -> Token {
    let kind = match token.kind {
        TokenKind::FString(parts) => TokenKind::FString(
            parts
                .into_iter()
                .map(|p| shift_fstring_part(p, line_offset, col_offset))
                .collect(),
        ),
        other => other,
    };
    Token {
        kind,
        span: shift_span(token.span, line_offset, col_offset),
    }
}

fn shift_fstring_part(part: FStringPart, line_offset: u32, col_offset: u32) -> FStringPart {
    match part {
        FStringPart::Text(text) => FStringPart::Text(text),
        FStringPart::Expr(tokens) => FStringPart::Expr(
            tokens
                .into_iter()
                .map(|t| shift_token(t, line_offset, col_offset))
                .collect(),
        ),
    }
}

/// Assembles a disposable `Program` dedicated to type-checking and executing a single
/// fence. `functions`/`structs`/`enums`/`consts` all hold values that are either `Arc` or
/// `Value` (value semantics, D-MUT-04), so cloning them is a lightweight reference-count
/// duplication only; `sources` is `Arc<SourceMap>`, so cloning it is just a pointer
/// duplication. Only `resolutions` is a side table keyed by `NodeId`, and is duplicated
/// field-by-field via [`clone_resolutions`] (so that entries this fence's type checking
/// appends never leak into another fence or the caller's `program`).
fn clone_program_for_fence(program: &Program) -> Program {
    Program {
        functions: program.functions.clone(),
        structs: program.structs.clone(),
        enums: program.enums.clone(),
        consts: program.consts.clone(),
        resolutions: clone_resolutions(&program.resolutions),
        sources: Arc::clone(&program.sources),
        abort_process_on_par_panic: false,
    }
}

/// `Resolutions` does not implement `Clone` (like AST nodes, resolution-result side tables
/// generally follow a policy of not deriving it), so it is duplicated field-by-field (each
/// field is `HashMap<NodeId, T>` with `T: Clone`).
fn clone_resolutions(r: &Resolutions) -> Resolutions {
    Resolutions {
        field_index: r.field_index.clone(),
        type_args: r.type_args.clone(),
        decode_target: r.decode_target.clone(),
        csv_encode_target: r.csv_encode_target.clone(),
        bare_ident_kind: r.bare_ident_kind.clone(),
        call_kind: r.call_kind.clone(),
        expr_ty: r.expr_ty.clone(),
        implicit_wrap: r.implicit_wrap.clone(),
        namespace_ref: r.namespace_ref.clone(),
        hof_forwarding: r.hof_forwarding.clone(),
    }
}

/// Collects the doctest targets' `DocFence`s (D-DOC-01: only those with no language tag) in
/// source order from `modules` (the entry plus sibling modules, right after parsing, before
/// being handed to `build_program_skeleton`). Following D-DOC-03 (a `##` block directly
/// before a def/struct/enum/module-level constant declaration is a target regardless of
/// which), this follows the `doc_comment` of not only all three kinds of `Item::Decl`
/// (`FunctionDecl` body, `StructDecl` body + each method, `EnumDecl` body) but also
/// `Item::Stmt(StmtKind::NameAssign)`.
///
/// This function takes `&[Module]` rather than `Program`, for the reasons stated in the
/// comment at the top of the module — the caller (`driver.rs`) calls this **before** handing
/// `modules`'s ownership to `build_program_skeleton`.
#[must_use]
pub fn collect_fences(modules: &[Module]) -> Vec<DocFence> {
    let mut out = Vec::new();
    for module in modules {
        for item in &module.items {
            match item {
                Item::Decl(Decl::Function(f)) => {
                    collect_from_doc_comment(f.doc_comment.as_ref(), &mut out);
                }
                Item::Decl(Decl::Struct(s)) => {
                    collect_from_doc_comment(s.doc_comment.as_ref(), &mut out);
                    for m in &s.methods {
                        collect_from_doc_comment(m.doc_comment.as_ref(), &mut out);
                    }
                }
                Item::Decl(Decl::Enum(e)) => {
                    collect_from_doc_comment(e.doc_comment.as_ref(), &mut out);
                }
                Item::Stmt(stmt) => {
                    if matches!(stmt.kind, StmtKind::NameAssign { .. }) {
                        collect_from_doc_comment(stmt.doc_comment.as_ref(), &mut out);
                    }
                }
            }
        }
    }
    out
}

fn collect_from_doc_comment(doc: Option<&DocComment>, out: &mut Vec<DocFence>) {
    let Some(doc) = doc else { return };
    out.extend(
        doc.fences
            .iter()
            .filter(|f| is_doctest_target(f))
            .map(clone_fence),
    );
}

/// D-DOC-01: only a fence with no language tag (`None`. `comment_attach.rs` also normalizes
/// an empty tag to `None`, so `Some("")` is never actually produced in practice, but this
/// keeps both conditions `DocFence::lang_tag`'s field comment lists, reflected as-is) is a
/// doctest target.
fn is_doctest_target(fence: &DocFence) -> bool {
    matches!(fence.lang_tag.as_deref(), None | Some(""))
}

/// `DocFence` does not implement `Clone` (AST nodes generally follow a policy of not
/// deriving it, ARCHITECTURE.md §3.4), so it is duplicated field-by-field. Every field
/// (`Option<String>`/`u32`/`String`/`Span`) implements `Clone`.
fn clone_fence(fence: &DocFence) -> DocFence {
    DocFence {
        lang_tag: fence.lang_tag.clone(),
        body_start_line: fence.body_start_line,
        raw_text: fence.raw_text.clone(),
        span: fence.span,
    }
}

/// Extra data needed to satisfy SPEC §13's "the scope is the whole file (the entry plus
/// every declaration in sibling modules)." `register_flat_namespace` in
/// `module_resolve/flat_namespace.rs` registers an `Item::Stmt` into `Program.consts` only
/// when it is in a file carrying a module directive (`module.is_module_directive == true`)
/// and matches D-MOD-02's restricted grammar (literals/collection literals/constant
/// references only), and treats the entry file's own top-level `NameAssign` (even if it
/// looks the same on the surface) as an "ordinary executable statement," out of scope for
/// `Program.consts` (D-TYPE-07) — this is correct behavior for ordinary program execution
/// (proper namespace separation, invisible from other `def`s), but D-DOC-03 generalizes
/// def/struct/enum/constant as doctest targets with no such distinction, and
/// `samples/doctest/target_declarations_struct_enum_const` requires that the `##` block
/// attached to `pi_approx = 3.14` directly under the entry file be able to reference
/// `pi_approx` itself (D-MOD-04, "the scope is the whole file").
///
/// So this function, regardless of `module.is_module_directive`, judges literal eligibility
/// for every file using `module_level_const` (`module_resolve::module_grammar`, a
/// `pub(crate)` helper sharing the same restricted-grammar check as D-MOD-02) and returns the
/// side-effect-free evaluated value of every eligible `NameAssign` — this is **dedicated
/// solely to building doctest's scope**, and does not change the ordinary program-execution
/// namespace (what `Program.consts` originally means, "the constants a module publicly
/// exposes"). Since a module-directive file's constants are already registered by
/// `register_flat_namespace` into `Program.consts`, only the **entry file** is targeted
/// here, to avoid double registration.
///
/// Like [`collect_fences`], this function also requires `&[Module]` from before
/// `build_program_skeleton` runs. The caller (`driver.rs`) merges this function's result
/// into `program.consts` after running `build_program_skeleton` and before running
/// `run_all_fences` (`entry(name).or_insert(value)` suffices — if the same name is already
/// present in `Program.consts`, it originates from a module constant, and is not overwritten
/// by the entry side's value).
#[must_use]
pub fn collect_doctest_pseudo_consts(modules: &[Module]) -> Vec<(Arc<str>, Value)> {
    let mut pending: Vec<(Arc<str>, &Expr)> = Vec::new();
    for module in modules {
        if module.is_module_directive {
            // A module-directive file's constants are already registered into
            // Program.consts by register_flat_namespace.
            continue;
        }
        for item in &module.items {
            if let Item::Stmt(stmt) = item
                && let Some((name, value)) = module_level_const(stmt)
            {
                pending.push((name.clone(), value));
            }
        }
    }

    // The same fixpoint iteration as flat_namespace.rs's resolve_module_const_values —
    // resolves forward references between constants (which can reference each other
    // regardless of declaration order, D-MOD-02).
    let mut known: HashMap<Arc<str>, Value> = HashMap::new();
    resolve_module_const_values(pending, &mut known);
    known.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::{
        BlockResult, DocFence, Outcome, Program, Stmt, collect_doctest_pseudo_consts,
        collect_fences, run_all_fences, typecheck_fence_only,
    };
    use crate::ast::Item;
    use crate::diagnostics::{DiagnosticBag, SourceMap};
    use crate::lexer::Lexer;
    use crate::module_resolve::{build_program_skeleton, discover_sibling_modules};
    use crate::parser::comment_attach::attach_comments;
    use crate::parser::parse_module;
    use crate::types::check_decl::check_program;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn sample_dir(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples/doctest")
            .join(name)
    }

    fn read_file(path: &Path) -> String {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => panic!("failed to read {}: {e}", path.display()),
        }
    }

    // ---- A minimal parser dedicated to expected.toml ----
    //
    // expected.toml only ever uses SAMPLES_PLAN's fixed layout (a repetition of
    // `[[case]]`, each field one key per line, with only `doc_blocks` being an array of
    // `{ .. }`). This is read via line-based extraction dedicated to this test, without
    // depending on the general-purpose TOML parser (stdlib::codec::toml) (the same thinking
    // as the D-FMT-06 rationale, favoring zero-dependency distribution and implementation
    // simplicity project-wide).

    struct ExpectedBlock {
        line: u32,
        result: String,
        code: Option<String>,
    }

    struct ExpectedCase {
        cmd: String,
        exit_code: i32,
        doc_blocks: Vec<ExpectedBlock>,
    }

    fn extract_field_line<'a>(chunk: &'a str, key: &str) -> Option<&'a str> {
        let prefix = format!("{key} = ");
        chunk
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(prefix.as_str()))
    }

    fn parse_doc_blocks(chunk: &str) -> Vec<ExpectedBlock> {
        let Some(start) = chunk.find("doc_blocks = [") else {
            return Vec::new();
        };
        let after = &chunk[start + "doc_blocks = [".len()..];
        let Some(end) = after.find(']') else {
            return Vec::new();
        };
        let body = &after[..end];
        body.split('{')
            .skip(1)
            .filter_map(|entry| {
                let close = entry.find('}')?;
                let fields = &entry[..close];
                let mut line = None;
                let mut result = None;
                let mut code = None;
                for part in fields.split(',') {
                    let part = part.trim();
                    if let Some(v) = part.strip_prefix("line = ") {
                        line = v.trim().parse::<u32>().ok();
                    } else if let Some(v) = part.strip_prefix("result = ") {
                        result = Some(v.trim().trim_matches('"').to_owned());
                    } else if let Some(v) = part.strip_prefix("code = ") {
                        code = Some(v.trim().trim_matches('"').to_owned());
                    }
                }
                Some(ExpectedBlock {
                    line: line?,
                    result: result?,
                    code,
                })
            })
            .collect()
    }

    fn parse_expected_toml(text: &str) -> Vec<ExpectedCase> {
        text.split("[[case]]")
            .skip(1)
            .map(|chunk| ExpectedCase {
                cmd: extract_field_line(chunk, "cmd")
                    .map(|v| v.trim_matches('"').to_owned())
                    .unwrap_or_default(),
                exit_code: extract_field_line(chunk, "exit_code")
                    .and_then(|v| v.trim().parse::<i32>().ok())
                    .unwrap_or(-1),
                doc_blocks: parse_doc_blocks(chunk),
            })
            .collect()
    }

    // ---- Building a `Program`+`DocFence` through lex/parse/module_resolve/TypeCheck ----
    //
    // Of `ybm test`'s "6 phases → doc fences," EffectCheck/Lint are deliberately skipped —
    // this module's responsibility is doctest extraction, independent execution, and
    // aggregation, and wiring up EffectCheck/Lint would require registering a synthesized
    // `FunctionDecl` following the `crate::effects::ENTRY_POINT_NAME` convention (see
    // driver.rs and the comment at the top of `src/effects/mod.rs`). None of the samples
    // under samples/doctest/ contain any excess effect declaration or lint violation, so
    // running EffectCheck/Lint would not change the result (0 diagnostics) anyway.

    fn load_program_and_fences(dir: &Path) -> (Vec<DocFence>, Program, bool) {
        let entry_path = dir.join("entry_main.ybm");
        let mut sibling_paths = discover_sibling_modules(&entry_path);
        let mut all_paths = vec![entry_path.clone()];
        all_paths.append(&mut sibling_paths);

        let mut sources = SourceMap::new();
        let mut modules = Vec::new();
        for path in &all_paths {
            let text = read_file(path);
            let file = sources.add(path.clone(), text.clone());
            let (tokens, comments, lex_diags) = Lexer::new(&text, file).tokenize();
            assert!(
                !lex_diags.has_any(),
                "{}: unexpected lex error",
                path.display()
            );
            let (mut module, parse_diags) = parse_module(&tokens, file);
            assert!(
                !parse_diags.has_any(),
                "{}: unexpected parse error",
                path.display()
            );
            attach_comments(&mut module, comments);
            modules.push(module);
        }

        let fences = collect_fences(&modules);
        let pseudo_consts = collect_doctest_pseudo_consts(&modules);

        // Since build_program_skeleton does not retain Item::Stmt (module_resolve/mod.rs),
        // the "entry's top-level executable statements" passed to check_program are set
        // aside via an independent re-parse (the same approach as the existing tests in
        // eval/mod.rs and effects/mod.rs).
        let entry_text = read_file(&entry_path);
        let mut entry_sources = SourceMap::new();
        let entry_file = entry_sources.add(entry_path.clone(), entry_text.clone());
        let (entry_tokens, _c, entry_lex_diags) = Lexer::new(&entry_text, entry_file).tokenize();
        assert!(!entry_lex_diags.has_any(), "lex error on entry re-parse");
        let (entry_module, entry_parse_diags) = parse_module(&entry_tokens, entry_file);
        assert!(
            !entry_parse_diags.has_any(),
            "parse error on entry re-parse"
        );
        let entry_stmts: Vec<Stmt> = entry_module
            .items
            .into_iter()
            .filter_map(|item| match item {
                Item::Stmt(s) => Some(s),
                Item::Decl(_) => None,
            })
            .collect();

        let sources = Arc::new(sources);
        let mut diagnostics = DiagnosticBag::new();
        let mut program = build_program_skeleton(modules, Arc::clone(&sources), &mut diagnostics);
        // D-MOD-04/SPEC §13 "the scope is the whole file": merges the entry file's own
        // top-level assignments consisting only of literals (module-level constants in all
        // but name) into doctest's scope (see the comment on collect_doctest_pseudo_consts).
        for (name, value) in pseudo_consts {
            program.consts.entry(name).or_insert(value);
        }
        check_program(&mut program, &entry_stmts, &mut diagnostics);

        let typecheck_ok = diagnostics.is_empty();
        (fences, program, typecheck_ok)
    }

    fn run_test_case(dir: &Path) -> (Vec<BlockResult>, i32) {
        let (fences, program, typecheck_ok) = load_program_and_fences(dir);
        let results = run_all_fences(&fences, &program);
        let any_fail = results
            .iter()
            .any(|r| matches!(r.outcome, Outcome::Fail(_)));
        let exit_code = i32::from(!typecheck_ok || any_fail);
        (results, exit_code)
    }

    /// Reproduces the doc-fence portion of `ybm check` (type-checking only, no execution,
    /// SPEC §13/§1). fmt's read-only diff / `--apply` write is the driver's responsibility
    /// and out of this test's scope — `check_vs_test_command_difference`'s main point is
    /// confirming that "check does not execute, and so cannot detect runtime
    /// inconsistencies" (see the comment at the top of entry_main.ybm), and it does not
    /// depend on fmt's result.
    fn run_check_case(dir: &Path) -> i32 {
        let (fences, program, typecheck_ok) = load_program_and_fences(dir);
        if !typecheck_ok {
            return 1;
        }
        for fence in &fences {
            if typecheck_fence_only(fence, &program).is_some() {
                return 1;
            }
        }
        let _ = program;
        0
    }

    fn assert_case_matches(actual: &[BlockResult], actual_exit: i32, expected: &ExpectedCase) {
        assert_eq!(actual_exit, expected.exit_code, "exit code mismatch");
        let mut actual_sorted: Vec<(u32, String, Option<String>)> = actual
            .iter()
            .map(|r| match &r.outcome {
                Outcome::Pass => (r.line, "pass".to_owned(), None),
                Outcome::Fail(diag) => (r.line, "fail".to_owned(), Some(diag.code.to_string())),
            })
            .collect();
        actual_sorted.sort_by_key(|(line, ..)| *line);
        let mut expected_sorted: Vec<(u32, String, Option<String>)> = expected
            .doc_blocks
            .iter()
            .map(|b| (b.line, b.result.clone(), b.code.clone()))
            .collect();
        expected_sorted.sort_by_key(|(line, ..)| *line);
        assert_eq!(actual_sorted, expected_sorted, "doc_blocks mismatch");
    }

    /// Shared processing that verifies one directory under samples/doctest/. For each
    /// `[[case]]` in `expected.toml`, this routes through either the check or test path
    /// depending on `cmd`, and cross-checks doc_blocks/exit code. The `test` path requires
    /// executing `assert` (`crate::stdlib::builtins::assert_bare`/`assert_with_message`) or
    /// something like `.parse_int()`/`.to_upper()` (`crate::stdlib::primitives`).
    fn verify_sample_dir(name: &str) {
        let dir = sample_dir(name);
        let expected_text = read_file(&dir.join("expected.toml"));
        let cases = parse_expected_toml(&expected_text);
        assert!(
            !cases.is_empty(),
            "{name}: no [[case]] found in expected.toml"
        );
        for case in &cases {
            match case.cmd.as_str() {
                "test" => {
                    let (results, exit_code) = run_test_case(&dir);
                    assert_case_matches(&results, exit_code, case);
                }
                "check" => {
                    let exit_code = run_check_case(&dir);
                    assert_eq!(
                        exit_code, case.exit_code,
                        "{name}: check exit code mismatch"
                    );
                    assert!(
                        case.doc_blocks.is_empty(),
                        "{name}: assumes a design where check reports no doc_blocks"
                    );
                }
                other => panic!("{name}: unknown cmd: {other}"),
            }
        }
    }

    #[test]
    fn check_vs_test_command_difference() {
        verify_sample_dir("check_vs_test_command_difference");
    }

    #[test]
    fn err_result_propagation_in_block() {
        verify_sample_dir("err_result_propagation_in_block");
    }

    #[test]
    fn failing_assert_and_report_line() {
        verify_sample_dir("failing_assert_and_report_line");
    }

    #[test]
    fn passing_multiple_blocks_same_declaration() {
        verify_sample_dir("passing_multiple_blocks_same_declaration");
    }

    #[test]
    fn scope_is_whole_file_incl_module() {
        verify_sample_dir("scope_is_whole_file_incl_module");
    }

    #[test]
    fn target_declarations_struct_enum_const() {
        verify_sample_dir("target_declarations_struct_enum_const");
    }

    // ---- Auxiliary tests independent of stdlib ----
    //
    // Every one of the 6 tests above depends on the execution of `assert(..)` (a stdlib
    // built-in function). To verify doctest's own logic (extraction, independent execution,
    // pass/fail determination, D-DOC-05's line-number mapping) independently of stdlib's correctness,
    // the auxiliary tests below verify directly, using minimal fences that never use any
    // stdlib call such as `assert`/`print`.

    fn program_from_source(src: &str) -> Program {
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("entry_main.ybm"), src.to_owned());
        let (tokens, comments, lex_diags) = Lexer::new(src, file).tokenize();
        assert!(
            !lex_diags.has_any(),
            "lex error: {:?}",
            lex_diags.into_vec()
        );
        let (mut module, parse_diags) = parse_module(&tokens, file);
        assert!(
            !parse_diags.has_any(),
            "parse error: {:?}",
            parse_diags.into_vec()
        );
        attach_comments(&mut module, comments);

        // This helper is dedicated to tests that require only "the declarations the fence
        // itself calls," on the assumption that the entry's top-level executable statements
        // never appear (an empty slice is passed to check_program) — even if an
        // `Item::Stmt` exists, it may be ignored here.
        let sources = Arc::new(sources);
        let mut diagnostics = DiagnosticBag::new();
        let mut program =
            build_program_skeleton(vec![module], Arc::clone(&sources), &mut diagnostics);
        assert!(
            !diagnostics.has_any(),
            "module resolve error: {:?}",
            diagnostics.into_vec()
        );
        let mut diagnostics = DiagnosticBag::new();
        check_program(&mut program, &[], &mut diagnostics);
        assert!(
            !diagnostics.has_any(),
            "type check error: {:?}",
            diagnostics.into_vec()
        );
        program
    }

    fn single_fence(module_src: &str) -> (DocFence, Program) {
        let program = program_from_source(module_src);
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("entry_main.ybm"), module_src.to_owned());
        let (tokens, comments, _lex_diags) = Lexer::new(module_src, file).tokenize();
        let (mut module, _parse_diags) = parse_module(&tokens, file);
        attach_comments(&mut module, comments);
        let fences = collect_fences(&[module]);
        let Some(fence) = fences.into_iter().next() else {
            panic!("not a single fence was found")
        };
        (fence, program)
    }

    #[test]
    fn pass_path_does_not_need_assert() {
        // A fence consisting only of assignment statements, using neither assert nor print
        // — verifies D-DOC-04's "pass if it can be run to completion as an independent
        // program" without depending on stdlib's `assert`.
        let src = "## An addition example.\n##\n## ```\n## x = 1 + 2\n## ```\ndef noop(): void\n    return\n";
        let (fence, program) = single_fence(src);
        let result = super::run_fence(&fence, &program);
        assert!(matches!(result.outcome, Outcome::Pass));
        assert_eq!(result.line, fence.body_start_line);
    }

    #[test]
    fn toplevel_err_propagation_fails_without_stdlib() {
        // Since the path where a user-defined function directly returns an Err depends on
        // stdlib not at all, this verifies the E6005 fail determination and D-DOC-05's line-number
        // mapping independently of stdlib's implementation.
        let src = "\
## Always fails.
##
## ```
## y = always_fails()?
## ```
def always_fails(): Result[int, Error]
    return Err(Error(kind: \"decode\", message: \"boom\", cause: None))
";
        let (fence, program) = single_fence(src);
        let result = super::run_fence(&fence, &program);
        match result.outcome {
            Outcome::Fail(diag) => {
                assert_eq!(
                    diag.code,
                    crate::diagnostics::ErrorCode::TopLevelErrPropagation
                );
                // Line 1 of the fence body (`y = always_fails()?`) is exactly
                // body_start_line.
                assert_eq!(result.line, fence.body_start_line);
            }
            Outcome::Pass => panic!("should have failed due to Err propagation"),
        }
    }

    // The case of a multi-line fence, "the line in a fail report is not the start of the
    // block but the real file line of the statement that actually failed (D-DOC-05)," is
    // already verified by `samples/doctest/failing_assert_and_report_line` (via
    // `verify_sample_dir`, a test earlier in this file).
    //
    // The concern that the `NodeId`s the dedicated fence Parser renumbers could numerically
    // collide with a real declaration body's `NodeId`s (see the comment at the top of the
    // old `run_fence`) has now been resolved by assigning a safe start offset via
    // `safe_fence_id_base` — the next test,
    // `fence_larger_than_declaration_does_not_corrupt_resolutions`, is the regression test
    // that verifies this.

    #[test]
    fn fence_larger_than_declaration_does_not_corrupt_resolutions() {
        // The total number of `NodeId`s the real declaration (the `Point(x: 1)` `Call`
        // expression in `make_point`'s body, `Resolutions::call_kind` is
        // `CallKind::StructInit`) consumes is quite small (1 struct + 1 function + a
        // handful of expressions in its body, measured at 0 through 4, 5 in total). The
        // fence body stacks up far more `Call` expressions than that (calling
        // `make_point()` 10 times, `CallKind::FunctionCall`) — without the start offset
        // from `safe_fence_id_base`, the fence's dedicated Parser would renumber these
        // `Call` expressions starting from 0 — measured, the `Call` expression of the 2nd
        // `make_point()` call ends up with id=3, numerically colliding with the real
        // declaration side's `Point(x: 1)` (also id=3). If `typecheck_fence` overwrites
        // that entry with `FunctionCall`, then at `make_point()`'s execution time, the
        // inner `Point(x: 1)` gets misinterpreted as "a top-level function call named
        // `Point`," `program.functions.get("Point")` fails to find it, and it panics via
        // `unreachable!()` (confirmed to actually reproduce this by temporarily disabling
        // `safe_fence_id_base`, and confirmed that this test correctly detects it). This is
        // a regression test confirming that this collision does not actually occur, and
        // that execution completes correctly to the end.
        let filler: String = (0..10)
            .map(|_| "## _ = make_point()\n".to_owned())
            .collect();
        let src = format!(
            "## Even a large fence doesn't collide on NodeId.\n##\n## ```\n{filler}## p = make_point()\n## assert(p.x == 1)\n## ```\nstruct Point\n    x: int\n\ndef make_point(): Point\n    return Point(x: 1)\n"
        );
        let (fence, program) = single_fence(&src);
        let result = super::run_fence(&fence, &program);
        match result.outcome {
            Outcome::Pass => {}
            Outcome::Fail(diag) => {
                panic!("suspected NodeId collision, fence execution failed: {diag:?}")
            }
        }
    }

    #[test]
    fn lang_tagged_fence_is_not_a_doctest_target() {
        // D-DOC-01: a fence with a language tag is excluded from collect_fences's targets.
        let src = "## An output example.\n##\n## ```json\n## {\"a\": 1}\n## ```\ndef f(): int\n    return 1\n";
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("entry_main.ybm"), src.to_owned());
        let (tokens, comments, _lex_diags) = Lexer::new(src, file).tokenize();
        let (mut module, _parse_diags) = parse_module(&tokens, file);
        attach_comments(&mut module, comments);
        assert!(collect_fences(&[module]).is_empty());
    }

    #[test]
    fn module_level_const_doc_comment_is_collected() {
        // D-DOC-03: the doc_comment of a module-level constant (an entry-side top-level
        // assignment that looks the same on the surface) is also a collection target —
        // directly verifies the design (collect_fences) of going through &[Module] rather
        // than Program, since build_program_skeleton discards Item::Stmt.
        let src =
            "## An approximation of pi.\n##\n## ```\n## y = pi_approx\n## ```\npi_approx = 3.14\n";
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("entry_main.ybm"), src.to_owned());
        let (tokens, comments, _lex_diags) = Lexer::new(src, file).tokenize();
        let (mut module, _parse_diags) = parse_module(&tokens, file);
        attach_comments(&mut module, comments);
        let fences = collect_fences(&[module]);
        assert_eq!(fences.len(), 1);
        assert_eq!(fences[0].raw_text, "y = pi_approx");
    }
}
