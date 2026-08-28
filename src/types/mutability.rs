//! D-MUT-01 through 05 mutability checking (E3001). Performed in the same single pass
//! as type inference of expressions (ARCHITECTURE.md §4.2, "Why TypeCheck does mutability
//! checking in the same pass").

use crate::ast::{Expr, ExprKind};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode};
use crate::types::env::TypeEnv;
use std::sync::Arc;

/// Returns the root variable for a writable place.
fn root_ident(expr: &Expr) -> Option<&Arc<str>> {
    match &expr.kind {
        ExprKind::Ident(name) => Some(name),
        ExprKind::FieldAccess { target, .. }
        | ExprKind::TupleIndex { target, .. }
        | ExprKind::Index { target, .. }
        | ExprKind::Grouping(target) => root_ident(target),
        _ => None,
    }
}

/// For an assignment-target expression (`FieldAssign` / `IndexAssign` / the reassignment
/// form of `NameAssign` / the receiver of a destructive method call), performs D-MUT-03
/// root-variable tracking and pushes E3001 to `diagnostics` if the root is not a `var`
/// binding. Also reports E3001 on the safe side when the root is not a simple variable
/// reference (e.g. a form that syntax should never have allowed in the first place, such
/// as writing directly to a call result). Does nothing when the root identifier cannot be
/// found in the current scope (an undefined identifier, which should already have been
/// reported by another diagnostic), in order to avoid a diagnostic cascade.
pub fn check_mutable_place(target: &Expr, env: &TypeEnv, diagnostics: &mut DiagnosticBag) {
    match root_ident(target) {
        Some(name) => match env.lookup(name) {
            Some(binding) if !binding.mutable => {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::ImmutableMutation,
                    span: target.span,
                    message: format!("'{name}' cannot be mutated because it is not a var binding (D-MUT-01 through 03)"),
                });
            }
            Some(_) | None => {}
        },
        None => {
            diagnostics.push(Diagnostic {
                code: ErrorCode::ImmutableMutation,
                span: target.span,
                message: "an expression that is not a write to a variable cannot be mutated"
                    .to_owned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check_mutable_place;
    use crate::ast::{Expr, ExprKind, NodeId};
    use crate::diagnostics::{DiagnosticBag, ErrorCode, FileId, Position, Span};
    use crate::types::Ty;
    use crate::types::env::TypeEnv;
    use std::sync::Arc;

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    fn ident_expr(name: &str) -> Expr {
        Expr {
            id: NodeId(0),
            kind: ExprKind::Ident(Arc::from(name)),
            span: dummy_span(),
        }
    }

    #[test]
    fn immutable_root_binding_reports_e3001() {
        let mut env = TypeEnv::root();
        env.bind(Arc::from("x"), Ty::Int, false, dummy_span());
        let mut diagnostics = DiagnosticBag::new();
        check_mutable_place(&ident_expr("x"), &env, &mut diagnostics);
        let diags = diagnostics.into_vec();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, ErrorCode::ImmutableMutation);
    }

    #[test]
    fn mutable_root_binding_is_allowed() {
        let mut env = TypeEnv::root();
        env.bind(Arc::from("x"), Ty::Int, true, dummy_span());
        let mut diagnostics = DiagnosticBag::new();
        check_mutable_place(&ident_expr("x"), &env, &mut diagnostics);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn nested_field_path_traces_back_to_root_variable() {
        let mut env = TypeEnv::root();
        env.bind(Arc::from("u"), Ty::Int, false, dummy_span());
        let nested = Expr {
            id: NodeId(1),
            kind: ExprKind::FieldAccess {
                target: Box::new(ident_expr("u")),
                field: Arc::from("tags"),
            },
            span: dummy_span(),
        };
        let mut diagnostics = DiagnosticBag::new();
        check_mutable_place(&nested, &env, &mut diagnostics);
        assert_eq!(diagnostics.len(), 1);
    }
}
