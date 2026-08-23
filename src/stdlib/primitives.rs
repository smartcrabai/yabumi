//! Methods on int/float/bool/str (STDLIB.md §1, ARCHITECTURE.md §2.1).

use crate::diagnostics::Span;
use crate::eval::value::Value;
use crate::eval::{Abort, panic};
use crate::stdlib::{err_value, error_value, none_value, ok_value, some_value};
use std::sync::Arc;

// --- 1.1 Type conversion (all pure, no effect) ---

/// `-2^63` (i64::MIN itself, exactly representable in f64).
const I64_MIN_AS_F64: f64 = -9_223_372_036_854_775_808.0;
/// `2^63` (one past i64::MAX, exactly representable in f64). `i64::MAX as f64` rounds up to
/// exactly `2^63` (the actual i64::MAX, `2^63 - 1`, isn't exactly representable in f64), so this
/// constant is used as the upper bound where "at or above this is out of range".
const I64_MAX_BOUND_AS_F64: f64 = 9_223_372_036_854_775_808.0;

/// `int(x: float): int` (truncation toward zero). Values outside the i64 range panic with E6003
/// (integer overflow).
pub fn int_from_float(x: f64, span: Span) -> Result<Value, Abort> {
    let truncated = x.trunc();
    if !truncated.is_finite() || !(I64_MIN_AS_F64..I64_MAX_BOUND_AS_F64).contains(&truncated) {
        return Err(panic::overflow(span));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "The preceding range check (I64_MIN_AS_F64/I64_MAX_BOUND_AS_F64) guarantees \
                  truncated is always within i64's representable range"
    )]
    let n = truncated as i64;
    Ok(Value::Int(n))
}

/// `float(x: int): float` (always succeeds).
#[must_use]
pub fn float_from_int(x: i64) -> Value {
    #[expect(
        clippy::cast_precision_loss,
        reason = "D-TYPE-14: a large i64 may lose mantissa precision, but that's not an error -- \
                  it's spec-compliant behavior (always succeeds)"
    )]
    let f = x as f64;
    Value::Float(f)
}

/// Stringifies a `float` using the shortest round-trip representation, always including a
/// decimal point (STDLIB.md §1.1). Rust's standard `Display` always produces a flat decimal
/// expansion and never switches to scientific notation (`1e300` becomes a string of 300+
/// digits), which doesn't match STDLIB.md's example of `1e20`. Switching to `{:e}` (Rust's
/// `LowerExp` also uses the shortest round-trip digit algorithm, so it produces a minimal-digit
/// scientific form like `1e20`) only when the absolute value is extreme (`>= 1e16` or `< 1e-4`,
/// following the threshold at which Python's `repr` switches to scientific notation) is a
/// decision made in this file, since SPEC/DECISIONS doesn't specify this threshold (flagged for
/// review).
fn format_float(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return if x.is_sign_negative() {
            "-inf".to_owned()
        } else {
            "inf".to_owned()
        };
    }
    let abs = x.abs();
    if abs != 0.0 && !(1e-4..1e16).contains(&abs) {
        return format!("{x:e}");
    }
    let plain = format!("{x}");
    if plain.contains('.') {
        plain
    } else {
        format!("{plain}.0")
    }
}

/// `str(x: int): str` / `str(x: float): str` / `str(x: bool): str` (the D-STDPOL-01 overload
/// special case, always succeeds). `str(x: float)` uses the shortest round-trip representation
/// and always includes a decimal point (1.0, 3.14, 1e20).
#[must_use]
pub fn str_from_value(x: &Value) -> Value {
    match x {
        Value::Int(n) => Value::Str(Arc::from(n.to_string())),
        Value::Float(f) => Value::Str(Arc::from(format_float(*f))),
        Value::Bool(b) => Value::Str(Arc::from(if *b { "true" } else { "false" })),
        _ => unreachable!("type-checked already, so str(x)'s x is always int/float/bool"),
    }
}

// --- 1.2 int ---

#[must_use]
pub fn int_to_str(x: i64) -> Value {
    Value::Str(Arc::from(x.to_string()))
}

// --- 1.3 float ---

#[must_use]
pub fn float_to_str(x: f64) -> Value {
    Value::Str(Arc::from(format_float(x)))
}

// --- 1.5 str ---

#[must_use]
pub fn str_len(s: &str) -> Value {
    Value::Int(i64::try_from(s.chars().count()).unwrap_or(i64::MAX))
}

/// `get(self: str, i: int): Option[str]` (out of range gives None, does not panic).
#[must_use]
pub fn str_get(s: &str, i: i64) -> Value {
    let ch = usize::try_from(i).ok().and_then(|idx| s.chars().nth(idx));
    match ch {
        Some(c) => some_value(Value::Str(Arc::from(c.to_string()))),
        None => none_value(),
    }
}

