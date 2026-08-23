//! YAML (safe subset) decode/encode (STDLIB.md §4.1, ARCHITECTURE.md §2.1). Anchors, aliases,
//! and multi-document input are unsupported (SPEC §11.1). Implemented from scratch without
//! serde. Flow style (`{...}`/`[...]`) and tags (`!!str` etc.) are also outside this safe
//! subset's scope, and return Err when encountered (a decision made in this file -- since SPEC/
//! STDLIB only demonstrates block style and doesn't mention whether flow style is required,
//! this errs on the safe side).

use super::{
    dyn_bool, dyn_dict, dyn_float, dyn_int, dyn_list, dyn_null, dyn_str, dyn_variant,
    parse_escaped_string_body,
};
use crate::eval::value::{MapKey, Value};
use indexmap::IndexMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct Line {
    indent: usize,
    content: String,
}

/// Pre-processes YAML text into lines: detects tab indentation and multi-document markers as
/// errors, and strips comments ([`strip_comment`]) and blank lines.
fn preprocess(text: &str) -> Result<Vec<Line>, String> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        if raw
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .any(|character| character == '\t')
        {
            return Err("YAML: tab-character indentation is not supported".to_owned());
        }
        let trimmed = raw.trim_end();
        let bare = trimmed.trim();
        if bare == "---" || bare == "..." || bare.starts_with("--- ") {
            return Err("YAML: multi-document input (---/...) is not supported".to_owned());
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        let content = strip_comment(trimmed.trim_start());
        if content.trim().is_empty() {
            continue;
        }
        lines.push(Line {
            indent,
            content: content.trim_end().to_owned(),
        });
    }
    Ok(lines)
}

/// Strips a comment (`#`) from a line, ignoring occurrences inside quotes.
fn strip_comment(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for character in source.chars() {
        if in_double && character == '\\' && !escaped {
            escaped = true;
            output.push(character);
            continue;
        }
        match character {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single && !escaped => in_double = !in_double,
            '#' if !in_single && !in_double => {
                let starts_comment =
                    output.is_empty() || output.ends_with(' ') || output.ends_with('\t');
                if starts_comment {
                    break;
                }
            }
            _ => {}
        }
        output.push(character);
        escaped = false;
    }
    output
}

pub fn parse_to_dynamic(text: &str) -> Result<Value, String> {
    let lines = preprocess(text)?;
    if lines.is_empty() {
        return Ok(dyn_null());
    }
    let root_indent = lines[0].indent;
    let (value, next) = parse_block(&lines, 0, root_indent, 0)?;
    if next != lines.len() {
        return Err("YAML: invalid indentation structure".to_owned());
    }
    Ok(value)
}

fn parse_block(
    lines: &[Line],
    start: usize,
    indent: usize,
    depth: usize,
) -> Result<(Value, usize), String> {
    if depth > 256 {
        return Err("YAML: nesting depth exceeds 256".to_owned());
    }
    if start >= lines.len() || lines[start].indent != indent {
        return Err("YAML: block is empty or has invalid indentation".to_owned());
    }
    let content = &lines[start].content;
    if content == "-" || content.starts_with("- ") {
        parse_sequence(lines, start, indent, depth)
    } else if !starts_with_flow_collection(content) && find_top_level_colon(content).is_some() {
        parse_mapping(lines, start, indent, depth)
    } else {
        Ok((parse_scalar(content)?, start + 1))
    }
}

/// If the whole line starts with `{`/`[`, it's itself a flow-style collection (syntactically
/// colliding with a JSON object/array). In this case, rather than attempting to interpret it as
/// a mapping/sequence, it's passed directly to [`parse_scalar`], consolidating onto that
/// function's explicit flow-style rejection -- otherwise, JSON-like input such as `{"a": 1}`
/// would trip `find_top_level_colon`'s naive colon search and be incorrectly accepted as a
/// mapping with the broken key `{"a` (a decision made in this file: since the safe subset never
/// handles flow style at all, a line starting with `{`/`[` is already syntactically guaranteed
/// to be flow style).
fn starts_with_flow_collection(s: &str) -> bool {
    s.starts_with('{') || s.starts_with('[')
}

