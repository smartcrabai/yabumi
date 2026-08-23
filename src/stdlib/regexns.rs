//! regex namespace (a wrapper around the `regex` crate, STDLIB.md §11, ARCHITECTURE.md §2.1).
//! No effect (pure). Has no dedicated Regex type -- the pattern is always passed as a `str`
//! (D-STDPOL-07). An invalid pattern is returned as a Result rather than panicking.

use crate::eval::value::Value;
use crate::stdlib::{err_value, error_value, none_value, ok_value, some_value};
use std::sync::Arc;

/// Converts a `regex::Regex::new` compile error into a `Value` (Error struct) with
/// `kind: "regex"`. The caller wraps this further in `Err(..)` (an intermediate representation
/// of `Result<regex::Regex, Value>`).
fn compile(pattern: &str) -> Result<regex::Regex, Value> {
    regex::Regex::new(pattern).map_err(|e| error_value("regex", e.to_string()))
}

#[must_use]
pub fn is_match(pattern: &str, s: &str) -> Value {
    match compile(pattern) {
        Ok(re) => ok_value(Value::Bool(re.is_match(s))),
        Err(e) => err_value(e),
    }
}

#[must_use]
pub fn find(pattern: &str, s: &str) -> Value {
    match compile(pattern) {
        Ok(re) => {
            let found = match re.find(s) {
                Some(m) => some_value(Value::Str(Arc::from(m.as_str()))),
                None => none_value(),
            };
            ok_value(found)
        }
        Err(e) => err_value(e),
    }
}

#[must_use]
pub fn find_all(pattern: &str, s: &str) -> Value {
    match compile(pattern) {
        Ok(re) => {
            let items = re
                .find_iter(s)
                .map(|m| Value::Str(Arc::from(m.as_str())))
                .collect();
            ok_value(Value::List(Arc::new(items)))
        }
        Err(e) => err_value(e),
    }
}

/// Replaces only the first match. `replacement` is passed through as-is using the `regex`
/// crate's standard replacement syntax (expanding capture references like `$1`, etc.) --
/// STDLIB.md doesn't itself specify a replacement syntax, so we follow the underlying crate's
/// natural behavior (a decision made in this file).
#[must_use]
pub fn replace(pattern: &str, s: &str, replacement: &str) -> Value {
    match compile(pattern) {
        Ok(re) => ok_value(Value::Str(Arc::from(
            re.replace(s, replacement).into_owned(),
        ))),
        Err(e) => err_value(e),
    }
}

#[must_use]
pub fn replace_all(pattern: &str, s: &str, replacement: &str) -> Value {
    match compile(pattern) {
        Ok(re) => ok_value(Value::Str(Arc::from(
            re.replace_all(s, replacement).into_owned(),
        ))),
        Err(e) => err_value(e),
    }
}

/// Index 0 is the whole match. Any unmatched capture group is treated as an empty string
/// (STDLIB.md's return type is `list[str]`, not `list[Option[str]]`, so this is a decision made
/// in this file).
#[must_use]
pub fn captures(pattern: &str, s: &str) -> Value {
    match compile(pattern) {
        Ok(re) => {
            let found = re.captures(s).map(|caps| {
                let items = caps
                    .iter()
                    .map(|m| Value::Str(Arc::from(m.map_or("", |mm| mm.as_str()))))
                    .collect();
                Value::List(Arc::new(items))
            });
            ok_value(match found {
                Some(v) => some_value(v),
                None => none_value(),
            })
        }
        Err(e) => err_value(e),
    }
}

#[cfg(test)]
mod tests {
    use super::{captures, find, find_all, is_match, replace, replace_all};
    use crate::eval::value::Value;

    fn assert_ok(v: Value) -> Value {
        let Value::Enum(inst) = v else {
            panic!("expected Result")
        };
        assert_eq!(inst.variant_name.as_ref(), "Ok");
        inst.fields[0].clone()
    }

    fn assert_err_kind(v: Value, kind: &str) {
        let Value::Enum(inst) = v else {
            panic!("expected Result")
        };
        assert_eq!(inst.variant_name.as_ref(), "Err");
        let Value::Struct(err) = &inst.fields[0] else {
            panic!("expected Error struct")
        };
        assert_eq!(err.fields[0], Value::Str(std::sync::Arc::from(kind)));
    }

    #[test]
    fn is_match_true_and_false() {
        assert_eq!(assert_ok(is_match(r"\d+", "abc123")), Value::Bool(true));
        assert_eq!(assert_ok(is_match(r"^\d+$", "abc123")), Value::Bool(false));
    }

    #[test]
    fn invalid_pattern_is_err_with_regex_kind() {
        assert_err_kind(is_match("(", "abc"), "regex");
    }

    #[test]
    fn find_returns_first_match() {
        let inner = assert_ok(find(r"\d+", "a12b34"));
        let Value::Enum(opt) = inner else {
            panic!("expected Option")
        };
        assert_eq!(opt.variant_name.as_ref(), "Some");
        assert_eq!(opt.fields[0], Value::Str(std::sync::Arc::from("12")));
    }

    #[test]
    fn find_all_returns_all_matches() {
        let inner = assert_ok(find_all(r"\d+", "a12b34"));
        let Value::List(items) = inner else {
            panic!("expected list")
        };
        assert_eq!(
            items.as_ref(),
            &vec![
                Value::Str(std::sync::Arc::from("12")),
                Value::Str(std::sync::Arc::from("34")),
            ]
        );
    }

    #[test]
    fn replace_replaces_first_only() {
        let inner = assert_ok(replace(r"\d", "a1b2", "X"));
        assert_eq!(inner, Value::Str(std::sync::Arc::from("aXb2")));
    }

    #[test]
    fn replace_all_replaces_every_match() {
        let inner = assert_ok(replace_all(r"\d", "a1b2", "X"));
        assert_eq!(inner, Value::Str(std::sync::Arc::from("aXbX")));
    }

    #[test]
    fn captures_index_zero_is_whole_match() {
        let inner = assert_ok(captures(r"(\d+)-(\d+)", "12-34"));
        let Value::Enum(opt) = inner else {
            panic!("expected Option")
        };
        let Value::List(items) = &opt.fields[0] else {
            panic!("expected list")
        };
        assert_eq!(
            items.as_ref(),
            &vec![
                Value::Str(std::sync::Arc::from("12-34")),
                Value::Str(std::sync::Arc::from("12")),
                Value::Str(std::sync::Arc::from("34")),
            ]
        );
    }
}
