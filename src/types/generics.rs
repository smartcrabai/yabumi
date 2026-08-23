//! Type-variable substitution and unification for generic functions/structs/enums
//! (D-FUNC-04).
//!
//! §3.8 "Generics: type erasure, not monomorphization" -- the type checking phase
//! unifies type variables from the argument types down to concrete types at each call
//! site, but the evaluator does not need this information (the monomorphized type
//! arguments are recorded into `Resolutions::type_args` purely for verification
//! purposes).
//!
//! This file additionally owns `ty_from_ann`, which translates the syntactic type
//! annotation `TypeAnn` into the semantic type `Ty` -- determining "whether a given name
//! refers to a type variable (generics) or to a builtin/user-defined concrete type" is a
//! concern continuous with D-FUNC-04's type-variable resolution.

use crate::ast::{BinaryOp, TypeAnn, TypeAnnKind};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::eval::env::Program;
use crate::types::{EffectSet, Ty};
use std::collections::HashMap;
use std::sync::Arc;

/// Translates a `TypeAnn` (syntax) into a `Ty` (semantics). Resolves to `Ty::TypeVar` if
/// the name is present in `generics_in_scope`; otherwise resolves in the order builtin
/// primitive / collection / `Result`·`Option`·`Value`·`Error` / user-defined struct·enum
/// (`program.structs`/`program.enums`). Returns `None` for the empty-name
/// `Named{name:"", args:[]}` sentinel the parser generates for an unannotated parameter
/// (D-TYPE-11, see `parse_param` in parser/decl.rs) -- the caller
/// (`check_decl::check_function_decl`) reports E1002. Also returns `None` for any other
/// unknown name (e.g. a nonexistent struct/enum name) (the caller falls back to the
/// recovery placeholder `Ty::Unknown` -- judgment call made in this file: since neither
/// SPEC/DECISIONS nor samples has a test case requiring this path, this errs on the
/// practically safe side (diagnostic-cascade prevention via [`Ty::Unknown`]) rather than
/// introducing a dedicated diagnostic code).
#[must_use]
pub fn ty_from_ann(ann: &TypeAnn, generics_in_scope: &[Arc<str>], program: &Program) -> Option<Ty> {
    match &ann.kind {
        TypeAnnKind::Void => Some(Ty::Void),
        TypeAnnKind::Tuple(elems) => {
            let mut tys = Vec::with_capacity(elems.len());
            for e in elems {
                tys.push(ty_from_ann(e, generics_in_scope, program)?);
            }
            Some(Ty::Tuple(tys))
        }
        TypeAnnKind::Function {
            params,
            effects,
            ret,
        } => {
            let mut param_tys = Vec::with_capacity(params.len());
            for p in params {
                param_tys.push(ty_from_ann(p, generics_in_scope, program)?);
            }
            let ret_ty = ty_from_ann(ret, generics_in_scope, program)?;
            let mut effect_set = EffectSet::empty();
            for name in effects {
                if let Some(e) = EffectSet::from_name(name) {
                    effect_set = effect_set.union(e);
                }
            }
            Some(Ty::Function {
                params: param_tys,
                effects: effect_set,
                ret: Box::new(ret_ty),
            })
        }
        TypeAnnKind::Named { name, args } => {
            named_ty_from_ann(name, args, generics_in_scope, program)
        }
    }
}

fn named_ty_from_ann(
    name: &Arc<str>,
    args: &[TypeAnn],
    generics_in_scope: &[Arc<str>],
    program: &Program,
) -> Option<Ty> {
    if name.is_empty() {
        // D-TYPE-11: the sentinel for an unannotated parameter (`parse_param` in parser/decl.rs).
        return None;
    }
    if generics_in_scope
        .iter()
        .any(|generic| generic.as_ref() == name.as_ref())
    {
        return args.is_empty().then(|| Ty::TypeVar(Arc::clone(name)));
    }
    let mut resolved_args = Vec::with_capacity(args.len());
    for a in args {
        resolved_args.push(ty_from_ann(a, generics_in_scope, program)?);
    }
    match name.as_ref() {
        "int" if resolved_args.is_empty() => Some(Ty::Int),
        "float" if resolved_args.is_empty() => Some(Ty::Float),
        "bool" if resolved_args.is_empty() => Some(Ty::Bool),
        "str" if resolved_args.is_empty() => Some(Ty::Str),
        "list" if resolved_args.len() == 1 => Some(Ty::List(Box::new(resolved_args.remove(0)))),
        "set" if resolved_args.len() == 1 => Some(Ty::Set(Box::new(resolved_args.remove(0)))),
        "dict" if resolved_args.len() == 2 => {
            let value = resolved_args.pop().unwrap_or_else(|| unreachable!());
            let key = resolved_args.pop().unwrap_or_else(|| unreachable!());
            Some(Ty::Dict(Box::new(key), Box::new(value)))
        }
        "Result" if resolved_args.len() == 2 => Some(Ty::Named {
            name: Arc::clone(name),
            args: resolved_args,
        }),
        "Option" if resolved_args.len() == 1 => Some(Ty::Named {
            name: Arc::clone(name),
            args: resolved_args,
        }),
        "Value" | "Error" if resolved_args.is_empty() => Some(Ty::Named {
            name: Arc::clone(name),
            args: resolved_args,
        }),
        _ => {
            let generic_count = program
                .structs
                .get(name.as_ref())
                .map(|declaration| declaration.generics.len())
                .or_else(|| {
                    program
                        .enums
                        .get(name.as_ref())
                        .map(|declaration| declaration.generics.len())
                });
            generic_count
                .filter(|count| *count == resolved_args.len())
                .map(|_| Ty::Named {
                    name: Arc::clone(name),
                    args: resolved_args,
                })
        }
    }
}