/// Returns the position of a key-separating `:` (immediately followed by whitespace or end of
/// line) that's outside quotes.
fn find_top_level_colon(s: &str) -> Option<usize> {
    let mut in_single = false;
    let mut in_double = false;
    let chars: Vec<char> = s.chars().collect();
    let mut byte_pos = 0;
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ':' if !in_single && !in_double => {
                let next_is_boundary = chars.get(i + 1).is_none_or(|n| *n == ' ' || *n == '\t');
                if next_is_boundary {
                    return Some(byte_pos);
                }
            }
            _ => {}
        }
        byte_pos += c.len_utf8();
    }
    None
}

fn parse_mapping(
    lines: &[Line],
    start: usize,
    indent: usize,
    depth: usize,
) -> Result<(Value, usize), String> {
    let mut map = IndexMap::new();
    let mut i = start;
    while i < lines.len() && lines[i].indent == indent {
        let content = &lines[i].content;
        let Some(colon) = find_top_level_colon(content) else {
            break;
        };
        let key = unquote_or_plain(content[..colon].trim())?;
        let rest = content[colon + 1..].trim();
        let (value, next_i) = if rest.is_empty() {
            if i + 1 < lines.len() && lines[i + 1].indent > indent {
                parse_block(lines, i + 1, lines[i + 1].indent, depth + 1)?
            } else {
                (dyn_null(), i + 1)
            }
        } else {
            (parse_scalar(rest)?, i + 1)
        };
        map.insert(MapKey::Str(Arc::from(key)), value);
        i = next_i;
    }
    Ok((dyn_dict(map), i))
}

fn parse_sequence(
    lines: &[Line],
    start: usize,
    indent: usize,
    depth: usize,
) -> Result<(Value, usize), String> {
    let mut items = Vec::new();
    let mut i = start;
    while i < lines.len() && lines[i].indent == indent {
        let content = &lines[i].content;
        if content != "-" && !content.starts_with("- ") {
            break;
        }
        let rest = if content == "-" {
            ""
        } else {
            content[2..].trim()
        };
        let (value, next_i) = if rest.is_empty() {
            if i + 1 < lines.len() && lines[i + 1].indent > indent {
                parse_block(lines, i + 1, lines[i + 1].indent, depth + 1)?
            } else {
                (dyn_null(), i + 1)
            }
        } else if !starts_with_flow_collection(rest) && find_top_level_colon(rest).is_some() {
            parse_inline_mapping_item(lines, i, indent, rest, depth + 1)?
        } else {
            (parse_scalar(rest)?, i + 1)
        };
        items.push(value);
        i = next_i;
    }
    Ok((dyn_list(items), i))
}

/// Assembles a mapping that starts inside a sequence item, like `- key: value`, into a
/// synthetic line list re-indented to a virtual indent (`indent + 2`), and simply reuses
/// [`parse_mapping`] on it.
fn parse_inline_mapping_item(
    lines: &[Line],
    i: usize,
    seq_indent: usize,
    first_rest: &str,
    depth: usize,
) -> Result<(Value, usize), String> {
    let virtual_indent = seq_indent + 2;
    let mut synthetic = vec![Line {
        indent: virtual_indent,
        content: first_rest.to_owned(),
    }];
    let mut j = i + 1;
    while j < lines.len() && lines[j].indent >= virtual_indent {
        synthetic.push(lines[j].clone());
        j += 1;
    }
    let (value, consumed) = parse_mapping(&synthetic, 0, virtual_indent, depth)?;
    Ok((value, i + consumed))
}

/// Strips quotes if quoted (`"..."`/`'...'`), otherwise returns as-is.
fn unquote_or_plain(s: &str) -> Result<String, String> {
    if let Some(stripped) = s.strip_prefix('"') {
        parse_double_quoted(stripped)
    } else if let Some(stripped) = s.strip_prefix('\'') {
        parse_single_quoted(stripped)
    } else {
        Ok(s.to_owned())
    }
}

fn parse_double_quoted(after_quote: &str) -> Result<String, String> {
    let mut escaped = false;
    for (index, character) in after_quote.char_indices() {
        if character == '"' && !escaped {
            if !after_quote[index + 1..].trim().is_empty() {
                return Err("YAML: trailing content after quoted scalar".to_owned());
            }
            return parse_escaped_string_body("YAML", &after_quote[..index]);
        }
        escaped = character == '\\' && !escaped;
    }
    Err("YAML: double-quoted string was not closed".to_owned())
}

