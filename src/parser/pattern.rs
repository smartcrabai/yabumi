//! The match pattern parser (D-SYN-06's nesting constraints are enforced by the type system, ARCHITECTURE.md §2.1/§3.5).

use super::{Parser, span_between};
use crate::ast::{LiteralPat, Pattern, SubPattern};
use crate::diagnostics::ErrorCode;
use crate::lexer::TokenKind;

impl Parser<'_> {
    /// A top-level match pattern (`Pattern`, allows nested variant/tuple patterns).
    pub(crate) fn parse_pattern(&mut self) -> Pattern {
        let start_span = self.current_span();
        if let Some(lit) = self.try_parse_literal_pattern() {
            return Pattern::Literal(lit, span_between(start_span, self.previous_span()));
        }
        match self.peek_kind() {
            Some(TokenKind::Underscore) => {
                self.bump();
                Pattern::Wildcard(start_span)
            }
            Some(TokenKind::LParen) => {
                self.bump();
                let elements =
                    self.parse_comma_separated(&TokenKind::RParen, Self::parse_sub_pattern);
                self.expect(&TokenKind::RParen, "`)`");
                Pattern::Tuple {
                    elements,
                    span: span_between(start_span, self.previous_span()),
                }
            }
            Some(TokenKind::Ident(_)) => {
                let name = self.expect_ident("pattern");
                if self.peek_kind() == Some(&TokenKind::LParen) {
                    self.bump();
                    let fields =
                        self.parse_comma_separated(&TokenKind::RParen, Self::parse_sub_pattern);
                    self.expect(&TokenKind::RParen, "`)`");
                    Pattern::Variant {
                        name,
                        fields,
                        span: span_between(start_span, self.previous_span()),
                    }
                } else {
                    // A bare identifier: whether it is a unit variant name or a new binding
                    // variable is settled by the type-checking phase from the scrutinee's
                    // type (D-SYN-06, "name resolution of bare identifiers").
                    Pattern::BareIdent(name, self.next_node_id(), start_span)
                }
            }
            _ => {
                self.push_diag(
                    ErrorCode::UnexpectedToken,
                    start_span,
                    "expected a match pattern but none was found",
                );
                self.bump();
                Pattern::Wildcard(start_span)
            }
        }
    }

    /// An element position of a variant/tuple destructure (`SubPattern`, only the three
    /// kinds literal/bare-identifier/wildcard -- another variant/tuple pattern cannot be
    /// accepted syntactically).
    pub(crate) fn parse_sub_pattern(&mut self) -> SubPattern {
        let start_span = self.current_span();
        if let Some(lit) = self.try_parse_literal_pattern() {
            return SubPattern::Literal(lit, span_between(start_span, self.previous_span()));
        }
        match self.peek_kind() {
            Some(TokenKind::Underscore) => {
                self.bump();
                SubPattern::Wildcard(start_span)
            }
            Some(TokenKind::Ident(_)) => {
                let name = self.expect_ident("pattern");
                if self.peek_kind() == Some(&TokenKind::LParen)
                    || self.peek_kind() == Some(&TokenKind::LBracket)
                {
                    // D-SYN-06: nesting enum/tuple patterns is forbidden. Because SubPattern
                    // has no such variant, this pushes one diagnostic and then skips the
                    // entire nested bracket pair (tracking depth), returning just the name
                    // as a bare identifier on a best-effort basis.
                    self.push_diag(
                        ErrorCode::UnexpectedToken,
                        start_span,
                        "nesting enum/tuple patterns is forbidden (D-SYN-06)",
                    );
                    self.skip_matching_brackets();
                }
                SubPattern::BareIdent(name, self.next_node_id(), start_span)
            }
            Some(TokenKind::LParen) => {
                self.push_diag(
                    ErrorCode::UnexpectedToken,
                    start_span,
                    "nesting enum/tuple patterns is forbidden (D-SYN-06)",
                );
                self.skip_matching_brackets();
                SubPattern::Wildcard(start_span)
            }
            _ => {
                self.push_diag(
                    ErrorCode::UnexpectedToken,
                    start_span,
                    "expected a pattern but none was found",
                );
                self.bump();
                SubPattern::Wildcard(start_span)
            }
        }
    }

    /// Skips an entire matching bracket pair starting with `(` / `[`, without interpreting
    /// its internal structure (used only for recovery from D-SYN-06's forbidden nesting
    /// pattern). Does nothing if the current position is not an opening bracket.
    fn skip_matching_brackets(&mut self) {
        let mut depth: i32 = 0;
        loop {
            match self.peek_kind() {
                Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace) => {
                    depth += 1;
                    self.bump();
                }
                Some(TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace) => {
                    self.bump();
                    depth -= 1;
                    if depth <= 0 {
                        return;
                    }
                }
                None | Some(TokenKind::Eof | TokenKind::Newline | TokenKind::Dedent) => return,
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    /// Tentatively tries to parse one literal pattern (int/float/bool/str). A numeric
    /// literal with a unary minus is also folded here into the equivalent of a single token
    /// (D-LEX-04's special-casing). Returns `None` without consuming a token if it does not
    /// match.
    fn try_parse_literal_pattern(&mut self) -> Option<LiteralPat> {
        match self.peek_kind() {
            Some(TokenKind::IntLiteral(n)) => {
                let n = *n;
                self.bump();
                Some(LiteralPat::Int(n))
            }
            Some(TokenKind::FloatLiteral(f)) => {
                let f = *f;
                self.bump();
                Some(LiteralPat::Float(f))
            }
            Some(TokenKind::StringLiteral(s)) => {
                let s = s.clone();
                self.bump();
                Some(LiteralPat::Str(s))
            }
            Some(TokenKind::True) => {
                self.bump();
                Some(LiteralPat::Bool(true))
            }
            Some(TokenKind::False) => {
                self.bump();
                Some(LiteralPat::Bool(false))
            }
            Some(TokenKind::Minus) => match self.peek_kind_at(1) {
                Some(TokenKind::IntLiteral(n)) => {
                    let n = *n;
                    self.bump();
                    self.bump();
                    Some(LiteralPat::Int(-n))
                }
                Some(TokenKind::FloatLiteral(f)) => {
                    let f = *f;
                    self.bump();
                    self.bump();
                    Some(LiteralPat::Float(-f))
                }
                _ => None,
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Diagnostic, ErrorCode, FileId};
    use crate::lexer::Lexer;

    /// Parses `src` (a single match pattern including its newline) standalone. Checks that
    /// there are no errors at either the lexical or syntax level (intentionally erroneous
    /// input is handled by `parse_pattern_diagnostics` instead).
    fn parse_one_pattern(src: &str) -> Pattern {
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "a lexing error occurred: {src:?}");
        let mut parser = Parser::new(&tokens, file);
        let pattern = parser.parse_pattern();
        assert!(
            parser.diagnostics.is_empty(),
            "a parse error occurred (unexpected): {src:?}"
        );
        pattern
    }

    /// For cases where we want to check for the presence of diagnostics (input that is
    /// intentionally erroneous, such as D-SYN-06's forbidden nesting).
    fn parse_pattern_diagnostics(src: &str) -> (Pattern, Vec<Diagnostic>) {
        let file = FileId(0);
        let (tokens, _comments, lex_diag) = Lexer::new(src, file).tokenize();
        assert!(lex_diag.is_empty(), "a lexing error occurred: {src:?}");
        let mut parser = Parser::new(&tokens, file);
        let pattern = parser.parse_pattern();
        let diags = parser.diagnostics.into_vec();
        (pattern, diags)
    }

    fn as_literal(p: &Pattern) -> &LiteralPat {
        match p {
            Pattern::Literal(lit, _) => lit,
            _ => panic!("expected a Literal pattern"),
        }
    }

    fn as_variant(p: &Pattern) -> (&str, &[SubPattern]) {
        match p {
            Pattern::Variant { name, fields, .. } => (name.as_ref(), fields.as_slice()),
            _ => panic!("expected a Variant pattern"),
        }
    }

    fn as_tuple(p: &Pattern) -> &[SubPattern] {
        match p {
            Pattern::Tuple { elements, .. } => elements.as_slice(),
            _ => panic!("expected a Tuple pattern"),
        }
    }

    fn sub_bare_ident_name(p: &SubPattern) -> &str {
        match p {
            SubPattern::BareIdent(name, ..) => name.as_ref(),
            _ => panic!("expected SubPattern::BareIdent"),
        }
    }

    #[test]
    fn literal_int_pattern_parses_as_literal_int() {
        let p = parse_one_pattern("42\n");
        assert!(matches!(as_literal(&p), LiteralPat::Int(42)));
    }

    /// D-LEX-04's special-casing: a numeric literal with a unary minus is folded into a single literal pattern.
    #[test]
    fn negative_int_literal_pattern_folds_minus_into_the_literal() {
        let p = parse_one_pattern("-5\n");
        assert!(matches!(as_literal(&p), LiteralPat::Int(-5)));
    }

    #[test]
    fn negative_float_literal_pattern_folds_minus_into_the_literal() {
        let p = parse_one_pattern("-1.5\n");
        match as_literal(&p) {
            LiteralPat::Float(f) => assert!((*f - (-1.5)).abs() < f64::EPSILON),
            _ => panic!("expected Float(-1.5)"),
        }
    }

    #[test]
    fn str_and_bool_literal_patterns_parse_correctly() {
        let p = parse_one_pattern("\"go\"\n");
        match as_literal(&p) {
            LiteralPat::Str(s) => assert_eq!(s, "go"),
            _ => panic!("expected Str(\"go\")"),
        }
        let p = parse_one_pattern("true\n");
        assert!(matches!(as_literal(&p), LiteralPat::Bool(true)));
    }

    #[test]
    fn wildcard_pattern_parses_as_wildcard() {
        let p = parse_one_pattern("_\n");
        assert!(matches!(p, Pattern::Wildcard(_)));
    }

    /// D-SYN-06 "name resolution of bare identifiers": the parser **does not** decide
    /// whether it is a unit variant name or a new binding variable -- it always builds a
    /// `BareIdent` and defers to the type-checking phase.
    #[test]
    fn bare_ident_pattern_defers_unit_variant_vs_binding_decision() {
        let p = parse_one_pattern("Red\n");
        assert!(matches!(p, Pattern::BareIdent(name, ..) if name.as_ref() == "Red"));
        let p = parse_one_pattern("x\n");
        assert!(matches!(p, Pattern::BareIdent(name, ..) if name.as_ref() == "x"));
    }

    /// D-SYN-07: enum variant destructuring is always positional. Preserves the order of multiple fields.
    #[test]
    fn variant_pattern_captures_positional_sub_patterns_in_order() {
        let p = parse_one_pattern("Rect(w, h)\n");
        let (name, fields) = as_variant(&p);
        assert_eq!(name, "Rect");
        assert_eq!(fields.len(), 2);
        assert_eq!(sub_bare_ident_name(&fields[0]), "w");
        assert_eq!(sub_bare_ident_name(&fields[1]), "h");
    }

    /// D-TYPE-12: a variant with no fields omits the parentheses -- it appears as a
    /// `BareIdent` (the same path as the bare_ident_pattern_... test above, not
    /// variant_pattern itself).
    #[test]
    fn single_field_variant_pattern_captures_one_sub_pattern() {
        let p = parse_one_pattern("Circle(r)\n");
        let (name, fields) = as_variant(&p);
        assert_eq!(name, "Circle");
        assert_eq!(fields.len(), 1);
        assert_eq!(sub_bare_ident_name(&fields[0]), "r");
    }

    /// D-TYPE-06/D-SYN-06(5): tuple destructuring is positional. Preserves element order.
    #[test]
    fn tuple_pattern_captures_elements_in_order() {
        let p = parse_one_pattern("(a, b)\n");
        let elements = as_tuple(&p);
        assert_eq!(elements.len(), 2);
        assert_eq!(sub_bare_ident_name(&elements[0]), "a");
        assert_eq!(sub_bare_ident_name(&elements[1]), "b");
    }

    /// Literals and wildcards are also allowed in an element position of a variant/tuple
    /// destructure (the remaining 2 of the 3 kinds allowed by D-SYN-06's nesting
    /// constraint).
    #[test]
    fn variant_pattern_sub_fields_accept_literal_and_wildcard() {
        let p = parse_one_pattern("Pair(1, _)\n");
        let (name, fields) = as_variant(&p);
        assert_eq!(name, "Pair");
        assert_eq!(fields.len(), 2);
        assert!(matches!(
            &fields[0],
            SubPattern::Literal(LiteralPat::Int(1), _)
        ));
        assert!(matches!(&fields[1], SubPattern::Wildcard(_)));
    }

    /// D-SYN-06: nesting enum/tuple patterns is forbidden (in v1, nest a `match` instead).
    /// Verifies that a diagnostic is emitted when another variant pattern is nested, as in
    /// `Some(Circle(r))` (since the SubPattern type itself cannot represent Variant/Tuple,
    /// the parser pushes a diagnostic and then recovers on a best-effort basis by skipping
    /// the entire matching bracket pair).
    #[test]
    fn nested_variant_pattern_inside_variant_is_rejected() {
        let (p, diags) = parse_pattern_diagnostics("Some(Circle(r))\n");
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "nesting enum/tuple patterns should produce E0502 (D-SYN-06): {diags:?}"
        );
        // Even after recovery there is no crash, and the outer pattern is built as a Variant pattern.
        let (name, fields) = as_variant(&p);
        assert_eq!(name, "Some");
        assert_eq!(fields.len(), 1);
    }

    /// Same as above: nesting a tuple pattern inside an element position of a tuple destructure is likewise a D-SYN-06 violation.
    #[test]
    fn nested_tuple_pattern_inside_tuple_is_rejected() {
        let (_p, diags) = parse_pattern_diagnostics("((a, b), c)\n");
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::UnexpectedToken),
            "nesting a tuple pattern inside a tuple destructure should produce E0502 (D-SYN-06): {diags:?}"
        );
    }
}
