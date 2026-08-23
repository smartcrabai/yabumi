//! Parser for def/struct/enum declarations (ARCHITECTURE.md §2.1). A module-level constant
//! is an ordinary `Stmt` (`StmtKind::NameAssign`) produced by `parse_stmt`, and has no
//! dedicated parse function of its own (the DOC-COMMENT-MISSING-ON-STMT-LEVEL-CONST
//! decision, §8).

use super::{Parser, span_between};
use crate::ast::{
    Decl, EnumDecl, EnumVariant, FunctionDecl, Item, Param, SelfParam, StructDecl, TypeAnn,
    TypeAnnKind,
};
use crate::diagnostics::ErrorCode;
use crate::lexer::TokenKind;
use std::sync::Arc;

impl Parser<'_> {
    /// Enumerates Items (Decl|Stmt) from the start of the file. If a DocComment (`##`) is
    /// attached immediately before, it is assigned as the `doc_comment` of
    /// `Decl::Function/Struct/Enum`, or of a `Stmt` (NameAssign) (the actual assignment is
    /// done by `attach_comments` in a separate pass, §5.9 -- this parser itself never looks
    /// at comments).
    pub(crate) fn parse_items(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            self.skip_blank_newlines();
            match self.peek_kind() {
                // Reaching a Dedent at the top level only happens in an abnormal case after
                // lexical-error recovery, but this stops conservatively to avoid an infinite
                // loop.
                None | Some(TokenKind::Eof | TokenKind::Dedent) => break,
                Some(TokenKind::Def) => {
                    let decl = self.parse_function_decl();
                    items.push(Item::Decl(Decl::Function(decl)));
                }
                Some(TokenKind::Struct) => {
                    let decl = self.parse_struct_decl();
                    items.push(Item::Decl(Decl::Struct(decl)));
                }
                Some(TokenKind::Enum) => {
                    let decl = self.parse_enum_decl();
                    items.push(Item::Decl(Decl::Enum(decl)));
                }
                _ => {
                    let stmt = self.parse_stmt();
                    items.push(Item::Stmt(stmt));
                }
            }
        }
        items
    }

    /// `def name[generics](params): ret uses {..} \n Block`. Whether `self`/`var self` is
    /// present determines `self_param` (D-MUT-01).
    pub(crate) fn parse_function_decl(&mut self) -> FunctionDecl {
        let start_span = self.current_span();
        self.bump(); // 'def'
        let name = self.expect_ident("function name");
        let generics = self.parse_optional_generics();
        self.expect(&TokenKind::LParen, "`(`");
        let (self_param, params) = self.parse_params();
        self.expect(&TokenKind::RParen, "`)`");
        self.expect(&TokenKind::Colon, "`:` (return type annotation)");
        let ret = self.parse_type_ann();
        let effects = self.parse_uses_clause();
        let body = self.parse_block();
        let end_span = self.previous_span();
        FunctionDecl {
            id: self.next_node_id(),
            name,
            generics,
            self_param,
            params,
            ret,
            effects,
            body,
            leading_comments: Vec::new(),
            doc_comment: None,
            span: span_between(start_span, end_span),
        }
    }

    /// `struct Name[generics] \n (field: ty)* (def ...)*`.
    pub(crate) fn parse_struct_decl(&mut self) -> StructDecl {
        let start_span = self.current_span();
        self.bump(); // 'struct'
        let name = self.expect_ident("struct name");
        let generics = self.parse_optional_generics();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        self.enter_body_indent_if_present(|p| {
            loop {
                p.skip_blank_newlines();
                match p.peek_kind() {
                    Some(TokenKind::Dedent) => {
                        p.bump();
                        break;
                    }
                    None | Some(TokenKind::Eof) => break,
                    Some(TokenKind::Def) => methods.push(p.parse_function_decl()),
                    Some(TokenKind::Ident(_)) => fields.push(p.parse_param()),
                    _ => {
                        let span = p.current_span();
                        p.push_diag(
                            ErrorCode::UnexpectedToken,
                            span,
                            "expected a struct field or method definition",
                        );
                        p.skip_to_sync_point();
                    }
                }
            }
        });
        let end_span = self.previous_span();
        let field_count = fields.len();
        StructDecl {
            id: self.next_node_id(),
            name,
            generics,
            fields,
            field_leading_comments: (0..field_count).map(|_| Vec::new()).collect(),
            field_trailing_comments: vec![None; field_count],
            methods,
            leading_comments: Vec::new(),
            doc_comment: None,
            span: span_between(start_span, end_span),
        }
    }

    /// `enum Name[generics] \n (Variant(fields)? | UnitVariant)*`.
    pub(crate) fn parse_enum_decl(&mut self) -> EnumDecl {
        let start_span = self.current_span();
        self.bump(); // 'enum'
        let name = self.expect_ident("enum name");
        let generics = self.parse_optional_generics();
        let mut variants = Vec::new();
        self.enter_body_indent_if_present(|p| {
            loop {
                p.skip_blank_newlines();
                match p.peek_kind() {
                    Some(TokenKind::Dedent) => {
                        p.bump();
                        break;
                    }
                    None | Some(TokenKind::Eof) => break,
                    Some(TokenKind::Ident(_)) => variants.push(p.parse_enum_variant()),
                    _ => {
                        let span = p.current_span();
                        p.push_diag(ErrorCode::UnexpectedToken, span, "expected an enum variant");
                        p.skip_to_sync_point();
                    }
                }
            }
        });
        let end_span = self.previous_span();
        EnumDecl {
            id: self.next_node_id(),
            name,
            generics,
            variants,
            leading_comments: Vec::new(),
            doc_comment: None,
            span: span_between(start_span, end_span),
        }
    }

    /// Consumes the struct/enum declaration body's `Newline Indent ... Dedent`, if present,
    /// before calling `body` (does nothing in the degenerate case where Indent is missing
    /// after D-SYN-01 lexical-error recovery -- the same lenient policy as parse_block, to
    /// avoid piling on an extra secondary diagnostic).
    fn enter_body_indent_if_present(&mut self, body: impl FnOnce(&mut Self)) {
        if matches!(self.peek_kind(), Some(TokenKind::Newline)) {
            self.bump();
        }
        if matches!(self.peek_kind(), Some(TokenKind::Indent)) {
            self.bump();
            body(self);
        }
    }

    /// `[T, U]` (type parameter names only, no constraint syntax). An empty Vec if absent.
    fn parse_optional_generics(&mut self) -> Vec<Arc<str>> {
        if self.peek_kind() != Some(&TokenKind::LBracket) {
            return Vec::new();
        }
        self.bump();
        let names = self.parse_comma_separated(&TokenKind::RBracket, |p| {
            p.expect_ident("type parameter name")
        });
        self.expect(&TokenKind::RBracket, "`]`");
        names
    }

    /// Assumes `(` has already been consumed, and parses `self`/`var self` (only at the
    /// front) plus the ordinary parameter list. Does not consume `)` (the caller does that
    /// via `expect`).
    fn parse_params(&mut self) -> (Option<SelfParam>, Vec<Param>) {
        let mut self_param = None;
        let mut params = Vec::new();
        if self.peek_kind() == Some(&TokenKind::RParen) {
            return (self_param, params);
        }
        if matches!(self.peek_kind(), Some(TokenKind::Var))
            && matches!(self.peek_kind_at(1), Some(TokenKind::KwSelf))
        {
            let sp_span = self.current_span();
            self.bump(); // var
            self.bump(); // self
            self_param = Some(SelfParam {
                mutable: true,
                span: sp_span,
            });
            if matches!(self.peek_kind(), Some(TokenKind::Comma)) {
                self.bump();
            }
        } else if matches!(self.peek_kind(), Some(TokenKind::KwSelf)) {
            let sp_span = self.current_span();
            self.bump();
            self_param = Some(SelfParam {
                mutable: false,
                span: sp_span,
            });
            if matches!(self.peek_kind(), Some(TokenKind::Comma)) {
                self.bump();
            }
        }
        while !matches!(
            self.peek_kind(),
            None | Some(TokenKind::Eof | TokenKind::RParen)
        ) {
            params.push(self.parse_param());
            if matches!(self.peek_kind(), Some(TokenKind::Comma)) {
                self.bump();
                if self.peek_kind() == Some(&TokenKind::RParen) {
                    break;
                }
            } else {
                break;
            }
        }
        (self_param, params)
    }

    /// `name: TypeAnn` (reused by both function arguments and struct fields). The type
    /// annotation itself (what follows `:`) is treated as syntactically optional; when it is
    /// absent, processing continues with a dummy empty-named `Named` type without pushing a
    /// diagnostic -- because per the D-TYPE-11/D-DIAG-02 decision, a "missing type
    /// annotation" is the responsibility of E1002 (the type-system layer, Unit7's
    /// responsibility), not E0502 (a syntax error, Unit4's responsibility). **Item found and
    /// fixed during this review**: the previous version unconditionally reported a missing
    /// `:` as E0502 via `expect`, which conflicted with the result required by
    /// samples/err/static/3-4_type_annotation_and_inference_errors/
    /// entry_missing_param_annotation.ybm (`def identity(x): int`, with parameter `x`
    /// unannotated) -- namely "the only diagnostic should be E1002" (under the old
    /// implementation, the missing `:` and the resulting missing type annotation piled on
    /// two extra E0502s, cascading unrelated syntax errors ahead of the E1002 the
    /// type-checking phase was supposed to emit later).
    fn parse_param(&mut self) -> Param {
        let start_span = self.current_span();
        let name = self.expect_ident("parameter name/field name");
        let ty = if matches!(self.peek_kind(), Some(TokenKind::Colon)) {
            self.bump();
            self.parse_type_ann()
        } else {
            TypeAnn {
                kind: TypeAnnKind::Named {
                    name: Arc::from(""),
                    args: Vec::new(),
                },
                span: self.previous_span(),
            }
        };
        let end_span = self.previous_span();
        Param {
            name,
            ty,
            span: span_between(start_span, end_span),
        }
    }

    /// `VariantName ('(' field (',' field)* ')')?`. No fields means a unit variant
    /// (D-TYPE-12).
    fn parse_enum_variant(&mut self) -> EnumVariant {
        let start_span = self.current_span();
        let name = self.expect_ident("variant name");
        let mut fields = Vec::new();
        let mut field_names = Vec::new();
        if matches!(self.peek_kind(), Some(TokenKind::LParen)) {
            self.bump();
            (field_names, fields) = self
                .parse_comma_separated(&TokenKind::RParen, Self::parse_variant_field_type)
                .into_iter()
                .unzip();
            self.expect(&TokenKind::RParen, "`)`");
        }
        let end_span = self.previous_span();
        EnumVariant {
            name,
            fields,
            field_names,
            leading_comments: Vec::new(),
            trailing_comment: None,
            span: span_between(start_span, end_span),
        }
    }

    /// One field type of an enum variant. A readability field name such as `radius: float`
    /// is never used during construction/destructuring per D-SYN-07 (always positional), but
    /// since SPEC §3.5 defines this notation as declaration grammar, it is kept in the AST so
    /// fmt can reproduce it (`EnumVariant.field_names`, owner ruling -- root cause D). A bare
    /// type annotation like `float` is still accepted too, in which case `None` is
    /// returned.
    fn parse_variant_field_type(&mut self) -> (Option<Arc<str>>, TypeAnn) {
        let field_name = if matches!(self.peek_kind(), Some(TokenKind::Ident(_)))
            && matches!(self.peek_kind_at(1), Some(TokenKind::Colon))
        {
            let name = self.expect_ident("field name");
            self.bump(); // ':'
            Some(name)
        } else {
            None
        };
        (field_name, self.parse_type_ann())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Diagnostic, ErrorCode, FileId};
    use crate::lexer::Lexer;

    fn parse_one_function_decl(src: &str) -> FunctionDecl {
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "a lexical error occurred: {src:?}");
        let mut parser = Parser::new(&tokens, file);
        let decl = parser.parse_function_decl();
        assert!(
            parser.diagnostics.is_empty(),
            "a syntax error occurred (unexpected): {src:?}"
        );
        decl
    }

    fn parse_one_struct_decl(src: &str) -> StructDecl {
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "a lexical error occurred: {src:?}");
        let mut parser = Parser::new(&tokens, file);
        let decl = parser.parse_struct_decl();
        assert!(
            parser.diagnostics.is_empty(),
            "a syntax error occurred (unexpected): {src:?}"
        );
        decl
    }

    fn parse_one_enum_decl(src: &str) -> EnumDecl {
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "a lexical error occurred: {src:?}");
        let mut parser = Parser::new(&tokens, file);
        let decl = parser.parse_enum_decl();
        assert!(
            parser.diagnostics.is_empty(),
            "a syntax error occurred (unexpected): {src:?}"
        );
        decl
    }

    fn function_decl_diagnostics(src: &str) -> Vec<Diagnostic> {
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "a lexical error occurred: {src:?}");
        let mut parser = Parser::new(&tokens, file);
        let _decl = parser.parse_function_decl();
        parser.diagnostics.into_vec()
    }

    fn named_type(ty: &TypeAnn) -> &str {
        match &ty.kind {
            crate::ast::TypeAnnKind::Named { name, .. } => name.as_ref(),
            _ => panic!("expected a Named type annotation"),
        }
    }

    #[test]
    fn function_decl_captures_name_generics_params_and_return_type() {
        let f = parse_one_function_decl("def add[T](x: T, y: T): T\n    return x\n");
        assert_eq!(f.name.as_ref(), "add");
        assert_eq!(
            f.generics.iter().map(Arc::as_ref).collect::<Vec<_>>(),
            vec!["T"]
        );
        assert!(f.self_param.is_none());
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.params[0].name.as_ref(), "x");
        assert_eq!(f.params[1].name.as_ref(), "y");
        assert_eq!(named_type(&f.ret), "T");
        assert!(f.effects.is_empty());
    }

    /// D-MUT-01: whether `self`/`var self` is present, and its mutability, is reflected in self_param.
    #[test]
    fn function_decl_detects_immutable_self_param() {
        let f = parse_one_function_decl("def get(self): int\n    return self.value\n");
        let sp = f
            .self_param
            .as_ref()
            .unwrap_or_else(|| panic!("self_param is None"));
        assert!(!sp.mutable);
        assert!(f.params.is_empty());
    }

    #[test]
    fn function_decl_detects_mutable_var_self_param() {
        let f = parse_one_function_decl("def bump(var self): void\n    self.n = self.n + 1\n");
        let sp = f
            .self_param
            .as_ref()
            .unwrap_or_else(|| panic!("self_param is None"));
        assert!(sp.mutable);
    }

    /// Correctly separated even when ordinary parameters follow `self`.
    #[test]
    fn function_decl_self_param_followed_by_regular_params() {
        let f = parse_one_function_decl("def add(self, n: int): int\n    return self.value + n\n");
        assert!(f.self_param.is_some());
        assert_eq!(f.params.len(), 1);
        assert_eq!(f.params[0].name.as_ref(), "n");
    }

    /// A `uses {..}` clause goes straight into the effects list.
    #[test]
    fn function_decl_uses_clause_captures_effect_names_in_order() {
        let f =
            parse_one_function_decl("def fetch(url: str): str uses {net, fs}\n    return url\n");
        assert_eq!(
            f.effects.iter().map(Arc::as_ref).collect::<Vec<_>>(),
            vec!["net", "fs"]
        );
    }

    /// D-TYPE-11: default argument (`x: int = 0`) syntax does not exist. Verifies that `=`
    /// becomes an unexpected token inside the parameter list, producing a syntax error
    /// (E0502).
    #[test]
    fn function_decl_default_argument_syntax_is_rejected_as_syntax_error() {
        let diags = function_decl_diagnostics("def f(x: int = 0): int\n    return x\n");
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "default argument syntax should become E0502 per D-TYPE-11: {diags:?}"
        );
    }

    /// D-TYPE-11: variadic argument (`*args`) syntax does not exist either. Verifies that a
    /// `*` in the parameter-name position becomes E0502 as an unexpected identifier.
    #[test]
    fn function_decl_varargs_syntax_is_rejected_as_syntax_error() {
        let diags = function_decl_diagnostics("def f(*args): int\n    return 1\n");
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "variadic argument syntax should become E0502 per D-TYPE-11: {diags:?}"
        );
    }

    /// Struct declaration: holds both fields (name: ty) and methods.
    #[test]
    fn struct_decl_captures_fields_and_methods() {
        let s = parse_one_struct_decl(
            "struct Counter\n    value: int\n\n    def get(self): int\n        return self.value\n",
        );
        assert_eq!(s.name.as_ref(), "Counter");
        assert_eq!(s.fields.len(), 1);
        assert_eq!(s.fields[0].name.as_ref(), "value");
        assert_eq!(named_type(&s.fields[0].ty), "int");
        assert_eq!(s.methods.len(), 1);
        assert_eq!(s.methods[0].name.as_ref(), "get");
        assert!(s.methods[0].self_param.is_some());
    }

    #[test]
    fn struct_decl_generics_are_captured_as_bare_names() {
        let s = parse_one_struct_decl("struct Pair[A, B]\n    first: A\n    second: B\n");
        assert_eq!(
            s.generics.iter().map(Arc::as_ref).collect::<Vec<_>>(),
            vec!["A", "B"]
        );
        assert_eq!(s.fields.len(), 2);
    }

    /// D-TYPE-12: a fieldless (unit) variant omits parentheses; fields is an empty Vec.
    #[test]
    fn enum_decl_unit_variants_have_no_fields() {
        let e = parse_one_enum_decl("enum Color\n    Red\n    Green\n    Blue\n");
        assert_eq!(e.name.as_ref(), "Color");
        assert_eq!(e.variants.len(), 3);
        for variant in &e.variants {
            assert!(variant.fields.is_empty());
        }
        assert_eq!(e.variants[0].name.as_ref(), "Red");
        assert_eq!(e.variants[2].name.as_ref(), "Blue");
    }

    /// D-SYN-07: a variant's fields are always resolved by type as positional arguments
    /// (`fields: Vec<TypeAnn>` carries no names). However, the readability field name used
    /// at declaration time (notation like `radius: float`) is kept separately in
    /// `field_names` so fmt can reproduce it (owner ruling -- root cause D; verified by the
    /// next test).
    #[test]
    fn enum_decl_variant_field_names_are_discarded_only_types_kept() {
        let e = parse_one_enum_decl(
            "enum Shape\n    Circle(radius: float)\n    Rect(w: float, h: float)\n",
        );
        assert_eq!(e.variants.len(), 2);
        assert_eq!(e.variants[0].name.as_ref(), "Circle");
        assert_eq!(e.variants[0].fields.len(), 1);
        assert_eq!(named_type(&e.variants[0].fields[0]), "float");
        assert_eq!(e.variants[1].name.as_ref(), "Rect");
        assert_eq!(e.variants[1].fields.len(), 2);
    }

    /// `field_names` corresponds to `fields` by the same index; a field written with a
    /// name becomes `Some(name)`, and a field with only a bare type annotation becomes
    /// `None` (owner ruling -- root cause D).
    #[test]
    fn enum_decl_variant_field_names_are_captured_alongside_types() {
        let e = parse_one_enum_decl(
            "enum Shape\n    Circle(radius: float)\n    Rect(w: float, h: float)\n    Origin(float)\n",
        );
        assert_eq!(
            e.variants[0]
                .field_names
                .iter()
                .map(|n| n.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("radius")]
        );
        assert_eq!(
            e.variants[1]
                .field_names
                .iter()
                .map(|n| n.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("w"), Some("h")]
        );
        assert_eq!(
            e.variants[2]
                .field_names
                .iter()
                .map(|n| n.as_deref())
                .collect::<Vec<_>>(),
            vec![None]
        );
    }

    /// Correctly determined even when a unit variant and a fielded variant are mixed within the same enum.
    #[test]
    fn enum_decl_mixes_unit_and_fielded_variants() {
        let e = parse_one_enum_decl(
            "enum Shape\n    Circle(float)\n    Rect(float, float)\n    Point\n",
        );
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].fields.len(), 1);
        assert_eq!(e.variants[1].fields.len(), 2);
        assert!(e.variants[2].fields.is_empty());
        assert_eq!(e.variants[2].name.as_ref(), "Point");
    }
}
