//! Attaches side-stream comments to the AST (leading/trailing) by matching line numbers
//! (ARCHITECTURE.md §5.9). This "attach comments to a following/same-line AST node based on
//! line number" mechanism is the single implementation shared by D-DOC-03 (deciding which
//! declaration a doc comment targets) and fmt's general-comment preservation.
//!
//! # Coverage (a decision made in this parser implementation)
//!
//! `Stmt`/`MatchArm`/`EnumVariant`/`FunctionDecl`/`StructDecl`/`EnumDecl` all carry fmt's
//! `leading_comments` (each comment line's text plus its actual source line number,
//! `LeadingComment`, ast/decl.rs) and `trailing_comment`; `FunctionDecl`/`StructDecl`/
//! `EnumDecl`/`Stmt` additionally carry `doc_comment` (`##`, D-DOC-01 through 03). Keeping
//! the line number lets fmt (`printer.rs`) restore blank lines (D-SYN-02) that were inside a
//! comment block or between it and the code body that follows. `Param` (function parameters,
//! struct fields) has no comment field on the ast/decl.rs side, so a comment right before a
//! field is carried forward to the next attachment target (the next field/method, or the
//! next declaration after the current one closes) -- a known structural limitation because
//! the existing ast type definitions do not anticipate per-field comment retention, and there
//! is no test under samples/ exercising this case. Likewise, comments between elements of a
//! literal/argument list (`[1, # first\n 2]`) are also unsupported (the same known
//! limitation as the R8 decision in ARCHITECTURE.md §5.9).

use crate::ast::{
    Block, Decl, DocComment, DocFence, DocPart, ElseBranch, Expr, ExprKind, FStringSegment, IfExpr,
    Item, LeadingComment, MatchArm, MatchArmBody, Module, PipeCallee, Stmt, StmtKind,
};
use crate::diagnostics::Span;
use crate::lexer::comments::RawComment;
use std::collections::VecDeque;

/// Assigns the `RawComment` sequence collected by lexing to the `leading_comments`/
/// `trailing_comment` of `Stmt`/`MatchArm`/`EnumVariant`/`FunctionDecl`/`StructDecl`/
/// `EnumDecl` within `module`, and to the `doc_comment` (a `##` fence, D-DOC-01 through 03)
/// of `FunctionDecl`/`StructDecl`/`EnumDecl`/`Stmt` (`NameAssign` only).
pub fn attach_comments(module: &mut Module, comments: Vec<RawComment>) {
    let mut queue: VecDeque<RawComment> = comments.into_iter().collect();
    for item in &mut module.items {
        match item {
            Item::Decl(decl) => attach_to_decl(decl, &mut queue),
            Item::Stmt(stmt) => attach_to_stmt(stmt, &mut queue),
        }
    }
    module.trailing_comments = to_leading_comments(queue.into_iter().collect());
}

fn attach_to_decl(decl: &mut Decl, queue: &mut VecDeque<RawComment>) {
    match decl {
        Decl::Function(f) => {
            let leading = take_leading_upto(queue, f.span.start.line);
            let (doc, generic) = split_doc_run(leading, f.span);
            f.leading_comments = generic;
            f.doc_comment = doc;
            attach_to_block(&mut f.body, queue);
        }
        Decl::Struct(s) => {
            let leading = take_leading_upto(queue, s.span.start.line);
            let (doc, generic) = split_doc_run(leading, s.span);
            s.leading_comments = generic;
            s.doc_comment = doc;
            for (index, field) in s.fields.iter().enumerate() {
                s.field_leading_comments[index] =
                    to_leading_comments(take_leading_upto(queue, field.span.start.line));
                s.field_trailing_comments[index] = take_trailing(queue, field.span.end.line);
            }
            for method in &mut s.methods {
                let m_leading = take_leading_upto(queue, method.span.start.line);
                let (m_doc, m_generic) = split_doc_run(m_leading, method.span);
                method.leading_comments = m_generic;
                method.doc_comment = m_doc;
                attach_to_block(&mut method.body, queue);
            }
        }
        Decl::Enum(e) => {
            let leading = take_leading_upto(queue, e.span.start.line);
            let (doc, generic) = split_doc_run(leading, e.span);
            e.leading_comments = generic;
            e.doc_comment = doc;
            for variant in &mut e.variants {
                let v_leading = take_leading_upto(queue, variant.span.start.line);
                variant.leading_comments = to_leading_comments(v_leading);
                variant.trailing_comment = take_trailing(queue, variant.span.end.line);
            }
        }
    }
}

