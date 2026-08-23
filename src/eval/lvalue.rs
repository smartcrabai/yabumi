//! Assignment-target path resolution (implements D-MUT-03's root-variable tracking as a
//! chain of `Arc::make_mut` calls, ARCHITECTURE.md §3.10).
//!
//! A subscript/destructive method like `u.tags.push("b")` may have `u` pointing at an
//! out-of-range index or a nonexistent dict key — per D-COL-02 (SPEC §7.4), this is a panic
//! target (E6001) that depends on runtime data and cannot be excluded by static checking
//! (type checking). So `resolve_place` cannot unconditionally return `&mut Value`; it
//! returns `Result<&mut Value, Abort>` (the R2 decision, §8).
//!
//! The signature example in ARCHITECTURE.md §3.10 has no `program` argument, but the
//! `Index` subscript expression can be an arbitrary expression (e.g. a function call), and
//! evaluating it requires a reference to `Program`. So a `program: &Arc<Program>` was added
//! here (a judgment call made in this file — this does not change the meaning or behavior
//! of the field itself relative to what ARCHITECTURE.md describes).
//!
//! On resolving field subscripts: `StmtKind::FieldAssign` has no entry in
//! `Resolutions::field_index` (`check_field_assign` in `types/check_stmt.rs` discards the
//! resolved subscript for type-checking purposes and uses only the type — confirmed in this
//! file). The `ExprKind::FieldAccess` side does record one, but to unify both paths into a
//! single implementation, eval never references `field_index` at all and always does a
//! linear search by field name from the runtime `StructInstance::type_name`
//! (`call::field_index_of`, a judgment call made in this file — an O(field count) linear
//! search, but a negligible cost at the struct sizes Yabumi targets).
//!
use super::call::field_index_of;
use super::env::{Environment, Program};
use super::value::{MapKey, Value};
use super::{Abort, Flow, panic};
use crate::ast::{Expr, ExprKind};
use crate::diagnostics::Span;
use std::sync::Arc;

enum PlaceStep {
    Field(Arc<str>),
    TupleIndex(u32, Span),
    Index(Value, Span),
}

pub struct PlacePath {
    root: Arc<str>,
    steps: Vec<PlaceStep>,
}

pub enum PlaceCapture {
    Path(PlacePath),
    Return(Value),
}

pub enum PlaceOutcome<'env> {
    Place(&'env mut Value),
    Return(Value),
}

pub fn capture_place(
    expr: &Expr,
    env: &mut Environment,
    program: &Arc<Program>,
) -> Result<PlaceCapture, Abort> {
    match &expr.kind {
        ExprKind::Ident(name) => Ok(PlaceCapture::Path(PlacePath {
            root: Arc::clone(name),
            steps: Vec::new(),
        })),
        ExprKind::Grouping(inner) => capture_place(inner, env, program),
        ExprKind::FieldAccess { target, field } => Ok(append_step(
            capture_place(target, env, program)?,
            PlaceStep::Field(Arc::clone(field)),
        )),
        ExprKind::TupleIndex { target, index } => Ok(append_step(
            capture_place(target, env, program)?,
            PlaceStep::TupleIndex(*index, expr.span),
        )),
        ExprKind::Index { target, index } => {
            let key = match super::expr::eval_expr(index, env, program)? {
                Flow::Value(value) => value,
                Flow::Return(value) => return Ok(PlaceCapture::Return(value)),
            };
            Ok(append_step(
                capture_place(target, env, program)?,
                PlaceStep::Index(key, expr.span),
            ))
        }
        _ => unreachable!("type checking only permits writable place expressions"),
    }
}

fn append_step(capture: PlaceCapture, step: PlaceStep) -> PlaceCapture {
    match capture {
        PlaceCapture::Path(mut path) => {
            path.steps.push(step);
            PlaceCapture::Path(path)
        }
        PlaceCapture::Return(value) => PlaceCapture::Return(value),
    }
}

pub fn resolve_captured_place<'env>(
    path: &PlacePath,
    env: &'env mut Environment,
    program: &Program,
    allow_final_dict_insert: bool,
) -> Result<&'env mut Value, Abort> {
    let root = env.lookup_mut(&path.root);
    resolve_steps(root, &path.steps, program, allow_final_dict_insert)
}

