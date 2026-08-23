//! TOML decode/encode (including the D-STDPOL-09 root-type constraint, STDLIB.md §4.1,
//! ARCHITECTURE.md §2.1). Implemented from scratch without serde.
//!
//! Supported syntax: `key = value` (at the top level / inside a table), `[table]`/
//! `[[array.of.tables]]` headers (dot-separated nesting), inline tables `{ .. }`, inline arrays
//! `[ .. ]` (including ones spanning multiple lines), basic strings `"..."` (with escapes) and
//! literal strings `'...'`, integers (underscore separators allowed), floating-point numbers,
//! and booleans.
//! Unsupported (returns Err): datetime literals (no dedicated type; for the same reason as
//! D-STDPOL-06, a TOML datetime type is also not added, a decision made in this file), and
//! multi-line strings (`"""..."""`/`'''...'''`).
//! On the encode side, nested structure is always represented using inline tables/inline arrays
//! (only the root level lays out `key = value` lines) -- a simplification that avoids managing
//! `[table]`-header sections while still always producing valid TOML (a decision made in this
//! file).

use super::{
    dyn_bool, dyn_dict, dyn_float, dyn_int, dyn_list, dyn_str, dyn_variant,
    parse_escaped_string_body,
};
use crate::eval::value::{MapKey, Value};
use crate::types::Ty;
use indexmap::IndexMap;
use std::sync::Arc;

// =========================================================================
// decode
// =========================================================================

/// An intermediate representation for building the table hierarchy. Eventually converted to a
/// dynamic `Value` by [`finalize`].
enum Node {
    Table(IndexMap<String, Node>),
    ArrayOfTables(Vec<IndexMap<String, Node>>),
    Leaf(Value),
}

pub fn parse_to_dynamic(text: &str) -> Result<Value, String> {
    let logical_lines = join_logical_lines(text)?;
    let mut root: IndexMap<String, Node> = IndexMap::new();
    let mut current_path: Vec<String> = Vec::new();
    for line in &logical_lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("[[") {
            let Some(path_str) = rest.strip_suffix("]]") else {
                return Err(format!("TOML: invalid array-of-tables header: {line}"));
            };
            let path = split_dotted_key(path_str.trim())?;
            append_array_of_tables(&mut root, &path)?;
            current_path = path;
        } else if let Some(rest) = line.strip_prefix('[') {
            let Some(path_str) = rest.strip_suffix(']') else {
                return Err(format!("TOML: invalid table header: {line}"));
            };
            let path = split_dotted_key(path_str.trim())?;
            ensure_table(&mut root, &path)?;
            current_path = path;
        } else {
            let Some(eq) = find_top_level_eq(line) else {
                return Err(format!("TOML: no '=' found: {line}"));
            };
            let key_path = split_dotted_key(line[..eq].trim())?;
            let value_str = line[eq + 1..].trim();
            let value = parse_value(value_str)?;
            set_leaf(&mut root, &current_path, &key_path, value)?;
        }
    }
    Ok(finalize(&root))
}

/// Joins physical lines into logical lines while stripping comments and tracking quote/bracket
/// balance (to handle arrays/inline tables that span multiple lines).
fn join_logical_lines(text: &str) -> Result<Vec<String>, String> {
    let mut logical = Vec::new();
    let mut buffer = String::new();
    let mut depth: i32 = 0;
    for raw in text.lines() {
        let stripped = strip_toml_comment(raw);
        if stripped.trim().is_empty() && depth == 0 {
            continue;
        }
        if !buffer.is_empty() {
            buffer.push(' ');
        }
        buffer.push_str(stripped.trim());
        depth += bracket_delta(&stripped);
        if depth < 0 {
            return Err("TOML: unbalanced brackets".to_owned());
        }
        if depth == 0 {
            logical.push(std::mem::take(&mut buffer));
        }
    }
    if depth != 0 || !buffer.trim().is_empty() {
        return Err("TOML: unbalanced brackets".to_owned());
    }
    Ok(logical)
}

