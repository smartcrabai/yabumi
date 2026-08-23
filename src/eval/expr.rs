//! Expression evaluation, `Flow` (ARCHITECTURE.md §5.6).

use super::env::{Environment, Program};
use super::value::{CallTarget, Closure, EnumInstance, LambdaBody, MapKey, Value};
use super::{Abort, EvalResult, Flow, eval_val};
use crate::ast::{
    Arg, ElseBranch, Expr, ExprKind, FStringSegment, IfExpr, LambdaParam, LiteralPat, MatchArm,
    MatchArmBody, ParKind, Pattern, PipeCallee, PipeExpr, PipeStage, StmtKind, SubPattern,
};
use crate::diagnostics::Span;
use crate::types::BareIdentKind;
use indexmap::{IndexMap, IndexSet};
use std::sync::Arc;

/// Evaluates a single expression. The body branches on each `ExprKind` variant
/// (`Call`/`MethodCall` delegate to eval/call.rs, reads like `Index` delegate to the
/// read-only path that pairs with `resolve_place` in eval/lvalue.rs, and arithmetic/
/// comparison delegate to eval/ops.rs).
pub fn eval_expr(expr: &Expr, env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    match &expr.kind {
        ExprKind::IntLit(n) => Ok(Flow::Value(Value::Int(*n))),
        ExprKind::FloatLit(n) => Ok(Flow::Value(Value::Float(*n))),
        ExprKind::BoolLit(b) => Ok(Flow::Value(Value::Bool(*b))),
        ExprKind::StringLit(s) => Ok(Flow::Value(Value::Str(Arc::from(s.as_str())))),
        ExprKind::FString(segments) => eval_fstring(segments, env, program),
        ExprKind::Ident(name) => Ok(Flow::Value(eval_ident(name, env, program))),
        ExprKind::ListLit { elements, .. } => eval_list_lit(elements, env, program),
        ExprKind::DictLit { entries, .. } => eval_dict_lit(entries, env, program),
        ExprKind::SetLit { elements, .. } => eval_set_lit(elements, env, program),
        ExprKind::TupleLit { elements, .. } => eval_tuple_lit(elements, env, program),
        ExprKind::Unary { op, operand } => {
            let v = eval_val!(operand, env, program);
            Ok(Flow::Value(super::ops::eval_unary(*op, v, expr.span)?))
        }
        ExprKind::Binary { op, lhs, rhs } => {
            eval_binary_expr(*op, lhs, rhs, expr.span, env, program)
        }
        ExprKind::Call { .. } => super::call::eval_call(expr, env, program),
        ExprKind::MethodCall { .. } => super::call::eval_method_call(expr, env, program),
        ExprKind::FieldAccess { target, field } => eval_field_access(target, field, env, program),
        ExprKind::TupleIndex { target, index } => {
            let v = eval_val!(target, env, program);
            let Value::Tuple(items) = v else {
                unreachable!(
                    "already type-checked, so a TupleIndex target is always a tuple (D-TYPE-06)"
                )
            };
            Ok(Flow::Value(crate::stdlib::collections::tuple_index(
                &items, *index,
            )))
        }
        ExprKind::Index { target, index } => eval_index_read(expr, target, index, env, program),
        ExprKind::Question { target } => eval_question(target, env, program),
        ExprKind::Pipe(pipe) => eval_pipe(pipe, env, program),
        ExprKind::Lambda { params, body } => Ok(Flow::Value(eval_lambda(params, body, env))),
        ExprKind::If(if_expr) => eval_if(if_expr, env, program),
        ExprKind::Match { scrutinee, arms } => eval_match(scrutinee, arms, env, program),
        ExprKind::Par { kind, elements } => {
            let owned_kind = match kind {
                ParKind::List => ParKind::List,
                ParKind::Tuple => ParKind::Tuple,
            };
            crate::concurrency::eval_par_list(owned_kind, elements, env, program)
        }
        ExprKind::Grouping(inner) => eval_expr(inner, env, program),
    }
}

