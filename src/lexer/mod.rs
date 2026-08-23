//! The lexer itself (ARCHITECTURE.md §5.1). The indent stack, bracket depth, and
//! line-continuation lookahead.

pub mod comments;
pub mod cursor;
pub mod fstring;
pub mod token;

pub use token::{FStringPart, Token, TokenKind};

use self::comments::{CommentStream, RawComment};
use self::cursor::Cursor;
use self::fstring::scan_fstring;
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, FileId, Position, Span};
use std::cmp::Ordering;
use std::sync::Arc;

/// A lexer that advances an indentation state machine one logical line at a time (§5.1). A
/// logical line is a run of physical lines joined either "while a bracket is open (D-SYN-04)"
/// or by "method-chain continuation (D-SYN-05)".
pub struct Lexer<'src> {
    cursor: Cursor<'src>,
    file: FileId,
    /// Initial value `[0]`.
    indent_stack: Vec<u32>,
    /// +1 on `(` `[` `{`, -1 on the matching closing bracket.
    bracket_depth: u32,
    tokens: Vec<Token>,
    comments: CommentStream,
    diagnostics: DiagnosticBag,
}

/// The result of scanning the leading whitespace of a physical line (§5.1 step 1).
struct LeadingWhitespace {
    spaces: u32,
    has_tab: bool,
    tab_pos: Position,
    start: Position,
}

/// Determines whether the leading token of `text`, after shebang removal (D-LEX-09), is
/// `TokenKind::Module` (D-LEX-08). Used by module resolution to decide whether a
/// same-directory file is a module eligible for import.
#[must_use]
pub fn text_starts_with_module_keyword(text: &str) -> bool {
    // A throwaway `FileId` used only for this check. The generated `Token`s never escape this
    // function and their Span contents are never referenced either, so this does not need to
    // be an ID actually registered in a real `SourceMap`.
    let throwaway_file = FileId(0);
    let (tokens, _comments, _diagnostics) = Lexer::new(text, throwaway_file).tokenize();
    matches!(tokens.first().map(|t| &t.kind), Some(TokenKind::Module))
}

impl<'src> Lexer<'src> {
    #[must_use]
    pub fn new(text: &'src str, file: FileId) -> Self {
        Self {
            cursor: Cursor::new(text),
            file,
            indent_stack: vec![0],
            bracket_depth: 0,
            tokens: Vec::new(),
            comments: Vec::new(),
            diagnostics: DiagnosticBag::new(),
        }
    }

    /// Goes through shebang removal -> module-directive detection (only the judgment used for
    /// the `Module.is_module_directive` flag; the semantic checks for E5001/E5002 are done by
    /// the parser and later phases) -> the indentation algorithm proper from §5.1, and returns
    /// a `Vec<Token>` (terminated by `Eof`), the comment side stream, and the collected
    /// diagnostics.
    ///
    /// The `module` reserved word itself is generated as an ordinary token through the same
    /// path as any other reserved word, so "module directive detection" needs no extra branch
    /// in this function (whether the first line is a `Module` token is decided by
    /// `parser::parse_module`, §4.2).
    #[must_use]
    pub fn tokenize(mut self) -> (Vec<Token>, CommentStream, DiagnosticBag) {
        self.strip_shebang();
        self.run();
        (self.tokens, self.comments, self.diagnostics)
    }

    /// D-LEX-09: only when the first line starts with `#!` does it skip that entire line
    /// (including the newline).
    fn strip_shebang(&mut self) {
        if self.cursor.peek() == Some('#') && self.cursor.peek2() == Some('!') {
            while let Some(c) = self.cursor.peek() {
                if c == '\n' {
                    break;
                }
                self.cursor.bump();
            }
            if self.cursor.peek() == Some('\n') {
                self.cursor.bump();
            }
        }
    }

