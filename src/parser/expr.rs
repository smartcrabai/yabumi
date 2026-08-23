//! A Pratt-style expression parser (the D-OP-01 precedence table, ARCHITECTURE.md §2.1).
//!
//! Implements the D-OP-01 precedence table (binding more tightly toward the top) directly as
//! recursive-descent tiers:
//! 1 (tightest, postfix `()` `[]` `.` `?`) -> 2 (unary `-` `not`) -> 3 (`*` `/` `%`) ->
//! 4 (`+` `-`) -> 5 (comparison, no chaining) -> 6 (`==` `!=`) -> 7 (`and`) -> 8 (`or`) ->
//! 9 (loosest, pipe `|>`).
//! `parse_logical` implements the two tiers 7/8 (and/or), `parse_comparison` implements the
//! two tiers 5/6 (comparison/equality), and `parse_arithmetic` implements the two tiers 3/4
//! (term/add-sub), each within a single function with private helper functions interposed
//! (Unit0 fixed only the names of the three composite functions `parse_logical`/
//! `parse_comparison`/`parse_arithmetic` out of the table's nine tiers; helper functions that
//! split the internals further can be added freely).

use super::{Parser, span_between};
use crate::ast::{
    Arg, BinaryOp, Expr, ExprKind, FStringSegment, LambdaParam, ParKind, PipeCallee, PipeExpr,
    PipeStage, UnaryOp,
};
use crate::diagnostics::{ErrorCode, Span};
use crate::lexer::{Token, TokenKind};
use std::sync::Arc;

impl<'a> Parser<'a> {
    /// The entry point for expressions as a whole. Implemented as a Pratt-style parser that
    /// steps down through `parse_pipe` -> `parse_logical` -> ... -> `parse_unary` ->
    /// `parse_postfix` -> `parse_primary`, following the D-OP-01 precedence table (pipe `|>`
    /// is loosest). The recursion-depth guard from the R4 decision (§5.11) is applied exactly
    /// once, here -- every nested expression (parentheses, collection elements, call
    /// arguments, etc.) always goes back through this `parse_expr`, so guarding this single
    /// spot protects every recursive path.
    pub(crate) fn parse_expr(&mut self) -> Expr {
        let span = self.current_span();
        if !self.depth_enter(span) {
            return self.poison_expr(span);
        }
        let result = self.parse_pipe();
        self.depth_exit();
        result
    }

    /// Precedence 9 (loosest). `|>` is left-associative. Determines the stage's trailing `?`
    /// and bare-name / `_`-placeholder calls.
    pub(crate) fn parse_pipe(&mut self) -> Expr {
        let source = self.parse_logical();
        if !matches!(self.peek_kind(), Some(TokenKind::PipeOp)) {
            return source;
        }
        let start_span = source.span;
        let mut stages = Vec::new();
        while matches!(self.peek_kind(), Some(TokenKind::PipeOp)) {
            self.bump();
            stages.push(self.parse_pipe_stage());
        }
        let end_span = stages.last().map_or(start_span, |s| s.span);
        Expr {
            id: self.next_node_id(),
            kind: ExprKind::Pipe(PipeExpr {
                source: Box::new(source),
                stages,
            }),
            span: span_between(start_span, end_span),
        }
    }

    /// One stage of a pipe: a bare name (`json.encode`), or a call containing a `_`
    /// placeholder (`fs.write("out.json", _)`), plus an optional trailing stage `?`. The
    /// callee's name portion (a chain of a bare ident + `.field`) is parsed with a dedicated
    /// lookahead that never syntactically allows an ordinary postfix `(` call (because the
    /// callee itself must not contain parentheses, as in `x |> f()` -- whether parentheses
    /// are present is itself the syntactic element that decides "Bare" versus "WithArgs", so
    /// `parse_postfix`'s general postfix chain cannot be reused).
    fn parse_pipe_stage(&mut self) -> PipeStage {
        let start_span = self.current_span();
        let callee_expr = self.parse_pipe_callee_target();
        let callee = if matches!(self.peek_kind(), Some(TokenKind::LParen)) {
            let (args, _was_multiline) = self.parse_call_args();
            let has_placeholder = args.iter().any(|a| a.is_placeholder);
            if !has_placeholder {
                let span = self.previous_span();
                self.push_diag(
                    ErrorCode::PipePlaceholderMissing,
                    span,
                    "a pipe destination with arguments requires a `_` placeholder (SPEC §6.3, D-DIAG-02 E0503)",
                );
            }
            PipeCallee::WithArgs {
                callee: Box::new(callee_expr),
                args,
            }
        } else {
            PipeCallee::Bare(callee_expr)
        };
        let question = matches!(self.peek_kind(), Some(TokenKind::Question));
        if question {
            self.bump();
            // D-PAR-03: a pipe's trailing stage `?` (SPEC §6.3) also goes through the same
            // Rust-side early-return mechanism as `ExprKind::Question`'s trailing `?`
            // (Flow::Return, ARCHITECTURE.md §5.6), so it is likewise forbidden inside a par
            // branch (or a lambda body passed to one). This is the pipe-path-specific
            // application of D-PAR-03, paired with `parse_postfix`'s ordinary `?` check.
            if self.bare_question_forbidden {
                let span = self.previous_span();
                self.push_diag(
                    ErrorCode::UnexpectedToken,
                    span,
                    "a trailing pipe-stage `?` inside a par branch (or a lambda body passed to one) is forbidden (D-PAR-03)",
                );
            }
        }
        let span = span_between(start_span, self.previous_span());
        PipeStage {
            callee,
            question,
            span,
        }
    }

