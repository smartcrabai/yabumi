//! Arithmetic/comparison/equality operators (including overflow and division-by-zero
//! checks, ARCHITECTURE.md §5.7). The arithmetic operator implementation folds in
//! overflow checking simply by using Rust's `checked_*` family of methods directly.
//!
//! `and`/`or` (short-circuit evaluation required, D-OP-01) are excluded here because
//! `eval/expr.rs` itself controls the evaluation of both operands — this module only
//! handles the non-short-circuiting binary operators, which start from a state where
//! "both sides have already been evaluated to values".

use super::Abort;
use super::panic;
use super::value::Value;
use crate::ast::{BinaryOp, UnaryOp};
use crate::diagnostics::Span;

/// The body of the D-OP-01 through 08 binary operators (excluding `and`/`or`). int/float
/// mixing has already been eliminated as E1050 during the type-checking phase, so this
/// only ever needs to handle operations between operands of the same type. Overflow
/// (D-OP-08, E6003) and division-by-zero (D-OP-04, E6002) are detected here as `Abort`.
/// `==`/`!=` (D-OP-06) delegate to `Value`'s `PartialEq` implementation (eval/value.rs) —
/// direct float comparison happens inside value.rs's eq implementation, so
/// `clippy::float_cmp` never fires here.
pub fn eval_binary(op: BinaryOp, lhs: Value, rhs: Value, span: Span) -> Result<Value, Abort> {
    match (op, lhs, rhs) {
        // --- D-OP-07 `+` ---
        (BinaryOp::Add, Value::Int(a), Value::Int(b)) => a
            .checked_add(b)
            .map(Value::Int)
            .ok_or_else(|| panic::overflow(span)),
        (BinaryOp::Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (BinaryOp::Add, Value::Str(a), Value::Str(b)) => {
            Ok(Value::Str(std::sync::Arc::from(format!("{a}{b}"))))
        }
        (BinaryOp::Add, Value::List(a), Value::List(b)) => {
            let mut merged = Vec::with_capacity(a.len() + b.len());
            merged.extend(a.iter().cloned());
            merged.extend(b.iter().cloned());
            Ok(Value::List(std::sync::Arc::new(merged)))
        }

        // --- `-` ---
        (BinaryOp::Sub, Value::Int(a), Value::Int(b)) => a
            .checked_sub(b)
            .map(Value::Int)
            .ok_or_else(|| panic::overflow(span)),
        (BinaryOp::Sub, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),

        // --- `*` ---
        (BinaryOp::Mul, Value::Int(a), Value::Int(b)) => a
            .checked_mul(b)
            .map(Value::Int)
            .ok_or_else(|| panic::overflow(span)),
        (BinaryOp::Mul, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),

        // --- D-OP-04 `/` ---
        (BinaryOp::Div, Value::Int(a), Value::Int(b)) => {
            if b == 0 {
                return Err(panic::div_by_zero(span));
            }
            // i64::MIN / -1 is the sole division case that lands outside i64's range
            // (overflow) — caught in one shot by checked_div (an easy-to-miss edge case).
            a.checked_div(b)
                .map(Value::Int)
                .ok_or_else(|| panic::overflow(span))
        }
        // Division by zero between two floats is not a panic target (D-ERR-04 covers only
        // "integer" `/` and `%`) — IEEE754 inf/nan is returned as-is.
        (BinaryOp::Div, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),

        // --- D-OP-04 `%` (int only, sign follows the left operand) ---
        (BinaryOp::Mod, Value::Int(a), Value::Int(b)) => {
            // Rust's `%` is already the remainder of truncating division (sign follows the
            // dividend), and i64::MIN % -1 is 0 (does not overflow), so `checked_rem` only
            // needs to catch division by zero.
            a.checked_rem(b)
                .map(Value::Int)
                .ok_or_else(|| panic::div_by_zero(span))
        }

        // --- D-OP-05 ordering comparisons (int/float/str) ---
        (BinaryOp::Lt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
        (BinaryOp::Lt, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
        (BinaryOp::Lt, Value::Str(a), Value::Str(b)) => Ok(Value::Bool(*a < *b)),
        (BinaryOp::LtEq, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
        (BinaryOp::LtEq, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
        (BinaryOp::LtEq, Value::Str(a), Value::Str(b)) => Ok(Value::Bool(*a <= *b)),
        (BinaryOp::Gt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
        (BinaryOp::Gt, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
        (BinaryOp::Gt, Value::Str(a), Value::Str(b)) => Ok(Value::Bool(*a > *b)),
        (BinaryOp::GtEq, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
        (BinaryOp::GtEq, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
        (BinaryOp::GtEq, Value::Str(a), Value::Str(b)) => Ok(Value::Bool(*a >= *b)),

        // --- D-OP-06 `==`/`!=` (structural equality across all types, delegates to Value::PartialEq) ---
        (BinaryOp::EqEq, a, b) => Ok(Value::Bool(a == b)),
        (BinaryOp::NotEq, a, b) => Ok(Value::Bool(a != b)),

        (BinaryOp::And | BinaryOp::Or, ..) => {
            unreachable!(
                "and/or short-circuit, so eval/expr.rs branches before both sides are evaluated"
            )
        }

        _ => {
            unreachable!(
                "already type-checked, so one of the type combinations above must match (D-OP-03/04/05/07)"
            )
        }
    }
}

/// Unary `-` (the only overflow case is negating i64::MIN's sign, D-OP-08) / `not` (D-OP-01).
pub fn eval_unary(op: UnaryOp, operand: Value, span: Span) -> Result<Value, Abort> {
    match (op, operand) {
        (UnaryOp::Neg, Value::Int(n)) => n
            .checked_neg()
            .map(Value::Int)
            .ok_or_else(|| panic::overflow(span)),
        (UnaryOp::Neg, Value::Float(n)) => Ok(Value::Float(-n)),
        (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
        _ => unreachable!(
            "already type-checked, so Neg only ever appears on int/float and Not only on bool"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{eval_binary, eval_unary};
    use crate::ast::{BinaryOp, UnaryOp};
    use crate::diagnostics::{FileId, Position, Span};
    use crate::eval::value::Value;

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    #[test]
    fn add_int_overflow_panics() {
        let r = eval_binary(
            BinaryOp::Add,
            Value::Int(i64::MAX),
            Value::Int(1),
            dummy_span(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn div_by_zero_panics() {
        let r = eval_binary(BinaryOp::Div, Value::Int(1), Value::Int(0), dummy_span());
        assert!(r.is_err());
    }

    #[test]
    fn mod_follows_dividend_sign() {
        let r = eval_binary(BinaryOp::Mod, Value::Int(-7), Value::Int(3), dummy_span());
        assert!(matches!(r, Ok(Value::Int(-1))));
    }

    #[test]
    fn int_div_truncates_toward_zero() {
        let r = eval_binary(BinaryOp::Div, Value::Int(-7), Value::Int(2), dummy_span());
        assert!(matches!(r, Ok(Value::Int(-3))));
    }

    #[test]
    fn min_div_neg_one_overflows() {
        let r = eval_binary(
            BinaryOp::Div,
            Value::Int(i64::MIN),
            Value::Int(-1),
            dummy_span(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn structural_equality_across_lists() {
        let a = Value::List(std::sync::Arc::new(vec![Value::Int(1), Value::Int(2)]));
        let b = Value::List(std::sync::Arc::new(vec![Value::Int(1), Value::Int(2)]));
        let r = eval_binary(BinaryOp::EqEq, a, b, dummy_span());
        assert!(matches!(r, Ok(Value::Bool(true))));
    }

    #[test]
    fn unary_neg_min_overflows() {
        let r = eval_unary(UnaryOp::Neg, Value::Int(i64::MIN), dummy_span());
        assert!(r.is_err());
    }

    #[test]
    fn unary_not_flips_bool() {
        let r = eval_unary(UnaryOp::Not, Value::Bool(true), dummy_span());
        assert!(matches!(r, Ok(Value::Bool(false))));
    }
}
