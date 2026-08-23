//! Formatting rules for each AST node (D-FMT-01 through 05, ARCHITECTURE.md §5.9).
//!
//! Policy (§5.9): AST regeneration. Each `print_*` function is unified under the
//! convention "the first line has no indentation applied by the caller / from the second
//! line on it carries its own absolute indentation" (the field/method lists of
//! `print_block`/`print_module`/`print_match_arms`/`print_struct_decl` and the variant list
//! of `print_enum_decl` are the sole exceptions, returning a string where "every line is
//! already absolutely indented" — because for these, the arrangement of the elements itself
//! is opaque to the caller, being a multi-line block). By confining these two conventions
//! into the two shared helpers `append_with_blank_line_rule`/`join_first_unindented`, the
//! same blank-line-preservation logic can be used to write top-level items, function
//! bodies, if/else blocks, match arms, struct bodies, and enum variant lists alike.

use crate::ast::{
    Arg, BinaryOp, Block, Decl, DocComment, ElseBranch, EnumDecl, EnumVariant, Expr, ExprKind,
    FStringSegment, FunctionDecl, IfExpr, Item, LambdaParam, LeadingComment, LiteralPat, MatchArm,
    MatchArmBody, Module, ParKind, Param, Pattern, PipeCallee, PipeExpr, PipeStage, Stmt, StmtKind,
    StructDecl, SubPattern, TypeAnn, TypeAnnKind, UnaryOp,
};
use crate::fmt::doc_fence;
use std::sync::Arc;