fn resolve_steps<'value>(
    value: &'value mut Value,
    steps: &[PlaceStep],
    program: &Program,
    allow_final_dict_insert: bool,
) -> Result<&'value mut Value, Abort> {
    let Some((step, remaining)) = steps.split_first() else {
        return Ok(value);
    };
    let child = match step {
        PlaceStep::Field(field) => {
            let Value::Struct(instance) = value else {
                unreachable!("field place receiver was already type-checked as a struct")
            };
            let index = field_index_of(program, &instance.type_name, field);
            &mut Arc::make_mut(instance).fields[index as usize]
        }
        PlaceStep::TupleIndex(index, span) => {
            let Value::Tuple(items) = value else {
                unreachable!("tuple place receiver was already type-checked as a tuple")
            };
            Arc::make_mut(items)
                .get_mut(*index as usize)
                .ok_or_else(|| panic::out_of_range(*span, "tuple index"))?
        }
        PlaceStep::Index(key, span) => match value {
            Value::List(items) => {
                let Value::Int(index) = key else {
                    unreachable!("list places are indexed by int")
                };
                let index = usize::try_from(*index)
                    .ok()
                    .filter(|index| *index < items.len())
                    .ok_or_else(|| panic::out_of_range(*span, "list index"))?;
                &mut Arc::make_mut(items)[index]
            }
            Value::Dict(entries) => {
                let key = MapKey::try_from_value(key)
                    .unwrap_or_else(|| unreachable!("dict place key type was already checked"));
                let entries = Arc::make_mut(entries);
                if remaining.is_empty() && allow_final_dict_insert {
                    entries.entry(key).or_insert(Value::Void)
                } else {
                    entries
                        .get_mut(&key)
                        .ok_or_else(|| panic::out_of_range(*span, "dict key"))?
                }
            }
            _ => unreachable!("index place receiver was already type-checked as list or dict"),
        },
    };
    resolve_steps(child, remaining, program, allow_final_dict_insert)
}

pub fn resolve_place<'env>(
    expr: &Expr,
    env: &'env mut Environment,
    program: &Arc<Program>,
) -> Result<PlaceOutcome<'env>, Abort> {
    match capture_place(expr, env, program)? {
        PlaceCapture::Path(path) => {
            resolve_captured_place(&path, env, program, false).map(PlaceOutcome::Place)
        }
        PlaceCapture::Return(value) => Ok(PlaceOutcome::Return(value)),
    }
}

pub fn resolve_field_place<'env>(
    target: &Expr,
    field: &Arc<str>,
    env: &'env mut Environment,
    program: &Arc<Program>,
) -> Result<PlaceOutcome<'env>, Abort> {
    let capture = append_step(
        capture_place(target, env, program)?,
        PlaceStep::Field(Arc::clone(field)),
    );
    match capture {
        PlaceCapture::Path(path) => {
            resolve_captured_place(&path, env, program, false).map(PlaceOutcome::Place)
        }
        PlaceCapture::Return(value) => Ok(PlaceOutcome::Return(value)),
    }
}

pub fn resolve_index_place<'env>(
    target: &Expr,
    index: &Expr,
    span: Span,
    env: &'env mut Environment,
    program: &Arc<Program>,
) -> Result<PlaceOutcome<'env>, Abort> {
    let key = match super::expr::eval_expr(index, env, program)? {
        Flow::Value(value) => value,
        Flow::Return(value) => return Ok(PlaceOutcome::Return(value)),
    };
    let capture = append_step(
        capture_place(target, env, program)?,
        PlaceStep::Index(key, span),
    );
    match capture {
        PlaceCapture::Path(path) => {
            resolve_captured_place(&path, env, program, true).map(PlaceOutcome::Place)
        }
        PlaceCapture::Return(value) => Ok(PlaceOutcome::Return(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaceOutcome, resolve_place};
    use crate::ast::{Expr, ExprKind};
    use crate::diagnostics::{FileId, Position, SourceMap, Span};
    use crate::eval::env::{Environment, Program};
    use crate::eval::value::Value;
    use std::sync::Arc;

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    fn ident(name: &str, id: u32) -> Expr {
        Expr {
            id: crate::ast::NodeId(id),
            kind: ExprKind::Ident(Arc::from(name)),
            span: dummy_span(),
        }
    }

    fn dummy_program() -> Arc<Program> {
        Arc::new(Program::new(Arc::new(SourceMap::new())))
    }

    #[test]
    fn resolves_bare_ident_root() {
        let mut env =
            Environment::with_frame(std::iter::once((Arc::from("xs"), Value::Int(1))).collect());
        let program = dummy_program();
        let e = ident("xs", 0);
        let place = resolve_place(&e, &mut env, &program);
        assert!(matches!(place, Ok(PlaceOutcome::Place(Value::Int(1)))));
    }

    #[test]
    fn index_out_of_range_list_aborts() {
        let list = Value::List(Arc::new(vec![Value::Int(1)]));
        let mut env = Environment::with_frame(std::iter::once((Arc::from("xs"), list)).collect());
        let program = dummy_program();
        let target = ident("xs", 0);
        let index_expr = Expr {
            id: crate::ast::NodeId(1),
            kind: ExprKind::IntLit(5),
            span: dummy_span(),
        };
        let full = Expr {
            id: crate::ast::NodeId(2),
            kind: ExprKind::Index {
                target: Box::new(target),
                index: Box::new(index_expr),
            },
            span: dummy_span(),
        };
        let result = resolve_place(&full, &mut env, &program);
        assert!(result.is_err());
    }
}
