//! Function/method call conventions, `var self` write-back (a chain of `Arc::make_mut`,
//! ARCHITECTURE.md §5.6).
//!
//! Evaluation of every `Call`/`MethodCall` expression is consolidated into this file:
//! - user-defined top-level functions, struct methods (`self`/`var self`), closure calls
//! - struct construction, enum variant construction (D-SYN-07/D-TYPE-13, distinguished via
//!   `CallKind`)
//! - the D-TYPE-14/D-STDPOL-01 flat builtins (`int`/`float`/`str`/`print`/`eprint`/`assert`/
//!   `set()`) — identifiers given special treatment that have no entry in
//!   `Resolutions::call_kind` (determined with the same priority as `check_call_named` in
//!   the type-checking phase, a judgment call made in this file)
//! - built-in methods on primitives/collections/Result/Option/Value (delegated to stdlib)
//! - namespace function calls (`fs.read` etc., distinguished via `Resolutions::namespace_ref`)
//!
//! Since the stdlib body itself (`src/stdlib/**`) already has Units 12-14 implemented, this
//! file is dedicated purely to wiring up to its public signatures (a policy adopted to avoid
//! changing out-of-scope files).

use super::env::{Environment, Program};
use super::value::{CallTarget, Closure, EnumInstance, LambdaBody, MapKey, StructInstance, Value};
use super::{Abort, Flow, eval_val};
use crate::ast::{Arg, Expr, ExprKind, FunctionDecl, TypeAnnKind};
use crate::diagnostics::Span;
use crate::types::{CallKind, NamespaceId, Ty};
use indexmap::{IndexMap, IndexSet};
use std::collections::HashMap;
use std::sync::Arc;

// =========================================================================
// Executing a function/method body (the core of the call convention)
// =========================================================================

/// Pairs up formal and actual parameters and builds the initial scope of a new frame (a
/// value copy, D-MUT-04). An ordinary function call always uses positional arguments
/// (D-TYPE-11).
fn bind_params(decl: &FunctionDecl, args: Vec<Value>) -> HashMap<Arc<str>, Value> {
    decl.params
        .iter()
        .zip(args)
        .map(|(p, v)| (Arc::clone(&p.name), v))
        .collect()
}

/// Runs `decl.body` starting from `scope`, and returns the return value along with the
/// `Environment` after execution (needed to read back `self` post-call). Shared by
/// `call_function`/`call_method_with_self`.
fn run_body_with_env(
    decl: &FunctionDecl,
    scope: HashMap<Arc<str>, Value>,
    program: &Arc<Program>,
) -> Result<(Value, Environment), Abort> {
    let _guard = super::enter_call(decl.span)?;
    let mut env = Environment::with_frame(scope);
    let flow = super::stmt::eval_block(&decl.body, &mut env, program)?;
    let result = match flow {
        Flow::Return(v) => v,
        Flow::Value(v) => {
            if matches!(decl.ret.kind, TypeAnnKind::Void) {
                Value::Void
            } else {
                v
            }
        }
    };
    Ok((result, env))
}

/// The function-call boundary. This is the sole place where `Flow::Return` is finally
/// converted into "that call's return value" — the `Flow::Return` variant itself never
/// leaks to the caller. Per the function-body value rule (the
/// VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT decision, §5.6/§8), when the return annotation
/// is void, the value of the final expression statement (even if it happens to hold some
/// value) is always discarded and `Value::Void` is implicitly returned. For the non-void
/// case, in a correctly type-checked program `eval_block` never returns `Flow::Value` (the
/// body is forced by type checking to always end either in an explicit `return`, or in an
/// if/match whose every branch ends in a `return`, i.e. one that "diverges").
pub fn call_function(
    decl: &FunctionDecl,
    args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let scope = bind_params(decl, args);
    let (result, _env) = run_body_with_env(decl, scope, program)?;
    Ok(result)
}

/// Calling a lambda body. A lambda is also a function boundary, and a bare `?` inside the
/// lambda is converted into its return value right here (D-ERR-02).
fn call_lambda(
    body: &LambdaBody,
    captured: &[(Arc<str>, Value)],
    args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let _guard = super::enter_call(body.body.span)?;
    let mut scope: HashMap<Arc<str>, Value> = captured
        .iter()
        .map(|(n, v)| (Arc::clone(n), v.clone()))
        .collect();
    for (p, v) in body.params.iter().zip(args) {
        scope.insert(Arc::clone(&p.name), v);
    }
    let mut env = Environment::with_frame(scope);
    let flow = super::expr::eval_expr(&body.body, &mut env, program)?;
    Ok(match flow {
        // Whether it's Flow::Value or Flow::Return, at a lambda call boundary it becomes
        // the return value either way (D-ERR-02: the scope of `?` is the innermost
        // function).
        Flow::Value(v) | Flow::Return(v) => v,
    })
}

/// A call as a lambda/function value (e.g. calling through a closure in something like
/// `xs.par_map(f)`). After injecting `Closure.captured` into the new frame's initial scope,
/// this delegates to either `call_function` or a stdlib built-in implementation depending
/// on `CallTarget`.
pub fn call_closure(
    closure: &Closure,
    args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    match &closure.target {
        CallTarget::Function(name) => {
            let decl = program.functions.get(name).unwrap_or_else(|| {
                unreachable!("already type-checked, so a top-level function must exist")
            });
            call_function(decl, args, program)
        }
        CallTarget::Lambda(body) => call_lambda(body, &closure.captured, args, program),
        CallTarget::Builtin(_id) => {
            // The current type checker (check_ident) does not implement Ident resolution
            // that references a namespace function or something like print/assert as a
            // value, so there is no path anywhere in eval that produces
            // `Value::Closure(CallTarget::Builtin(_))` (reported as needing follow-up — if
            // Units 12/13 ever add syntax for passing a stdlib function as a value, an
            // implementation will be needed here).
            unreachable!(
                "unreachable because the current type checker never resolves syntax that references a namespace/builtin function as a value"
            )
        }
    }
}