fn strip_toml_comment(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut in_basic = false;
    let mut in_literal = false;
    let mut escaped = false;
    for character in source.chars() {
        if in_basic && character == '\\' && !escaped {
            escaped = true;
            output.push(character);
            continue;
        }
        match character {
            '"' if !in_literal && !escaped => in_basic = !in_basic,
            '\'' if !in_basic => in_literal = !in_literal,
            '#' if !in_basic && !in_literal => break,
            _ => {}
        }
        output.push(character);
        escaped = false;
    }
    output
}

fn bracket_delta(s: &str) -> i32 {
    let mut delta = 0;
    let mut in_basic = false;
    let mut in_literal = false;
    for c in s.chars() {
        match c {
            '"' if !in_literal => in_basic = !in_basic,
            '\'' if !in_basic => in_literal = !in_literal,
            '[' | '{' if !in_basic && !in_literal => delta += 1,
            ']' | '}' if !in_basic && !in_literal => delta -= 1,
            _ => {}
        }
    }
    delta
}

/// Returns the byte offset of the first `=` outside a quoted region.
fn find_top_level_eq(s: &str) -> Option<usize> {
    let mut in_basic = false;
    let mut in_literal = false;
    let mut byte_pos = 0;
    for c in s.chars() {
        match c {
            '"' if !in_literal => in_basic = !in_basic,
            '\'' if !in_basic => in_literal = !in_literal,
            '=' if !in_basic && !in_literal => return Some(byte_pos),
            _ => {}
        }
        byte_pos += c.len_utf8();
    }
    None
}

/// Splits a dot-separated key (quoted segments are also allowed).
fn split_dotted_key(s: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_basic = false;
    let mut in_literal = false;
    for c in s.chars() {
        match c {
            '"' if !in_literal => in_basic = !in_basic,
            '\'' if !in_basic => in_literal = !in_literal,
            '.' if !in_basic && !in_literal => {
                parts.push(current.trim().to_owned());
                current = String::new();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current.trim().to_owned());
    parts
        .into_iter()
        .map(|p| unquote_key(&p))
        .collect::<Result<Vec<_>, _>>()
}

fn unquote_key(s: &str) -> Result<String, String> {
    if let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Ok(inner.to_owned())
    } else if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        Ok(inner.to_owned())
    } else if s.is_empty() {
        Err("TOML: empty key".to_owned())
    } else {
        Ok(s.to_owned())
    }
}

/// Gets the (nested) table `path` designates. Follows TOML's convention that, if an
/// `ArrayOfTables` is encountered along the way, it's traversed via its most recently appended
/// element (the last table).
fn navigate_mut<'a>(
    root: &'a mut IndexMap<String, Node>,
    path: &[String],
) -> Result<&'a mut IndexMap<String, Node>, String> {
    let mut current = root;
    for segment in path {
        let entry = current
            .entry(segment.clone())
            .or_insert_with(|| Node::Table(IndexMap::new()));
        match entry {
            Node::Table(t) => current = t,
            Node::ArrayOfTables(v) => {
                let Some(last) = v.last_mut() else {
                    return Err(format!("TOML: array of tables is empty: {segment}"));
                };
                current = last;
            }
            Node::Leaf(_) => {
                return Err(format!("TOML: '{segment}' is already defined as a value"));
            }
        }
    }
    Ok(current)
}

fn ensure_table(root: &mut IndexMap<String, Node>, path: &[String]) -> Result<(), String> {
    let Some((last, prefix)) = path.split_last() else {
        return Err("TOML: empty table header".to_owned());
    };
    let parent = navigate_mut(root, prefix)?;
    parent
        .entry(last.clone())
        .or_insert_with(|| Node::Table(IndexMap::new()));
    Ok(())
}

