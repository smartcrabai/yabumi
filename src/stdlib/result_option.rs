//! Methods on Result[T,E]/Option[T] (STDLIB.md §3.1-3.2, ARCHITECTURE.md §2.1).
//!
//! `Result`/`Option` are represented as ordinary builtin enums (`Value::Enum`) per D-TYPE-09 --
//! there's no dedicated Rust type; branching is done on `variant_name` ("Ok"/"Err"/"Some"/
//! "None").

use crate::diagnostics::Span;
use crate::eval::call::{call_closure, error_message_of};
use crate::eval::env::Program;
use crate::eval::value::{Closure, EnumInstance, Value};
use crate::eval::{Abort, panic};
use crate::stdlib::{err_value, none_value, ok_value, some_value};
use std::sync::Arc;

/// Builds a `Value::Enum` that's a plain copy of `self_`. Used by `map`/`map_err`/`and_then`
/// to "pass the non-target variant through unchanged".
fn passthrough(self_: &EnumInstance) -> Value {
    Value::Enum(Arc::new(self_.clone()))
}

fn is_variant(self_: &EnumInstance, name: &str) -> bool {
    self_.variant_name.as_ref() == name
}

// --- 3.1 Result[T, E] ---

#[must_use]
pub fn is_ok(self_: &EnumInstance) -> Value {
    Value::Bool(is_variant(self_, "Ok"))
}

#[must_use]
pub fn is_err(self_: &EnumInstance) -> Value {
    Value::Bool(is_variant(self_, "Err"))
}

#[must_use]
pub fn result_ok(self_: &EnumInstance) -> Value {
    if is_variant(self_, "Ok") {
        some_value(self_.fields[0].clone())
    } else {
        none_value()
    }
}

#[must_use]
pub fn result_err(self_: &EnumInstance) -> Value {
    if is_variant(self_, "Err") {
        some_value(self_.fields[0].clone())
    } else {
        none_value()
    }
}

/// panics: Err(E6007). Includes `Error.message` in the trace.
pub fn result_unwrap(self_: &EnumInstance, span: Span) -> Result<Value, Abort> {
    if is_variant(self_, "Ok") {
        Ok(self_.fields[0].clone())
    } else {
        let message = error_message_of(&self_.fields[0]);
        Err(panic::unwrap_failed(
            span,
            &format!("called unwrap() on an Err value: {message}"),
        ))
    }
}

#[must_use]
pub fn result_unwrap_or(self_: &EnumInstance, default: Value) -> Value {
    if is_variant(self_, "Ok") {
        self_.fields[0].clone()
    } else {
        default
    }
}

pub fn result_unwrap_or_else(
    self_: &EnumInstance,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    if is_variant(self_, "Ok") {
        Ok(self_.fields[0].clone())
    } else {
        call_closure(f, vec![self_.fields[0].clone()], program)
    }
}

pub fn result_map(
    self_: &EnumInstance,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    if is_variant(self_, "Ok") {
        let mapped = call_closure(f, vec![self_.fields[0].clone()], program)?;
        Ok(ok_value(mapped))
    } else {
        Ok(passthrough(self_))
    }
}

pub fn result_map_err(
    self_: &EnumInstance,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    if is_variant(self_, "Err") {
        let mapped = call_closure(f, vec![self_.fields[0].clone()], program)?;
        Ok(err_value(mapped))
    } else {
        Ok(passthrough(self_))
    }
}

pub fn result_and_then(
    self_: &EnumInstance,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    if is_variant(self_, "Ok") {
        call_closure(f, vec![self_.fields[0].clone()], program)
    } else {
        Ok(passthrough(self_))
    }
}

// --- 3.2 Option[T] ---

#[must_use]
pub fn is_some(self_: &EnumInstance) -> Value {
    Value::Bool(is_variant(self_, "Some"))
}

#[must_use]
pub fn is_none(self_: &EnumInstance) -> Value {
    Value::Bool(is_variant(self_, "None"))
}

