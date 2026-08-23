//! A hand-written minimal TOML reader dedicated to the `expected.toml` schema (a plain
//! `[[case]]` array of tables plus scalar values only) (ARCHITECTURE.md §6.2). Lets work on the
//! harness itself begin without waiting for the product's `toml` codec implementation
//! (src/stdlib/codec/toml.rs) to be finished — avoiding the circularity of a test harness using
//! the very implementation under test.
//!
//! Supports only the range of the `SAMPLES_PLAN.md` §1.3 schema needed here, as a minimal
//! implementation: strings, integers, bools, arrays (elements are strings/inline tables),
//! inline tables (`{ mode = "exact", value = "..." }`), and `[[case]]` arrays of tables.
//! Supports multi-line arrays (`doc_blocks = [\n    { .. },\n]`), trailing commas, line
//! comments (`#`), and string escapes (`\n` `\t` `\r` `\\` `\"` `\0`).

use std::collections::BTreeMap;

/// One parsed TOML value. Represents only the types that appear in the `expected.toml` schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TomlValue {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<TomlValue>),
    Table(BTreeMap<String, TomlValue>),
}

impl TomlValue {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_array(&self) -> Option<&[TomlValue]> {
        match self {
            Self::Array(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_table(&self) -> Option<&BTreeMap<String, TomlValue>> {
        match self {
            Self::Table(t) => Some(t),
            _ => None,
        }
    }
}

/// The `stdout`/`stderr` field (`SAMPLES_PLAN.md` §1.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdioMode {
    Exact,
    Contains,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioExpectation {
    pub mode: StdioMode,
    pub value: String,
}

impl Default for StdioExpectation {
    fn default() -> Self {
        Self {
            mode: StdioMode::Exact,
            value: String::new(),
        }
    }
}

/// One element of `doc_blocks` (`SAMPLES_PLAN.md` §1.3, D-DOC-05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocBlockExpectation {
    pub line: u32,
    pub result: String, // "pass" | "fail"
    pub code: Option<String>,
}

/// One case from `expected.toml` (the common schema in `SAMPLES_PLAN.md` §1.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCase {
    pub id: String,
    pub entry: String,
    pub cmd: String,
    pub args: Vec<String>,
    pub stdin_file: String,
    pub exit_code: i32,
    pub diagnostics: Vec<String>,
    pub fmt_diff_expected: bool,
    pub fmt_result_file: String,
    pub stdout: StdioExpectation,
    pub stderr: StdioExpectation,
    pub doc_blocks: Vec<DocBlockExpectation>,
    pub requires_env: Vec<String>,
}

/// Reads `expected.toml` text into a `[[case]]` array of tables.
///
/// # Panics
/// Panics if the text cannot be interpreted as the `SAMPLES_PLAN.md` §1.3 schema (syntax
/// error, missing required key, etc.) — this harness only ever takes the existing
/// `expected.toml` files under `samples/` as input, as an internal tool dedicated to tests, so
/// there is no need to hand a `Result` back to the caller for malformed input (the caller is
/// always test code).
#[must_use]
pub fn parse_expected_toml(text: &str) -> Vec<ExpectedCase> {
    match parse_cases(text) {
        Ok(cases) => cases.iter().map(table_to_case).collect(),
        Err(e) => panic!("failed to parse expected.toml: {e}"),
    }
}

fn table_to_case(table: &BTreeMap<String, TomlValue>) -> ExpectedCase {
    let get_str = |key: &str| -> String {
        table
            .get(key)
            .and_then(TomlValue::as_str)
            .map(str::to_string)
            .unwrap_or_default()
    };
    let get_bool =
        |key: &str| -> bool { table.get(key).and_then(TomlValue::as_bool).unwrap_or(false) };
    let get_int = |key: &str| -> i32 {
        table
            .get(key)
            .and_then(TomlValue::as_int)
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(0)
    };
    let get_str_array = |key: &str| -> Vec<String> {
        table
            .get(key)
            .and_then(TomlValue::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(TomlValue::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let get_stdio = |key: &str| -> StdioExpectation {
        let Some(t) = table.get(key).and_then(TomlValue::as_table) else {
            return StdioExpectation::default();
        };
        let mode = match t.get("mode").and_then(TomlValue::as_str) {
            Some("contains") => StdioMode::Contains,
            _ => StdioMode::Exact,
        };
        let value = t
            .get("value")
            .and_then(TomlValue::as_str)
            .map(str::to_string)
            .unwrap_or_default();
        StdioExpectation { mode, value }
    };
    let doc_blocks = table
        .get("doc_blocks")
        .and_then(TomlValue::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(TomlValue::as_table)
                .map(|t| DocBlockExpectation {
                    line: t
                        .get("line")
                        .and_then(TomlValue::as_int)
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(0),
                    result: t
                        .get("result")
                        .and_then(TomlValue::as_str)
                        .map(str::to_string)
                        .unwrap_or_default(),
                    code: t
                        .get("code")
                        .and_then(TomlValue::as_str)
                        .map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();

    ExpectedCase {
        id: get_str("id"),
        entry: get_str("entry"),
        cmd: get_str("cmd"),
        args: get_str_array("args"),
        stdin_file: get_str("stdin_file"),
        exit_code: get_int("exit_code"),
        diagnostics: get_str_array("diagnostics"),
        fmt_diff_expected: get_bool("fmt_diff_expected"),
        fmt_result_file: get_str("fmt_result_file"),
        stdout: get_stdio("stdout"),
        stderr: get_stdio("stderr"),
        doc_blocks,
        requires_env: get_str_array("requires_env"),
    }
}

/// The parser itself. A recursive-descent parser walking over a `char` sequence (zero
/// dependencies).
struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    src: &'a str,
}

type PResult<T> = Result<T, String>;

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            chars: src.chars().collect(),
            pos: 0,
            src,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn error(&self, msg: &str) -> String {
        format!(
            "{msg} (at character position {}, input starts with: {:?})",
            self.pos,
            self.src.chars().take(40).collect::<String>()
        )
    }

    /// Skips whitespace, newlines, and `#` line comments (called only outside string
    /// literals).
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.advance();
                }
                Some('#') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                _ => break,
            }
        }
    }

    fn expect_char(&mut self, expected: char) -> PResult<()> {
        match self.advance() {
            Some(c) if c == expected => Ok(()),
            Some(c) => Err(self.error(&format!("expected '{expected}' but found '{c}'"))),
            None => Err(self.error(&format!("expected '{expected}' but reached end of input"))),
        }
    }

    /// Reads a double-bracket header like `[[case]]`. On success, returns the identifier
    /// inside (e.g. "case").
    fn try_table_array_header(&mut self) -> PResult<Option<String>> {
        self.skip_trivia();
        if self.peek() != Some('[') {
            return Ok(None);
        }
        let checkpoint = self.pos;
        self.advance(); // '['
        if self.peek() != Some('[') {
            self.pos = checkpoint;
            return Ok(None);
        }
        self.advance(); // second '['
        self.skip_trivia();
        let name = self.read_bare_key()?;
        self.skip_trivia();
        self.expect_char(']')?;
        self.expect_char(']')?;
        Ok(Some(name))
    }

    fn read_bare_key(&mut self) -> PResult<String> {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        if s.is_empty() {
            Err(self.error("expected a key name"))
        } else {
            Ok(s)
        }
    }

    fn parse_string(&mut self) -> PResult<String> {
        self.expect_char('"')?;
        let mut s = String::new();
        loop {
            match self.advance() {
                None => return Err(self.error("unterminated string literal")),
                Some('"') => return Ok(s),
                Some('\\') => match self.advance() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('\\') => s.push('\\'),
                    Some('"') => s.push('"'),
                    Some('0') => s.push('\0'),
                    Some('u') => {
                        let hex = self.read_unicode_escape_hex()?;
                        let code = u32::from_str_radix(&hex, 16)
                            .map_err(|e| self.error(&format!("invalid \\u escape: {e}")))?;
                        let ch = char::from_u32(code)
                            .ok_or_else(|| self.error("invalid Unicode code point"))?;
                        s.push(ch);
                    }
                    Some(other) => return Err(self.error(&format!("unknown escape \\{other}"))),
                    None => return Err(self.error("unterminated string literal (trailing escape)")),
                },
                Some(c) => s.push(c),
            }
        }
    }

