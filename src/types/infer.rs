//! Assignment-target-annotation-driven inference (D-TYPE-15/16), and type unification.

use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::types::{EffectSet, Ty};

/// Unifies two types. On success, returns the unified concrete type (when one side
/// contains a type variable, it aligns to the concrete side). Returns `None` on failure
/// (the caller picks a context-appropriate diagnostic code such as E1010/E1020).
///
/// Both `Ty::Unknown` (an internal recovery placeholder for this crate) and `Ty::TypeVar`
/// (an unresolved type variable while checking a generic function body) are treated as
/// "always compatible with the other side" -- the former to prevent a diagnostic cascade,
/// and the latter because of the design where a generic function body is checked exactly
/// once while its type variables remain unresolved (D-FUNC-04, before monomorphization);
/// this avoids spuriously raising a type error when comparing the same `T` against itself
/// (or `T` against the abstract pre-call context) across if/match branches, for example.
#[must_use]
pub fn unify(a: &Ty, b: &Ty) -> Option<Ty> {
    match (a, b) {
        (Ty::Unknown | Ty::TypeVar(_), other) | (other, Ty::Unknown | Ty::TypeVar(_)) => {
            Some(other.clone())
        }
        (Ty::Int, Ty::Int) => Some(Ty::Int),
        (Ty::Float, Ty::Float) => Some(Ty::Float),
        (Ty::Bool, Ty::Bool) => Some(Ty::Bool),
        (Ty::Str, Ty::Str) => Some(Ty::Str),
        (Ty::Void, Ty::Void) => Some(Ty::Void),
        (Ty::List(x), Ty::List(y)) => Some(Ty::List(Box::new(unify(x, y)?))),
        (Ty::Set(x), Ty::Set(y)) => Some(Ty::Set(Box::new(unify(x, y)?))),
        (Ty::Dict(xk, xv), Ty::Dict(yk, yv)) => {
            Some(Ty::Dict(Box::new(unify(xk, yk)?), Box::new(unify(xv, yv)?)))
        }
        (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
            let mut out = Vec::with_capacity(xs.len());
            for (x, y) in xs.iter().zip(ys.iter()) {
                out.push(unify(x, y)?);
            }
            Some(Ty::Tuple(out))
        }
        (Ty::Named { name: xn, args: xa }, Ty::Named { name: yn, args: ya })
            if xn == yn && xa.len() == ya.len() =>
        {
            let mut out = Vec::with_capacity(xa.len());
            for (x, y) in xa.iter().zip(ya.iter()) {
                out.push(unify(x, y)?);
            }
            Some(Ty::Named {
                name: xn.clone(),
                args: out,
            })
        }
        (
            Ty::Function {
                params: xp,
                effects: xe,
                ret: xr,
            },
            Ty::Function {
                params: yp,
                effects: ye,
                ret: yr,
            },
        ) if xp.len() == yp.len() => {
            let mut params = Vec::with_capacity(xp.len());
            for (x, y) in xp.iter().zip(yp.iter()) {
                params.push(unify(x, y)?);
            }
            let ret = unify(xr, yr)?;
            // effects are excluded from compatibility checking (same reason as
            // unify_collect in generics.rs, EFFECT-HOF-POLYMORPHISM) -- the result merely
            // keeps the union of both sides as information.
            Some(Ty::Function {
                params,
                effects: EffectSet::union(*xe, *ye),
                ret: Box::new(ret),
            })
        }
        _ => None,
    }
}

/// Assignment-target-driven inference for the 4 contexts D-TYPE-16 defines (variable
/// declaration initializer / return statement / function call argument / struct or enum
/// constructor argument). Handles D-TYPE-15's uninferable-type determination (E1003)
/// uniformly for empty collections / a bare `None` / a return-value-only type variable --
/// if `expected` is not a concrete type (contains no type variable), pushes E1003 as
/// uninferable and returns the recovery placeholder [`Ty::Unknown`].
///
/// The caller (`check_expr.rs`) calls this function when checking an expression that
/// "cannot be determined without context", such as an empty list/dict/set literal or a
/// bare `None`. The case of a generic function call whose type variable appears only in
/// the return value (D-TYPE-15 item 3) is reported as E1003 independently by
/// `generics::finalize_ret`, so this function serves as the receiving end for the
/// remaining 2 patterns not routed through that path (empty collection / bare `None`).
#[must_use]
pub fn infer_with_expected(
    expected: Option<&Ty>,
    span: Span,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    // Even when `expected` contains a `Ty::TypeVar` (belonging to the generic function
    // currently under check), treat this as "successfully inferred" -- for example, when
    // abstractly checking the body of `make_empty[T](): list[T] { return [] }`, the
    // `return`'s ret_ctx is `list[T]` as-is (T stays unresolved), and this must be
    // distinguished from the "no context at all" case (where `expected` is None itself,
    // as in `x = []`) -- this file's judgment call: the former is the normal case where
    // D-TYPE-16's 4 contexts are in effect, and only the latter falls under D-TYPE-15's
    // uninferable-type case. The actual resolution of the type variable per call site is
    // handled separately by generics::finalize_ret.
    if let Some(ty) = expected {
        ty.clone()
    } else {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UninferableType,
            span,
            message: "cannot infer the type (add a type annotation)".to_owned(),
        });
        Ty::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::{infer_with_expected, unify};
    use crate::diagnostics::{DiagnosticBag, ErrorCode, FileId, Position, Span};
    use crate::types::Ty;

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    #[test]
    fn unify_matches_identical_primitives() {
        assert_eq!(unify(&Ty::Int, &Ty::Int), Some(Ty::Int));
        assert_eq!(unify(&Ty::Int, &Ty::Str), None);
    }

    #[test]
    fn unify_recurses_into_list_element_type() {
        assert_eq!(
            unify(&Ty::List(Box::new(Ty::Int)), &Ty::List(Box::new(Ty::Int))),
            Some(Ty::List(Box::new(Ty::Int)))
        );
        assert_eq!(
            unify(&Ty::List(Box::new(Ty::Int)), &Ty::List(Box::new(Ty::Str))),
            None
        );
    }

    #[test]
    fn unify_unknown_is_always_compatible() {
        assert_eq!(unify(&Ty::Unknown, &Ty::Int), Some(Ty::Int));
        assert_eq!(unify(&Ty::Str, &Ty::Unknown), Some(Ty::Str));
    }

    #[test]
    fn infer_with_expected_uses_concrete_expected_type() {
        let mut diagnostics = DiagnosticBag::new();
        let ty = infer_with_expected(
            Some(&Ty::List(Box::new(Ty::Int))),
            dummy_span(),
            &mut diagnostics,
        );
        assert_eq!(ty, Ty::List(Box::new(Ty::Int)));
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn infer_with_expected_reports_e1003_without_concrete_expected() {
        let mut diagnostics = DiagnosticBag::new();
        let ty = infer_with_expected(None, dummy_span(), &mut diagnostics);
        assert_eq!(ty, Ty::Unknown);
        let diags = diagnostics.into_vec();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, ErrorCode::UninferableType);
    }
}
