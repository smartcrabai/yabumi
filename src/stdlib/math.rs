//! math namespace (including checked_*, STDLIB.md §12, ARCHITECTURE.md §2.1). No effect (pure).

use crate::eval::value::Value;
use crate::stdlib::{none_value, some_value};

pub const PI: f64 = std::f64::consts::PI;
pub const E: f64 = std::f64::consts::E;

/// Shared wrapper for the `checked_*` family: `Some(v)` becomes `Some(int)`, while `None`
/// (overflow or division by zero) becomes `None`.
fn checked_int(r: Option<i64>) -> Value {
    r.map_or_else(none_value, |v| some_value(Value::Int(v)))
}

#[must_use]
pub fn checked_div(a: i64, b: i64) -> Value {
    checked_int(a.checked_div(b))
}

#[must_use]
pub fn checked_mod(a: i64, b: i64) -> Value {
    checked_int(a.checked_rem(b))
}

#[must_use]
pub fn checked_add(a: i64, b: i64) -> Value {
    checked_int(a.checked_add(b))
}

#[must_use]
pub fn checked_sub(a: i64, b: i64) -> Value {
    checked_int(a.checked_sub(b))
}

#[must_use]
pub fn checked_mul(a: i64, b: i64) -> Value {
    checked_int(a.checked_mul(b))
}

/// `abs_int(x: int): int`. Neither STDLIB.md nor D-ERR-04 lists `abs_int` as something that can
/// panic (and its return type is a plain `int`, not `Option[int]`), so instead of `.abs()`
/// (which triggers a native Rust panic when the absolute value of `i64::MIN` overflows -- the
/// one case where this can happen), we use `wrapping_abs()` (which returns `i64::MIN` unchanged
/// for `i64::MIN`, the natural two's-complement wraparound). Since no panicking-free "safe"
/// variant exists, the choice is to not panic at all (a decision made in this file, flagged for
/// review).
#[must_use]
pub fn abs_int(x: i64) -> Value {
    Value::Int(x.wrapping_abs())
}

#[must_use]
pub fn abs_float(x: f64) -> Value {
    Value::Float(x.abs())
}

#[must_use]
pub fn min_int(a: i64, b: i64) -> Value {
    Value::Int(a.min(b))
}

#[must_use]
pub fn max_int(a: i64, b: i64) -> Value {
    Value::Int(a.max(b))
}

#[must_use]
pub fn min_float(a: f64, b: f64) -> Value {
    Value::Float(a.min(b))
}

#[must_use]
pub fn max_float(a: f64, b: f64) -> Value {
    Value::Float(a.max(b))
}

/// Saturating f64 -> i64 conversion (Rust 1.45+'s `as` cast has a defined behavior of NaN -> 0,
/// clamping out-of-range values to MIN/MAX -- no UB). Since neither STDLIB.md nor D-ERR-04
/// includes `floor`/`ceil`/`round` among the panicking operations -- they must "always succeed"
/// -- this saturating conversion is the most straightforward match for the spec (a decision made
/// in this file).
#[expect(
    clippy::cast_possible_truncation,
    reason = "The f64->i64 `as` cast has been a defined saturating conversion since Rust 1.45+ \
              (NaN => 0, out-of-range clamps to MIN/MAX). floor/ceil/round are excluded from \
              panicking in STDLIB.md (always succeed), so this behavior matches the spec"
)]
fn saturating_f64_to_i64(x: f64) -> i64 {
    x as i64
}

#[must_use]
pub fn floor(x: f64) -> Value {
    Value::Int(saturating_f64_to_i64(x.floor()))
}

#[must_use]
pub fn ceil(x: f64) -> Value {
    Value::Int(saturating_f64_to_i64(x.ceil()))
}

#[must_use]
pub fn round(x: f64) -> Value {
    Value::Int(saturating_f64_to_i64(x.round()))
}

#[must_use]
pub fn sqrt(x: f64) -> Value {
    Value::Float(x.sqrt())
}

#[must_use]
pub fn pow(base: f64, exp: f64) -> Value {
    Value::Float(base.powf(exp))
}

#[cfg(test)]
mod tests {
    use super::{
        abs_int, ceil, checked_add, checked_div, checked_mod, checked_mul, checked_sub, floor,
        round,
    };
    use crate::eval::value::Value;

    #[test]
    fn checked_div_handles_zero_and_min_over_neg_one() {
        assert_eq!(checked_div(10, 0), super::none_value());
        assert_eq!(checked_div(i64::MIN, -1), super::none_value());
        assert_eq!(checked_div(10, 2), super::some_value(Value::Int(5)));
    }

    #[test]
    fn checked_mod_handles_zero() {
        assert_eq!(checked_mod(10, 0), super::none_value());
        assert_eq!(checked_mod(-7, 3), super::some_value(Value::Int(-1)));
    }

    #[test]
    fn checked_add_sub_mul_detect_overflow() {
        assert_eq!(checked_add(i64::MAX, 1), super::none_value());
        assert_eq!(checked_sub(i64::MIN, 1), super::none_value());
        assert_eq!(checked_mul(i64::MAX, 2), super::none_value());
        assert_eq!(checked_add(1, 2), super::some_value(Value::Int(3)));
    }

    #[test]
    fn abs_int_does_not_panic_on_i64_min() {
        assert_eq!(abs_int(i64::MIN), Value::Int(i64::MIN));
        assert_eq!(abs_int(-5), Value::Int(5));
    }

    #[test]
    fn floor_ceil_round_saturate_instead_of_panicking() {
        assert_eq!(floor(1.9), Value::Int(1));
        assert_eq!(ceil(1.1), Value::Int(2));
        assert_eq!(round(1.5), Value::Int(2));
        assert_eq!(floor(f64::NAN), Value::Int(0));
        assert_eq!(ceil(1e300), Value::Int(i64::MAX));
    }
}