    /// The core of §5.1's physical-line-driven algorithm. While `at_line_start` is true, it
    /// performs steps 1-5 (tab detection -> pass-through-line judgment -> method-chain
    /// lookahead -> indent comparison); while false, it tokenizes the rest of the current
    /// physical line. Consuming a single newline is the only path back to the
    /// `at_line_start` state.
    #[expect(
        clippy::too_many_lines,
        reason = "the physical-line lexer state machine is clearer as one loop"
    )]
    fn run(&mut self) {
        let mut have_logical_line = false;
        let mut current_base_indent: u32 = 0;
        let mut at_line_start = true;
        let mut line_has_token = false;

        loop {
            if at_line_start {
                if self.cursor.is_eof() {
                    break;
                }
                let ws = self.scan_leading_whitespace();
                if ws.has_tab {
                    self.diagnostics.push(make_diag(
                        ErrorCode::TabCharacter,
                        self.file,
                        ws.tab_pos,
                        ws.tab_pos,
                        "a tab character is mixed into the indentation (D-SYN-01)".to_owned(),
                    ));
                    // Fatal: aborts lexing of the whole file without doing any further
                    // analysis (§5.1 step 1).
                    return;
                }
                line_has_token = false;
                match self.cursor.peek() {
                    None => break,
                    Some('\n') => {
                        self.cursor.bump();
                        continue;
                    }
                    Some('#') => {
                        self.consume_line_comment(false);
                        if self.cursor.peek() == Some('\n') {
                            self.cursor.bump();
                        }
                        continue;
                    }
                    Some(_) => {
                        if self.bracket_depth == 0 {
                            let is_continuation = have_logical_line
                                && ws.spaces > current_base_indent
                                && peek_starts_continuation(&self.cursor);
                            if !is_continuation {
                                self.handle_indent_transition(
                                    ws.spaces,
                                    have_logical_line,
                                    ws.start,
                                );
                                current_base_indent = ws.spaces;
                                have_logical_line = true;
                            }
                        }
                        at_line_start = false;
                    }
                }
            }
            if !at_line_start {
                if let Some(tab_position) = skip_inline_whitespace(&mut self.cursor) {
                    self.diagnostics.push(make_diag(
                        ErrorCode::TabCharacter,
                        self.file,
                        tab_position,
                        tab_position,
                        "tab characters are forbidden (D-SYN-01)".to_owned(),
                    ));
                    continue;
                }
                match self.cursor.peek() {
                    None => break,
                    Some('\n') => {
                        self.cursor.bump();
                        at_line_start = true;
                    }
                    Some('#') => {
                        self.consume_line_comment(line_has_token);
                    }
                    Some(_) => {
                        let prev = self.tokens.last().map(|t| &t.kind);
                        match scan_token(&mut self.cursor, self.file, prev) {
                            Ok(token) => {
                                match token.kind {
                                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                                        self.bracket_depth += 1;
                                    }
                                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                                        self.bracket_depth = self.bracket_depth.saturating_sub(1);
                                    }
                                    _ => {}
                                }
                                self.tokens.push(token);
                                line_has_token = true;
                            }
                            Err(diag) => {
                                self.diagnostics.push(diag);
                            }
                        }
                    }
                }
            }
        }

        while self.indent_stack.len() > 1 {
            self.indent_stack.pop();
            self.push_structural(TokenKind::Dedent);
        }
        self.push_structural(TokenKind::Eof);
    }

    /// Consumes the leading whitespace (spaces/tabs) of a physical line and returns the space
    /// count and whether a tab was mixed in (§5.1 step 1; only the leading whitespace is
    /// checked).
    fn scan_leading_whitespace(&mut self) -> LeadingWhitespace {
        let start = self.cursor.position();
        let mut spaces: u32 = 0;
        loop {
            match self.cursor.peek() {
                Some(' ') => {
                    self.cursor.bump();
                    spaces += 1;
                }
                Some('\t') => {
                    let tab_pos = self.cursor.position();
                    self.cursor.bump();
                    return LeadingWhitespace {
                        spaces,
                        has_tab: true,
                        tab_pos,
                        start,
                    };
                }
                _ => break,
            }
        }
        LeadingWhitespace {
            spaces,
            has_tab: false,
            tab_pos: start,
            start,
        }
    }

    /// Pushes everything from `#`/`##` to the end of the line (just before `\n`) onto the
    /// `CommentStream` as a comment. The cursor is pointing at `#` when this is called.
    /// Ignored through to the end of the line regardless of bracket depth (§5.1 step 3).
    fn consume_line_comment(&mut self, is_trailing: bool) {
        let start = self.cursor.position();
        self.cursor.bump(); // the first '#'
        let is_doc = self.cursor.peek() == Some('#');
        if is_doc {
            self.cursor.bump();
        }
        let mut text = String::new();
        while let Some(c) = self.cursor.peek() {
            if c == '\n' {
                break;
            }
            text.push(c);
            self.cursor.bump();
        }
        let end = self.cursor.position();
        self.comments.push(RawComment {
            text,
            is_doc,
            is_trailing,
            span: Span {
                file: self.file,
                start,
                end,
            },
        });
    }

    /// §5.1 step 5: compares this physical line's leading space count `n` against the indent
    /// stack and emits Newline/Indent/Dedent. Emits a Newline only when `have_logical_line` is
    /// true.
    fn handle_indent_transition(&mut self, n: u32, have_logical_line: bool, at: Position) {
        if have_logical_line {
            self.push_structural(TokenKind::Newline);
        }
        let top = *self.indent_stack.last().unwrap_or(&0);
        match n.cmp(&top) {
            Ordering::Equal => {}
            Ordering::Greater if n == top + 4 => {
                self.push_structural(TokenKind::Indent);
                self.indent_stack.push(n);
            }
            Ordering::Greater => {
                self.diagnostics.push(make_diag(
                    ErrorCode::IndentMismatch,
                    self.file,
                    at,
                    at,
                    "an indent increase is allowed only by +4 relative to the previous line (D-SYN-01)"
                        .to_owned(),
                ));
            }
            Ordering::Less => {
                while let Some(&top_val) = self.indent_stack.last() {
                    if top_val <= n {
                        break;
                    }
                    self.indent_stack.pop();
                    self.push_structural(TokenKind::Dedent);
                }
                let matched = matches!(self.indent_stack.last(), Some(&val) if val == n);
                if !matched {
                    self.diagnostics.push(make_diag(
                        ErrorCode::IndentMismatch,
                        self.file,
                        at,
                        at,
                        "this indentation amount does not match any existing indent level (D-SYN-01)"
                            .to_owned(),
                    ));
                }
            }
        }
    }

    /// Emits a structural token with no text width, such as Newline/Indent/Dedent/Eof, at the
    /// current cursor position (a zero-width Span).
    fn push_structural(&mut self, kind: TokenKind) {
        let pos = self.cursor.position();
        self.tokens.push(Token {
            kind,
            span: Span {
                file: self.file,
                start: pos,
                end: pos,
            },
        });
    }
}

/// Consumes inline spaces and reports a forbidden tab's position.
fn skip_inline_whitespace(cursor: &mut Cursor<'_>) -> Option<Position> {
    loop {
        match cursor.peek() {
            Some(' ') => {
                cursor.bump();
            }
            Some('\t') => {
                let position = cursor.position();
                cursor.bump();
                return Some(position);
            }
            _ => return None,
        }
    }
}

/// D-SYN-05: determines, without consuming it, whether the upcoming token is the start of a
/// method-chain continuation (`.` or `|>`).
fn peek_starts_continuation(cursor: &Cursor<'_>) -> bool {
    match cursor.peek() {
        Some('.') => true,
        Some('|') => cursor.peek2() == Some('>'),
        _ => false,
    }
}

/// Scans exactly one token, assuming the cursor points at a non-whitespace, non-newline,
/// non-EOF character (also shared with the recursive lexing of f-string expressions, §5.2).
/// Newlines, comments (`#`), and leading indentation at the start of a line are outside this
/// function's concern -- it is called on the assumption that the caller has already handled
/// them.
///
/// `prev` is the kind of the most recently emitted token. This is context information that is
/// essential for resolving an ambiguity in number-related scanning (in both cases,
/// SPEC/DECISIONS does not spell this out; a decision made here at the lexer level):
/// - When a digit follows immediately after `.`: D-TYPE-06's tuple element access `t.0` must
///   be split into the two tokens `Dot` + `IntLiteral(0)`, which looks the same on the
///   surface (a `.` followed by a digit) as the lone `.5` (a float with no digit before the
///   decimal point) that D-LEX-04 treats as a lexical error, yet the meaning differs. If the
///   previous token is one that ends an expression (`token_ends_expr`), it is the former;
///   otherwise it is the latter.
/// - When the previous token is exactly `Dot`: the digits that follow are a tuple element
///   access index, and to make a chain like `t.0.1` (two levels of tuple element access) work,
///   the following `.` must not be swallowed as D-LEX-04's decimal point (only the digit run
///   itself is scanned by `scan_tuple_index`).
fn scan_token(
    cursor: &mut Cursor<'_>,
    file: FileId,
    prev: Option<&TokenKind>,
) -> Result<Token, Diagnostic> {
    let start = cursor.position();
    let prev_ends_expr = prev.is_some_and(token_ends_expr);
    match cursor.peek() {
        Some(c) if c.is_ascii_digit() && matches!(prev, Some(TokenKind::Dot)) => {
            scan_tuple_index(cursor, file, start)
        }
        Some(c) if c.is_ascii_digit() => scan_number(cursor, file, start),
        Some('.') if !prev_ends_expr && matches!(cursor.peek2(), Some(d) if d.is_ascii_digit()) => {
            scan_leading_dot_number(cursor, file, start)
        }
        Some(c) if c == '_' || c.is_ascii_alphabetic() => scan_ident_like(cursor, file, start),
        Some('"') => scan_plain_string(cursor, file, start),
        Some(c) if is_operator_start(c) => scan_operator(cursor, file, start),
        Some(_) => Err(scan_unknown(cursor, file, start)),
        None => unreachable!("scan_token: called at EOF (a caller precondition violation)"),
    }
}

