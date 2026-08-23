//! Shared decode/encode dispatch, building the intermediate representation from a `Ty`
//! (ARCHITECTURE.md §5.3).
//!
//! The codec core receives "what the target shape is right now" simply as a `Ty` value and can
//! process it recursively, with no need at all for Rust generics (monomorphization like
//! `decode::<T>`) -- the dynamic `Value` (Yabumi's builtin enum, the T=Value case) is likewise
//! handled naturally as just one branch of the same function. The evaluator never directly
//! references `TypeAnn` (the syntactic type-annotation node) -- everything that resolves a
//! concrete `Ty` from a type annotation is the type-check phase's job, and the evaluator always
//! receives only an already-resolved `Ty`, via `Resolutions::decode_target`.
//!
//! ## Being explicit about the format
//!
//! [`decode`]/[`encode`] take, as-is, the `NamespaceId` (`Json`/`Yaml`/`Toml` -- `Csv` doesn't
//! reach here since it goes through the separate `codec::csv` path) that `dispatch_namespace` in
//! `eval/call.rs` already holds, as their first argument `format`. Internally, no self-detection
//! from syntax is ever performed -- only the single parser/serializer that `format` designates
//! (`parse_to_dynamic`/`dynamic_to_string` in the respective `json`/`yaml`/`toml` module) is
//! called. So passing JSON syntax to `yaml.decode` is only ever interpreted as YAML (the safe
//! subset), and unsupported syntax such as flow style becomes a decode error as-is -- this is the
//! intended behavior (as specified by the task).

pub mod csv;
pub mod json;
pub mod toml;
pub mod yaml;

use crate::eval::env::Program;
use crate::eval::value::{EnumInstance, MapKey, StructInstance, Value};
use crate::stdlib::{err_value, error_value, none_value, ok_value, some_value};
use crate::types::{NamespaceId, Ty};
use indexmap::{IndexMap, IndexSet};
use std::sync::Arc;

// =========================================================================
// Result[T,E] / Option[T] / Error construction (D-TYPE-09/D-STDPOL-05) uses the shared helpers
// in `stdlib/mod.rs` (`ok_value`/`err_value`/`some_value`/`none_value`/`error_value`).
// =========================================================================

/// The `kind: "decode"` Error (the STDLIB.md §3.3 table) that every part of codec returns.
#[must_use]
pub(crate) fn decode_error(message: impl Into<String>) -> Value {
    error_value("decode", message)
}

// =========================================================================
// Construction/destructuring of the dynamic `Value` (Yabumi's builtin enum from STDLIB.md §3.4,
// D-TYPE-10). At runtime it's represented as `eval::value::Value::Enum(type_name="Value", ..)`
// -- note that this is a separate concept from `eval::value::Value` itself (ARCHITECTURE.md
// §3.9), see the comment in value_type.rs.
// =========================================================================

const DYN_VARIANTS: [&str; 7] = ["Null", "Bool", "Int", "Float", "Str", "List", "Dict"];

fn dyn_enum(variant_name: &str, fields: Vec<Value>) -> Value {
    let variant_index = DYN_VARIANTS
        .iter()
        .position(|v| *v == variant_name)
        .unwrap_or(0);
    Value::Enum(Arc::new(EnumInstance {
        type_name: Arc::from("Value"),
        variant_index: u32::try_from(variant_index).unwrap_or(0),
        variant_name: Arc::from(variant_name),
        fields,
    }))
}

#[must_use]
pub(crate) fn dyn_null() -> Value {
    dyn_enum("Null", vec![])
}

#[must_use]
pub(crate) fn dyn_bool(b: bool) -> Value {
    dyn_enum("Bool", vec![Value::Bool(b)])
}

#[must_use]
pub(crate) fn dyn_int(n: i64) -> Value {
    dyn_enum("Int", vec![Value::Int(n)])
}

#[must_use]
pub(crate) fn dyn_float(f: f64) -> Value {
    dyn_enum("Float", vec![Value::Float(f)])
}

#[must_use]
pub(crate) fn dyn_str(s: impl Into<Arc<str>>) -> Value {
    dyn_enum("Str", vec![Value::Str(s.into())])
}

#[must_use]
pub(crate) fn dyn_list(items: Vec<Value>) -> Value {
    dyn_enum("List", vec![Value::List(Arc::new(items))])
}