/// Structurally unifies `pattern` (the declaration-side type, which may contain type
/// variables) with `concrete` (the concrete type determined at the call site), writing
/// any `Ty::TypeVar` bindings encountered into `subst`. When a type variable already
/// bound is encountered again, checks whether the existing binding and `concrete` are
/// structurally consistent (ignoring type variables). Returns `false` if the shapes do
/// not match.
pub fn unify_collect(pattern: &Ty, concrete: &Ty, subst: &mut HashMap<Arc<str>, Ty>) -> bool {
    if matches!(concrete, Ty::Unknown | Ty::TypeVar(_)) {
        // When the concrete side is Ty::Unknown (a recovery placeholder, for diagnostic-
        // cascade prevention) or Ty::TypeVar (a "don't care" placeholder the caller has no
        // structural knowledge of -- e.g. a provisional type variable used when
        // check_question hints the target of `?` via assignment-target-annotation-driven
        // inference and Result[T,E]'s E side has no concrete error type -- or an
        // unresolved type variable belonging to the generic function currently under
        // check), always treat it as compatible.
        return true;
    }
    match pattern {
        Ty::TypeVar(name) => {
            if let Some(existing) = subst.get(name.as_ref()) {
                let existing = existing.clone();
                structurally_compatible(&existing, concrete)
            } else {
                subst.insert(Arc::clone(name), concrete.clone());
                true
            }
        }
        Ty::List(a) => matches!(concrete, Ty::List(b) if unify_collect(a, b, subst)),
        Ty::Set(a) => matches!(concrete, Ty::Set(b) if unify_collect(a, b, subst)),
        Ty::Dict(ak, av) => {
            matches!(concrete, Ty::Dict(bk, bv) if unify_collect(ak, bk, subst) && unify_collect(av, bv, subst))
        }
        Ty::Tuple(a) => match concrete {
            Ty::Tuple(b) if a.len() == b.len() => a
                .iter()
                .zip(b.iter())
                .all(|(x, y)| unify_collect(x, y, subst)),
            _ => false,
        },
        Ty::Named { name: pn, args: pa } => match concrete {
            Ty::Named { name: cn, args: ca } if pn == cn && pa.len() == ca.len() => pa
                .iter()
                .zip(ca.iter())
                .all(|(x, y)| unify_collect(x, y, subst)),
            _ => false,
        },
        Ty::Function {
            params: pp,
            ret: pr,
            ..
        } => match concrete {
            // effects are ignored (EFFECT-HOF-POLYMORPHISM: a syntactic function-type
            // annotation always has empty effects, while only the actual lambda/function
            // value passed as an argument carries real effects -- comparing the two would
            // always be false, so effects are excluded from type-compatibility checking).
            Ty::Function {
                params: cp,
                ret: cr,
                ..
            } if pp.len() == cp.len() => {
                pp.iter()
                    .zip(cp.iter())
                    .all(|(x, y)| unify_collect(x, y, subst))
                    && unify_collect(pr, cr, subst)
            }
            _ => false,
        },
        Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Void | Ty::Unknown => {
            structurally_compatible(pattern, concrete)
        }
    }
}

/// Whether two types with no type variables (or already substituted) are structurally
/// identical. The same check as [`crate::types::infer::unify`], but since there is no
/// need to return the result itself as an `Option<Ty>` (used only for subst-collection
/// consistency checks), this file keeps a bool-only lightweight version.
fn structurally_compatible(a: &Ty, b: &Ty) -> bool {
    crate::types::infer::unify(a, b).is_some()
}

/// Returns the concrete type obtained by substituting `Ty::TypeVar`s in `ty` via
/// `substitution` (applied to a function's `ret` type etc. after unification). A type
/// variable absent from the substitution table is left as-is (the caller handles it via
/// E1003 etc.).
#[must_use]
pub fn substitute(ty: &Ty, substitution: &HashMap<Arc<str>, Ty>) -> Ty {
    match ty {
        Ty::TypeVar(name) => substitution
            .get(name.as_ref())
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Ty::List(inner) => Ty::List(Box::new(substitute(inner, substitution))),
        Ty::Set(inner) => Ty::Set(Box::new(substitute(inner, substitution))),
        Ty::Dict(k, v) => Ty::Dict(
            Box::new(substitute(k, substitution)),
            Box::new(substitute(v, substitution)),
        ),
        Ty::Tuple(elems) => Ty::Tuple(elems.iter().map(|t| substitute(t, substitution)).collect()),
        Ty::Named { name, args } => Ty::Named {
            name: Arc::clone(name),
            args: args.iter().map(|t| substitute(t, substitution)).collect(),
        },
        Ty::Function {
            params,
            effects,
            ret,
        } => Ty::Function {
            params: params.iter().map(|t| substitute(t, substitution)).collect(),
            effects: *effects,
            ret: Box::new(substitute(ret, substitution)),
        },
        Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Void | Ty::Unknown => ty.clone(),
    }
}