/// D-TYPE-06: the judgment the lexer uses to distinguish the `.` of a tuple element access
/// such as `t.0` from the lone `.5` that D-LEX-04 forbids. If the previous token is a kind that
/// ends an expression (an identifier, any of the literal kinds, `self`, `)`/`]`/`}`), the `.`
/// that follows is treated as a postfix tuple-element-access operator (the digit right after
/// it becomes an independent `IntLiteral` on the next call to `scan_token`).
fn token_ends_expr(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident(_)
            | TokenKind::IntLiteral(_)
            | TokenKind::FloatLiteral(_)
            | TokenKind::StringLiteral(_)
            | TokenKind::FString(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::KwSelf
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
    )
}

/// Scanning dedicated to D-TYPE-06's tuple-element-access index: called from `scan_token` only
/// in the context where the previous token is `Dot`. Consumes only the digits of
/// `[0-9][0-9_]*`, and does not swallow a following `.` as a D-LEX-04 decimal point (so the
/// second `.` in `t.0.1` stands on its own as the next token). The same rule as D-LEX-03
/// applies for validating the `_` separator.
fn scan_tuple_index(
    cursor: &mut Cursor<'_>,
    file: FileId,
    start: Position,
) -> Result<Token, Diagnostic> {
    let digits = consume_digit_run(cursor);
    let end = cursor.position();
    if !is_valid_digit_run(&digits) {
        return Err(make_diag(
            ErrorCode::InvalidNumberLiteral,
            file,
            start,
            end,
            "invalid `_` separator in a tuple element access index (D-LEX-03: leading, trailing, or consecutive `_` are forbidden, D-TYPE-06)"
                .to_owned(),
        ));
    }
    let Ok(value) = digits.replace('_', "").parse::<u32>() else {
        return Err(make_diag(
            ErrorCode::InvalidNumberLiteral,
            file,
            start,
            end,
            "tuple index is outside the supported u32 range".to_owned(),
        ));
    };
    let value = i64::from(value);
    Ok(Token {
        kind: TokenKind::IntLiteral(value),
        span: Span { file, start, end },
    })
}

/// D-LEX-03: `[0-9][0-9_]*`. If a `.` follows, it is definitively interpreted as a D-LEX-04
/// floating-point literal.
fn scan_number(
    cursor: &mut Cursor<'_>,
    file: FileId,
    start: Position,
) -> Result<Token, Diagnostic> {
    let int_part = consume_digit_run(cursor);
    let int_valid = is_valid_digit_run(&int_part);

    if cursor.peek() != Some('.') {
        let end = cursor.position();
        if !int_valid {
            return Err(make_diag(
                ErrorCode::InvalidNumberLiteral,
                file,
                start,
                end,
                "invalid `_` separator in an integer literal (D-LEX-03: leading, trailing, or consecutive `_` are forbidden)"
                    .to_owned(),
            ));
        }
        let Ok(value) = int_part.replace('_', "").parse::<i64>() else {
            return Err(make_diag(
                ErrorCode::InvalidNumberLiteral,
                file,
                start,
                end,
                "integer literal is outside the supported i64 range".to_owned(),
            ));
        };
        return Ok(Token {
            kind: TokenKind::IntLiteral(value),
            span: Span { file, start, end },
        });
    }

    // Once a `.` follows, D-LEX-04 definitively interprets this as a floating-point literal
    // (if no digit immediately follows the decimal point, as in `5.`, that itself becomes a
    // lexical error).
    cursor.bump(); // '.'
    let has_frac_digit = matches!(cursor.peek(), Some(c) if c.is_ascii_digit());
    let frac_part = consume_digit_run(cursor);
    let frac_valid = has_frac_digit && is_valid_digit_run(&frac_part);

    let mut exp_sign = String::new();
    let mut exp_digits = String::new();
    let mut has_exponent = false;
    let mut exp_valid = true;
    if matches!(cursor.peek(), Some('e' | 'E')) {
        has_exponent = true;
        cursor.bump();
        if matches!(cursor.peek(), Some('+' | '-'))
            && let Some(sign) = cursor.bump()
        {
            exp_sign.push(sign);
        }
        let has_exp_digit = matches!(cursor.peek(), Some(c) if c.is_ascii_digit());
        exp_digits = consume_digit_run(cursor);
        exp_valid = has_exp_digit && is_valid_digit_run(&exp_digits);
    }

    let end = cursor.position();
    if !int_valid || !frac_valid || !exp_valid {
        return Err(make_diag(
            ErrorCode::InvalidNumberLiteral,
            file,
            start,
            end,
            "invalid digits or `_` separator in a floating-point literal (D-LEX-04)".to_owned(),
        ));
    }

    let mut literal =
        String::with_capacity(int_part.len() + frac_part.len() + exp_digits.len() + 2);
    literal.push_str(&int_part.replace('_', ""));
    literal.push('.');
    literal.push_str(&frac_part.replace('_', ""));
    if has_exponent {
        literal.push('e');
        literal.push_str(&exp_sign);
        literal.push_str(&exp_digits.replace('_', ""));
    }
    let value = literal.parse::<f64>().unwrap_or(0.0);
    Ok(Token {
        kind: TokenKind::FloatLiteral(value),
        span: Span { file, start, end },
    })
}

/// D-LEX-04: a form like `.5` with no digit before it violates the rule requiring at least one
/// digit before the decimal point, so it is always a lexical error (E0004).
fn scan_leading_dot_number(
    cursor: &mut Cursor<'_>,
    file: FileId,
    start: Position,
) -> Result<Token, Diagnostic> {
    cursor.bump(); // '.'
    while matches!(cursor.peek(), Some(c) if c.is_ascii_digit() || c == '_') {
        cursor.bump();
    }
    Err(make_diag(
        ErrorCode::InvalidNumberLiteral,
        file,
        start,
        cursor.position(),
        "at least one digit is required before the decimal point (D-LEX-04)".to_owned(),
    ))
}

fn consume_digit_run(cursor: &mut Cursor<'_>) -> String {
    let mut s = String::new();
    while matches!(cursor.peek(), Some(c) if c.is_ascii_digit() || c == '_') {
        if let Some(c) = cursor.bump() {
            s.push(c);
        }
    }
    s
}