/// Calling a struct method (`self`/`var self`). For `var self`, after the call, `self`'s
/// final value is written back into the receiver's slot (a chain of Arc::make_mut,
/// D-MUT-01/02/03, ARCHITECTURE.md §3.10).
fn call_method_with_self(
    decl: &FunctionDecl,
    self_value: Value,
    args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<(Value, Value), Abort> {
    let mut scope = bind_params(decl, args);
    scope.insert(Arc::from("self"), self_value);
    let (result, mut env) = run_body_with_env(decl, scope, program)?;
    let final_self = env.lookup_mut("self").clone();
    Ok((result, final_self))
}

// =========================================================================
// Call expressions (function call / struct construction / enum variant construction /
// closure call / flat builtins)
// =========================================================================

/// Evaluation of `ExprKind::Call`. If `Resolutions::call_kind` has an entry, this is one of
/// the four user-defined categories (function/struct construction/enum variant
/// construction/closure call); if not, it is one of the D-TYPE-14/D-STDPOL-01 flat builtins
/// (the set of names for which the type-checking phase's `check_call_named` returns early
/// without writing a `call_kind`, kept in sync with `types/check_expr.rs`).
pub fn eval_call(expr: &Expr, env: &mut Environment, program: &Arc<Program>) -> super::EvalResult {
    let ExprKind::Call { callee, args, .. } = &expr.kind else {
        unreachable!("eval_call is only ever called for a Call")
    };
    if let Some(kind) = program.resolutions.call_kind.get(&expr.id).copied() {
        return match kind {
            CallKind::ClosureCall => eval_closure_call(callee, args, env, program),
            CallKind::FunctionCall => eval_user_function_call(callee, args, env, program),
            CallKind::StructInit => {
                let ExprKind::Ident(name) = &callee.kind else {
                    unreachable!("a StructInit callee is always Ident")
                };
                eval_struct_init(name, args, env, program)
            }
            CallKind::EnumVariantInit => {
                let ExprKind::Ident(name) = &callee.kind else {
                    unreachable!("an EnumVariantInit callee is always Ident")
                };
                eval_enum_variant_init(name, args, env, program)
            }
        };
    }
    let ExprKind::Ident(name) = &callee.kind else {
        unreachable!("a Call with no call_kind is only ever a flat identifier call")
    };
    eval_flat_builtin_call(name.as_ref(), args, expr.span, env, program)
}

fn eval_closure_call(
    callee: &Expr,
    args: &[Arg],
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    let callee_v = eval_val!(callee, env, program);
    let Value::Closure(closure) = callee_v else {
        unreachable!("already type-checked, so a ClosureCall target is always Closure")
    };
    let mut arg_values = Vec::with_capacity(args.len());
    for a in args {
        arg_values.push(eval_val!(&a.value, env, program));
    }
    Ok(Flow::Value(call_closure(&closure, arg_values, program)?))
}

fn eval_user_function_call(
    callee: &Expr,
    args: &[Arg],
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    let ExprKind::Ident(name) = &callee.kind else {
        unreachable!("a FunctionCall callee is always Ident")
    };
    let decl = Arc::clone(program.functions.get(name.as_ref()).unwrap_or_else(|| {
        unreachable!("already type-checked, so a top-level function must exist")
    }));
    let mut arg_values = Vec::with_capacity(args.len());
    for a in args {
        arg_values.push(eval_val!(&a.value, env, program));
    }
    Ok(Flow::Value(call_function(&decl, arg_values, program)?))
}

/// The declaration-order field-name list for the built-in structs (Error/Response/
/// HttpOptions/ProcOutput) (STDLIB.md §3.3/§6/§8). While `stdlib::prelude::install` is not
/// yet implemented and no instance of them exists in `program.structs`, eval holds the same
/// order directly as the type-checking phase (`builtin_struct_fields` in
/// `types/check_expr.rs`) does (a judgment call made in this file).
fn builtin_struct_field_names(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "Error" => Some(&["kind", "message", "cause"]),
        "Response" => Some(&["status", "headers", "body"]),
        "HttpOptions" => Some(&["headers", "timeout_ms"]),
        "ProcOutput" => Some(&["stdout", "stderr", "exit_code"]),
        _ => None,
    }
}

/// Resolves at runtime the declaration-order index of `field` on the struct type named
/// `type_name` (see the documentation in `eval/lvalue.rs` — since `StmtKind::FieldAssign`
/// has no entry in `Resolutions::field_index`, eval always goes through this path for a
/// field index, to keep the implementation unified with the `ExprKind::FieldAccess` side).
pub(crate) fn field_index_of(program: &Program, type_name: &str, field: &str) -> u32 {
    if let Some(names) = builtin_struct_field_names(type_name) {
        let idx = names
            .iter()
            .position(|n| *n == field)
            .unwrap_or_else(|| unreachable!("already type-checked, so the field name must match"));
        return u32::try_from(idx).unwrap_or(0);
    }
    let decl = program.structs.get(type_name).unwrap_or_else(|| {
        unreachable!("already type-checked, so the struct declaration must exist")
    });
    let idx = decl
        .fields
        .iter()
        .position(|f| f.name.as_ref() == field)
        .unwrap_or_else(|| unreachable!("already type-checked, so the field name must match"));
    u32::try_from(idx).unwrap_or(0)
}

/// Formats an arbitrary generic `Result` error payload for diagnostics.
pub(crate) fn error_message_of(value: &Value) -> String {
    if let Value::Struct(instance) = value
        && instance.type_name.as_ref() == "Error"
        && let Some(Value::Str(message)) = instance.fields.get(1)
    {
        message.to_string()
    } else {
        format!("{value:?}")
    }
}

fn eval_struct_init(
    struct_name: &Arc<str>,
    args: &[Arg],
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    let field_names: Vec<Arc<str>> = if let Some(builtin) = builtin_struct_field_names(struct_name)
    {
        builtin.iter().map(|s| Arc::from(*s)).collect()
    } else {
        let decl = program
            .structs
            .get(struct_name.as_ref())
            .unwrap_or_else(|| {
                unreachable!("already type-checked, so the struct declaration must exist")
            });
        decl.fields.iter().map(|f| Arc::clone(&f.name)).collect()
    };
    let mut fields: Vec<Option<Value>> = (0..field_names.len()).map(|_| None).collect();
    for arg in args {
        let name = arg.name.as_ref().unwrap_or_else(|| {
            unreachable!("struct construction requires named arguments (D-TYPE-13)")
        });
        let idx = field_names
            .iter()
            .position(|f| f.as_ref() == name.as_ref())
            .unwrap_or_else(|| unreachable!("already type-checked, so the field name must match"));
        let v = eval_val!(&arg.value, env, program);
        fields[idx] = Some(v);
    }
    let fields: Vec<Value> = fields
        .into_iter()
        .map(|f| {
            f.unwrap_or_else(|| unreachable!("already type-checked, so every field is supplied"))
        })
        .collect();
    Ok(Flow::Value(Value::Struct(Arc::new(StructInstance {
        type_name: Arc::clone(struct_name),
        fields,
    }))))
}

/// Linearly searches every user-defined enum and returns the `(owning enum name,
/// declaration-order index)` of the variant matching `variant_name` (D-TYPE-07: variant
/// names are unique within the flat namespace).
pub(crate) fn find_variant_in_program(
    program: &Arc<Program>,
    variant_name: &str,
) -> Option<(Arc<str>, u32)> {
    program.enums.values().find_map(|decl| {
        decl.variants
            .iter()
            .position(|v| v.name.as_ref() == variant_name)
            .map(|idx| (Arc::clone(&decl.name), u32::try_from(idx).unwrap_or(0)))
    })
}

/// Resolves `(type_name, variant_index)` from the non-unit variant name of a built-in enum
/// (Result/Option/Value) (D-TYPE-09/D-TYPE-10). `None`/`Null` are unit variants and are
/// handled on the `ExprKind::Ident` side instead (`eval_ident` in `eval/expr.rs`), not here.
fn builtin_variant_info(variant_name: &str) -> Option<(&'static str, u32)> {
    match variant_name {
        "Ok" => Some(("Result", 0)),
        "Err" => Some(("Result", 1)),
        "Some" => Some(("Option", 0)),
        "Bool" => Some(("Value", 1)),
        "Int" => Some(("Value", 2)),
        "Float" => Some(("Value", 3)),
        "Str" => Some(("Value", 4)),
        "List" => Some(("Value", 5)),
        "Dict" => Some(("Value", 6)),
        _ => None,
    }
}

fn eval_enum_variant_init(
    variant_name: &Arc<str>,
    args: &[Arg],
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    let (type_name, variant_index): (Arc<str>, u32) = if let Some((tn, idx)) =
        builtin_variant_info(variant_name.as_ref())
    {
        (Arc::from(tn), idx)
    } else {
        find_variant_in_program(program, variant_name.as_ref())
            .unwrap_or_else(|| unreachable!("already type-checked, so the enum variant must exist"))
    };
    let mut fields = Vec::with_capacity(args.len());
    for arg in args {
        fields.push(eval_val!(&arg.value, env, program));
    }
    Ok(Flow::Value(Value::Enum(Arc::new(EnumInstance {
        type_name,
        variant_index,
        variant_name: Arc::clone(variant_name),
        fields,
    }))))
}

