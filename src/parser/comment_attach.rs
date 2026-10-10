//! Attaches side-stream comments to the AST by matching line numbers (ARCHITECTURE.md §5.9).
//! This "attach comments to a following/same-line AST node based on line number" mechanism is
//! the single implementation shared by D-DOC-03 (deciding which declaration a doc comment
//! targets) and fmt's general-comment preservation.
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
//! is no test under samples/ exercising this case.
//!
//! A trailing comment is attached on a node's true last line (`ast/end_line.rs`), not on its
//! span end, because the parser's span end for a block-bodied node points at the next sibling.
//! A node takes its leading comments, then its trailing comment, then the comments inside it.
//! Taking the trailing comment before descending gives it to the outermost node ending on that
//! line: in `xs.map((v) =>\n    if v\n        1\n    else\n        2) # c`, `# c` follows the
//! `)`, so it belongs to the statement, not to the nested `2`.
//!
//! Header comments are stored in `header_comment` of the block (or of `Lambda`/`Match`/
//! `StructDecl`/`EnumDecl`) that the header line opens: a `def` signature, `struct`/`enum`
//! lines, `if`/`else` lines, a `match` scrutinee line, a block match arm's `pattern =>` line,
//! and a lambda's `=>` line when its `if`/`match` body starts on the next line.
//!
//! Comments inside a bracketed list (a list/dict/set/tuple/`par` literal, or a call's argument
//! list, including pipe stages `f(_, ..)`) are stored in `ListComments::attached`: the comment
//! after the opening bracket, standalone lines before an element, the comment after an
//! element, and standalone lines before the closing bracket. A list with attached comments is
//! printed one element per line.
//!
//! Unsupported positions: comments on chain or pipe continuation lines (`.m()` or `|>` lines),
//! multi-line `def` and lambda parameter lists, type arguments, index brackets, patterns, and
//! grouping parentheses. No node claims such a comment: it stays in the stream and becomes the
//! leading comment of a later node (or an entry of `Module::trailing_comments`), so fmt
//! relocates it.

use crate::ast::end_line::{
    true_end_line_of_block, true_end_line_of_expr, true_end_line_of_match_arm,
    true_end_line_of_stmt,
};
use crate::ast::{
    Arg, AttachedListComments, Block, Decl, DocComment, DocFence, DocPart, ElseBranch, Expr,
    ExprKind, FStringSegment, IfExpr, Item, LeadingComment, ListComments, MatchArm, MatchArmBody,
    Module, PipeCallee, PipeExpr, Stmt, StmtKind,
};
use crate::diagnostics::Span;
use crate::lexer::comments::RawComment;
use std::collections::VecDeque;

/// Assigns the `RawComment` sequence collected by lexing to the `leading_comments`/
/// `trailing_comment` of `Stmt`/`MatchArm`/`EnumVariant`/`FunctionDecl`/`StructDecl`/
/// `EnumDecl` within `module`, to the `doc_comment` (a `##` fence, D-DOC-01 through 03)
/// of `FunctionDecl`/`StructDecl`/`EnumDecl`/`Stmt` (`NameAssign` only), and to the
/// `header_comment` and `ListComments::attached` fields.
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
            attach_function_body(&mut f.body, f.ret.span.end.line, queue);
        }
        Decl::Struct(s) => {
            let leading = take_leading_upto(queue, s.span.start.line);
            let (doc, generic) = split_doc_run(leading, s.span);
            s.leading_comments = generic;
            s.doc_comment = doc;
            s.header_comment = take_trailing_on(queue, s.span.start.line);
            for (index, field) in s.fields.iter().enumerate() {
                s.field_leading_comments[index] =
                    to_leading_comments(take_leading_upto(queue, field.span.start.line));
                s.field_trailing_comments[index] = take_trailing_on(queue, field.span.end.line);
            }
            for method in &mut s.methods {
                let m_leading = take_leading_upto(queue, method.span.start.line);
                let (m_doc, m_generic) = split_doc_run(m_leading, method.span);
                method.leading_comments = m_generic;
                method.doc_comment = m_doc;
                attach_function_body(&mut method.body, method.ret.span.end.line, queue);
            }
        }
        Decl::Enum(e) => {
            let leading = take_leading_upto(queue, e.span.start.line);
            let (doc, generic) = split_doc_run(leading, e.span);
            e.leading_comments = generic;
            e.doc_comment = doc;
            e.header_comment = take_trailing_on(queue, e.span.start.line);
            for variant in &mut e.variants {
                let v_leading = take_leading_upto(queue, variant.span.start.line);
                variant.leading_comments = to_leading_comments(v_leading);
                variant.trailing_comment = take_trailing_on(queue, variant.span.end.line);
            }
        }
    }
}