/// Common to D-LEX-03/04: non-empty, does not start or end with `_`, and has no consecutive
/// `_`.
fn is_valid_digit_run(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('_') && !s.ends_with('_') && !s.contains("__")
}

/// D-LEX-02's identifier character class `[a-zA-Z_][a-zA-Z0-9_]*`, D-LEX-01's reserved-word
/// matching, and detection of an f-string via the `f"` prefix (D-LEX-07).
fn scan_ident_like(
    cursor: &mut Cursor<'_>,
    file: FileId,
    start: Position,
) -> Result<Token, Diagnostic> {
    if cursor.peek() == Some('f') && cursor.peek2() == Some('"') {
        cursor.bump(); // 'f'
        cursor.bump(); // the opening '"'
        return match scan_fstring(cursor, file) {
            Ok(parts) => Ok(Token {
                kind: TokenKind::FString(parts),
                span: Span {
                    file,
                    start,
                    end: cursor.position(),
                },
            }),
            Err(diag) => Err(diag),
        };
    }
    let mut s = String::new();
    while matches!(cursor.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_') {
        if let Some(c) = cursor.bump() {
            s.push(c);
        }
    }
    let end = cursor.position();
    let kind = keyword_kind(&s).unwrap_or_else(|| TokenKind::Ident(Arc::from(s.as_str())));
    Ok(Token {
        kind,
        span: Span { file, start, end },
    })
}

/// D-LEX-01's reserved-word table. `Ok`/`Err`/`Some`/`None`/`int`/`float`/`str` are not
/// reserved words, so they are not included here (they are generated as ordinary Idents).
fn keyword_kind(s: &str) -> Option<TokenKind> {
    Some(match s {
        "def" => TokenKind::Def,
        "struct" => TokenKind::Struct,
        "enum" => TokenKind::Enum,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "match" => TokenKind::Match,
        "return" => TokenKind::Return,
        "var" => TokenKind::Var,
        "uses" => TokenKind::Uses,
        "par" => TokenKind::Par,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "self" => TokenKind::KwSelf,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "in" => TokenKind::In,
        "_" => TokenKind::Underscore,
        "module" => TokenKind::Module,
        "void" => TokenKind::Void,
        _ => return None,
    })
}

/// D-LEX-05's string literal body proper. Crossing a newline, or lacking a terminating `"`,
/// both yield E0002 (unterminated string). Even after detecting a D-LEX-06 escape-rule
/// violation (E0003), scanning continues to the terminating `"` (only the first one is
/// reported), so that a wrong cascading diagnostic is not produced for subsequent tokens (a
/// decision made here).
fn scan_plain_string(
    cursor: &mut Cursor<'_>,
    file: FileId,
    start: Position,
) -> Result<Token, Diagnostic> {
    cursor.bump(); // the opening '"'
    let mut content = String::new();
    let mut first_error: Option<Diagnostic> = None;
    loop {
        match cursor.peek() {
            None => {
                let end = cursor.position();
                return Err(first_error.unwrap_or_else(|| unterminated_string(file, start, end)));
            }
            Some('\n') => {
                let end = cursor.position();
                return Err(first_error.unwrap_or_else(|| unterminated_string(file, start, end)));
            }
            Some('"') => {
                cursor.bump();
                let end = cursor.position();
                return match first_error {
                    Some(diag) => Err(diag),
                    None => Ok(Token {
                        kind: TokenKind::StringLiteral(content),
                        span: Span { file, start, end },
                    }),
                };
            }
            Some('\\') => {
                cursor.bump();
                match decode_escape(cursor, file) {
                    Ok(ch) => content.push(ch),
                    Err(diag) => {
                        if first_error.is_none() {
                            first_error = Some(diag);
                        }
                    }
                }
            }
            Some(c) => {
                cursor.bump();
                content.push(c);
            }
        }
    }
}

fn unterminated_string(file: FileId, start: Position, end: Position) -> Diagnostic {
    make_diag(
        ErrorCode::UnterminatedString,
        file,
        start,
        end,
        "unterminated string literal (D-LEX-05: a multi-line string directly containing a newline is not allowed)"
            .to_owned(),
    )
}

/// D-LEX-06: `\n` `\t` `\r` `\\` `\"` `\0` `\u{H..H}`. The cursor points right after the
/// backslash when this is called. An unknown escape yields E0003.
fn decode_escape(cursor: &mut Cursor<'_>, file: FileId) -> Result<char, Diagnostic> {
    let esc_start = cursor.position();
    match cursor.bump() {
        Some('n') => Ok('\n'),
        Some('t') => Ok('\t'),
        Some('r') => Ok('\r'),
        Some('\\') => Ok('\\'),
        Some('"') => Ok('"'),
        Some('0') => Ok('\0'),
        Some('u') => decode_unicode_escape(cursor, file, esc_start),
        Some(_) | None => Err(make_diag(
            ErrorCode::InvalidEscape,
            file,
            esc_start,
            cursor.position(),
            "unknown escape sequence (D-LEX-06)".to_owned(),
        )),
    }
}

fn decode_unicode_escape(
    cursor: &mut Cursor<'_>,
    file: FileId,
    esc_start: Position,
) -> Result<char, Diagnostic> {
    if cursor.peek() != Some('{') {
        return Err(make_diag(
            ErrorCode::InvalidEscape,
            file,
            esc_start,
            cursor.position(),
            "a `\\u` escape requires `{H..H}` (1 to 6 hex digits) (D-LEX-06)".to_owned(),
        ));
    }
    cursor.bump(); // '{'
    let mut hex = String::new();
    while hex.len() < 6 {
        match cursor.peek() {
            Some(c) if c.is_ascii_hexdigit() => {
                hex.push(c);
                cursor.bump();
            }
            _ => break,
        }
    }
    if cursor.peek() == Some('}') {
        cursor.bump();
    } else {
        return Err(make_diag(
            ErrorCode::InvalidEscape,
            file,
            esc_start,
            cursor.position(),
            "the `\\u{...}` escape is not terminated by `}` (D-LEX-06)".to_owned(),
        ));
    }
    if hex.is_empty() {
        return Err(make_diag(
            ErrorCode::InvalidEscape,
            file,
            esc_start,
            cursor.position(),
            "the hex digits of a `\\u{}` escape are empty (D-LEX-06)".to_owned(),
        ));
    }
    let Ok(code) = u32::from_str_radix(&hex, 16) else {
        return Err(make_diag(
            ErrorCode::InvalidEscape,
            file,
            esc_start,
            cursor.position(),
            "the hex digits of a `\\u{...}` escape are invalid (D-LEX-06)".to_owned(),
        ));
    };
    match char::from_u32(code) {
        Some(ch) => Ok(ch),
        None => Err(make_diag(
            ErrorCode::InvalidEscape,
            file,
            esc_start,
            cursor.position(),
            "a `\\u{...}` escape is not a valid Unicode code point (D-LEX-06)".to_owned(),
        )),
    }
}