/// D-TYPE-14 (`int`/`float`/`str`) / D-STDPOL-01 (`print`/`eprint`/`assert`) / D-TYPE-03
/// (the `set()` pseudo-constructor) — the only possibility for a Call with no entry in
/// `Resolutions::call_kind` (kept in sync with the early return in `check_call_named` in
/// `types/check_expr.rs`).
fn eval_flat_builtin_call(
    name: &str,
    args: &[Arg],
    span: Span,
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    match name {
        "int" => {
            let v = eval_val!(&args[0].value, env, program);
            let Value::Float(x) = v else {
                unreachable!("already type-checked, so x in int(x) is always float")
            };
            Ok(Flow::Value(crate::stdlib::primitives::int_from_float(
                x, span,
            )?))
        }
        "float" => {
            let v = eval_val!(&args[0].value, env, program);
            let Value::Int(x) = v else {
                unreachable!("already type-checked, so x in float(x) is always int")
            };
            Ok(Flow::Value(crate::stdlib::primitives::float_from_int(x)))
        }
        "str" => {
            let v = eval_val!(&args[0].value, env, program);
            Ok(Flow::Value(crate::stdlib::primitives::str_from_value(&v)))
        }
        "print" => {
            let v = eval_val!(&args[0].value, env, program);
            crate::stdlib::builtins::print(&v);
            Ok(Flow::Value(Value::Void))
        }
        "eprint" => {
            let v = eval_val!(&args[0].value, env, program);
            crate::stdlib::builtins::eprint(&v);
            Ok(Flow::Value(Value::Void))
        }
        "assert" => {
            let cond_expr = &args[0].value;
            let cond_v = eval_val!(cond_expr, env, program);
            let Value::Bool(cond) = cond_v else {
                unreachable!("already type-checked, so assert's first argument is always bool")
            };
            let result = if let Some(msg_arg) = args.get(1) {
                let msg_v = eval_val!(&msg_arg.value, env, program);
                let Value::Str(msg) = msg_v else {
                    unreachable!("already type-checked, so assert's second argument is always str")
                };
                crate::stdlib::builtins::assert_with_message(cond, &msg, span)?
            } else {
                let source_text = program.sources.slice(cond_expr.span);
                crate::stdlib::builtins::assert_bare(cond, source_text, span)?
            };
            Ok(Flow::Value(result))
        }
        "set" => Ok(Flow::Value(Value::Set(Arc::new(IndexSet::new())))),
        _ => unreachable!(
            "a Call with no call_kind is always one of the fixed set above (kept in sync with check_call_named in types/check_expr.rs)"
        ),
    }
}

// =========================================================================
// MethodCall expressions (namespace functions / built-in type methods / user struct methods)
// =========================================================================

/// Whether the receiver expression can be resolved to a writable place.
fn is_lvalue_shaped(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Ident(_) => true,
        ExprKind::FieldAccess { target, .. }
        | ExprKind::TupleIndex { target, .. }
        | ExprKind::Index { target, .. }
        | ExprKind::Grouping(target) => is_lvalue_shaped(target),
        _ => false,
    }
}

fn lvalue_root_is_local(expr: &Expr, env: &Environment) -> bool {
    match &expr.kind {
        ExprKind::Ident(name) => env.try_lookup(name.as_ref()).is_some(),
        ExprKind::FieldAccess { target, .. }
        | ExprKind::TupleIndex { target, .. }
        | ExprKind::Index { target, .. }
        | ExprKind::Grouping(target) => lvalue_root_is_local(target, env),
        _ => false,
    }
}

/// Evaluation of `ExprKind::MethodCall`.
pub fn eval_method_call(
    expr: &Expr,
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
        ..
    } = &expr.kind
    else {
        unreachable!("eval_method_call is only ever called for a MethodCall")
    };

    if let ExprKind::Ident(_) = &receiver.kind
        && let Some(&ns) = program.resolutions.namespace_ref.get(&receiver.id)
    {
        return eval_namespace_call(ns, method.as_ref(), args, expr, env, program);
    }

    if is_lvalue_shaped(receiver) && lvalue_root_is_local(receiver, env) {
        let path = match super::lvalue::capture_place(receiver, env, program)? {
            super::lvalue::PlaceCapture::Path(path) => path,
            super::lvalue::PlaceCapture::Return(value) => return Ok(Flow::Return(value)),
        };
        let snapshot = super::lvalue::resolve_captured_place(&path, env, program, false)?.clone();
        if let Some(mutation) = classify_mutation(&snapshot, method.as_ref(), program) {
            return eval_mutating_method(expr, path, mutation, snapshot, args, env, program);
        }
        return eval_readonly_method(expr, snapshot, method.as_ref(), args, env, program);
    }

    let receiver_v = eval_val!(receiver, env, program);
    eval_readonly_method(expr, receiver_v, method.as_ref(), args, env, program)
}

/// The kind of a destructive method call. `Struct` holds the actual `var self` method
/// declaration (borrowed from `program`, not cloned).
enum Mutation<'p> {
    List,
    Dict,
    Set,
    Struct(&'p FunctionDecl),
}