/// Attaches one statement in the order leading -> trailing comment on its true last line ->
/// expressions inside it. The true line matters: the parser's span end for a block-bodied
/// statement points at the next sibling's line.
fn attach_to_stmt(stmt: &mut Stmt, queue: &mut VecDeque<RawComment>) {
    let leading = take_leading_upto(queue, stmt.span.start.line);
    let (doc, generic) = split_doc_run(leading, stmt.span);
    stmt.doc_comment = doc;
    stmt.leading_comments = generic;
    stmt.trailing_comment = take_trailing_on(queue, true_end_line_of_stmt(stmt));
    attach_within_stmt(stmt, queue);
}

fn attach_to_block(block: &mut Block, queue: &mut VecDeque<RawComment>) {
    for stmt in &mut block.stmts {
        attach_to_stmt(stmt, queue);
    }
}

/// Attaches a `def` body. Its header comment is the trailing comment on the signature's last
/// line (`signature_line`), so it is taken before the first statement's leading comments.
fn attach_function_body(body: &mut Block, signature_line: u32, queue: &mut VecDeque<RawComment>) {
    body.header_comment = take_header(queue, signature_line, first_stmt_line(body));
    attach_to_block(body, queue);
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
/// which contains a sequence of statements or arms), and attaches the comments inside the
/// bracketed lists and call argument lists it contains. Literals and identifiers contain no
/// such statements/arms, so nothing is done for them.
fn attach_within_expr(expr: &mut Expr, queue: &mut VecDeque<RawComment>) {
    let start_line = expr.span.start.line;
    let close_line = expr.span.end.line;
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
        ExprKind::ListLit {
            elements, comments, ..
        }
        | ExprKind::SetLit {
            elements, comments, ..
        }
        | ExprKind::TupleLit {
            elements, comments, ..
        }
        | ExprKind::Par {
            elements, comments, ..
        } => {
            attach_list(
                comments,
                close_line,
                elements,
                queue,
                expr_extent,
                attach_within_expr,
            );
        }
        ExprKind::DictLit {
            entries, comments, ..
        } => {
            attach_list(
                comments,
                close_line,
                entries,
                queue,
                |entry| (entry.0.span.start.line, true_end_line_of_expr(&entry.1)),
                |entry, q| {
                    attach_within_expr(&mut entry.0, q);
                    attach_within_expr(&mut entry.1, q);
                },
            );
        }
        ExprKind::Unary { operand, .. } => attach_within_expr(operand, queue),
        ExprKind::Binary { lhs, rhs, .. } => {
            attach_within_expr(lhs, queue);
            attach_within_expr(rhs, queue);
        }
        ExprKind::Call {
            callee,
            args,
            comments,
            ..
        } => {
            attach_within_expr(callee, queue);
            attach_args(comments, close_line, args, queue);
        }
        ExprKind::MethodCall {
            receiver,
            args,
            comments,
            ..
        } => {
            attach_within_expr(receiver, queue);
            attach_args(comments, close_line, args, queue);
        }
        ExprKind::FieldAccess { target, .. } | ExprKind::TupleIndex { target, .. } => {
            attach_within_expr(target, queue);
        }
        ExprKind::Index { target, index } => {
            attach_within_expr(target, queue);
            attach_within_expr(index, queue);
        }
        ExprKind::Question { target } => attach_within_expr(target, queue),
        ExprKind::Pipe(pipe) => attach_within_pipe(pipe, queue),
        ExprKind::Lambda {
            body,
            header_comment,
            ..
        } => attach_within_lambda(body, header_comment, start_line, queue),
        ExprKind::If(if_expr) => attach_within_if(if_expr, queue),
        ExprKind::Match {
            scrutinee,
            arms,
            header_comment,
        } => attach_within_match(scrutinee, arms, header_comment, queue),
        ExprKind::Grouping(inner) => attach_within_expr(inner, queue),
    }
}