/// Evaluation of `expr?` (D-ERR-01/D-ERR-02). Rust's `?` is used directly to propagate
/// `Abort` — if the inner `?` has already done an early return (`Flow::Return`), this passes
/// it through unchanged.
fn eval_question(target: &Expr, env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    let v = match eval_expr(target, env, program)? {
        Flow::Value(v) => v,
        flow @ Flow::Return(_) => return Ok(flow),
    };
    match unwrap_result_or_option(&v) {
        Unwrapped::Ok(inner) => Ok(Flow::Value(inner)),
        Unwrapped::ErrOrNone(payload) => Ok(Flow::Return(payload)),
    }
}

enum Unwrapped {
    Ok(Value),
    ErrOrNone(Value),
}

/// If `v` is `Result::Ok(inner)`/`Option::Some(inner)`, returns `Unwrapped::Ok(inner)`; if
/// `Result::Err(e)`/`Option::None`, returns `Unwrapped::ErrOrNone(payload)` (`payload` is the
/// entire `Value::Enum` for `Err(e)`, or the `Value::Enum` None itself for `None`).
fn unwrap_result_or_option(v: &Value) -> Unwrapped {
    let Value::Enum(inst) = v else {
        unreachable!(
            "already type-checked, so a `?` target is always a Result/Option Enum (D-ERR-01)"
        )
    };
    match inst.variant_name.as_ref() {
        "Ok" | "Some" => Unwrapped::Ok(inst.fields[0].clone()),
        "Err" | "None" => Unwrapped::ErrOrNone(v.clone()),
        _ => unreachable!(
            "already type-checked, so a `?` target is always one of Result/Option's variants"
        ),
    }
}

/// Evaluation of an identifier. The priority order is local variable → built-in unit
/// variant (`None`/`Null`) → a user-defined enum's unit variant → a top-level function
/// (referenced as a value, turned into a closure) → a top-level constant — the evaluator
/// side reproduces the same priority order as `check_ident` in `types/check_expr.rs` (a
/// judgment call made in this file, "identifier resolution priority order" in
/// ARCHITECTURE.md §5.12).
fn eval_ident(name: &Arc<str>, env: &mut Environment, program: &Arc<Program>) -> Value {
    if let Some(v) = env.try_lookup(name.as_ref()) {
        return v.clone();
    }
    if name.as_ref() == "None" {
        return Value::Enum(Arc::new(EnumInstance {
            type_name: Arc::from("Option"),
            variant_index: 1,
            variant_name: Arc::clone(name),
            fields: Vec::new(),
        }));
    }
    if name.as_ref() == "Null" {
        return Value::Enum(Arc::new(EnumInstance {
            type_name: Arc::from("Value"),
            variant_index: 0,
            variant_name: Arc::clone(name),
            fields: Vec::new(),
        }));
    }
    if let Some((type_name, variant_index)) =
        super::call::find_variant_in_program(program, name.as_ref())
    {
        return Value::Enum(Arc::new(EnumInstance {
            type_name,
            variant_index,
            variant_name: Arc::clone(name),
            fields: Vec::new(),
        }));
    }
    if program.functions.contains_key(name.as_ref()) {
        return Value::Closure(Arc::new(Closure {
            target: CallTarget::Function(Arc::clone(name)),
            captured: Vec::new(),
        }));
    }
    if let Some(v) = program.consts.get(name.as_ref()) {
        return v.clone();
    }
    unreachable!(
        "already type-checked, so an identifier must resolve via one of these paths: {name}"
    )
}

fn eval_fstring(
    segments: &[FStringSegment],
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    let mut out = String::new();
    for seg in segments {
        match seg {
            FStringSegment::Text(t) => out.push_str(t),
            FStringSegment::Expr(e) => {
                let v = eval_val!(e, env, program);
                out.push_str(&fstring_value_to_str(&v));
            }
        }
    }
    Ok(Flow::Value(Value::Str(Arc::from(out))))
}

