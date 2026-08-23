//! Type annotation parser (ARCHITECTURE.md §2.1/§3.6).

use super::{Parser, span_between};
use crate::ast::TypeAnn;
use crate::ast::TypeAnnKind;
use crate::diagnostics::Span;
use crate::lexer::TokenKind;
use std::sync::Arc;

impl Parser<'_> {
    /// Parses one of `int` / `list[T]` / `User[Args...]` / `(int) -> str uses {net}` /
    /// `void` / `tuple[A, B]`.
    ///
    /// Applies the R4 decision (§5.11) here too (a decision made in this parser
    /// implementation): recursion in type annotations (nesting such as
    /// `list[list[list[...]]]`) is a recursion path independent from `parser/expr.rs`'s
    /// expression recursion (`parse_function_type_ann`/`parse_named_or_tuple_type_ann`/
    /// `parse_type_arg_list` all eventually call this `parse_type_ann` recursively), so
    /// `parse_expr`'s depth guard alone cannot protect this path. As with expressions,
    /// hanging `depth_enter`/`depth_exit` on this single function is enough to protect every
    /// recursion path on the type-annotation side (when the caller is already inside
    /// expression-side nesting, the same `self.depth` counter is shared and simply
    /// incremented further, so it keeps a 1-to-1 correspondence with Rust call-stack
    /// depth).
    pub(crate) fn parse_type_ann(&mut self) -> TypeAnn {
        let start_span = self.current_span();
        if !self.depth_enter(start_span) {
            return TypeAnn {
                kind: TypeAnnKind::Named {
                    name: Arc::from(""),
                    args: Vec::new(),
                },
                span: start_span,
            };
        }
        let result = match self.peek_kind() {
            Some(TokenKind::Void) => {
                self.bump();
                TypeAnn {
                    kind: TypeAnnKind::Void,
                    span: start_span,
                }
            }
            Some(TokenKind::LParen) => self.parse_function_type_ann(start_span),
            Some(TokenKind::Ident(_)) => self.parse_named_or_tuple_type_ann(start_span),
            _ => {
                self.push_diag(
                    crate::diagnostics::ErrorCode::UnexpectedToken,
                    start_span,
                    "expected a type annotation but did not find one",
                );
                TypeAnn {
                    kind: TypeAnnKind::Named {
                        name: Arc::from(""),
                        args: Vec::new(),
                    },
                    span: start_span,
                }
            }
        };
        self.depth_exit();
        result
    }

    /// `(T1, T2, ...) -> Ret (uses {..})?`. D-LEX-01's function type notation (`->` in a type context).
    fn parse_function_type_ann(&mut self, start_span: Span) -> TypeAnn {
        self.bump(); // '('
        let params = self.parse_comma_separated(&TokenKind::RParen, Self::parse_type_ann);
        self.expect(&TokenKind::RParen, "`)`");
        self.expect(&TokenKind::Arrow, "`->`");
        let ret = self.parse_type_ann();
        let effects = self.parse_uses_clause();
        let end_span = self.previous_span();
        TypeAnn {
            kind: TypeAnnKind::Function {
                params,
                effects,
                ret: Box::new(ret),
            },
            span: span_between(start_span, end_span),
        }
    }

    /// A type annotation that starts with a bare identifier: `int`/`User`/`list[int]`/
    /// `Result[T,E]`/`tuple[A, B]`. Built as `TypeAnnKind::Tuple` (the dedicated variant, see
    /// ast/ty_ann.rs) only when the name is `tuple`; everything else is built as
    /// `Named{name, args}` (matching the design of the type definition itself on the ast
    /// side: D-TYPE-09's principle is applied to list/dict/set etc. too, while only tuple has
    /// a dedicated representation that plainly expresses a variable element count).
    fn parse_named_or_tuple_type_ann(&mut self, start_span: Span) -> TypeAnn {
        let name = self.expect_ident("type name");
        let args = if self.peek_kind() == Some(&TokenKind::LBracket) {
            self.parse_type_arg_list()
        } else {
            Vec::new()
        };
        let end_span = self.previous_span();
        let span = span_between(start_span, end_span);
        if name.as_ref() == "tuple" {
            TypeAnn {
                kind: TypeAnnKind::Tuple(args),
                span,
            }
        } else {
            TypeAnn {
                kind: TypeAnnKind::Named { name, args },
                span,
            }
        }
    }

    /// `[` T1, T2, ... `]` (current position is `[`). Shared by both generic type arguments
    /// and an explicit-type-argument call `f[Type](...)`.
    pub(crate) fn parse_type_arg_list(&mut self) -> Vec<TypeAnn> {
        self.bump(); // '['
        let args = self.parse_comma_separated(&TokenKind::RBracket, Self::parse_type_ann);
        self.expect(&TokenKind::RBracket, "`]`");
        args
    }

    /// `uses { name, name, ... }`. Returns an empty Vec if absent (consumes no token).
    /// Shared by both a function declaration's own `uses` clause (decl.rs) and the `uses`
    /// clause inside a function type annotation (this file).
    pub(crate) fn parse_uses_clause(&mut self) -> Vec<Arc<str>> {
        if self.peek_kind() != Some(&TokenKind::Uses) {
            return Vec::new();
        }
        self.bump(); // 'uses'
        self.expect(&TokenKind::LBrace, "`{`");
        let effects =
            self.parse_comma_separated(&TokenKind::RBrace, |p| p.expect_ident("effect name"));
        self.expect(&TokenKind::RBrace, "`}`");
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Item, Stmt, StmtKind, TypeAnnKind};
    use crate::diagnostics::FileId;
    use crate::lexer::Lexer;
    use std::path::{Path, PathBuf};

    fn sample_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    /// Parses `src` (one type annotation, including its trailing newline) on its own.
    /// Verifies there are no lexical or syntax errors and that parsing has stopped right
    /// after the type annotation (consumed everything up to just before a Newline/Eof).
    fn parse_one_type_ann(src: &str) -> TypeAnn {
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "a lexer error occurred: {src:?}");
        let mut parser = Parser::new(&tokens, file);
        let ty = parser.parse_type_ann();
        assert!(
            parser.diagnostics.is_empty(),
            "a type annotation parse error occurred: {src:?}, diags={:?}",
            parser.diagnostics
        );
        assert!(
            matches!(
                parser.peek_kind(),
                Some(TokenKind::Newline | TokenKind::Eof)
            ),
            "leftover tokens remain after the type annotation: {src:?}"
        );
        ty
    }

    fn as_named(ty: &TypeAnn) -> (&str, &[TypeAnn]) {
        match &ty.kind {
            TypeAnnKind::Named { name, args } => (name.as_ref(), args.as_slice()),
            other => panic!("expected a Named type annotation but got something else: {other:?}"),
        }
    }

    fn as_tuple(ty: &TypeAnn) -> &[TypeAnn] {
        match &ty.kind {
            TypeAnnKind::Tuple(elems) => elems.as_slice(),
            other => panic!("expected a Tuple type annotation but got something else: {other:?}"),
        }
    }

    fn as_function(ty: &TypeAnn) -> (&[TypeAnn], &[Arc<str>], &TypeAnn) {
        match &ty.kind {
            TypeAnnKind::Function {
                params,
                effects,
                ret,
            } => (params.as_slice(), effects.as_slice(), ret.as_ref()),
            other => {
                panic!("expected a Function type annotation but got something else: {other:?}")
            }
        }
    }

    #[test]
    fn void_type_ann_parses_as_void_kind() {
        let ty = parse_one_type_ann("void\n");
        assert!(matches!(ty.kind, TypeAnnKind::Void));
    }

    #[test]
    fn bare_primitive_named_type_ann_has_no_args() {
        // Per D-LEX-01, int/float/bool/str are not reserved words but ordinary Ident
        // tokens; as a type annotation they become Named{name, args: []} with no special
        // treatment.
        let ty = parse_one_type_ann("int\n");
        let (name, args) = as_named(&ty);
        assert_eq!(name, "int");
        assert!(args.is_empty());
    }

    #[test]
    fn generic_list_type_ann_carries_one_type_arg() {
        let ty = parse_one_type_ann("list[int]\n");
        let (name, args) = as_named(&ty);
        assert_eq!(name, "list");
        assert_eq!(args.len(), 1);
        let (inner_name, inner_args) = as_named(&args[0]);
        assert_eq!(inner_name, "int");
        assert!(inner_args.is_empty());
    }

    #[test]
    fn dict_type_ann_carries_key_and_value_type_args_in_order() {
        let ty = parse_one_type_ann("dict[str, int]\n");
        let (name, args) = as_named(&ty);
        assert_eq!(name, "dict");
        assert_eq!(args.len(), 2);
        assert_eq!(as_named(&args[0]).0, "str");
        assert_eq!(as_named(&args[1]).0, "int");
    }

    /// Parses, on its own, the very `list[list[int]]` from
    /// samples/ok/3-4_type_annotations_and_inference that is the source example for
    /// D-TYPE-16's "recursive propagation into nested collections", and verifies it becomes
    /// two nested levels of Named.
    #[test]
    fn nested_list_of_list_type_ann_from_sample_nests_two_named_levels() {
        let ty = parse_one_type_ann("list[list[int]]\n");
        let (outer_name, outer_args) = as_named(&ty);
        assert_eq!(outer_name, "list");
        assert_eq!(outer_args.len(), 1);
        let (mid_name, mid_args) = as_named(&outer_args[0]);
        assert_eq!(mid_name, "list");
        assert_eq!(mid_args.len(), 1);
        assert_eq!(as_named(&mid_args[0]).0, "int");
    }

    /// Only the name `tuple` is, by `ty_ann.rs`'s design, built as the dedicated
    /// `TypeAnnKind::Tuple` variant rather than `Named{name: "tuple", ..}` (see the comment
    /// at the top of this file).
    #[test]
    fn tuple_type_ann_uses_dedicated_tuple_kind_not_named() {
        let ty = parse_one_type_ann("tuple[int, str]\n");
        let elems = as_tuple(&ty);
        assert_eq!(elems.len(), 2);
        assert_eq!(as_named(&elems[0]).0, "int");
        assert_eq!(as_named(&elems[1]).0, "str");
    }

    /// Verifies the zero-parameter function type `() -> T` used by
    /// `def combine_two[T, U](f: () -> T, g: () -> U): tuple[T, U]` in
    /// samples/ok/8_effects/entry_transitive_and_hof_effects.ybm (D-LEX-01, "function type
    /// notation is `->` in a type context").
    #[test]
    fn zero_param_function_type_ann_from_sample_has_empty_params() {
        let ty = parse_one_type_ann("() -> T\n");
        let (params, effects, ret) = as_function(&ty);
        assert!(params.is_empty());
        assert!(effects.is_empty());
        assert_eq!(as_named(ret).0, "T");
    }

    /// A function type annotation with `uses {..}` (SPEC §5, "effects are declared with
    /// uses"; notation in a type context). Verifies a sequence of multiple effects lands in
    /// `effects` with its order preserved as-is.
    #[test]
    fn function_type_ann_with_multiple_effects_preserves_order() {
        let ty = parse_one_type_ann("(str) -> Result[int, Error] uses {net, fs}\n");
        let (params, effects, ret) = as_function(&ty);
        assert_eq!(params.len(), 1);
        assert_eq!(as_named(&params[0]).0, "str");
        assert_eq!(
            effects.iter().map(Arc::as_ref).collect::<Vec<_>>(),
            vec!["net", "fs"]
        );
        let (ret_name, ret_args) = as_named(ret);
        assert_eq!(ret_name, "Result");
        assert_eq!(ret_args.len(), 2);
        assert_eq!(as_named(&ret_args[0]).0, "int");
        assert_eq!(as_named(&ret_args[1]).0, "Error");
    }

    /// Verification driven by an actual file under samples/ (per the owning unit's
    /// instructions): parses `xs: list[list[int]] = [[], [1, 2]]` -- the example for
    /// D-TYPE-16 context (1), "a variable declaration's initializer" -- not as a standalone
    /// type annotation but through the real `parse_stmt` path (NameAssign), and confirms
    /// `xs`'s ty field is two nested levels of Named.
    #[test]
    fn sample_file_var_decl_type_ann_nested_list_parses_via_real_stmt_path() {
        let path = sample_path("samples/ok/3-4_type_annotations_and_inference/entry_main.ybm");
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => panic!("failed to read sample file {}: {e}", path.display()),
        };
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(&src, file).tokenize();
        assert!(lex_diag.is_empty(), "lexer error: {lex_diag:?}");
        let (module, parse_diag) = crate::parser::parse_module(&tokens, file);
        assert!(parse_diag.is_empty(), "parse error: {parse_diag:?}");

        let xs_stmt: &Stmt = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Stmt(stmt) => match &stmt.kind {
                    StmtKind::NameAssign { name, .. } if name.as_ref() == "xs" => Some(stmt),
                    _ => None,
                },
                Item::Decl(_) => None,
            })
            .unwrap_or_else(|| panic!("could not find a NameAssign for `xs`"));
        let StmtKind::NameAssign { ty, .. } = &xs_stmt.kind else {
            unreachable!("find_map's condition already confirmed this is a NameAssign")
        };
        let ty = ty
            .as_ref()
            .unwrap_or_else(|| panic!("`xs` should have the type annotation `list[list[int]]`"));
        let (outer_name, outer_args) = as_named(ty);
        assert_eq!(outer_name, "list");
        assert_eq!(outer_args.len(), 1);
        let (inner_name, inner_args) = as_named(&outer_args[0]);
        assert_eq!(inner_name, "list");
        assert_eq!(inner_args.len(), 1);
        assert_eq!(as_named(&inner_args[0]).0, "int");
    }

    /// Verifies that the R4 decision (§5.11) has also been applied to recursion in type
    /// annotations: just like the deep nesting on the `parse_expr` side (of the same shape
    /// as mod.rs's `deeply_nested_parens_...`), verifies that deep nesting of
    /// `list[list[...]]` also reports E0502 and finishes without panicking (before this test
    /// was added, `parse_type_ann` had no depth guard and this was a path that could crash
    /// with a native stack overflow -- an item fixed during this review).
    #[test]
    fn deeply_nested_list_type_ann_exceeds_max_parse_depth_and_reports_e0502() {
        let depth = 3_000;
        let mut src = String::from("xs: ");
        for _ in 0..depth {
            src.push_str("list[");
        }
        src.push_str("int");
        for _ in 0..depth {
            src.push(']');
        }
        src.push_str(" = []\n");

        let builder = std::thread::Builder::new().stack_size(64 * 1024 * 1024);
        let spawned = builder.spawn(move || {
            let file = FileId(0);
            let (tokens, _comments, lex_diag) = Lexer::new(&src, file).tokenize();
            assert!(lex_diag.is_empty(), "a lexer error occurred (unexpected)");
            let (_module, parse_diag) = crate::parser::parse_module(&tokens, file);
            parse_diag.into_vec()
        });
        let handle = match spawned {
            Ok(h) => h,
            Err(e) => panic!("failed to spawn the test thread: {e}"),
        };
        let Ok(diags) = handle.join() else {
            panic!("the test thread panicked (an unexpected stack overflow or similar)");
        };
        assert!(
            diags
                .iter()
                .any(|d| d.code == crate::diagnostics::ErrorCode::UnexpectedToken),
            "exceeding MAX_PARSE_DEPTH on the type-annotation side should also report E0502: diagnostic count={}",
            diags.len()
        );
    }
}