fn attach_within_pipe(pipe: &mut PipeExpr, queue: &mut VecDeque<RawComment>) {
    attach_within_expr(&mut pipe.source, queue);
    for stage in &mut pipe.stages {
        let stage_close = stage.span.end.line;
        match &mut stage.callee {
            PipeCallee::Bare(e) => attach_within_expr(e, queue),
            PipeCallee::WithArgs {
                callee,
                args,
                comments,
            } => {
                attach_within_expr(callee, queue);
                attach_args(comments, stage_close, args, queue);
            }
        }
    }
}

/// `lambda_start` is the lambda's first line; the `=>` line holds the header comment when the
/// if/match body starts below it.
fn attach_within_lambda(
    body: &mut Expr,
    header_comment: &mut Option<String>,
    lambda_start: u32,
    queue: &mut VecDeque<RawComment>,
) {
    if matches!(body.kind, ExprKind::If(_) | ExprKind::Match { .. }) {
        *header_comment = take_header(queue, lambda_start, Some(body.span.start.line));
    }
    attach_within_expr(body, queue);
}

fn attach_within_match(
    scrutinee: &mut Expr,
    arms: &mut [MatchArm],
    header_comment: &mut Option<String>,
    queue: &mut VecDeque<RawComment>,
) {
    attach_within_expr(scrutinee, queue);
    let scrutinee_end = true_end_line_of_expr(scrutinee);
    let first_arm = arms.first().map(|arm| arm.span.start.line);
    *header_comment = take_header(queue, scrutinee_end, first_arm);
    attach_to_match_arms(arms, queue);
}

fn attach_within_if(if_expr: &mut IfExpr, queue: &mut VecDeque<RawComment>) {
    attach_within_expr(&mut if_expr.cond, queue);
    let cond_end = true_end_line_of_expr(&if_expr.cond);
    let then_first = first_stmt_line(&if_expr.then_branch);
    if_expr.then_branch.header_comment = take_header(queue, cond_end, then_first);
    attach_to_block(&mut if_expr.then_branch, queue);
    let then_end = true_end_line_of_block(&if_expr.then_branch);
    match &mut if_expr.else_branch {
        ElseBranch::Block(block) => {
            // The `else` line has no span of its own: its trailing comment is the first one
            // strictly between the then-block's last line and the else-block's first statement.
            block.header_comment = take_header(queue, then_end + 1, first_stmt_line(block));
            attach_to_block(block, queue);
        }
        ElseBranch::ElseIf(inner_if) => attach_within_if(inner_if, queue),
    }
}

fn attach_to_match_arms(arms: &mut [MatchArm], queue: &mut VecDeque<RawComment>) {
    for arm in arms.iter_mut() {
        let arm_line = arm.span.start.line;
        let leading = take_leading_upto(queue, arm_line);
        // MatchArm has no doc_comment (not a D-DOC-03 target), so the raw text is stored
        // directly into leading_comments.
        arm.leading_comments = to_leading_comments(leading);
        arm.trailing_comment = take_trailing_on(queue, true_end_line_of_match_arm(arm));
        match &mut arm.body {
            MatchArmBody::Expr(e) => attach_within_expr(e, queue),
            MatchArmBody::Block(block) => {
                block.header_comment = take_header(queue, arm_line, first_stmt_line(block));
                attach_to_block(block, queue);
            }
        }
    }
}

/// Pulls every comment (regardless of whether it is trailing) off the front of `queue` that
/// sits on a line before `before_line`. Nodes are processed in source order, so the comments
/// of already processed nodes are gone by now. A comment left behind by an unsupported
/// position (see the module docs) is therefore taken here as the leading comment of the next
/// node, as before.
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

/// Removes the first trailing comment whose line lies in `from..=to` and returns its text
/// (the conventional single space right after `#`/`##` stripped, the same normalization as
/// doc body text). The queue is scanned in source order and the scan stops past `to`;
/// comments passed over stay in place, so a stray comment keeps its fallback.
fn take_trailing_within(queue: &mut VecDeque<RawComment>, from: u32, to: u32) -> Option<String> {
    let mut index = 0;
    while let Some(c) = queue.get(index) {
        let line = c.span.start.line;
        if line > to {
            break;
        }
        if c.is_trailing && line >= from {
            return queue
                .remove(index)
                .map(|c| strip_one_leading_space(&c.text));
        }
        index += 1;
    }
    None
}

