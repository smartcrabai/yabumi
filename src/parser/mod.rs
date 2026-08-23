//! Recursive descent parser core, the `Parser` struct, and the error recovery policy (ARCHITECTURE.md §2.1).
//! The parser's own recursive descent + Pratt expression parsing lives in expr.rs, statements/if/match/blocks
//! in stmt.rs, def/struct/enum/constants in decl.rs, patterns in pattern.rs, and type annotations in ty_ann.rs
//! (all as `impl` blocks on the same `Parser` type -- since these are child modules of the `parser` module,
//! they can access its private fields).
//!
//! # Error recovery policy (shared by this file and all child modules)
//!
//! None of the `parse_*` functions return `Result` (the return type is the AST type itself, which Unit0
//! already fixed). When a syntax error is detected, a `Diagnostic` is pushed onto `self.diagnostics`, and
//! processing always **continues by constructing some plausible value** (in the spirit of D-CLI-03's
//! "collect everything" -- applying globally the same idea that ARCHITECTURE.md §5.6 states as the
//! recovery policy for a bare `?` inside par). To avoid cascading diagnostics, wherever an "unexpected
//! token" is detected, control resumes only after skipping ahead to a safe restart point via
//! [`Parser::skip_to_sync_point`].

pub mod comment_attach;
pub mod decl;
pub mod expr;
pub mod pattern;
pub mod stmt;
pub mod ty_ann;

use crate::ast::{Module, NodeId};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, FileId, Position, Span};
use crate::lexer::{Token, TokenKind};

/// Recursion depth cap for the parser's own recursive descent + Pratt expression parsing
/// (R4 decision, §5.11/§8). Like the evaluator's `MAX_CALL_DEPTH`, this is an explicit counter
/// so we never rely at all on Rust's native call-stack limit. Since a single tier of Pratt
/// expression parsing tends to consume more stack frames than a single evaluator call, we pick
/// a value smaller than the evaluator's threshold (adjustable based on measurement).
const MAX_PARSE_DEPTH: u32 = 2_000;

pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    /// Counter used to allocate `ast::NodeId` values (see the "allocation policy" in ast/mod.rs).
    next_id: u32,
    /// R4 decision: incremented/decremented every time we enter nested parsing of an
    /// expression, literal, or parenthesis.
    depth: u32,
    /// D-PAR-03 (bare `?` forbidden inside a par arm). True while parsing the elements /
    /// lambda body of `par`/`par_map`/`par_each`. Cleared upon entering a new `Lambda`'s
    /// body (§5.6).
    bare_question_forbidden: bool,
    file: FileId,
    diagnostics: DiagnosticBag,
}

impl<'a> Parser<'a> {
    #[must_use]
    pub fn new(tokens: &'a [Token], file: FileId) -> Self {
        Self::with_start_id(tokens, file, 0)
    }

    /// Same as `new`, but starts the `NodeId` allocation counter at `start_id` instead of 0.
    /// Used to separate the `NodeId`s produced by multiple independent `Parser` instances into
    /// non-overlapping ranges after the fact (by having the caller choose an appropriate
    /// `start_id`) -- `parse_module_with_offset` (at the end of this file) is the sole route
    /// that uses this, and doctest (`src/doctest/mod.rs`) uses it to parse fence bodies.
    #[must_use]
    pub fn with_start_id(tokens: &'a [Token], file: FileId, start_id: u32) -> Self {
        Self {
            tokens,
            pos: 0,
            next_id: start_id,
            depth: 0,
            bare_question_forbidden: false,
            file,
            diagnostics: DiagnosticBag::new(),
        }
    }