    /// A plain `\uXXXX` (fixed 4 digits, reading somewhat leniently with optional `{}`
    /// delimiters), rather than the `\u{H..H}` style.
    fn read_unicode_escape_hex(&mut self) -> PResult<String> {
        let braced = self.peek() == Some('{');
        if braced {
            self.advance();
        }
        let mut hex = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_hexdigit() {
                hex.push(c);
                self.advance();
            } else {
                break;
            }
        }
        if braced {
            self.expect_char('}')?;
        }
        if hex.is_empty() {
            Err(self.error("\\u escape has no hex digits"))
        } else {
            Ok(hex)
        }
    }

    fn parse_bare_or_number_or_bool(&mut self) -> PResult<TomlValue> {
        let start = self.pos;
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }
        if s.is_empty() {
            self.pos = start;
            return Err(self.error("expected a value (string/array/inline table/number/bool)"));
        }
        match s.as_str() {
            "true" => Ok(TomlValue::Bool(true)),
            "false" => Ok(TomlValue::Bool(false)),
            _ => s
                .parse::<i64>()
                .map(TomlValue::Int)
                .map_err(|e| self.error(&format!("invalid literal '{s}': {e}"))),
        }
    }

    fn parse_array(&mut self) -> PResult<Vec<TomlValue>> {
        self.expect_char('[')?;
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek() == Some(']') {
                self.advance();
                return Ok(items);
            }
            items.push(self.parse_value()?);
            self.skip_trivia();
            match self.peek() {
                Some(',') => {
                    self.advance();
                }
                Some(']') => {
                    self.advance();
                    return Ok(items);
                }
                Some(c) => {
                    return Err(
                        self.error(&format!("expected ',' or ']' in array but found '{c}'"))
                    );
                }
                None => return Err(self.error("unterminated array")),
            }
        }
    }

    fn parse_inline_table(&mut self) -> PResult<BTreeMap<String, TomlValue>> {
        self.expect_char('{')?;
        let mut table = BTreeMap::new();
        loop {
            self.skip_trivia();
            if self.peek() == Some('}') {
                self.advance();
                return Ok(table);
            }
            let key = self.read_bare_key()?;
            self.skip_trivia();
            self.expect_char('=')?;
            self.skip_trivia();
            let value = self.parse_value()?;
            table.insert(key, value);
            self.skip_trivia();
            match self.peek() {
                Some(',') => {
                    self.advance();
                }
                Some('}') => {
                    self.advance();
                    return Ok(table);
                }
                Some(c) => {
                    return Err(self.error(&format!(
                        "expected ',' or '}}' in inline table but found '{c}'"
                    )));
                }
                None => return Err(self.error("unterminated inline table")),
            }
        }
    }

    fn parse_value(&mut self) -> PResult<TomlValue> {
        self.skip_trivia();
        match self.peek() {
            Some('"') => Ok(TomlValue::Str(self.parse_string()?)),
            Some('[') => Ok(TomlValue::Array(self.parse_array()?)),
            Some('{') => Ok(TomlValue::Table(self.parse_inline_table()?)),
            Some(_) => self.parse_bare_or_number_or_bool(),
            None => Err(self.error("expected a value but reached end of input")),
        }
    }

    /// Reads one `[[case]] ... key = value ...` block as a table (up to the next `[[` header
    /// or EOF).
    fn parse_case_body(&mut self) -> PResult<BTreeMap<String, TomlValue>> {
        let mut table = BTreeMap::new();
        loop {
            self.skip_trivia();
            match self.peek() {
                None | Some('[') => return Ok(table),
                Some(_) => {
                    let key = self.read_bare_key()?;
                    self.skip_trivia();
                    self.expect_char('=')?;
                    let value = self.parse_value()?;
                    table.insert(key, value);
                }
            }
        }
    }
}

