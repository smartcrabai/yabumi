//! CSV decode/encode/decode_rows (STDLIB.md §4.2, ARCHITECTURE.md §2.1). The delimiter is fixed
//! as `,`. Newlines are normalized to LF on output. `,`/`"`/newlines inside a field are escaped
//! RFC 4180-style with double quotes and doubling. No effect (pure).
//!
//! `decode[T]`'s T is restricted to a flat struct, and each field type is restricted to
//! int/float/bool/str only (STDLIB.md §4.2, already checked at compile time as the equivalent of
//! E1002). Thanks to this restriction, how each field is handled can be decided just by looking
//! at the TypeAnn's name directly (without going through a `Program`-mediated type resolution
//! like `crate::types::generics::ty_from_ann`) -- which is why `Program` doesn't appear in
//! csv.decode/csv.encode's signatures.

use super::{decode_error, dyn_str};
use crate::ast::{StructDecl, TypeAnn, TypeAnnKind};
use crate::eval::value::{MapKey, StructInstance, Value};
use crate::stdlib::{err_value, ok_value};
use indexmap::IndexMap;
use std::sync::Arc;

/// If a field's type annotation is a primitive supported by csv (int/float/bool/str), returns
/// its name.
fn field_kind_name(ty: &TypeAnn) -> Option<&str> {
    match &ty.kind {
        TypeAnnKind::Named { name, args } if args.is_empty() => match name.as_ref() {
            "int" | "float" | "bool" | "str" => Some(name.as_ref()),
            _ => None,
        },
        _ => None,
    }
}

fn parse_cell(cell: &str, kind: &str) -> Result<Value, String> {
    match kind {
        "int" => cell
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("'{cell}' cannot be interpreted as int")),
        "float" => cell
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| format!("'{cell}' cannot be interpreted as float")),
        "bool" => match cell.trim() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!("'{cell}' cannot be interpreted as bool")),
        },
        // Anything other than "str" is already rejected by the caller (field_kind_name), so
        // this is unreachable, but it's spelled out explicitly for exhaustiveness.
        _ => Ok(Value::Str(Arc::from(cell))),
    }
}

fn format_cell(value: &Value, kind: &str) -> String {
    match (kind, value) {
        ("int", Value::Int(n)) => n.to_string(),
        ("float", Value::Float(f)) => super::format_float_default(*f),
        ("bool", Value::Bool(b)) => b.to_string(),
        ("str", Value::Str(s)) => s.to_string(),
        _ => String::new(),
    }
}

fn escape_csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_owned()
    }
}

