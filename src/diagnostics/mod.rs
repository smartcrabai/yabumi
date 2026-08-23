//! Diagnostics infrastructure (ARCHITECTURE.md §3.2). Provides the types that support
//! generating the `file:line:col [Exxxx] message` format, the every-diagnostic collection in
//! which every phase receives `&mut DiagnosticBag` and pushes its findings, ascending
//! `file:line:col` sorting, and exit-code determination in the driver.

pub mod codes;
pub mod source_map;

pub use codes::ErrorCode;
pub use source_map::{FileId, Position, SourceFile, SourceMap, Span};

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: ErrorCode,
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    /// `file:line:col [Exxxx] message` (SPEC §1; D-ERR-05 also requires this same format for panic-style errors).
    #[must_use]
    pub fn render(&self, sources: &SourceMap) -> String {
        format!(
            "{}:{}:{} [{}] {}",
            sources.path(self.span.file).display(),
            self.span.start.line,
            self.span.start.col,
            self.code,
            self.message,
        )
    }
}

/// A diagnostics container shared by every phase. Implements D-CLI-03 (collect everything, sort ascending).
#[derive(Debug, Default)]
pub struct DiagnosticBag {
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticBag {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }

    #[must_use]
    pub fn has_any(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Extracts diagnostics in collection order. Used in contexts with no SourceMap available
    /// (single-phase unit tests, checks within a single file where order does not matter).
    /// For diagnostics shown to the user, use `into_sorted` instead (D-CLI-03).
    #[must_use]
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    /// A decision made here: SPEC/DECISIONS does not specify the ordering across files.
    /// The file path string's lexicographic order is used as the primary key, with line and
    /// col as the secondary and tertiary keys (within a single file this is always exactly
    /// ascending file:line:col order, satisfying D-CLI-03, ARCHITECTURE.md §3.2/§4.4).
    #[must_use]
    pub fn into_sorted(mut self, sources: &SourceMap) -> Vec<Diagnostic> {
        self.diagnostics.sort_by(|a, b| {
            sources
                .path(a.span.file)
                .cmp(sources.path(b.span.file))
                .then(a.span.start.line.cmp(&b.span.start.line))
                .then(a.span.start.col.cmp(&b.span.start.col))
        });
        self.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, DiagnosticBag, ErrorCode, FileId, Position, SourceMap, Span};
    use std::path::PathBuf;

    fn dummy_span(file: FileId, line: u32, col: u32) -> Span {
        Span {
            file,
            start: Position { line, col },
            end: Position { line, col },
        }
    }

    #[test]
    fn render_formats_file_line_col_code_message() {
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("entry_main.ybm"), "x = 1\n".to_owned());
        let diag = Diagnostic {
            code: ErrorCode::DuplicateName,
            span: dummy_span(file, 3, 5),
            message: "duplicate definition of 'x'".to_owned(),
        };
        assert_eq!(
            diag.render(&sources),
            "entry_main.ybm:3:5 [E1001] duplicate definition of 'x'"
        );
    }

    #[test]
    fn into_sorted_orders_by_line_then_col() {
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("entry_main.ybm"), String::new());
        let mut bag = DiagnosticBag::new();
        bag.push(Diagnostic {
            code: ErrorCode::UnusedVariable,
            span: dummy_span(file, 5, 1),
            message: String::new(),
        });
        bag.push(Diagnostic {
            code: ErrorCode::UnusedVariable,
            span: dummy_span(file, 2, 9),
            message: String::new(),
        });
        bag.push(Diagnostic {
            code: ErrorCode::UnusedVariable,
            span: dummy_span(file, 2, 1),
            message: String::new(),
        });
        let sorted = bag.into_sorted(&sources);
        let lines_cols: Vec<(u32, u32)> = sorted
            .iter()
            .map(|d| (d.span.start.line, d.span.start.col))
            .collect();
        assert_eq!(lines_cols, vec![(2, 1), (2, 9), (5, 1)]);
    }

    #[test]
    fn into_sorted_orders_diagnostics_across_files() {
        let mut sources = SourceMap::new();
        let z_file = sources.add(PathBuf::from("z.ybm"), String::new());
        let a_file = sources.add(PathBuf::from("a.ybm"), String::new());
        let mut bag = DiagnosticBag::new();
        bag.push(Diagnostic {
            code: ErrorCode::UnusedVariable,
            span: dummy_span(z_file, 1, 1),
            message: String::new(),
        });
        bag.push(Diagnostic {
            code: ErrorCode::UnusedVariable,
            span: dummy_span(a_file, 9, 1),
            message: String::new(),
        });
        let sorted = bag.into_sorted(&sources);
        assert_eq!(
            sources.path(sorted[0].span.file),
            std::path::Path::new("a.ybm")
        );
        assert_eq!(
            sources.path(sorted[1].span.file),
            std::path::Path::new("z.ybm")
        );
    }
}