fn append_array_of_tables(
    root: &mut IndexMap<String, Node>,
    path: &[String],
) -> Result<(), String> {
    let Some((last, prefix)) = path.split_last() else {
        return Err("TOML: empty array-of-tables header".to_owned());
    };
    let parent = navigate_mut(root, prefix)?;
    match parent
        .entry(last.clone())
        .or_insert_with(|| Node::ArrayOfTables(Vec::new()))
    {
        Node::ArrayOfTables(v) => v.push(IndexMap::new()),
        _ => {
            return Err(format!("TOML: '{last}' is already defined as a table"));
        }
    }
    Ok(())
}

fn set_leaf(
    root: &mut IndexMap<String, Node>,
    table_path: &[String],
    key_path: &[String],
    value: Value,
) -> Result<(), String> {
    let table = navigate_mut(root, table_path)?;
    let Some((last, prefix)) = key_path.split_last() else {
        return Err("TOML: empty key".to_owned());
    };
    let target = navigate_mut(table, prefix)?;
    if target.contains_key(last) {
        return Err(format!("TOML: duplicate key '{last}'"));
    }
    target.insert(last.clone(), Node::Leaf(value));
    Ok(())
}

fn finalize(node: &IndexMap<String, Node>) -> Value {
    let mut map = IndexMap::with_capacity(node.len());
    for (k, v) in node {
        let value = match v {
            Node::Table(t) => finalize(t),
            Node::ArrayOfTables(items) => dyn_list(items.iter().map(finalize).collect()),
            Node::Leaf(v) => v.clone(),
        };
        map.insert(MapKey::Str(Arc::from(k.as_str())), value);
    }
    dyn_dict(map)
}

/// Parses a value portion (the right-hand side of `=`, or an element of an inline array/table)
/// via recursive descent.
const MAX_INLINE_DEPTH: usize = 256;

fn parse_value(source: &str) -> Result<Value, String> {
    parse_value_at(source, 0)
}

fn parse_value_at(source: &str, depth: usize) -> Result<Value, String> {
    if depth > MAX_INLINE_DEPTH {
        return Err("TOML: inline container nesting exceeds 256".to_owned());
    }
    let source = source.trim();
    if source.is_empty() {
        return Err("TOML: value is empty".to_owned());
    }
    if let Some(inner) = source
        .strip_prefix('"')
        .and_then(|remainder| remainder.strip_suffix('"'))
    {
        if source.starts_with("\"\"\"") {
            return Err("TOML: multi-line strings are not supported".to_owned());
        }
        return Ok(dyn_str(parse_escaped_string_body("TOML", inner)?));
    }
    if let Some(inner) = source
        .strip_prefix('\'')
        .and_then(|remainder| remainder.strip_suffix('\''))
    {
        if source.starts_with("'''") {
            return Err("TOML: multi-line strings are not supported".to_owned());
        }
        return Ok(dyn_str(inner.to_owned()));
    }
    if source == "true" {
        return Ok(dyn_bool(true));
    }
    if source == "false" {
        return Ok(dyn_bool(false));
    }
    if let Some(inner) = source
        .strip_prefix('[')
        .and_then(|remainder| remainder.strip_suffix(']'))
    {
        return parse_inline_array(inner, depth + 1);
    }
    if let Some(inner) = source
        .strip_prefix('{')
        .and_then(|remainder| remainder.strip_suffix('}'))
    {
        return parse_inline_table(inner, depth + 1);
    }
    if looks_like_datetime(source) {
        return Err(format!(
            "TOML: datetime literals are not supported: {source}"
        ));
    }
    parse_number(source)
}

fn looks_like_datetime(s: &str) -> bool {
    // A fixed literal prefix, so compilation cannot fail; a compile failure falls back to
    // "not a datetime" (which just routes to parse_number's error).
    regex::Regex::new(r"^\d{4}-").is_ok_and(|re| re.is_match(s))
}