#[must_use]
pub(crate) fn dyn_dict(map: IndexMap<MapKey, Value>) -> Value {
    dyn_enum("Dict", vec![Value::Dict(Arc::new(map))])
}

/// If `v` is a (tagged) dynamic `Value`, returns `(variant_name, fields)`.
pub(crate) fn dyn_variant(v: &Value) -> Option<(&str, &[Value])> {
    match v {
        Value::Enum(e) if e.type_name.as_ref() == "Value" => {
            Some((e.variant_name.as_ref(), e.fields.as_slice()))
        }
        _ => None,
    }
}

/// A dict key is always `MapKey::Str` (D-TYPE-10: dict keys are fixed as str across json/csv/
/// yaml/toml). Anything else arriving here is a caller bug, so it falls back to an empty string
/// (doesn't panic).
pub(crate) fn map_key_as_str(k: &MapKey) -> &str {
    match k {
        MapKey::Str(s) => s.as_ref(),
        MapKey::Int(_) | MapKey::Bool(_) | MapKey::Tuple(_) => "",
    }
}

/// Converts a dict[K,V]'s K, when it's something other than str (D-TYPE-05 also allows int/
/// bool/tuple), into a str key for json/yaml/toml output (since each codec format's output can
/// only represent the equivalent of dict[str,V]).
pub(crate) fn map_key_to_output_str(k: &MapKey) -> Arc<str> {
    match k {
        MapKey::Str(s) => Arc::clone(s),
        MapKey::Int(n) => Arc::from(n.to_string()),
        MapKey::Bool(b) => Arc::from(b.to_string()),
        MapKey::Tuple(items) => {
            let joined: Vec<String> = items
                .iter()
                .map(|item| map_key_to_output_str(item).to_string())
                .collect();
            Arc::from(format!("({})", joined.join(", ")))
        }
    }
}

/// The shortest round-trip representation that always includes a decimal point or exponent, like
/// `1.0`/`3.14`/`1e20` (borrowing the spirit of D-TYPE-14's `str(x: float)` for float
/// serialization across json/yaml/toml/csv). NaN/Infinity have no standard direct representation
/// in any of json/yaml/toml/csv, so they fall back to `0.0` (a decision made in this file, an
/// edge case not specified by SPEC/STDLIB).
#[must_use]
pub(crate) fn format_float_default(f: f64) -> String {
    if f.is_nan() || f.is_infinite() {
        return "0.0".to_owned();
    }
    let s = format!("{f}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

/// Processes the escape sequences shared by TOML basic strings and YAML double-quoted strings.
/// Unknown and incomplete escapes are rejected.
pub(crate) fn parse_escaped_string_body(label: &str, inner: &str) -> Result<String, String> {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                let cp = u32::from_str_radix(&hex, 16)
                    .map_err(|_| format!("{label}: invalid \\u escape"))?;
                let ch = char::from_u32(cp)
                    .ok_or_else(|| format!("{label}: invalid Unicode code point"))?;
                out.push(ch);
            }
            Some(other) => return Err(format!("{label}: invalid escape '\\{other}'")),
            None => return Err(format!("{label}: incomplete string escape")),
        }
    }
    Ok(out)
}

// =========================================================================
// Conversion between the runtime Value (ARCHITECTURE.md §3.9) and the dynamic Value (D-TYPE-10).
// =========================================================================