/// Whether `ty` contains an unresolved `Ty::TypeVar` (even partially). Used for D-TYPE-
/// 15/16's "when the expected type has been determined as a concrete type" check, and for
/// D-TYPE-15's "type argument uninferable" (E1003) check.
#[must_use]
pub fn contains_type_var(ty: &Ty) -> bool {
    match ty {
        Ty::TypeVar(_) => true,
        Ty::List(inner) | Ty::Set(inner) => contains_type_var(inner),
        Ty::Dict(k, v) => contains_type_var(k) || contains_type_var(v),
        Ty::Tuple(elems) => elems.iter().any(contains_type_var),
        Ty::Named { args, .. } => args.iter().any(contains_type_var),
        Ty::Function { params, ret, .. } => {
            params.iter().any(contains_type_var) || contains_type_var(ret)
        }
        Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Void | Ty::Unknown => false,
    }
}

/// Return-value determination at a generic call site (D-FUNC-04). When the caller (`check_expr.rs`) checks arguments individually in 2 stages
/// (order: [non-function-type arguments] -> [function-type/lambda arguments]) to supply
/// expected-type hints to lambda arguments, updating `subst` via `unify_collect` each
/// time, calling this function once at the end applies the determination rule
/// (D-TYPE-15 item 3: if determined by neither arguments nor explicit specification, try
/// unification against the expected type, and if still undetermined, E1003).
pub fn finalize_ret(
    declared_ret: &Ty,
    subst: &mut HashMap<Arc<str>, Ty>,
    generics: &[Arc<str>],
    expected_ret: Option<&Ty>,
    call_span: Span,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    // The criterion for "undetermined" is limited strictly to "are all of this call's own
    // type variables (`generics`) bound in subst", not "does TypeVar remain anywhere in
    // the declaration-side Ty in general" -- it is normal for `declared_ret` to keep an
    // unrelated `Ty::TypeVar` (belonging to the generic function currently under check,
    // outside this call) (D-FUNC-04: a generic function body is checked abstractly, with
    // its type variables unresolved, exactly once), and this avoids misdetecting that as
    // "uninferable" (judgment call made in this file -- originally this was determined
    // solely by `contains_type_var(&resolved_ret)`, which incorrectly reported E1003 even
    // for a call that merely passes an outer `T` straight through, such as
    // `first[T](xs: list[T]): Option[T] { return xs.get(0) }`).
    let unresolved =
        |s: &HashMap<Arc<str>, Ty>| generics.iter().any(|g| !s.contains_key(g.as_ref()));
    if unresolved(subst)
        && let Some(expected) = expected_ret
    {
        // It is fine for `expected` itself to contain a type variable (e.g. the
        // "Result[expected type, provisional error type variable]" hint check_question.rs
        // supplies to the target of `?`) -- since `unify_collect` always treats a
        // concrete-side Ty::TypeVar as compatible, only the part whose structure is known
        // (here, the T position) gets correctly picked up.
        let attempt = substitute(declared_ret, subst);
        let _ = unify_collect(&attempt, expected, subst);
    }
    if unresolved(subst) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UninferableType,
            span: call_span,
            message: "cannot infer the type argument (specify it explicitly with f[Type](...))"
                .to_owned(),
        });
        return Ty::Unknown;
    }
    substitute(declared_ret, subst)
}

/// Checks whether `op` (an arithmetic/ordering-comparison operator) is being used
/// directly on an unconstrained type parameter `T` (D-FUNC-05: only `==`/`!=` are always
/// allowed; anything else is E1013 on use). Meaningful only when `operand_ty` is a
/// `Ty::TypeVar` (a type parameter of the generic function/method currently under check,
/// not yet unified to a concrete type) -- operator checking against the concrete type
/// after monomorphization at the call site is handled separately by the usual D-OP-03
/// through 08 determination (check_expr.rs).
pub fn check_type_param_operator_usage(
    operand_ty: &Ty,
    op: BinaryOp,
    span: Span,
    diagnostics: &mut DiagnosticBag,
) -> bool {
    let is_type_var = matches!(operand_ty, Ty::TypeVar(_));
    let allowed = matches!(op, BinaryOp::EqEq | BinaryOp::NotEq);
    if is_type_var && !allowed {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UnsupportedOperatorForTypeParam,
            span,
            message: "an unconstrained type parameter can only use == / != as operators".to_owned(),
        });
        false
    } else {
        true
    }
}