/// D-LEX-07: the only types allowed embedded in an f-string are int/float/bool/str (struct/
/// enum are already excluded as type errors by D-STDPOL-02). int/float/bool are
/// automatically stringified by D-STDPOL-01's built-in conversion rule.
fn fstring_value_to_str(v: &Value) -> Arc<str> {
    match v {
        Value::Str(s) => Arc::clone(s),
        Value::Int(_) | Value::Float(_) | Value::Bool(_) => {
            let Value::Str(s) = crate::stdlib::primitives::str_from_value(v) else {
                unreachable!("str_from_value always returns Value::Str")
            };
            s
        }
        _ => unreachable!(
            "already type-checked, so an f-string embedding is always str/int/float/bool (D-STDPOL-02)"
        ),
    }
}

fn eval_list_lit(elements: &[Expr], env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    let mut vals = Vec::with_capacity(elements.len());
    for e in elements {
        vals.push(eval_val!(e, env, program));
    }
    Ok(Flow::Value(Value::List(Arc::new(vals))))
}

fn eval_dict_lit(
    entries: &[(Expr, Expr)],
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    let mut map = IndexMap::new();
    for (k, v) in entries {
        let kv = eval_val!(k, env, program);
        let vv = eval_val!(v, env, program);
        let key = MapKey::try_from_value(&kv).unwrap_or_else(|| {
            unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
        });
        map.insert(key, vv);
    }
    Ok(Flow::Value(Value::Dict(Arc::new(map))))
}

fn eval_set_lit(elements: &[Expr], env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    let mut set = IndexSet::new();
    for e in elements {
        let v = eval_val!(e, env, program);
        let key = MapKey::try_from_value(&v).unwrap_or_else(|| {
            unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
        });
        set.insert(key);
    }
    Ok(Flow::Value(Value::Set(Arc::new(set))))
}

fn eval_tuple_lit(elements: &[Expr], env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    let mut vals = Vec::with_capacity(elements.len());
    for e in elements {
        vals.push(eval_val!(e, env, program));
    }
    Ok(Flow::Value(Value::Tuple(Arc::from(vals))))
}

/// `and`/`or` (D-OP-01) require short-circuit evaluation, so they are handled separately
/// from the other operators, which evaluate both sides to completion before passing them to
/// `ops::eval_binary` — short-circuiting can only be decided here (it's too late once both
/// values are in hand).
fn eval_binary_expr(
    op: crate::ast::BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    use crate::ast::BinaryOp;
    match op {
        BinaryOp::And => {
            let l = eval_val!(lhs, env, program);
            let Value::Bool(lb) = l else {
                unreachable!("already type-checked, so and's left side is always bool")
            };
            if !lb {
                return Ok(Flow::Value(Value::Bool(false)));
            }
            Ok(Flow::Value(eval_val!(rhs, env, program)))
        }
        BinaryOp::Or => {
            let l = eval_val!(lhs, env, program);
            let Value::Bool(lb) = l else {
                unreachable!("already type-checked, so or's left side is always bool")
            };
            if lb {
                return Ok(Flow::Value(Value::Bool(true)));
            }
            Ok(Flow::Value(eval_val!(rhs, env, program)))
        }
        _ => {
            let l = eval_val!(lhs, env, program);
            let r = eval_val!(rhs, env, program);
            Ok(Flow::Value(super::ops::eval_binary(op, l, r, span)?))
        }
    }
}

fn eval_field_access(
    target: &Expr,
    field: &Arc<str>,
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    if let ExprKind::Ident(_) = &target.kind
        && let Some(&ns) = program.resolutions.namespace_ref.get(&target.id)
    {
        return Ok(Flow::Value(eval_namespace_const(ns, field.as_ref())));
    }
    let v = eval_val!(target, env, program);
    let Value::Struct(inst) = v else {
        unreachable!("already type-checked, so a FieldAccess target must always be a Struct")
    };
    let idx = super::call::field_index_of(program, &inst.type_name, field.as_ref());
    Ok(Flow::Value(inst.fields[idx as usize].clone()))
}