/// panics: None(E6007).
pub fn option_unwrap(self_: &EnumInstance, span: Span) -> Result<Value, Abort> {
    if is_variant(self_, "Some") {
        Ok(self_.fields[0].clone())
    } else {
        Err(panic::unwrap_failed(
            span,
            "called unwrap() on a None value",
        ))
    }
}

#[must_use]
pub fn option_unwrap_or(self_: &EnumInstance, default: Value) -> Value {
    if is_variant(self_, "Some") {
        self_.fields[0].clone()
    } else {
        default
    }
}

/// `f: () -> T` (a zero-argument closure).
pub fn option_unwrap_or_else(
    self_: &EnumInstance,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    if is_variant(self_, "Some") {
        Ok(self_.fields[0].clone())
    } else {
        call_closure(f, Vec::new(), program)
    }
}

pub fn option_map(
    self_: &EnumInstance,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    if is_variant(self_, "Some") {
        let mapped = call_closure(f, vec![self_.fields[0].clone()], program)?;
        Ok(some_value(mapped))
    } else {
        Ok(passthrough(self_))
    }
}

pub fn option_and_then(
    self_: &EnumInstance,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    if is_variant(self_, "Some") {
        call_closure(f, vec![self_.fields[0].clone()], program)
    } else {
        Ok(passthrough(self_))
    }
}

pub fn option_filter(
    self_: &EnumInstance,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    if !is_variant(self_, "Some") {
        return Ok(none_value());
    }
    let keep = call_closure(f, vec![self_.fields[0].clone()], program)?;
    let Value::Bool(b) = keep else {
        unreachable!("type-checked already, so filter's f always returns bool")
    };
    Ok(if b { passthrough(self_) } else { none_value() })
}