fn is_operator_start(c: char) -> bool {
    matches!(
        c,
        '+' | '-'
            | '*'
            | '/'
            | '%'
            | '='
            | '<'
            | '>'
            | '!'
            | '.'
            | ','
            | ':'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '?'
            | '|'
    )
}

/// Scans a 1-to-2-character operator/delimiter symbol. A lone `!` or a lone `|` yields E0005
/// (each of them can only be part of the corresponding 2-character token `!=`/`|>`; a symbol
/// with no entry in D-LEX-01's list).
fn scan_operator(
    cursor: &mut Cursor<'_>,
    file: FileId,
    start: Position,
) -> Result<Token, Diagnostic> {
    let Some(c) = cursor.bump() else {
        unreachable!("scan_operator: called at EOF (a caller precondition violation)");
    };
    let kind = match c {
        '+' => TokenKind::Plus,
        '-' => {
            if cursor.peek() == Some('>') {
                cursor.bump();
                TokenKind::Arrow
            } else {
                TokenKind::Minus
            }
        }
        '*' => TokenKind::Star,
        '/' => TokenKind::Slash,
        '%' => TokenKind::Percent,
        '=' => {
            if cursor.peek() == Some('=') {
                cursor.bump();
                TokenKind::EqEq
            } else if cursor.peek() == Some('>') {
                cursor.bump();
                TokenKind::FatArrow
            } else {
                TokenKind::Eq
            }
        }
        '<' => {
            if cursor.peek() == Some('=') {
                cursor.bump();
                TokenKind::LtEq
            } else {
                TokenKind::Lt
            }
        }
        '>' => {
            if cursor.peek() == Some('=') {
                cursor.bump();
                TokenKind::GtEq
            } else {
                TokenKind::Gt
            }
        }
        '!' => {
            if cursor.peek() == Some('=') {
                cursor.bump();
                TokenKind::NotEq
            } else {
                return Err(make_diag(
                    ErrorCode::UnknownToken,
                    file,
                    start,
                    cursor.position(),
                    "a lone `!` is not an operator that exists (D-DIAG-02 E0005)".to_owned(),
                ));
            }
        }
        '.' => TokenKind::Dot,
        ',' => TokenKind::Comma,
        ':' => TokenKind::Colon,
        '(' => TokenKind::LParen,
        ')' => TokenKind::RParen,
        '[' => TokenKind::LBracket,
        ']' => TokenKind::RBracket,
        '{' => TokenKind::LBrace,
        '}' => TokenKind::RBrace,
        '?' => TokenKind::Question,
        '|' => {
            if cursor.peek() == Some('>') {
                cursor.bump();
                TokenKind::PipeOp
            } else {
                return Err(make_diag(
                    ErrorCode::UnknownToken,
                    file,
                    start,
                    cursor.position(),
                    "a lone `|` is not an operator that exists (D-DIAG-02 E0005)".to_owned(),
                ));
            }
        }
        _ => unreachable!(
            "scan_operator: should only ever see characters allowed by is_operator_start"
        ),
    };
    Ok(Token {
        kind,
        span: Span {
            file,
            start,
            end: cursor.position(),
        },
    })
}

/// D-LEX-02/D-DIAG-02 E0005: a character matching no lexical rule. For non-ASCII characters
/// (so that a single misuse of a non-ASCII identifier, e.g. something like "name", is
/// consolidated into one diagnostic), a run of consecutive non-ASCII characters is consumed as
/// a single chunk; ASCII symbols (`@` etc.) are consumed one character at a time.
fn scan_unknown(cursor: &mut Cursor<'_>, file: FileId, start: Position) -> Diagnostic {
    match cursor.peek() {
        Some(c) if !c.is_ascii() => {
            while matches!(cursor.peek(), Some(ch) if !ch.is_ascii()) {
                cursor.bump();
            }
        }
        _ => {
            cursor.bump();
        }
    }
    make_diag(
        ErrorCode::UnknownToken,
        file,
        start,
        cursor.position(),
        "undefined character/token (D-DIAG-02 E0005)".to_owned(),
    )
}

fn make_diag(
    code: ErrorCode,
    file: FileId,
    start: Position,
    end: Position,
    message: String,
) -> Diagnostic {
    Diagnostic {
        code,
        span: Span { file, start, end },
        message,
    }
}