/// Removes the trailing comment on exactly `line`.
fn take_trailing_on(queue: &mut VecDeque<RawComment>, line: u32) -> Option<String> {
    take_trailing_within(queue, line, line)
}

/// Removes every comment strictly between `after` and `before` (neither line included), in
/// source order. Comments outside that range stay in place.
fn take_between(queue: &mut VecDeque<RawComment>, after: u32, before: u32) -> Vec<RawComment> {
    let mut taken = Vec::new();
    let mut index = 0;
    while let Some(c) = queue.get(index) {
        let line = c.span.start.line;
        if line >= before {
            break;
        }
        if line > after {
            taken.extend(queue.remove(index));
        } else {
            index += 1;
        }
    }
    taken
}

/// The header comment of an indented body: the trailing comment on the lines from `from` up
/// to the line before the body's first statement (`body_line`). It is taken only when the body
/// really starts on a later line. Covers a `def` signature's last line, `if`/`else` lines, a
/// `match` scrutinee line, a block arm's `pattern =>` line, and a lambda's `=>` line.
fn take_header(
    queue: &mut VecDeque<RawComment>,
    from: u32,
    body_line: Option<u32>,
) -> Option<String> {
    match body_line {
        Some(body) if body > from => take_trailing_within(queue, from, body - 1),
        _ => None,
    }
}

/// Source line of the first statement of `block`, if it has any.
fn first_stmt_line(block: &Block) -> Option<u32> {
    block.stmts.first().map(|stmt| stmt.span.start.line)
}

/// First and last source line of an expression (the last one is its real final token, see
/// `true_end_line_of_expr`).
fn expr_extent(expr: &Expr) -> (u32, u32) {
    (expr.span.start.line, true_end_line_of_expr(expr))
}

/// Attaches the comments inside one bracketed, comma-separated list: a list/dict/set/tuple/
/// `par` literal (elements, or dict entries) or a call's argument list (see `attach_args`).
/// `comments.open_line` is the opening bracket's line and `close_line` the closing bracket's.
/// `extent` gives an element's first and last line, and `recurse` attaches the comments nested
/// inside an element, after the element's leading and trailing comments are taken.
/// `comments.attached` is set only when at least one comment was found.
fn attach_list<T>(
    comments: &mut ListComments,
    close_line: u32,
    items: &mut [T],
    queue: &mut VecDeque<RawComment>,
    extent: impl Fn(&T) -> (u32, u32),
    mut recurse: impl FnMut(&mut T, &mut VecDeque<RawComment>),
) {
    let open_line = comments.open_line;
    // `foo( # c`: the opening line's comment, when the first element starts on a later line
    // (or, for an empty list, when the closing bracket is on a later line).
    let open = match items.first().map(|item| extent(item).0) {
        Some(first_line) if first_line > open_line => take_trailing_on(queue, open_line),
        None if open_line < close_line => take_trailing_on(queue, open_line),
        _ => None,
    };
    let mut leading = Vec::with_capacity(items.len());
    let mut trailing = Vec::with_capacity(items.len());
    let mut prev_line = open_line;
    let mut rest = items;
    while let Some((item, tail)) = rest.split_first_mut() {
        let (start, end) = extent(&*item);
        leading.push(to_leading_comments(take_between(queue, prev_line, start)));
        // A trailing comment belongs to this element only when it sits before the next element
        // (or the closing bracket): a comment on the closing line belongs to the statement.
        let bound = tail.first().map_or(close_line, |next| extent(next).0);
        trailing.push(if end < bound {
            take_trailing_on(queue, end)
        } else {
            None
        });
        recurse(item, queue);
        prev_line = end;
        rest = tail;
    }
    let closing = to_leading_comments(take_between(queue, prev_line, close_line));
    if open.is_none()
        && closing.is_empty()
        && leading.iter().all(Vec::is_empty)
        && trailing.iter().all(Option::is_none)
    {
        return;
    }
    comments.attached = Some(Box::new(AttachedListComments {
        open,
        leading,
        trailing,
        closing,
    }));
}

