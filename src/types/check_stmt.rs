//! Statement type checking, block value rule (D-SYN-11, applies only to if/match branches).

use crate::ast::{Block, ElseBranch, Expr, ExprKind, IfExpr, MatchArmBody, Stmt, StmtKind};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::eval::env::Program;
use crate::types::check_expr::{check_expr, push_type_mismatch, resolve_struct_field};
use crate::types::env::TypeEnv;
use crate::types::generics::ty_from_ann;
use crate::types::infer;
use crate::types::mutability;
use crate::types::{EffectSet, Ty, WrapKind};
use std::sync::Arc;

fn is_result_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::Named { name, args } if name.as_ref() == "Result" && args.len() == 2)
}

/// Type-checks a single statement. `NameAssign`/`VarDecl`/`FieldAssign`/`IndexAssign`
/// also perform the mutability check from mutability.rs. `Return` performs D-TYPE-17's
/// implicit-wrap determination, and if it is priority 2, records it into
/// `resolutions.implicit_wrap` on the spot (IMPLICIT-WRAP-NO-RESOLUTIONS-FIELD decision,
/// §5.6/§8).
///
/// `ret_ctx` is the current function/lambda's return type (used for `Return`/`?` match
/// checking; shares the same value as `check_expr.rs`).
pub fn check_stmt(
    stmt: &Stmt,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) {
    match &stmt.kind {
        StmtKind::VarDecl { name, ty, value } => {
            check_var_decl(
                name,
                ty.as_ref(),
                value,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        StmtKind::NameAssign { name, ty, value } => {
            check_name_assign(
                name,
                ty.as_ref(),
                value,
                stmt.span,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        StmtKind::FieldAssign {
            target,
            field,
            value,
        } => {
            check_field_assign(
                target,
                field,
                value,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        StmtKind::IndexAssign {
            target,
            index,
            value,
        } => {
            check_index_assign(
                target,
                index,
                value,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        StmtKind::Discard(expr) => {
            check_expr(expr, None, ret_ctx, env, program, effects, diagnostics);
        }
        StmtKind::Return(expr_opt) => {
            check_return(
                expr_opt.as_ref(),
                stmt.span,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        StmtKind::ExprStmt(expr) => {
            let ty = check_expr(expr, None, ret_ctx, env, program, effects, diagnostics);
            if is_result_ty(&ty) {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::UnusedResult,
                    span: expr.span,
                    message: "the Result return value is unused (D-ERR-03); discard it explicitly with `_ = expr`"
                        .to_owned(),
                });
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check a var declaration (including type-annotation-driven inference)"
)]
fn check_var_decl(
    name: &Arc<str>,
    ty: Option<&crate::ast::TypeAnn>,
    value: &Expr,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) {
    let expected = ty.and_then(|t| ty_from_ann(t, env.generics(), program));
    let value_ty = check_expr(
        value,
        expected.as_ref(),
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    let final_ty = match expected {
        Some(e) => {
            if infer::unify(&e, &value_ty).is_none() && !matches!(value_ty, Ty::Unknown) {
                push_type_mismatch(
                    value.span,
                    diagnostics,
                    "the type of the var declaration's initializer does not match the annotation",
                );
            }
            e
        }
        None => value_ty,
    };
    env.bind(Arc::clone(name), final_ty, true);
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check an assignment to a bare identifier (the 3-way branch of new immutable binding / reassignment / E3001, the NameAssign rule in stmt.rs)"
)]
fn check_name_assign(
    name: &Arc<str>,
    ty: Option<&crate::ast::TypeAnn>,
    value: &Expr,
    stmt_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) {
    let expected = ty.and_then(|t| ty_from_ann(t, env.generics(), program));
    // The determination of "does x exist in the current scope" is made over the entire
    // visible scope within the function boundary (env.lookup) (judgment call made in this
    // file -- since samples/ had no case testing reassignment of an outer var from a
    // nested block and the matter was ambiguous, this adopts the intuition of ordinary
    // variable-shadowing languages: "reuse the visible binding").
    match env.lookup(name.as_ref()).cloned() {
        Some(binding) if binding.mutable => {
            let expected_ref = expected.as_ref().unwrap_or(&binding.ty);
            let value_ty = check_expr(
                value,
                Some(expected_ref),
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            if infer::unify(&binding.ty, &value_ty).is_none() && !matches!(value_ty, Ty::Unknown) {
                push_type_mismatch(
                    value.span,
                    diagnostics,
                    "the type of the reassignment does not match the existing type",
                );
            }
        }
        Some(_) => {
            diagnostics.push(Diagnostic {
                code: ErrorCode::ImmutableMutation,
                span: stmt_span,
                message: format!("'{name}' cannot be reassigned because it is not a var binding (D-MUT-01 through 03)"),
            });
            check_expr(
                value,
                expected.as_ref(),
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        None => {
            let value_ty = check_expr(
                value,
                expected.as_ref(),
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            let final_ty = match expected {
                Some(e) => {
                    if infer::unify(&e, &value_ty).is_none() && !matches!(value_ty, Ty::Unknown) {
                        push_type_mismatch(
                            value.span,
                            diagnostics,
                            "the type of the initializer does not match the annotation",
                        );
                    }
                    e
                }
                None => value_ty,
            };
            env.bind(Arc::clone(name), final_ty, false);
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check a field assignment (including D-MUT-03 root-variable tracking)"
)]
fn check_field_assign(
    target: &Expr,
    field: &Arc<str>,
    value: &Expr,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) {
    mutability::check_mutable_place(target, env, diagnostics);
    let target_ty = check_expr(target, None, ret_ctx, env, program, effects, diagnostics);
    let field_ty = resolve_struct_field(&target_ty, field.as_ref(), program).map(|(_, t)| t);
    let value_ty = check_expr(
        value,
        field_ty.as_ref(),
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    match &field_ty {
        Some(ft) => {
            if infer::unify(ft, &value_ty).is_none() && !matches!(value_ty, Ty::Unknown) {
                push_type_mismatch(
                    value.span,
                    diagnostics,
                    "the type of the field assignment does not match",
                );
            }
        }
        None if !matches!(target_ty, Ty::Unknown) => {
            push_type_mismatch(
                target.span,
                diagnostics,
                &format!("field '{field}' was not found"),
            );
        }
        None => {}
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check an index assignment (D-COL-02, both list/dict)"
)]
fn check_index_assign(
    target: &Expr,
    index: &Expr,
    value: &Expr,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) {
    mutability::check_mutable_place(target, env, diagnostics);
    let target_ty = check_expr(target, None, ret_ctx, env, program, effects, diagnostics);
    match &target_ty {
        Ty::List(t) => {
            let idx_ty = check_expr(
                index,
                Some(&Ty::Int),
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            if !matches!(idx_ty, Ty::Int | Ty::Unknown) {
                push_type_mismatch(
                    index.span,
                    diagnostics,
                    "a list index must be int (D-COL-02)",
                );
            }
            let elem_ty = (**t).clone();
            let value_ty = check_expr(
                value,
                Some(&elem_ty),
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            if infer::unify(&elem_ty, &value_ty).is_none() && !matches!(value_ty, Ty::Unknown) {
                push_type_mismatch(
                    value.span,
                    diagnostics,
                    "the type of the value assigned to the list does not match the element type",
                );
            }
        }
        Ty::Dict(k, v) => {
            let k_ty = (**k).clone();
            let idx_ty = check_expr(
                index,
                Some(&k_ty),
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            if infer::unify(&idx_ty, &k_ty).is_none() && !matches!(idx_ty, Ty::Unknown) {
                push_type_mismatch(
                    index.span,
                    diagnostics,
                    "the type of the dict index does not match the key type (D-COL-02)",
                );
            }
            let v_ty = (**v).clone();
            let value_ty = check_expr(
                value,
                Some(&v_ty),
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            if infer::unify(&v_ty, &value_ty).is_none() && !matches!(value_ty, Ty::Unknown) {
                push_type_mismatch(
                    value.span,
                    diagnostics,
                    "the type of the value assigned to the dict does not match the value type",
                );
            }
        }
        Ty::Unknown => {
            check_expr(index, None, ret_ctx, env, program, effects, diagnostics);
            check_expr(value, None, ret_ctx, env, program, effects, diagnostics);
        }
        _ => {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: target.span,
                message: "assignment with [] can only be used on list/dict (D-COL-02)".to_owned(),
            });
            check_expr(index, None, ret_ctx, env, program, effects, diagnostics);
            check_expr(value, None, ret_ctx, env, program, effects, diagnostics);
        }
    }
}

/// Checks a `return` statement. Determines D-TYPE-17's (implicit Ok/Some wrap) 3
/// priorities, and records into `resolutions.implicit_wrap` for priority 2.
fn check_return(
    expr_opt: Option<&Expr>,
    stmt_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) {
    let Some(expr) = expr_opt else {
        if let Some(rc) = ret_ctx
            && !matches!(rc, Ty::Void | Ty::Unknown)
        {
            push_type_mismatch(
                stmt_span,
                diagnostics,
                "a value-less return cannot be used in a non-void function (D-TYPE-17)",
            );
        }
        return;
    };
    let ty = check_expr(expr, ret_ctx, ret_ctx, env, program, effects, diagnostics);
    let Some(rc) = ret_ctx else {
        return;
    };
    if matches!(ty, Ty::Unknown) || infer::unify(rc, &ty).is_some() {
        // Priority 1: matches the annotation as-is (an already-error-reported Unknown is
        // also permitted here, to prevent a diagnostic cascade).
        return;
    }
    match rc {
        Ty::Named { name, args }
            if name.as_ref() == "Result"
                && args.len() == 2
                && infer::unify(&args[0], &ty).is_some() =>
        {
            program
                .resolutions
                .implicit_wrap
                .insert(expr.id, WrapKind::Ok);
        }
        Ty::Named { name, args }
            if name.as_ref() == "Option"
                && args.len() == 1
                && infer::unify(&args[0], &ty).is_some() =>
        {
            program
                .resolutions
                .implicit_wrap
                .insert(expr.id, WrapKind::Some);
        }
        _ => {
            push_type_mismatch(
                expr.span,
                diagnostics,
                "the type of the return target does not match the return-type annotation (D-TYPE-17)",
            );
        }
    }
}

/// Value rule for an if/match branch body (Block) (D-SYN-11). If the last statement is an
/// ExprStmt, returns that expression's type. If the last statement is a lone `Return`, or
/// an ExprStmt that is an if/match where all branches diverge, this "diverges"
/// (VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT decision, §5.6/§8) -- returns `None`, and the
/// caller excludes it from unification against other branches' types. Otherwise (the last
/// statement holds no value and does not diverge), pushes E1020 to `diagnostics`.
///
/// D-ERR-03 rule 4 (checking for an unused Result when an if/match branch is discarded as
/// an expression statement) is not performed by this function, because at this point
/// there is no guarantee the block's trailing value will be either used or discarded --
/// only the `ExprStmt` branch of `check_stmt` ultimately knows whether this value ends up
/// discarded as an expression statement, so it is checked there in one place (satisfying
/// "no per-branch exemption" by "determining it in a single place" -- judgment call made
/// in this file).
pub fn check_block_value(
    block: &Block,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Option<Ty> {
    let Some((last, rest)) = block.stmts.split_last() else {
        return Some(Ty::Void);
    };
    for s in rest {
        check_stmt(s, ret_ctx, env, program, effects, diagnostics);
    }
    match &last.kind {
        StmtKind::Return(_) => {
            check_stmt(last, ret_ctx, env, program, effects, diagnostics);
            None
        }
        StmtKind::ExprStmt(e) => {
            let ty = check_expr(e, expected, ret_ctx, env, program, effects, diagnostics);
            if expr_diverges(e) { None } else { Some(ty) }
        }
        _ => {
            check_stmt(last, ret_ctx, env, program, effects, diagnostics);
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: last.span,
                message:
                    "a block must end with an expression statement that returns a value (D-SYN-11)"
                        .to_owned(),
            });
            Some(Ty::Unknown)
        }
    }
}

/// Whether it can be syntactically guaranteed that `block` diverges (control never exits
/// normally and instead flows into a return, etc.) (§5.6 "the function body value rule and
/// 'divergence'"). If **any single statement** within the block satisfies
/// [`stmt_diverges`], the whole block is judged to diverge even if a (syntactically
/// unreachable) statement follows it -- looking only at "is the last statement a return"
/// would misjudge as non-diverging a case where (intentionally) unreachable code follows
/// a `return`, causing a spurious E1020 (type check) ahead of E4004 (unreachable code,
/// D-LINT-04), which is properly lint's responsibility (this function is the workaround
/// for that). Detection of unreachable code itself remains lint's responsibility, not
/// type checking's concern.
#[must_use]
pub fn block_diverges(block: &Block) -> bool {
    block.stmts.iter().any(stmt_diverges)
}

/// Whether a single statement diverges on its own (`return`, or an expression statement
/// that is an if/match where all branches diverge).
fn stmt_diverges(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Return(_) => true,
        StmtKind::ExprStmt(e) => expr_diverges(e),
        _ => false,
    }
}

/// Whether `expr` itself diverges (because all branches of an if/match diverge). Used to
/// recursively determine from `block_diverges` the case where an if/match expression
/// placed at the end of a block returns in all branches (VOID-VALUE-AND-BLOCK-VALUE-RULE-
/// CONFLICT decision).
#[must_use]
pub fn expr_diverges(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::If(if_expr) => if_expr_diverges(if_expr),
        ExprKind::Match { arms, .. } => arms.iter().all(|a| match &a.body {
            MatchArmBody::Block(b) => block_diverges(b),
            MatchArmBody::Expr(e) => expr_diverges(e),
        }),
        ExprKind::Grouping(inner) => expr_diverges(inner),
        _ => false,
    }
}

fn if_expr_diverges(if_expr: &IfExpr) -> bool {
    let then_d = block_diverges(&if_expr.then_branch);
    let else_d = match &if_expr.else_branch {
        ElseBranch::Block(b) => block_diverges(b),
        ElseBranch::ElseIf(inner) => if_expr_diverges(inner),
    };
    then_d && else_d
}