/// From the receiver's snapshot and the `method` name, determines whether this is a
/// destructive method call. list/dict/set are judged against a fixed set of destructive
/// method names (STDLIB.md), while struct is judged by whether a matching `var self` method
/// actually exists.
fn classify_mutation<'p>(
    snapshot: &Value,
    method: &str,
    program: &'p Program,
) -> Option<Mutation<'p>> {
    match snapshot {
        Value::List(_)
            if matches!(
                method,
                "push" | "pop" | "insert" | "remove" | "extend" | "clear" | "shuffle"
            ) =>
        {
            Some(Mutation::List)
        }
        Value::Dict(_) if matches!(method, "insert" | "remove" | "clear") => Some(Mutation::Dict),
        Value::Set(_) if matches!(method, "insert" | "remove" | "clear") => Some(Mutation::Set),
        Value::Struct(inst) => {
            let decl = program.structs.get(inst.type_name.as_ref())?;
            let m = decl.methods.iter().find(|m| m.name.as_ref() == method)?;
            let mutable = m.self_param.as_ref().is_some_and(|p| p.mutable);
            if mutable {
                Some(Mutation::Struct(m))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn eval_mutating_method(
    call_expr: &Expr,
    path: super::lvalue::PlacePath,
    mutation: Mutation<'_>,
    mut snapshot: Value,
    args: &[Arg],
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    // Arguments are all evaluated up front here (propagating a bare `?`'s Flow::Return,
    // D-ERR-01/02) — the actual mutating operation (list_mutate etc.) and the write-back
    // happen only once argument evaluation has completed normally.
    let mut arg_values = Vec::with_capacity(args.len());
    for a in args {
        arg_values.push(eval_val!(&a.value, env, program));
    }
    let ExprKind::MethodCall { method, .. } = &call_expr.kind else {
        unreachable!("eval_mutating_method is only ever called for a MethodCall")
    };
    let result = match mutation {
        Mutation::List => {
            let Value::List(ref mut arc) = snapshot else {
                unreachable!(
                    "classify_mutation judged this List, so snapshot is always Value::List"
                )
            };
            list_mutate(arc, method.as_ref(), arg_values, call_expr.span)?
        }
        Mutation::Dict => {
            let Value::Dict(ref mut arc) = snapshot else {
                unreachable!(
                    "classify_mutation judged this Dict, so snapshot is always Value::Dict"
                )
            };
            dict_mutate(arc, method.as_ref(), arg_values)
        }
        Mutation::Set => {
            let Value::Set(ref mut arc) = snapshot else {
                unreachable!("classify_mutation judged this Set, so snapshot is always Value::Set")
            };
            set_mutate(arc, method.as_ref(), arg_values)
        }
        Mutation::Struct(decl) => {
            let (result, new_self) = call_method_with_self(decl, snapshot, arg_values, program)?;
            snapshot = new_self;
            result
        }
    };
    let place = super::lvalue::resolve_captured_place(&path, env, program, false)?;
    *place = snapshot;
    Ok(Flow::Value(result))
}

/// The body of list[T]'s destructive methods (STDLIB.md §2.1). Arguments consume an
/// already-evaluated `Vec<Value>` (the caller `eval_mutating_method` has already handled
/// propagation of a bare `?`, so this is pure value transformation).
fn list_mutate(
    arc: &mut Arc<Vec<Value>>,
    method: &str,
    mut args: Vec<Value>,
    span: Span,
) -> Result<Value, Abort> {
    Ok(match method {
        "push" => {
            crate::stdlib::collections::list_push(arc, args.remove(0));
            Value::Void
        }
        "pop" => crate::stdlib::collections::list_pop(arc),
        "insert" => {
            let Value::Int(i) = args[0] else {
                unreachable!("already type-checked")
            };
            crate::stdlib::collections::list_insert(arc, i, args.remove(1), span)?
        }
        "remove" => {
            let Value::Int(i) = args[0] else {
                unreachable!("already type-checked")
            };
            crate::stdlib::collections::list_remove(arc, i, span)?
        }
        "extend" => {
            let Value::List(other) = &args[0] else {
                unreachable!("already type-checked")
            };
            crate::stdlib::collections::list_extend(arc, other);
            Value::Void
        }
        "clear" => {
            crate::stdlib::collections::list_clear(arc);
            Value::Void
        }
        "shuffle" => {
            crate::stdlib::rand::shuffle(arc);
            Value::Void
        }
        _ => unreachable!("matches the set of destructive list names classify_mutation judged"),
    })
}

/// The body of dict[K,V]'s destructive methods (STDLIB.md §2.2).
fn dict_mutate(arc: &mut Arc<IndexMap<MapKey, Value>>, method: &str, args: Vec<Value>) -> Value {
    match method {
        "insert" => {
            let key = MapKey::try_from_value(&args[0]).unwrap_or_else(|| {
                unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
            });
            crate::stdlib::collections::dict_insert(arc, key, args[1].clone())
        }
        "remove" => {
            let key = MapKey::try_from_value(&args[0]).unwrap_or_else(|| {
                unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
            });
            crate::stdlib::collections::dict_remove(arc, &key)
        }
        "clear" => {
            crate::stdlib::collections::dict_clear(arc);
            Value::Void
        }
        _ => unreachable!("matches the set of destructive dict names classify_mutation judged"),
    }
}

/// The body of set[T]'s destructive methods (STDLIB.md §2.3).
fn set_mutate(arc: &mut Arc<IndexSet<MapKey>>, method: &str, args: Vec<Value>) -> Value {
    match method {
        "insert" => {
            let key = MapKey::try_from_value(&args[0]).unwrap_or_else(|| {
                unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
            });
            crate::stdlib::collections::set_insert(arc, key)
        }
        "remove" => {
            let key = MapKey::try_from_value(&args[0]).unwrap_or_else(|| {
                unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
            });
            crate::stdlib::collections::set_remove(arc, &key)
        }
        "clear" => {
            crate::stdlib::collections::set_clear(arc);
            Value::Void
        }
        _ => unreachable!("matches the set of destructive set names classify_mutation judged"),
    }
}

// =========================================================================
// Read-only method calls (struct/Result/Option/Value/str/int/float/list/dict/set)
// =========================================================================

fn eval_readonly_method(
    call_expr: &Expr,
    receiver_v: Value,
    method: &str,
    args: &[Arg],
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    let mut arg_values = Vec::with_capacity(args.len());
    for a in args {
        arg_values.push(eval_val!(&a.value, env, program));
    }
    let result = dispatch_readonly(call_expr, receiver_v, method, arg_values, program)?;
    Ok(Flow::Value(result))
}

fn as_closure(v: Value) -> Arc<Closure> {
    let Value::Closure(c) = v else {
        unreachable!(
            "already type-checked, so the actual argument for a function-typed parameter is always Closure"
        )
    };
    c
}

fn dispatch_readonly(
    call_expr: &Expr,
    receiver_v: Value,
    method: &str,
    args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    match receiver_v {
        Value::Struct(inst) => {
            let decl = program
                .structs
                .get(inst.type_name.as_ref())
                .unwrap_or_else(|| unreachable!("already type-checked"));
            let m = decl
                .methods
                .iter()
                .find(|m| m.name.as_ref() == method)
                .unwrap_or_else(|| unreachable!("already type-checked"));
            let (result, _) = call_method_with_self(m, Value::Struct(inst), args, program)?;
            Ok(result)
        }
        Value::Enum(inst) if inst.type_name.as_ref() == "Result" => {
            result_method(&inst, method, args, call_expr.span, program)
        }
        Value::Enum(inst) if inst.type_name.as_ref() == "Option" => {
            option_method(&inst, method, args, call_expr.span, program)
        }
        Value::Enum(inst) if inst.type_name.as_ref() == "Value" => {
            Ok(value_method(&inst, method, args))
        }
        Value::Str(s) => str_method(call_expr, &s, method, args, program),
        Value::Int(n) if method == "to_str" => Ok(crate::stdlib::primitives::int_to_str(n)),
        Value::Float(n) if method == "to_str" => Ok(crate::stdlib::primitives::float_to_str(n)),
        Value::List(xs) => list_method_readonly(call_expr, &xs, method, args, program),
        Value::Dict(m) => dict_method_readonly(&m, method, args, program),
        Value::Set(s) => set_method_readonly(call_expr, &s, method, args, program),
        _ => unreachable!(
            "already type-checked, so MethodCall only ever appears on a type that supports method calls"
        ),
    }
}

fn result_method(
    inst: &EnumInstance,
    method: &str,
    mut args: Vec<Value>,
    span: Span,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    use crate::stdlib::result_option as ro;
    Ok(match method {
        "is_ok" => ro::is_ok(inst),
        "is_err" => ro::is_err(inst),
        "ok" => ro::result_ok(inst),
        "err" => ro::result_err(inst),
        "unwrap" => ro::result_unwrap(inst, span)?,
        "unwrap_or" => ro::result_unwrap_or(inst, args.remove(0)),
        "unwrap_or_else" => {
            let f = as_closure(args.remove(0));
            ro::result_unwrap_or_else(inst, &f, program)?
        }
        "map" => {
            let f = as_closure(args.remove(0));
            ro::result_map(inst, &f, program)?
        }
        "map_err" => {
            let f = as_closure(args.remove(0));
            ro::result_map_err(inst, &f, program)?
        }
        "and_then" => {
            let f = as_closure(args.remove(0));
            ro::result_and_then(inst, &f, program)?
        }
        _ => unreachable!("already type-checked, so a Result method name is one of STDLIB.md §3.1"),
    })
}

fn option_method(
    inst: &EnumInstance,
    method: &str,
    mut args: Vec<Value>,
    span: Span,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    use crate::stdlib::result_option as ro;
    Ok(match method {
        "is_some" => ro::is_some(inst),
        "is_none" => ro::is_none(inst),
        "unwrap" => ro::option_unwrap(inst, span)?,
        "unwrap_or" => ro::option_unwrap_or(inst, args.remove(0)),
        "unwrap_or_else" => {
            let f = as_closure(args.remove(0));
            ro::option_unwrap_or_else(inst, &f, program)?
        }
        "map" => {
            let f = as_closure(args.remove(0));
            ro::option_map(inst, &f, program)?
        }
        "and_then" => {
            let f = as_closure(args.remove(0));
            ro::option_and_then(inst, &f, program)?
        }
        "filter" => {
            let f = as_closure(args.remove(0));
            ro::option_filter(inst, &f, program)?
        }
        "ok_or" => ro::option_ok_or(inst, args.remove(0)),
        _ => {
            unreachable!("already type-checked, so an Option method name is one of STDLIB.md §3.2")
        }
    })
}

fn value_method(inst: &EnumInstance, method: &str, mut args: Vec<Value>) -> Value {
    use crate::stdlib::value_type as vt;
    match method {
        "as_int" => vt::as_int(inst),
        "as_float" => vt::as_float(inst),
        "as_str" => vt::as_str(inst),
        "as_bool" => vt::as_bool(inst),
        "as_list" => vt::as_list(inst),
        "as_dict" => vt::as_dict(inst),
        "is_null" => vt::is_null(inst),
        "get" => {
            let Value::Str(k) = args.remove(0) else {
                unreachable!("already type-checked")
            };
            vt::value_get(inst, &k)
        }
        "at" => {
            let Value::Int(i) = args.remove(0) else {
                unreachable!("already type-checked")
            };
            vt::value_at(inst, i)
        }
        _ => unreachable!("already type-checked, so a Value method name is one of STDLIB.md §3.4"),
    }
}

/// str methods. Per D-COL-03, the iterator-style ones (map/filter/fold/find_by/any/all/
/// enumerate/zip/rev/take/skip/flat_map/sort_by/chain) are converted into the list[str]
/// equivalent of `.chars()` and then delegated to the generic list implementation
/// (`list_method_readonly`) (per the policy noted in primitives.rs's comments).
fn str_method(
    call_expr: &Expr,
    s: &Arc<str>,
    method: &str,
    mut args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    use crate::stdlib::primitives as prim;
    Ok(match method {
        "len" | "count" => prim::str_len(s),
        "get" => {
            let Value::Int(i) = args.remove(0) else {
                unreachable!("already type-checked")
            };
            prim::str_get(s, i)
        }
        "bytes" => prim::str_bytes(s),
        "chars" => prim::str_chars(s),
        "trim" => prim::str_trim(s),
        "trim_start" => prim::str_trim_start(s),
        "trim_end" => prim::str_trim_end(s),
        "to_upper" => prim::str_to_upper(s),
        "to_lower" => prim::str_to_lower(s),
        "is_empty" => prim::str_is_empty(s),
        "to_str" => prim::str_to_str(s),
        "contains" => {
            let Value::Str(needle) = &args[0] else {
                unreachable!("already type-checked")
            };
            prim::str_contains(s, needle)
        }
        "starts_with" => {
            let Value::Str(p) = &args[0] else {
                unreachable!("already type-checked")
            };
            prim::str_starts_with(s, p)
        }
        "ends_with" => {
            let Value::Str(p) = &args[0] else {
                unreachable!("already type-checked")
            };
            prim::str_ends_with(s, p)
        }
        "replace" => {
            let Value::Str(from) = &args[0] else {
                unreachable!("already type-checked")
            };
            let Value::Str(to) = &args[1] else {
                unreachable!("already type-checked")
            };
            prim::str_replace(s, from, to)
        }
        "repeat" => {
            let Value::Int(n) = args[0] else {
                unreachable!("already type-checked")
            };
            prim::str_repeat(s, n)
        }
        "find" => {
            let Value::Str(needle) = &args[0] else {
                unreachable!("already type-checked")
            };
            prim::str_find(s, needle)
        }
        "slice" => {
            let Value::Int(a) = args[0] else {
                unreachable!("already type-checked")
            };
            let Value::Int(b) = args[1] else {
                unreachable!("already type-checked")
            };
            prim::str_slice(s, a, b, call_expr.span)?
        }
        "parse_int" => prim::str_parse_int(s),
        "parse_float" => prim::str_parse_float(s),
        "split" => {
            let Value::Str(sep) = &args[0] else {
                unreachable!("already type-checked")
            };
            prim::str_split(s, sep)
        }
        "map" | "filter" | "fold" | "find_by" | "any" | "all" | "enumerate" | "rev" | "take"
        | "skip" | "flat_map" | "sort_by" | "zip" | "chain" => {
            let Value::List(chars) = prim::str_chars(s) else {
                unreachable!("str_chars always returns list[str]")
            };
            if matches!(method, "zip" | "chain") {
                let Value::Str(other) = &args[0] else {
                    unreachable!("string zip/chain accepts another string")
                };
                args[0] = prim::str_chars(other);
            }
            let delegated = if method == "find_by" { "find" } else { method };
            list_method_readonly(call_expr, &chars, delegated, args, program)?
        }
        _ => unreachable!("method name was already type-checked"),
    })
}

/// list[T]'s methods (STDLIB.md §2.1; the destructive ones are in `list_mutate`).
fn list_method_readonly(
    call_expr: &Expr,
    xs: &Arc<Vec<Value>>,
    method: &str,
    mut args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    use crate::stdlib::collections as col;
    Ok(match method {
        "map" => col::list_map(xs, &as_closure(args.remove(0)), program)?,
        "filter" => col::list_filter(xs, &as_closure(args.remove(0)), program)?,
        "fold" => {
            let f = as_closure(args.remove(1));
            let init = args.remove(0);
            col::list_fold(xs, init, &f, program)?
        }
        "find" => col::list_find(xs, &as_closure(args.remove(0)), program)?,
        "any" => col::list_any(xs, &as_closure(args.remove(0)), program)?,
        "all" => col::list_all(xs, &as_closure(args.remove(0)), program)?,
        "count" | "len" => col::list_len(xs),
        "sum" => {
            let empty_is_float = if let ExprKind::MethodCall { receiver, .. } = &call_expr.kind {
                matches!(
                    program.resolutions.expr_ty.get(&receiver.id),
                    Some(Ty::List(element)) if matches!(element.as_ref(), Ty::Float)
                )
            } else {
                false
            };
            col::list_sum(xs, empty_is_float, call_expr.span)?
        }
        "enumerate" => col::list_enumerate(xs),
        "zip" => {
            let Value::List(other) = args.remove(0) else {
                unreachable!("already type-checked")
            };
            col::list_zip(xs, &other)
        }
        "rev" => col::list_rev(xs),
        "take" => {
            let Value::Int(n) = args[0] else {
                unreachable!("already type-checked")
            };
            col::list_take(xs, n)
        }
        "skip" => {
            let Value::Int(n) = args[0] else {
                unreachable!("already type-checked")
            };
            col::list_skip(xs, n)
        }
        "flat_map" => col::list_flat_map(xs, &as_closure(args.remove(0)), program)?,
        "sort_by" => col::list_sort_by(xs, &as_closure(args.remove(0)), program)?,
        "chain" => {
            let Value::List(other) = args.remove(0) else {
                unreachable!("already type-checked")
            };
            col::list_chain(xs, &other)
        }
        "get" => {
            let Value::Int(i) = args[0] else {
                unreachable!("already type-checked")
            };
            col::list_get(xs, i)
        }
        "is_empty" => col::list_is_empty(xs),
        "contains" => col::list_contains(xs, &args[0]),
        "first" => col::list_first(xs),
        "last" => col::list_last(xs),
        "join" => {
            let Value::Str(sep) = &args[0] else {
                unreachable!("already type-checked")
            };
            col::list_join(xs, sep)
        }
        "slice" => {
            let Value::Int(a) = args[0] else {
                unreachable!("already type-checked")
            };
            let Value::Int(b) = args[1] else {
                unreachable!("already type-checked")
            };
            col::list_slice(xs, a, b, call_expr.span)?
        }
        "to_set" => col::list_to_set(xs),
        "each" => col::list_each(xs, &as_closure(args.remove(0)), program)?,
        "par_map" => {
            let f = as_closure(args.remove(0));
            let results = crate::concurrency::eval_par_map(xs.as_ref().clone(), &f, program)?;
            Value::List(Arc::new(results))
        }
        "par_each" => {
            let f = as_closure(args.remove(0));
            let _ = crate::concurrency::eval_par_map(xs.as_ref().clone(), &f, program)?;
            Value::Void
        }
        _ => unreachable!("already type-checked, so a list method name is one of STDLIB.md §2.1"),
    })
}

/// dict[K,V]'s methods (STDLIB.md §2.2). The higher-order methods (map/filter/any/all/find/
/// fold/each) wire through to the corresponding implementation in `collections.rs`,
/// passing `program` (the same shape as `list_method_readonly` for list[T], resolving here
/// something left as a handoff back in Unit 11).
fn dict_method_readonly(
    m: &Arc<IndexMap<MapKey, Value>>,
    method: &str,
    mut args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    use crate::stdlib::collections as col;
    Ok(match method {
        "get" => {
            let key = MapKey::try_from_value(&args[0]).unwrap_or_else(|| {
                unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
            });
            col::dict_get(m, &key)
        }
        "contains_key" => {
            let key = MapKey::try_from_value(&args[0]).unwrap_or_else(|| {
                unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
            });
            col::dict_contains_key(m, &key)
        }
        "keys" => col::dict_keys(m),
        "values" => col::dict_values(m),
        "entries" => col::dict_entries(m),
        "len" => col::dict_len(m),
        "is_empty" => Value::Bool(m.is_empty()),
        "map" => col::dict_map(m, &as_closure(args.remove(0)), program)?,
        "filter" => col::dict_filter(m, &as_closure(args.remove(0)), program)?,
        "any" => col::dict_any(m, &as_closure(args.remove(0)), program)?,
        "all" => col::dict_all(m, &as_closure(args.remove(0)), program)?,
        "find" => col::dict_find(m, &as_closure(args.remove(0)), program)?,
        "fold" => {
            let f = as_closure(args.remove(1));
            let init = args.remove(0);
            col::dict_fold(m, init, &f, program)?
        }
        "each" => col::dict_each(m, &as_closure(args.remove(0)), program)?,
        _ => unreachable!("already type-checked, so a dict method name is one of STDLIB.md §2.2"),
    })
}

/// set[T]'s methods (STDLIB.md §2.3). The higher-order methods (map/filter/any/all/find/
/// fold/each) and `sum` wire through to the corresponding implementation in
/// `collections.rs`, passing `program` (resolving, the same as dict, something left as a
/// handoff back in Unit 11).
fn set_method_readonly(
    call_expr: &Expr,
    s: &Arc<IndexSet<MapKey>>,
    method: &str,
    mut args: Vec<Value>,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    use crate::stdlib::collections as col;
    Ok(match method {
        "contains" => {
            let key = MapKey::try_from_value(&args[0]).unwrap_or_else(|| {
                unreachable!("already type-checked, so only D-TYPE-05's allowed key types occur")
            });
            col::set_contains(s, &key)
        }
        "len" | "count" => col::set_len(s),
        "is_empty" => Value::Bool(s.is_empty()),
        "union" => {
            let Value::Set(other) = &args[0] else {
                unreachable!("already type-checked")
            };
            col::set_union(s, other)
        }
        "intersection" => {
            let Value::Set(other) = &args[0] else {
                unreachable!("already type-checked")
            };
            col::set_intersection(s, other)
        }
        "difference" => {
            let Value::Set(other) = &args[0] else {
                unreachable!("already type-checked")
            };
            col::set_difference(s, other)
        }
        "to_list" => col::set_to_list(s),
        "sum" => col::set_sum(s, call_expr.span)?,
        "map" => col::set_map(s, &as_closure(args.remove(0)), program)?,
        "filter" => col::set_filter(s, &as_closure(args.remove(0)), program)?,
        "any" => col::set_any(s, &as_closure(args.remove(0)), program)?,
        "all" => col::set_all(s, &as_closure(args.remove(0)), program)?,
        "find" => col::set_find(s, &as_closure(args.remove(0)), program)?,
        "fold" => {
            let f = as_closure(args.remove(1));
            let init = args.remove(0);
            col::set_fold(s, init, &f, program)?
        }
        "each" => col::set_each(s, &as_closure(args.remove(0)), program)?,
        _ => unreachable!("already type-checked, so a set method name is one of STDLIB.md §2.3"),
    })
}

// =========================================================================
// Namespace function calls (`fs.read` etc., distinguished via `Resolutions::namespace_ref`)
// =========================================================================

fn eval_namespace_call(
    ns: NamespaceId,
    method: &str,
    args: &[Arg],
    call_expr: &Expr,
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    let mut vals = Vec::with_capacity(args.len());
    for a in args {
        vals.push(eval_val!(&a.value, env, program));
    }
    Ok(Flow::Value(dispatch_namespace(
        ns, method, vals, args, call_expr, program,
    )))
}

fn v_str(v: &Value) -> &str {
    let Value::Str(s) = v else {
        unreachable!(
            "already type-checked, so a namespace function's str argument is always Value::Str"
        )
    };
    s
}

fn v_int(v: &Value) -> i64 {
    let Value::Int(n) = v else {
        unreachable!(
            "already type-checked, so a namespace function's int argument is always Value::Int"
        )
    };
    *n
}

fn v_float(v: &Value) -> f64 {
    let Value::Float(n) = v else {
        unreachable!(
            "already type-checked, so a namespace function's float argument is always Value::Float"
        )
    };
    *n
}

/// The actual call to a namespace function's implementation. None of these ever panics
/// (being outside the panic targets D-ERR-04 enumerates), and failure is expressed as an
/// ordinary Yabumi value, `Err(Error)` (borne out by every signature in
/// `stdlib::fs`/`http`/`env`/`proc`/`time`/`rand`/`regex`/`math`/`codec` returning `Value`
/// directly, not `Result<Value, Abort>`).
#[expect(
    clippy::too_many_lines,
    reason = "this holds the wiring for every namespace function STDLIB.md enumerates \
              (fs/http/env/proc/time/rand/regex/math/json/yaml/toml/csv) as a single \
              dispatch table, and since each function needs at least one line to pull out \
              its arguments, splitting it would not improve readability"
)]
fn dispatch_namespace(
    ns: NamespaceId,
    method: &str,
    vals: Vec<Value>,
    arg_exprs: &[Arg],
    call_expr: &Expr,
    program: &Arc<Program>,
) -> Value {
    use crate::stdlib::{envns, fs, http, math, proc, rand, regexns, time};
    match (ns, method) {
        (NamespaceId::Fs, "read") => fs::read(v_str(&vals[0])),
        (NamespaceId::Fs, "read_bytes") => fs::read_bytes(v_str(&vals[0])),
        (NamespaceId::Fs, "write") => fs::write(v_str(&vals[0]), v_str(&vals[1])),
        (NamespaceId::Fs, "append") => fs::append(v_str(&vals[0]), v_str(&vals[1])),
        (NamespaceId::Fs, "list") => fs::list(v_str(&vals[0])),
        (NamespaceId::Fs, "exists") => fs::exists(v_str(&vals[0])),
        (NamespaceId::Fs, "remove") => fs::remove(v_str(&vals[0])),

        (NamespaceId::Http, "get") => http::get(v_str(&vals[0])),
        (NamespaceId::Http, "delete") => http::delete(v_str(&vals[0])),
        (NamespaceId::Http, "post") => http::post(v_str(&vals[0]), v_str(&vals[1])),
        (NamespaceId::Http, "put") => http::put(v_str(&vals[0]), v_str(&vals[1])),
        (NamespaceId::Http, "request") => http::request(v_str(&vals[0]), v_str(&vals[1]), &vals[2]),

        (NamespaceId::Env, "get") => envns::get(v_str(&vals[0])),
        (NamespaceId::Env, "set") => {
            envns::set(v_str(&vals[0]), v_str(&vals[1]));
            Value::Void
        }
        (NamespaceId::Env, "args") => envns::args(),
        (NamespaceId::Env, "stdin") => envns::stdin(),

        (NamespaceId::Proc, "run") => {
            let Value::List(xs) = &vals[1] else {
                unreachable!("already type-checked")
            };
            proc::run(v_str(&vals[0]), xs)
        }
        (NamespaceId::Time, "now") => time::now(),
        (NamespaceId::Time, "sleep") => {
            time::sleep(v_int(&vals[0]));
            Value::Void
        }
        (NamespaceId::Time, "format") => time::format(v_int(&vals[0]), v_str(&vals[1])),
        (NamespaceId::Time, "parse") => time::parse(v_str(&vals[0]), v_str(&vals[1])),

        (NamespaceId::Rand, "int") => rand::int(v_int(&vals[0]), v_int(&vals[1])),
        (NamespaceId::Rand, "float") => rand::float(),
        (NamespaceId::Rand, "bool") => rand::bool_(),
        (NamespaceId::Rand, "choice") => {
            let Value::List(xs) = &vals[0] else {
                unreachable!("already type-checked")
            };
            rand::choice(xs)
        }

        (NamespaceId::Regex, "is_match") => regexns::is_match(v_str(&vals[0]), v_str(&vals[1])),
        (NamespaceId::Regex, "find") => regexns::find(v_str(&vals[0]), v_str(&vals[1])),
        (NamespaceId::Regex, "find_all") => regexns::find_all(v_str(&vals[0]), v_str(&vals[1])),
        (NamespaceId::Regex, "replace") => {
            regexns::replace(v_str(&vals[0]), v_str(&vals[1]), v_str(&vals[2]))
        }
        (NamespaceId::Regex, "replace_all") => {
            regexns::replace_all(v_str(&vals[0]), v_str(&vals[1]), v_str(&vals[2]))
        }
        (NamespaceId::Regex, "captures") => regexns::captures(v_str(&vals[0]), v_str(&vals[1])),

        (NamespaceId::Math, "checked_div") => math::checked_div(v_int(&vals[0]), v_int(&vals[1])),
        (NamespaceId::Math, "checked_mod") => math::checked_mod(v_int(&vals[0]), v_int(&vals[1])),
        (NamespaceId::Math, "checked_add") => math::checked_add(v_int(&vals[0]), v_int(&vals[1])),
        (NamespaceId::Math, "checked_sub") => math::checked_sub(v_int(&vals[0]), v_int(&vals[1])),
        (NamespaceId::Math, "checked_mul") => math::checked_mul(v_int(&vals[0]), v_int(&vals[1])),
        (NamespaceId::Math, "abs_int") => math::abs_int(v_int(&vals[0])),
        (NamespaceId::Math, "abs_float") => math::abs_float(v_float(&vals[0])),
        (NamespaceId::Math, "sqrt") => math::sqrt(v_float(&vals[0])),
        (NamespaceId::Math, "min_int") => math::min_int(v_int(&vals[0]), v_int(&vals[1])),
        (NamespaceId::Math, "max_int") => math::max_int(v_int(&vals[0]), v_int(&vals[1])),
        (NamespaceId::Math, "min_float") => math::min_float(v_float(&vals[0]), v_float(&vals[1])),
        (NamespaceId::Math, "max_float") => math::max_float(v_float(&vals[0]), v_float(&vals[1])),
        (NamespaceId::Math, "pow") => math::pow(v_float(&vals[0]), v_float(&vals[1])),
        (NamespaceId::Math, "floor") => math::floor(v_float(&vals[0])),
        (NamespaceId::Math, "ceil") => math::ceil(v_float(&vals[0])),
        (NamespaceId::Math, "round") => math::round(v_float(&vals[0])),

        (NamespaceId::Json | NamespaceId::Yaml | NamespaceId::Toml, "decode") => {
            let target = decode_target_of(call_expr, program);
            crate::stdlib::codec::decode(ns, &target, v_str(&vals[0]), program)
        }
        (NamespaceId::Json | NamespaceId::Yaml | NamespaceId::Toml, "encode") => {
            crate::stdlib::codec::encode(ns, &vals[0], program)
        }
        (NamespaceId::Csv, "decode") => {
            let target = decode_target_of(call_expr, program);
            let Ty::Named { name, .. } = &target else {
                unreachable!("D-STDPOL: csv.decode's target type is always a struct")
            };
            let decl = program
                .structs
                .get(name.as_ref())
                .unwrap_or_else(|| unreachable!("already type-checked"));
            crate::stdlib::codec::csv::decode(v_str(&vals[0]), decl)
        }
        (NamespaceId::Csv, "encode") => {
            let Value::List(rows) = &vals[0] else {
                unreachable!("already type-checked")
            };
            // The target struct is normally determined at runtime from the first element
            // of rows (list[T]) (every element shares one type, D-TYPE-04). An empty list
            // cannot determine T this way, but when `rows` is directly the call's own
            // argument expression (`csv.encode(xs)`, the standard form STDLIB.md §4.2
            // shows), the static type of `rows` itself as list[T] can be obtained directly
            // from `Resolutions::expr_ty` (already recorded by the type-checking phase for
            // every argument expression via `check_positional_call`, the same shape as the
            // precedent where `toml.encode`'s D-STDPOL-09 check reads this same field, a
            // judgment call made in this file), so the struct declaration (→ the header
            // row) can be recovered even when the runtime list is empty (the input
            // equivalent to csv.encode([]) is an edge case STDLIB.md does not specify, but
            // as long as the static type is known, csv::encode itself can happily produce
            // a header-only CSV).
            let decl = rows
                .first()
                .and_then(|v| {
                    let Value::Struct(inst) = v else {
                        unreachable!(
                            "already type-checked, so a csv.encode element is always a struct"
                        )
                    };
                    program.structs.get(inst.type_name.as_ref())
                })
                .or_else(|| csv_encode_empty_target_decl(arg_exprs, program))
                .or_else(|| csv_encode_pipe_target_decl(call_expr, program));
            let Some(decl) = decl else {
                unreachable!("type checking records the CSV row type for empty inputs")
            };
            Value::Str(Arc::from(crate::stdlib::codec::csv::encode(rows, decl)))
        }
        (NamespaceId::Csv, "decode_rows") => {
            crate::stdlib::codec::csv::decode_rows(v_str(&vals[0]))
        }

        _ => unreachable!(
            "already type-checked, so a namespace function call is always one of the combinations enumerated in STDLIB.md"
        ),
    }
}

fn decode_target_of(call_expr: &Expr, program: &Arc<Program>) -> Ty {
    program
        .resolutions
        .decode_target
        .get(&call_expr.id)
        .cloned()
        .unwrap_or_else(|| {
            unreachable!("already type-checked (D-TYPE-16), so decode_target must exist")
        })
}

/// When `rows` in `csv.encode(rows)` (the direct-call form) turns out to be an empty list
/// at runtime, this recovers the target struct declaration from the static type.
/// `check_positional_call` (`types/check_expr.rs`) runs the usual `check_expr` on every
/// non-placeholder argument expression, so `rows`'s own `list[T]` is recorded in
/// `Resolutions::expr_ty` (the same shape as the precedent where `toml.encode`'s D-STDPOL-09
/// check reads this same field, a judgment call made in this file). Returns `None` if
/// `arg_exprs` is empty (there is no corresponding Arg via a pipe call, see the comment
/// above).
fn csv_encode_empty_target_decl<'p>(
    arg_exprs: &[Arg],
    program: &'p Program,
) -> Option<&'p Arc<crate::ast::StructDecl>> {
    let rows_expr = arg_exprs.first()?;
    let Ty::List(elem_ty) = program.resolutions.expr_ty.get(&rows_expr.value.id)? else {
        return None;
    };
    let Ty::Named { name, .. } = elem_ty.as_ref() else {
        return None;
    };
    program.structs.get(name.as_ref())
}

