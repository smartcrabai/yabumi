//! Representation of source positions (§3.1). Full implementation of Position/Span/FileId/
//! SourceFile/SourceMap.
//!
//! Columns are counted in Unicode scalar value units (so the "1 character = 1 unit"
//! char-based philosophy of D-COL-03 also matches the column numbers an editor displays.
//! Counting by byte would disagree with the editor's column number on lines containing
//! multi-byte characters -- ARCHITECTURE.md §3.1, "decision made here").

use std::path::{Path, PathBuf};

/// A 1-indexed line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

/// Points to some place within some file. Because multiple files (the entry file plus
/// modules in the same directory) are handled simultaneously, this always carries a
/// `FileId` in addition to the start/end positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file: FileId,
    pub start: Position,
    /// Exclusive (this position itself is not included in the range).
    pub end: Position,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

/// The source body for one file, loaded during a single `ybm` invocation.
pub struct SourceFile {
    path: PathBuf,
    text: String,
    /// The starting byte offset of each line (0-indexed). Used by `SourceMap::slice` to
    /// convert between Position and byte offset (e.g. when the one-argument form of assert
    /// cuts out and displays a portion of the source text).
    line_starts: Vec<u32>,
}

impl SourceFile {
    #[must_use]
    fn new(path: PathBuf, text: String) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                let next_line_start = u32::try_from(i + 1).unwrap_or(u32::MAX);
                line_starts.push(next_line_start);
            }
        }
        Self {
            path,
            text,
            line_starts,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Computes the starting byte offset of the character that `pos` (a 1-indexed line and a
    /// 1-indexed column counted in Unicode scalar values) points to (D-COL-03). When `pos`
    /// exceeds the actual character count of that line (including the line's trailing `\n`),
    /// it is clamped to the end of the line (the next line's start offset, or the end of the
    /// text for the last line) -- this matches how a Span's `end` is an exclusive boundary
    /// pointing "just after the last token".
    #[must_use]
    fn position_to_byte_offset(&self, pos: Position) -> usize {
        let text_len = self.text.len();
        let line_idx = pos.line.saturating_sub(1) as usize;
        let line_start = self
            .line_starts
            .get(line_idx)
            .copied()
            .map_or(text_len, |v| (v as usize).min(text_len));
        let line_end = self
            .line_starts
            .get(line_idx + 1)
            .copied()
            .map_or(text_len, |v| (v as usize).min(text_len));
        // line_start/line_end are always on a UTF-8 boundary (0, the end of the text, or the
        // byte right after a '\n').
        let Some(line_slice) = self.text.get(line_start..line_end) else {
            return line_start.min(text_len);
        };
        let col_idx = pos.col.saturating_sub(1) as usize;
        match line_slice.char_indices().nth(col_idx) {
            Some((byte_idx, _)) => line_start + byte_idx,
            // Even after counting every character in the line (including the trailing `\n`),
            // col_idx is not reached -- clamp to point at the end of the line.
            None => line_end,
        }
    }
}

/// Holds every file loaded during a single `ybm` invocation (the entry file plus modules in
/// the same directory that were auto-imported). Built only after all files have finished
/// being read, before lexing begins.
///
/// Per the PAR-ABORT-NOT-ACTUALLY-IMMEDIATE decision (ARCHITECTURE.md §5.8/§8), this is
/// shared as an `Arc<SourceMap>` via `Program.sources` (§3.11) so that diagnostics can be
/// rendered even from within a `par` worker thread.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, path: PathBuf, text: String) -> FileId {
        let id = FileId(u32::try_from(self.files.len()).unwrap_or(u32::MAX));
        self.files.push(SourceFile::new(path, text));
        id
    }

    #[must_use]
    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    #[must_use]
    pub fn path(&self, id: FileId) -> &Path {
        self.file(id).path()
    }

    /// Cuts out the text fragment that `span` points to (used when the one-argument form of
    /// assert automatically cuts out and displays a portion of the source text, STDLIB.md
    /// §13).
    ///
    /// Both `start` and `end` are converted from Position (1-indexed line, 1-indexed column
    /// counted in Unicode scalar values, D-COL-03) to a byte offset before slicing. So that
    /// this never panics even when `end` comes before `start` (a mistakenly reversed Span),
    /// the smaller of the two is always treated as the starting byte offset.
    #[must_use]
    pub fn slice(&self, span: Span) -> &str {
        let file = self.file(span.file);
        let start = file.position_to_byte_offset(span.start);
        let end = file.position_to_byte_offset(span.end);
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        // start/end were either derived via char_indices or clamped to a line boundary
        // (right after a '\n', or the end of the text), so they are always on a UTF-8
        // boundary. Only out-of-range values are conservatively kept at the end of the text.
        let text = file.text();
        let start = start.min(text.len());
        let end = end.min(text.len());
        text.get(start..end).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(text: &str) -> (SourceMap, FileId) {
        let mut sm = SourceMap::new();
        let id = sm.add(PathBuf::from("test.ybm"), text.to_string());
        (sm, id)
    }

    fn span(file: FileId, sl: u32, sc: u32, el: u32, ec: u32) -> Span {
        Span {
            file,
            start: Position { line: sl, col: sc },
            end: Position { line: el, col: ec },
        }
    }

    #[test]
    fn slice_ascii_single_line() {
        let (sm, id) = build("let x = 1\n");
        // "x" is at 1-indexed column 5 (1='l', 2='e', 3='t', 4=' ', 5='x').
        assert_eq!(sm.slice(span(id, 1, 5, 1, 6)), "x");
        assert_eq!(sm.slice(span(id, 1, 1, 1, 4)), "let");
    }

    #[test]
    fn slice_full_line_including_trailing_content() {
        let (sm, id) = build("abc\ndef\n");
        assert_eq!(sm.slice(span(id, 1, 1, 1, 4)), "abc");
        assert_eq!(sm.slice(span(id, 2, 1, 2, 4)), "def");
    }

    #[test]
    fn slice_multiline_span() {
        let (sm, id) = build("abc\ndef\n");
        // From "c" on line 1 through "d" on line 2.
        assert_eq!(sm.slice(span(id, 1, 3, 2, 2)), "c\nd");
    }

    #[test]
    fn slice_multibyte_columns() {
        // "âêî" is 3 characters; in UTF-8 each is 2 bytes (6 bytes total).
        let (sm, id) = build("âêî\n");
        // Verifies columns are counted in Unicode scalar value units, not bytes (D-COL-03).
        assert_eq!(sm.slice(span(id, 1, 1, 1, 2)), "â");
        assert_eq!(sm.slice(span(id, 1, 2, 1, 3)), "ê");
        assert_eq!(sm.slice(span(id, 1, 3, 1, 4)), "î");
        assert_eq!(sm.slice(span(id, 1, 1, 1, 4)), "âêî");
    }

    #[test]
    fn slice_mixed_ascii_and_multibyte_columns() {
        // "x = â": col1='x' col2=' ' col3='=' col4=' ' col5='â'
        let (sm, id) = build("x = â\n");
        assert_eq!(sm.slice(span(id, 1, 5, 1, 6)), "â");
        assert_eq!(sm.slice(span(id, 1, 1, 1, 2)), "x");
    }

    #[test]
    fn slice_emoji_counts_as_single_column() {
        // 🎉 (U+1F389) is a single Rust char (a single Unicode scalar value), 4 bytes in UTF-8.
        let (sm, id) = build("x = 🎉\n");
        assert_eq!(sm.slice(span(id, 1, 5, 1, 6)), "🎉");
        // The column right after the emoji (6) through just before the newline is empty.
        assert_eq!(sm.slice(span(id, 1, 6, 1, 6)), "");
    }

    #[test]
    fn slice_emoji_and_multibyte_together() {
        let (sm, id) = build("🎉â🎉\n");
        assert_eq!(sm.slice(span(id, 1, 1, 1, 2)), "🎉");
        assert_eq!(sm.slice(span(id, 1, 2, 1, 3)), "â");
        assert_eq!(sm.slice(span(id, 1, 3, 1, 4)), "🎉");
        assert_eq!(sm.slice(span(id, 1, 1, 1, 4)), "🎉â🎉");
    }

    #[test]
    fn slice_end_of_line_clamped_at_newline() {
        let (sm, id) = build("ab\ncd\n");
        // An end pointing right after "ab" (column 3) points to just before the '\n' (i.e.
        // exactly the line's actual character count).
        assert_eq!(sm.slice(span(id, 1, 1, 1, 3)), "ab");
    }

    #[test]
    fn slice_last_line_without_trailing_newline() {
        let (sm, id) = build("ab\nxyz");
        // Even on a final line with no trailing newline, a column beyond the actual
        // character count is clamped to the end of the text.
        assert_eq!(sm.slice(span(id, 2, 1, 2, 4)), "xyz");
        assert_eq!(sm.slice(span(id, 2, 1, 2, 100)), "xyz");
    }

    #[test]
    fn slice_reversed_span_is_normalized() {
        let (sm, id) = build("hello\n");
        // Even when start > end (a broken Span), this does not panic; the range is
        // normalized before slicing.
        assert_eq!(sm.slice(span(id, 1, 6, 1, 1)), "hello");
    }

    #[test]
    fn slice_empty_span_is_empty_string() {
        let (sm, id) = build("hello\n");
        assert_eq!(sm.slice(span(id, 1, 3, 1, 3)), "");
    }

    #[test]
    fn slice_out_of_range_line_clamps_to_text_end() {
        let (sm, id) = build("abc\n");
        // A Position pointing to a line that does not exist is clamped to the end of the
        // text, and does not panic.
        assert_eq!(sm.slice(span(id, 99, 1, 99, 5)), "");
    }
}