/// "Lifts" a concrete runtime `Value` (struct/list/dict/enum, etc.) into a recursive tree of the
/// (tagged) dynamic `Value` (the first stage of encoding). `program` is needed to look up a
/// struct's field names (since `StructInstance` only holds fields positionally, ARCHITECTURE.md
/// §3.9).
fn to_dynamic(value: &Value, program: &Program) -> Value {
    match value {
        // Both void and closure fall back to the same null, since neither has a value worth
        // serializing (the same reasoning as the Closure comparison in §3.9, for
        // clippy::match_same_arms).
        Value::Void | Value::Closure(_) => dyn_null(),
        Value::Int(n) => dyn_int(*n),
        Value::Float(f) => dyn_float(*f),
        Value::Bool(b) => dyn_bool(*b),
        Value::Str(s) => dyn_str(Arc::clone(s)),
        Value::List(items) => dyn_list(items.iter().map(|v| to_dynamic(v, program)).collect()),
        Value::Tuple(items) => dyn_list(items.iter().map(|v| to_dynamic(v, program)).collect()),
        Value::Set(items) => dyn_list(
            items
                .iter()
                .map(|k| to_dynamic(&k.to_value(), program))
                .collect(),
        ),
        Value::Dict(map) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, v) in map.iter() {
                out.insert(
                    MapKey::Str(map_key_to_output_str(k)),
                    to_dynamic(v, program),
                );
            }
            dyn_dict(out)
        }
        Value::Struct(inst) => {
            let mut out = IndexMap::new();
            if let Some(decl) = program.structs.get(inst.type_name.as_ref()) {
                for (field, v) in decl.fields.iter().zip(inst.fields.iter()) {
                    out.insert(MapKey::Str(Arc::clone(&field.name)), to_dynamic(v, program));
                }
            }
            dyn_dict(out)
        }
        Value::Enum(inst) => {
            if inst.type_name.as_ref() == "Value" {
                // Already a dynamic Value, so pass it through as-is (the case of passing a
                // T=Value value into json.encode etc.).
                value.clone()
            } else if matches!(inst.type_name.as_ref(), "Option" | "Result") {
                // Ok(v)/Some(v) -> v as-is, Err(e) -> e as-is, None -> null. Since SPEC/STDLIB
                // doesn't specify a natural representation for encoding Result/Option directly
                // to json etc., the policy is to lift the contents through as-is (a decision
                // made in this file).
                match inst.fields.first() {
                    Some(inner) => to_dynamic(inner, program),
                    None => dyn_null(),
                }
            } else {
                // A user-defined enum: represented in the externally-tagged form
                // {variant_name: fields} (since SPEC/STDLIB doesn't specify this, a decision
                // made in this file -- following serde's externally-tagged representation, the
                // most widely applicable convention).
                let payload = match inst.fields.len() {
                    0 => dyn_null(),
                    1 => to_dynamic(&inst.fields[0], program),
                    _ => dyn_list(inst.fields.iter().map(|v| to_dynamic(v, program)).collect()),
                };
                let mut out = IndexMap::new();
                out.insert(MapKey::Str(Arc::clone(&inst.variant_name)), payload);
                dyn_dict(out)
            }
        }
    }
}

/// "Lowers" a recursive tree of the (tagged) dynamic `Value` into the concrete shape the target
/// `Ty` requires (the second stage of decoding). On success returns an `eval::value::Value` in
/// the shape `Ty` requires; on failure returns an `Error` struct value (not yet wrapped in a
/// Result).
fn lower(target: &Ty, dynamic: &Value, program: &Program) -> Result<Value, Value> {
    // T=Value: this is the dynamic decode itself, so return it as-is.
    if let Ty::Named { name, .. } = target
        && name.as_ref() == "Value"
    {
        return Ok(dynamic.clone());
    }

    let Some((variant, fields)) = dyn_variant(dynamic) else {
        // Since parse_to_dynamic always produces a tagged dynamic Value, reaching here can only
        // be an internal inconsistency (unreachable at runtime for a type-checked program, the
        // same treatment as the "type-checked already, so" cases elsewhere in the D-CLI family).
        return Err(decode_error(
            "internal error: the dynamic intermediate representation is invalid",
        ));
    };

    match target {
        Ty::Int => match (variant, fields.first()) {
            ("Int", Some(Value::Int(n))) => Ok(Value::Int(*n)),
            _ => Err(decode_error(format!("expected int but found {variant}"))),
        },
        Ty::Float => match (variant, fields.first()) {
            ("Float", Some(Value::Float(f))) => Ok(Value::Float(*f)),
            // A widening int -> float conversion always succeeds (in the spirit of D-TYPE-14).
            #[expect(
                clippy::cast_precision_loss,
                reason = "D-TYPE-14: int->float may lose mantissa precision (53 bits), but \
                          that's not itself an error -- it's the language spec's established \
                          behavior"
            )]
            ("Int", Some(Value::Int(n))) => Ok(Value::Float(*n as f64)),
            _ => Err(decode_error(format!("expected float but found {variant}"))),
        },
        Ty::Bool => match (variant, fields.first()) {
            ("Bool", Some(Value::Bool(b))) => Ok(Value::Bool(*b)),
            _ => Err(decode_error(format!("expected bool but found {variant}"))),
        },
        Ty::Str => match (variant, fields.first()) {
            ("Str", Some(Value::Str(s))) => Ok(Value::Str(Arc::clone(s))),
            _ => Err(decode_error(format!("expected str but found {variant}"))),
        },
        Ty::List(elem) => lower_list(elem, variant, fields, program),
        Ty::Set(elem) => lower_set(elem, variant, fields, program),
        Ty::Tuple(items_ty) => lower_tuple(items_ty, variant, fields, program),
        Ty::Dict(key_ty, val_ty) => lower_dict(key_ty, val_ty, variant, fields, program),
        Ty::Named { name, args } if name.as_ref() == "Option" && args.len() == 1 => {
            if variant == "Null" {
                Ok(none_value())
            } else {
                Ok(some_value(lower(&args[0], dynamic, program)?))
            }
        }
        Ty::Named { name, args } if program.enums.contains_key(name.as_ref()) => {
            lower_enum(name, args, dynamic, program)
        }
        Ty::Named { name, args } => lower_struct(name, args, variant, fields, program),
        Ty::Void | Ty::Function { .. } | Ty::TypeVar(_) | Ty::Unknown => {
            Err(decode_error("decoding into this type is not supported"))
        }
    }
}

