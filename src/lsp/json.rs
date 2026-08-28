use crate::eval::value::{MapKey, Value};
use crate::stdlib::codec::json::write_json_string;
use crate::stdlib::codec::{dyn_variant, json as codec_json};
use indexmap::IndexMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(IndexMap<String, Json>),
}

impl Json {
    pub(crate) fn obj(items: Vec<(&str, Json)>) -> Self {
        Self::Obj(
            items
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Obj(object) => object.get(key),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Self::Arr(values) => Some(values),
            _ => None,
        }
    }

    pub(crate) fn as_obj(&self) -> Option<&IndexMap<String, Json>> {
        match self {
            Self::Obj(object) => Some(object),
            _ => None,
        }
    }
}

impl fmt::Display for Json {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut output = String::new();
        write_json(self, &mut output);
        formatter.write_str(&output)
    }
}

pub(crate) fn parse(text: &str) -> Result<Json, String> {
    let value = codec_json::parse_to_dynamic(text)?;
    from_dynamic(&value)
}

fn from_dynamic(value: &Value) -> Result<Json, String> {
    let Some((variant, fields)) = dyn_variant(value) else {
        return Err("JSON: invalid dynamic value".to_owned());
    };
    match variant {
        "Null" if fields.is_empty() => Ok(Json::Null),
        "Bool" => match fields {
            [Value::Bool(value)] => Ok(Json::Bool(*value)),
            _ => Err("JSON: invalid bool value".to_owned()),
        },
        "Int" => match fields {
            [Value::Int(value)] => Ok(Json::Int(*value)),
            _ => Err("JSON: invalid integer value".to_owned()),
        },
        "Float" => match fields {
            [Value::Float(value)] if value.is_finite() => Ok(Json::Float(*value)),
            [Value::Float(_)] => Err("JSON: number is outside the finite range".to_owned()),
            _ => Err("JSON: invalid float value".to_owned()),
        },
        "Str" => match fields {
            [Value::Str(value)] => Ok(Json::Str(value.to_string())),
            _ => Err("JSON: invalid string value".to_owned()),
        },
        "List" => match fields {
            [Value::List(values)] => values
                .iter()
                .map(from_dynamic)
                .collect::<Result<Vec<_>, _>>()
                .map(Json::Arr),
            _ => Err("JSON: invalid array value".to_owned()),
        },
        "Dict" => match fields {
            [Value::Dict(values)] => {
                let mut object = IndexMap::new();
                for (key, value) in values.iter() {
                    let MapKey::Str(key) = key else {
                        return Err("JSON: object key is not a string".to_owned());
                    };
                    object.insert(key.to_string(), from_dynamic(value)?);
                }
                Ok(Json::Obj(object))
            }
            _ => Err("JSON: invalid object value".to_owned()),
        },
        _ => Err("JSON: invalid dynamic value".to_owned()),
    }
}

fn write_json(value: &Json, output: &mut String) {
    match value {
        Json::Null => output.push_str("null"),
        Json::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Json::Int(value) => output.push_str(&value.to_string()),
        Json::Float(value) => {
            let text = value.to_string();
            output.push_str(&text);
            if !text.contains(['.', 'e', 'E']) {
                output.push_str(".0");
            }
        }
        Json::Str(value) => write_json_string(value, output),
        Json::Arr(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json(value, output);
            }
            output.push(']');
        }
        Json::Obj(object) => {
            output.push('{');
            for (index, (key, value)) in object.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json_string(key, output);
                output.push(':');
                write_json(value, output);
            }
            output.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Json, parse};

    fn must<T>(result: Result<T, String>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("expected JSON to parse: {error}"),
        }
    }

    #[test]
    fn round_trips_protocol_values() {
        let value = Json::obj(vec![
            ("ok", Json::Bool(true)),
            ("items", Json::Arr(vec![Json::Int(3), Json::Float(2.5)])),
            ("none", Json::Null),
        ]);
        let parsed = must(parse(&value.to_string()));
        assert_eq!(parsed, value);
    }

    #[test]
    fn escapes_and_decodes_strings() {
        let value = Json::Str("quote \" slash \\ newline\n tab\t control\u{1f}".to_owned());
        let encoded = value.to_string();
        assert_eq!(
            encoded,
            "\"quote \\\" slash \\\\ newline\\n tab\\t control\\u001f\""
        );
        assert_eq!(must(parse(&encoded)), value);
        assert_eq!(must(parse(r#""😀""#)), Json::Str("😀".to_owned()));
    }

    #[test]
    fn preserves_integral_float_values_when_serializing() {
        let value = Json::Float(1.0);
        assert_eq!(value.to_string(), "1.0");
        assert_eq!(must(parse(&value.to_string())), value);
    }

    #[test]
    fn rejects_invalid_protocol_values() {
        assert!(parse("1e999").is_err());
        assert!(parse("{").is_err());
    }
}