/// Recursively walks the whole AST and applies as fixed rules D-FMT-01 (spacing around
/// operators/commas/colons), D-FMT-02 (strings are always double-quoted), D-FMT-03
/// (comment spacing), D-FMT-04 (splitting into one stage per line at 3+ `|>`s), D-TYPE-02
/// (trailing comma when spanning multiple lines), and D-FMT-05 (multi-line expansion based
/// on `was_multiline`), producing the canonical-form text.
pub fn print_module(module: &Module) -> String {
    let mut out = String::new();
    if module.is_module_directive {
        // D-LEX-08/09: a bare `module` leaves only a single bit on the `Module` struct
        // (see parser/mod.rs). Everything under samples/ok/10* takes the form
        // `module\n\n<body>`, so this reproduces it as a fixed shape.
        out.push_str("module\n\n");
    }
    let mut prev_end: Option<u32> = None;
    for item in &module.items {
        match item {
            Item::Decl(decl) => {
                let rendered = print_decl(decl, 0);
                append_with_blank_line_rule(
                    &mut out,
                    &mut prev_end,
                    decl_effective_start_line(decl),
                    decl_end_line(decl),
                    &rendered,
                    "",
                );
            }
            Item::Stmt(stmt) => {
                let rendered = print_stmt(stmt, 0);
                append_with_blank_line_rule(
                    &mut out,
                    &mut prev_end,
                    stmt_effective_start_line(stmt),
                    true_end_line_of_stmt(stmt),
                    &rendered,
                    "",
                );
            }
        }
    }
    for comment in &module.trailing_comments {
        let rendered = format_comment_line(&comment.text);
        append_with_blank_line_rule(
            &mut out,
            &mut prev_end,
            comment.line,
            comment.line,
            &rendered,
            "",
        );
    }
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Shared helpers: blank-line preservation (D-SYN-02), multi-line concatenation conventions
// ---------------------------------------------------------------------------

/// Called once for each "element in a series" — a top-level item, a statement, a member of
/// a struct/enum's member list, etc. Compares the previous element's end line (`prev_end`)
/// with this element's effective start line (`start_line`, its beginning including things
/// like comments), and inserts exactly one blank line if the source had one or more blank
/// lines there (D-SYN-02: consecutive blank lines are normalized to at most one). `rendered`
/// is passed a string following the convention "the first line has no indentation, from the
/// second line on it is already absolutely indented".
fn append_with_blank_line_rule(
    out: &mut String,
    prev_end: &mut Option<u32>,
    start_line: u32,
    end_line: u32,
    rendered: &str,
    pad: &str,
) {
    if let Some(prev) = *prev_end {
        if start_line.saturating_sub(prev) > 1 {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(pad);
    out.push_str(rendered);
    *prev_end = Some(end_line);
}

/// Assembles `lines` (each element is one logical line containing no newline, or a
/// multi-line string returned by `render_doc_comment`) into a single string with "the first
/// line unindented, and from the second line on absolutely indented by `indent`". Used for
/// placing multiple "logical line groups" — such as a doc/leading comment plus the body —
/// side by side as a single element. An empty-string element represents a "restored blank
/// line" (D-SYN-02, inserted by `push_blank_if_gap`) and, unlike other lines, is not given
/// any indentation (so as not to leave trailing whitespace on a blank line — the same
/// treatment as `append_with_blank_line_rule` inserting a blank line between siblings with
/// no indentation).
fn join_first_unindented(lines: &[String], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            if !l.is_empty() {
                out.push_str(&pad);
            }
        }
        out.push_str(l);
    }
    out
}

/// If there is a gap of one or more lines between `prev_line` (the real source line number
/// of the line placed immediately before) and `next_line` (the real source line number of
/// the line about to be placed), appends one empty string to `lines` (a restored blank
/// line, per D-SYN-02's "consecutive blank lines are normalized to at most one"). Does
/// nothing if `prev_line` is `None` (nothing precedes it) — so as not to create a spurious
/// blank line at the start of an element.
fn push_blank_if_gap(lines: &mut Vec<String>, prev_line: Option<u32>, next_line: u32) {
    if let Some(p) = prev_line
        && next_line.saturating_sub(p) > 1
    {
        lines.push(String::new());
    }
}

/// Assembles `leading` (standalone `#` comments, with line numbers) + `doc` (a `##` doc
/// comment, if any) + `core` (the body, with `trailing` appended inline if present) into a
/// single "first line unindented, from the second line on absolutely indented" string,
/// restoring the original blank lines (D-SYN-02) from gaps between the recorded actual line
/// numbers. `core_start_line` is the actual start line of the code body itself (excluding
/// comments, the `span.start.line` of the `Stmt`/`FunctionDecl` etc.) — used to judge blank
/// lines between the comment block and the body. Since `Stmt`/`MatchArm`/`EnumVariant`/
/// `FunctionDecl`/`StructDecl`/`EnumDecl` all share the same shape
/// (leading_comments(+doc_comment)+trailing_comment), this can be shared across them
/// (ARCHITECTURE.md §5.9, fixes for cause A/B).
fn assemble_with_leading_trailing(
    leading: &[LeadingComment],
    doc: Option<&DocComment>,
    core_start_line: u32,
    trailing: Option<&String>,
    indent: usize,
    core: String,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut prev_line: Option<u32> = None;
    for c in leading {
        push_blank_if_gap(&mut lines, prev_line, c.line);
        lines.push(format_comment_line(&c.text));
        prev_line = Some(c.line);
    }
    if let Some(d) = doc {
        push_blank_if_gap(&mut lines, prev_line, d.span.start.line);
        lines.extend(render_doc_comment_lines(d));
        prev_line = Some(d.span.end.line);
    }
    push_blank_if_gap(&mut lines, prev_line, core_start_line);
    let core = match trailing {
        Some(t) => format!("{core} {}", format_comment_line(t)),
        None => core,
    };
    lines.push(core);
    join_first_unindented(&lines, indent)
}

/// Normalization of a standalone/trailing `#` comment (D-FMT-03): all leading whitespace is
/// dropped, and it becomes `# `+content if the content is non-empty, or a bare `#` if empty
/// (symmetric with the rule that an empty `##` line becomes a bare `##`).
fn format_comment_line(text: &str) -> String {
    let t = text.trim_start();
    if t.is_empty() {
        "#".to_owned()
    } else {
        format!("# {t}")
    }
}

/// Splits the (unindented) multi-line string returned by `doc_fence::render_doc_comment`
/// into the `Vec<String>` (a list of elements each containing no newline) that can be mixed
/// into `leading_lines` for `Stmt`/`FunctionDecl` etc.
fn render_doc_comment_lines(doc: &DocComment) -> Vec<String> {
    doc_fence::render_doc_comment(doc)
        .lines()
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// Recomputing the actual final token line (working around a known quirk on the parser side)
// ---------------------------------------------------------------------------
//
// `parser/stmt.rs::parse_block` decides `Block.span.end` via `self.previous_span()`
// **after** `self.bump()`-ing the `Dedent` token (a file out of scope here, and its code is
// left unchanged). Because the synthesized `Dedent` token's span points at the next
// non-blank line (= the start of the next sibling element, or further still if blank lines
// follow), the `.span.end.line` of a `Block` / the `FunctionDecl`/`StructDecl`/`EnumDecl`/
// `MatchArm` (a block body) / `IfExpr` (when the else is a block) containing it ends up
// pointing not at "the line of the actual final token" but at "the line of the next
// sibling element." If D-SYN-02's blank-line-preservation logic (the gap computation) uses
// this tainted `.span.end.line` directly, it ends up inserting a blank line in a spot where
// there should genuinely be none (confirmed to actually occur in samples/ok/7-5_assert —
// noted in the report as needing follow-up). What follows recomputes "the line of the
// actual final token" recursively, dedicated to this sibling-gap computation.
// (Expressions that don't pass through a block (Call/MethodCall/a string literal etc.) have
// their span decided by an actual token such as `)`, so they aren't tainted and
// `expr.span.end.line` remains correct as-is.)

fn true_end_line_of_stmt(stmt: &Stmt) -> u32 {
    match &stmt.kind {
        StmtKind::VarDecl { value, .. }
        | StmtKind::NameAssign { value, .. }
        | StmtKind::FieldAssign { value, .. }
        | StmtKind::IndexAssign { value, .. }
        | StmtKind::Discard(value)
        | StmtKind::ExprStmt(value)
        | StmtKind::Return(Some(value)) => true_end_line_of_expr(value),
        StmtKind::Return(None) => stmt.span.start.line,
    }
}

fn true_end_line_of_expr(expr: &Expr) -> u32 {
    match &expr.kind {
        ExprKind::If(if_expr) => true_end_line_of_if(if_expr),
        ExprKind::Match { arms, .. } => arms
            .last()
            .map_or(expr.span.start.line, true_end_line_of_match_arm),
        ExprKind::Lambda { body, .. } => true_end_line_of_expr(body),
        ExprKind::Grouping(inner) => true_end_line_of_expr(inner),
        _ => expr.span.end.line,
    }
}

fn true_end_line_of_if(if_expr: &IfExpr) -> u32 {
    match &if_expr.else_branch {
        ElseBranch::Block(block) => true_end_line_of_block(block),
        ElseBranch::ElseIf(inner) => true_end_line_of_if(inner),
    }
}

fn true_end_line_of_block(block: &Block) -> u32 {
    block
        .stmts
        .last()
        .map_or(block.span.start.line, true_end_line_of_stmt)
}

fn true_end_line_of_match_arm(arm: &MatchArm) -> u32 {
    match &arm.body {
        MatchArmBody::Expr(e) => true_end_line_of_expr(e),
        MatchArmBody::Block(block) => true_end_line_of_block(block),
    }
}

fn true_end_line_of_function_decl(f: &FunctionDecl) -> u32 {
    true_end_line_of_block(&f.body)
}

fn true_end_line_of_struct_decl(s: &StructDecl) -> u32 {
    if let Some(m) = s.methods.last() {
        true_end_line_of_function_decl(m)
    } else if let Some(field) = s.fields.last() {
        field.span.end.line
    } else {
        s.span.start.line
    }
}

fn true_end_line_of_enum_decl(e: &EnumDecl) -> u32 {
    e.variants
        .last()
        .map_or(e.span.start.line, |v| v.span.end.line)
}

// ---------------------------------------------------------------------------
// Declarations (Decl/FunctionDecl/StructDecl/EnumDecl)
// ---------------------------------------------------------------------------

/// Returns the actual line number of `leading`'s first comment if there is one, otherwise
/// `doc`'s start line, otherwise `fallback` (the node's own start line) — the shared logic
/// for the "effective start line" used in D-SYN-02's blank-line check, i.e. the gap computation
/// between siblings). Now that `leading_comments` holds the actual source line number
/// (`LeadingComment.line`), `Decl`/`Stmt`/`EnumVariant`/`MatchArm` can all return the
/// precise line with no approximation (fixes for cause A/B).
fn effective_start_line(
    leading: &[LeadingComment],
    doc: Option<&DocComment>,
    fallback: u32,
) -> u32 {
    leading
        .first()
        .map(|c| c.line)
        .or_else(|| doc.map(|d| d.span.start.line))
        .unwrap_or(fallback)
}

/// The "effective start line" that serves as the basis for the gap. Since
/// `FunctionDecl`/`StructDecl`/`EnumDecl` all carry `leading_comments` (§5.9, fix for cause
/// A), this delegates to `effective_start_line`.
fn decl_effective_start_line(decl: &Decl) -> u32 {
    match decl {
        Decl::Function(f) => effective_start_line(
            &f.leading_comments,
            f.doc_comment.as_ref(),
            f.span.start.line,
        ),
        Decl::Struct(s) => effective_start_line(
            &s.leading_comments,
            s.doc_comment.as_ref(),
            s.span.start.line,
        ),
        Decl::Enum(e) => effective_start_line(
            &e.leading_comments,
            e.doc_comment.as_ref(),
            e.span.start.line,
        ),
    }
}

fn decl_end_line(decl: &Decl) -> u32 {
    match decl {
        Decl::Function(f) => true_end_line_of_function_decl(f),
        Decl::Struct(s) => true_end_line_of_struct_decl(s),
        Decl::Enum(e) => true_end_line_of_enum_decl(e),
    }
}

fn print_decl(decl: &Decl, indent: usize) -> String {
    match decl {
        Decl::Function(f) => print_function_decl(f, indent),
        Decl::Struct(s) => print_struct_decl(s, indent),
        Decl::Enum(e) => print_enum_decl(e, indent),
    }
}

/// `[T, U]`. Outputs nothing when empty.
fn print_generics(generics: &[Arc<str>]) -> String {
    if generics.is_empty() {
        String::new()
    } else {
        let joined = generics
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&str>>()
            .join(", ");
        format!("[{joined}]")
    }
}

/// ` uses {e1, e2}`. Outputs nothing when empty (SPEC §8: "empty means pure").
fn print_uses_clause(effects: &[Arc<str>]) -> String {
    if effects.is_empty() {
        String::new()
    } else {
        let joined = effects
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&str>>()
            .join(", ");
        format!(" uses {{{joined}}}")
    }
}

/// `def name[generics](params): ret uses {..}` + body. Prepends leading_comments/
/// doc_comment if present (fix for cause A — also preserves an unmarked comment
/// immediately before a declaration).
fn print_function_decl(f: &FunctionDecl, indent: usize) -> String {
    let mut params_parts: Vec<String> = Vec::new();
    if let Some(sp) = &f.self_param {
        params_parts.push(if sp.mutable {
            "var self".to_owned()
        } else {
            "self".to_owned()
        });
    }
    for p in &f.params {
        params_parts.push(print_param(p));
    }
    let params_str = params_parts.join(", ");
    let ret_str = print_type_ann(&f.ret);
    let uses_str = print_uses_clause(&f.effects);
    let generics_str = print_generics(&f.generics);
    let header = format!(
        "def {}{generics_str}({params_str}): {ret_str}{uses_str}",
        f.name
    );
    let mut out = assemble_with_leading_trailing(
        &f.leading_comments,
        f.doc_comment.as_ref(),
        f.span.start.line,
        None,
        indent,
        header,
    );
    out.push('\n');
    out.push_str(&print_block(&f.body, indent + 4));
    out
}

/// `name: Type` (used for both function arguments and struct fields — `Param` has the same
/// shape for either).
///
/// **A quirk on the parser side (see the documentation for `parser/decl.rs::parse_param`)**:
/// the type annotation (after `:`) is syntactically optional, and when omitted, processing
/// continues without emitting a diagnostic, using a dummy empty-name type
/// `Named{name: "", args: []}` (per the D-TYPE-11/D-DIAG-02 decision, a "missing type
/// annotation" is the responsibility of E1002 (the type-system layer), not a syntax error —
/// samples/err/static/3-4_type_annotation_and_inference_errors/entry_missing_param_annotation.ybm
/// requires this shape). This dummy empty-name type can never be written in actual Yabumi
/// code (an identifier can't be an empty string), so fmt uses it as a safe marker for "no
/// annotation was present to begin with" and prints just `name` without a `name: ` prefix
/// (prefixing it anyway would produce the invalid output `x: `, breaking idempotency — an
/// actual bug that was confirmed to occur, see the report).
fn print_param(p: &Param) -> String {
    if is_missing_type_ann(&p.ty) {
        p.name.to_string()
    } else {
        format!("{}: {}", p.name, print_type_ann(&p.ty))
    }
}

/// Detects the marker for "no type annotation" (`Named{name: "", args: []}`, a shape that
/// can never be constructed in actual Yabumi code syntactically) produced by the error
/// recovery of `parse_param`/`parse_type_ann`.
fn is_missing_type_ann(ty: &TypeAnn) -> bool {
    matches!(&ty.kind, TypeAnnKind::Named { name, args } if name.is_empty() && args.is_empty())
}

/// `struct Name[generics]` + field list + method list. Fields and methods are separate
/// `Vec`s (ast/decl.rs), but both are processed together as a single blank-line-preservation
/// sequence (actual samples go fields, then methods, and blank lines between the two groups are
/// preserved as well).
fn print_struct_decl(s: &StructDecl, indent: usize) -> String {
    let header = format!("struct {}{}", s.name, print_generics(&s.generics));
    let mut out = assemble_with_leading_trailing(
        &s.leading_comments,
        s.doc_comment.as_ref(),
        s.span.start.line,
        None,
        indent,
        header,
    );
    out.push('\n');
    let pad = " ".repeat(indent + 4);
    let mut prev_end: Option<u32> = None;
    for (index, field) in s.fields.iter().enumerate() {
        let leading = &s.field_leading_comments[index];
        let rendered = assemble_with_leading_trailing(
            leading,
            None,
            field.span.start.line,
            s.field_trailing_comments[index].as_ref(),
            indent + 4,
            print_param(field),
        );
        append_with_blank_line_rule(
            &mut out,
            &mut prev_end,
            effective_start_line(leading, None, field.span.start.line),
            field.span.end.line,
            &rendered,
            &pad,
        );
    }
    for method in &s.methods {
        let start_line = effective_start_line(
            &method.leading_comments,
            method.doc_comment.as_ref(),
            method.span.start.line,
        );
        let rendered = print_function_decl(method, indent + 4);
        append_with_blank_line_rule(
            &mut out,
            &mut prev_end,
            start_line,
            true_end_line_of_function_decl(method),
            &rendered,
            &pad,
        );
    }
    out
}

/// `enum Name[generics]` + variant list.
fn print_enum_decl(e: &EnumDecl, indent: usize) -> String {
    let header = format!("enum {}{}", e.name, print_generics(&e.generics));
    let mut out = assemble_with_leading_trailing(
        &e.leading_comments,
        e.doc_comment.as_ref(),
        e.span.start.line,
        None,
        indent,
        header,
    );
    out.push('\n');
    let pad = " ".repeat(indent + 4);
    let mut prev_end: Option<u32> = None;
    for variant in &e.variants {
        let start_line =
            effective_start_line(&variant.leading_comments, None, variant.span.start.line);
        let rendered = print_enum_variant(variant, indent + 4);
        append_with_blank_line_rule(
            &mut out,
            &mut prev_end,
            start_line,
            variant.span.end.line,
            &rendered,
            &pad,
        );
    }
    out
}

/// `Name` (unit variant) or `Name(field1, field2)` (D-SYN-07: construction/destructuring
/// always use positional arguments). If field names (`field_names`) are recorded, this
/// reconstructs the `name: ty` form — D-SYN-07 is about construction/destructuring, and
/// does not decide that fmt may strip away the declaration form SPEC §3.5 defines
/// (`Circle(radius: float)`) (an owner ruling, fix for cause D).
fn print_enum_variant(v: &EnumVariant, indent: usize) -> String {
    let core = if v.fields.is_empty() {
        v.name.to_string()
    } else {
        let joined = v
            .fields
            .iter()
            .zip(v.field_names.iter())
            .map(|(ty, name)| match name {
                Some(n) => format!("{n}: {}", print_type_ann(ty)),
                None => print_type_ann(ty),
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({joined})", v.name)
    };
    assemble_with_leading_trailing(
        &v.leading_comments,
        None,
        v.span.start.line,
        v.trailing_comment.as_ref(),
        indent,
        core,
    )
}

// ---------------------------------------------------------------------------
// Statements (Stmt/Block)
// ---------------------------------------------------------------------------

/// The "effective start line" that serves as the basis for the gap. Delegates to
/// `effective_start_line` (fix for cause B — since `leading_comments` now holds the actual
/// line number, the approximation is no longer needed).
fn stmt_effective_start_line(stmt: &Stmt) -> u32 {
    effective_start_line(
        &stmt.leading_comments,
        stmt.doc_comment.as_ref(),
        stmt.span.start.line,
    )
}

/// Assembles a `Block` (an if/else branch, a function body, or a match arm's multi-statement
/// body) as a string with "every line already absolutely indented."
fn print_block(block: &Block, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::new();
    let mut prev_end: Option<u32> = None;
    for stmt in &block.stmts {
        let rendered = print_stmt(stmt, indent);
        append_with_blank_line_rule(
            &mut out,
            &mut prev_end,
            stmt_effective_start_line(stmt),
            true_end_line_of_stmt(stmt),
            &rendered,
            &pad,
        );
    }
    out
}

fn print_stmt(stmt: &Stmt, indent: usize) -> String {
    let core = print_stmt_kind(&stmt.kind, indent);
    assemble_with_leading_trailing(
        &stmt.leading_comments,
        stmt.doc_comment.as_ref(),
        stmt.span.start.line,
        stmt.trailing_comment.as_ref(),
        indent,
        core,
    )
}

fn print_stmt_kind(kind: &StmtKind, indent: usize) -> String {
    match kind {
        StmtKind::VarDecl { name, ty, value } => match ty {
            Some(t) => format!(
                "var {name}: {} = {}",
                print_type_ann(t),
                print_expr(value, indent)
            ),
            None => format!("var {name} = {}", print_expr(value, indent)),
        },
        StmtKind::NameAssign { name, ty, value } => match ty {
            Some(t) => format!(
                "{name}: {} = {}",
                print_type_ann(t),
                print_expr(value, indent)
            ),
            None => format!("{name} = {}", print_expr(value, indent)),
        },
        StmtKind::FieldAssign {
            target,
            field,
            value,
        } => format!(
            "{}.{field} = {}",
            print_expr(target, indent),
            print_expr(value, indent)
        ),
        StmtKind::IndexAssign {
            target,
            index,
            value,
        } => format!(
            "{}[{}] = {}",
            print_expr(target, indent),
            print_expr(index, indent),
            print_expr(value, indent)
        ),
        StmtKind::Discard(e) => format!("_ = {}", print_expr(e, indent)),
        StmtKind::Return(Some(e)) => format!("return {}", print_expr(e, indent)),
        StmtKind::Return(None) => "return".to_owned(),
        StmtKind::ExprStmt(e) => print_expr(e, indent),
    }
}

// ---------------------------------------------------------------------------
// Expressions (Expr) — D-FMT-01/02/04/05, D-SYN-05, D-TYPE-01/02
// ---------------------------------------------------------------------------

fn print_expr(expr: &Expr, indent: usize) -> String {
    match &expr.kind {
        ExprKind::IntLit(n) => n.to_string(),
        ExprKind::FloatLit(f) => format_float(*f),
        ExprKind::BoolLit(b) => b.to_string(),
        ExprKind::StringLit(s) => format!("\"{}\"", escape_string_content(s)),
        ExprKind::FString(segments) => print_fstring(segments, indent),
        ExprKind::Ident(name) => name.to_string(),
        ExprKind::ListLit {
            elements,
            was_multiline,
        } => print_bracket_list("[", "]", elements, *was_multiline, indent, print_expr),
        ExprKind::DictLit {
            entries,
            was_multiline,
        } => print_bracket_list("{", "}", entries, *was_multiline, indent, |pair, ind| {
            format!("{}: {}", print_expr(&pair.0, ind), print_expr(&pair.1, ind))
        }),
        ExprKind::SetLit {
            elements,
            was_multiline,
        } => print_bracket_list("{", "}", elements, *was_multiline, indent, print_expr),
        ExprKind::TupleLit {
            elements,
            was_multiline,
        } => print_tuple_lit(elements, *was_multiline, indent),
        ExprKind::Unary { op, operand } => match op {
            UnaryOp::Neg => format!("-{}", print_expr(operand, indent)),
            UnaryOp::Not => format!("not {}", print_expr(operand, indent)),
        },
        ExprKind::Binary { op, lhs, rhs } => format!(
            "{} {} {}",
            print_expr(lhs, indent),
            binary_op_str(*op),
            print_expr(rhs, indent)
        ),
        ExprKind::Call {
            callee,
            type_args,
            args,
            was_multiline,
        } => format!(
            "{}{}{}",
            print_expr(callee, indent),
            print_type_args(type_args),
            print_args_paren(args, *was_multiline, indent)
        ),
        ExprKind::MethodCall {
            receiver,
            method,
            type_args,
            args,
            was_multiline,
        } => print_method_call_chain(
            expr,
            receiver,
            method,
            type_args,
            args,
            *was_multiline,
            indent,
        ),
        ExprKind::FieldAccess { target, field } => {
            print_dot_chain(expr, target, &format!(".{field}"), indent)
        }
        ExprKind::TupleIndex { target, index } => {
            print_dot_chain(expr, target, &format!(".{index}"), indent)
        }
        ExprKind::Index { target, index } => {
            format!(
                "{}[{}]",
                print_expr(target, indent),
                print_expr(index, indent)
            )
        }
        ExprKind::Question { target } => format!("{}?", print_expr(target, indent)),
        ExprKind::Pipe(pipe) => print_pipe(pipe, indent),
        ExprKind::Lambda { params, body } => print_lambda(params, body, indent),
        ExprKind::If(if_expr) => print_if_expr(if_expr, indent),
        ExprKind::Match { scrutinee, arms } => format!(
            "match {}\n{}",
            print_expr(scrutinee, indent),
            print_match_arms(arms, indent + 4)
        ),
        ExprKind::Par { kind, elements } => print_par(kind, elements, indent),
        ExprKind::Grouping(inner) => format!("({})", print_expr(inner, indent)),
    }
}

/// The core of D-FMT-05 (plus D-TYPE-02's trailing comma): 0 or 1 elements are always
/// collapsed onto a single line (expanding a single element into "one line per element"
/// would change nothing visually, and would run counter to the purpose D-FMT-05's
/// was_multiline is actually meant for, "readability of multiple elements side by side" —
/// this handling is needed for a case like samples/ok/5-1_lambdas, "a lambda with one
/// argument whose body spans multiple lines via if/match," a judgment call made in this
/// printer implementation). With 2 or more elements and was_multiline, expands to one
/// element per line + trailing comma.
fn print_bracket_list<T>(
    open: &str,
    close: &str,
    items: &[T],
    was_multiline: bool,
    indent: usize,
    print_item: impl Fn(&T, usize) -> String,
) -> String {
    match items.len() {
        0 => format!("{open}{close}"),
        1 => format!("{open}{}{close}", print_item(&items[0], indent)),
        _ if was_multiline => {
            let pad_inner = " ".repeat(indent + 4);
            let pad_outer = " ".repeat(indent);
            let mut s = format!("{open}\n");
            for item in items {
                s.push_str(&pad_inner);
                s.push_str(&print_item(item, indent + 4));
                s.push_str(",\n");
            }
            s.push_str(&pad_outer);
            s.push_str(close);
            s
        }
        _ => {
            let joined = items
                .iter()
                .map(|i| print_item(i, indent))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{open}{joined}{close}")
        }
    }
}

/// Tuple-only: a single element always requires a trailing comma (D-TYPE-01, applied
/// regardless of `was_multiline`).
fn print_tuple_lit(elements: &[Expr], was_multiline: bool, indent: usize) -> String {
    if elements.len() == 1 {
        format!("({},)", print_expr(&elements[0], indent))
    } else {
        print_bracket_list("(", ")", elements, was_multiline, indent, print_expr)
    }
}

fn print_args_paren(args: &[Arg], was_multiline: bool, indent: usize) -> String {
    print_bracket_list("(", ")", args, was_multiline, indent, print_arg)
}

fn print_arg(arg: &Arg, indent: usize) -> String {
    if arg.is_placeholder {
        return "_".to_owned();
    }
    match &arg.name {
        Some(name) => format!("{name}: {}", print_expr(&arg.value, indent)),
        None => print_expr(&arg.value, indent),
    }
}

fn print_type_args(type_args: &[TypeAnn]) -> String {
    if type_args.is_empty() {
        String::new()
    } else {
        let joined = type_args
            .iter()
            .map(print_type_ann)
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{joined}]")
    }
}

/// `f"..."`. `FStringSegment::Text` has already had its escapes resolved (by the lexer), so
/// on top of the usual string escaping, `{`/`}` are re-escaped to the inverse of D-LEX-07
/// (`{{`/`}}`).
fn print_fstring(segments: &[FStringSegment], indent: usize) -> String {
    let mut s = String::from("f\"");
    for seg in segments {
        match seg {
            FStringSegment::Text(t) => s.push_str(&escape_fstring_text(t)),
            FStringSegment::Expr(e) => {
                s.push('{');
                s.push_str(&print_expr(e, indent));
                s.push('}');
            }
        }
    }
    s.push('"');
    s
}

/// Reproduces D-SYN-05's continuation check ("the first token on the next line is `.`") via a
/// comparison of span line numbers: if the line where this link's own content (including
/// its arguments) begins differs from the receiver's end line, this is judged to have "been
/// broken across lines as a continuation," and fmt always normalizes the line break to the
/// base line (`indent`, the indentation of the statement's start) + 4 (the latter half of
/// D-SYN-05). If they match, it stays flush on the same line.
fn chain_break_prefix(
    receiver_end_line: u32,
    this_first_line: u32,
    indent: usize,
) -> Option<String> {
    if this_first_line == receiver_end_line {
        None
    } else {
        Some(format!("\n{}", " ".repeat(indent + 4)))
    }
}

fn print_method_call_chain(
    expr: &Expr,
    receiver: &Expr,
    method: &str,
    type_args: &[TypeAnn],
    args: &[Arg],
    was_multiline: bool,
    indent: usize,
) -> String {
    let recv_str = print_expr(receiver, indent);
    let args_str = print_args_paren(args, was_multiline, indent);
    let type_args_str = print_type_args(type_args);
    let link = format!(".{method}{type_args_str}{args_str}");
    let first_line = args
        .first()
        .map_or(expr.span.end.line, |a| a.value.span.start.line);
    match chain_break_prefix(receiver.span.end.line, first_line, indent) {
        Some(prefix) => format!("{recv_str}{prefix}{link}"),
        None => format!("{recv_str}{link}"),
    }
}

fn print_dot_chain(expr: &Expr, target: &Expr, link: &str, indent: usize) -> String {
    let recv_str = print_expr(target, indent);
    match chain_break_prefix(target.span.end.line, expr.span.end.line, indent) {
        Some(prefix) => format!("{recv_str}{prefix}{link}"),
        None => format!("{recv_str}{link}"),
    }
}

/// D-FMT-04 (revised): once there are 3 or more `|>` stages, always one stage per line (a
/// fixed rule independent of whether the source had line breaks, in contrast to D-FMT-05).
/// 2 or fewer (`x |> f`, `x |> f |> g`) are always kept on the same line regardless of
/// whether the source broke them — since both SPEC §6.3's `x |> parse? |> validate?` and
/// SPEC §15's `top |> toml.encode |> fs.write("top.toml", _)` write 2 `|>`s on one line, a
/// threshold of 2 would have fmt itself break the look of the SPEC's own body text, so the
/// owner revised the threshold to 3.
fn print_pipe(pipe: &PipeExpr, indent: usize) -> String {
    let source_str = print_expr(&pipe.source, indent);
    if pipe.stages.len() >= 3 {
        let pad = " ".repeat(indent + 4);
        let mut s = source_str;
        for stage in &pipe.stages {
            s.push('\n');
            s.push_str(&pad);
            s.push_str("|> ");
            s.push_str(&print_pipe_stage(stage, indent + 4));
        }
        s
    } else {
        let mut s = source_str;
        for stage in &pipe.stages {
            s.push_str(" |> ");
            s.push_str(&print_pipe_stage(stage, indent));
        }
        s
    }
}

fn print_pipe_stage(stage: &PipeStage, indent: usize) -> String {
    let mut s = match &stage.callee {
        PipeCallee::Bare(e) => print_expr(e, indent),
        PipeCallee::WithArgs { callee, args } => {
            let args_str = args
                .iter()
                .map(|a| print_arg(a, indent))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({args_str})", print_expr(callee, indent))
        }
    };
    if stage.question {
        s.push('?');
    }
    s
}

/// `(params) => body`. If the body is if/match (syntactically guaranteed to span multiple
/// lines, D-SYN-10), this breaks the line immediately after `=>`; otherwise it continues on
/// the same line (a distinction confirmed against `samples/ok/5-1_lambdas` — an ordinary
/// value/pipe/chain etc. body always continues directly after `=> `).
fn print_lambda(params: &[LambdaParam], body: &Expr, indent: usize) -> String {
    let params_str = params
        .iter()
        .map(|p| match &p.ty {
            Some(t) => format!("{}: {}", p.name, print_type_ann(t)),
            None => p.name.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    if is_block_shaped(body) {
        let pad = " ".repeat(indent + 4);
        format!("({params_str}) =>\n{pad}{}", print_expr(body, indent + 4))
    } else {
        format!("({params_str}) => {}", print_expr(body, indent))
    }
}

fn is_block_shaped(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::If(_) | ExprKind::Match { .. })
}

/// `if cond` (flush against what precedes it, no line break) + the then-branch (+4) +
/// `else` (same column as if, D-SYN-03) + the else-branch. If the else-branch is `ElseIf`
/// (D-SYN-03's multi-branch), this recurses into the same shape.
fn print_if_expr(if_expr: &IfExpr, indent: usize) -> String {
    let mut s = format!(
        "if {}\n{}",
        print_expr(&if_expr.cond, indent),
        print_block(&if_expr.then_branch, indent + 4)
    );
    s.push('\n');
    s.push_str(&" ".repeat(indent));
    s.push_str("else");
    match &if_expr.else_branch {
        ElseBranch::Block(block) => {
            s.push('\n');
            s.push_str(&print_block(block, indent + 4));
        }
        ElseBranch::ElseIf(inner) => {
            s.push('\n');
            s.push_str(&" ".repeat(indent + 4));
            s.push_str(&print_if_expr(inner, indent + 4));
        }
    }
    s
}

fn print_match_arms(arms: &[MatchArm], indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::new();
    let mut prev_end: Option<u32> = None;
    for arm in arms {
        let start_line = effective_start_line(&arm.leading_comments, None, arm.span.start.line);
        let rendered = print_match_arm(arm, indent);
        let end_line = true_end_line_of_match_arm(arm);
        append_with_blank_line_rule(
            &mut out,
            &mut prev_end,
            start_line,
            end_line,
            &rendered,
            &pad,
        );
    }
    out
}

/// `pattern => expr` (a single-expression arm) or `pattern =>` line break + block (D-SYN-11's
/// multi-statement arm).
fn print_match_arm(arm: &MatchArm, indent: usize) -> String {
    let pattern_str = print_pattern(&arm.pattern);
    let core = match &arm.body {
        MatchArmBody::Expr(e) => format!("{pattern_str} => {}", print_expr(e, indent)),
        MatchArmBody::Block(block) => {
            format!("{pattern_str} =>\n{}", print_block(block, indent + 4))
        }
    };
    assemble_with_leading_trailing(
        &arm.leading_comments,
        None,
        arm.span.start.line,
        arm.trailing_comment.as_ref(),
        indent,
        core,
    )
}

/// `par [..]` / `par (..)` (`Par` has no `was_multiline`, so it is always one line).
fn print_par(kind: &ParKind, elements: &[Expr], indent: usize) -> String {
    let joined = elements
        .iter()
        .map(|e| print_expr(e, indent))
        .collect::<Vec<_>>()
        .join(", ");
    match kind {
        ParKind::List => format!("par [{joined}]"),
        ParKind::Tuple => format!("par ({joined})"),
    }
}

fn binary_op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::EqEq => "==",
        BinaryOp::NotEq => "!=",
        BinaryOp::And => "and",
        BinaryOp::Or => "or",
    }
}

/// `f64::to_string()` rounds `1.0` down to `"1"`, so a `.0` is appended whenever there is no
/// decimal point or exponent notation, so the output doesn't lose the int/float
/// distinction.
fn format_float(f: f64) -> String {
    let s = f.to_string();
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

/// D-FMT-02: strings are always double-quoted. Since the lexer holds them with escapes
/// already resolved (as real characters, see token.rs), this re-escapes at print time
/// within the range D-LEX-06 supports.
fn escape_string_content(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out
}

/// For an f-string's Text segment: on top of the usual string escaping, this also performs
/// the inverse of D-LEX-07's `{{`/`}}` expansion (turning a lone `{`/`}` into `{{`/`}}`
/// respectively).
fn escape_fstring_text(s: &str) -> String {
    escape_string_content(s)
        .replace('{', "{{")
        .replace('}', "}}")
}

// ---------------------------------------------------------------------------
// Patterns (Pattern/SubPattern/LiteralPat) — D-SYN-06/07
// ---------------------------------------------------------------------------

fn print_pattern(p: &Pattern) -> String {
    match p {
        Pattern::Literal(lit, _) => print_literal_pat(lit),
        Pattern::BareIdent(name, _, _) => name.to_string(),
        Pattern::Wildcard(_) => "_".to_owned(),
        Pattern::Variant { name, fields, .. } => {
            if fields.is_empty() {
                name.to_string()
            } else {
                let joined = fields
                    .iter()
                    .map(print_sub_pattern)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{name}({joined})")
            }
        }
        Pattern::Tuple { elements, .. } => {
            let joined = elements
                .iter()
                .map(print_sub_pattern)
                .collect::<Vec<_>>()
                .join(", ");
            format!("({joined})")
        }
    }
}

fn print_sub_pattern(p: &SubPattern) -> String {
    match p {
        SubPattern::Literal(lit, _) => print_literal_pat(lit),
        SubPattern::BareIdent(name, _, _) => name.to_string(),
        SubPattern::Wildcard(_) => "_".to_owned(),
    }
}

fn print_literal_pat(lit: &LiteralPat) -> String {
    match lit {
        LiteralPat::Int(n) => n.to_string(),
        LiteralPat::Float(f) => format_float(*f),
        LiteralPat::Bool(b) => b.to_string(),
        LiteralPat::Str(s) => format!("\"{}\"", escape_string_content(s)),
    }
}

// ---------------------------------------------------------------------------
// Type annotations (TypeAnn) — ARCHITECTURE.md §3.6
// ---------------------------------------------------------------------------

fn print_type_ann(ty: &TypeAnn) -> String {
    match &ty.kind {
        TypeAnnKind::Named { name, args } => {
            if args.is_empty() {
                name.to_string()
            } else {
                let joined = args
                    .iter()
                    .map(print_type_ann)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{name}[{joined}]")
            }
        }
        TypeAnnKind::Tuple(elems) => {
            let joined = elems
                .iter()
                .map(print_type_ann)
                .collect::<Vec<_>>()
                .join(", ");
            format!("tuple[{joined}]")
        }
        TypeAnnKind::Function {
            params,
            effects,
            ret,
        } => {
            let p = params
                .iter()
                .map(print_type_ann)
                .collect::<Vec<_>>()
                .join(", ");
            let r = print_type_ann(ret);
            let u = print_uses_clause(effects);
            format!("({p}) -> {r}{u}")
        }
        TypeAnnKind::Void => "void".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::print_module;
    use crate::diagnostics::FileId;
    use crate::lexer::Lexer;
    use crate::parser::comment_attach::attach_comments;
    use crate::parser::parse_module;
    use std::path::{Path, PathBuf};

    fn sample_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("samples")
    }

    /// Splits off a shebang (if present) and returns the remainder as a `Module`, lexed,
    /// parsed, and with comments attached. Reproducing the shebang is outside
    /// `print_module`'s contract (`&Module` never holds a shebang at all — `lexer/mod.rs`
    /// already strips it at the character level) — that is expected to be driver.rs's
    /// (Unit 17's) responsibility, and this test harness does the same thing to compare
    /// against samples/fmt/'s `.out.ybm`.
    fn split_shebang(src: &str) -> (Option<&str>, &str) {
        if src.starts_with("#!") {
            if let Some(nl) = src.find('\n') {
                return (Some(&src[..=nl]), &src[nl + 1..]);
            }
            return (Some(src), "");
        }
        (None, src)
    }

    /// Lexes, parses, and attaches comments to `src`, and returns the text after fmt (the caller
    /// concatenates the shebang separately). Returns `None` if there is a lex/parse error.
    fn format_source(src: &str) -> Option<String> {
        let file = FileId(0);
        let (tokens, comments, lex_diag) = Lexer::new(src, file).tokenize();
        if !lex_diag.is_empty() {
            return None;
        }
        let (mut module, parse_diag) = parse_module(&tokens, file);
        if !parse_diag.is_empty() {
            return None;
        }
        attach_comments(&mut module, comments);
        Some(print_module(&module))
    }

    /// Formats while preserving the shebang (reproducing driver.rs's intended behavior on
    /// the test-harness side).
    ///
    /// `print_module` (the product code in this file) takes only `&Module` and knows
    /// nothing about a shebang at all — this is correct per the contract (reproducing the
    /// shebang is driver.rs's, Unit 17's, responsibility). However, lexing
    /// (`lexer/mod.rs::strip_shebang`, a file out of scope here) merely consumes the
    /// shebang line's `\n` by moving the cursor past it without resetting the line number
    /// back to 1, so the **absolute line number**, counted from the line after the shebang,
    /// remains as-is on comment/token spans. In other words, the fact of "whether there was
    /// a blank line between the shebang and the first content" is not lost, and can be
    /// observed as whether `rest` (the remaining source string with the shebang removed)
    /// itself starts with a blank line. To apply D-SYN-02 (blank lines don't affect block
    /// structure, and fmt normalizes consecutive blank lines to at most one) consistently
    /// right after the shebang too, this test harness restores one blank line in the
    /// formatted output as well if `rest` starts with a blank line (most of samples/ok/ is
    /// written in this `#!...`+blank line+explanatory comment form — this restoration will
    /// also be needed when driver.rs is eventually implemented, see the remaining concern
    /// noted in the report).
    fn format_file_text(src: &str) -> Option<String> {
        let (shebang, rest) = split_shebang(src);
        let formatted_rest = format_source(rest)?;
        Some(match shebang {
            Some(sb) => {
                let first_line = rest.split('\n').next().unwrap_or("");
                if !rest.is_empty() && first_line.trim().is_empty() {
                    format!("{sb}\n{formatted_rest}")
                } else {
                    format!("{sb}{formatted_rest}")
                }
            }
            None => formatted_rest,
        })
    }

    fn read_sample(path: &Path) -> String {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => panic!("failed to read sample {}: {e}", path.display()),
        }
    }

    fn list_fmt_sample_dirs() -> Vec<PathBuf> {
        let fmt_dir = sample_root().join("fmt");
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(&fmt_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect(),
            Err(e) => panic!("failed to read samples/fmt: {e}"),
        };
        entries.sort();
        entries
    }

    /// The result of formatting `sample.in.ybm` under every directory in samples/fmt/ must
    /// be byte-identical to `sample.out.ybm` (completion condition 4). With both cause A
    /// (missing unmarked comment right before a declaration) and cause B (blank-line
    /// position unrecoverable) fixed, this is verified with a strict, exceptionless
    /// full-match.
    #[test]
    fn fmt_samples_match_expected_output_byte_for_byte() {
        let entries = list_fmt_sample_dirs();
        assert!(
            !entries.is_empty(),
            "no directories found under samples/fmt"
        );
        let mut failures: Vec<String> = Vec::new();
        for dir in &entries {
            let in_path = dir.join("sample.in.ybm");
            let out_path = dir.join("sample.out.ybm");
            if !in_path.exists() || !out_path.exists() {
                continue;
            }
            let input = read_sample(&in_path);
            let expected = read_sample(&out_path);
            match format_file_text(&input) {
                Some(actual) if actual == expected => {}
                Some(actual) => failures.push(format!(
                    "{dir_disp}: mismatch\n--- expected ---\n{expected}\n--- actual ---\n{actual}",
                    dir_disp = dir.display()
                )),
                None => failures.push(format!("{}: lex/parse error", dir.display())),
            }
        }
        assert!(
            failures.is_empty(),
            "fmt samples mismatch:\n{}",
            failures.join("\n\n")
        );
    }

    /// None of samples/fmt/'s `.out.ybm` files change when formatted (a direct check of
    /// idempotency, completion condition 4).
    #[test]
    fn fmt_samples_out_files_are_idempotent() {
        let entries = list_fmt_sample_dirs();
        let mut failures: Vec<String> = Vec::new();
        for dir in &entries {
            let out_path = dir.join("sample.out.ybm");
            if !out_path.exists() {
                continue;
            }
            let expected = read_sample(&out_path);
            match format_file_text(&expected) {
                Some(actual) if actual == expected => {}
                Some(actual) => failures.push(format!(
                    "{dir_disp}: formatting out.ybm changed it\n--- before ---\n{expected}\n--- after ---\n{actual}",
                    dir_disp = dir.display()
                )),
                None => failures.push(format!("{}: lex/parse error", dir.display())),
            }
        }
        assert!(
            failures.is_empty(),
            "out.ybm idempotency violation:\n{}",
            failures.join("\n\n")
        );
    }

    fn discover_ybm_files(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(root) {
            Ok(rd) => rd,
            Err(e) => panic!("failed to read directory {}: {e}", root.display()),
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                out.extend(discover_ybm_files(&path));
            } else if path.extension().is_some_and(|e| e == "ybm") {
                out.push(path);
            }
        }
        out
    }

    /// Verifies idempotency (applying fmt twice equals applying it once) for every `.ybm` under samples/ (completion
    /// condition 3). Files that fail to lex/parse (some under samples/err/static) are
    /// skipped as out of scope for fmt — fmt is regeneration from an already-parsed AST,
    /// and an input with a syntax error has no AST to begin with.
    #[test]
    fn all_samples_are_idempotent_under_fmt() {
        let all = discover_ybm_files(&sample_root());
        assert!(
            all.len() >= 150,
            "fewer .ybm files under samples/ than expected: {}",
            all.len()
        );
        let mut checked = 0usize;
        let mut skipped = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for path in &all {
            let src = read_sample(path);
            let Some(once) = format_file_text(&src) else {
                skipped += 1;
                continue;
            };
            let Some(twice) = format_file_text(&once) else {
                failures.push(format!(
                    "{}: the first fmt result failed to parse on the second pass",
                    path.display()
                ));
                continue;
            };
            if once == twice {
                checked += 1;
            } else {
                failures.push(format!(
                    "{}: fmt(fmt(x)) != fmt(x)\n--- once ---\n{once}\n--- twice ---\n{twice}",
                    path.display()
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "idempotency violation ({checked} succeeded / {skipped} skipped):\n{}",
            failures.join("\n\n")
        );
    }

    /// Not a single byte of any `.ybm` under samples/ok/ changes when formatted (completion
    /// condition 2). Causes A through E have all been fixed (either by fixing fmt itself, or
    /// by overwriting non-canonical form on the samples side with fmt's output), so the
    /// exception-based classification logic has been removed, and only a strict full byte
    /// match is verified.
    #[test]
    fn ok_samples_are_unchanged_by_fmt() {
        let ok_dir = sample_root().join("ok");
        let all = discover_ybm_files(&ok_dir);
        assert!(!all.is_empty(), "no .ybm files found under samples/ok");
        let mut failures: Vec<String> = Vec::new();
        for path in &all {
            let src = read_sample(path);
            let Some(formatted) = format_file_text(&src) else {
                failures.push(format!("{}: lex/parse error", path.display()));
                continue;
            };
            if formatted != src {
                failures.push(format!(
                    "{}: changed by fmt\n--- original ---\n{src}\n--- formatted ---\n{formatted}",
                    path.display()
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "ok/ was changed by fmt:\n{}",
            failures.join("\n\n")
        );
    }

    #[test]
    fn formatter_preserves_field_trailing_and_interleaved_doc_comments() {
        let source = "## before\n## ```\n## assert(true)\n## ```\n## between\n## ```\n## assert(true)\n## ```\nstruct User\n    # field note\n    name: str # trailing\n\n# eof note\n";
        assert_eq!(format_source(source).as_deref(), Some(source));
    }
}