fn lower_list(
    elem: &Ty,
    variant: &str,
    fields: &[Value],
    program: &Program,
) -> Result<Value, Value> {
    match (variant, fields.first()) {
        ("List", Some(Value::List(items))) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                out.push(lower(elem, item, program)?);
            }
            Ok(Value::List(Arc::new(out)))
        }
        _ => Err(decode_error(format!("expected list but found {variant}"))),
    }
}

fn lower_set(
    elem: &Ty,
    variant: &str,
    fields: &[Value],
    program: &Program,
) -> Result<Value, Value> {
    match (variant, fields.first()) {
        ("List", Some(Value::List(items))) => {
            let mut out = IndexSet::new();
            for item in items.iter() {
                let lowered = lower(elem, item, program)?;
                let Some(key) = MapKey::try_from_value(&lowered) else {
                    return Err(decode_error(
                        "a set's elements must be int/str/bool/tuple only",
                    ));
                };
                out.insert(key);
            }
            Ok(Value::Set(Arc::new(out)))
        }
        _ => Err(decode_error(format!("expected set but found {variant}"))),
    }
}

fn lower_tuple(
    items_ty: &[Ty],
    variant: &str,
    fields: &[Value],
    program: &Program,
) -> Result<Value, Value> {
    match (variant, fields.first()) {
        ("List", Some(Value::List(items))) if items.len() == items_ty.len() => {
            let mut out = Vec::with_capacity(items.len());
            for (t, item) in items_ty.iter().zip(items.iter()) {
                out.push(lower(t, item, program)?);
            }
            Ok(Value::Tuple(out.into()))
        }
        _ => Err(decode_error(format!(
            "expected a tuple with {} elements",
            items_ty.len()
        ))),
    }
}

fn lower_dict(
    key_ty: &Ty,
    val_ty: &Ty,
    variant: &str,
    fields: &[Value],
    program: &Program,
) -> Result<Value, Value> {
    match (variant, fields.first()) {
        ("Dict", Some(Value::Dict(map))) if matches!(*key_ty, Ty::Str) => {
            let mut out = IndexMap::with_capacity(map.len());
            for (k, v) in map.iter() {
                out.insert(k.clone(), lower(val_ty, v, program)?);
            }
            Ok(Value::Dict(Arc::new(out)))
        }
        _ => Err(decode_error("expected dict[str, V]")),
    }
}