/// A namespace constant (currently only `math.PI`/`math.E`, see `namespace_const_ty` in
/// `types/check_expr.rs`).
fn eval_namespace_const(ns: crate::types::NamespaceId, field: &str) -> Value {
    match (ns, field) {
        (crate::types::NamespaceId::Math, "PI") => Value::Float(crate::stdlib::math::PI),
        (crate::types::NamespaceId::Math, "E") => Value::Float(crate::stdlib::math::E),
        _ => unreachable!(
            "already type-checked, so a namespace constant is always math.PI or math.E"
        ),
    }
}

fn eval_index_read(
    expr: &Expr,
    target: &Expr,
    index: &Expr,
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    let container = eval_val!(target, env, program);
    let key = eval_val!(index, env, program);
    match container {
        Value::List(arr) => {
            let Value::Int(i) = key else {
                unreachable!("already type-checked, so a list subscript is always int")
            };
            let in_range = usize::try_from(i).ok().filter(|&u| u < arr.len());
            match in_range {
                Some(u) => Ok(Flow::Value(arr[u].clone())),
                None => Err(super::panic::out_of_range(expr.span, "list index")),
            }
        }
        Value::Dict(map) => {
            let mk = MapKey::try_from_value(&key).unwrap_or_else(|| {
                unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
            });
            match map.get(&mk) {
                Some(v) => Ok(Flow::Value(v.clone())),
                None => Err(super::panic::out_of_range(expr.span, "dict key")),
            }
        }
        _ => unreachable!(
            "already type-checked, so an Index target is always list/dict (str not supported, D-COL-03)"
        ),
    }
}

fn eval_pipe(pipe: &PipeExpr, env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    let mut current = eval_val!(&pipe.source, env, program);
    for stage in &pipe.stages {
        let staged = match eval_pipe_stage(stage, current, env, program)? {
            Flow::Value(v) => v,
            flow @ Flow::Return(_) => return Ok(flow),
        };
        current = if stage.question {
            match unwrap_result_or_option(&staged) {
                Unwrapped::Ok(inner) => inner,
                Unwrapped::ErrOrNone(payload) => {
                    return Ok(Flow::Return(payload));
                }
            }
        } else {
            staged
        };
    }
    Ok(Flow::Value(current))
}

fn eval_pipe_stage(
    stage: &PipeStage,
    input: Value,
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    match &stage.callee {
        PipeCallee::Bare(callee_expr) => Ok(Flow::Value(super::call::invoke_pipe_bare(
            callee_expr,
            input,
            env,
            program,
        )?)),
        PipeCallee::WithArgs { callee, args } => {
            super::call::invoke_pipe_with_args(callee, args, &input, env, program)
        }
    }
}

/// Evaluation of a lambda expression. A capture is always a value copy (D-MUT-04) —
/// obtained by enumerating every variable visible in the current frame via
/// `Environment::visible_bindings`. Because the lambda body (`Expr`) is part of the AST,
/// which does not derive `Clone` (to preserve fmt/comments, out of scope in
/// `src/ast/**`), this file explicitly performs a deep copy into a form `Value::Closure` can
/// own independently (`clone_expr_kind`, below). Type annotations (`TypeAnn`) are
/// information the evaluator never references at all (§3.8), so they are not copied and are
/// dropped to `None`.
fn eval_lambda(params: &[LambdaParam], body: &Expr, env: &Environment) -> Value {
    let captured = env.visible_bindings();
    let cloned_params: Vec<LambdaParam> = params
        .iter()
        .map(|p| LambdaParam {
            name: Arc::clone(&p.name),
            ty: None,
            span: p.span,
        })
        .collect();
    let cloned_body = clone_expr(body);
    let lambda_body = Arc::new(LambdaBody {
        params: cloned_params,
        body: cloned_body,
    });
    Value::Closure(Arc::new(Closure {
        target: CallTarget::Lambda(lambda_body),
        captured,
    }))
}

