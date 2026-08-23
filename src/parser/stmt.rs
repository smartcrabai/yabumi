//! Statement parser, if/match, blocks (ARCHITECTURE.md §2.1).

use super::{Parser, span_between};
use crate::ast::{
    Block, ElseBranch, Expr, ExprKind, IfExpr, MatchArm, MatchArmBody, Stmt, StmtKind,
};
use crate::diagnostics::ErrorCode;
use crate::lexer::TokenKind;
use std::sync::Arc;

impl Parser<'_> {
    /// Parses an indented block (the sequence of statements between `Indent` and `Dedent`).
    /// At call time the current position is normally `Newline` (right after the heading
    /// line) -- consume it if present, then expect `Indent`. When `Indent` is not found (e.g.
    /// the structure has been thrown off by error recovery from a D-SYN-01 violation), this
    /// returns an empty block without pushing a new diagnostic, and continues processing
    /// (preventing a cascade of secondary diagnostics).
    pub(crate) fn parse_block(&mut self) -> Block {
        let start_span = self.current_span();
        if matches!(self.peek_kind(), Some(TokenKind::Newline)) {
            self.bump();
        }
        if !matches!(self.peek_kind(), Some(TokenKind::Indent)) {
            return Block {
                stmts: Vec::new(),
                span: start_span,
            };
        }
        self.bump(); // Indent
        let mut stmts = Vec::new();
        loop {
            self.skip_blank_newlines();
            match self.peek_kind() {
                Some(TokenKind::Dedent) => {
                    self.bump();
                    break;
                }
                None | Some(TokenKind::Eof) => break,
                _ => stmts.push(self.parse_stmt()),
            }
        }
        let end_span = self.previous_span();
        Block {
            stmts,
            span: span_between(start_span, end_span),
        }
    }

    /// Parses one statement (one of: `var` declaration / assignment / `_ = expr` /
    /// `return` / expression statement). If a `##`/`#` comment is attached at the front,
    /// `comment_attach.rs` later assigns it to `Stmt.doc_comment`/`leading_comments` (this
    /// parser itself never looks at comments).
    pub(crate) fn parse_stmt(&mut self) -> Stmt {
        let start_span = self.current_span();
        let kind = match self.peek_kind() {
            Some(TokenKind::Var) => self.parse_var_decl_kind(),
            Some(TokenKind::Return) => self.parse_return_kind(),
            Some(TokenKind::Underscore) if self.peek_kind_at(1) == Some(&TokenKind::Eq) => {
                self.bump(); // '_'
                self.bump(); // '='
                let value = self.parse_expr();
                StmtKind::Discard(value)
            }
            _ => self.parse_assignment_or_expr_kind(),
        };
        let end_span = self.previous_span();
        Stmt {
            kind,
            span: span_between(start_span, end_span),
            doc_comment: None,
            leading_comments: Vec::new(),
            trailing_comment: None,
        }
    }

    /// `var name (: TypeAnn)? = expr`. Always a new mutable binding in the current scope.
    fn parse_var_decl_kind(&mut self) -> StmtKind {
        self.bump(); // 'var'
        let name = self.expect_var_binding_name("variable name");
        let ty = if matches!(self.peek_kind(), Some(TokenKind::Colon)) {
            self.bump();
            Some(self.parse_type_ann())
        } else {
            None
        };
        self.expect(&TokenKind::Eq, "`=`");
        let value = self.parse_expr();
        StmtKind::VarDecl { name, ty, value }
    }

    /// The binding name for a `var` declaration: accepts an ordinary identifier, plus the
    /// "reserved discard identifier `_`" that D-LINT-01 spells out (`var _ = ...`). `_` is
    /// a dedicated token listed in D-LEX-01's reserved-word table (`TokenKind::Underscore`),
    /// not an ordinary `Ident`, so it is checked here, specific to this one position, rather
    /// than through the general-purpose `expect_ident` (which is also used at places -- a
    /// struct name/function name/field name, etc. -- that must never allow `_`) (**an item
    /// discovered and fixed during this review**: previously, a discard var declaration such
    /// as `var _ = 0` went through `expect_ident`, which reported an E0502 expecting an
    /// "identifier", and this did not mesh with the subsequent token stream either, causing
    /// E0502 to cascade -- which conflicted with the result that
    /// samples/err/static/6-1_match_and_branch_errors/entry_block_tail_non_expression.ybm
    /// expects, namely "the only diagnostic is E1020").
    fn expect_var_binding_name(&mut self, what: &str) -> Arc<str> {
        if matches!(self.peek_kind(), Some(TokenKind::Underscore)) {
            self.bump();
            return Arc::from("_");
        }
        self.expect_ident(what)
    }

    /// `return` (no return value) or `return expr`.
    fn parse_return_kind(&mut self) -> StmtKind {
        self.bump(); // 'return'
        if matches!(
            self.peek_kind(),
            None | Some(TokenKind::Newline | TokenKind::Dedent | TokenKind::Eof)
        ) {
            StmtKind::Return(None)
        } else {
            StmtKind::Return(Some(self.parse_expr()))
        }
    }

    /// First parses the left-hand side as an ordinary expression (`x` / `x.field` / `x[i]`
    /// are all valid expressions), then settles on one of `NameAssign`/`FieldAssign`/
    /// `IndexAssign`/an expression statement, based on whether the following token is `:`/`=`
    /// and on the shape of that expression (`Ident`/`FieldAccess`/`Index`) (a decision made
    /// here, as shown by the StmtKind::NameAssign comment in ARCHITECTURE.md §3.4: the parser
    /// does not know "is x an existing variable", so distinguishing assignment from a new
    /// binding is left to the type-checking phase).
    fn parse_assignment_or_expr_kind(&mut self) -> StmtKind {
        let lhs = self.parse_expr();
        match self.peek_kind() {
            Some(TokenKind::Colon) => {
                self.bump();
                let ty = self.parse_type_ann();
                self.expect(&TokenKind::Eq, "`=`");
                let value = self.parse_expr();
                if let ExprKind::Ident(name) = lhs.kind {
                    StmtKind::NameAssign {
                        name,
                        ty: Some(ty),
                        value,
                    }
                } else {
                    self.push_diag(
                        ErrorCode::UnexpectedToken,
                        lhs.span,
                        "the left-hand side of a type-annotated assignment must be an identifier",
                    );
                    StmtKind::NameAssign {
                        name: Arc::from(""),
                        ty: Some(ty),
                        value,
                    }
                }
            }
            Some(TokenKind::Eq) => {
                self.bump();
                let value = self.parse_expr();
                match lhs.kind {
                    ExprKind::Ident(name) => StmtKind::NameAssign {
                        name,
                        ty: None,
                        value,
                    },
                    ExprKind::FieldAccess { target, field } => StmtKind::FieldAssign {
                        target: *target,
                        field,
                        value,
                    },
                    ExprKind::Index { target, index } => StmtKind::IndexAssign {
                        target: *target,
                        index: *index,
                        value,
                    },
                    other => {
                        self.push_diag(
                            ErrorCode::UnexpectedToken,
                            lhs.span,
                            "invalid expression for the left-hand side of an assignment",
                        );
                        StmtKind::ExprStmt(Expr {
                            id: lhs.id,
                            kind: other,
                            span: lhs.span,
                        })
                    }
                }
            }
            _ => StmtKind::ExprStmt(lhs),
        }
    }

    /// `if cond \n Block \n else \n (Block | if ...)`. else is mandatory (per the note in
    /// §3.4, "an if is always an expression": an `if` without else is a syntax error).
    pub(crate) fn parse_if(&mut self) -> IfExpr {
        let start_span = self.current_span();
        self.bump(); // 'if'
        let cond = Box::new(self.parse_expr());
        let then_branch = self.parse_branch_block();
        let else_branch = if matches!(self.peek_kind(), Some(TokenKind::Else)) {
            self.bump();
            if matches!(self.peek_kind(), Some(TokenKind::If)) {
                ElseBranch::ElseIf(Box::new(self.parse_if()))
            } else {
                ElseBranch::Block(self.parse_branch_block())
            }
        } else {
            let span = self.current_span();
            self.push_diag(
                ErrorCode::UnexpectedToken,
                span,
                "an if expression requires else (D-SYN-03)",
            );
            self.skip_to_sync_point();
            ElseBranch::Block(Block {
                stmts: Vec::new(),
                span,
            })
        };
        let end_span = self.previous_span();
        IfExpr {
            cond,
            then_branch,
            else_branch,
            span: span_between(start_span, end_span),
        }
    }

    /// One branch body of if/else. If `Newline`+`Indent` follows, this is an ordinary
    /// multi-statement block (D-SYN-11); otherwise a single expression is wrapped as a
    /// one-statement block (D-SYN-10: a lambda body in a parenthesis-suppressed context, and
    /// a branch value written on the same line).
    fn parse_branch_block(&mut self) -> Block {
        if self.is_indented_block_ahead() {
            self.parse_block()
        } else {
            let expr = self.parse_expr();
            let span = expr.span;
            Block {
                stmts: vec![Stmt {
                    kind: StmtKind::ExprStmt(expr),
                    span,
                    doc_comment: None,
                    leading_comments: Vec::new(),
                    trailing_comment: None,
                }],
                span,
            }
        }
    }

    /// Parses a single arm of `match scrutinee \n (pattern => expr | pattern =>\n Block)*`
    /// (a single expression or a multi-statement block, a target of D-SYN-11). The loop over
    /// the whole arm list is handled by expr.rs's `match` expression parsing (called from
    /// `parse_primary`).
    pub(crate) fn parse_match_arm(&mut self) -> MatchArm {
        let start_span = self.current_span();
        let pattern = self.parse_pattern();
        self.expect(&TokenKind::FatArrow, "`=>`");
        let body = if self.is_indented_block_ahead() {
            MatchArmBody::Block(self.parse_block())
        } else {
            MatchArmBody::Expr(self.parse_expr())
        };
        let end_span = self.previous_span();
        MatchArm {
            pattern,
            body,
            leading_comments: Vec::new(),
            trailing_comment: None,
            span: span_between(start_span, end_span),
        }
    }
}