fn csv_encode_pipe_target_decl<'program>(
    call_expr: &Expr,
    program: &'program Program,
) -> Option<&'program Arc<crate::ast::StructDecl>> {
    let Ty::Named { name, .. } = program.resolutions.csv_encode_target.get(&call_expr.id)? else {
        return None;
    };
    program.structs.get(name.as_ref())
}

// =========================================================================
// Pipe (`|>`) stage calls (used by `eval_pipe` in `eval/expr.rs`)
// =========================================================================

/// `x |> callee` (no arguments, implicitly passing the single input value).
pub(crate) fn invoke_pipe_bare(
    callee_expr: &Expr,
    input: Value,
    env: &mut Environment,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    invoke_pipe_call(callee_expr, vec![input], env, program)
}

/// `x |> callee(a, _, b)` (substituting `_` with input, evaluating everything else normally).
pub(crate) fn invoke_pipe_with_args(
    callee_expr: &Expr,
    args: &[Arg],
    input: &Value,
    env: &mut Environment,
    program: &Arc<Program>,
) -> super::EvalResult {
    let mut values = Vec::with_capacity(args.len());
    for arg in args {
        if arg.is_placeholder {
            values.push(input.clone());
        } else {
            values.push(eval_val!(&arg.value, env, program));
        }
    }
    Ok(Flow::Value(invoke_pipe_call(
        callee_expr,
        values,
        env,
        program,
    )?))
}