/// Parses RFC 4180-style CSV into rows of fields (`Vec<Vec<String>>`) (including the header
/// row). Supports embedded commas/newlines and doubled `""` inside quoted fields.
fn parse_csv_rows(source: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = source.chars().peekable();
    let mut in_quotes = false;
    let mut quote_closed = false;
    let mut row_has_content = false;
    while let Some(character) = chars.next() {
        if in_quotes {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                    quote_closed = true;
                }
            } else {
                field.push(character);
            }
            continue;
        }
        if quote_closed && !matches!(character, ',' | '\r' | '\n') {
            return Err("characters after a closing quote are not allowed".to_owned());
        }
        match character {
            '"' if field.is_empty() && !quote_closed => {
                in_quotes = true;
                row_has_content = true;
            }
            '"' => return Err("quote in an unquoted field".to_owned()),
            ',' => {
                row.push(std::mem::take(&mut field));
                row_has_content = true;
                quote_closed = false;
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                row_has_content = false;
                quote_closed = false;
            }
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                row_has_content = false;
                quote_closed = false;
            }
            _ => {
                field.push(character);
                row_has_content = true;
            }
        }
    }
    if in_quotes {
        return Err("quoted field was not closed".to_owned());
    }
    if !field.is_empty() || !row.is_empty() || row_has_content {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

/// `decode[T](s: str): Result[list[T], Error]`. T is a flat struct whose fields are restricted
/// to D-TYPE-14's int/float/bool/str only (already checked at compile time as the equivalent of
/// E1002). The first row is the header, matched against field names. If a field name is missing
/// from the header, this is Err. Extra columns are ignored.
#[must_use]
pub fn decode(s: &str, target_struct: &StructDecl) -> Value {
    let rows = match parse_csv_rows(s) {
        Ok(rows) => rows,
        Err(error) => return err_value(decode_error(format!("csv: {error}"))),
    };
    let Some((header, data_rows)) = rows.split_first() else {
        return err_value(decode_error("csv: missing header row"));
    };
    let mut column_of = Vec::with_capacity(target_struct.fields.len());
    for field in &target_struct.fields {
        let Some(idx) = header.iter().position(|h| h.trim() == field.name.as_ref()) else {
            return err_value(decode_error(format!(
                "csv: header is missing field '{}'",
                field.name
            )));
        };
        let Some(kind) = field_kind_name(&field.ty) else {
            return err_value(decode_error(format!(
                "csv: field '{}' has an unsupported type (only int/float/bool/str are allowed)",
                field.name
            )));
        };
        column_of.push((idx, kind));
    }
    let mut out = Vec::with_capacity(data_rows.len());
    for row in data_rows {
        if row.len() == 1 && row[0].is_empty() {
            continue;
        }
        if row.len() != header.len() {
            return err_value(decode_error(format!(
                "csv: field count mismatch (expected {}, got {})",
                header.len(),
                row.len()
            )));
        }
        let mut fields = Vec::with_capacity(target_struct.fields.len());
        for (idx, kind) in &column_of {
            match parse_cell(&row[*idx], kind) {
                Ok(v) => fields.push(v),
                Err(msg) => return err_value(decode_error(msg)),
            }
        }
        out.push(Value::Struct(Arc::new(StructInstance {
            type_name: Arc::clone(&target_struct.name),
            fields,
        })));
    }
    ok_value(Value::List(Arc::new(out)))
}

/// `encode[T](rows: list[T]): str`. The header is the field names in struct declaration order.
#[must_use]
pub fn encode(rows: &[Value], source_struct: &StructDecl) -> String {
    let mut out = String::new();
    let header: Vec<String> = source_struct
        .fields
        .iter()
        .map(|f| escape_csv_field(f.name.as_ref()))
        .collect();
    out.push_str(&header.join(","));
    out.push('\n');
    for row in rows {
        let Value::Struct(inst) = row else {
            // Since type-checked already, list[T]'s elements are always Value::Struct
            // (defensive, unreachable).
            continue;
        };
        let mut cells = Vec::with_capacity(source_struct.fields.len());
        for (field, value) in source_struct.fields.iter().zip(inst.fields.iter()) {
            let kind = field_kind_name(&field.ty).unwrap_or("str");
            cells.push(escape_csv_field(&format_cell(value, kind)));
        }
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

/// `decode_rows(s: str): Result[list[dict[str, Value]], Error]`. Dynamic decoding for when T is
/// unknown. Every cell is a Value.Str.
#[must_use]
pub fn decode_rows(s: &str) -> Value {
    let rows = match parse_csv_rows(s) {
        Ok(rows) => rows,
        Err(error) => return err_value(decode_error(format!("csv: {error}"))),
    };
    let Some((header, data_rows)) = rows.split_first() else {
        return err_value(decode_error("csv: missing header row"));
    };
    let mut out = Vec::with_capacity(data_rows.len());
    for row in data_rows {
        if row.len() == 1 && row[0].is_empty() {
            continue;
        }
        if row.len() != header.len() {
            return err_value(decode_error(format!(
                "csv: field count mismatch (expected {}, got {})",
                header.len(),
                row.len()
            )));
        }
        let mut map = IndexMap::with_capacity(header.len());
        for (h, cell) in header.iter().zip(row.iter()) {
            map.insert(MapKey::Str(Arc::from(h.trim())), dyn_str(cell.clone()));
        }
        out.push(Value::Dict(Arc::new(map)));
    }
    ok_value(Value::List(Arc::new(out)))
}

#[cfg(test)]
mod tests {
    use super::{decode, decode_rows, encode};
    use crate::ast::{NodeId, Param, StructDecl, TypeAnn, TypeAnnKind};
    use crate::diagnostics::{FileId, Position, Span};
    use crate::eval::value::{MapKey, StructInstance, Value};
    use std::sync::Arc;

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 0, col: 0 },
            end: Position { line: 0, col: 0 },
        }
    }

    fn field(name: &str, ty: &str) -> Param {
        Param {
            name: Arc::from(name),
            ty: TypeAnn {
                kind: TypeAnnKind::Named {
                    name: Arc::from(ty),
                    args: vec![],
                },
                span: dummy_span(),
            },
            span: dummy_span(),
        }
    }

    fn user_struct() -> StructDecl {
        StructDecl {
            id: NodeId(0),
            name: Arc::from("User"),
            generics: vec![],
            fields: vec![field("name", "str"), field("age", "int")],
            field_leading_comments: vec![vec![], vec![]],
            field_trailing_comments: vec![None, None],
            methods: vec![],
            leading_comments: vec![],
            doc_comment: None,
            span: dummy_span(),
        }
    }

    fn result_ok_list(v: &Value) -> &[Value] {
        match v {
            Value::Enum(e) if e.variant_name.as_ref() == "Ok" => &e.fields,
            other => panic!("expected Ok(..), got {other:?}"),
        }
    }

    fn result_is_err(v: &Value) -> bool {
        matches!(v, Value::Enum(e) if e.variant_name.as_ref() == "Err")
    }

    #[test]
    fn decodes_typed_rows_matching_header_by_name() {
        let decl = user_struct();
        let result = decode("name,age\nalice,30\nbob,25\n", &decl);
        let fields = result_ok_list(&result);
        let Some(Value::List(rows)) = fields.first() else {
            panic!("expected list");
        };
        assert_eq!(rows.len(), 2);
        let Value::Struct(first) = &rows[0] else {
            panic!("expected struct");
        };
        assert_eq!(first.fields[0], Value::Str(Arc::from("alice")));
        assert_eq!(first.fields[1], Value::Int(30));
    }

    #[test]
    fn decode_ignores_extra_header_columns() {
        let decl = user_struct();
        let result = decode("name,age,extra\nalice,30,ignored\n", &decl);
        assert!(!result_is_err(&result));
    }

    #[test]
    fn decode_errors_on_missing_header_field() {
        let decl = user_struct();
        let result = decode("name\nalice\n", &decl);
        assert!(result_is_err(&result));
    }

    #[test]
    fn decode_errors_on_field_count_mismatch() {
        let decl = user_struct();
        let result = decode("name,age\nalice,30,extra\n", &decl);
        assert!(result_is_err(&result));
    }

    #[test]
    fn decode_handles_quoted_fields_with_commas_and_newlines() {
        let decl = user_struct();
        let result = decode("name,age\n\"doe, jane\",30\n\"multi\nline\",25\n", &decl);
        let fields = result_ok_list(&result);
        let Some(Value::List(rows)) = fields.first() else {
            panic!("expected list");
        };
        assert_eq!(rows.len(), 2);
        let Value::Struct(first) = &rows[0] else {
            panic!("expected struct");
        };
        assert_eq!(first.fields[0], Value::Str(Arc::from("doe, jane")));
        let Value::Struct(second) = &rows[1] else {
            panic!("expected struct");
        };
        assert_eq!(second.fields[0], Value::Str(Arc::from("multi\nline")));
    }

    #[test]
    fn encodes_header_and_rows() {
        let decl = user_struct();
        let rows = vec![
            Value::Struct(Arc::new(StructInstance {
                type_name: Arc::from("User"),
                fields: vec![Value::Str(Arc::from("alice")), Value::Int(30)],
            })),
            Value::Struct(Arc::new(StructInstance {
                type_name: Arc::from("User"),
                fields: vec![Value::Str(Arc::from("bob")), Value::Int(25)],
            })),
        ];
        let text = encode(&rows, &decl);
        assert!(text.contains("name,age"));
        assert!(text.contains("alice,30"));
        assert!(text.contains("bob,25"));
    }

    #[test]
    fn encode_quotes_fields_needing_escaping() {
        let decl = user_struct();
        let rows = vec![Value::Struct(Arc::new(StructInstance {
            type_name: Arc::from("User"),
            fields: vec![Value::Str(Arc::from("doe, jane")), Value::Int(1)],
        }))];
        let text = encode(&rows, &decl);
        assert!(text.contains("\"doe, jane\","));
    }

    #[test]
    fn decode_rows_dynamic_uses_str_cells() {
        let result = decode_rows("name,age\nalice,30\nbob,25\n");
        let fields = result_ok_list(&result);
        let Some(Value::List(rows)) = fields.first() else {
            panic!("expected list");
        };
        assert_eq!(rows.len(), 2);
        let Value::Dict(map) = &rows[0] else {
            panic!("expected dict");
        };
        let name = map
            .get(&MapKey::Str(Arc::from("name")))
            .unwrap_or_else(|| panic!("missing name"));
        assert!(matches!(super::super::dyn_variant(name), Some(("Str", _))));
        let age = map
            .get(&MapKey::Str(Arc::from("age")))
            .unwrap_or_else(|| panic!("missing age"));
        match super::super::dyn_variant(age) {
            Some(("Str", fields)) => {
                assert_eq!(fields.first(), Some(&Value::Str(Arc::from("30"))));
            }
            other => panic!("expected Str variant, got {other:?}"),
        }
    }

    #[test]
    fn decode_rows_errors_on_missing_header() {
        let result = decode_rows("");
        assert!(result_is_err(&result));
    }

    #[test]
    fn decode_rows_errors_on_field_count_mismatch() {
        let result = decode_rows("a,b\n1,2,3\n");
        assert!(result_is_err(&result));
    }

    #[test]
    fn round_trips_decode_then_encode_then_decode() {
        let decl = user_struct();
        let original = "name,age\nalice,30\nbob,25\n";
        let decoded1 = decode(original, &decl);
        let fields = result_ok_list(&decoded1);
        let Some(Value::List(rows)) = fields.first() else {
            panic!("expected list");
        };
        let text = encode(rows, &decl);
        let decoded2 = decode(&text, &decl);
        let fields2 = result_ok_list(&decoded2);
        let Some(Value::List(rows2)) = fields2.first() else {
            panic!("expected list");
        };
        assert_eq!(rows.as_slice(), rows2.as_slice());
    }
}
