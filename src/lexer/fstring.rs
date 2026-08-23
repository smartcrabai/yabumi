//! f-string scanning (brace depth, recursive lexing of the expr portion, D-LEX-07,
//! ARCHITECTURE.md §5.2).

use super::cursor::Cursor;
use super::token::{FStringPart, Token};
use crate::diagnostics::{Diagnostic, ErrorCode, FileId, Position, Span};

/// Starts scanning right after `f"` (the opening double quote already consumed), consumes the
/// terminating `"`, and returns a `Vec<FStringPart>`. Implements here the depth counting for
/// `{expr}` (+1 on `{`, -1 on `}`; a `}` at depth 0 terminates the interpolation), `{{`/`}}`
/// escaping, and the prohibition on `"` inside expr (D-LEX-07).
///
/// # Errors
/// Returns the first `Diagnostic` detected, for: unterminated (equivalent to D-LEX-05, E0002),
/// a string literal appearing inside expr (forbidden by D-LEX-07, reusing E0005 -- see
/// ARCHITECTURE.md §5.2, "decision made here"), or the recursive lexing of expr itself
/// detecting a lexical error.
pub fn scan_fstring(cursor: &mut Cursor<'_>, file: FileId) -> Result<Vec<FStringPart>, Diagnostic> {
    let mut parts = Vec::new();
    let mut text = String::new();
    loop {
        match cursor.peek() {
            None | Some('\n') => return Err(unterminated(file, cursor.position())),
            Some('"') => {
                cursor.bump();
                if !text.is_empty() {
                    parts.push(FStringPart::Text(text));
                }
                return Ok(parts);
            }
            Some('\\') => {
                cursor.bump();
                let ch = super::decode_escape(cursor, file)?;
                text.push(ch);
            }
            Some('{') => {
                cursor.bump();
                if cursor.peek() == Some('{') {
                    cursor.bump();
                    text.push('{');
                } else {
                    if !text.is_empty() {
                        parts.push(FStringPart::Text(std::mem::take(&mut text)));
                    }
                    let expression_start = cursor.position();
                    let expr_src = scan_expr_slice(cursor, file)?;
                    let tokens = tokenize_expr_slice(&expr_src, file, expression_start)?;
                    parts.push(FStringPart::Expr(tokens));
                }
            }
            Some('}') => {
                // D-LEX-07: `}}` -> literal `}`. A lone `}` (outside an interpolation) is
                // also leniently treated as a literal `}` (it just has no pair; the meaning
                // does not change -- SPEC/DECISIONS does not spell out this edge case, so
                // this is a decision made here).
                cursor.bump();
                if cursor.peek() == Some('}') {
                    cursor.bump();
                }
                text.push('}');
            }
            Some(c) => {
                cursor.bump();
                text.push(c);
            }
        }
    }
}

fn unterminated(file: FileId, at: Position) -> Diagnostic {
    Diagnostic {
        code: ErrorCode::UnterminatedString,
        span: Span {
            file,
            start: at,
            end: at,
        },
        message: "unterminated f-string literal (D-LEX-05)".to_owned(),
    }
}

/// Cuts out the raw substring from right after the interpolation-opening `{` (depth 1) up to
/// just before the `}` where depth reaches 0 (the scanning algorithm of §5.2). If a `"`
/// appears inside expr, returns E0005 per D-LEX-07.
fn scan_expr_slice(cursor: &mut Cursor<'_>, file: FileId) -> Result<String, Diagnostic> {
    let mut depth: u32 = 1;
    let mut buf = String::new();
    loop {
        match cursor.peek() {
            None => return Err(unterminated(file, cursor.position())),
            Some('"') => {
                let at = cursor.position();
                return Err(Diagnostic {
                    code: ErrorCode::UnknownToken,
                    span: Span {
                        file,
                        start: at,
                        end: at,
                    },
                    message:
                        "a string literal cannot be written inside an f-string's expr (D-LEX-07)"
                            .to_owned(),
                });
            }
            Some('{') => {
                cursor.bump();
                depth += 1;
                buf.push('{');
            }
            Some('}') => {
                cursor.bump();
                depth -= 1;
                if depth == 0 {
                    return Ok(buf);
                }
                buf.push('}');
            }
            Some(c) => {
                cursor.bump();
                buf.push(c);
            }
        }
    }
}

/// Recursively tokenizes the cut-out raw expr text using the same lexical rules
/// (`super::scan_token`) (§5.2, "by recursively invoking the same Lexer"). Bails out
/// immediately on the first error -- since `scan_expr_slice` has already excluded `"`, an
/// f-string can never be nested here.
///
/// Just like the top-level `run`, passes to `scan_token` whether the previous token is a kind
/// that ends an expression (`super::token_ends_expr`) -- so that something like tuple element
/// access in `f"{t.0}"` is correctly split into `Dot` + `IntLiteral(0)` inside expr too
/// (D-TYPE-06; see the comment on the `scan_token` side for how the ambiguity with D-LEX-04 is
/// resolved).
fn tokenize_expr_slice(
    text: &str,
    file: FileId,
    expression_start: Position,
) -> Result<Vec<Token>, Diagnostic> {
    let mut sub_cursor = Cursor::new(text);
    let mut tokens = Vec::new();
    loop {
        if let Some(position) = super::skip_inline_whitespace(&mut sub_cursor) {
            let span = shift_span(
                Span {
                    file,
                    start: position,
                    end: position,
                },
                expression_start,
            );
            return Err(Diagnostic {
                code: ErrorCode::TabCharacter,
                span,
                message: "tab characters are forbidden (D-SYN-01)".to_owned(),
            });
        }
        if sub_cursor.peek().is_none() {
            return Ok(tokens);
        }
        let previous = tokens.last().map(|token: &Token| &token.kind);
        let token = super::scan_token(&mut sub_cursor, file, previous)
            .map_err(|diagnostic| shift_diagnostic(diagnostic, expression_start))?;
        tokens.push(Token {
            kind: token.kind,
            span: shift_span(token.span, expression_start),
        });
    }
}

fn shift_diagnostic(mut diagnostic: Diagnostic, start: Position) -> Diagnostic {
    diagnostic.span = shift_span(diagnostic.span, start);
    diagnostic
}

fn shift_span(span: Span, start: Position) -> Span {
    fn shift(position: Position, start: Position) -> Position {
        Position {
            line: start.line + position.line - 1,
            col: if position.line == 1 {
                start.col + position.col - 1
            } else {
                position.col
            },
        }
    }
    Span {
        file: span.file,
        start: shift(span.start, start),
        end: shift(span.end, start),
    }
}