fn invoke_pipe_call(
    callee_expr: &Expr,
    args: Vec<Value>,
    env: &mut Environment,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    match &callee_expr.kind {
        ExprKind::FieldAccess { target, field } => {
            if let ExprKind::Ident(_) = &target.kind
                && let Some(&namespace) = program.resolutions.namespace_ref.get(&target.id)
            {
                return Ok(dispatch_namespace(
                    namespace,
                    field,
                    args,
                    &[],
                    callee_expr,
                    program,
                ));
            }
            unreachable!("pipe field targets are namespace functions")
        }
        ExprKind::Ident(name) if is_flat_builtin(name) => {
            invoke_flat_builtin_values(name, args, callee_expr.span, program)
        }
        ExprKind::Ident(name) => {
            if let Some(Value::Closure(closure)) = env.try_lookup(name).cloned() {
                return call_closure(&closure, args, program);
            }
            let declaration = program
                .functions
                .get(name.as_ref())
                .unwrap_or_else(|| unreachable!("type-checked pipe function must exist"));
            call_function(declaration, args, program)
        }
        _ => unreachable!("pipe destinations are callable identifiers or namespaces"),
    }
}

fn is_flat_builtin(name: &str) -> bool {
    matches!(
        name,
        "int" | "float" | "str" | "print" | "eprint" | "assert" | "set"
    )
}

