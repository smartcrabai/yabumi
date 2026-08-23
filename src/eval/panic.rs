//! `Abort` type, common constructors for E6001-E6008 (ARCHITECTURE.md §5.6/§5.7).
//!
//! Per D-ERR-05 ("never display the full call stack — a single frame only"), no call
//! traceback mechanism is implemented at all — `Abort(Diagnostic)`'s `Diagnostic.span` is
//! simply the `Span` of the expression that triggered the panic itself, and it is emitted
//! as a single line in the form `file:line:col [E6xxx] message`.

use crate::diagnostics::{Diagnostic, ErrorCode, Span};

/// panic-class errors (D-ERR-04) are a signal that must abnormally terminate the entire
/// process immediately without distinguishing function boundaries at all, so they are
/// handled independently of `Flow` (eval/expr.rs) by deferring to Rust's own unwinding via
/// `?` (§5.6).
#[derive(Debug)]
pub struct Abort(pub Diagnostic);

/// D-ERR-04-1: out-of-range subscript access on a list/tuple/string, or a nonexistent key
/// on a dict (E6001).
#[must_use]
pub fn out_of_range(span: Span, detail: &str) -> Abort {
    Abort(Diagnostic {
        code: ErrorCode::IndexOutOfRange,
        span,
        message: format!("panic: index out of range ({detail})"),
    })
}

/// D-ERR-04-3: division by zero for integer `/` and `%` (E6002).
#[must_use]
pub fn div_by_zero(span: Span) -> Abort {
    Abort(Diagnostic {
        code: ErrorCode::DivisionByZero,
        span,
        message: "panic: division by zero".to_owned(),
    })
}

/// D-ERR-04-4: i64 range overflow from `+` `-` `*` unary `-`, and out-of-range conversion
/// in `int(x: float)` (E6003).
#[must_use]
pub fn overflow(span: Span) -> Abort {
    Abort(Diagnostic {
        code: ErrorCode::IntegerOverflow,
        span,
        message: "panic: integer overflow".to_owned(),
    })
}

/// D-ERR-04-5: `assert` failure (E6004). `detail` is either the source text of the
/// condition expression (1-argument form, STDLIB.md §13) or the user-supplied `msg`
/// (2-argument form).
#[must_use]
pub fn assert_failed(span: Span, detail: &str) -> Abort {
    Abort(Diagnostic {
        code: ErrorCode::AssertFailed,
        span,
        message: format!("panic: assertion failed: {detail}"),
    })
}

/// D-ERR-04-6: failure of `Result.unwrap()` / `Option.unwrap()` (E6007).
#[must_use]
pub fn unwrap_failed(span: Span, detail: &str) -> Abort {
    Abort(Diagnostic {
        code: ErrorCode::UnwrapFailed,
        span,
        message: format!("panic: {detail}"),
    })
}

/// D-ERR-04-7: stack overflow from deep recursion (E6008).
#[must_use]
pub fn stack_overflow(span: Span) -> Abort {
    Abort(Diagnostic {
        code: ErrorCode::StackOverflow,
        span,
        message: "panic: stack overflow (recursion too deep)".to_owned(),
    })
}

/// Err propagation via top-level `?` (E6005, the `"unwrapped Err via ?: <message>"` form
/// from D-ERR-05).
#[must_use]
pub fn toplevel_err_propagation(span: Span, error_message: &str) -> Abort {
    Abort(Diagnostic {
        code: ErrorCode::TopLevelErrPropagation,
        span,
        message: format!("unwrapped Err via ?: {error_message}"),
    })
}

/// None propagation via top-level `?` (E6006).
#[must_use]
pub fn toplevel_none_propagation(span: Span) -> Abort {
    Abort(Diagnostic {
        code: ErrorCode::TopLevelNonePropagation,
        span,
        message: "unwrapped None via ?".to_owned(),
    })
}