#[must_use]
pub fn str_chars(s: &str) -> Value {
    let items = s
        .chars()
        .map(|c| Value::Str(Arc::from(c.to_string())))
        .collect();
    Value::List(Arc::new(items))
}

#[must_use]
pub fn str_bytes(s: &str) -> Value {
    let items = s.bytes().map(|b| Value::Int(i64::from(b))).collect();
    Value::List(Arc::new(items))
}

#[must_use]
pub fn str_split(s: &str, sep: &str) -> Value {
    let items = s
        .split(sep)
        .map(|part| Value::Str(Arc::from(part)))
        .collect();
    Value::List(Arc::new(items))
}

#[must_use]
pub fn str_trim(s: &str) -> Value {
    Value::Str(Arc::from(s.trim()))
}

#[must_use]
pub fn str_trim_start(s: &str) -> Value {
    Value::Str(Arc::from(s.trim_start()))
}

#[must_use]
pub fn str_trim_end(s: &str) -> Value {
    Value::Str(Arc::from(s.trim_end()))
}

#[must_use]
pub fn str_to_upper(s: &str) -> Value {
    Value::Str(Arc::from(s.to_uppercase()))
}

#[must_use]
pub fn str_to_lower(s: &str) -> Value {
    Value::Str(Arc::from(s.to_lowercase()))
}

#[must_use]
pub fn str_is_empty(s: &str) -> Value {
    Value::Bool(s.is_empty())
}

/// `to_str(self: str): str` (not documented in STDLIB.md §1.5, but `eval/call.rs` calls it from
/// `str_method`'s `"to_str"` branch -- provided as an identity function for symmetry with every
/// other primitive type having a `to_str`, a decision made in this file).
#[must_use]
pub fn str_to_str(s: &Arc<str>) -> Value {
    Value::Str(Arc::clone(s))
}

#[must_use]
pub fn str_contains(s: &str, needle: &str) -> Value {
    Value::Bool(s.contains(needle))
}

#[must_use]
pub fn str_starts_with(s: &str, prefix: &str) -> Value {
    Value::Bool(s.starts_with(prefix))
}

#[must_use]
pub fn str_ends_with(s: &str, suffix: &str) -> Value {
    Value::Bool(s.ends_with(suffix))
}

#[must_use]
pub fn str_replace(s: &str, from: &str, to: &str) -> Value {
    Value::Str(Arc::from(s.replace(from, to)))
}

/// `repeat(self: str, n: int): str`. A negative n is undefined by STDLIB.md, so instead of
/// panicking it's treated as an empty string (equivalent to 0 repetitions) (a decision made in
/// this file).
#[must_use]
pub fn str_repeat(s: &str, n: i64) -> Value {
    let count = usize::try_from(n).unwrap_or(0);
    Value::Str(Arc::from(s.repeat(count)))
}

/// `find(self: str, needle: str): Option[int]`. Returns a char index (D-COL-03) -- since
/// `str::find` returns a byte offset, it's converted into the char count of the substring
/// preceding the match.
#[must_use]
pub fn str_find(s: &str, needle: &str) -> Value {
    match s.find(needle) {
        Some(byte_idx) => {
            let char_idx = s[..byte_idx].chars().count();
            some_value(Value::Int(i64::try_from(char_idx).unwrap_or(i64::MAX)))
        }
        None => none_value(),
    }
}

/// `slice(self: str, start: int, end: int): str`. panics: out of range (E6001). No safe variant
/// (check with `len()` beforehand). Char-index based (D-COL-03).
pub fn str_slice(s: &str, start: i64, end: i64, span: Span) -> Result<Value, Abort> {
    let char_count = s.chars().count();
    let bounds = usize::try_from(start)
        .ok()
        .zip(usize::try_from(end).ok())
        .filter(|(s_i, e_i)| s_i <= e_i && *e_i <= char_count);
    match bounds {
        Some((s_i, e_i)) => {
            let sliced: String = s.chars().skip(s_i).take(e_i - s_i).collect();
            Ok(Value::Str(Arc::from(sliced)))
        }
        None => Err(panic::out_of_range(span, "str slice")),
    }
}

/// `parse_int(self: str): Result[int, Error]` (kind: "decode").
#[must_use]
pub fn str_parse_int(s: &str) -> Value {
    match s.trim().parse::<i64>() {
        Ok(n) => ok_value(Value::Int(n)),
        Err(e) => err_value(error_value("decode", format!("invalid int literal: {e}"))),
    }
}

/// `parse_float(self: str): Result[float, Error]` (kind: "decode").
#[must_use]
pub fn str_parse_float(s: &str) -> Value {
    match s.trim().parse::<f64>() {
        Ok(f) => ok_value(Value::Float(f)),
        Err(e) => err_value(error_value("decode", format!("invalid float literal: {e}"))),
    }
}