    /// The pipe destination's name portion: a chain of a bare identifier + `.field`
    /// (`json` / `json.encode` / `fs.write`). Never consumes `(`/`[`/`?` -- whether call
    /// parentheses are present is decided separately by `parse_pipe_stage`.
    fn parse_pipe_callee_target(&mut self) -> Expr {
        let start_span = self.current_span();
        let name = self.expect_ident("pipe destination function name/namespace name");
        let mut expr = self.leaf_expr(ExprKind::Ident(name), start_span);
        while matches!(self.peek_kind(), Some(TokenKind::Dot)) {
            self.bump();
            let field = self.expect_ident("field/method name");
            let span = span_between(expr.span, self.previous_span());
            expr = Expr {
                id: self.next_node_id(),
                kind: ExprKind::FieldAccess {
                    target: Box::new(expr),
                    field,
                },
                span,
            };
        }
        expr
    }

    /// Precedence 7-8. `and`/`or` (left-associative). `or` binds more loosely than `and`.
    pub(crate) fn parse_logical(&mut self) -> Expr {
        let mut lhs = self.parse_and();
        while matches!(self.peek_kind(), Some(TokenKind::Or)) {
            self.bump();
            let rhs = self.parse_and();
            let span = span_between(lhs.span, rhs.span);
            lhs = Expr {
                id: self.next_node_id(),
                kind: ExprKind::Binary {
                    op: BinaryOp::Or,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        lhs
    }

    fn parse_and(&mut self) -> Expr {
        let mut lhs = self.parse_comparison();
        while matches!(self.peek_kind(), Some(TokenKind::And)) {
            self.bump();
            let rhs = self.parse_comparison();
            let span = span_between(lhs.span, rhs.span);
            lhs = Expr {
                id: self.next_node_id(),
                kind: ExprKind::Binary {
                    op: BinaryOp::And,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        lhs
    }

    /// Precedence 5-6. `==` `!=` (left-associative), below which sit `<` `<=` `>` `>=`
    /// (chained comparison not allowed, D-OP-01).
    pub(crate) fn parse_comparison(&mut self) -> Expr {
        let mut lhs = self.parse_relational();
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::EqEq) => BinaryOp::EqEq,
                Some(TokenKind::NotEq) => BinaryOp::NotEq,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_relational();
            let span = span_between(lhs.span, rhs.span);
            lhs = Expr {
                id: self.next_node_id(),
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        lhs
    }

    /// `<` `<=` `>` `>=` is applied at most once (not in a loop) -- this directly realizes
    /// D-OP-01's constraint that chained comparisons such as `a < b < c` cannot be written,
    /// by never consuming a second comparison operator (an unconsumed operator is then
    /// detected as an unexpected token by the statement-termination check).
    fn parse_relational(&mut self) -> Expr {
        let lhs = self.parse_arithmetic();
        let op = match self.peek_kind() {
            Some(TokenKind::Lt) => BinaryOp::Lt,
            Some(TokenKind::LtEq) => BinaryOp::LtEq,
            Some(TokenKind::Gt) => BinaryOp::Gt,
            Some(TokenKind::GtEq) => BinaryOp::GtEq,
            _ => return lhs,
        };
        self.bump();
        let rhs = self.parse_arithmetic();
        let span = span_between(lhs.span, rhs.span);
        Expr {
            id: self.next_node_id(),
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            span,
        }
    }

    /// Precedence 3-4. `+` `-` (left-associative), below which sit `*` `/` `%` (left-associative).
    pub(crate) fn parse_arithmetic(&mut self) -> Expr {
        let mut lhs = self.parse_term();
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Plus) => BinaryOp::Add,
                Some(TokenKind::Minus) => BinaryOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_term();
            let span = span_between(lhs.span, rhs.span);
            lhs = Expr {
                id: self.next_node_id(),
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        lhs
    }

    fn parse_term(&mut self) -> Expr {
        let mut lhs = self.parse_unary();
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Star) => BinaryOp::Mul,
                Some(TokenKind::Slash) => BinaryOp::Div,
                Some(TokenKind::Percent) => BinaryOp::Mod,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary();
            let span = span_between(lhs.span, rhs.span);
            lhs = Expr {
                id: self.next_node_id(),
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
        }
        lhs
    }

    /// Precedence 2. Prefix `-`/`not`. The right-recursion itself (e.g. `not not x`) is also
    /// made subject to the nesting-depth guard (since this is the one recursive path that
    /// does not go through `parse_expr`, satisfying the intent of the R4 decision requires
    /// guarding it individually -- a decision made in this parser implementation).
    pub(crate) fn parse_unary(&mut self) -> Expr {
        let start_span = self.current_span();
        let op = match self.peek_kind() {
            Some(TokenKind::Minus) => UnaryOp::Neg,
            Some(TokenKind::Not) => UnaryOp::Not,
            _ => return self.parse_postfix(),
        };
        self.bump();
        let operand = if self.depth_enter(start_span) {
            let result = self.parse_unary();
            self.depth_exit();
            result
        } else {
            self.poison_expr(start_span)
        };
        let span = span_between(start_span, operand.span);
        Expr {
            id: self.next_node_id(),
            kind: ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
            span,
        }
    }

    /// Precedence 1 (tightest). Left-associative sequential application of postfix
    /// `()` `[]` `.` `?` (`f(x)?.y` is `(f(x)?).y`, D-OP-02).
    ///
    /// The syntactic ambiguity between an explicit-type-argument call `f[Type, ...](args)`
    /// and indexing `xs[i]` (both can take the same shape `ident [ ... ]`) is resolved by
    /// having `at_explicit_type_args` look ahead for two conditions: the content right after
    /// `[` starts with a token that looks like a type annotation, and the matching `]` is
    /// immediately followed by `(` (not in ARCHITECTURE.md -- a decision made in this parser
    /// implementation: in observed Yabumi code, explicit type arguments only ever appear in
    /// this shape, while indexing such as `xs[i]` either has no `(` right after the `]`, or
    /// its content is not valid as a type annotation).
    pub(crate) fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        loop {
            match self.peek_kind() {
                Some(TokenKind::Dot) => {
                    self.bump();
                    expr = self.parse_dot_postfix(expr);
                }
                Some(TokenKind::LParen) => {
                    let (args, was_multiline) = self.parse_call_args();
                    let span = span_between(expr.span, self.previous_span());
                    expr = Expr {
                        id: self.next_node_id(),
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            type_args: Vec::new(),
                            args,
                            was_multiline,
                        },
                        span,
                    };
                }
                Some(TokenKind::LBracket) if self.at_explicit_type_args() => {
                    let type_args = self.parse_type_arg_list();
                    let (args, was_multiline) = self.parse_call_args();
                    let span = span_between(expr.span, self.previous_span());
                    expr = Expr {
                        id: self.next_node_id(),
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            type_args,
                            args,
                            was_multiline,
                        },
                        span,
                    };
                }
                Some(TokenKind::LBracket) => {
                    self.bump();
                    let index = self.parse_expr();
                    self.expect(&TokenKind::RBracket, "`]`");
                    let span = span_between(expr.span, self.previous_span());
                    expr = Expr {
                        id: self.next_node_id(),
                        kind: ExprKind::Index {
                            target: Box::new(expr),
                            index: Box::new(index),
                        },
                        span,
                    };
                }
                Some(TokenKind::Question) => {
                    self.bump();
                    if self.bare_question_forbidden {
                        let span = self.previous_span();
                        self.push_diag(
                            ErrorCode::UnexpectedToken,
                            span,
                            "a bare `?` inside a par branch (or a lambda body passed to one) is forbidden (D-PAR-03)",
                        );
                    }
                    let span = span_between(expr.span, self.previous_span());
                    expr = Expr {
                        id: self.next_node_id(),
                        kind: ExprKind::Question {
                            target: Box::new(expr),
                        },
                        span,
                    };
                }
                _ => break,
            }
        }
        expr
    }

    /// Parses whichever of `0` (a tuple index), `field`, or `method(args)` follows right
    /// after `.` (already consumed).
    fn parse_dot_postfix(&mut self, receiver: Expr) -> Expr {
        let receiver_span = receiver.span;
        match self.peek_kind() {
            Some(TokenKind::IntLiteral(n)) => {
                let n = *n;
                self.bump();
                let index = u32::try_from(n)
                    .unwrap_or_else(|_| unreachable!("lexer rejects tuple indices above u32"));
                let span = span_between(receiver_span, self.previous_span());
                Expr {
                    id: self.next_node_id(),
                    kind: ExprKind::TupleIndex {
                        target: Box::new(receiver),
                        index,
                    },
                    span,
                }
            }
            Some(TokenKind::Ident(_)) => {
                let name = self.expect_ident("field/method name");
                let type_args = if self.at_explicit_type_args() {
                    self.parse_type_arg_list()
                } else {
                    Vec::new()
                };
                if matches!(self.peek_kind(), Some(TokenKind::LParen)) {
                    let (args, was_multiline) = self.parse_call_args();
                    let span = span_between(receiver_span, self.previous_span());
                    Expr {
                        id: self.next_node_id(),
                        kind: ExprKind::MethodCall {
                            receiver: Box::new(receiver),
                            method: name,
                            type_args,
                            args,
                            was_multiline,
                        },
                        span,
                    }
                } else {
                    if !type_args.is_empty() {
                        let span = self.previous_span();
                        self.push_diag(
                            ErrorCode::UnexpectedToken,
                            span,
                            "an explicit type argument list must be followed by a call `(...)`",
                        );
                    }
                    let span = span_between(receiver_span, self.previous_span());
                    Expr {
                        id: self.next_node_id(),
                        kind: ExprKind::FieldAccess {
                            target: Box::new(receiver),
                            field: name,
                        },
                        span,
                    }
                }
            }
            _ => {
                let span = self.current_span();
                self.push_diag(
                    ErrorCode::UnexpectedToken,
                    span,
                    "a field name, method name, or tuple index (a number) is required after `.`",
                );
                receiver
            }
        }
    }

    /// Parses literals, identifiers, parentheses, list/dict/set/tuple literals, lambdas, and if/match/par.
    pub(crate) fn parse_primary(&mut self) -> Expr {
        let start_span = self.current_span();
        match self.peek_kind() {
            Some(TokenKind::IntLiteral(n)) => {
                let n = *n;
                self.bump();
                self.leaf_expr(ExprKind::IntLit(n), start_span)
            }
            Some(TokenKind::FloatLiteral(f)) => {
                let f = *f;
                self.bump();
                self.leaf_expr(ExprKind::FloatLit(f), start_span)
            }
            Some(TokenKind::True) => {
                self.bump();
                self.leaf_expr(ExprKind::BoolLit(true), start_span)
            }
            Some(TokenKind::False) => {
                self.bump();
                self.leaf_expr(ExprKind::BoolLit(false), start_span)
            }
            Some(TokenKind::StringLiteral(s)) => {
                let s = s.clone();
                self.bump();
                self.leaf_expr(ExprKind::StringLit(s), start_span)
            }
            Some(TokenKind::FString(_)) => {
                let segments = self.parse_fstring_segments();
                let span = span_between(start_span, self.previous_span());
                Expr {
                    id: self.next_node_id(),
                    kind: ExprKind::FString(segments),
                    span,
                }
            }
            Some(TokenKind::KwSelf) => {
                self.bump();
                self.leaf_expr(ExprKind::Ident(Arc::from("self")), start_span)
            }
            Some(TokenKind::Ident(_)) => {
                let name = self.expect_ident("identifier");
                self.leaf_expr(ExprKind::Ident(name), start_span)
            }
            Some(TokenKind::LParen) => self.parse_paren_expr(start_span),
            Some(TokenKind::LBracket) => self.parse_list_lit(start_span),
            Some(TokenKind::LBrace) => self.parse_brace_lit(start_span),
            Some(TokenKind::If) => {
                let if_expr = self.parse_if();
                let span = if_expr.span;
                Expr {
                    id: self.next_node_id(),
                    kind: ExprKind::If(Box::new(if_expr)),
                    span,
                }
            }
            Some(TokenKind::Match) => self.parse_match_expr(start_span),
            Some(TokenKind::Par) => self.parse_par_expr(start_span),
            _ => {
                self.push_diag(
                    ErrorCode::UnexpectedToken,
                    start_span,
                    "expected an expression but did not find one",
                );
                self.bump();
                self.poison_expr(start_span)
            }
        }
    }

    fn leaf_expr(&mut self, kind: ExprKind, span: Span) -> Expr {
        Expr {
            id: self.next_node_id(),
            kind,
            span,
        }
    }

    /// A harmless dummy expression (`0`) used for syntax-error recovery. Used only to let
    /// parsing continue, in keeping with the spirit of D-CLI-03's collect-everything
    /// approach -- it is never actually evaluated (a diagnostic has already been emitted
    /// before the type-checking stage, so later phases never run, ARCHITECTURE.md §4.1).
    fn poison_expr(&mut self, span: Span) -> Expr {
        Expr {
            id: self.next_node_id(),
            kind: ExprKind::IntLit(0),
            span,
        }
    }

    /// An expression starting with `(`: either a lambda (`(params) => body`), a grouping
    /// (`(expr)`), or a tuple literal (`(a, b, ...)`; a single element requires `(a,)`,
    /// D-TYPE-01). Since `(x)` is ambiguous between a grouping and a one-element implicit
    /// tuple, first look ahead at whether the matching `)` is immediately followed by `=>` to
    /// settle whether it is a lambda -- if not a lambda, grouping versus tuple is decided by
    /// what follows (whether there is a comma).
    fn parse_paren_expr(&mut self, start_span: Span) -> Expr {
        if self.matching_close_followed_by(&TokenKind::FatArrow) {
            return self.parse_lambda(start_span);
        }
        self.bump(); // '('
        if matches!(self.peek_kind(), Some(TokenKind::RParen)) {
            self.bump();
            let span = span_between(start_span, self.previous_span());
            return Expr {
                id: self.next_node_id(),
                kind: ExprKind::TupleLit {
                    elements: Vec::new(),
                    was_multiline: false,
                },
                span,
            };
        }
        let first = self.parse_expr();
        if matches!(self.peek_kind(), Some(TokenKind::Comma)) {
            self.bump();
            let mut elements = vec![first];
            elements.extend(self.parse_comma_separated(&TokenKind::RParen, Self::parse_expr));
            let close_line = self.current_span().start.line;
            self.expect(&TokenKind::RParen, "`)`");
            let was_multiline = start_span.start.line != close_line;
            let span = span_between(start_span, self.previous_span());
            Expr {
                id: self.next_node_id(),
                kind: ExprKind::TupleLit {
                    elements,
                    was_multiline,
                },
                span,
            }
        } else {
            self.expect(&TokenKind::RParen, "`)`");
            let span = span_between(start_span, self.previous_span());
            Expr {
                id: self.next_node_id(),
                kind: ExprKind::Grouping(Box::new(first)),
                span,
            }
        }
    }

    /// Assuming the current position is an opening bracket (`(`/`[`/`{`), determines without
    /// consuming anything whether the token immediately after the matching closing bracket
    /// is `expected`. Shared by the two syntactic-ambiguity lookaheads: `)` followed by `=>`
    /// distinguishes a lambda `(params) => body` from a grouping/tuple (parse_paren_expr),
    /// and `]` followed by `(` distinguishes an explicit-type-argument call `f[Type](...)`
    /// from index access (at_explicit_type_args).
    fn matching_close_followed_by(&self, expected: &TokenKind) -> bool {
        let mut depth: i32 = 0;
        let mut i = self.pos;
        loop {
            match self.tokens.get(i).map(|t| &t.kind) {
                Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace) => depth += 1,
                Some(TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace) => {
                    depth -= 1;
                    if depth == 0 {
                        return self.tokens.get(i + 1).map(|t| &t.kind) == Some(expected);
                    }
                }
                None | Some(TokenKind::Eof) => return false,
                Some(_) => {}
            }
            i += 1;
        }
    }

    /// `(` has already been confirmed (not yet consumed). Per D-SYN-10, a lambda body is a
    /// single expression only. A nested lambda establishes its own `?` scope.
    fn parse_lambda(&mut self, start_span: Span) -> Expr {
        self.bump(); // '('
        let params = self.parse_comma_separated(&TokenKind::RParen, Self::parse_lambda_param);
        self.expect(&TokenKind::RParen, "`)`");
        self.expect(&TokenKind::FatArrow, "`=>`");
        let saved_flag = self.bare_question_forbidden;
        self.bare_question_forbidden = false;
        let body = self.parse_expr();
        self.bare_question_forbidden = saved_flag;
        let span = span_between(start_span, self.previous_span());
        Expr {
            id: self.next_node_id(),
            kind: ExprKind::Lambda {
                params,
                body: Box::new(body),
            },
            span,
        }
    }

    fn parse_lambda_param(&mut self) -> LambdaParam {
        let start_span = self.current_span();
        let name = self.expect_ident("lambda parameter name");
        let ty = if matches!(self.peek_kind(), Some(TokenKind::Colon)) {
            self.bump();
            Some(self.parse_type_ann())
        } else {
            None
        };
        let span = span_between(start_span, self.previous_span());
        LambdaParam { name, ty, span }
    }

    /// `[` elem (',' elem)* ','? `]` (D-TYPE-02 allows a trailing comma; was_multiline is D-FMT-05).
    fn parse_list_lit(&mut self, start_span: Span) -> Expr {
        self.bump(); // '['
        let elements = self.parse_comma_separated(&TokenKind::RBracket, Self::parse_expr);
        let close_line = self.current_span().start.line;
        self.expect(&TokenKind::RBracket, "`]`");
        let was_multiline = start_span.start.line != close_line;
        let span = span_between(start_span, self.previous_span());
        Expr {
            id: self.next_node_id(),
            kind: ExprKind::ListLit {
                elements,
                was_multiline,
            },
            span,
        }
    }

    /// An expression starting with `{`: an empty `{}` is always an empty dict (D-TYPE-03).
    /// When non-empty, it is a dict if `:` follows right after the first element, or a set
    /// otherwise.
    fn parse_brace_lit(&mut self, start_span: Span) -> Expr {
        self.bump(); // '{'
        if matches!(self.peek_kind(), Some(TokenKind::RBrace)) {
            self.bump();
            let span = span_between(start_span, self.previous_span());
            return Expr {
                id: self.next_node_id(),
                kind: ExprKind::DictLit {
                    entries: Vec::new(),
                    was_multiline: false,
                },
                span,
            };
        }
        let first_key = self.parse_expr();
        if matches!(self.peek_kind(), Some(TokenKind::Colon)) {
            self.bump();
            let first_value = self.parse_expr();
            let mut entries = vec![(first_key, first_value)];
            entries.extend(self.parse_comma_separated_tail(&TokenKind::RBrace, |p| {
                let key = p.parse_expr();
                p.expect(&TokenKind::Colon, "`:`");
                let value = p.parse_expr();
                (key, value)
            }));
            let close_line = self.current_span().start.line;
            self.expect(&TokenKind::RBrace, "`}`");
            let was_multiline = start_span.start.line != close_line;
            let span = span_between(start_span, self.previous_span());
            Expr {
                id: self.next_node_id(),
                kind: ExprKind::DictLit {
                    entries,
                    was_multiline,
                },
                span,
            }
        } else {
            let mut elements = vec![first_key];
            elements.extend(self.parse_comma_separated_tail(&TokenKind::RBrace, Self::parse_expr));
            let close_line = self.current_span().start.line;
            self.expect(&TokenKind::RBrace, "`}`");
            let was_multiline = start_span.start.line != close_line;
            let span = span_between(start_span, self.previous_span());
            Expr {
                id: self.next_node_id(),
                kind: ExprKind::SetLit {
                    elements,
                    was_multiline,
                },
                span,
            }
        }
    }

    /// The arm list following `match scrutinee`. If `Newline` + `Indent` follows, this is
    /// parsed as the ordinary block form (arms separated by Newline, terminated by Dedent);
    /// otherwise (a bracket-suppressed context), it is parsed as a run of arms that each
    /// self-delimit at the start of a pattern (a decision made in this parser implementation
    /// that generalizes the same bracket-suppression handling as §5.6 to match's arm list
    /// too).
    fn parse_match_expr(&mut self, start_span: Span) -> Expr {
        self.bump(); // 'match'
        let scrutinee = Box::new(self.parse_expr());
        let mut arms = Vec::new();
        if self.is_indented_block_ahead() {
            self.bump(); // Newline
            self.bump(); // Indent
            loop {
                self.skip_blank_newlines();
                match self.peek_kind() {
                    Some(TokenKind::Dedent) => {
                        self.bump();
                        break;
                    }
                    None | Some(TokenKind::Eof) => break,
                    _ => arms.push(self.parse_match_arm()),
                }
            }
        } else {
            while self.at_pattern_start() {
                arms.push(self.parse_match_arm());
            }
        }
        let span = span_between(start_span, self.previous_span());
        Expr {
            id: self.next_node_id(),
            kind: ExprKind::Match { scrutinee, arms },
            span,
        }
    }

    /// `par [elem, ...]` / `par (elem, ...)`. Sets `bare_question_forbidden` while parsing
    /// each element (D-PAR-03).
    fn parse_par_expr(&mut self, start_span: Span) -> Expr {
        self.bump(); // 'par'
        let (kind, closing, closing_desc) = match self.peek_kind() {
            Some(TokenKind::LBracket) => (ParKind::List, TokenKind::RBracket, "`]`"),
            Some(TokenKind::LParen) => (ParKind::Tuple, TokenKind::RParen, "`)`"),
            _ => {
                self.push_diag(
                    ErrorCode::UnexpectedToken,
                    start_span,
                    "`par` must be followed by `[` or `(`",
                );
                return self.poison_expr(start_span);
            }
        };
        self.bump(); // '[' or '('
        let saved_flag = self.bare_question_forbidden;
        self.bare_question_forbidden = true;
        let elements = self.parse_comma_separated(&closing, Self::parse_expr);
        self.bare_question_forbidden = saved_flag;
        self.expect(&closing, closing_desc);
        let span = span_between(start_span, self.previous_span());
        Expr {
            id: self.next_node_id(),
            kind: ExprKind::Par { kind, elements },
            span,
        }
    }

    /// Converts an f-string's `Vec<FStringPart>` (lexical level) into a `Vec<FStringSegment>`
    /// (which carries a syntax tree) -- each `Expr` portion recursively calls `parse_expr`.
    /// Assumes the caller (`parse_primary`) has already confirmed the current token is
    /// `FString`.
    pub(crate) fn parse_fstring_segments(&mut self) -> Vec<FStringSegment> {
        let Some(parts) = self.current_fstring_parts() else {
            return Vec::new();
        };
        self.pos += 1; // consume the FString token itself
        let mut segments = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                crate::lexer::FStringPart::Text(text) => {
                    segments.push(FStringSegment::Text(text.clone()));
                }
                crate::lexer::FStringPart::Expr(sub_tokens) => {
                    let expr = self.parse_token_slice_as_expr(sub_tokens.as_slice());
                    segments.push(FStringSegment::Expr(Box::new(expr)));
                }
            }
        }
        segments
    }

    /// If the token at the current position is `FString`, returns its inner
    /// `Vec<FStringPart>` still borrowed with the same `'a` as `self.tokens`. The lifetime
    /// shrunk to `&self` by `peek` cannot be used for the later temporary swap of
    /// `self.tokens` (`parse_token_slice_as_expr`), so this dedicated accessor explicitly
    /// preserves `'a` (a decision made in this parser implementation).
    fn current_fstring_parts(&self) -> Option<&'a Vec<crate::lexer::FStringPart>> {
        match self.tokens.get(self.pos) {
            Some(Token {
                kind: TokenKind::FString(parts),
                ..
            }) => Some(parts),
            _ => None,
        }
    }

    /// Runs `parse_expr` on an f-string's `{expr}` portion (an already-recursively-lexed
    /// `&'a [Token]`, not including the terminal `Eof`), temporarily swapping out only
    /// `self.tokens`/`self.pos` while keeping the current parser state
    /// (`next_id`/`depth`/`diagnostics`/`bare_question_forbidden`) intact (the parser-side
    /// counterpart of §5.2's "by recursively invoking the same Lexer").
    fn parse_token_slice_as_expr(&mut self, tokens: &'a [Token]) -> Expr {
        let saved_tokens = self.tokens;
        let saved_pos = self.pos;
        self.tokens = tokens;
        self.pos = 0;
        let expr = self.parse_expr();
        if self.pos < self.tokens.len() {
            let span = self.current_span();
            self.push_diag(
                ErrorCode::UnexpectedToken,
                span,
                "there are extra tokens in the f-string's expression",
            );
        }
        self.tokens = saved_tokens;
        self.pos = saved_pos;
        expr
    }

    /// Assuming `(` has already been consumed, parses a comma-separated argument list,
    /// consumes `)`, and returns `(argument list, was_multiline)` (D-FMT-05: whether there
    /// was a newline between the opening and closing parens is decided from line numbers).
    fn parse_call_args(&mut self) -> (Vec<Arg>, bool) {
        let open_line = self.current_span().start.line;
        self.bump(); // '(' or '[' (the argument list right after a type-argument call is always '(')
        let args = self.parse_comma_separated(&TokenKind::RParen, Self::parse_call_arg);
        let close_line = self.current_span().start.line;
        self.expect(&TokenKind::RParen, "`)`");
        (args, open_line != close_line)
    }

    /// A single argument: a pipe's `_` placeholder / `name: value` (named) / an ordinary
    /// expression (positional).
    fn parse_call_arg(&mut self) -> Arg {
        if matches!(self.peek_kind(), Some(TokenKind::Underscore)) {
            let span = self.current_span();
            self.bump();
            return Arg {
                name: None,
                value: self.leaf_expr(ExprKind::Ident(Arc::from("_")), span),
                is_placeholder: true,
            };
        }
        if matches!(self.peek_kind(), Some(TokenKind::Ident(_)))
            && matches!(self.peek_kind_at(1), Some(TokenKind::Colon))
        {
            let name = self.expect_ident("named argument name");
            self.bump(); // ':'
            let value = self.parse_expr();
            return Arg {
                name: Some(name),
                value,
                is_placeholder: false,
            };
        }
        let value = self.parse_expr();
        Arg {
            name: None,
            value,
            is_placeholder: false,
        }
    }

    /// Determines whether the content right after `[` starts with a token that looks like a
    /// type annotation, and whether the matching `]` is immediately followed by `(` (resolves
    /// the ambiguity between an explicit-type-argument call and index access -- see
    /// `parse_postfix`'s documentation). Immediately false if the current position is not
    /// `[`.
    fn at_explicit_type_args(&self) -> bool {
        if !matches!(self.peek_kind(), Some(TokenKind::LBracket)) {
            return false;
        }
        if !matches!(
            self.peek_kind_at(1),
            Some(TokenKind::Ident(_) | TokenKind::LParen | TokenKind::Void)
        ) {
            return false;
        }
        self.matching_close_followed_by(&TokenKind::LParen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::PipeStage;
    use crate::diagnostics::{Diagnostic, FileId};
    use crate::lexer::Lexer;

    /// Parses `src` as a single expression (not wrapped in a surrounding `x = ...` statement
    /// just for statistics, since we want to verify the expression's own syntax tree
    /// directly). Confirms there are no errors in either lexing or parsing, and that tokens
    /// are consumed all the way to the end (through Newline/Eof).
    fn parse_one_expr(src: &str) -> Expr {
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "a lexing error occurred: {src:?}");
        let mut parser = Parser::new(&tokens, file);
        let expr = parser.parse_expr();
        assert!(
            parser.diagnostics.is_empty(),
            "a parsing error occurred: {src:?}"
        );
        assert!(
            matches!(
                parser.peek_kind(),
                Some(TokenKind::Newline | TokenKind::Eof)
            ),
            "extra tokens remain after the expression (not fully consumed at the expected precedence): {src:?}"
        );
        expr
    }

    /// For when only the presence/absence of diagnostics matters (the shape of the syntax
    /// tree does not). Returns diagnostics from both the lexing and parsing phases, combined
    /// and sorted ascending by `file:line:col`.
    fn parse_diagnostics(src: &str) -> Vec<Diagnostic> {
        use crate::diagnostics::SourceMap;
        let mut sources = SourceMap::new();
        let file = sources.add(std::path::PathBuf::from("test.ybm"), src.to_owned());
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        let (_module, parse_diag) = crate::parser::parse_module(&tokens, file);
        let mut combined = lex_diag.into_sorted(&sources);
        combined.extend(parse_diag.into_sorted(&sources));
        combined
    }

    fn op_name(op: BinaryOp) -> &'static str {
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

    fn as_binary(e: &Expr) -> (&'static str, &Expr, &Expr) {
        match &e.kind {
            ExprKind::Binary { op, lhs, rhs } => (op_name(*op), lhs, rhs),
            _ => panic!("expected a Binary expression"),
        }
    }

    fn as_unary(e: &Expr) -> (&'static str, &Expr) {
        match &e.kind {
            ExprKind::Unary { op, operand } => {
                let name = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not",
                };
                (name, operand)
            }
            _ => panic!("expected a Unary expression"),
        }
    }

    fn is_int(e: &Expr, expected: i64) -> bool {
        matches!(e.kind, ExprKind::IntLit(n) if n == expected)
    }

    fn is_ident(e: &Expr, expected: &str) -> bool {
        matches!(&e.kind, ExprKind::Ident(n) if n.as_ref() == expected)
    }

    fn as_field_access(e: &Expr) -> (&Expr, &str) {
        match &e.kind {
            ExprKind::FieldAccess { target, field } => (target, field.as_ref()),
            _ => panic!("expected a FieldAccess expression"),
        }
    }

    fn as_index(e: &Expr) -> (&Expr, &Expr) {
        match &e.kind {
            ExprKind::Index { target, index } => (target, index),
            _ => panic!("expected an Index expression"),
        }
    }

    fn as_question(e: &Expr) -> &Expr {
        match &e.kind {
            ExprKind::Question { target } => target,
            _ => panic!("expected a Question expression"),
        }
    }

    fn as_call(e: &Expr) -> (&Expr, usize) {
        match &e.kind {
            ExprKind::Call { callee, args, .. } => (callee, args.len()),
            _ => panic!("expected a Call expression"),
        }
    }

    fn as_pipe(e: &Expr) -> (&Expr, &[PipeStage]) {
        match &e.kind {
            ExprKind::Pipe(p) => (&p.source, p.stages.as_slice()),
            _ => panic!("expected a Pipe expression"),
        }
    }

    fn pipe_stage_bare_name(stage: &PipeStage) -> &str {
        match &stage.callee {
            PipeCallee::Bare(e) => match &e.kind {
                ExprKind::Ident(n) => n.as_ref(),
                _ => panic!("expected a bare-name pipe destination"),
            },
            PipeCallee::WithArgs { .. } => panic!("expected Bare but got WithArgs"),
        }
    }

    // ------------------------------------------------------------------
    // Verifies the D-OP-01 precedence table (9 tiers) in a table-driven way. Each test
    // confirms that one row of the table, or the relative relationship between two adjacent
    // rows, is actually reflected in the syntax tree (at least 20 expressions, per the
    // assigned unit's instructions).
    // ------------------------------------------------------------------

    #[test]
    fn row1_postfix_field_after_call_left_to_right() {
        // f(x).y is (f(x)).y (call then field access, left-associative postfix)
        let e = parse_one_expr("f(x).y\n");
        let (target, field) = as_field_access(&e);
        assert_eq!(field, "y");
        let (_callee, argc) = as_call(target);
        assert_eq!(argc, 1);
    }

    #[test]
    fn row1_postfix_index_left_to_right() {
        // a[0][1] is (a[0])[1]
        let e = parse_one_expr("a[0][1]\n");
        let (target, index) = as_index(&e);
        assert!(is_int(index, 1));
        let (inner_target, inner_index) = as_index(target);
        assert!(is_ident(inner_target, "a"));
        assert!(is_int(inner_index, 0));
    }

    #[test]
    fn d_op_02_postfix_question_then_dot() {
        // f(x)?.y is (f(x)?).y (D-OP-02: ? is at the same tier as .[]() and left-associative)
        let e = parse_one_expr("f(x)?.y\n");
        let (target, field) = as_field_access(&e);
        assert_eq!(field, "y");
        let questioned = as_question(target);
        let (_callee, argc) = as_call(questioned);
        assert_eq!(argc, 1);
    }

    #[test]
    fn row2_unary_neg_binds_tighter_than_postfix_operand_is_evaluated_first() {
        // -a[0] is -(a[0]) (unary minus acts on the expression after postfix has already been applied)
        let e = parse_one_expr("-a[0]\n");
        let (op, operand) = as_unary(&e);
        assert_eq!(op, "-");
        let (target, index) = as_index(operand);
        assert!(is_ident(target, "a"));
        assert!(is_int(index, 0));
    }

    #[test]
    fn row2_unary_neg_binds_tighter_than_row3_mul() {
        // -a * b is (-a) * b (unary minus binds tighter than *)
        let e = parse_one_expr("-a * b\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "*");
        assert!(is_ident(rhs, "b"));
        let (uop, uoperand) = as_unary(lhs);
        assert_eq!(uop, "-");
        assert!(is_ident(uoperand, "a"));
    }

    #[test]
    fn row2_unary_not_binds_tighter_than_row5_comparison_risk_high_case() {
        // not 1 > 2 is (not 1) > 2 (exactly the D-OP-01 risk:high ruling)
        let e = parse_one_expr("not 1 > 2\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, ">");
        assert!(is_int(rhs, 2));
        let (uop, uoperand) = as_unary(lhs);
        assert_eq!(uop, "not");
        assert!(is_int(uoperand, 1));
    }

    #[test]
    fn row2_unary_right_recursion_double_negation() {
        // - -a is -(-(a)) (unary right-recursion)
        let e = parse_one_expr("- -a\n");
        let (op1, inner1) = as_unary(&e);
        assert_eq!(op1, "-");
        let (op2, inner2) = as_unary(inner1);
        assert_eq!(op2, "-");
        assert!(is_ident(inner2, "a"));
    }

    #[test]
    fn row3_mul_binds_tighter_than_row4_add_left_operand() {
        // 1 + 2 * 3 is 1 + (2 * 3)
        let e = parse_one_expr("1 + 2 * 3\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "+");
        assert!(is_int(lhs, 1));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(rhs);
        assert_eq!(inner_op, "*");
        assert!(is_int(inner_lhs, 2));
        assert!(is_int(inner_rhs, 3));
    }

    #[test]
    fn row3_mul_binds_tighter_than_row4_add_right_operand() {
        // 1 * 2 + 3 is (1 * 2) + 3
        let e = parse_one_expr("1 * 2 + 3\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "+");
        assert!(is_int(rhs, 3));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "*");
        assert!(is_int(inner_lhs, 1));
        assert!(is_int(inner_rhs, 2));
    }

    #[test]
    fn row3_div_is_left_associative() {
        // 10 / 2 / 5 is (10 / 2) / 5 (if right-associative it would be 10/(2/5), changing the value)
        let e = parse_one_expr("10 / 2 / 5\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "/");
        assert!(is_int(rhs, 5));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "/");
        assert!(is_int(inner_lhs, 10));
        assert!(is_int(inner_rhs, 2));
    }

    #[test]
    fn row4_sub_is_left_associative() {
        // 1 - 2 - 3 is (1 - 2) - 3
        let e = parse_one_expr("1 - 2 - 3\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "-");
        assert!(is_int(rhs, 3));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "-");
        assert!(is_int(inner_lhs, 1));
        assert!(is_int(inner_rhs, 2));
    }

    #[test]
    fn row4_add_binds_tighter_than_row5_comparison() {
        // 1 + 2 < 4 is (1 + 2) < 4
        let e = parse_one_expr("1 + 2 < 4\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "<");
        assert!(is_int(rhs, 4));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "+");
        assert!(is_int(inner_lhs, 1));
        assert!(is_int(inner_rhs, 2));
    }

    #[test]
    fn row5_chained_comparison_is_rejected() {
        // a < b < c cannot be written (D-OP-01). Since the second `<` is not consumed and is
        // left over, confirm that parsing the whole statement produces E0502 for the extra
        // token.
        let diags = parse_diagnostics("x = a < b < c\n");
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "a chained comparison should produce E0502: {diags:?}"
        );
    }

    #[test]
    fn row5_comparison_binds_tighter_than_row6_equality() {
        // 1 < 2 == true is (1 < 2) == true
        let e = parse_one_expr("1 < 2 == true\n");
        let (op, lhs, _rhs) = as_binary(&e);
        assert_eq!(op, "==");
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "<");
        assert!(is_int(inner_lhs, 1));
        assert!(is_int(inner_rhs, 2));
    }

    #[test]
    fn row6_equality_is_left_associative_chain_allowed() {
        // a == b != c is (a == b) != c (equality is not subject to the no-chaining rule, left-associative)
        let e = parse_one_expr("a == b != c\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "!=");
        assert!(is_ident(rhs, "c"));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "==");
        assert!(is_ident(inner_lhs, "a"));
        assert!(is_ident(inner_rhs, "b"));
    }

    #[test]
    fn row6_equality_binds_tighter_than_row7_and() {
        // 1 == 1 and 2 == 2 is (1 == 1) and (2 == 2)
        let e = parse_one_expr("1 == 1 and 2 == 2\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "and");
        let (lop, _, _) = as_binary(lhs);
        assert_eq!(lop, "==");
        let (rop, _, _) = as_binary(rhs);
        assert_eq!(rop, "==");
    }

    #[test]
    fn row7_and_is_left_associative() {
        // a and b and c is (a and b) and c
        let e = parse_one_expr("a and b and c\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "and");
        assert!(is_ident(rhs, "c"));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "and");
        assert!(is_ident(inner_lhs, "a"));
        assert!(is_ident(inner_rhs, "b"));
    }

    #[test]
    fn row7_and_binds_tighter_than_row8_or() {
        // a and b or c is (a and b) or c
        let e = parse_one_expr("a and b or c\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "or");
        assert!(is_ident(rhs, "c"));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "and");
        assert!(is_ident(inner_lhs, "a"));
        assert!(is_ident(inner_rhs, "b"));
    }

    #[test]
    fn row8_or_is_left_associative() {
        // a or b or c is (a or b) or c
        let e = parse_one_expr("a or b or c\n");
        let (op, lhs, rhs) = as_binary(&e);
        assert_eq!(op, "or");
        assert!(is_ident(rhs, "c"));
        let (inner_op, inner_lhs, inner_rhs) = as_binary(lhs);
        assert_eq!(inner_op, "or");
        assert!(is_ident(inner_lhs, "a"));
        assert!(is_ident(inner_rhs, "b"));
    }

    #[test]
    fn row8_or_binds_tighter_than_row9_pipe() {
        // a or b |> f is (a or b) |> f (pipe has the loosest precedence)
        let e = parse_one_expr("a or b |> f\n");
        let (source, stages) = as_pipe(&e);
        assert_eq!(stages.len(), 1);
        assert_eq!(pipe_stage_bare_name(&stages[0]), "f");
        let (op, lhs, rhs) = as_binary(source);
        assert_eq!(op, "or");
        assert!(is_ident(lhs, "a"));
        assert!(is_ident(rhs, "b"));
    }

    #[test]
    fn row9_pipe_add_binds_before_pipe_spec_15_example() {
        // 2 + 3 |> triple is (2 + 3) |> triple (an example of the same kind as SPEC §15)
        let e = parse_one_expr("2 + 3 |> triple\n");
        let (source, stages) = as_pipe(&e);
        assert_eq!(stages.len(), 1);
        assert_eq!(pipe_stage_bare_name(&stages[0]), "triple");
        let (op, lhs, rhs) = as_binary(source);
        assert_eq!(op, "+");
        assert!(is_int(lhs, 2));
        assert!(is_int(rhs, 3));
    }

    #[test]
    fn row9_pipe_is_left_associative_single_node_multi_stage() {
        // x |> f |> g is one Pipe node with 2 stages (does not become a nested Pipe)
        let e = parse_one_expr("x |> f |> g\n");
        let (source, stages) = as_pipe(&e);
        assert!(is_ident(source, "x"));
        assert_eq!(stages.len(), 2);
        assert_eq!(pipe_stage_bare_name(&stages[0]), "f");
        assert_eq!(pipe_stage_bare_name(&stages[1]), "g");
    }

    #[test]
    fn pipe_stage_postfix_question_is_a_stage_flag_not_a_question_expr() {
        // x |> f? is the stage's question flag, and does not become ExprKind::Question
        let e = parse_one_expr("x |> f?\n");
        let (_source, stages) = as_pipe(&e);
        assert_eq!(stages.len(), 1);
        assert!(stages[0].question);
        assert_eq!(pipe_stage_bare_name(&stages[0]), "f");
    }
}
