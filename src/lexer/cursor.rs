//! A low-level character cursor (peek/bump in Unicode scalar units, ARCHITECTURE.md §2.1).
//! The column number (`Position::col`) is counted in Unicode scalar value units (so
//! D-COL-03's philosophy also matches an editor's column display, §3.1).

use crate::diagnostics::Position;

/// A character cursor for one source file. Tracks both the byte offset and the (line, col)
/// pair -- the byte offset is used for `&str` slicing, and (line, col) for building a `Span`.
pub struct Cursor<'src> {
    text: &'src str,
    byte_pos: usize,
    position: Position,
}

impl<'src> Cursor<'src> {
    #[must_use]
    pub fn new(text: &'src str) -> Self {
        Self {
            text,
            byte_pos: 0,
            position: Position { line: 1, col: 1 },
        }
    }

    /// The next character to read from the current position (does not consume it). Using
    /// `str::get` eliminates any worry about crossing a byte boundary at the type level
    /// (`None` if out of range or not on a boundary).
    #[must_use]
    pub fn peek(&self) -> Option<char> {
        self.text
            .get(self.byte_pos..)
            .and_then(|rest| rest.chars().next())
    }

    /// One further than `peek()` (a 2-character lookahead, used to distinguish 2-character
    /// tokens such as `..`).
    #[must_use]
    pub fn peek2(&self) -> Option<char> {
        self.text.get(self.byte_pos..)?.chars().nth(1)
    }

    /// Consumes and returns one character. If it crosses a newline, updates `position`'s
    /// line/col (columns are counted in Unicode scalar value units, D-COL-03).
    pub fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.byte_pos += c.len_utf8();
        if c == '\n' {
            self.position.line += 1;
            self.position.col = 1;
        } else {
            self.position.col += 1;
        }
        Some(c)
    }

    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.byte_pos >= self.text.len()
    }

    #[must_use]
    pub fn position(&self) -> Position {
        self.position
    }
}