fn eval_if(if_expr: &IfExpr, env: &mut Environment, program: &Arc<Program>) -> EvalResult {
    let cond = eval_val!(&if_expr.cond, env, program);
    let Value::Bool(cond) = cond else {
        unreachable!("already type-checked, so an if condition is always bool")
    };
    if cond {
        env.push_scope();
        let r = super::stmt::eval_block(&if_expr.then_branch, env, program);
        env.pop_scope();
        r
    } else {
        match &if_expr.else_branch {
            ElseBranch::Block(block) => {
                env.push_scope();
                let r = super::stmt::eval_block(block, env, program);
                env.pop_scope();
                r
            }
            ElseBranch::ElseIf(inner) => eval_if(inner, env, program),
        }
    }
}

fn eval_match(
    scrutinee: &Expr,
    arms: &[MatchArm],
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    let v = eval_val!(scrutinee, env, program);
    for arm in arms {
        if let Some(bindings) = try_match_pattern(&arm.pattern, &v, program) {
            env.push_scope();
            for (name, val) in bindings {
                env.bind(name, val);
            }
            let result = eval_match_arm_body(&arm.body, env, program);
            env.pop_scope();
            return result;
        }
    }
    unreachable!(
        "already type-checked, so match must always hit one of the arms (exhaustiveness already checked)"
    )
}

fn eval_match_arm_body(
    body: &MatchArmBody,
    env: &mut Environment,
    program: &Arc<Program>,
) -> EvalResult {
    match body {
        MatchArmBody::Expr(e) => eval_expr(e, env, program),
        MatchArmBody::Block(block) => super::stmt::eval_block(block, env, program),
    }
}

/// Determines whether a single pattern matches the scrutinee value `v`, and if it matches,
/// returns the list of new bindings (`Vec<(name, value)>`) (D-SYN-06). Whether a bare
/// identifier (`Pattern::BareIdent`) is a unit variant name or a new binding is decided
/// using `Resolutions::bare_ident_kind` (already settled during the type-checking phase).
fn try_match_pattern(
    pattern: &Pattern,
    v: &Value,
    program: &Arc<Program>,
) -> Option<Vec<(Arc<str>, Value)>> {
    match pattern {
        Pattern::Wildcard(_) => Some(Vec::new()),
        Pattern::Literal(lit, _) => literal_matches(lit, v).then(Vec::new),
        Pattern::BareIdent(name, node_id, _) => {
            match program.resolutions.bare_ident_kind.get(node_id) {
                Some(BareIdentKind::UnitVariant) => {
                    let Value::Enum(inst) = v else {
                        unreachable!(
                            "already type-checked, so a unit-variant pattern target is an Enum"
                        )
                    };
                    (inst.variant_name.as_ref() == name.as_ref()).then(Vec::new)
                }
                _ => Some(vec![(Arc::clone(name), v.clone())]),
            }
        }
        Pattern::Variant { name, fields, .. } => {
            let Value::Enum(inst) = v else {
                unreachable!("already type-checked, so a Variant pattern target is an Enum")
            };
            if inst.variant_name.as_ref() != name.as_ref() {
                return None;
            }
            let mut bindings = Vec::new();
            for (sub, field_v) in fields.iter().zip(inst.fields.iter()) {
                bindings.extend(match_sub_pattern(sub, field_v, program)?);
            }
            Some(bindings)
        }
        Pattern::Tuple { elements, .. } => {
            let Value::Tuple(items) = v else {
                unreachable!("already type-checked, so a Tuple pattern target is a tuple")
            };
            let mut bindings = Vec::new();
            for (sub, item_v) in elements.iter().zip(items.iter()) {
                bindings.extend(match_sub_pattern(sub, item_v, program)?);
            }
            Some(bindings)
        }
    }
}

fn match_sub_pattern(
    sub: &SubPattern,
    v: &Value,
    program: &Arc<Program>,
) -> Option<Vec<(Arc<str>, Value)>> {
    match sub {
        SubPattern::Wildcard(_) => Some(Vec::new()),
        SubPattern::Literal(lit, _) => literal_matches(lit, v).then(Vec::new),
        SubPattern::BareIdent(name, node_id, _) => {
            match program.resolutions.bare_ident_kind.get(node_id) {
                Some(BareIdentKind::UnitVariant) => {
                    let Value::Enum(inst) = v else {
                        unreachable!(
                            "already type-checked, so a unit-variant sub-pattern target is an Enum"
                        )
                    };
                    (inst.variant_name.as_ref() == name.as_ref()).then(Vec::new)
                }
                _ => Some(vec![(Arc::clone(name), v.clone())]),
            }
        }
    }
}