fn invoke_flat_builtin_values(
    name: &str,
    mut args: Vec<Value>,
    span: Span,
    program: &Program,
) -> Result<Value, Abort> {
    Ok(match name {
        "int" => {
            let Value::Float(value) = args.remove(0) else {
                unreachable!("piped int input was type-checked as float")
            };
            crate::stdlib::primitives::int_from_float(value, span)?
        }
        "float" => {
            let Value::Int(value) = args.remove(0) else {
                unreachable!("piped float input was type-checked as int")
            };
            crate::stdlib::primitives::float_from_int(value)
        }
        "str" => crate::stdlib::primitives::str_from_value(&args[0]),
        "print" => {
            crate::stdlib::builtins::print(&args[0]);
            Value::Void
        }
        "eprint" => {
            crate::stdlib::builtins::eprint(&args[0]);
            Value::Void
        }
        "assert" => {
            let Value::Bool(condition) = args[0] else {
                unreachable!("piped assert input was type-checked as bool")
            };
            let source = program.sources.slice(span);
            crate::stdlib::builtins::assert_bare(condition, source, span)?
        }
        "set" => Value::Set(Arc::new(IndexSet::new())),
        _ => unreachable!("checked by is_flat_builtin"),
    })
}

#[cfg(test)]
mod tests {
    use super::{builtin_struct_field_names, builtin_variant_info};
    use crate::stdlib::builtins::test_pipeline::run_ok_source;
    use std::path::PathBuf;

