//! JSON decode/encode (STDLIB.md §4.1, ARCHITECTURE.md §2.1). Implemented from scratch without
//! serde (owner's ruling, ARCHITECTURE.md §1.6). JSON5/XML are out of scope (SPEC §11.1) -- this
//! parser only accepts standard JSON (RFC 8259).

use super::{
    dyn_bool, dyn_dict, dyn_float, dyn_int, dyn_list, dyn_null, dyn_str, dyn_variant,
    map_key_as_str,
};
use crate::eval::value::{MapKey, Value};
use indexmap::IndexMap;
use std::sync::Arc;

/// Parses JSON text into the dynamic intermediate representation (`Value`, read first as a
/// recursive tree of eval::value::Value). A syntax error is converted into an Err with
/// kind="decode" outside this function (in codec::mod::decode).
pub fn parse_to_dynamic(text: &str) -> Result<Value, String> {
    let mut p = Parser {
        chars: text.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let value = p.parse_value(0)?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err(format!(
            "JSON: extra characters after the value (position {})",
            p.pos
        ));
    }
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

const MAX_NESTING_DEPTH: usize = 256;

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<(), String> {
        match self.bump() {
            Some(c) if c == expected => Ok(()),
            Some(c) => Err(format!(
                "JSON: expected '{expected}' at position {} but found '{c}'",
                self.pos - 1
            )),
            None => Err(format!("JSON: input ended before '{expected}'")),
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_NESTING_DEPTH {
            return Err("JSON: nesting depth exceeds 256".to_owned());
        }
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(depth),
            Some('[') => self.parse_array(depth),
            Some('"') => self.parse_string().map(dyn_str),
            Some('t') => self.parse_keyword("true", dyn_bool(true)),
            Some('f') => self.parse_keyword("false", dyn_bool(false)),
            Some('n') => self.parse_keyword("null", dyn_null()),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!(
                "JSON: unexpected character '{c}' at position {}",
                self.pos
            )),
            None => Err("JSON: empty input".to_owned()),
        }
    }

    fn parse_keyword(&mut self, word: &str, value: Value) -> Result<Value, String> {
        for expected in word.chars() {
            self.expect_char(expected)?;
        }
        Ok(value)
    }

    fn parse_object(&mut self, depth: usize) -> Result<Value, String> {
        self.expect_char('{')?;
        self.skip_ws();
        let mut map = IndexMap::new();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(dyn_dict(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some('"') {
                return Err(format!(
                    "JSON: expected an object key (a string) at position {}",
                    self.pos
                ));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect_char(':')?;
            self.skip_ws();
            let value = self.parse_value(depth + 1)?;
            map.insert(MapKey::Str(Arc::from(key)), value);
            self.skip_ws();
            match self.bump() {
                Some(',') => {}
                Some('}') => break,
                Some(c) => {
                    return Err(format!(
                        "JSON: expected ',' or '}}' at position {} but found '{c}'",
                        self.pos - 1
                    ));
                }
                None => return Err("JSON: object was not closed".to_owned()),
            }
        }
        Ok(dyn_dict(map))
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, String> {
        self.expect_char('[')?;
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(dyn_list(items));
        }
        loop {
            let value = self.parse_value(depth + 1)?;
            items.push(value);
            self.skip_ws();
            match self.bump() {
                Some(',') => {}
                Some(']') => break,
                Some(c) => {
                    return Err(format!(
                        "JSON: expected ',' or ']' at position {} but found '{c}'",
                        self.pos - 1
                    ));
                }
                None => return Err("JSON: array was not closed".to_owned()),
            }
        }
        Ok(dyn_list(items))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect_char('"')?;
        let mut out = String::new();
        loop {
            match self.bump() {
                Some('"') => break,
                Some('\\') => {
                    let escaped = self.bump().ok_or_else(|| {
                        "JSON: string ended in the middle of an escape".to_owned()
                    })?;
                    match escaped {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let cp = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&cp) {
                                self.expect_char('\\')?;
                                self.expect_char('u')?;
                                let low = self.parse_hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err("JSON: invalid surrogate pair".to_owned());
                                }
                                let combined = 0x10000
                                    + (u32::from(cp) - 0xD800) * 0x400
                                    + (u32::from(low) - 0xDC00);
                                let c = char::from_u32(combined)
                                    .ok_or_else(|| "JSON: invalid Unicode code point".to_owned())?;
                                out.push(c);
                            } else {
                                let c = char::from_u32(u32::from(cp))
                                    .ok_or_else(|| "JSON: invalid Unicode code point".to_owned())?;
                                out.push(c);
                            }
                        }
                        other => {
                            return Err(format!("JSON: invalid escape '\\{other}'"));
                        }
                    }
                }
                Some(c) if (c as u32) < 0x20 => {
                    return Err(
                        "JSON: a control character cannot appear directly in a string".to_owned(),
                    );
                }
                Some(c) => out.push(c),
                None => return Err("JSON: string was not closed".to_owned()),
            }
        }
        Ok(out)
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        let hex: String = (0..4)
            .map(|_| self.bump())
            .collect::<Option<String>>()
            .ok_or_else(|| "JSON: \\u escape is missing digits".to_owned())?;
        u16::from_str_radix(&hex, 16).map_err(|_| format!("JSON: invalid \\u escape '{hex}'"))
    }

    fn parse_number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        match self.peek() {
            Some('0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(digit) if digit.is_ascii_digit()) {
                    return Err(format!("JSON: leading zero in number at position {start}"));
                }
            }
            Some(digit) if digit.is_ascii_digit() => {
                while matches!(self.peek(), Some(digit) if digit.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return Err(format!("JSON: invalid number literal at position {start}")),
        }

        let mut is_float = false;
        if self.peek() == Some('.') {
            is_float = true;
            self.pos += 1;
            let fraction_start = self.pos;
            while matches!(self.peek(), Some(digit) if digit.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == fraction_start {
                return Err(format!(
                    "JSON: fraction requires digits at position {start}"
                ));
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.pos += 1;
            }
            let exponent_start = self.pos;
            while matches!(self.peek(), Some(digit) if digit.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == exponent_start {
                return Err(format!(
                    "JSON: exponent requires digits at position {start}"
                ));
            }
        }

        let text: String = self.chars[start..self.pos].iter().collect();
        if is_float {
            text.parse::<f64>()
                .map(dyn_float)
                .map_err(|_| format!("JSON: invalid number literal: {text}"))
        } else {
            match text.parse::<i64>() {
                Ok(number) => Ok(dyn_int(number)),
                Err(_) => text
                    .parse::<f64>()
                    .map(dyn_float)
                    .map_err(|_| format!("JSON: invalid number literal: {text}")),
            }
        }
    }
}

/// Renders the dynamic intermediate representation to JSON text.
#[must_use]
pub fn dynamic_to_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match dyn_variant(value) {
        // "Null" has no separate arm since its output is identical to the wildcard arm below
        // (clippy::match_same_arms) -- the fallback for an invalid dynamic Value also happens to
        // land on null, and the intentional null serialization coincides with it, so merging them
        // is fine.
        Some(("Bool", fields)) => match fields.first() {
            Some(Value::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
            _ => out.push_str("null"),
        },
        Some(("Int", fields)) => match fields.first() {
            Some(Value::Int(n)) => out.push_str(&n.to_string()),
            _ => out.push_str("null"),
        },
        Some(("Float", fields)) => match fields.first() {
            Some(Value::Float(f)) => out.push_str(&super::format_float_default(*f)),
            _ => out.push_str("null"),
        },
        Some(("Str", fields)) => match fields.first() {
            Some(Value::Str(s)) => write_json_string(s, out),
            _ => out.push_str("\"\""),
        },
        Some(("List", fields)) => match fields.first() {
            Some(Value::List(items)) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_value(item, out);
                }
                out.push(']');
            }
            _ => out.push_str("[]"),
        },
        Some(("Dict", fields)) => match fields.first() {
            Some(Value::Dict(map)) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(map_key_as_str(k), out);
                    out.push(':');
                    write_value(v, out);
                }
                out.push('}');
            }
            _ => out.push_str("{}"),
        },
        _ => out.push_str("null"),
    }
}