fn lower_struct(
    name: &Arc<str>,
    args: &[Ty],
    variant: &str,
    fields: &[Value],
    program: &Program,
) -> Result<Value, Value> {
    let Some(declaration) = program.structs.get(name.as_ref()) else {
        return Err(decode_error(format!(
            "decode target type '{name}' was not found"
        )));
    };
    let substitution = declaration
        .generics
        .iter()
        .cloned()
        .zip(args.iter().cloned())
        .collect();
    match (variant, fields.first()) {
        ("Dict", Some(Value::Dict(map))) => {
            let mut output = Vec::with_capacity(declaration.fields.len());
            for field in &declaration.fields {
                let key = MapKey::Str(Arc::clone(&field.name));
                let Some(dynamic_field) = map.get(&key) else {
                    return Err(decode_error(format!("field '{}' is missing", field.name)));
                };
                let Some(field_type) =
                    crate::types::generics::ty_from_ann(&field.ty, &declaration.generics, program)
                else {
                    return Err(decode_error(format!(
                        "could not resolve the type of field '{}'",
                        field.name
                    )));
                };
                let field_type = crate::types::generics::substitute(&field_type, &substitution);
                output.push(lower(&field_type, dynamic_field, program)?);
            }
            Ok(Value::Struct(Arc::new(StructInstance {
                type_name: Arc::clone(name),
                fields: output,
            })))
        }
        _ => Err(decode_error(format!(
            "decoding struct '{name}' requires an object"
        ))),
    }
}