#[expect(
    clippy::float_cmp,
    reason = "a D-LEX-04 float literal pattern is specified to require an exact match \
              against the lexical literal value, so replacing it with an approximate \
              comparison would violate the spec"
)]
fn literal_matches(lit: &LiteralPat, v: &Value) -> bool {
    match (lit, v) {
        (LiteralPat::Int(n), Value::Int(m)) => n == m,
        (LiteralPat::Float(n), Value::Float(m)) => n == m,
        (LiteralPat::Bool(a), Value::Bool(b)) => a == b,
        (LiteralPat::Str(a), Value::Str(b)) => a.as_str() == b.as_ref(),
        _ => unreachable!(
            "already type-checked, so a literal pattern's type matches the scrutinee's type"
        ),
    }
}

// =========================================================================
// Deep AST copy for a lambda body (dedicated to `eval_lambda`).
//
// `ast::Expr` etc. do not derive `Clone` (a design choice for fmt idempotency and comment
// preservation, src/ast/** is out of scope here). `Value::Closure` needs an `Arc<LambdaBody>`
// that owns the lambda body independently (§3.9), so a borrowed `&Expr` cannot be held —
// this performs the sole copy that preserves only the information needed for evaluation
// (NodeId, Span, the structure relevant to execution). fmt-only accompanying information
// (leading_comments/trailing_comment/doc_comment/was_multiline/the TypeAnn type annotation)
// is never referenced by the evaluator, so it is not copied and is dropped to its default
// value — a judgment call made in this file, within the range of "minor variation" that the
// opening of ARCHITECTURE.md allows (it does not change the resolution keys NodeId/Span or
// the execution semantics).
// =========================================================================