fn parse_single_quoted(after_quote: &str) -> Result<String, String> {
    let Some(end) = after_quote.rfind('\'') else {
        return Err("YAML: single-quoted string was not closed".to_owned());
    };
    if !after_quote[end + 1..].trim().is_empty() {
        return Err("YAML: trailing content after quoted scalar".to_owned());
    }
    Ok(after_quote[..end].replace("''", "'"))
}

fn parse_scalar(s: &str) -> Result<Value, String> {
    let s = s.trim();
    if s.is_empty() || s == "~" || s.eq_ignore_ascii_case("null") {
        return Ok(dyn_null());
    }
    if s.starts_with('&') || s.starts_with('*') {
        return Err("YAML: anchors/aliases are not supported".to_owned());
    }
    if s.starts_with('!') {
        return Err("YAML: tags are not supported".to_owned());
    }
    if s.starts_with('{') || s.starts_with('[') {
        return Err("YAML: flow style is not supported".to_owned());
    }
    if s.eq_ignore_ascii_case("true") {
        return Ok(dyn_bool(true));
    }
    if s.eq_ignore_ascii_case("false") {
        return Ok(dyn_bool(false));
    }
    if let Some(stripped) = s.strip_prefix('"') {
        return Ok(dyn_str(parse_double_quoted(stripped)?));
    }
    if let Some(stripped) = s.strip_prefix('\'') {
        return Ok(dyn_str(parse_single_quoted(stripped)?));
    }
    if let Ok(n) = s.parse::<i64>() {
        return Ok(dyn_int(n));
    }
    if looks_like_float(s)
        && let Ok(f) = s.parse::<f64>()
    {
        return Ok(dyn_float(f));
    }
    Ok(dyn_str(s.to_owned()))
}

/// Since `f64::from_str` also accepts things like "inf"/"nan", this first checks whether the
/// string is composed only of characters that look like a YAML number (sign, digits, decimal
/// point, exponent).
fn looks_like_float(s: &str) -> bool {
    let mut has_digit = false;
    let mut has_dot_or_exp = false;
    for c in s.chars() {
        match c {
            '0'..='9' => has_digit = true,
            '.' | 'e' | 'E' | '+' | '-' => has_dot_or_exp = true,
            _ => return false,
        }
    }
    has_digit && has_dot_or_exp
}

#[must_use]
pub fn dynamic_to_string(value: &Value) -> String {
    let mut out = String::new();
    write_node(value, 0, &mut out);
    out
}

fn write_indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push(' ');
    }
}

fn write_node(value: &Value, indent: usize, out: &mut String) {
    match dyn_variant(value) {
        Some(("Dict", f)) => match f.first() {
            Some(Value::Dict(map)) if !map.is_empty() => {
                for (key, value) in map.iter() {
                    write_indent(indent, out);
                    out.push_str(&format_yaml_key(super::map_key_as_str(key)));
                    out.push(':');
                    write_child(value, indent, out);
                }
            }
            _ => {
                write_indent(indent, out);
                out.push_str("{}\n");
            }
        },
        Some(("List", f)) => match f.first() {
            Some(Value::List(items)) if !items.is_empty() => {
                for item in items.iter() {
                    write_seq_item(item, indent, out);
                }
            }
            _ => {
                write_indent(indent, out);
                out.push_str("[]\n");
            }
        },
        _ => {
            write_indent(indent, out);
            out.push_str(&scalar_to_yaml(value));
            out.push('\n');
        }
    }
}