fn lower_enum(
    name: &Arc<str>,
    args: &[Ty],
    dynamic: &Value,
    program: &Program,
) -> Result<Value, Value> {
    let Some(declaration) = program.enums.get(name.as_ref()) else {
        return Err(decode_error(format!("enum '{name}' was not found")));
    };
    let Some(("Dict", dynamic_fields)) = dyn_variant(dynamic) else {
        return Err(decode_error(format!(
            "decoding enum '{name}' requires an externally tagged object"
        )));
    };
    let Some(Value::Dict(tagged)) = dynamic_fields.first() else {
        return Err(decode_error("enum representation is not an object"));
    };
    if tagged.len() != 1 {
        return Err(decode_error("enum object must contain exactly one variant"));
    }
    let Some((MapKey::Str(variant_name), payload)) = tagged.first() else {
        return Err(decode_error("enum variant name must be a string"));
    };
    let Some((variant_index, variant)) = declaration
        .variants
        .iter()
        .enumerate()
        .find(|(_, variant)| variant.name.as_ref() == variant_name.as_ref())
    else {
        return Err(decode_error(format!(
            "unknown variant '{variant_name}' for enum '{name}'"
        )));
    };
    let substitution = declaration
        .generics
        .iter()
        .cloned()
        .zip(args.iter().cloned())
        .collect();
    let field_types: Option<Vec<Ty>> = variant
        .fields
        .iter()
        .map(|field| {
            crate::types::generics::ty_from_ann(field, &declaration.generics, program)
                .map(|field| crate::types::generics::substitute(&field, &substitution))
        })
        .collect();
    let Some(field_types) = field_types else {
        return Err(decode_error("could not resolve enum field types"));
    };
    let fields = match field_types.as_slice() {
        [] => Vec::new(),
        [field_type] => vec![lower(field_type, payload, program)?],
        field_types => {
            let Some(("List", payload_fields)) = dyn_variant(payload) else {
                return Err(decode_error("multi-field enum payload must be an array"));
            };
            let Some(Value::List(payload_fields)) = payload_fields.first() else {
                return Err(decode_error("multi-field enum payload must be an array"));
            };
            if payload_fields.len() != field_types.len() {
                return Err(decode_error("enum payload field count does not match"));
            }
            field_types
                .iter()
                .zip(payload_fields.iter())
                .map(|(field_type, field)| lower(field_type, field, program))
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok(Value::Enum(Arc::new(EnumInstance {
        type_name: Arc::clone(name),
        variant_index: u32::try_from(variant_index)
            .unwrap_or_else(|_| unreachable!("enum variant count fits u32")),
        variant_name: Arc::clone(variant_name),
        fields,
    })))
}

/// Parses text into the dynamic intermediate representation using only the single codec that
/// `format` designates. (`Csv` goes through the separate `codec::csv::decode` path, so reaching
/// here always means Json/Yaml/Toml.)
fn parse_dynamic(format: NamespaceId, text: &str) -> Result<Value, String> {
    match format {
        NamespaceId::Json => json::parse_to_dynamic(text).map_err(|e| format!("json: {e}")),
        NamespaceId::Yaml => yaml::parse_to_dynamic(text).map_err(|e| format!("yaml: {e}")),
        NamespaceId::Toml => toml::parse_to_dynamic(text).map_err(|e| format!("toml: {e}")),
        NamespaceId::Fs
        | NamespaceId::Http
        | NamespaceId::Env
        | NamespaceId::Proc
        | NamespaceId::Time
        | NamespaceId::Rand
        | NamespaceId::Regex
        | NamespaceId::Math
        | NamespaceId::Csv => {
            unreachable!("dispatch_namespace only calls codec::decode for Json/Yaml/Toml")
        }
    }
}

/// The shared decode implementation across each codec (json/yaml/toml). `format` is simply the
/// `NamespaceId` forwarded as-is from the caller (`dispatch_namespace` in `eval/call.rs`, see the
/// comment at the top of this module). `target` is the target type resolved either by
/// assignment-target-annotation-driven inference (D-TYPE-16) or an explicit `[T]`. A failure is
/// returned not as a Rust Abort but as an ordinary `Value` at the Yabumi level,
/// `Err(Error{kind:"decode",..})` (a generalization of the policy stated at the end of D-ERR-04:
/// a string-to-number parse failure is excluded from the set of panicking operations).
#[must_use]
pub fn decode(format: NamespaceId, target: &Ty, text: &str, program: &Program) -> Value {
    match parse_dynamic(format, text) {
        Ok(dynamic) => match lower(target, &dynamic, program) {
            Ok(v) => ok_value(v),
            Err(e) => err_value(e),
        },
        Err(message) => err_value(decode_error(message)),
    }
}

/// The shared encode implementation across codecs. Calls only the serializer for the single codec
/// `format` designates (see the comment at the top of this module).
#[must_use]
pub fn encode(format: NamespaceId, value: &Value, program: &Program) -> Value {
    let dynamic = to_dynamic(value, program);
    let text = match format {
        NamespaceId::Json => json::dynamic_to_string(&dynamic),
        NamespaceId::Yaml => yaml::dynamic_to_string(&dynamic),
        NamespaceId::Toml => toml::dynamic_to_string(&dynamic),
        NamespaceId::Fs
        | NamespaceId::Http
        | NamespaceId::Env
        | NamespaceId::Proc
        | NamespaceId::Time
        | NamespaceId::Rand
        | NamespaceId::Regex
        | NamespaceId::Math
        | NamespaceId::Csv => {
            unreachable!("dispatch_namespace only calls codec::encode for Json/Yaml/Toml")
        }
    };
    Value::Str(Arc::from(text))
}

#[cfg(test)]
mod tests {
    use super::{decode, dyn_variant, encode};
    use crate::diagnostics::SourceMap;
    use crate::eval::env::Program;
    use crate::eval::value::{MapKey, Value};
    use crate::types::{NamespaceId, Ty};
    use indexmap::IndexMap;
    use std::sync::Arc;

    fn test_program() -> Program {
        Program::new(Arc::new(SourceMap::new()))
    }

    /// The target type for T=Value (dynamic decode, D-TYPE-10).
    fn dyn_value_ty() -> Ty {
        Ty::Named {
            name: Arc::from("Value"),
            args: vec![],
        }
    }

    /// A runtime `Value::Dict` equivalent to `{name: "alice", age: 30}` (a dict[str, V]
    /// representation that needs no struct declaration; `to_dynamic` can lift this as-is).
    fn sample_dict() -> Value {
        let mut map = IndexMap::new();
        map.insert(
            MapKey::Str(Arc::from("name")),
            Value::Str(Arc::from("alice")),
        );
        map.insert(MapKey::Str(Arc::from("age")), Value::Int(30));
        Value::Dict(Arc::new(map))
    }

    fn unwrap_str(v: &Value) -> &str {
        match v {
            Value::Str(s) => s.as_ref(),
            other => panic!("expected Value::Str, got {other:?}"),
        }
    }

    /// A `decode`/`encode` failure is not a panic but an ordinary Yabumi value, `Err(Error)`
    /// (see the comment at the top of this module), so the test assertions also judge it by the
    /// Enum (Result)'s variant name.
    fn expect_ok(v: &Value) -> &Value {
        match v {
            Value::Enum(e) if e.variant_name.as_ref() == "Ok" => &e.fields[0],
            other => panic!("expected Ok(..), got {other:?}"),
        }
    }

    fn expect_err(v: &Value) {
        assert!(
            matches!(v, Value::Enum(e) if e.variant_name.as_ref() == "Err"),
            "expected Err(..), got {v:?}"
        );
    }

    fn dict_get<'a>(map: &'a IndexMap<MapKey, Value>, key: &str) -> &'a Value {
        match map.get(&MapKey::Str(Arc::from(key))) {
            Some(v) => v,
            None => panic!("missing key {key}"),
        }
    }

    /// The regression this test pins down: `encode` outputs only the single format its Format
    /// argument designates (previously a known bug where `ns` wasn't forwarded and output was
    /// always hardcoded to JSON, see the task).
    #[test]
    fn encode_uses_the_format_actually_requested() {
        let program = test_program();
        let value = sample_dict();

        let json_out = encode(NamespaceId::Json, &value, &program);
        let yaml_out = encode(NamespaceId::Yaml, &value, &program);
        let toml_out = encode(NamespaceId::Toml, &value, &program);

        let json_text = unwrap_str(&json_out);
        let yaml_text = unwrap_str(&yaml_out);
        let toml_text = unwrap_str(&toml_out);

        // json.encode: compact JSON syntax ({"k":v,...}).
        assert_eq!(json_text, r#"{"name":"alice","age":30}"#);
        // yaml.encode: actually returns YAML (block style) -- the spot that, before the fix,
        // matched the JSON string.
        assert_eq!(yaml_text, "name: alice\nage: 30\n");
        // toml.encode: actually returns TOML (key = value) -- likewise.
        assert_eq!(toml_text, "name = \"alice\"\nage = 30\n");

        // The 3 formats must all differ from each other (a direct check that none of them
        // collapsed to JSON).
        assert_ne!(yaml_text, json_text);
        assert_ne!(toml_text, json_text);
        assert_ne!(yaml_text, toml_text);
    }

    /// `decode` likewise uses only the single parser its Format argument designates. Verifies
    /// that each format's `encode` output can be read back by `decode` of the same format (a
    /// regression check that this path never relies on self-detection [e.g. trying JSON -> TOML
    /// -> YAML in order]).
    #[test]
    fn decode_round_trips_each_format_through_its_own_codec() {
        let program = test_program();
        let target = dyn_value_ty();

        for ns in [NamespaceId::Json, NamespaceId::Yaml, NamespaceId::Toml] {
            let encoded = encode(ns, &sample_dict(), &program);
            let text = unwrap_str(&encoded).to_owned();

            let decoded = decode(ns, &target, &text, &program);
            let dynamic = expect_ok(&decoded);
            let Some(("Dict", fields)) = dyn_variant(dynamic) else {
                panic!("{ns:?}: expected dynamic Dict, got {dynamic:?}");
            };
            let Some(Value::Dict(map)) = fields.first() else {
                panic!("{ns:?}: expected dict payload");
            };
            assert!(
                matches!(
                    dyn_variant(dict_get(map, "name")),
                    Some(("Str", f)) if matches!(f.first(), Some(Value::Str(s)) if s.as_ref() == "alice")
                ),
                "{ns:?}: name mismatch"
            );
            assert!(
                matches!(
                    dyn_variant(dict_get(map, "age")),
                    Some(("Int", f)) if matches!(f.first(), Some(Value::Int(30)))
                ),
                "{ns:?}: age mismatch"
            );
        }
    }

    /// Passing JSON-specific flow style (`{"a": 1}`) to `yaml.decode` becomes Err, since the
    /// safe-subset YAML parser never handles flow style at all -- this pins down the behavior
    /// that, in the old implementation which relied on self-detection, this would have been
    /// accepted just like `json.decode`.
    #[test]
    fn yaml_decode_rejects_json_flow_style_object() {
        let program = test_program();
        let target = dyn_value_ty();

        let decoded = decode(NamespaceId::Yaml, &target, r#"{"a": 1}"#, &program);
        expect_err(&decoded);

        let decoded_array = decode(NamespaceId::Yaml, &target, "[1, 2, 3]", &program);
        expect_err(&decoded_array);
    }

    /// Control check: the same JSON flow-style input naturally succeeds with `json.decode`
    /// (confirms that yaml's rejection is a result of the correct format argument, not a format
    /// misdetection).
    #[test]
    fn json_decode_accepts_the_same_flow_style_object() {
        let program = test_program();
        let target = dyn_value_ty();

        let decoded = decode(NamespaceId::Json, &target, r#"{"a": 1}"#, &program);
        let dynamic = expect_ok(&decoded);
        assert!(matches!(dyn_variant(dynamic), Some(("Dict", _))));
    }
}
