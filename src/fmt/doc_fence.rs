//! Excludes code inside doc-comment fences from formatting (D-FMT-06, ARCHITECTURE.md §5.9).

use crate::ast::{DocComment, DocPart};

/// Prose is normalized while fence bodies remain byte-for-byte unchanged.
pub fn render_doc_comment(doc: &DocComment) -> String {
    let mut lines = Vec::new();
    for part in &doc.parts {
        match part {
            DocPart::Prose(index) => lines.push(format_prose_line(&doc.prose_lines[*index])),
            DocPart::Fence(index) => {
                let fence = &doc.fences[*index];
                let tag = fence.lang_tag.as_deref().unwrap_or("");
                lines.push(format!("## ```{tag}"));
                if !fence.raw_text.is_empty() {
                    for raw_line in fence.raw_text.split('\n') {
                        lines.push(format_fence_body_line(raw_line));
                    }
                }
                lines.push("## ```".to_owned());
            }
        }
    }
    lines.join("\n")
}

/// A prose line (explanatory text outside a fence) is subject to D-FMT-03 — all leading
/// whitespace is dropped, and it becomes `## `+content if the content is non-empty, or a
/// bare `##` if empty.
fn format_prose_line(text: &str) -> String {
    let t = text.trim_start();
    if t.is_empty() {
        "##".to_owned()
    } else {
        format!("## {t}")
    }
}

/// A fence-body line is exempt from D-FMT-06 — nothing is changed except for prefixing the
/// conventional single `## ` (leading whitespace is not trimmed either).
fn format_fence_body_line(raw_line: &str) -> String {
    if raw_line.is_empty() {
        "##".to_owned()
    } else {
        format!("## {raw_line}")
    }
}