// str's iterator-family methods (map/filter/fold/find_by/any/all/count/enumerate/zip/rev/take/
// skip/flat_map/sort_by/chain) treat self as the equivalent of `.chars()` (list[str]) and always
// return list[U] (STDLIB.md §1.5). The implementation reuses collections.rs's generic list[T]
// iterator implementation via `str_chars(s)` (`eval/call.rs`'s `str_method` already wires this
// up), so no separate str-specific functions are provided here.

#[cfg(test)]
mod tests {
    use super::{
        float_from_int, format_float, int_from_float, str_bytes, str_chars, str_find, str_get,
        str_len, str_parse_float, str_parse_int, str_slice, str_split,
    };
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
    fn int_from_float_truncates_toward_zero() {
        let Ok(v) = int_from_float(3.9, dummy_span()) else {
            panic!("expected Ok")
        };
        assert_eq!(v, Value::Int(3));
        let Ok(v) = int_from_float(-3.9, dummy_span()) else {
            panic!("expected Ok")
        };
        assert_eq!(v, Value::Int(-3));
    }

    #[test]
    fn int_from_float_panics_on_overflow() {
        assert!(int_from_float(1e300, dummy_span()).is_err());
        assert!(int_from_float(f64::NAN, dummy_span()).is_err());
    }

    #[test]
    fn float_from_int_always_succeeds() {
        assert_eq!(float_from_int(0), Value::Float(0.0));
        assert_eq!(float_from_int(-5), Value::Float(-5.0));
    }

    #[test]
    fn format_float_always_has_decimal_point_in_plain_range() {
        assert_eq!(format_float(1.0), "1.0");
        assert_eq!(format_float(2.5), "2.5");
        assert_eq!(format_float(-2.0), "-2.0");
    }

    #[test]
    fn format_float_switches_to_scientific_for_extreme_magnitudes() {
        assert_eq!(format_float(1e20), "1e20");
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "The point here is verifying bit-for-bit round-trip fidelity \
                  (parse(format_float(x)) == x), not a tolerance-based numeric comparison, so \
                  strict equality is the intended check"
    )]
    fn format_float_round_trips_via_parse() {
        for x in [0.0, 1.0, -1.0, 2.5, 1e20, 1e-10, 123_456.789] {
            let s = format_float(x);
            let Ok(parsed) = s.parse::<f64>() else {
                panic!("format_float must produce a parseable string, got {s}")
            };
            assert_eq!(parsed, x, "round-trip failed for {x} -> {s}");
        }
    }

    #[test]
    fn str_len_counts_unicode_scalar_values_not_bytes() {
        assert_eq!(str_len("héllo"), Value::Int(5));
    }

    #[test]
    fn str_get_returns_option() {
        let Value::Enum(some) = str_get("abc", 1) else {
            panic!("expected Option")
        };
        assert_eq!(some.variant_name.as_ref(), "Some");
        assert_eq!(some.fields[0], Value::Str(std::sync::Arc::from("b")));
        let Value::Enum(none) = str_get("abc", 10) else {
            panic!("expected Option")
        };
        assert_eq!(none.variant_name.as_ref(), "None");
    }

    #[test]
    fn str_chars_splits_into_single_char_strings() {
        let Value::List(items) = str_chars("ab") else {
            panic!("expected list")
        };
        assert_eq!(
            items.as_ref(),
            &vec![
                Value::Str(std::sync::Arc::from("a")),
                Value::Str(std::sync::Arc::from("b")),
            ]
        );
    }

    #[test]
    fn str_bytes_returns_utf8_bytes() {
        let Value::List(items) = str_bytes("A") else {
            panic!("expected list")
        };
        assert_eq!(items.as_ref(), &vec![Value::Int(65)]);
    }

    #[test]
    fn str_split_splits_by_separator() {
        let Value::List(items) = str_split("a,b,c", ",") else {
            panic!("expected list")
        };
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn str_find_returns_char_index_not_byte_index() {
        let Value::Enum(some) = str_find("héllo", "llo") else {
            panic!("expected Option")
        };
        // "h" and "é" together are 2 elements, so "llo" starts at char index 2 (byte index would
        // be 3).
        assert_eq!(some.fields[0], Value::Int(2));
    }

    #[test]
    fn str_slice_uses_char_indices_and_panics_out_of_range() {
        let Ok(v) = str_slice("héllo", 1, 3, dummy_span()) else {
            panic!("expected Ok")
        };
        assert_eq!(v, Value::Str(std::sync::Arc::from("él")));
        assert!(str_slice("abc", 0, 10, dummy_span()).is_err());
        assert!(str_slice("abc", 2, 1, dummy_span()).is_err());
    }

    #[test]
    fn str_parse_int_and_float_round_trip() {
        assert_eq!(str_parse_int("42"), super::ok_value(Value::Int(42)));
        assert!(
            matches!(str_parse_int("not a number"), Value::Enum(e) if e.variant_name.as_ref() == "Err")
        );
        assert_eq!(str_parse_float("3.5"), super::ok_value(Value::Float(3.5)));
    }
}