/// Shared with `fstring.rs` so it can recursively lex an f-string's `{expr}` portion
/// (`scan_token`, `skip_inline_whitespace`, and `decode_escape` are all reachable from a child
/// module of this module via `super::`, so Rust's visibility rules require no additional
/// `pub`).
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn tokenize_str(src: &str) -> (Vec<Token>, DiagnosticBag) {
        let (tokens, _comments, diagnostics) = Lexer::new(src, FileId(0)).tokenize();
        (tokens, diagnostics)
    }

    fn kinds(tokens: &[Token]) -> Vec<TokenKind> {
        tokens.iter().map(|t| t.kind.clone()).collect()
    }

    fn structural_count(tokens: &[Token]) -> usize {
        tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.kind,
                    TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
                )
            })
            .count()
    }

    fn sample_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn read_sample(rel: &str) -> String {
        let path = sample_path(rel);
        match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => panic!("failed to read sample file {}: {e}", path.display()),
        }
    }

    /// A simple scan that pulls the `diagnostics` array (a sequence of string codes) out of
    /// `expected.toml` for the case whose `entry` matches (there's no need to parse the whole
    /// TOML document -- it only picks up these two lines of this fixed schema).
    fn expected_diagnostics_for(toml_text: &str, entry_file: &str) -> Vec<String> {
        let mut case_matches_entry = false;
        for line in toml_text.lines() {
            let trimmed = line.trim();
            if trimmed == "[[case]]" {
                case_matches_entry = false;
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("entry") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=')
                    && rest.contains(entry_file)
                {
                    case_matches_entry = true;
                }
                continue;
            }
            if case_matches_entry && let Some(rest) = trimmed.strip_prefix("diagnostics") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    return extract_quoted(rest);
                }
            }
        }
        Vec::new()
    }

    fn extract_quoted(s: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut current = String::new();
        let mut in_str = false;
        for c in s.chars() {
            if c == '"' {
                if in_str {
                    result.push(current.clone());
                    current.clear();
                }
                in_str = !in_str;
            } else if in_str {
                current.push(c);
            }
        }
        result
    }

    // --- Verify that samples/ok/ tokenizes with zero diagnostics ---

    #[test]
    fn tokenizes_2_lexical_basics_without_diagnostics() {
        let src = read_sample("samples/ok/2_lexical_basics/entry_main.ybm");
        let (tokens, diagnostics) = tokenize_str(&src);
        assert!(diagnostics.is_empty(), "diagnostics should be empty");
        assert!(matches!(
            tokens.last().map(|t| &t.kind),
            Some(TokenKind::Eof)
        ));
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::Def)));
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::Indent)));
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::Dedent)));
    }

    #[test]
    fn tokenizes_6_4_strings_without_diagnostics() {
        let src = read_sample("samples/ok/6-4_strings/entry_main.ybm");
        let (tokens, diagnostics) = tokenize_str(&src);
        assert!(diagnostics.is_empty(), "diagnostics should be empty");
        assert!(matches!(
            tokens.last().map(|t| &t.kind),
            Some(TokenKind::Eof)
        ));
        assert!(
            tokens
                .iter()
                .any(|t| matches!(&t.kind, TokenKind::FString(parts) if !parts.is_empty()))
        );
    }

    #[test]
    fn tokenizes_6_3_operator_precedence_without_diagnostics() {
        let src = read_sample("samples/ok/6-3_operator_precedence_mixed_expression/entry_main.ybm");
        let (tokens, diagnostics) = tokenize_str(&src);
        assert!(diagnostics.is_empty(), "diagnostics should be empty");
        assert!(matches!(
            tokens.last().map(|t| &t.kind),
            Some(TokenKind::Eof)
        ));
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::PipeOp)));
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::Question)));
        assert!(tokens.iter().any(|t| matches!(t.kind, TokenKind::Not)));
    }

    // --- Verify each file under samples/err/static/2_lexical_errors/ produces the expected code ---

    fn assert_single_expected_code(entry_file: &str) {
        let dir = "samples/err/static/2_lexical_errors";
        let src = read_sample(&format!("{dir}/{entry_file}"));
        let toml_text = read_sample(&format!("{dir}/expected.toml"));
        let expected = expected_diagnostics_for(&toml_text, entry_file);
        assert_eq!(
            expected.len(),
            1,
            "should be able to pull exactly one diagnostic code for {entry_file} out of expected.toml"
        );
        let (_tokens, diagnostics) = tokenize_str(&src);
        let sorted = diagnostics.into_sorted(&dummy_source_map(&src));
        let codes: Vec<String> = sorted.iter().map(|d| d.code.to_string()).collect();
        assert_eq!(
            codes, expected,
            "{entry_file}'s diagnostics should match what is expected"
        );
    }

    fn dummy_source_map(text: &str) -> crate::diagnostics::SourceMap {
        let mut sources = crate::diagnostics::SourceMap::new();
        sources.add(PathBuf::from("entry.ybm"), text.to_owned());
        sources
    }

    #[test]
    fn e0001_tab_character() {
        assert_single_expected_code("entry_tab_character.ybm");
    }

    #[test]
    fn e0002_unterminated_string() {
        assert_single_expected_code("entry_unterminated_string.ybm");
    }

    #[test]
    fn e0003_invalid_escape() {
        assert_single_expected_code("entry_invalid_escape.ybm");
    }

    #[test]
    fn e0004_invalid_int_literal() {
        assert_single_expected_code("entry_invalid_int_literal.ybm");
    }

    #[test]
    fn e0004_invalid_float_literal() {
        assert_single_expected_code("entry_invalid_float_literal.ybm");
    }

    #[test]
    fn e0005_unknown_token() {
        assert_single_expected_code("entry_unknown_token.ybm");
    }

    #[test]
    fn e0005_non_ascii_identifier() {
        assert_single_expected_code("entry_non_ascii_identifier.ybm");
    }

    #[test]
    fn e0501_indentation_mismatch_from_syntax_errors_sample() {
        let dir = "samples/err/static/2_syntax_errors";
        let src = read_sample(&format!("{dir}/entry_indentation_mismatch.ybm"));
        let toml_text = read_sample(&format!("{dir}/expected.toml"));
        let expected = expected_diagnostics_for(&toml_text, "entry_indentation_mismatch.ybm");
        assert_eq!(expected, vec!["E0501".to_owned()]);
        let (_tokens, diagnostics) = tokenize_str(&src);
        let sorted = diagnostics.into_sorted(&dummy_source_map(&src));
        let codes: Vec<String> = sorted.iter().map(|d| d.code.to_string()).collect();
        assert_eq!(codes, expected);
    }

    // --- Individual unit tests for Indent/Dedent generation ---

    #[test]
    fn nested_indent_emits_indent_per_level() {
        let src = "if true\n    if true\n        x = 1\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::If,
                TokenKind::True,
                TokenKind::Newline,
                TokenKind::Indent,
                TokenKind::If,
                TokenKind::True,
                TokenKind::Newline,
                TokenKind::Indent,
                TokenKind::Ident(Arc::from("x")),
                TokenKind::Eq,
                TokenKind::IntLiteral(1),
                TokenKind::Dedent,
                TokenKind::Dedent,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn multi_level_dedent_via_following_line() {
        let src = "if true\n    if true\n        x = 1\ny = 2\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        let tail = &kinds(&tokens)[11..];
        assert_eq!(
            tail,
            vec![
                TokenKind::Newline,
                TokenKind::Dedent,
                TokenKind::Dedent,
                TokenKind::Ident(Arc::from("y")),
                TokenKind::Eq,
                TokenKind::IntLiteral(2),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn dedent_to_unmatched_level_is_e0501() {
        let src = "if true\n    x = 1\n  y = 2\n";
        let (_tokens, diagnostics) = tokenize_str(src);
        let sorted = diagnostics.into_sorted(&dummy_source_map(src));
        assert_eq!(sorted.len(), 1);
        assert_eq!(
            sorted[0].code,
            crate::diagnostics::ErrorCode::IndentMismatch
        );
        assert_eq!(sorted[0].span.start, Position { line: 3, col: 1 });
    }

    #[test]
    fn indent_increase_not_multiple_of_four_is_e0501() {
        let src = "def compute(): int\n  return 1\n";
        let (_tokens, diagnostics) = tokenize_str(src);
        let sorted = diagnostics.into_sorted(&dummy_source_map(src));
        assert_eq!(sorted.len(), 1);
        assert_eq!(
            sorted[0].code,
            crate::diagnostics::ErrorCode::IndentMismatch
        );
        assert_eq!(sorted[0].span.start, Position { line: 2, col: 1 });
    }

    #[test]
    fn bracket_interior_newlines_are_ignored() {
        let src = "x = foo(\n    1,\n    2,\n)\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        assert_eq!(
            structural_count(&tokens),
            0,
            "inside brackets, a newline does not generate a structural token"
        );
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Ident(Arc::from("x")),
                TokenKind::Eq,
                TokenKind::Ident(Arc::from("foo")),
                TokenKind::LParen,
                TokenKind::IntLiteral(1),
                TokenKind::Comma,
                TokenKind::IntLiteral(2),
                TokenKind::Comma,
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn method_chain_continuation_suppresses_newline() {
        let src = "result = foo\n    .bar()\n    .baz()\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        assert_eq!(
            structural_count(&tokens),
            0,
            "in a D-SYN-05 method-chain continuation, a newline does not generate a structural token"
        );
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Ident(Arc::from("result")),
                TokenKind::Eq,
                TokenKind::Ident(Arc::from("foo")),
                TokenKind::Dot,
                TokenKind::Ident(Arc::from("bar")),
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Dot,
                TokenKind::Ident(Arc::from("baz")),
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn pipe_continuation_suppresses_newline() {
        let src = "result = foo\n    |> bar\n    |> baz\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        assert_eq!(structural_count(&tokens), 0);
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Ident(Arc::from("result")),
                TokenKind::Eq,
                TokenKind::Ident(Arc::from("foo")),
                TokenKind::PipeOp,
                TokenKind::Ident(Arc::from("bar")),
                TokenKind::PipeOp,
                TokenKind::Ident(Arc::from("baz")),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn tab_in_indentation_is_fatal_and_aborts_lexing() {
        let src = "def x(): int\n\treturn 1\n";
        let (tokens, diagnostics) = tokenize_str(src);
        let sorted = diagnostics.into_sorted(&dummy_source_map(src));
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].code, crate::diagnostics::ErrorCode::TabCharacter);
        assert_eq!(sorted[0].span.start, Position { line: 2, col: 1 });
        // Because of the fatal abort, not a single token after "return" is generated
        // (also confirm there are no structural/code tokens at all after the Def token).
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Def,
                TokenKind::Ident(Arc::from("x")),
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Colon,
                TokenKind::Ident(Arc::from("int")),
            ]
        );
    }

    #[test]
    fn shebang_is_stripped_and_does_not_produce_tokens() {
        let src = "#!/usr/bin/env ybm\nx = 1\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Ident(Arc::from("x")),
                TokenKind::Eq,
                TokenKind::IntLiteral(1),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn blank_and_comment_only_lines_do_not_affect_indent() {
        let src = "x = 1\n\n# comment\n\ny = 2\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        assert_eq!(
            structural_count(&tokens),
            1,
            "even with blank lines/comment-only lines in between, there is only one Newline"
        );
    }

    #[test]
    fn fstring_interpolation_and_escapes_tokenize() {
        let src = "greeting = f\"hello {{ {name} }}\"\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        let fstring_tok = tokens.iter().find_map(|t| match &t.kind {
            TokenKind::FString(parts) => Some(parts.clone()),
            _ => None,
        });
        let Some(parts) = fstring_tok else {
            panic!("could not find an FString token");
        };
        let actual: Vec<FStringPart> = parts.into_iter().map(strip_expr_spans).collect();
        let expected: Vec<FStringPart> = vec![
            FStringPart::Text("hello { ".to_owned()),
            FStringPart::Expr(vec![Token {
                kind: TokenKind::Ident(Arc::from("name")),
                span: Span {
                    file: FileId(0),
                    start: Position { line: 1, col: 1 },
                    end: Position { line: 1, col: 1 },
                },
            }]),
            FStringPart::Text(" }".to_owned()),
        ]
        .into_iter()
        .map(strip_expr_spans)
        .collect();
        assert_eq!(actual, expected);
    }

    /// For an f-string's Expr portion, verifying the recursive token sequence of the
    /// expression does not need an exact match down to `span`, so it is simplified to just
    /// `TokenKind` for comparison.
    fn strip_expr_spans(part: FStringPart) -> FStringPart {
        match part {
            FStringPart::Expr(toks) => FStringPart::Expr(
                toks.into_iter()
                    .map(|t| Token {
                        kind: t.kind,
                        span: Span {
                            file: FileId(0),
                            start: Position { line: 1, col: 1 },
                            end: Position { line: 1, col: 1 },
                        },
                    })
                    .collect(),
            ),
            other @ FStringPart::Text(_) => other,
        }
    }

    // --- Tokenization verification for all 158 .ybm files under samples/ ---
    // For err/static/2_lexical_errors and entry_indentation_mismatch.ybm, it is correct
    // behavior for lexical-layer diagnostics to be emitted (checked one by one against the
    // codes below). For the other 150 files, verify they tokenize with zero lexical-layer
    // diagnostics (errors from the parser/type-checking and beyond are outside this unit's
    // concern).

    /// (path relative to the samples root, the list of expected lexical-layer diagnostic codes).
    fn expected_lexical_diagnostics() -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            (
                "err/static/2_lexical_errors/entry_tab_character.ybm",
                vec!["E0001"],
            ),
            (
                "err/static/2_lexical_errors/entry_unterminated_string.ybm",
                vec!["E0002"],
            ),
            (
                "err/static/2_lexical_errors/entry_invalid_escape.ybm",
                vec!["E0003"],
            ),
            (
                "err/static/2_lexical_errors/entry_invalid_int_literal.ybm",
                vec!["E0004"],
            ),
            (
                "err/static/2_lexical_errors/entry_invalid_float_literal.ybm",
                vec!["E0004"],
            ),
            (
                "err/static/2_lexical_errors/entry_unknown_token.ybm",
                vec!["E0005"],
            ),
            (
                "err/static/2_lexical_errors/entry_non_ascii_identifier.ybm",
                vec!["E0005"],
            ),
            (
                "err/static/2_syntax_errors/entry_indentation_mismatch.ybm",
                vec!["E0501"],
            ),
        ]
    }

    fn all_sample_ybm_files() -> Vec<PathBuf> {
        let root = sample_path("samples");
        let mut out = Vec::new();
        collect_ybm_files(&root, &mut out);
        out.sort();
        out
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

    #[test]
    fn all_158_sample_ybm_files_tokenize_as_expected() {
        let files = all_sample_ybm_files();
        assert_eq!(
            files.len(),
            158,
            "the number of .ybm files under samples/ should match the expected count (158) (detects an unexpected addition/removal)"
        );

        let samples_root = sample_path("samples");
        let exceptions = expected_lexical_diagnostics();

        let mut checked_exceptions: Vec<String> = Vec::new();
        let mut clean_count = 0usize;

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
            let (tokens, diagnostics) = tokenize_str(&src);

            if let Some((_, expected_codes)) = exceptions.iter().find(|(p, _)| *p == rel.as_str()) {
                checked_exceptions.push(rel.clone());
                let sorted = diagnostics.into_sorted(&dummy_source_map(&src));
                let codes: Vec<String> = sorted.iter().map(|d| d.code.to_string()).collect();
                let expected: Vec<String> =
                    expected_codes.iter().map(|s| (*s).to_owned()).collect();
                assert_eq!(
                    codes, expected,
                    "{rel}: should match the expected lexical-layer diagnostics"
                );
            } else {
                assert!(
                    diagnostics.is_empty(),
                    "{rel}: should have zero lexical-layer diagnostics (everything except \
                     err/static/2_lexical_errors and indentation_mismatch should tokenize \
                     successfully). Actual: {:?}",
                    diagnostics
                        .into_sorted(&dummy_source_map(&src))
                        .iter()
                        .map(|d| d.code.to_string())
                        .collect::<Vec<_>>()
                );
                assert!(
                    matches!(tokens.last().map(|t| &t.kind), Some(TokenKind::Eof)),
                    "{rel}: the token sequence must always terminate with Eof"
                );
                clean_count += 1;
            }
        }

        assert_eq!(
            checked_exceptions.len(),
            exceptions.len(),
            "every expected lexical-layer diagnostic file should actually have been found (detects e.g. a rename)"
        );
        assert_eq!(clean_count + checked_exceptions.len(), 158);
    }

    // --- Additional boundary verification for newlines inside brackets, line continuation, consecutive dedents, and the implicit dedent at end of file ---

    #[test]
    fn bracket_interior_allows_nested_brackets_and_blank_lines() {
        // A blank line inside brackets does not affect tokenization / the depth of nested
        // brackets also returns correctly.
        let src = "x = foo(\n\n    [1, 2],\n\n    {3: 4},\n)\ny = 1\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        // "y = 1" after the closing bracket should return to ordinary (top-level) indent
        // judgment.
        assert_eq!(
            structural_count(&tokens),
            1,
            "only the first newline after closing the bracket generates one Newline"
        );
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Ident(ref s) if &**s == "y"))
        );
    }

    #[test]
    fn consecutive_dedent_to_zero_at_eof_without_trailing_newline_marker_line() {
        // If the file ends while still indented multiple levels, the Dedents for however many
        // levels are open at EOF must all be emitted together, followed by Eof (§5.1's
        // end-of-file handling).
        let src = "if true\n    if true\n        x = 1\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        let kinds_vec = kinds(&tokens);
        let last_four = &kinds_vec[kinds_vec.len() - 3..];
        assert_eq!(
            last_four,
            vec![TokenKind::Dedent, TokenKind::Dedent, TokenKind::Eof]
        );
    }

    #[test]
    fn line_continuation_chain_of_five_method_calls_suppresses_all_intermediate_newlines() {
        // D-SYN-05: with a 5-step method chain, the continuation judgment is true four times
        // in a row (ARCHITECTURE.md §5.1).
        let src = "result = foo\n    .a()\n    .b()\n    .c()\n    .d()\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        assert_eq!(
            structural_count(&tokens),
            0,
            "not a single structural token is generated across the whole 5-step method chain"
        );
        let dot_count = tokens
            .iter()
            .filter(|t| matches!(t.kind, TokenKind::Dot))
            .count();
        assert_eq!(dot_count, 4, "all 4 continuations via `.` are recognized");
    }

    #[test]
    fn tuple_field_access_dot_digit_is_not_a_leading_dot_float() {
        // D-TYPE-06: `t.0` should be the two tokens `Dot` + `IntLiteral(0)`, and D-LEX-04's
        // "a lone `.5` is always a lexical error" rule must not be mistakenly applied here
        // (the `multi_trigger_tuple.0` in
        // samples/fmt/collection_and_call_arg_line_splitting/sample.in.ybm catches this
        // regression).
        let src = "x = t.0\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Ident(Arc::from("x")),
                TokenKind::Eq,
                TokenKind::Ident(Arc::from("t")),
                TokenKind::Dot,
                TokenKind::IntLiteral(0),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn chained_tuple_field_access_dot_digit_dot_digit() {
        // `t.0.1` (element access into a tuple of tuples) must also correctly apply the same
        // judgment in a row.
        let src = "x = t.0.1\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Ident(Arc::from("x")),
                TokenKind::Eq,
                TokenKind::Ident(Arc::from("t")),
                TokenKind::Dot,
                TokenKind::IntLiteral(0),
                TokenKind::Dot,
                TokenKind::IntLiteral(1),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn leading_dot_number_at_expression_start_is_still_e0004() {
        // When the previous token does not end an expression (i.e. at the start of a
        // statement), `.5` remains a lexical error per D-LEX-04.
        let src = "x = .5\n";
        let (_tokens, diagnostics) = tokenize_str(src);
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn tuple_field_access_inside_fstring_expr() {
        // The same judgment must also apply inside an f-string's expr (§5.2's recursive
        // lexing).
        let src = "greeting = f\"{t.0}\"\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
        let fstring_tok = tokens.iter().find_map(|t| match &t.kind {
            TokenKind::FString(parts) => Some(parts.clone()),
            _ => None,
        });
        let Some(parts) = fstring_tok else {
            panic!("could not find an FString token");
        };
        assert_eq!(parts.len(), 1);
        let FStringPart::Expr(expr_tokens) = &parts[0] else {
            panic!("could not find the expr portion");
        };
        assert_eq!(
            expr_tokens
                .iter()
                .map(|t| t.kind.clone())
                .collect::<Vec<_>>(),
            vec![
                TokenKind::Ident(Arc::from("t")),
                TokenKind::Dot,
                TokenKind::IntLiteral(0),
            ]
        );
    }

    #[test]
    fn dedent_after_bracket_interior_resumes_indent_tracking_correctly() {
        // The physical line right after closing a bracket is compared against the baseline
        // indent from before the bracket was opened (confirms that after D-SYN-04 ends,
        // things return to the ordinary §5.1 step 5).
        let src = "if true\n    x = foo(\n        1,\n    )\n    y = 2\n";
        let (tokens, diagnostics) = tokenize_str(src);
        assert!(diagnostics.is_empty());
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::If,
                TokenKind::True,
                TokenKind::Newline,
                TokenKind::Indent,
                TokenKind::Ident(Arc::from("x")),
                TokenKind::Eq,
                TokenKind::Ident(Arc::from("foo")),
                TokenKind::LParen,
                TokenKind::IntLiteral(1),
                TokenKind::Comma,
                TokenKind::RParen,
                TokenKind::Newline,
                TokenKind::Ident(Arc::from("y")),
                TokenKind::Eq,
                TokenKind::IntLiteral(2),
                TokenKind::Dedent,
                TokenKind::Eof,
            ]
        );
    }
}