fn clone_expr(expr: &Expr) -> Expr {
    Expr {
        id: expr.id,
        kind: clone_expr_kind(&expr.kind),
        span: expr.span,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "every ExprKind variant must be copied one-to-one, and the number of variants \
              (about 20) directly determines this function's line count, so splitting it \
              would not improve readability"
)]
fn clone_expr_kind(kind: &ExprKind) -> ExprKind {
    match kind {
        ExprKind::IntLit(n) => ExprKind::IntLit(*n),
        ExprKind::FloatLit(n) => ExprKind::FloatLit(*n),
        ExprKind::BoolLit(b) => ExprKind::BoolLit(*b),
        ExprKind::StringLit(s) => ExprKind::StringLit(s.clone()),
        ExprKind::FString(segs) => {
            ExprKind::FString(segs.iter().map(clone_fstring_segment).collect())
        }
        ExprKind::Ident(name) => ExprKind::Ident(Arc::clone(name)),
        ExprKind::ListLit {
            elements,
            was_multiline,
        } => ExprKind::ListLit {
            elements: elements.iter().map(clone_expr).collect(),
            was_multiline: *was_multiline,
        },
        ExprKind::DictLit {
            entries,
            was_multiline,
        } => ExprKind::DictLit {
            entries: entries
                .iter()
                .map(|(k, v)| (clone_expr(k), clone_expr(v)))
                .collect(),
            was_multiline: *was_multiline,
        },
        ExprKind::SetLit {
            elements,
            was_multiline,
        } => ExprKind::SetLit {
            elements: elements.iter().map(clone_expr).collect(),
            was_multiline: *was_multiline,
        },
        ExprKind::TupleLit {
            elements,
            was_multiline,
        } => ExprKind::TupleLit {
            elements: elements.iter().map(clone_expr).collect(),
            was_multiline: *was_multiline,
        },
        ExprKind::Unary { op, operand } => ExprKind::Unary {
            op: *op,
            operand: Box::new(clone_expr(operand)),
        },
        ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
            op: *op,
            lhs: Box::new(clone_expr(lhs)),
            rhs: Box::new(clone_expr(rhs)),
        },
        ExprKind::Call {
            callee,
            args,
            was_multiline,
            ..
        } => ExprKind::Call {
            callee: Box::new(clone_expr(callee)),
            type_args: Vec::new(),
            args: args.iter().map(clone_arg).collect(),
            was_multiline: *was_multiline,
        },
        ExprKind::MethodCall {
            receiver,
            method,
            args,
            was_multiline,
            ..
        } => ExprKind::MethodCall {
            receiver: Box::new(clone_expr(receiver)),
            method: Arc::clone(method),
            type_args: Vec::new(),
            args: args.iter().map(clone_arg).collect(),
            was_multiline: *was_multiline,
        },
        ExprKind::FieldAccess { target, field } => ExprKind::FieldAccess {
            target: Box::new(clone_expr(target)),
            field: Arc::clone(field),
        },
        ExprKind::TupleIndex { target, index } => ExprKind::TupleIndex {
            target: Box::new(clone_expr(target)),
            index: *index,
        },
        ExprKind::Index { target, index } => ExprKind::Index {
            target: Box::new(clone_expr(target)),
            index: Box::new(clone_expr(index)),
        },
        ExprKind::Question { target } => ExprKind::Question {
            target: Box::new(clone_expr(target)),
        },
        ExprKind::Pipe(pipe) => ExprKind::Pipe(clone_pipe_expr(pipe)),
        ExprKind::Lambda { params, body } => ExprKind::Lambda {
            params: params
                .iter()
                .map(|p| LambdaParam {
                    name: Arc::clone(&p.name),
                    ty: None,
                    span: p.span,
                })
                .collect(),
            body: Box::new(clone_expr(body)),
        },
        ExprKind::If(if_expr) => ExprKind::If(Box::new(clone_if_expr(if_expr))),
        ExprKind::Match { scrutinee, arms } => ExprKind::Match {
            scrutinee: Box::new(clone_expr(scrutinee)),
            arms: arms.iter().map(clone_match_arm).collect(),
        },
        ExprKind::Par { kind, elements } => ExprKind::Par {
            kind: match kind {
                ParKind::List => ParKind::List,
                ParKind::Tuple => ParKind::Tuple,
            },
            elements: elements.iter().map(clone_expr).collect(),
        },
        ExprKind::Grouping(inner) => ExprKind::Grouping(Box::new(clone_expr(inner))),
    }
}

fn clone_fstring_segment(seg: &FStringSegment) -> FStringSegment {
    match seg {
        FStringSegment::Text(t) => FStringSegment::Text(t.clone()),
        FStringSegment::Expr(e) => FStringSegment::Expr(Box::new(clone_expr(e))),
    }
}

fn clone_arg(arg: &Arg) -> Arg {
    Arg {
        name: arg.name.clone(),
        value: clone_expr(&arg.value),
        is_placeholder: arg.is_placeholder,
    }
}

fn clone_pipe_expr(pipe: &PipeExpr) -> PipeExpr {
    PipeExpr {
        source: Box::new(clone_expr(&pipe.source)),
        stages: pipe.stages.iter().map(clone_pipe_stage).collect(),
    }
}

fn clone_pipe_stage(stage: &PipeStage) -> PipeStage {
    PipeStage {
        callee: match &stage.callee {
            PipeCallee::Bare(e) => PipeCallee::Bare(clone_expr(e)),
            PipeCallee::WithArgs { callee, args } => PipeCallee::WithArgs {
                callee: Box::new(clone_expr(callee)),
                args: args.iter().map(clone_arg).collect(),
            },
        },
        question: stage.question,
        span: stage.span,
    }
}

