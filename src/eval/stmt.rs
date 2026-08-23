//! Statement evaluation (ARCHITECTURE.md §5.6).

use super::env::{Environment, Program};
use super::value::Value;
use super::{EvalResult, Flow, eval_val};
use crate::ast::{Block, Stmt, StmtKind};
use std::sync::Arc;

/// Evaluates a block's statements in order from the top. If a statement partway through
/// returns `Flow::Return`, evaluation of the remaining statements stops and the same
/// `Flow::Return` is relayed on as-is (the conversion at the call boundary happens in
/// exactly one fixed place, `eval::call::call_function`, §5.6). If the final statement is
/// an `ExprStmt`, its value becomes the block's value (depending on the caller's context)
/// — whether this `Block` is an if/match branch body or a `FunctionDecl.body` gives it a
/// different meaning (the D-SYN-11 rule vs. the §5.6 function-body rule, per the
/// VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT decision), but `eval_block` itself doesn't
/// need to make that distinction — it simply returns the last `Flow` it evaluated.
pub fn eval_block(block: &Block, env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    let mut last = Flow::Value(Value::Void);
    for stmt in &block.stmts {
        last = eval_stmt(stmt, env, program)?;
        if let Flow::Return(_) = last {
            return Ok(last);
        }
    }
    Ok(last)
}

/// Evaluates a single statement. `Return` is applied by looking up the D-TYPE-17 implicit
/// wrap (the IMPLICIT-WRAP-NO-RESOLUTIONS-FIELD decision) from
/// `program.resolutions.implicit_wrap`.
pub fn eval_stmt(stmt: &Stmt, env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    match &stmt.kind {
        StmtKind::VarDecl { name, value, .. } => {
            // `var x = expr` (D-SYN: always a new mutable binding in the current scope).
            let v = eval_val!(value, env, program);
            env.bind(Arc::clone(name), v);
            Ok(Flow::Value(Value::Void))
        }
        StmtKind::NameAssign { name, value, .. } => eval_name_assign(name, value, env, program),
        StmtKind::FieldAssign {
            target,
            field,
            value,
        } => {
            let v = eval_val!(value, env, program);
            match super::lvalue::resolve_field_place(target, field, env, program)? {
                super::lvalue::PlaceOutcome::Place(place) => {
                    *place = v;
                    Ok(Flow::Value(Value::Void))
                }
                super::lvalue::PlaceOutcome::Return(value) => Ok(Flow::Return(value)),
            }
        }
        StmtKind::IndexAssign {
            target,
            index,
            value,
        } => {
            let v = eval_val!(value, env, program);
            match super::lvalue::resolve_index_place(target, index, stmt.span, env, program)? {
                super::lvalue::PlaceOutcome::Place(place) => {
                    *place = v;
                    Ok(Flow::Value(Value::Void))
                }
                super::lvalue::PlaceOutcome::Return(value) => Ok(Flow::Return(value)),
            }
        }
        StmtKind::Discard(expr) => {
            let _ = eval_val!(expr, env, program);
            Ok(Flow::Value(Value::Void))
        }
        StmtKind::Return(expr_opt) => eval_return(expr_opt.as_ref(), env, program),
        StmtKind::ExprStmt(expr) => super::expr::eval_expr(expr, env, program),
    }
}

/// `x = expr` (assignment to a bare identifier). The type-checking phase
/// (`check_name_assign` in `types/check_stmt.rs`) decides this with the priority
/// "reassign if an existing `var` binding is visible within the current function boundary
/// (i.e. found by `env.lookup`), otherwise create a new immutable binding" (documented
/// explicitly in that same file's comments), calling `env.bind` (a fresh write into the
/// current scope) only when creating a new binding — on reassignment it reuses the
/// existing binding's type as-is and does not create a new binding. The evaluator mirrors
/// this exactly: if there is an existing visible binding (`Environment::try_lookup`
/// searches the entire current frame), it rewrites that slot directly via `lookup_mut`
/// (correctly reflecting reassignment to a `var` variable that lives across nested
/// if/match scopes); otherwise it creates a new binding in the current innermost scope via
/// `bind`.
fn eval_name_assign(
    name: &Arc<str>,
    value: &crate::ast::Expr,
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    let v = eval_val!(value, env, program);
    if env.try_lookup(name.as_ref()).is_some() {
        *env.lookup_mut(name.as_ref()) = v;
    } else {
        env.bind(Arc::clone(name), v);
    }
    Ok(Flow::Value(Value::Void))
}

/// `return expr`/`return` (bare, void functions only). Applies D-TYPE-17's implicit
/// `Ok`/`Some` wrap by looking it up from `Resolutions::implicit_wrap` (keyed by `expr`'s
/// own `NodeId`) (the IMPLICIT-WRAP-NO-RESOLUTIONS-FIELD decision, ARCHITECTURE.md §5.6).
fn eval_return(
    expr_opt: Option<&crate::ast::Expr>,
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    let Some(expr) = expr_opt else {
        return Ok(Flow::Return(Value::Void));
    };
    let v = eval_val!(expr, env, program);
    let wrapped = match program.resolutions.implicit_wrap.get(&expr.id) {
        Some(crate::types::WrapKind::Ok) => wrap_enum(v, "Result", "Ok", 0),
        Some(crate::types::WrapKind::Some) => wrap_enum(v, "Option", "Some", 0),
        None => v,
    };
    Ok(Flow::Return(wrapped))
}

fn wrap_enum(inner: Value, type_name: &str, variant_name: &str, variant_index: u32) -> Value {
    Value::Enum(Arc::new(super::value::EnumInstance {
        type_name: Arc::from(type_name),
        variant_index,
        variant_name: Arc::from(variant_name),
        fields: vec![inner],
    }))
}
