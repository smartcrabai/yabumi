use crate::diagnostics::{Position, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf16,
    Utf32,
}

pub(crate) fn to_lsp_pos(text: &str, pos: Position, encoding: Encoding) -> (u32, u32) {
    let requested_line = pos.line.saturating_sub(1);
    let (line, line_text) = line_at(text, requested_line);
    let requested_column = pos.col.saturating_sub(1);
    (line, to_lsp_column(line_text, requested_column, encoding))
}

pub(crate) fn from_lsp_pos(text: &str, line: u32, character: u32, encoding: Encoding) -> Position {
    let (line, line_text) = line_at(text, line);
    let col = from_lsp_column(line_text, character, encoding);
    Position {
        line: line.saturating_add(1),
        col: col.saturating_add(1),
    }
}

pub(crate) fn span_to_range(
    text: &str,
    span: Span,
    encoding: Encoding,
) -> ((u32, u32), (u32, u32)) {
    (
        to_lsp_pos(text, span.start, encoding),
        to_lsp_pos(text, span.end, encoding),
    )
}

fn line_at(text: &str, requested_line: u32) -> (u32, &str) {
    let mut last = (0, "");
    for (index, line) in text.split('\n').enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let line = line.strip_suffix('\r').unwrap_or(line);
        last = (index, line);
        if index >= requested_line {
            return last;
        }
    }
    last
}

pub(crate) fn document_end(text: &str, encoding: Encoding) -> (u32, u32) {
    let mut line = 0;
    let mut line_text = "";
    for (index, current) in text.split('\n').enumerate() {
        line = u32::try_from(index).unwrap_or(u32::MAX);
        line_text = current.strip_suffix('\r').unwrap_or(current);
    }
    let column = u32::try_from(line_text.chars().count()).unwrap_or(u32::MAX);
    (line, to_lsp_column(line_text, column, encoding))
}

fn char_width(character: char, encoding: Encoding) -> u32 {
    match encoding {
        Encoding::Utf16 => u32::try_from(character.len_utf16()).unwrap_or(u32::MAX),
        Encoding::Utf32 => 1,
    }
}

fn to_lsp_column(line: &str, requested_column: u32, encoding: Encoding) -> u32 {
    line.chars()
        .take(requested_column as usize)
        .map(|character| char_width(character, encoding))
        .fold(0, u32::saturating_add)
}

fn from_lsp_column(line: &str, requested_column: u32, encoding: Encoding) -> u32 {
    let mut column = 0u32;
    let mut lsp_column = 0u32;
    for character in line.chars() {
        let width = char_width(character, encoding);
        if lsp_column.saturating_add(width) > requested_column {
            break;
        }
        lsp_column = lsp_column.saturating_add(width);
        column = column.saturating_add(1);
    }
    column
}

#[cfg(test)]
mod tests {
    use crate::diagnostics::{FileId, Position, Span};

    use super::{Encoding, from_lsp_pos, span_to_range, to_lsp_pos};

    #[test]
    fn converts_emoji_columns_for_utf16_and_utf32() {
        let text = "\u{1f600}\u{65e5}\u{672c}\n";
        assert_eq!(
            to_lsp_pos(text, Position { line: 1, col: 2 }, Encoding::Utf16),
            (0, 2)
        );
        assert_eq!(
            to_lsp_pos(text, Position { line: 1, col: 2 }, Encoding::Utf32),
            (0, 1)
        );
        assert_eq!(
            from_lsp_pos(text, 0, 2, Encoding::Utf16),
            Position { line: 1, col: 2 }
        );
        assert_eq!(
            from_lsp_pos(text, 0, 1, Encoding::Utf32),
            Position { line: 1, col: 2 }
        );
    }

    #[test]
    fn clamps_positions_to_the_available_line_and_column() {
        let text = "abc\ndef";
        assert_eq!(
            to_lsp_pos(text, Position { line: 9, col: 99 }, Encoding::Utf16),
            (1, 3)
        );
        assert_eq!(
            from_lsp_pos(text, 9, 99, Encoding::Utf16),
            Position { line: 2, col: 4 }
        );
    }

    #[test]
    fn converts_span_endpoints() {
        let span = Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 3 },
        };
        assert_eq!(
            span_to_range("\u{65e5}\u{672c}", span, Encoding::Utf16),
            ((0, 0), (0, 2))
        );
    }
}