fn attach_to_stmt(stmt: &mut Stmt, queue: &mut VecDeque<RawComment>) {
    let leading = take_leading_upto(queue, stmt.span.start.line);
    let (doc, generic) = split_doc_run(leading, stmt.span);
    stmt.doc_comment = doc;
    stmt.leading_comments = generic;
    stmt.trailing_comment = take_trailing(queue, stmt.span.end.line);
    attach_within_stmt(stmt, queue);
}

fn attach_to_block(block: &mut Block, queue: &mut VecDeque<RawComment>) {
    for stmt in &mut block.stmts {
        attach_to_stmt(stmt, queue);
    }
}

/// Recursively walks the expression(s) held by a `Stmt`'s body, propagating comment
/// attachment into any `If`/`Match`/`Lambda` block or arm nested inside it.
fn attach_within_stmt(stmt: &mut Stmt, queue: &mut VecDeque<RawComment>) {
    match &mut stmt.kind {
        StmtKind::VarDecl { value, .. } | StmtKind::NameAssign { value, .. } => {
            attach_within_expr(value, queue);
        }
        StmtKind::FieldAssign { target, value, .. } => {
            attach_within_expr(target, queue);
            attach_within_expr(value, queue);
        }
        StmtKind::IndexAssign {
            target,
            index,
            value,
        } => {
            attach_within_expr(target, queue);
            attach_within_expr(index, queue);
            attach_within_expr(value, queue);
        }
        StmtKind::Discard(expr) | StmtKind::ExprStmt(expr) | StmtKind::Return(Some(expr)) => {
            attach_within_expr(expr, queue);
        }
        StmtKind::Return(None) => {}
    }
}

