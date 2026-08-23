//! match exhaustiveness (enum coverage and D-TYPE-18's non-enum wildcard rule).

use crate::ast::{EnumDecl, LiteralPat, Pattern};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::types::Ty;
use std::collections::HashSet;

/// Checks whether the `arms` pattern sequence is exhaustive for `scrutinee_ty`.
///
/// - enum scrutinee (user-defined): whether every variant of `enum_decl` is covered by
///   at least one pattern (D-SYN-06's variant decomposition / wildcard). Reports E1021
///   if not exhaustive.
/// - Builtin enums `Result`/`Option` (determined from `scrutinee_ty`'s name even when
///   `enum_decl` is `None` -- judgment call made in this file: since
///   `stdlib::prelude::install` is unimplemented, no entity exists in `program.enums`):
///   applies the same rule treating `Result` as fixed 2 variants `Ok`/`Err`, and `Option`
///   as fixed 2 variants `Some`/`None`.
/// - Non-enum scrutinee (int/str, etc.): D-TYPE-18 requires a trailing wildcard `_`
///   (this function only checks whether a wildcard exists anywhere in the sequence --
///   judgment call made in this file: like D-SYN-06's bare-identifier resolution, the
///   syntactic positional constraint of "must be trailing" is treated as merely a lint
///   concern -- a non-trailing wildcard just makes subsequent arms unreachable -- so
///   exhaustiveness checking itself does not care about position). For bool, having both
///   `true`/`false` literal arms makes a wildcard unnecessary. For tuple, since the
///   element count is fixed by the type, a single pattern always satisfies exhaustiveness
///   (D-SYN-06/STDLIB.md §2.4).
pub fn check_exhaustiveness(
    scrutinee_ty: &Ty,
    enum_decl: Option<&EnumDecl>,
    arm_patterns: &[&Pattern],
    match_span: Span,
    diagnostics: &mut DiagnosticBag,
) {
    if let Some(decl) = enum_decl {
        let variants: Vec<(&str, bool)> = decl
            .variants
            .iter()
            .map(|v| (v.name.as_ref(), v.fields.is_empty()))
            .collect();
        check_named_variants_exhaustiveness(&variants, arm_patterns, match_span, diagnostics);
        return;
    }
    match scrutinee_ty {
        Ty::Named { name, .. } if name.as_ref() == "Result" => {
            check_named_variants_exhaustiveness(
                &[("Ok", false), ("Err", false)],
                arm_patterns,
                match_span,
                diagnostics,
            );
        }
        Ty::Named { name, .. } if name.as_ref() == "Option" => {
            check_named_variants_exhaustiveness(
                &[("Some", false), ("None", true)],
                arm_patterns,
                match_span,
                diagnostics,
            );
        }
        Ty::Bool => check_bool_exhaustiveness(arm_patterns, match_span, diagnostics),
        Ty::Tuple(_) => {
            let exhaustive = arm_patterns.iter().any(|pattern| match pattern {
                Pattern::Wildcard(_) | Pattern::BareIdent(..) => true,
                Pattern::Tuple { elements, .. } => elements
                    .iter()
                    .all(|element| !matches!(element, crate::ast::SubPattern::Literal(..))),
                _ => false,
            });
            if !exhaustive {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::NonExhaustiveMatch,
                    span: match_span,
                    message: "tuple match requires an irrefutable tuple or catch-all pattern"
                        .to_owned(),
                });
            }
        }
        Ty::Unknown => {}
        _ => {
            let has_catchall = arm_patterns
                .iter()
                .any(|p| matches!(p, Pattern::Wildcard(_) | Pattern::BareIdent(..)));
            if !has_catchall {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::NonExhaustiveMatch,
                    span: match_span,
                    message: "a match on a non-enum scrutinee requires a trailing wildcard `_` arm"
                        .to_owned(),
                });
            }
        }
    }
}

/// Exhaustiveness check for a list of `(variant name, is unit variant)` (called in common
/// from both user-defined enums and builtin Result/Option).
fn check_named_variants_exhaustiveness(
    variants: &[(&str, bool)],
    arm_patterns: &[&Pattern],
    match_span: Span,
    diagnostics: &mut DiagnosticBag,
) {
    let mut covered: HashSet<&str> = HashSet::new();
    let mut has_catchall = false;
    for pattern in arm_patterns {
        match pattern {
            Pattern::Wildcard(_) => has_catchall = true,
            Pattern::Variant { name, fields, .. }
                if fields
                    .iter()
                    .all(|field| !matches!(field, crate::ast::SubPattern::Literal(..))) =>
            {
                covered.insert(name.as_ref());
            }
            Pattern::BareIdent(name, ..) => {
                let is_unit_variant = variants
                    .iter()
                    .any(|(vname, is_unit)| *vname == name.as_ref() && *is_unit);
                if is_unit_variant {
                    covered.insert(name.as_ref());
                } else {
                    // D-SYN-06 "bare identifier name resolution": a bare identifier that
                    // doesn't match a known unit variant name is a new binding variable,
                    // which matches any value (= catch-all).
                    has_catchall = true;
                }
            }
            Pattern::Variant { .. } | Pattern::Literal(..) | Pattern::Tuple { .. } => {}
        }
    }
    if has_catchall {
        return;
    }
    let missing: Vec<&str> = variants
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !covered.contains(name))
        .collect();
    if !missing.is_empty() {
        diagnostics.push(Diagnostic {
            code: ErrorCode::NonExhaustiveMatch,
            span: match_span,
            message: format!(
                "match is not exhaustive (uncovered variants: {})",
                missing.join(", ")
            ),
        });
    }
}

fn check_bool_exhaustiveness(
    arm_patterns: &[&Pattern],
    match_span: Span,
    diagnostics: &mut DiagnosticBag,
) {
    let mut has_true = false;
    let mut has_false = false;
    let mut has_catchall = false;
    for pattern in arm_patterns {
        match pattern {
            Pattern::Wildcard(_) | Pattern::BareIdent(..) => has_catchall = true,
            Pattern::Literal(LiteralPat::Bool(true), _) => has_true = true,
            Pattern::Literal(LiteralPat::Bool(false), _) => has_false = true,
            Pattern::Literal(..) | Pattern::Variant { .. } | Pattern::Tuple { .. } => {}
        }
    }
    if !(has_catchall || (has_true && has_false)) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::NonExhaustiveMatch,
            span: match_span,
            message: "a match on bool requires both true/false arms, or a wildcard `_` arm"
                .to_owned(),
        });
    }
}