#[must_use]
pub fn option_ok_or(self_: &EnumInstance, err: Value) -> Value {
    if is_variant(self_, "Some") {
        ok_value(self_.fields[0].clone())
    } else {
        err_value(err)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_err, is_none, is_ok, is_some, option_filter, option_map, option_ok_or, option_unwrap,
        option_unwrap_or, option_unwrap_or_else, result_and_then, result_err, result_map,
        result_ok, result_unwrap, result_unwrap_or,
    };
    use crate::diagnostics::{FileId, Position, Span};
    use crate::eval::value::{CallTarget, Closure, EnumInstance, Value};
    use crate::stdlib::{err_value, none_value, ok_value, some_value};
    use std::sync::Arc;

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    fn enum_of(v: Value) -> EnumInstance {
        let Value::Enum(inst) = v else {
            panic!("expected an enum value")
        };
        EnumInstance {
            type_name: Arc::clone(&inst.type_name),
            variant_index: inst.variant_index,
            variant_name: Arc::clone(&inst.variant_name),
            fields: inst.fields.clone(),
        }
    }

    /// An identity closure equivalent to `(x) -> x`. Since constructing `ExprKind::Ident`
    /// (rather than just `Ident`) is inconvenient without going through `LambdaBody`, tests that
    /// don't need to require a real function that `CallTarget::Function` points to only verify
    /// the pure branches that never go through the closure-calling path (`call_closure`) --
    /// things like is_ok -- while map/and_then-family tests that do go through a closure only
    /// verify the "doesn't call the closure" branch (the None/Err side) of `option_filter`/
    /// `option_map`. `call_closure` itself is Unit 11's implementation responsibility and is out
    /// of scope for this test (a decision made in this file).
    fn unused_closure() -> Closure {
        Closure {
            target: CallTarget::Function(Arc::from("__unused__")),
            captured: Vec::new(),
        }
    }

    #[test]
    fn result_predicates_and_accessors() {
        let ok = enum_of(ok_value(Value::Int(1)));
        let err = enum_of(err_value(Value::Int(2)));
        assert_eq!(is_ok(&ok), Value::Bool(true));
        assert_eq!(is_err(&ok), Value::Bool(false));
        assert_eq!(is_ok(&err), Value::Bool(false));
        assert_eq!(is_err(&err), Value::Bool(true));
        assert_eq!(result_ok(&ok), some_value(Value::Int(1)));
        assert_eq!(result_ok(&err), none_value());
        assert_eq!(result_err(&err), some_value(Value::Int(2)));
        assert_eq!(result_unwrap_or(&ok, Value::Int(9)), Value::Int(1));
        assert_eq!(result_unwrap_or(&err, Value::Int(9)), Value::Int(9));
    }

    #[test]
    fn result_unwrap_succeeds_on_ok_and_panics_on_err() {
        let ok = enum_of(ok_value(Value::Int(1)));
        let Ok(v) = result_unwrap(&ok, dummy_span()) else {
            panic!("expected Ok")
        };
        assert_eq!(v, Value::Int(1));

        let inner_error = crate::stdlib::error_value("decode", "boom".to_owned());
        let err = enum_of(err_value(inner_error));
        assert!(result_unwrap(&err, dummy_span()).is_err());
    }

    #[test]
    fn result_map_and_and_then_pass_through_err_without_calling_f() {
        let err = enum_of(err_value(Value::Int(2)));
        let f = unused_closure();
        let program = Arc::new(crate::eval::env::Program::new(Arc::new(
            crate::diagnostics::SourceMap::new(),
        )));
        let Ok(v) = result_map(&err, &f, &program) else {
            panic!("expected Ok (passthrough)")
        };
        assert_eq!(v, err_value(Value::Int(2)));
        let Ok(v) = result_and_then(&err, &f, &program) else {
            panic!("expected Ok (passthrough)")
        };
        assert_eq!(v, err_value(Value::Int(2)));
    }

    #[test]
    fn option_predicates_and_accessors() {
        let some = enum_of(some_value(Value::Int(1)));
        let none = enum_of(none_value());
        assert_eq!(is_some(&some), Value::Bool(true));
        assert_eq!(is_none(&some), Value::Bool(false));
        assert_eq!(is_some(&none), Value::Bool(false));
        assert_eq!(is_none(&none), Value::Bool(true));
        assert_eq!(option_unwrap_or(&some, Value::Int(9)), Value::Int(1));
        assert_eq!(option_unwrap_or(&none, Value::Int(9)), Value::Int(9));
        assert_eq!(option_ok_or(&some, Value::Int(0)), ok_value(Value::Int(1)));
        assert_eq!(option_ok_or(&none, Value::Int(0)), err_value(Value::Int(0)));
    }

    #[test]
    fn option_unwrap_succeeds_on_some_and_panics_on_none() {
        let some = enum_of(some_value(Value::Int(1)));
        let Ok(v) = option_unwrap(&some, dummy_span()) else {
            panic!("expected Ok")
        };
        assert_eq!(v, Value::Int(1));
        let none = enum_of(none_value());
        assert!(option_unwrap(&none, dummy_span()).is_err());
    }

    #[test]
    fn option_map_and_filter_pass_through_none_without_calling_f() {
        let none = enum_of(none_value());
        let f = unused_closure();
        let program = Arc::new(crate::eval::env::Program::new(Arc::new(
            crate::diagnostics::SourceMap::new(),
        )));
        let Ok(v) = option_map(&none, &f, &program) else {
            panic!("expected Ok (passthrough)")
        };
        assert_eq!(v, none_value());
        let Ok(v) = option_filter(&none, &f, &program) else {
            panic!("expected Ok (passthrough)")
        };
        assert_eq!(v, none_value());
    }

    #[test]
    fn option_unwrap_or_else_skips_call_on_some() {
        let some = enum_of(some_value(Value::Int(7)));
        let f = unused_closure();
        let program = Arc::new(crate::eval::env::Program::new(Arc::new(
            crate::diagnostics::SourceMap::new(),
        )));
        let Ok(v) = option_unwrap_or_else(&some, &f, &program) else {
            panic!("expected Ok")
        };
        assert_eq!(v, Value::Int(7));
    }
}