fn format_yaml_key(key: &str) -> String {
    let plain = !key.is_empty()
        && key.trim() == key
        && !key.starts_with(['-', '?', ':', '#'])
        && !key.contains([':', '#', '\n', '\r']);
    if plain {
        key.to_owned()
    } else {
        format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn write_seq_item(v: &Value, indent: usize, out: &mut String) {
    write_indent(indent, out);
    out.push('-');
    write_child(v, indent, out);
}

/// Writes the value portion that follows `key:` or `-`. For a container (a non-empty dict/list),
/// writes a newline and then the nested block at `indent + 2`; for a scalar (or an empty
/// container), writes it inline on the spot.
fn write_child(v: &Value, indent: usize, out: &mut String) {
    match dyn_variant(v) {
        Some(("Dict", f)) if matches!(f.first(), Some(Value::Dict(m)) if !m.is_empty()) => {
            out.push('\n');
            write_node(v, indent + 2, out);
        }
        Some(("List", f)) if matches!(f.first(), Some(Value::List(items)) if !items.is_empty()) => {
            out.push('\n');
            write_node(v, indent + 2, out);
        }
        _ => {
            out.push(' ');
            out.push_str(&scalar_to_yaml(v));
            out.push('\n');
        }
    }
}

fn scalar_to_yaml(value: &Value) -> String {
    match dyn_variant(value) {
        // "Null" has no separate arm since its output is identical to the wildcard arm below
        // (clippy::match_same_arms).
        Some(("Bool", f)) => match f.first() {
            Some(Value::Bool(b)) => b.to_string(),
            _ => "null".to_owned(),
        },
        Some(("Int", f)) => match f.first() {
            Some(Value::Int(n)) => n.to_string(),
            _ => "null".to_owned(),
        },
        Some(("Float", f)) => match f.first() {
            Some(Value::Float(x)) => super::format_float_default(*x),
            _ => "null".to_owned(),
        },
        Some(("Str", f)) => match f.first() {
            Some(Value::Str(s)) => quote_if_needed(s),
            _ => "\"\"".to_owned(),
        },
        Some(("Dict", f)) if matches!(f.first(), Some(Value::Dict(m)) if m.is_empty()) => {
            "{}".to_owned()
        }
        Some(("List", f)) if matches!(f.first(), Some(Value::List(items)) if items.is_empty()) => {
            "[]".to_owned()
        }
        _ => "null".to_owned(),
    }
}

/// Wraps a string in double quotes only when it would otherwise be misread as a different type
/// upon re-decoding (empty string, something equivalent to true/false/null, looks like a number,
/// leading/trailing whitespace, contains a colon or other special characters, etc.).
fn quote_if_needed(s: &str) -> String {
    let needs_quote = s.is_empty()
        || s.trim() != s
        || s.eq_ignore_ascii_case("null")
        || s == "~"
        || s.eq_ignore_ascii_case("true")
        || s.eq_ignore_ascii_case("false")
        || s.parse::<i64>().is_ok()
        || looks_like_float(s)
        || find_top_level_colon(s).is_some()
        || s.contains('\n')
        || s.contains('#')
        || matches!(
            s.chars().next(),
            Some('&' | '*' | '!' | '{' | '[' | '"' | '\'' | '-' | ' ')
        );
    if needs_quote {
        let escaped = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        format!("\"{escaped}\"")
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{dynamic_to_string, parse_to_dynamic};
    use crate::eval::value::{MapKey, Value};

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
    fn parses_flat_block_mapping_with_scalar_inference() {
        let v = must(parse_to_dynamic(
            "name: bob\nage: 25\nactive: true\nscore: 3.5\nnote: ~\n",
        ));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        assert!(matches!(
            variant(dict_get(map, "name"), "Str").first(),
            Some(Value::Str(s)) if s.as_ref() == "bob"
        ));
        assert!(matches!(
            variant(dict_get(map, "age"), "Int").first(),
            Some(Value::Int(25))
        ));
        assert!(matches!(
            variant(dict_get(map, "active"), "Bool").first(),
            Some(Value::Bool(true))
        ));
        assert!(matches!(
            variant(dict_get(map, "score"), "Float").first(),
            Some(Value::Float(f)) if (*f - 3.5).abs() < 1e-9
        ));
        assert!(matches!(
            super::dyn_variant(dict_get(map, "note")),
            Some(("Null", _))
        ));
    }

    #[test]
    fn parses_nested_mapping_by_indentation() {
        let v = must(parse_to_dynamic(
            "user:\n  name: alice\n  age: 30\ncount: 1\n",
        ));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        let user_fields = variant(dict_get(map, "user"), "Dict");
        let Some(Value::Dict(user_map)) = user_fields.first() else {
            panic!("expected nested dict");
        };
        assert!(matches!(
            variant(dict_get(user_map, "name"), "Str").first(),
            Some(Value::Str(s)) if s.as_ref() == "alice"
        ));
    }

    #[test]
    fn parses_block_sequence_of_scalars() {
        let v = must(parse_to_dynamic("- 1\n- 2\n- 3\n"));
        let fields = variant(&v, "List");
        let Some(Value::List(items)) = fields.first() else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 3);
        assert!(matches!(
            variant(&items[0], "Int").first(),
            Some(Value::Int(1))
        ));
    }

    #[test]
    fn parses_sequence_of_inline_mappings() {
        let v = must(parse_to_dynamic(
            "- name: alice\n  age: 30\n- name: bob\n  age: 25\n",
        ));
        let fields = variant(&v, "List");
        let Some(Value::List(items)) = fields.first() else {
            panic!("expected list");
        };
        assert_eq!(items.len(), 2);
        let first_fields = variant(&items[0], "Dict");
        let Some(Value::Dict(first_map)) = first_fields.first() else {
            panic!("expected dict");
        };
        assert!(matches!(
            variant(dict_get(first_map, "name"), "Str").first(),
            Some(Value::Str(s)) if s.as_ref() == "alice"
        ));
    }

    #[test]
    fn parses_quoted_strings() {
        let v = must(parse_to_dynamic("key: \"hello world\"\n"));
        let fields = variant(&v, "Dict");
        let Some(Value::Dict(map)) = fields.first() else {
            panic!("expected dict");
        };
        assert!(matches!(
            variant(dict_get(map, "key"), "Str").first(),
            Some(Value::Str(s)) if s.as_ref() == "hello world"
        ));
    }

    #[test]
    fn rejects_anchors_aliases_and_multi_document() {
        assert!(parse_to_dynamic("key: &anchor value\n").is_err());
        assert!(parse_to_dynamic("key: *anchor\n").is_err());
        assert!(parse_to_dynamic("---\nkey: value\n").is_err());
        assert!(parse_to_dynamic("key: value\n---\nkey2: value2\n").is_err());
        assert!(parse_to_dynamic("...\n").is_err());
    }

    #[test]
    fn rejects_flow_style_and_tabs() {
        assert!(parse_to_dynamic("key: {a: 1}\n").is_err());
        assert!(parse_to_dynamic("key: [1, 2]\n").is_err());
        assert!(parse_to_dynamic("\tkey: value\n").is_err());
    }

    /// Regression for when the top level is itself JSON-specific flow style (`{"a": 1}`). A
    /// naive colon search (`find_top_level_colon`) alone determining a mapping would trip on
    /// the `:` inside this quote and incorrectly accept it as a mapping with the broken key
    /// `{"a` -- this is blocked by the `starts_with_flow_collection` guard.
    #[test]
    fn rejects_top_level_json_object_and_array() {
        assert!(parse_to_dynamic(r#"{"a": 1}"#).is_err());
        assert!(parse_to_dynamic(r#"{"a": 1, "b": 2}"#).is_err());
        assert!(parse_to_dynamic("[1, 2, 3]").is_err());
    }

    /// Also rejects flow style arriving directly as a sequence item (`- {a: 1}`), likewise (the
    /// same kind of guard inside `parse_sequence`).
    #[test]
    fn rejects_flow_style_as_sequence_item() {
        assert!(parse_to_dynamic("- {a: 1}\n").is_err());
        assert!(parse_to_dynamic("- [1, 2]\n").is_err());
    }

    #[test]
    fn round_trips_struct_like_mapping() {
        let original = must(parse_to_dynamic("name: dana\nage: 33\n"));
        let text = dynamic_to_string(&original);
        let decoded = must(parse_to_dynamic(&text));
        assert_eq!(original, decoded);
    }

    #[test]
    fn round_trips_nested_structure() {
        let original = must(parse_to_dynamic(
            "user:\n  name: alice\n  tags:\n    - a\n    - b\ncount: 2\n",
        ));
        let text = dynamic_to_string(&original);
        let decoded = must(parse_to_dynamic(&text));
        assert_eq!(original, decoded);
    }

    #[test]
    fn encode_quotes_ambiguous_strings() {
        let v = super::dyn_str("42");
        let text = dynamic_to_string(&v);
        assert_eq!(text.trim(), "\"42\"");
        let decoded = must(parse_to_dynamic(&text));
        assert!(matches!(super::dyn_variant(&decoded), Some(("Str", _))));
    }
}
