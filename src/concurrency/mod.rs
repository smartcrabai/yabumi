//! Bounded OS-thread execution for `par`, `par_map`, and `par_each`.

use crate::ast::{Expr, ParKind};
use crate::eval::env::{Environment, Program};
use crate::eval::value::{Closure, Value};
use crate::eval::{Abort, EvalResult, Flow};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

fn worker_count(item_count: usize) -> usize {
    item_count.min(std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
}

#[expect(
    clippy::expect_used,
    reason = "bounded worker creation can still fail only under unrecoverable OS resource exhaustion"
)]
pub fn eval_par_list(
    kind: ParKind,
    elements: &[Expr],
    env: &Environment,
    program: &Arc<Program>,
) -> EvalResult {
    let n = elements.len();
    if n == 0 {
        return Ok(Flow::Value(match kind {
            ParKind::List => Value::List(Arc::new(Vec::new())),
            ParKind::Tuple => Value::Tuple(Arc::from([])),
        }));
    }

    let workers = worker_count(n);
    let chunk_size = n.div_ceil(workers);
    let mut captured: Vec<Environment> = elements.iter().map(|_| env.snapshot_for_par()).collect();
    let (tx, rx) = mpsc::channel::<(usize, EvalResult)>();
    let cancelled = AtomicBool::new(false);
    let mut slots: Vec<Option<Value>> = vec![None; n];
    let mut first_abort = None;

    std::thread::scope(|scope| {
        for (chunk_index, (exprs, environments)) in elements
            .chunks(chunk_size)
            .zip(captured.chunks_mut(chunk_size))
            .enumerate()
        {
            let tx = tx.clone();
            let program = Arc::clone(program);
            let cancelled = &cancelled;
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    let base = chunk_index * chunk_size;
                    for (offset, (expr, local_env)) in
                        exprs.iter().zip(environments.iter_mut()).enumerate()
                    {
                        if cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        let result = crate::eval::expr::eval_expr(expr, local_env, &program);
                        if result.is_err() {
                            cancelled.store(true, Ordering::Release);
                        }
                        let _ = tx.send((base + offset, result));
                    }
                })
                .expect("bounded par worker creation failed");
        }
        drop(tx);

        while let Ok((index, result)) = rx.recv() {
            match result {
                Ok(Flow::Value(value)) => slots[index] = Some(value),
                Ok(Flow::Return(_)) => {
                    unreachable!("bare question operators are rejected inside par branches")
                }
                Err(abort) => {
                    if program.abort_process_on_par_panic {
                        eprintln!("{}", abort.0.render(&program.sources));
                        std::process::exit(1);
                    }
                    first_abort = Some(abort);
                    break;
                }
            }
            if slots.iter().all(Option::is_some) {
                break;
            }
        }
    });

    if let Some(abort) = first_abort {
        return Err(abort);
    }
    let values = slots
        .into_iter()
        .map(|value| value.unwrap_or_else(|| unreachable!("every par branch returned a value")))
        .collect();
    Ok(Flow::Value(match kind {
        ParKind::List => Value::List(Arc::new(values)),
        ParKind::Tuple => Value::Tuple(Arc::from(values)),
    }))
}

#[expect(
    clippy::expect_used,
    reason = "bounded worker creation can still fail only under unrecoverable OS resource exhaustion"
)]
pub fn eval_par_map(
    values: Vec<Value>,
    closure: &Closure,
    program: &Arc<Program>,
) -> Result<Vec<Value>, Abort> {
    let n = values.len();
    if n == 0 {
        return Ok(Vec::new());
    }

    let workers = worker_count(n);
    let chunk_size = n.div_ceil(workers);
    let (tx, rx) = mpsc::channel::<(usize, Result<Value, Abort>)>();
    let cancelled = AtomicBool::new(false);
    let mut slots: Vec<Option<Value>> = vec![None; n];
    let mut first_abort = None;

    std::thread::scope(|scope| {
        let mut remaining = values;
        let mut base = 0usize;
        while !remaining.is_empty() {
            let tail = if remaining.len() > chunk_size {
                remaining.split_off(chunk_size)
            } else {
                Vec::new()
            };
            let chunk = std::mem::replace(&mut remaining, tail);
            let tx = tx.clone();
            let program = Arc::clone(program);
            let cancelled = &cancelled;
            let chunk_base = base;
            base += chunk.len();
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    for (offset, value) in chunk.into_iter().enumerate() {
                        if cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        let result =
                            crate::eval::call::call_closure(closure, vec![value], &program);
                        if result.is_err() {
                            cancelled.store(true, Ordering::Release);
                        }
                        let _ = tx.send((chunk_base + offset, result));
                    }
                })
                .expect("bounded par_map worker creation failed");
        }
        drop(tx);

        while let Ok((index, result)) = rx.recv() {
            match result {
                Ok(value) => slots[index] = Some(value),
                Err(abort) => {
                    if program.abort_process_on_par_panic {
                        eprintln!("{}", abort.0.render(&program.sources));
                        std::process::exit(1);
                    }
                    first_abort = Some(abort);
                    break;
                }
            }
            if slots.iter().all(Option::is_some) {
                break;
            }
        }
    });

    if let Some(abort) = first_abort {
        return Err(abort);
    }
    Ok(slots
        .into_iter()
        .map(|value| value.unwrap_or_else(|| unreachable!("every par_map branch returned a value")))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::eval_par_map;
    use crate::eval::env::Program;
    use crate::eval::value::{CallTarget, Closure, LambdaBody, Value};
    use std::sync::Arc;

    fn dummy_program() -> Arc<Program> {
        Arc::new(Program::new(Arc::new(crate::diagnostics::SourceMap::new())))
    }

    fn double_closure() -> Closure {
        // Equivalent to `(x) => x` (returns the parameter as-is, a minimal lambda with no capture).
        let body = crate::ast::Expr {
            id: crate::ast::NodeId(0),
            kind: crate::ast::ExprKind::Ident(Arc::from("x")),
            span: dummy_span(),
        };
        Closure {
            target: CallTarget::Lambda(Arc::new(LambdaBody {
                params: vec![crate::ast::LambdaParam {
                    name: Arc::from("x"),
                    ty: None,
                    span: dummy_span(),
                }],
                body,
            })),
            captured: Vec::new(),
        }
    }

    fn dummy_span() -> crate::diagnostics::Span {
        crate::diagnostics::Span {
            file: crate::diagnostics::FileId(0),
            start: crate::diagnostics::Position { line: 1, col: 1 },
            end: crate::diagnostics::Position { line: 1, col: 1 },
        }
    }

    #[test]
    fn par_map_preserves_input_order() {
        let closure = double_closure();
        let program = dummy_program();
        let values = vec![Value::Int(1), Value::Int(2), Value::Int(3)];
        let Ok(result) = eval_par_map(values, &closure, &program) else {
            panic!("identity closure must not abort")
        };
        assert_eq!(result, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    }
}