fn clone_if_expr(if_expr: &IfExpr) -> IfExpr {
    IfExpr {
        cond: Box::new(clone_expr(&if_expr.cond)),
        then_branch: clone_block(&if_expr.then_branch),
        else_branch: match &if_expr.else_branch {
            ElseBranch::Block(b) => ElseBranch::Block(clone_block(b)),
            ElseBranch::ElseIf(inner) => ElseBranch::ElseIf(Box::new(clone_if_expr(inner))),
        },
        span: if_expr.span,
    }
}

fn clone_match_arm(arm: &MatchArm) -> MatchArm {
    MatchArm {
        pattern: clone_pattern(&arm.pattern),
        body: match &arm.body {
            MatchArmBody::Expr(e) => MatchArmBody::Expr(clone_expr(e)),
            MatchArmBody::Block(b) => MatchArmBody::Block(clone_block(b)),
        },
        leading_comments: Vec::new(),
        trailing_comment: None,
        span: arm.span,
    }
}

fn clone_pattern(pattern: &Pattern) -> Pattern {
    match pattern {
        Pattern::Literal(lit, span) => Pattern::Literal(clone_literal(lit), *span),
        Pattern::BareIdent(name, id, span) => Pattern::BareIdent(Arc::clone(name), *id, *span),
        Pattern::Wildcard(span) => Pattern::Wildcard(*span),
        Pattern::Variant { name, fields, span } => Pattern::Variant {
            name: Arc::clone(name),
            fields: fields.iter().map(clone_sub_pattern).collect(),
            span: *span,
        },
        Pattern::Tuple { elements, span } => Pattern::Tuple {
            elements: elements.iter().map(clone_sub_pattern).collect(),
            span: *span,
        },
    }
}

fn clone_sub_pattern(sub: &SubPattern) -> SubPattern {
    match sub {
        SubPattern::Literal(lit, span) => SubPattern::Literal(clone_literal(lit), *span),
        SubPattern::BareIdent(name, id, span) => {
            SubPattern::BareIdent(Arc::clone(name), *id, *span)
        }
        SubPattern::Wildcard(span) => SubPattern::Wildcard(*span),
    }
}

fn clone_literal(lit: &LiteralPat) -> LiteralPat {
    match lit {
        LiteralPat::Int(n) => LiteralPat::Int(*n),
        LiteralPat::Float(n) => LiteralPat::Float(*n),
        LiteralPat::Bool(b) => LiteralPat::Bool(*b),
        LiteralPat::Str(s) => LiteralPat::Str(s.clone()),
    }
}

fn clone_block(block: &crate::ast::Block) -> crate::ast::Block {
    crate::ast::Block {
        stmts: block.stmts.iter().map(clone_stmt).collect(),
        span: block.span,
    }
}

fn clone_stmt(stmt: &crate::ast::Stmt) -> crate::ast::Stmt {
    crate::ast::Stmt {
        kind: clone_stmt_kind(&stmt.kind),
        span: stmt.span,
        doc_comment: None,
        leading_comments: Vec::new(),
        trailing_comment: None,
    }
}

fn clone_stmt_kind(kind: &StmtKind) -> StmtKind {
    match kind {
        StmtKind::VarDecl { name, value, .. } => StmtKind::VarDecl {
            name: Arc::clone(name),
            ty: None,
            value: clone_expr(value),
        },
        StmtKind::NameAssign { name, value, .. } => StmtKind::NameAssign {
            name: Arc::clone(name),
            ty: None,
            value: clone_expr(value),
        },
        StmtKind::FieldAssign {
            target,
            field,
            value,
        } => StmtKind::FieldAssign {
            target: clone_expr(target),
            field: Arc::clone(field),
            value: clone_expr(value),
        },
        StmtKind::IndexAssign {
            target,
            index,
            value,
        } => StmtKind::IndexAssign {
            target: clone_expr(target),
            index: clone_expr(index),
            value: clone_expr(value),
        },
        StmtKind::Discard(e) => StmtKind::Discard(clone_expr(e)),
        StmtKind::Return(e) => StmtKind::Return(e.as_ref().map(clone_expr)),
        StmtKind::ExprStmt(e) => StmtKind::ExprStmt(clone_expr(e)),
    }
}