    fn next_node_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    /// R4 decision: call this at the entry point of nested parsing of an expression, literal,
    /// or parenthesis. Exceeding the threshold reuses the existing E0502 (the general syntax
    /// error code) -- no new diagnostic code is added.
    ///
    /// Implemented as a plain increment/decrement pair (`depth_enter`/`depth_exit`) rather than
    /// an RAII guard like `ParseDepthGuard` (a decision specific to this parser's implementation
    /// -- the design originally had `enter_nesting` return a guard, but since the guard's `Ok`
    /// arm held `&mut self`, trying to use `self` in the `Err` arm produced a borrow error. This
    /// parser never uses Rust's `?` for early return, and every `parse_*` function is guaranteed
    /// to terminate normally, so depth can be managed safely without relying on Drop as long as
    /// the caller reliably calls the pair together). On exceeding the depth, `self.depth` is
    /// promptly restored (undoing the speculative increment) -- otherwise, once the threshold is
    /// exceeded even once, `depth` would stay permanently poisoned through to the end of the
    /// file, and even unrelated shallow nesting would keep failing forever.
    ///
    /// Callers must follow the convention "if `depth_enter` returns true, call the matching
    /// `depth_exit`" (when it returns false on exceeding the threshold, do not call
    /// `depth_exit` -- the speculative increment has already been undone by this function
    /// itself).
    fn depth_enter(&mut self, span: Span) -> bool {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            self.push_diag(
                ErrorCode::UnexpectedToken,
                span,
                "expression nesting is too deep",
            );
            false
        } else {
            true
        }
    }

    fn depth_exit(&mut self) {
        self.depth -= 1;
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    /// The token kind at the current position (a lightweight reference that takes no ownership).
    fn peek_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    /// The token kind `offset` tokens ahead (0 = current position). `None` if out of range.
    fn peek_kind_at(&self, offset: usize) -> Option<&TokenKind> {
        self.pos
            .checked_add(offset)
            .and_then(|i| self.tokens.get(i))
            .map(|t| &t.kind)
    }

    /// The `Span` of the token at the current position. Since the token stream is always
    /// terminated with `Eof`, this is normally always `Some`, but even in the unlikely case of
    /// an empty slice (an abnormal case for an f-string expression fragment), it does not panic
    /// and instead returns a dummy `Span` pointing at the start of the file.
    fn current_span(&self) -> Span {
        self.peek().map_or(
            Span {
                file: self.file,
                start: Position { line: 1, col: 1 },
                end: Position { line: 1, col: 1 },
            },
            |t| t.span,
        )
    }

    /// The `Span` of the most recently consumed token (i.e. advanced past via `bump`). Falls
    /// back to `current_span` if nothing has been consumed yet (`pos == 0`). A general-purpose
    /// helper used when building a syntax element's end position as "the end of the last
    /// consumed token".
    fn previous_span(&self) -> Span {
        self.pos
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .map_or_else(|| self.current_span(), |t| t.span)
    }

    /// Skips consecutive `Newline`s at the current position (since lexing treats comment-only
    /// lines and blank lines transparently, this is normally 0 or 1, but multiple are also
    /// tolerated as a safety net after error recovery).
    fn skip_blank_newlines(&mut self) {
        while matches!(self.peek_kind(), Some(TokenKind::Newline)) {
            self.bump();
        }
    }

    fn push_diag(&mut self, code: ErrorCode, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            code,
            span,
            message: message.into(),
        });
    }

    /// If the current token matches `kind`, consumes it and returns true. If it does not
    /// match, pushes an E0502 that includes `what` and returns false (the token is not
    /// consumed).
    fn expect(&mut self, kind: &TokenKind, what: &str) -> bool {
        if self.peek_kind() == Some(kind) {
            self.bump();
            true
        } else {
            let span = self.current_span();
            self.push_diag(
                ErrorCode::UnexpectedToken,
                span,
                format!("expected {what} but did not find it"),
            );
            false
        }
    }

    /// If the current token is an identifier, consumes it and returns the name. Otherwise
    /// pushes an E0502 and returns an empty-string dummy identifier (so the caller can keep
    /// going).
    fn expect_ident(&mut self, what: &str) -> std::sync::Arc<str> {
        if let Some(TokenKind::Ident(name)) = self.peek_kind() {
            let name = std::sync::Arc::clone(name);
            self.bump();
            name
        } else {
            let span = self.current_span();
            self.push_diag(
                ErrorCode::UnexpectedToken,
                span,
                format!("expected {what} (an identifier) but did not find it"),
            );
            std::sync::Arc::from("")
        }
    }
    /// Parses a comma-separated list of items up to (but not consuming) `closing`, allowing
    /// one trailing comma (`item (`,` item)* `,`?`). Shared by every list-literal / argument
    /// list / parameter list site in expr.rs, decl.rs, ty_ann.rs, and pattern.rs (this loop
    /// used to be copy-pasted at each of them). The opening bracket has already been
    /// consumed by the caller; the caller also consumes `closing` afterwards (usually via
    /// `expect`, so that a missing closer gets its own diagnostic).
    fn parse_comma_separated<T>(
        &mut self,
        closing: &TokenKind,
        mut one: impl FnMut(&mut Self) -> T,
    ) -> Vec<T> {
        let mut items = Vec::new();
        if self.peek_kind() != Some(closing) {
            items.push(one(self));
            items.extend(self.parse_comma_separated_tail(closing, one));
        }
        items
    }

    /// The continuation of [`Parser::parse_comma_separated`] for a list whose first item has
    /// already been parsed by the caller (the current position is the `,` or `closing`
    /// right after it). Used where the first element must be parsed separately, e.g. to
    /// disambiguate a dict literal from a set literal.
    fn parse_comma_separated_tail<T>(
        &mut self,
        closing: &TokenKind,
        mut one: impl FnMut(&mut Self) -> T,
    ) -> Vec<T> {
        let mut items = Vec::new();
        while self.peek_kind() == Some(&TokenKind::Comma) {
            self.bump();
            if self.peek_kind() == Some(closing) {
                break;
            }
            items.push(one(self));
        }
        items
    }

    /// Determines whether the current position has the shape of a `Newline` immediately
    /// followed by an `Indent` (i.e. whether an indented multi-statement block follows)
    /// (D-SYN-04/D-SYN-10). In a paren-suppression context like `.map((x) => if ... else ...)`,
    /// no Newline/Indent is ever generated, so this is always false there and the caller parses
    /// it as a single expression.
    fn is_indented_block_ahead(&self) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::Newline))
            && matches!(self.peek_kind_at(1), Some(TokenKind::Indent))
    }

    /// Used to decide whether a match-arm sequence continues (when there is no Indent/Dedent
    /// due to a paren-suppression context): whether the current token is valid as the start of
    /// a pattern. Covers the start tokens for D-SYN-06's pattern vocabulary (literal / bare
    /// identifier / wildcard / enum variant destructuring / tuple destructuring).
    fn at_pattern_start(&self) -> bool {
        matches!(
            self.peek_kind(),
            Some(
                TokenKind::IntLiteral(_)
                    | TokenKind::FloatLiteral(_)
                    | TokenKind::StringLiteral(_)
                    | TokenKind::True
                    | TokenKind::False
                    | TokenKind::Underscore
                    | TokenKind::Ident(_)
                    | TokenKind::LParen
                    | TokenKind::Minus
            )
        )
    }

    /// Recovery handling for when an unexpected token is encountered: skips tokens while
    /// counting matching `Indent`/`Dedent` pairs, and stops right before the `Dedent` that ends
    /// the current nesting (the block the caller is syntactically trying to close), or at
    /// `Eof` (neither is consumed). This prevents a single root cause -- such as a missing
    /// `else` on an `if` -- from cascading into multiple diagnostics (D-CLI-03 calls for
    /// collecting everything, but a pointless proliferation of secondary diagnostics is still
    /// undesirable).
    fn skip_to_sync_point(&mut self) {
        let mut depth: i32 = 0;
        loop {
            match self.peek_kind() {
                None | Some(TokenKind::Eof) => return,
                Some(TokenKind::Dedent) => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.bump();
                }
                Some(TokenKind::Indent) => {
                    depth += 1;
                    self.bump();
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }
}

/// Builds a `Span` combining `start`'s start position with `end`'s end position (assumes the
/// same file -- since the parser always processes a single file's token stream, `file` is the
/// same regardless of which one it is taken from).
fn span_between(start: Span, end: Span) -> Span {
    Span {
        file: start.file,
        start: start.start,
        end: end.end,
    }
}

/// Entry point. Converts one file's worth of a token stream into a `Module`.
///
/// The shebang has already been stripped at the character level by lexing (see the comment in
/// lexer/mod.rs), so here we only determine whether "the start of the token stream is `module`
/// alone" (D-LEX-08/09). If it is well-formed (immediately followed by Newline/Eof), only the
/// `Module` token is consumed and the flag is set. If other tokens follow immediately, as in
/// `module foo` (a violation of D-LEX-08's "bare keyword with no name" requirement), E5001 is
/// reported directly right here -- since `Module` holds only the single bit
/// `is_module_directive` (ast/decl.rs, a type already fixed in Unit3), and the subsequent phase
/// (module_resolve) never looks at the token stream again, information detectable only here
/// cannot be deferred to a later stage (noted in the report as something to reconcile).
pub fn parse_module(tokens: &[Token], file: FileId) -> (Module, DiagnosticBag) {
    let (module, diagnostics, _next_id) = parse_module_with_offset(tokens, file, 0);
    (module, diagnostics)
}

/// Same as `parse_module`, but starts the `NodeId` allocation counter at `start_id`, and
/// returns as the third element of the return value the final counter value at the point
/// parsing completes (i.e. one past the last `NodeId` consumed by this call). Used to prevent
/// the `NodeId`s generated by multiple independent `Parser` calls (such as each `##` fence in a
/// doctest) from overlapping with ranges already used by actual declarations or other calls
/// (see `safe_fence_id_base` in `src/doctest/mod.rs` -- since `NodeId` allocation is
/// deterministic for the same token stream, if all you want to know is how many were consumed
/// starting from some `start_id`, this return value can be reused directly as the next
/// `start_id`).
pub fn parse_module_with_offset(
    tokens: &[Token],
    file: FileId,
    start_id: u32,
) -> (Module, DiagnosticBag, u32) {
    let mut parser = Parser::with_start_id(tokens, file, start_id);

    let is_module_directive = if parser.peek_kind() == Some(&TokenKind::Module) {
        let directive_span = parser.current_span();
        parser.bump();
        let well_formed = matches!(
            parser.peek_kind(),
            Some(TokenKind::Newline | TokenKind::Eof)
        );
        if !well_formed {
            let bad_span = parser.current_span();
            parser.push_diag(
                ErrorCode::ModuleDirectiveMalformed,
                span_between(directive_span, bad_span),
                "a module directive must be the bare keyword with no name (D-LEX-08)".to_owned(),
            );
            // Skip the rest of this line and safely resume subsequent declaration parsing.
            while !matches!(
                parser.peek_kind(),
                None | Some(TokenKind::Newline | TokenKind::Eof)
            ) {
                parser.bump();
            }
        }
        true
    } else {
        false
    };

    let items = parser.parse_items();

    (
        Module {
            file,
            is_module_directive,
            items,
            trailing_comments: Vec::new(),
        },
        parser.diagnostics,
        parser.next_id,
    )
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

    /// Lexes then parses the source text, and returns a `Module` together with a
    /// `Vec<Diagnostic>` that combines the diagnostics from both phases sorted ascending by
    /// `file:line:col` (a simplified pipeline for testing Unit4 in isolation -- the actual
    /// driver is Unit17's responsibility).
    fn lex_and_parse(src: &str) -> (Module, Vec<Diagnostic>) {
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("test.ybm"), src.to_owned());
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        let (module, parse_diag) = parse_module(&tokens, file);
        let mut combined = lex_diag.into_sorted(&sources);
        combined.extend(parse_diag.into_sorted(&sources));
        (module, combined)
    }

    fn collect_ybm_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => panic!("failed to walk directory {}: {e}", dir.display()),
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.is_dir() {
                collect_ybm_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "ybm") {
                out.push(path);
            }
        }
    }

    fn all_ok_sample_files() -> Vec<PathBuf> {
        let root = sample_path("samples/ok");
        let mut out = Vec::new();
        collect_ybm_files(&root, &mut out);
        out.sort();
        out
    }

    /// Unit4 completion condition: every `.ybm` across the 40 directories under `samples/ok/`
    /// can be parsed into a `Module` (zero diagnostics across both the lexing and parsing
    /// phases).
    #[test]
    fn all_samples_ok_files_parse_without_diagnostics() {
        let files = all_ok_sample_files();
        assert_eq!(
            files.len(),
            62,
            "the number of .ybm files under samples/ok/ should match the expected count (62) (detects unexpected additions/removals)"
        );

        let samples_root = sample_path("samples");
        let mut failures = Vec::new();
        for path in &files {
            let rel = path
                .strip_prefix(&samples_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let src = match fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => panic!("failed to read {}: {e}", path.display()),
            };
            let (module, diags) = lex_and_parse(&src);
            if !diags.is_empty() {
                failures.push(format!("{rel}: {diags:?}"));
            } else if module.items.is_empty() {
                failures.push(format!("{rel}: parse result's items is empty (unexpected)"));
            }
        }
        assert!(
            failures.is_empty(),
            "the following samples/ok/ files failed to parse with zero diagnostics:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn bare_module_directive_alone_is_well_formed() {
        let (module, diags) = lex_and_parse("module\n\ndef f(): int\n    return 1\n");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(module.is_module_directive);
        assert_eq!(module.items.len(), 1);
    }

    #[test]
    fn module_directive_with_trailing_content_is_e5001() {
        let (module, diags) = lex_and_parse("module foo\n");
        assert!(module.is_module_directive);
        assert!(
            diags
                .iter()
                .any(|d| d.code == ErrorCode::ModuleDirectiveMalformed),
            "`module foo` should report E5001 as a D-LEX-08 violation: {diags:?}"
        );
    }

    #[test]
    fn entry_file_without_module_directive_is_not_flagged() {
        let (module, diags) = lex_and_parse("x = 1 + 1\n");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(!module.is_module_directive);
    }

    #[test]
    fn elif_is_not_supported_and_reports_e0502() {
        let src = read_sample("samples/err/static/2_syntax_errors/entry_elif_not_supported.ybm");
        let (_module, diags) = lex_and_parse(&src);
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "`elif` should become E0502: {diags:?}"
        );
    }

    #[test]
    fn indentation_mismatch_sample_reports_e0501() {
        let src = read_sample("samples/err/static/2_syntax_errors/entry_indentation_mismatch.ybm");
        let (_module, diags) = lex_and_parse(&src);
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::IndentMismatch),
            "the lexer's E0501 should be propagating: {diags:?}"
        );
    }

    #[test]
    fn pipe_missing_placeholder_sample_reports_e0503() {
        let src =
            read_sample("samples/err/static/2_syntax_errors/entry_pipe_missing_placeholder.ybm");
        let (_module, diags) = lex_and_parse(&src);
        assert!(
            diags
                .iter()
                .any(|d| d.code == ErrorCode::PipePlaceholderMissing),
            "a missing `_` placeholder should become E0503: {diags:?}"
        );
    }

    #[test]
    fn par_literal_bare_question_reports_e0502() {
        let src = read_sample(
            "samples/err/static/9_par_branch_bare_question_operator/entry_par_literal_bare_question.ybm",
        );
        let (_module, diags) = lex_and_parse(&src);
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "a bare `?` inside a `par[...]` branch should become E0502 (D-PAR-03): {diags:?}"
        );
    }

    #[test]
    fn par_map_lambda_question_is_deferred_until_receiver_resolution() {
        let src = read_sample(
            "samples/err/static/9_par_branch_bare_question_operator/entry_par_map_lambda_bare_question.ybm",
        );
        let (_module, diags) = lex_and_parse(&src);
        assert!(
            !diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "the parser cannot know whether par_map is a builtin list method: {diags:?}"
        );
    }

    /// D-PAR-03 also extends to a pipe's trailing stage `?` (SPEC §6.3): because `?`
    /// goes through the same Rust-side early-return mechanism as `ExprKind::Question`
    /// (ARCHITECTURE.md §5.6's `Flow::Return`) applied to the stage's result, when a
    /// `par [...]` branch's expression is a pipe and its final stage carries a `?`, it is
    /// forbidden for the same reason (since a pipe's `?` goes through the separate route
    /// `parse_pipe_stage`, it cannot be caught solely by `parse_postfix`'s check for
    /// `ExprKind::Question` -- so it is verified separately).
    #[test]
    fn par_literal_pipe_stage_bare_question_reports_e0502() {
        let src = "def f(s: str): Result[int, Error]\n    return s.parse_int()\n\nresults = par [\"1\" |> f?, f(\"2\")]\n";
        let (_module, diags) = lex_and_parse(src);
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "a pipe stage's trailing `?` inside a `par[...]` branch should also become E0502 (D-PAR-03): {diags:?}"
        );
    }

    /// A method's receiver type decides whether `par_map` is the builtin parallel HOF.
    #[test]
    fn par_map_pipe_question_is_deferred_until_receiver_resolution() {
        let src = "def f(s: str): Result[int, Error]\n    return s.parse_int()\n\ninputs = [\"1\", \"2\"]\nresults = inputs.par_map((s) => s |> f?)\n";
        let (_module, diags) = lex_and_parse(src);
        assert!(
            !diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "receiver resolution belongs to type checking: {diags:?}"
        );
    }

    /// Control check: an ordinary pipe stage's trailing `?` outside of
    /// par/par_map/par_each is not subject to D-PAR-03 and produces no diagnostic
    /// (symmetric with the existing D-PAR-03 target tests).
    #[test]
    fn ordinary_pipe_stage_bare_question_outside_par_is_allowed() {
        let src =
            "def f(s: str): Result[int, Error]\n    return s.parse_int()\n\nresult = \"1\" |> f?\n";
        let (_module, diags) = lex_and_parse(src);
        assert!(
            !diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "a pipe stage's trailing `?` outside of par/par_map/par_each should not be subject to D-PAR-03: {diags:?}"
        );
    }

    #[test]
    fn ordinary_map_lambda_bare_question_is_allowed() {
        // D-PAR-03 is limited to `par`/`par_map`/`par_each`. A `?` in the lambda body
        // passed to an ordinary `.map` is not forbidden (the general rule from §5.6,
        // "cleared once parsing enters a new Lambda's body").
        let src = "def f(s: str): Result[int, Error]\n    return s.parse_int()\n\nresults = par [1, 2].map((x) => x)\nys = [\"1\"].map((s) => f(s)?)\n";
        let (_module, diags) = lex_and_parse(src);
        assert!(
            !diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "a `?` inside a lambda passed to an ordinary .map should not be subject to D-PAR-03: {diags:?}"
        );
    }

    /// The R4 decision (§5.11): on exceeding `MAX_PARSE_DEPTH`, E0502 must be reported
    /// and execution must end without panicking.
    ///
    /// In production, the runtime pipeline runs this parsing on a dedicated 64MiB-stack
    /// thread (ARCHITECTURE.md §4.5/§5.7). The default thread stack `cargo test` allocates
    /// per test is far smaller than that, so this test likewise runs on a dedicated thread
    /// with a large stack -- this avoids conflating a native stack overflow caused by the
    /// test harness's own constraint (a false positive) with what is actually being
    /// verified, namely that the parser's own R4 depth guard correctly reports E0502
    /// first.
    #[test]
    fn deeply_nested_parens_exceed_max_parse_depth_and_report_e0502() {
        let depth = 3_000;
        let mut src = String::from("x = ");
        for _ in 0..depth {
            src.push('(');
        }
        src.push('1');
        for _ in 0..depth {
            src.push(')');
        }
        src.push('\n');

        let builder = std::thread::Builder::new().stack_size(64 * 1024 * 1024);
        let spawned = builder.spawn(move || {
            let (_module, diags) = lex_and_parse(&src);
            diags
        });
        let handle = match spawned {
            Ok(h) => h,
            Err(e) => panic!("failed to spawn the test thread: {e}"),
        };
        let Ok(diags) = handle.join() else {
            panic!("the test thread panicked (an unexpected stack overflow, etc.)");
        };
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "exceeding MAX_PARSE_DEPTH should report E0502: diagnostic count={}",
            diags.len()
        );
    }

    /// Every `.ybm` under samples/fmt/ (fmt input/output fixtures) and samples/doctest/
    /// (doctest samples) should always be syntactically valid, since doc/ordinary comments
    /// are skipped at the lexing stage as `#`/`##` lines (the parser itself never looks at
    /// comments at all, see the comment at the top of this file). The fmt/doctest portion
    /// of satisfying this unit's instruction to "actually parse the 158 files under
    /// samples".
    #[test]
    fn all_samples_fmt_and_doctest_files_parse_without_diagnostics() {
        let mut files = Vec::new();
        collect_ybm_files(&sample_path("samples/fmt"), &mut files);
        collect_ybm_files(&sample_path("samples/doctest"), &mut files);
        files.sort();
        assert_eq!(
            files.len(),
            27,
            "the total number of .ybm files in samples/fmt (20) + samples/doctest (7) should match the expected count (27)"
        );

        let samples_root = sample_path("samples");
        let mut failures = Vec::new();
        for path in &files {
            let rel = path
                .strip_prefix(&samples_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let src = match fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => panic!("failed to read {}: {e}", path.display()),
            };
            let (_module, diags) = lex_and_parse(&src);
            if !diags.is_empty() {
                failures.push(format!("{rel}: {diags:?}"));
            }
        }
        assert!(
            failures.is_empty(),
            "the following files under samples/fmt or samples/doctest failed to parse with zero diagnostics:\n{}",
            failures.join("\n")
        );
    }

    /// Every `.ybm` under samples/err/lint/ and samples/err/runtime/ is syntactically
    /// correct (both lint warnings and runtime panics are detected by phases that come
    /// after syntax parsing), so, just like samples/ok/, they should parse with zero
    /// diagnostics.
    #[test]
    fn all_samples_err_lint_and_runtime_files_parse_without_diagnostics() {
        let mut files = Vec::new();
        collect_ybm_files(&sample_path("samples/err/lint"), &mut files);
        collect_ybm_files(&sample_path("samples/err/runtime"), &mut files);
        files.sort();
        assert_eq!(
            files.len(),
            19,
            "the total number of .ybm files in samples/err/lint (5) + samples/err/runtime (14) should match the expected count (19)"
        );

        let samples_root = sample_path("samples");
        let mut failures = Vec::new();
        for path in &files {
            let rel = path
                .strip_prefix(&samples_root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let src = match fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => panic!("failed to read {}: {e}", path.display()),
            };
            let (_module, diags) = lex_and_parse(&src);
            if !diags.is_empty() {
                failures.push(format!("{rel}: {diags:?}"));
            }
        }
        assert!(
            failures.is_empty(),
            "the following files under samples/err/lint or samples/err/runtime failed to parse with zero diagnostics:\n{}",
            failures.join("\n")
        );
    }

    /// A simplified parser (added in this review) that pulls just `entry` (the file
    /// name) and `diagnostics` (the expected code list) out of `expected.toml`'s
    /// `[[case]]` blocks. Adding a toml crate would require changing Cargo.toml (out of
    /// scope), so this is implemented by hand under the assumption that the format is
    /// regular (each field fits on one line, and string values contain no `"`).
    fn parse_expected_toml_cases(text: &str) -> Vec<(String, Vec<String>)> {
        let mut cases: Vec<(String, Vec<String>)> = Vec::new();
        let mut current_entry: Option<String> = None;
        let mut current_diags: Vec<String> = Vec::new();
        let mut in_case = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed == "[[case]]" {
                if in_case && let Some(entry) = current_entry.take() {
                    cases.push((entry, std::mem::take(&mut current_diags)));
                }
                in_case = true;
                current_entry = None;
                current_diags = Vec::new();
                continue;
            }
            if !in_case {
                continue;
            }
            let Some(eq_pos) = trimmed.find('=') else {
                continue;
            };
            let key = trimmed[..eq_pos].trim();
            let value = trimmed[eq_pos + 1..].trim();
            if key == "entry" {
                current_entry = extract_quoted_string(value);
            } else if key == "diagnostics" {
                current_diags = extract_quoted_string_list(value);
            }
        }
        if in_case && let Some(entry) = current_entry {
            cases.push((entry, current_diags));
        }
        cases
    }

    fn extract_quoted_string(s: &str) -> Option<String> {
        let start = s.find('"')?;
        let rest = &s[start + 1..];
        let end = rest.find('"')?;
        Some(rest[..end].to_owned())
    }

    fn extract_quoted_string_list(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find('"') {
            rest = &rest[start + 1..];
            let Some(end) = rest.find('"') else { break };
            out.push(rest[..end].to_owned());
            rest = &rest[end + 1..];
        }
        out
    }

    /// Determines, via D-DIAG-01's range division (E0000-0499 lexical, E0500-0999
    /// syntax), whether a code is one the lexing/parsing phases could report
    /// (`lex_and_parse` returns both phases' diagnostics combined, so they are treated
    /// alike here). E5001 (a malformed module directive) is a special case that
    /// parse_module itself detects right here per D-LEX-08/D-MOD-01 (see the comment at
    /// the top of mod.rs), and while it is not an E0xxx category, it is included here
    /// because the parser does actually report it.
    fn is_parser_reportable_code(code: &str) -> bool {
        if code == "E5001" {
            return true;
        }
        let Some(digits) = code.strip_prefix('E') else {
            return false;
        };
        let Ok(n) = digits.parse::<u32>() else {
            return false;
        };
        n < 1000
    }

    /// The list of directories under samples/err/static/ (sorted).
    fn list_err_static_dirs() -> Vec<PathBuf> {
        let root = sample_path("samples/err/static");
        let mut dirs: Vec<PathBuf> = Vec::new();
        match fs::read_dir(&root) {
            Ok(entries) => {
                for entry in entries {
                    let Ok(entry) = entry else { continue };
                    let path = entry.path();
                    if path.is_dir() {
                        dirs.push(path);
                    }
                }
            }
            Err(e) => panic!("failed to scan samples/err/static: {e}"),
        }
        dirs.sort();
        dirs
    }

    /// The list of `.ybm` files directly under `dir` (non-recursive, sorted).
    fn list_ybm_files_non_recursive(dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = Vec::new();
        match fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries {
                    let Ok(entry) = entry else { continue };
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "ybm") {
                        files.push(path);
                    }
                }
            }
            Err(e) => panic!("failed to read {}: {e}", dir.display()),
        }
        files.sort();
        files
    }

    /// Whether this is one of the 4 directories involving D-MOD-01 (auto-import). These
    /// expected.toml's `diagnostics` are "the results the full pipeline reports when
    /// checking the entry file, including modules in the same directory", and do not
    /// match the result of parsing the entry file alone (e.g.
    /// 10b_module_directive_malformed/entry_probe.ybm is syntactically valid on its own;
    /// the E5001 arises from the content of a different file in the same directory,
    /// mod_bad_directive.ybm).
    fn is_module_cross_file_static_dir(dir: &Path) -> bool {
        let dir_name = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        matches!(
            dir_name.as_str(),
            "10a_module_name_collision"
                | "10b_module_directive_malformed"
                | "10c_module_toplevel_statement_cascade"
                | "10d_entry_self_module_directive"
        )
    }

    /// Decides, for `file_name`, the expected diagnostic code list that its standalone
    /// parse result should report.
    fn expected_codes_for_file(
        file_name: &str,
        is_module_cross_file_dir: bool,
        cases: &[(String, Vec<String>)],
    ) -> Vec<String> {
        if is_module_cross_file_dir {
            // Only this is spelled out explicitly, since it is clear from its content
            // that mod_bad_directive.ybm's own first line `module foo` is a D-LEX-08
            // violation (E5001). Every other file (entry_*.ybm / mod_util.ybm /
            // mod_broken.ybm) is expected to be syntactically valid (zero diagnostics)
            // when parsed standalone -- E1001/E5002 are both the responsibility of a
            // later phase that detects across multiple files, and are out of scope for
            // Unit4.
            if file_name == "mod_bad_directive.ybm" {
                return vec!["E5001".to_owned()];
            }
            return Vec::new();
        }
        for (entry_name, diags_for_entry) in cases {
            if entry_name == file_name {
                return diags_for_entry.clone();
            }
        }
        Vec::new()
    }

    /// Parses one file, and records it into `failures` if it disagrees with the set of
    /// expected diagnostic codes.
    fn check_err_static_file(
        path: &Path,
        samples_root: &Path,
        cases: &[(String, Vec<String>)],
        is_module_cross_file_dir: bool,
        failures: &mut Vec<String>,
    ) {
        let file_name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let rel = path
            .strip_prefix(samples_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => panic!("failed to read {}: {e}", path.display()),
        };
        let (_module, diags) = lex_and_parse(&src);
        let expected_codes = expected_codes_for_file(&file_name, is_module_cross_file_dir, cases);
        let parser_codes: Vec<&String> = expected_codes
            .iter()
            .filter(|c| is_parser_reportable_code(c))
            .collect();

        if parser_codes.is_empty() {
            if !diags.is_empty() {
                failures.push(format!(
                    "{rel}: should be syntactically valid (expected codes {expected_codes:?} are exclusive to later phases such as types/effects/mutability/modules), but diagnostics were reported: {diags:?}"
                ));
            }
            return;
        }
        let has_match = parser_codes
            .iter()
            .any(|expected| diags.iter().any(|d| d.code.to_string() == **expected));
        if !has_match {
            failures.push(format!(
                "{rel}: none of the expected syntax diagnostics {parser_codes:?} were reported: actual={diags:?}"
            ));
        }
    }

    /// The most important task in this unit's instructions: actually parses every
    /// `.ybm` under samples/err/static/ (not only files explicitly named as entry, but
    /// also auto-imported `mod_*.ybm` files), and confirms it does not disagree with the
    /// diagnostic code set each `expected.toml` expects. If the expected codes include
    /// even one E0xxx/E5001 (a code the parser could report, per
    /// `is_parser_reportable_code`), that file should report a corresponding diagnostic at
    /// the syntax-parsing stage; if not (only codes exclusive to later phases such as
    /// types/effects/mutability/module-level statements), it should be syntactically
    /// valid and parse with zero diagnostics.
    #[test]
    fn err_static_samples_produce_expected_parser_diagnostic_category() {
        let dirs = list_err_static_dirs();
        assert_eq!(
            dirs.len(),
            19,
            "the number of directories under samples/err/static should match the expected count (19)"
        );

        let samples_root = sample_path("samples");
        let mut total_files_checked = 0usize;
        let mut failures = Vec::new();

        for dir in &dirs {
            let expected_path = dir.join("expected.toml");
            let expected_text = match fs::read_to_string(&expected_path) {
                Ok(s) => s,
                Err(e) => panic!("failed to read {}: {e}", expected_path.display()),
            };
            let cases = parse_expected_toml_cases(&expected_text);
            assert!(
                !cases.is_empty(),
                "{}: not a single [[case]] was found (the simplified toml parser may be inconsistent)",
                expected_path.display()
            );

            let ybm_files = list_ybm_files_non_recursive(dir);
            let is_module_cross_file_dir = is_module_cross_file_static_dir(dir);

            for path in &ybm_files {
                total_files_checked += 1;
                check_err_static_file(
                    path,
                    &samples_root,
                    &cases,
                    is_module_cross_file_dir,
                    &mut failures,
                );
            }
        }

        assert_eq!(
            total_files_checked, 50,
            "the total number of .ybm files under samples/err/static should match the expected count (50)"
        );
        assert!(
            failures.is_empty(),
            "files under samples/err/static that disagree with the expected syntax-parsing category:\n{}",
            failures.join("\n")
        );
    }
}