pub(crate) fn write_json_string(s: &str, out: &mut String) {
    use std::fmt::Write as _;
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Writing to a String can't fail except on allocation failure, so the Result can
                // be ignored (`.unwrap()`/`.expect()` are unused because clippy denies them).
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::{dynamic_to_string, parse_to_dynamic};
    use crate::eval::value::{MapKey, Value};

    /// `.unwrap()`/`.expect()` are unused because clippy denies them (Cargo.toml
    /// `[lints.clippy]`). A generic "this obviously succeeds" extraction helper used only in
    /// tests.
    fn must<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
        match r {
            Ok(v) => v,
            Err(e) => panic!("expected Ok, got Err: {e:?}"),
        }
    }

    fn variant<'a>(v: &'a Value, name: &str) -> &'a [Value] {
        match super::dyn_variant(v) {
            Some((n, fields)) if n == name => fields,
            other => panic!("expected variant {name}, got {other:?}"),
        }
    }

    fn dict_get<'a>(map: &'a indexmap::IndexMap<MapKey, Value>, key: &str) -> &'a Value {
        match map.get(&MapKey::Str(std::sync::Arc::from(key))) {
            Some(v) => v,
            None => panic!("missing key {key}"),
        }
    }

    #[test]
    fn parses_null_bool_and_numbers() {
        assert!(matches!(
            super::dyn_variant(&must(parse_to_dynamic("null"))),
            Some(("Null", _))
        ));
        assert!(matches!(
            variant(&must(parse_to_dynamic("true")), "Bool").first(),
            Some(Value::Bool(true))
        ));
        assert!(matches!(
            variant(&must(parse_to_dynamic("false")), "Bool").first(),
            Some(Value::Bool(false))
        ));
        assert!(matches!(
            variant(&must(parse_to_dynamic("42")), "Int").first(),
            Some(Value::Int(42))
        ));
        assert!(matches!(
            variant(&must(parse_to_dynamic("-7")), "Int").first(),
            Some(Value::Int(-7))
        ));
    }

    #[test]
    fn distinguishes_int_from_float() {
        // Uses a value not close to pi (2.75) to avoid clippy::approx_constant (deny).
        assert!(matches!(
            variant(&must(parse_to_dynamic("2.75")), "Float").first(),
            Some(Value::Float(f)) if (*f - 2.75).abs() < 1e-9
        ));
        assert!(matches!(
            variant(&must(parse_to_dynamic("1e3")), "Float").first(),
            Some(Value::Float(f)) if (*f - 1000.0).abs() < 1e-9
        ));
        assert!(matches!(
            variant(&must(parse_to_dynamic("30")), "Int").first(),
            Some(Value::Int(30))
        ));
    }

    #[test]
    fn parses_nested_object_and_array() {
        let v = must(parse_to_dynamic(r#"{"a": [1, 2, {"b": "c"}], "d": null}"#));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        let a = dict_get(map, "a");
        let a_items = variant(a, "List");
        let Some(Value::List(items)) = a_items.first() else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 3);
        let d = dict_get(map, "d");
        assert!(matches!(super::dyn_variant(d), Some(("Null", _))));
    }

    #[test]
    fn parses_escapes_including_unicode() {
        let v = must(parse_to_dynamic(r#""a\n\t\"\\é""#));
        let fields = variant(&v, "Str");
        let Some(Value::Str(s)) = fields.first() else {
            panic!("expected str");
        };
        assert_eq!(s.as_ref(), "a\n\t\"\\\u{e9}");
    }

    #[test]
    fn parses_surrogate_pair_escape() {
        // U+1F600 (grinning face) as a UTF-16 surrogate pair.
        let v = must(parse_to_dynamic(r#""😀""#));
        let fields = variant(&v, "Str");
        let Some(Value::Str(s)) = fields.first() else {
            panic!("expected str");
        };
        assert_eq!(s.as_ref(), "\u{1f600}");
    }

    #[test]
    fn parses_empty_array_and_object() {
        assert!(matches!(
            super::dyn_variant(&must(parse_to_dynamic("[]"))),
            Some(("List", _))
        ));
        assert!(matches!(
            super::dyn_variant(&must(parse_to_dynamic("{}"))),
            Some(("Dict", _))
        ));
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(parse_to_dynamic("").is_err());
        assert!(parse_to_dynamic("{").is_err());
        assert!(parse_to_dynamic("[1, 2").is_err());
        assert!(parse_to_dynamic(r#"{"a": }"#).is_err());
        assert!(parse_to_dynamic("truex").is_err());
        assert!(parse_to_dynamic(r#""unterminated"#).is_err());
        assert!(parse_to_dynamic("1 2").is_err());
    }

    #[test]
    fn round_trips_through_encode_decode() {
        let original = r#"{"age":30,"name":"alice","scores":[1,2.5,true,null]}"#;
        let v1 = must(parse_to_dynamic(original));
        let text = dynamic_to_string(&v1);
        let v2 = must(parse_to_dynamic(&text));
        assert_eq!(v1, v2);
    }

    #[test]
    fn encodes_float_with_decimal_point() {
        let v = super::dyn_float(1.0);
        assert_eq!(dynamic_to_string(&v), "1.0");
        let v2 = super::dyn_float(3.5);
        assert_eq!(dynamic_to_string(&v2), "3.5");
    }
}