fn parse_number(source: &str) -> Result<Value, String> {
    let bytes = source.as_bytes();
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == b'_'
            && (index == 0
                || index + 1 == bytes.len()
                || !bytes[index - 1].is_ascii_digit()
                || !bytes[index + 1].is_ascii_digit())
        {
            return Err(format!("TOML: invalid numeric separator: {source}"));
        }
    }
    let cleaned: String = source
        .chars()
        .filter(|character| *character != '_')
        .collect();
    let unsigned = cleaned.strip_prefix(['+', '-']).unwrap_or(&cleaned);
    let integer_part = unsigned.split(['.', 'e', 'E']).next().unwrap_or(unsigned);
    if integer_part.len() > 1 && integer_part.starts_with('0') {
        return Err(format!("TOML: leading zero in number: {source}"));
    }
    let is_float = cleaned.contains('.') || cleaned.contains('e') || cleaned.contains('E');
    if is_float {
        let valid_fraction = cleaned.split_once('.').is_none_or(|(_, remainder)| {
            remainder.chars().next().is_some_and(|c| c.is_ascii_digit())
        });
        if !valid_fraction {
            return Err(format!("TOML: invalid float literal: {source}"));
        }
        cleaned
            .parse::<f64>()
            .map(dyn_float)
            .map_err(|_| format!("TOML: invalid number literal: {source}"))
    } else {
        cleaned
            .parse::<i64>()
            .map(dyn_int)
            .map_err(|_| format!("TOML: invalid value: {source}"))
    }
}

/// Splits the contents (with the enclosing brackets removed) by top-level commas and parses each
/// with [`parse_value`]. The comma-splitting itself also accounts for quotes and nested brackets.
fn parse_inline_array(inner: &str, depth: usize) -> Result<Value, String> {
    let mut items = Vec::new();
    for part in split_top_level_commas(inner) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        items.push(parse_value_at(part, depth)?);
    }
    Ok(dyn_list(items))
}

fn parse_inline_table(inner: &str, depth: usize) -> Result<Value, String> {
    let mut map = IndexMap::new();
    for part in split_top_level_commas(inner) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some(eq) = find_top_level_eq(part) else {
            return Err(format!("TOML: inline table entry is missing '=': {part}"));
        };
        let key = unquote_key(part[..eq].trim())?;
        let value = parse_value_at(part[eq + 1..].trim(), depth)?;
        let key = MapKey::Str(Arc::from(key));
        if map.contains_key(&key) {
            return Err("TOML: duplicate key in inline table".to_owned());
        }
        map.insert(key, value);
    }
    Ok(dyn_dict(map))
}

fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;
    let mut in_basic = false;
    let mut in_literal = false;
    for c in s.chars() {
        match c {
            '"' if !in_literal => in_basic = !in_basic,
            '\'' if !in_basic => in_literal = !in_literal,
            '[' | '{' if !in_basic && !in_literal => depth += 1,
            ']' | '}' if !in_basic && !in_literal => depth -= 1,
            ',' if !in_basic && !in_literal && depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

// =========================================================================
// encode
// =========================================================================

/// D-STDPOL-09: only valid when `T` is `dict[str, V]` or a struct (a type whose top level can be
/// represented as a table). A `T` where `list`/`set`/etc. sits at the top level is a type error
/// (equivalent to E1002, checked once `T` is resolved -- either from an explicit `[T]` at the
/// call site or from the assignment target's type -- which is the type-check phase's
/// responsibility; here we only ever receive a `value` whose validity is already guaranteed).
#[must_use]
pub fn is_valid_root_type(ty: &Ty) -> bool {
    match ty {
        Ty::Dict(key, _) => matches!(**key, Ty::Str),
        Ty::List(element) => is_valid_root_type(element),
        Ty::Named { .. } => true,
        _ => false,
    }
}

#[must_use]
pub fn dynamic_to_string(value: &Value) -> String {
    match dyn_variant(value) {
        Some(("Dict", f)) => match f.first() {
            Some(Value::Dict(map)) => {
                let mut out = String::new();
                for (key, value) in map.iter() {
                    if is_dynamic_null(value) {
                        continue;
                    }
                    out.push_str(&format_toml_key(super::map_key_as_str(key)));
                    out.push_str(" = ");
                    out.push_str(&inline_value(value));
                    out.push('\n');
                }
                out
            }
            _ => String::new(),
        },
        // A defensive fallback for when the root isn't a Dict (normally unreachable, since
        // validity is already guaranteed by type checking).
        _ => format!("value = {}\n", inline_value(value)),
    }
}

fn inline_value(value: &Value) -> String {
    match dyn_variant(value) {
        Some(("Bool", f)) => match f.first() {
            Some(Value::Bool(b)) => b.to_string(),
            _ => "false".to_owned(),
        },
        Some(("Int", f)) => match f.first() {
            Some(Value::Int(n)) => n.to_string(),
            _ => "0".to_owned(),
        },
        Some(("Float", f)) => match f.first() {
            Some(Value::Float(x)) => super::format_float_default(*x),
            _ => "0.0".to_owned(),
        },
        Some(("Str", f)) => match f.first() {
            Some(Value::Str(s)) => quote_toml_string(s),
            _ => "\"\"".to_owned(),
        },
        Some(("List", f)) => match f.first() {
            Some(Value::List(items)) => {
                let parts: Vec<String> = items.iter().map(inline_value).collect();
                format!("[{}]", parts.join(", "))
            }
            _ => "[]".to_owned(),
        },
        Some(("Dict", f)) => match f.first() {
            Some(Value::Dict(map)) => {
                let parts: Vec<String> = map
                    .iter()
                    .filter(|(_, value)| !is_dynamic_null(value))
                    .map(|(key, value)| {
                        format!(
                            "{} = {}",
                            format_toml_key(super::map_key_as_str(key)),
                            inline_value(value)
                        )
                    })
                    .collect();
                format!("{{ {} }}", parts.join(", "))
            }
            _ => "{ }".to_owned(),
        },
        _ => "\"\"".to_owned(),
    }
}

fn is_dynamic_null(value: &Value) -> bool {
    matches!(dyn_variant(value), Some(("Null", _)))
}

fn format_toml_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        key.to_owned()
    } else {
        quote_toml_string(key)
    }
}