/// Attaches the comments inside a call's argument list (`f(..)`, `x.m(..)`, or a pipe stage
/// `f(_, ..)`). `close_line` is the line of the closing parenthesis.
fn attach_args(
    comments: &mut ListComments,
    close_line: u32,
    args: &mut [Arg],
    queue: &mut VecDeque<RawComment>,
) {
    attach_list(
        comments,
        close_line,
        args,
        queue,
        |arg| expr_extent(&arg.value),
        |arg, q| attach_within_expr(&mut arg.value, q),
    );
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

    /// The statement at top-level position `index` of `module`.
    fn stmt_item(module: &Module, index: usize) -> &Stmt {
        match &module.items[index] {
            Item::Stmt(stmt) => stmt,
            Item::Decl(_) => panic!("expected Item {index} to be a statement"),
        }
    }

    /// The right-hand side of a `name = value` statement.
    fn assigned_value(stmt: &Stmt) -> &Expr {
        match &stmt.kind {
            StmtKind::NameAssign { value, .. } => value,
            _ => panic!("expected a NameAssign statement"),
        }
    }

    /// The attached list comments; panics when none were attached.
    fn attached_list(comments: &ListComments) -> &AttachedListComments {
        let Some(attached) = comments.attached.as_deref() else {
            panic!("list comments were not attached");
        };
        attached
    }

    #[test]
    fn def_header_comment_attaches_to_body() {
        let src = "def f(x: int): int # sig\n    return x\n";
        let module = lex_parse_and_attach(src);
        let f = first_function(&module);
        assert_eq!(f.body.header_comment.as_deref(), Some("sig"));
    }

    #[test]
    fn method_header_comment_attaches_to_body() {
        let src = "struct Counter\n    value: int\n\n    def get(self): int # m\n        return self.value\n";
        let module = lex_parse_and_attach(src);
        let Item::Decl(Decl::Struct(s)) = &module.items[0] else {
            panic!("expected the first Item to be a struct");
        };
        assert_eq!(s.methods[0].body.header_comment.as_deref(), Some("m"));
    }

    #[test]
    fn struct_and_enum_header_comments_attach() {
        let src = "struct P # s\n    x: int # fx\n\nenum C # e\n    A\n    B # b\n";
        let module = lex_parse_and_attach(src);
        let Item::Decl(Decl::Struct(s)) = &module.items[0] else {
            panic!("expected the first Item to be a struct");
        };
        assert_eq!(s.header_comment.as_deref(), Some("s"));
        assert_eq!(s.field_trailing_comments[0].as_deref(), Some("fx"));
        let Item::Decl(Decl::Enum(e)) = &module.items[1] else {
            panic!("expected the second Item to be an enum");
        };
        assert_eq!(e.header_comment.as_deref(), Some("e"));
        assert_eq!(e.variants[1].trailing_comment.as_deref(), Some("b"));
    }

    #[test]
    fn if_and_else_header_comments_attach_to_their_blocks() {
        let src = "r = if x > 0 # cond\n    a\nelse # els\n    b\n";
        let module = lex_parse_and_attach(src);
        let ExprKind::If(if_expr) = &assigned_value(stmt_item(&module, 0)).kind else {
            panic!("expected an If expression");
        };
        assert_eq!(if_expr.then_branch.header_comment.as_deref(), Some("cond"));
        let ElseBranch::Block(else_block) = &if_expr.else_branch else {
            panic!("expected a block else branch");
        };
        assert_eq!(else_block.header_comment.as_deref(), Some("els"));
    }

    #[test]
    fn comment_after_if_expression_is_not_glued_to_its_last_block_line() {
        let src = "r = if x > 0\n    a\nelse\n    b\ny = 1 # yc\n";
        let module = lex_parse_and_attach(src);
        assert_eq!(
            stmt_item(&module, 1).trailing_comment.as_deref(),
            Some("yc")
        );
        let ExprKind::If(if_expr) = &assigned_value(stmt_item(&module, 0)).kind else {
            panic!("expected an If expression");
        };
        let ElseBranch::Block(else_block) = &if_expr.else_branch else {
            panic!("expected a block else branch");
        };
        assert_eq!(else_block.stmts[0].trailing_comment, None);
    }

    #[test]
    fn match_header_and_block_arm_comments_attach() {
        let src = "r = match x # sc\n    1 => # arm\n        a\n    _ => 3 # three\n";
        let module = lex_parse_and_attach(src);
        let ExprKind::Match {
            scrutinee: _,
            arms,
            header_comment,
        } = &assigned_value(stmt_item(&module, 0)).kind
        else {
            panic!("expected a Match expression");
        };
        assert_eq!(header_comment.as_deref(), Some("sc"));
        let MatchArmBody::Block(block) = &arms[0].body else {
            panic!("expected a block arm");
        };
        assert_eq!(block.header_comment.as_deref(), Some("arm"));
        assert_eq!(block.stmts[0].trailing_comment, None);
        // `# three` is not taken by the block arm's last line; the statement ends on that
        // line, so as the outermost node there it owns the comment.
        assert_eq!(arms[0].trailing_comment, None);
        assert_eq!(
            stmt_item(&module, 0).trailing_comment.as_deref(),
            Some("three")
        );
    }

    #[test]
    fn lambda_header_comment_attaches_to_its_if_body() {
        let src = "labels = xs.map((x) => # lam\n    if x % 2 == 0\n        \"even\"\n    else\n        \"odd\")\n";
        let module = lex_parse_and_attach(src);
        let ExprKind::MethodCall { args, .. } = &assigned_value(stmt_item(&module, 0)).kind else {
            panic!("expected a MethodCall expression");
        };
        let ExprKind::Lambda { header_comment, .. } = &args[0].value.kind else {
            panic!("expected a Lambda argument");
        };
        assert_eq!(header_comment.as_deref(), Some("lam"));
    }

    #[test]
    fn list_open_leading_trailing_and_closing_comments_attach() {
        let src = "xs = [ # o\n    1, # first\n    # note\n    2,\n    # tail\n]\n";
        let module = lex_parse_and_attach(src);
        let ExprKind::ListLit {
            elements, comments, ..
        } = &assigned_value(stmt_item(&module, 0)).kind
        else {
            panic!("expected a ListLit expression");
        };
        assert_eq!(elements.len(), 2);
        let list = attached_list(comments);
        assert_eq!(list.open.as_deref(), Some("o"));
        assert!(list.leading[0].is_empty());
        assert_eq!(list.leading[1][0].text, "note");
        assert_eq!(list.leading[1][0].line, 3);
        assert_eq!(list.trailing, vec![Some("first".to_owned()), None]);
        assert_eq!(list.closing[0].text, "tail");
        assert_eq!(list.closing[0].line, 5);
    }

    #[test]
    fn call_argument_comments_attach_and_closing_line_comment_goes_to_statement() {
        let src = "z = foo( # c\n    a, # first\n    b\n) # after\n";
        let module = lex_parse_and_attach(src);
        let z = stmt_item(&module, 0);
        assert_eq!(z.trailing_comment.as_deref(), Some("after"));
        let ExprKind::Call { comments, .. } = &assigned_value(z).kind else {
            panic!("expected a Call expression");
        };
        let list = attached_list(comments);
        assert_eq!(list.open.as_deref(), Some("c"));
        assert_eq!(list.trailing, vec![Some("first".to_owned()), None]);
        assert!(list.leading.iter().all(Vec::is_empty));
        assert!(list.closing.is_empty());
    }

    #[test]
    fn pipe_stage_argument_comment_attaches() {
        let src = "w = seed |> foo(_, # p\n    1)\n";
        let module = lex_parse_and_attach(src);
        let ExprKind::Pipe(pipe) = &assigned_value(stmt_item(&module, 0)).kind else {
            panic!("expected a Pipe expression");
        };
        let PipeCallee::WithArgs { comments, .. } = &pipe.stages[0].callee else {
            panic!("expected a stage with arguments");
        };
        assert_eq!(attached_list(comments).trailing[0].as_deref(), Some("p"));
    }

    #[test]
    fn stray_chain_comment_is_not_captured_by_following_argument_list() {
        // A comment on a chain continuation line is an unsupported position: the argument
        // list that follows must not claim it, and it falls back to the next statement's
        // leading comments as before.
        let src = "z = xs # chain\n    .len()\nw = foo(\n    a,\n)\n";
        let module = lex_parse_and_attach(src);
        assert_eq!(stmt_item(&module, 0).trailing_comment, None);
        let w = stmt_item(&module, 1);
        assert_eq!(w.leading_comments.len(), 1);
        assert_eq!(w.leading_comments[0].text, "chain");
        assert_eq!(w.leading_comments[0].line, 1);
        let ExprKind::Call { comments, .. } = &assigned_value(w).kind else {
            panic!("expected a Call expression");
        };
        assert!(comments.attached.is_none());
    }
}