fn parse_cases(text: &str) -> PResult<Vec<BTreeMap<String, TomlValue>>> {
    let mut parser = Parser::new(text);
    let mut cases = Vec::new();
    loop {
        match parser.try_table_array_header()? {
            None => {
                parser.skip_trivia();
                if parser.peek().is_none() {
                    return Ok(cases);
                }
                return Err(parser.error("only '[[case]]' headers are allowed at the top level"));
            }
            Some(name) if name == "case" => {
                let body = parser.parse_case_body()?;
                cases.push(body);
            }
            Some(other) => {
                return Err(parser.error(&format!("unknown array-of-tables name '{other}'")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn parses_minimal_single_case() {
        let text = r#"
[[case]]
id = "run_ok"
entry = "entry_main.ybm"
cmd = "run"
args = []
stdin_file = ""
exit_code = 0
diagnostics = []
fmt_diff_expected = false
fmt_result_file = ""
stdout = { mode = "exact", value = "hello\n" }
stderr = { mode = "exact", value = "" }
doc_blocks = []
requires_env = []
notes = "description"
"#;
        let cases = parse_expected_toml(text);
        assert_eq!(cases.len(), 1);
        let case = &cases[0];
        assert_eq!(case.id, "run_ok");
        assert_eq!(case.entry, "entry_main.ybm");
        assert_eq!(case.cmd, "run");
        assert_eq!(case.exit_code, 0);
        assert!(!case.fmt_diff_expected);
        assert_eq!(case.stdout.mode, StdioMode::Exact);
        assert_eq!(case.stdout.value, "hello\n");
        assert!(case.doc_blocks.is_empty());
    }

    #[test]
    fn parses_multiple_cases_and_diagnostics_array() {
        let text = r#"
[[case]]
id = "a"
entry = "e.ybm"
cmd = "check"
args = []
stdin_file = ""
exit_code = 1
diagnostics = ["E1002", "E1050", "E1021"]
fmt_diff_expected = false
fmt_result_file = ""
stdout = { mode = "contains", value = "x" }
stderr = { mode = "exact", value = "" }
doc_blocks = []
requires_env = ["YABUMI_TEST_HTTP_BASE"]

[[case]]
id = "b"
entry = "e2.ybm"
cmd = "run"
args = ["--check"]
stdin_file = "in.txt"
exit_code = 0
diagnostics = []
fmt_diff_expected = false
fmt_result_file = ""
stdout = { mode = "exact", value = "" }
stderr = { mode = "exact", value = "" }
doc_blocks = []
requires_env = []
"#;
        let cases = parse_expected_toml(text);
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].diagnostics, vec!["E1002", "E1050", "E1021"]);
        assert_eq!(cases[0].requires_env, vec!["YABUMI_TEST_HTTP_BASE"]);
        assert_eq!(cases[1].args, vec!["--check"]);
        assert_eq!(cases[1].stdin_file, "in.txt");
    }

    #[test]
    fn parses_multiline_doc_blocks_array_with_trailing_comma() {
        let text = r#"
[[case]]
id = "c"
entry = "e.ybm"
cmd = "test"
args = []
stdin_file = ""
exit_code = 1
diagnostics = []
fmt_diff_expected = false
fmt_result_file = ""
stdout = { mode = "contains", value = "" }
stderr = { mode = "contains", value = "E6004" }
doc_blocks = [
    { line = 11, result = "fail", code = "E6004" },
    { line = 19, result = "pass" },
]
requires_env = []
"#;
        let cases = parse_expected_toml(text);
        assert_eq!(cases.len(), 1);
        let blocks = &cases[0].doc_blocks;
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].line, 11);
        assert_eq!(blocks[0].result, "fail");
        assert_eq!(blocks[0].code.as_deref(), Some("E6004"));
        assert_eq!(blocks[1].line, 19);
        assert_eq!(blocks[1].result, "pass");
        assert_eq!(blocks[1].code, None);
    }

    #[test]
    fn parses_string_escapes_and_embedded_quotes() {
        let text = r#"
[[case]]
id = "d"
entry = "e.ybm"
cmd = "run"
args = []
stdin_file = ""
exit_code = 0
diagnostics = []
fmt_diff_expected = false
fmt_result_file = ""
stdout = { mode = "exact", value = "line1\nline2\t\"quoted\"\\end" }
stderr = { mode = "exact", value = "" }
doc_blocks = []
requires_env = []
notes = "\"ab12cd34\" quoting check; a multibyte comment should also pass through cleanly (e.g. café)"
"#;
        let cases = parse_expected_toml(text);
        assert_eq!(cases[0].stdout.value, "line1\nline2\t\"quoted\"\\end");
    }

    #[test]
    fn parses_leading_comment_lines() {
        let text = r#"
# this is an explanatory comment
# second comment line
[[case]]
id = "e"
entry = "e.ybm" # trailing comment
cmd = "run"
args = []
stdin_file = ""
exit_code = 0
diagnostics = []
fmt_diff_expected = false
fmt_result_file = ""
stdout = { mode = "exact", value = "" }
stderr = { mode = "exact", value = "" }
doc_blocks = []
requires_env = []
"#;
        let cases = parse_expected_toml(text);
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].entry, "e.ybm");
    }

    /// Confirms that every real `expected.toml` under samples/ (89 of them) parses
    /// (verification of `SAMPLES_PLAN.md` §1.3 conformance). This is a unit test of the
    /// harness itself, so it is not marked `#[ignore]` (a completion condition of Unit 18).
    #[test]
    fn parses_every_real_expected_toml_in_samples() {
        let samples_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let mut dirs = Vec::new();
        collect_expected_toml_dirs(&samples_root, &mut dirs);
        assert!(
            !dirs.is_empty(),
            "no expected.toml found anywhere under samples/: {}",
            samples_root.display()
        );
        let mut failures = Vec::new();
        for expected_path in &dirs {
            let text = match fs::read_to_string(expected_path) {
                Ok(t) => t,
                Err(e) => {
                    failures.push(format!("{}: failed to read: {e}", expected_path.display()));
                    continue;
                }
            };
            match parse_cases(&text) {
                Ok(cases) => {
                    if cases.is_empty() {
                        failures.push(format!("{}: no [[case]] found", expected_path.display()));
                    }
                }
                Err(e) => failures.push(format!("{}: {e}", expected_path.display())),
            }
        }
        assert!(
            failures.is_empty(),
            "failed to parse {} expected.toml file(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    fn collect_expected_toml_dirs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_expected_toml_dirs(&path, out);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("expected.toml") {
                out.push(path);
            }
        }
    }
}