fn quote_toml_string(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{dynamic_to_string, is_valid_root_type, parse_to_dynamic};
    use crate::eval::value::{MapKey, Value};
    use crate::types::Ty;

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
    fn parses_flat_key_values() {
        let v = must(parse_to_dynamic(
            "name = \"carol\"\nage = 40\nactive = true\nscore = 3.5\n",
        ));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        assert!(matches!(
            variant(dict_get(map, "name"), "Str").first(),
            Some(Value::Str(s)) if s.as_ref() == "carol"
        ));
        assert!(matches!(
            variant(dict_get(map, "age"), "Int").first(),
            Some(Value::Int(40))
        ));
        assert!(matches!(
            variant(dict_get(map, "active"), "Bool").first(),
            Some(Value::Bool(true))
        ));
        assert!(matches!(
            variant(dict_get(map, "score"), "Float").first(),
            Some(Value::Float(f)) if (*f - 3.5).abs() < 1e-9
        ));
    }

    #[test]
    fn parses_table_headers() {
        let v = must(parse_to_dynamic(
            "[user]\nname = \"alice\"\nage = 30\n\n[other]\nk = 1\n",
        ));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        let user_fields = variant(dict_get(map, "user"), "Dict");
        let Some(Value::Dict(user_map)) = user_fields.first() else {
            panic!("expected dict");
        };
        assert!(matches!(
            variant(dict_get(user_map, "name"), "Str").first(),
            Some(Value::Str(s)) if s.as_ref() == "alice"
        ));
    }

    #[test]
    fn parses_dotted_table_headers() {
        let v = must(parse_to_dynamic("[a.b]\nc = 1\n"));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        let a_fields = variant(dict_get(map, "a"), "Dict");
        let Some(Value::Dict(a_map)) = a_fields.first() else {
            panic!("expected dict");
        };
        let b_fields = variant(dict_get(a_map, "b"), "Dict");
        let Some(Value::Dict(b_map)) = b_fields.first() else {
            panic!("expected dict");
        };
        assert!(matches!(
            variant(dict_get(b_map, "c"), "Int").first(),
            Some(Value::Int(1))
        ));
    }

    #[test]
    fn parses_array_of_tables() {
        let v = must(parse_to_dynamic(
            "[[fruit]]\nname = \"apple\"\n\n[[fruit]]\nname = \"banana\"\n",
        ));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        let fruit_fields = variant(dict_get(map, "fruit"), "List");
        let Some(Value::List(items)) = fruit_fields.first() else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 2);
        let first_fields = variant(&items[0], "Dict");
        let Some(Value::Dict(first_map)) = first_fields.first() else {
            panic!("expected dict");
        };
        assert!(matches!(
            variant(dict_get(first_map, "name"), "Str").first(),
            Some(Value::Str(s)) if s.as_ref() == "apple"
        ));
    }

    #[test]
    fn parses_inline_table_and_array() {
        let v = must(parse_to_dynamic(
            "point = { x = 1, y = 2 }\nnums = [1, 2, 3]\n",
        ));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        let point_fields = variant(dict_get(map, "point"), "Dict");
        let Some(Value::Dict(point_map)) = point_fields.first() else {
            panic!("expected dict");
        };
        assert!(matches!(
            variant(dict_get(point_map, "x"), "Int").first(),
            Some(Value::Int(1))
        ));
        let nums_fields = variant(dict_get(map, "nums"), "List");
        let Some(Value::List(items)) = nums_fields.first() else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn parses_multiline_array() {
        let v = must(parse_to_dynamic("nums = [\n  1,\n  2,\n  3,\n]\n"));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        let nums_fields = variant(dict_get(map, "nums"), "List");
        let Some(Value::List(items)) = nums_fields.first() else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn rejects_datetime_literals() {
        assert!(parse_to_dynamic("d = 1979-05-27T07:32:00Z\n").is_err());
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse_to_dynamic("key value\n").is_err());
        assert!(parse_to_dynamic("nums = [1, 2\n").is_err());
        assert!(parse_to_dynamic("a = \"\"\"multi\nline\"\"\"\n").is_err());
    }

    #[test]
    fn root_type_constraint_accepts_dict_and_struct_rejects_others() {
        assert!(is_valid_root_type(&Ty::Dict(
            Box::new(Ty::Str),
            Box::new(Ty::Int)
        )));
        assert!(is_valid_root_type(&Ty::Named {
            name: std::sync::Arc::from("User"),
            args: vec![],
        }));
        assert!(!is_valid_root_type(&Ty::List(Box::new(Ty::Int))));
        assert!(!is_valid_root_type(&Ty::Set(Box::new(Ty::Int))));
        assert!(!is_valid_root_type(&Ty::Int));
    }

    #[test]
    fn round_trips_flat_table() {
        let original = must(parse_to_dynamic("name = \"dana\"\nage = 33\n"));
        let text = dynamic_to_string(&original);
        let decoded = must(parse_to_dynamic(&text));
        assert_eq!(original, decoded);
    }

    #[test]
    fn round_trips_nested_via_inline_encoding() {
        let original = must(parse_to_dynamic("[user]\nname = \"alice\"\nage = 30\n"));
        let text = dynamic_to_string(&original);
        let decoded = must(parse_to_dynamic(&text));
        assert_eq!(original, decoded);
    }
}
