//! Methods on the dynamic `Value` type (STDLIB.md §3.4, ARCHITECTURE.md §2.1).
//!
//! Naming collision warning: this is distinct from `eval::value::Value` in ARCHITECTURE.md (the
//! representation of all runtime values) -- what's handled here is Yabumi's builtin
//! `enum Value { Null, Bool, Int, Float, Str, List, Dict }` (the dynamic schema returned by
//! codec/decode_rows etc.), which at runtime is represented as `eval::value::Value::Enum` (with
//! type_name="Value").

use crate::eval::value::{EnumInstance, MapKey, Value};
use crate::stdlib::{none_value, some_value};
use std::sync::Arc;

/// Declaration order matching D-TYPE-10 (kept in sync with `builtin_variant_info` in
/// `eval/call.rs`): Null=0, Bool=1, Int=2, Float=3, Str=4, List=5, Dict=6.
mod variant {
    pub const NULL: u32 = 0;
    pub const BOOL: u32 = 1;
    pub const INT: u32 = 2;
    pub const FLOAT: u32 = 3;
    pub const STR: u32 = 4;
    pub const LIST: u32 = 5;
    pub const DICT: u32 = 6;
}

/// If `self_` is the variant at `expected_idx`, wraps its payload (always 1 field -- every
/// variant except the unit variant `Null`) in `Some`.
fn payload_if(self_: &EnumInstance, expected_idx: u32) -> Value {
    if self_.variant_index == expected_idx {
        some_value(self_.fields[0].clone())
    } else {
        none_value()
    }
}

#[must_use]
pub fn as_int(self_: &EnumInstance) -> Value {
    payload_if(self_, variant::INT)
}

#[must_use]
pub fn as_float(self_: &EnumInstance) -> Value {
    payload_if(self_, variant::FLOAT)
}

#[must_use]
pub fn as_str(self_: &EnumInstance) -> Value {
    payload_if(self_, variant::STR)
}

#[must_use]
pub fn as_bool(self_: &EnumInstance) -> Value {
    payload_if(self_, variant::BOOL)
}

#[must_use]
pub fn as_list(self_: &EnumInstance) -> Value {
    payload_if(self_, variant::LIST)
}

#[must_use]
pub fn as_dict(self_: &EnumInstance) -> Value {
    payload_if(self_, variant::DICT)
}

#[must_use]
pub fn is_null(self_: &EnumInstance) -> Value {
    Value::Bool(self_.variant_index == variant::NULL)
}

/// `get(self: Value, key: str): Option[Value]`. Only for the Dict variant; None if the key is
/// missing.
#[must_use]
pub fn value_get(self_: &EnumInstance, key: &str) -> Value {
    if self_.variant_index != variant::DICT {
        return none_value();
    }
    let Value::Dict(m) = &self_.fields[0] else {
        unreachable!(
            "D-TYPE-10: fields[0] of the Dict variant is always Value::Dict(dict[str, Value])"
        )
    };
    match m.get(&MapKey::Str(Arc::from(key))) {
        Some(v) => some_value(v.clone()),
        None => none_value(),
    }
}

/// `at(self: Value, i: int): Option[Value]`. Only for the List variant.
#[must_use]
pub fn value_at(self_: &EnumInstance, i: i64) -> Value {
    if self_.variant_index != variant::LIST {
        return none_value();
    }
    let Value::List(xs) = &self_.fields[0] else {
        unreachable!("D-TYPE-10: fields[0] of the List variant is always Value::List(list[Value])")
    };
    match usize::try_from(i).ok().and_then(|idx| xs.get(idx)) {
        Some(v) => some_value(v.clone()),
        None => none_value(),
    }
}

#[cfg(test)]
mod tests {
    use super::{as_bool, as_dict, as_int, as_list, as_str, is_null, value_at, value_get};
    use crate::eval::value::{EnumInstance, MapKey, Value};
    use crate::stdlib::{none_value, some_value};
    use indexmap::IndexMap;
    use std::sync::Arc;

    fn value_enum(variant_index: u32, variant_name: &str, fields: Vec<Value>) -> EnumInstance {
        EnumInstance {
            type_name: Arc::from("Value"),
            variant_index,
            variant_name: Arc::from(variant_name),
            fields,
        }
    }

    #[test]
    fn as_helpers_return_some_only_for_matching_variant() {
        let int_v = value_enum(2, "Int", vec![Value::Int(5)]);
        assert_eq!(as_int(&int_v), some_value(Value::Int(5)));
        assert_eq!(as_str(&int_v), none_value());
        assert_eq!(as_bool(&int_v), none_value());

        let bool_v = value_enum(1, "Bool", vec![Value::Bool(true)]);
        assert_eq!(as_bool(&bool_v), some_value(Value::Bool(true)));
    }

    #[test]
    fn is_null_only_true_for_null_variant() {
        let null_v = value_enum(0, "Null", vec![]);
        assert_eq!(is_null(&null_v), Value::Bool(true));
        let int_v = value_enum(2, "Int", vec![Value::Int(1)]);
        assert_eq!(is_null(&int_v), Value::Bool(false));
    }

    #[test]
    fn value_get_only_works_on_dict_variant() {
        let mut m = IndexMap::new();
        m.insert(MapKey::Str(Arc::from("a")), Value::Int(1));
        let dict_v = value_enum(6, "Dict", vec![Value::Dict(Arc::new(m))]);
        assert_eq!(value_get(&dict_v, "a"), some_value(Value::Int(1)));
        assert_eq!(value_get(&dict_v, "missing"), none_value());

        let int_v = value_enum(2, "Int", vec![Value::Int(1)]);
        assert_eq!(value_get(&int_v, "a"), none_value());
    }

    #[test]
    fn value_at_only_works_on_list_variant() {
        let list_v = value_enum(5, "List", vec![Value::List(Arc::new(vec![Value::Int(9)]))]);
        assert_eq!(value_at(&list_v, 0), some_value(Value::Int(9)));
        assert_eq!(value_at(&list_v, 1), none_value());
        assert_eq!(value_at(&list_v, -1), none_value());

        let str_v = value_enum(4, "Str", vec![Value::Str(Arc::from("x"))]);
        assert_eq!(value_at(&str_v, 0), none_value());
    }

    #[test]
    fn as_list_and_as_dict() {
        let list_v = value_enum(5, "List", vec![Value::List(Arc::new(vec![]))]);
        assert_eq!(as_list(&list_v), some_value(Value::List(Arc::new(vec![]))));
        let int_v = value_enum(2, "Int", vec![Value::Int(1)]);
        assert_eq!(as_dict(&int_v), none_value());
    }
}