/// Finds and recurses into any `If`/`Match`/`Lambda` embedded within an expression (each of
/// which contains a sequence of statements or arms). Literals and identifiers contain no
/// such statements/arms, so nothing is done for them.
fn attach_within_expr(expr: &mut Expr, queue: &mut VecDeque<RawComment>) {
    match &mut expr.kind {
        ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::Ident(_) => {}
        ExprKind::FString(segments) => {
            for seg in segments {
                if let FStringSegment::Expr(e) = seg {
                    attach_within_expr(e, queue);
                }
            }
        }
        ExprKind::ListLit { elements, .. }
        | ExprKind::SetLit { elements, .. }
        | ExprKind::TupleLit { elements, .. }
        | ExprKind::Par { elements, .. } => {
            for e in elements {
                attach_within_expr(e, queue);
            }
        }
        ExprKind::DictLit { entries, .. } => {
            for (k, v) in entries {
                attach_within_expr(k, queue);
                attach_within_expr(v, queue);
            }
        }
        ExprKind::Unary { operand, .. } => attach_within_expr(operand, queue),
        ExprKind::Binary { lhs, rhs, .. } => {
            attach_within_expr(lhs, queue);
            attach_within_expr(rhs, queue);
        }
        ExprKind::Call { callee, args, .. } => {
            attach_within_expr(callee, queue);
            for a in args {
                attach_within_expr(&mut a.value, queue);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            attach_within_expr(receiver, queue);
            for a in args {
                attach_within_expr(&mut a.value, queue);
            }
        }
        ExprKind::FieldAccess { target, .. } | ExprKind::TupleIndex { target, .. } => {
            attach_within_expr(target, queue);
        }
        ExprKind::Index { target, index } => {
            attach_within_expr(target, queue);
            attach_within_expr(index, queue);
        }
        ExprKind::Question { target } => attach_within_expr(target, queue),
        ExprKind::Pipe(pipe) => {
            attach_within_expr(&mut pipe.source, queue);
            for stage in &mut pipe.stages {
                match &mut stage.callee {
                    PipeCallee::Bare(e) => attach_within_expr(e, queue),
                    PipeCallee::WithArgs { callee, args } => {
                        attach_within_expr(callee, queue);
                        for a in args {
                            attach_within_expr(&mut a.value, queue);
                        }
                    }
                }
            }
        }
        ExprKind::Lambda { body, .. } => attach_within_expr(body, queue),
        ExprKind::If(if_expr) => attach_within_if(if_expr, queue),
        ExprKind::Match { scrutinee, arms } => {
            attach_within_expr(scrutinee, queue);
            attach_to_match_arms(arms, queue);
        }
        ExprKind::Grouping(inner) => attach_within_expr(inner, queue),
    }
}

fn attach_within_if(if_expr: &mut IfExpr, queue: &mut VecDeque<RawComment>) {
    attach_within_expr(&mut if_expr.cond, queue);
    attach_to_block(&mut if_expr.then_branch, queue);
    match &mut if_expr.else_branch {
        ElseBranch::Block(block) => attach_to_block(block, queue),
        ElseBranch::ElseIf(inner_if) => attach_within_if(inner_if, queue),
    }
}

fn attach_to_match_arms(arms: &mut [MatchArm], queue: &mut VecDeque<RawComment>) {
    for arm in arms.iter_mut() {
        let leading = take_leading_upto(queue, arm.span.start.line);
        // MatchArm has no doc_comment (not a D-DOC-03 target), so the raw text is stored
        // directly into leading_comments.
        arm.leading_comments = to_leading_comments(leading);
        arm.trailing_comment = take_trailing(queue, arm.span.end.line);
        match &mut arm.body {
            MatchArmBody::Expr(e) => attach_within_expr(e, queue),
            MatchArmBody::Block(block) => attach_to_block(block, queue),
        }
    }
}

/// Pulls every comment (regardless of whether it is trailing) off the front of `queue` that
/// sits on a line before `before_line`. Because callers are required to always follow the
/// order "for each node: leading -> recurse -> trailing", any "trailing comment on a
/// preceding line" still remaining at this point must already have been consumed by
/// `take_trailing` while processing the previous node, so it can never be mixed in here.
fn take_leading_upto(queue: &mut VecDeque<RawComment>, before_line: u32) -> Vec<RawComment> {
    let mut taken = Vec::new();
    while queue
        .front()
        .is_some_and(|c| c.span.start.line < before_line)
    {
        if let Some(c) = queue.pop_front() {
            taken.push(c);
        } else {
            break;
        }
    }
    taken
}

/// If the front of `queue` is a trailing comment on line `line`, pops it and returns its
/// text (stripping the conventional single space right after `#`/`##`, the inverse of
/// D-FMT-03's processing -- the same normalization as doc body text).
fn take_trailing(queue: &mut VecDeque<RawComment>, line: u32) -> Option<String> {
    if queue
        .front()
        .is_some_and(|c| c.is_trailing && c.span.start.line == line)
    {
        queue.pop_front().map(|c| strip_one_leading_space(&c.text))
    } else {
        None
    }
}

/// Converts a raw comment sequence into a `LeadingComment` sequence for `fmt`, keeping the
/// line numbers (stripping the conventional single space right after `#`/`##`, the inverse
/// of D-FMT-03's processing).
fn to_leading_comments(raw: Vec<RawComment>) -> Vec<LeadingComment> {
    raw.into_iter()
        .map(|c| LeadingComment {
            text: strip_one_leading_space(&c.text),
            line: c.span.start.line,
        })
        .collect()
}

/// From the end of the leading zone, cuts out a run of consecutive-line `##` lines as a
/// single doc-comment run (D-DOC-01 through 03). If none can be cut out, everything is
/// routed to the raw text on the `leading_comments` side instead.
fn split_doc_run(
    leading: Vec<RawComment>,
    fallback_span: Span,
) -> (Option<DocComment>, Vec<LeadingComment>) {
    let mut split_at = leading.len();
    let mut expected_next_line: Option<u32> = None;
    for (i, c) in leading.iter().enumerate().rev() {
        if !c.is_doc {
            break;
        }
        if let Some(next_line) = expected_next_line
            && c.span.start.line + 1 != next_line
        {
            break;
        }
        expected_next_line = Some(c.span.start.line);
        split_at = i;
    }
    if split_at >= leading.len() {
        return (None, to_leading_comments(leading));
    }
    let mut iter = leading.into_iter();
    let generic: Vec<RawComment> = iter.by_ref().take(split_at).collect();
    let doc_lines: Vec<RawComment> = iter.collect();
    (
        Some(build_doc_comment(doc_lines, fallback_span)),
        to_leading_comments(generic),
    )
}

/// Converts a run of consecutive `##` lines (`doc_lines`, non-empty) into a `DocComment`
/// (prose lines + fence sequence). Each line's text has exactly one conventional space
/// right after `## ` stripped (the inverse of D-FMT-03's processing -- actual indentation
/// inside a fence remains as extra leading whitespace beyond that one space).
fn build_doc_comment(doc_lines: Vec<RawComment>, fallback_span: Span) -> DocComment {
    let overall_span = match (doc_lines.first(), doc_lines.last()) {
        (Some(first), Some(last)) => Span {
            file: first.span.file,
            start: first.span.start,
            end: last.span.end,
        },
        _ => fallback_span,
    };

    let mut prose_lines = Vec::new();
    let mut fences = Vec::new();
    let mut parts = Vec::new();
    let mut open_fence: Option<Option<String>> = None;
    let mut fence_body_start_line = 0u32;
    let mut fence_body: Vec<String> = Vec::new();
    let mut fence_start_span = overall_span;

    for c in &doc_lines {
        let normalized = strip_one_leading_space(&c.text);
        let trimmed = normalized.trim();
        match open_fence.take() {
            Some(tag) => {
                if trimmed.starts_with("```") {
                    parts.push(DocPart::Fence(fences.len()));
                    fences.push(DocFence {
                        lang_tag: tag,
                        body_start_line: fence_body_start_line,
                        raw_text: fence_body.join("\n"),
                        span: Span {
                            file: fence_start_span.file,
                            start: fence_start_span.start,
                            end: c.span.end,
                        },
                    });
                    fence_body = Vec::new();
                } else {
                    fence_body.push(normalized);
                    open_fence = Some(tag);
                }
            }
            None => {
                if trimmed.starts_with("```") {
                    let tag_text = trimmed.trim_start_matches('`').trim();
                    open_fence = Some(if tag_text.is_empty() {
                        None
                    } else {
                        Some(tag_text.to_owned())
                    });
                    fence_body_start_line = c.span.start.line + 1;
                    fence_body = Vec::new();
                    fence_start_span = c.span;
                } else {
                    parts.push(DocPart::Prose(prose_lines.len()));
                    prose_lines.push(normalized);
                }
            }
        }
    }
    // If a fence is never closed by the end of the file, that incomplete fence (already
    // syntactically broken) is discarded rather than returned to the prose side -- there is
    // no test under samples/ for this case.

    DocComment {
        prose_lines,
        fences,
        parts,
        span: overall_span,
    }
}

fn strip_one_leading_space(text: &str) -> String {
    text.strip_prefix(' ').unwrap_or(text).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Decl, StmtKind};
    use crate::diagnostics::FileId;
    use crate::lexer::Lexer;

    fn lex_parse_and_attach(src: &str) -> Module {
        let file = FileId(0);
        let (tokens, comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "lexing error: {src:?}");
        let (mut module, parse_diag) = crate::parser::parse_module(&tokens, file);
        assert!(parse_diag.is_empty(), "parsing error: {src:?}");
        attach_comments(&mut module, comments);
        module
    }

    fn first_function(module: &Module) -> &crate::ast::FunctionDecl {
        match &module.items[0] {
            Item::Decl(Decl::Function(f)) => f,
            _ => panic!("expected the first Item to be Decl::Function"),
        }
    }

    #[test]
    fn doc_comment_with_single_untagged_fence_attaches_to_function() {
        let src = "## Doubles n.\n##\n## ```\n## assert(f(2) == 4)\n## ```\ndef f(n: int): int\n    return n * 2\n";
        let module = lex_parse_and_attach(src);
        let f = first_function(&module);
        let Some(doc) = &f.doc_comment else {
            panic!("doc_comment was not attached");
        };
        assert_eq!(
            doc.prose_lines,
            vec!["Doubles n.".to_owned(), String::new()]
        );
        assert_eq!(doc.fences.len(), 1);
        assert_eq!(doc.fences[0].lang_tag, None);
        assert_eq!(doc.fences[0].raw_text, "assert(f(2) == 4)");
        // The fence body's actual file line number (D-DOC-05): line 4, 1-indexed.
        assert_eq!(doc.fences[0].body_start_line, 4);
    }

    #[test]
    fn doc_comment_language_tagged_fence_is_recorded_with_its_tag() {
        // D-DOC-01: a language-tagged fence is also recorded as a DocFence, but its tag is
        // kept -- on the premise that doctest collection (Unit16) decides whether something
        // is a test target based on whether a tag is present.
        let src = "## Example output.\n##\n## ```json\n## {\"a\": 1}\n## ```\ndef f(): int\n    return 1\n";
        let module = lex_parse_and_attach(src);
        let f = first_function(&module);
        let Some(doc) = &f.doc_comment else {
            panic!("doc_comment was not attached");
        };
        assert_eq!(doc.fences.len(), 1);
        assert_eq!(doc.fences[0].lang_tag, Some("json".to_owned()));
        assert_eq!(doc.fences[0].raw_text, "{\"a\": 1}");
    }

    #[test]
    fn doc_comment_multiple_fences_all_captured_in_order() {
        // Same shape as
        // samples/doctest/passing_multiple_blocks_same_declaration/entry_main.ybm: three in
        // a row -- a plain fence -> a language-tagged fence (ignored) -> a plain fence.
        let src = concat!(
            "## Adds two ints.\n",
            "##\n",
            "## ```\n",
            "## assert(add(1, 2) == 3)\n",
            "## ```\n",
            "##\n",
            "## Example of the output format (not a test target since it's language-tagged).\n",
            "##\n",
            "## ```json\n",
            "## {\"a\": 1, \"b\": 2}\n",
            "## ```\n",
            "##\n",
            "## Also verify with a different addition pattern.\n",
            "##\n",
            "## ```\n",
            "## assert(add(10, 20) == 30)\n",
            "## ```\n",
            "def add(a: int, b: int): int\n",
            "    return a + b\n",
        );
        let module = lex_parse_and_attach(src);
        let f = first_function(&module);
        let Some(doc) = &f.doc_comment else {
            panic!("doc_comment was not attached");
        };
        assert_eq!(doc.fences.len(), 3);
        assert_eq!(doc.fences[0].lang_tag, None);
        assert_eq!(doc.fences[0].raw_text, "assert(add(1, 2) == 3)");
        assert_eq!(doc.fences[1].lang_tag, Some("json".to_owned()));
        assert_eq!(doc.fences[2].lang_tag, None);
        assert_eq!(doc.fences[2].raw_text, "assert(add(10, 20) == 30)");
    }

    #[test]
    fn generic_comments_attach_with_leading_space_stripped() {
        let src = "x = 1  # trailing note\n# leading note for y\ny = 2\n";
        let module = lex_parse_and_attach(src);
        let Item::Stmt(first) = &module.items[0] else {
            panic!("expected the first Item to be a statement");
        };
        assert_eq!(first.trailing_comment, Some("trailing note".to_owned()));
        let Item::Stmt(second) = &module.items[1] else {
            panic!("expected the second Item to be a statement");
        };
        assert_eq!(second.leading_comments.len(), 1);
        assert_eq!(second.leading_comments[0].text, "leading note for y");
        assert_eq!(second.leading_comments[0].line, 2);
    }

    #[test]
    fn comment_inside_if_block_attaches_to_nested_statement() {
        let src = "y = if x > 0\n    # positive branch\n    1\nelse\n    2\n";
        let module = lex_parse_and_attach(src);
        let Item::Stmt(stmt) = &module.items[0] else {
            panic!("expected the first Item to be a statement");
        };
        let StmtKind::NameAssign { value, .. } = &stmt.kind else {
            panic!("expected NameAssign");
        };
        let crate::ast::ExprKind::If(if_expr) = &value.kind else {
            panic!("expected an If expression");
        };
        let inner = &if_expr.then_branch.stmts[0];
        assert_eq!(inner.leading_comments.len(), 1);
        assert_eq!(inner.leading_comments[0].text, "positive branch");
    }

    #[test]
    fn struct_field_free_comment_does_not_panic_and_method_doc_still_attaches() {
        // Param has no comment field, so a comment right before a field is not retained
        // (a known limitation, see the documentation at the top of this file). This
        // verifies that even in that case there is no crash, and the following method's
        // doc_comment still attaches correctly.
        let src = concat!(
            "struct Counter\n",
            "    # This comment is not retained (known limitation)\n",
            "    value: int\n",
            "\n",
            "    ## Returns the value.\n",
            "    ##\n",
            "    ## ```\n",
            "    ## assert(true)\n",
            "    ## ```\n",
            "    def get(self): int\n",
            "        return self.value\n",
        );
        let module = lex_parse_and_attach(src);
        let Item::Decl(Decl::Struct(s)) = &module.items[0] else {
            panic!("expected the first Item to be a struct");
        };
        assert_eq!(s.fields.len(), 1);
        let method = &s.methods[0];
        let Some(doc) = &method.doc_comment else {
            panic!("the method's doc_comment was not attached");
        };
        assert_eq!(doc.fences.len(), 1);
        assert_eq!(doc.fences[0].raw_text, "assert(true)");
    }

    #[test]
    fn decl_level_leading_comment_is_no_longer_discarded() {
        // Cause A (must-fix per owner ruling): an unmarked `#` comment immediately before
        // a `##` doc comment must be retained as FunctionDecl/StructDecl/EnumDecl's
        // leading_comments.
        let src = "# general comment\n## doc body\ndef f(): int\n    return 1\n";
        let module = lex_parse_and_attach(src);
        let f = first_function(&module);
        assert_eq!(f.leading_comments.len(), 1);
        assert_eq!(f.leading_comments[0].text, "general comment");
        assert!(f.doc_comment.is_some());
    }

    #[test]
    fn decl_leading_comment_without_doc_comment_is_kept() {
        let src = "# note before struct\nstruct S\n    x: int\n";
        let module = lex_parse_and_attach(src);
        let Item::Decl(Decl::Struct(s)) = &module.items[0] else {
            panic!("expected the first Item to be a struct");
        };
        assert_eq!(s.leading_comments.len(), 1);
        assert_eq!(s.leading_comments[0].text, "note before struct");
    }
}