    #[test]
    fn builtin_struct_field_names_matches_stdlib() {
        assert_eq!(
            builtin_struct_field_names("Error"),
            Some(["kind", "message", "cause"].as_slice())
        );
        assert_eq!(builtin_struct_field_names("NotAStruct"), None);
    }

    #[test]
    fn builtin_variant_info_matches_result_option_value() {
        assert_eq!(builtin_variant_info("Ok"), Some(("Result", 0)));
        assert_eq!(builtin_variant_info("Err"), Some(("Result", 1)));
        assert_eq!(builtin_variant_info("Some"), Some(("Option", 0)));
        assert_eq!(builtin_variant_info("Bool"), Some(("Value", 1)));
        assert_eq!(builtin_variant_info("NotAVariant"), None);
    }

    /// Regression test for issue 2: verifies through the full pipeline that
    /// `str_method`'s `trim_start`/`trim_end`/`is_empty`/`to_str` actually call the proper
    /// `stdlib::primitives` functions (`str_trim_start`/`str_trim_end`/`str_is_empty`/
    /// `str_to_str`) rather than an inline Rust implementation. Since `samples/**` cannot be
    /// changed, this passes a source string directly to `run_ok_source`
    /// (`stdlib::builtins::test_pipeline`).
    #[test]
    fn str_trim_and_is_empty_and_to_str_are_wired_to_primitives() {
        let src = r#"
a = "  hi  "
assert(a.trim_start() == "hi  ")
assert(a.trim_end() == "  hi")
assert("".is_empty() == true)
assert("x".is_empty() == false)
assert("hello".to_str() == "hello")
"#;
        let result = run_ok_source(
            "str_trim_is_empty_to_str",
            &PathBuf::from("str_trim_is_empty_to_str.ybm"),
            src,
        );
        assert!(
            result.is_ok(),
            "sample should run without Abort: {result:?}"
        );
    }
}
