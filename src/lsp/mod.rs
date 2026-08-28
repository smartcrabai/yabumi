mod json;
mod pos;
mod query;
mod transport;
mod uri;

use crate::diagnostics::{FileId, SourceMap};
use crate::driver::{self, Analysis, Overlay};
use crate::lexer::Lexer;
use crate::parser::comment_attach::attach_comments;
use crate::parser::parse_module;
use json::Json;
use pos::{Encoding, document_end, from_lsp_pos, span_to_range};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(crate) fn run_server(reader: impl BufRead, writer: impl Write) -> ExitCode {
    let mut reader = reader;
    let mut writer = writer;
    let mut state = ServerState::new();
    loop {
        let body = match transport::read_message(&mut reader) {
            Ok(Some(body)) => body,
            Ok(None) => return ExitCode::SUCCESS,
            Err(_) => return ExitCode::FAILURE,
        };
        let Ok(message) = json::parse(&body) else {
            if send_error(&mut writer, Json::Null, -32700, "parse error").is_err() {
                return ExitCode::FAILURE;
            }
            continue;
        };
        let Some(object) = message.as_obj() else {
            if send_error(&mut writer, Json::Null, -32600, "invalid request").is_err() {
                return ExitCode::FAILURE;
            }
            continue;
        };
        let id = object.get("id").cloned();
        if object.get("jsonrpc").and_then(Json::as_str) != Some("2.0") {
            if send_error(
                &mut writer,
                id.unwrap_or(Json::Null),
                -32600,
                "invalid request",
            )
            .is_err()
            {
                return ExitCode::FAILURE;
            }
            continue;
        }
        let Some(method) = object
            .get("method")
            .and_then(Json::as_str)
            .map(str::to_owned)
        else {
            if send_error(
                &mut writer,
                id.unwrap_or(Json::Null),
                -32600,
                "invalid request",
            )
            .is_err()
            {
                return ExitCode::FAILURE;
            }
            continue;
        };
        if method == "exit" {
            return if state.shutdown_requested {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        if dispatch(&mut state, &method, object.get("params"), id, &mut writer).is_err() {
            return ExitCode::FAILURE;
        }
    }
}

struct ServerState {
    encoding: Encoding,
    docs: HashMap<PathBuf, String>,
    analyses: HashMap<PathBuf, Analysis>,
    shutdown_requested: bool,
}

impl ServerState {
    fn new() -> Self {
        Self {
            encoding: Encoding::Utf16,
            docs: HashMap::new(),
            analyses: HashMap::new(),
            shutdown_requested: false,
        }
    }

    fn reanalyze(
        &mut self,
        path: &Path,
        overlay: &Overlay,
        writer: &mut impl Write,
    ) -> io::Result<()> {
        let previous_paths = self
            .analyses
            .get(path)
            .map(analysis_paths)
            .unwrap_or_default();
        let analysis = driver::analyze(path, overlay);
        let current_paths = analysis_paths(&analysis);
        let diagnostics = diagnostic_values(&analysis, self.encoding);
        self.analyses.insert(path.to_path_buf(), analysis);

        for previous_path in previous_paths {
            if !current_paths.contains(&previous_path) {
                publish_diagnostics(writer, &previous_path, Vec::new())?;
            }
        }
        for (diagnostic_path, values) in diagnostics {
            publish_diagnostics(writer, &diagnostic_path, values)?;
        }
        Ok(())
    }

    fn reanalyze_all(&mut self, writer: &mut impl Write) -> io::Result<()> {
        let overlay = Overlay(self.docs.clone());
        let paths: Vec<PathBuf> = self.docs.keys().cloned().collect();
        for path in paths {
            self.reanalyze(&path, &overlay, writer)?;
        }
        Ok(())
    }
}

fn dispatch(
    state: &mut ServerState,
    method: &str,
    params: Option<&Json>,
    id: Option<Json>,
    writer: &mut impl Write,
) -> io::Result<()> {
    match method {
        "initialize" => {
            let Some(encoding) = choose_encoding(params) else {
                if let Some(id) = id {
                    send_error(writer, id, -32602, "no supported position encoding")?;
                }
                return Ok(());
            };
            state.encoding = encoding;
            if let Some(id) = id {
                send_response(writer, id, initialize_result(state.encoding))?;
            }
        }
        "initialized" | "$/cancelRequest" => {}
        "shutdown" => {
            state.shutdown_requested = true;
            if let Some(id) = id {
                send_response(writer, id, Json::Null)?;
            }
        }
        "textDocument/didOpen" => did_open(state, params, writer)?,
        "textDocument/didChange" => did_change(state, params, writer)?,
        "textDocument/didSave" => did_save(state, params, writer)?,
        "textDocument/didClose" => did_close(state, params, writer)?,
        "textDocument/hover" | "textDocument/definition" => {
            if let Some(id) = id {
                if valid_position_params(params) {
                    let result = if method == "textDocument/hover" {
                        hover_result(state, params)
                    } else {
                        definition_result(state, params)
                    };
                    send_response(writer, id, result)?;
                } else {
                    send_error(writer, id, -32602, "invalid text document position")?;
                }
            }
        }
        "textDocument/formatting" => {
            let Some(id) = id else { return Ok(()) };
            if !valid_formatting_params(params) {
                return send_error(writer, id, -32602, "invalid formatting parameters");
            }
            send_response(writer, id, formatting_result(state, params))?;
        }
        _ => {
            if let Some(id) = id {
                send_error(writer, id, -32601, "method not found")?;
            }
        }
    }
    Ok(())
}

fn valid_position_params(params: Option<&Json>) -> bool {
    document_path(params).is_some() && lsp_position(params).is_some()
}

fn valid_formatting_params(params: Option<&Json>) -> bool {
    document_path(params).is_some()
        && params
            .and_then(Json::as_obj)
            .and_then(|params| params.get("options"))
            .and_then(Json::as_obj)
            .is_some()
}

fn choose_encoding(params: Option<&Json>) -> Option<Encoding> {
    let Some(encodings) = params
        .and_then(Json::as_obj)
        .and_then(|params| params.get("capabilities"))
        .and_then(Json::as_obj)
        .and_then(|capabilities| capabilities.get("general"))
        .and_then(Json::as_obj)
        .and_then(|general| general.get("positionEncodings"))
        .and_then(Json::as_arr)
    else {
        return Some(Encoding::Utf16);
    };
    if encodings
        .iter()
        .any(|encoding| encoding.as_str() == Some("utf-32"))
    {
        Some(Encoding::Utf32)
    } else if encodings
        .iter()
        .any(|encoding| encoding.as_str() == Some("utf-16"))
    {
        Some(Encoding::Utf16)
    } else {
        None
    }
}

fn initialize_result(encoding: Encoding) -> Json {
    let position_encoding = match encoding {
        Encoding::Utf16 => "utf-16",
        Encoding::Utf32 => "utf-32",
    };
    Json::obj(vec![
        (
            "capabilities",
            Json::obj(vec![
                ("positionEncoding", Json::Str(position_encoding.to_owned())),
                ("textDocumentSync", Json::Int(1)),
                ("hoverProvider", Json::Bool(true)),
                ("definitionProvider", Json::Bool(true)),
                ("documentFormattingProvider", Json::Bool(true)),
            ]),
        ),
        (
            "serverInfo",
            Json::obj(vec![
                ("name", Json::Str("ybm".to_owned())),
                ("version", Json::Str(env!("CARGO_PKG_VERSION").to_owned())),
            ]),
        ),
    ])
}

fn did_open(
    state: &mut ServerState,
    params: Option<&Json>,
    writer: &mut impl Write,
) -> io::Result<()> {
    let Some(object) = params.and_then(Json::as_obj) else {
        return Ok(());
    };
    let Some(document) = object.get("textDocument").and_then(Json::as_obj) else {
        return Ok(());
    };
    let Some(uri) = document.get("uri").and_then(Json::as_str) else {
        return Ok(());
    };
    let Some(path) = canonical_path(uri) else {
        return Ok(());
    };
    let Some(text) = document.get("text").and_then(Json::as_str) else {
        return Ok(());
    };
    state.docs.insert(path, text.to_owned());
    state.reanalyze_all(writer)
}

fn did_change(
    state: &mut ServerState,
    params: Option<&Json>,
    writer: &mut impl Write,
) -> io::Result<()> {
    let Some((path, changes)) = document_and_changes(params) else {
        return Ok(());
    };
    let Some(change) = changes.last().and_then(Json::as_obj) else {
        return Ok(());
    };
    let Some(text) = change.get("text").and_then(Json::as_str) else {
        return Ok(());
    };
    state.docs.insert(path, text.to_owned());
    state.reanalyze_all(writer)
}

fn did_save(
    state: &mut ServerState,
    params: Option<&Json>,
    writer: &mut impl Write,
) -> io::Result<()> {
    let Some(path) = document_path(params) else {
        return Ok(());
    };
    if state.docs.contains_key(&path) {
        state.reanalyze_all(writer)?;
    }
    Ok(())
}

fn did_close(
    state: &mut ServerState,
    params: Option<&Json>,
    writer: &mut impl Write,
) -> io::Result<()> {
    let Some(path) = document_path(params) else {
        return Ok(());
    };
    let previous_paths = state
        .analyses
        .get(&path)
        .map(analysis_paths)
        .unwrap_or_default();
    state.docs.remove(&path);
    state.analyses.remove(&path);
    state.reanalyze_all(writer)?;

    // The closed document may have been the only open root whose analysis included sibling
    // modules. Clear every path that is no longer covered by any remaining open analysis.
    let mut paths_to_clear = previous_paths;
    paths_to_clear.push(path);
    paths_to_clear.sort();
    paths_to_clear.dedup();
    for stale_path in paths_to_clear {
        let still_analyzed = state
            .analyses
            .values()
            .any(|analysis| analysis_paths(analysis).contains(&stale_path));
        if !still_analyzed {
            publish_diagnostics(writer, &stale_path, Vec::new())?;
        }
    }
    Ok(())
}

fn document_and_changes(params: Option<&Json>) -> Option<(PathBuf, &[Json])> {
    let path = document_path(params)?;
    let changes = params
        .and_then(Json::as_obj)?
        .get("contentChanges")?
        .as_arr()?;
    Some((path, changes))
}

fn document_path(params: Option<&Json>) -> Option<PathBuf> {
    let uri = params
        .and_then(|params| params.get("textDocument"))
        .and_then(Json::as_obj)
        .and_then(|document| document.get("uri"))
        .and_then(Json::as_str)?;
    canonical_path(uri)
}

fn canonical_path(uri: &str) -> Option<PathBuf> {
    uri::uri_to_path(uri).map(|path| driver::canonical_or_raw(&path))
}

fn lsp_position(params: Option<&Json>) -> Option<(u32, u32)> {
    let position = params
        .and_then(Json::as_obj)
        .and_then(|params| params.get("position"))
        .and_then(Json::as_obj)?;
    let line = position
        .get("line")?
        .as_i64()
        .and_then(|value| u32::try_from(value).ok())?;
    let character = position
        .get("character")?
        .as_i64()
        .and_then(|value| u32::try_from(value).ok())?;
    Some((line, character))
}

fn expression_at_position<'a>(
    state: &'a ServerState,
    params: Option<&Json>,
) -> Option<(
    &'a SourceMap,
    &'a crate::eval::env::Program,
    crate::lsp::query::ExprAt,
)> {
    let path = document_path(params)?;
    let Some(Analysis::Checked {
        sources,
        program: Some(program),
        ..
    }) = state.analyses.get(&path)
    else {
        return None;
    };
    let text = sources.file(FileId(0)).text();
    let (line, character) = lsp_position(params)?;
    let position = from_lsp_pos(text, line, character, state.encoding);
    let expr = query::expr_at(program, FileId(0), position)?;
    Some((sources, program, expr))
}

fn hover_result(state: &ServerState, params: Option<&Json>) -> Json {
    let Some((sources, program, expr)) = expression_at_position(state, params) else {
        return Json::Null;
    };
    let Some(ty) = program.resolutions.expr_ty.get(&expr.id) else {
        return Json::Null;
    };
    let range = span_to_range(
        sources.file(expr.span.file).text(),
        expr.span,
        state.encoding,
    );
    Json::obj(vec![
        (
            "contents",
            Json::obj(vec![
                ("kind", Json::Str("markdown".to_owned())),
                ("value", Json::Str(format!("```yabumi\n{ty}\n```"))),
            ]),
        ),
        ("range", range_json(range)),
    ])
}

fn definition_result(state: &ServerState, params: Option<&Json>) -> Json {
    let Some((sources, program, expr)) = expression_at_position(state, params) else {
        return Json::Null;
    };
    let Some(span) = query::definition_span(program, &expr) else {
        return Json::Null;
    };
    Json::obj(vec![
        ("uri", Json::Str(uri::path_to_uri(sources.path(span.file)))),
        (
            "range",
            range_json(span_to_range(
                sources.file(span.file).text(),
                span,
                state.encoding,
            )),
        ),
    ])
}

fn formatting_result(state: &ServerState, params: Option<&Json>) -> Json {
    let Some(path) = document_path(params) else {
        return Json::Null;
    };
    let Some(text) = state.docs.get(&path) else {
        return Json::Null;
    };
    let file = FileId(0);
    let (tokens, comments, lex_diagnostics) = Lexer::new(text, file).tokenize();
    if lex_diagnostics.has_any() {
        return Json::Null;
    }
    let (mut module, parse_diagnostics) = parse_module(&tokens, file);
    if parse_diagnostics.has_any() {
        return Json::Null;
    }
    attach_comments(&mut module, comments);
    let formatted = driver::format_module_text(text, &module);
    if formatted == *text {
        return Json::Arr(Vec::new());
    }
    let end = document_end(text, state.encoding);
    Json::Arr(vec![Json::obj(vec![
        ("range", range_json(((0, 0), end))),
        ("newText", Json::Str(formatted)),
    ])])
}

fn source_file_id(index: usize) -> FileId {
    FileId(u32::try_from(index).unwrap_or(u32::MAX))
}

fn analysis_paths(analysis: &Analysis) -> Vec<PathBuf> {
    match analysis {
        Analysis::Io(error) => vec![error.path.clone()],
        Analysis::Checked { sources, .. } => (0..sources.len())
            .map(|index| sources.path(source_file_id(index)).to_path_buf())
            .collect(),
    }
}

fn diagnostic_values(analysis: &Analysis, encoding: Encoding) -> Vec<(PathBuf, Vec<Json>)> {
    match analysis {
        Analysis::Io(error) => vec![(
            error.path.clone(),
            vec![diagnostic_json(
                ((0, 0), (0, 1)),
                1,
                error.code.to_string(),
                format!("{}: {}", error.path.display(), error.message),
            )],
        )],
        Analysis::Checked {
            sources,
            diagnostics,
            ..
        } => {
            let mut values: Vec<(PathBuf, Vec<Json>)> = (0..sources.len())
                .map(|index| {
                    (
                        sources.path(source_file_id(index)).to_path_buf(),
                        Vec::new(),
                    )
                })
                .collect();
            for diagnostic in diagnostics {
                let file_index = diagnostic.span.file.0 as usize;
                let Some((_, file_values)) = values.get_mut(file_index) else {
                    continue;
                };
                let range = span_to_range(
                    sources.file(diagnostic.span.file).text(),
                    diagnostic.span,
                    encoding,
                );
                let severity = if (4000..5000).contains(&diagnostic.code.numeric()) {
                    2
                } else {
                    1
                };
                file_values.push(diagnostic_json(
                    range,
                    severity,
                    diagnostic.code.to_string(),
                    diagnostic.message.clone(),
                ));
            }
            values
        }
    }
}

fn diagnostic_json(
    range: ((u32, u32), (u32, u32)),
    severity: i64,
    code: String,
    message: String,
) -> Json {
    Json::obj(vec![
        ("range", range_json(range)),
        ("severity", Json::Int(severity)),
        ("code", Json::Str(code)),
        ("source", Json::Str("ybm".to_owned())),
        ("message", Json::Str(message)),
    ])
}

fn publish_diagnostics(
    writer: &mut impl Write,
    path: &Path,
    diagnostics: Vec<Json>,
) -> io::Result<()> {
    let message = Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".to_owned())),
        (
            "method",
            Json::Str("textDocument/publishDiagnostics".to_owned()),
        ),
        (
            "params",
            Json::obj(vec![
                ("uri", Json::Str(uri::path_to_uri(path))),
                ("diagnostics", Json::Arr(diagnostics)),
            ]),
        ),
    ]);
    transport::write_message(writer, &message.to_string())
}

fn range_json(range: ((u32, u32), (u32, u32))) -> Json {
    Json::obj(vec![
        ("start", position_json(range.0)),
        ("end", position_json(range.1)),
    ])
}

fn position_json(position: (u32, u32)) -> Json {
    Json::obj(vec![
        ("line", Json::Int(i64::from(position.0))),
        ("character", Json::Int(i64::from(position.1))),
    ])
}

fn send_response(writer: &mut impl Write, id: Json, result: Json) -> io::Result<()> {
    let response = Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".to_owned())),
        ("id", id),
        ("result", result),
    ]);
    transport::write_message(writer, &response.to_string())
}

fn send_error(writer: &mut impl Write, id: Json, code: i64, message: &str) -> io::Result<()> {
    let response = Json::obj(vec![
        ("jsonrpc", Json::Str("2.0".to_owned())),
        ("id", id),
        (
            "error",
            Json::obj(vec![
                ("code", Json::Int(code)),
                ("message", Json::Str(message.to_owned())),
            ]),
        ),
    ]);
    transport::write_message(writer, &response.to_string())
}

#[cfg(test)]
mod tests {
    use super::run_server;
    use crate::lsp::json::{Json, parse};
    use crate::lsp::transport::write_message;
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_id() -> u128 {
        let timestamp = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_nanos(),
            Err(error) => panic!("clock before epoch: {error}"),
        };
        timestamp * 1000 + u128::from(NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed))
    }

    fn must<T>(result: Option<T>) -> T {
        match result {
            Some(value) => value,
            None => panic!("expected a JSON field"),
        }
    }

    fn request(id: i64, method: &str, params: Json) -> Json {
        Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".to_owned())),
            ("id", Json::Int(id)),
            ("method", Json::Str(method.to_owned())),
            ("params", params),
        ])
    }

    fn notification(method: &str, params: Json) -> Json {
        Json::obj(vec![
            ("jsonrpc", Json::Str("2.0".to_owned())),
            ("method", Json::Str(method.to_owned())),
            ("params", params),
        ])
    }

    fn initialize_params() -> Json {
        Json::obj(vec![(
            "capabilities",
            Json::obj(vec![(
                "general",
                Json::obj(vec![(
                    "positionEncodings",
                    Json::Arr(vec![Json::Str("utf-16".to_owned())]),
                )]),
            )]),
        )])
    }

    fn run_messages(messages: &[Json]) -> (std::process::ExitCode, Vec<Json>) {
        let mut input = Vec::new();
        for message in messages {
            let body = message.to_string();
            match write_message(&mut input, &body) {
                Ok(()) => {}
                Err(error) => panic!("expected in-memory write: {error}"),
            }
        }
        let mut output = Vec::new();
        let exit = run_server(Cursor::new(input), &mut output);
        let mut reader = Cursor::new(output);
        let mut messages = Vec::new();
        while let Some(line) = match crate::lsp::transport::read_message(&mut reader) {
            Ok(message) => message,
            Err(error) => panic!("expected in-memory read: {error}"),
        } {
            messages.push(match parse(&line) {
                Ok(message) => message,
                Err(error) => panic!("expected response JSON: {error}"),
            });
        }
        (exit, messages)
    }

    fn temp_file(text: &str) -> (PathBuf, String) {
        let nonce = unique_temp_id();
        let path = std::env::temp_dir().join(format!("ybm-lsp-{nonce}.ybm"));
        match fs::write(&path, text) {
            Ok(()) => {}
            Err(error) => panic!("write temp file: {error}"),
        }
        let canonical = match fs::canonicalize(&path) {
            Ok(canonical) => canonical,
            Err(error) => panic!("canonicalize temp file: {error}"),
        };
        let uri = crate::lsp::uri::path_to_uri(&canonical);
        (path, uri)
    }

    fn remove_temp(path: PathBuf) {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) => panic!("remove temp file: {error}"),
        }
    }

    fn temp_project(entry_text: &str, module_text: &str) -> (PathBuf, PathBuf, String) {
        let nonce = unique_temp_id();
        let directory = std::env::temp_dir().join(format!("ybm-lsp-project-{nonce}"));
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) => panic!("create temp project: {error}"),
        }
        let entry = directory.join("entry.ybm");
        let module = directory.join("mod_bad.ybm");
        match fs::write(&entry, entry_text) {
            Ok(()) => {}
            Err(error) => panic!("write temp entry: {error}"),
        }
        match fs::write(&module, module_text) {
            Ok(()) => {}
            Err(error) => panic!("write temp module: {error}"),
        }
        let entry = match fs::canonicalize(entry) {
            Ok(path) => path,
            Err(error) => panic!("canonicalize temp entry: {error}"),
        };
        let module = match fs::canonicalize(module) {
            Ok(path) => path,
            Err(error) => panic!("canonicalize temp module: {error}"),
        };
        let uri = crate::lsp::uri::path_to_uri(&entry);
        (entry, module, uri)
    }

    fn remove_temp_project(entry: PathBuf, module: PathBuf) {
        match fs::remove_file(entry) {
            Ok(()) => {}
            Err(error) => panic!("remove temp entry: {error}"),
        }
        match fs::remove_file(&module) {
            Ok(()) => {}
            Err(error) => panic!("remove temp module: {error}"),
        }
        let directory = module
            .parent()
            .unwrap_or_else(|| panic!("temp module has no parent"));
        match fs::remove_dir(directory) {
            Ok(()) => {}
            Err(error) => panic!("remove temp project: {error}"),
        }
    }

    fn did_open(uri: &str, text: &str) -> Json {
        notification(
            "textDocument/didOpen",
            Json::obj(vec![(
                "textDocument",
                Json::obj(vec![
                    ("uri", Json::Str(uri.to_owned())),
                    ("languageId", Json::Str("yabumi".to_owned())),
                    ("version", Json::Int(1)),
                    ("text", Json::Str(text.to_owned())),
                ]),
            )]),
        )
    }

    fn did_close(uri: &str) -> Json {
        notification(
            "textDocument/didClose",
            Json::obj(vec![(
                "textDocument",
                Json::obj(vec![("uri", Json::Str(uri.to_owned()))]),
            )]),
        )
    }

    fn position_params(uri: &str, line: i64, character: i64) -> Json {
        Json::obj(vec![
            (
                "textDocument",
                Json::obj(vec![("uri", Json::Str(uri.to_owned()))]),
            ),
            (
                "position",
                Json::obj(vec![
                    ("line", Json::Int(line)),
                    ("character", Json::Int(character)),
                ]),
            ),
        ])
    }

    fn formatting_params(uri: &str) -> Json {
        Json::obj(vec![
            (
                "textDocument",
                Json::obj(vec![("uri", Json::Str(uri.to_owned()))]),
            ),
            ("options", Json::obj(Vec::new())),
        ])
    }

    #[test]
    fn publishes_entry_diagnostics_with_lsp_ranges() {
        let text = "x = 5\nx = 6\nprint(x)";
        let (path, uri) = temp_file(text);
        let messages = [
            request(1, "initialize", initialize_params()),
            did_open(&uri, text),
            request(2, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        let diagnostic = must(
            responses
                .get(1)
                .and_then(|message| message.get("params"))
                .and_then(Json::as_obj)
                .and_then(|params| params.get("diagnostics"))
                .and_then(Json::as_arr)
                .and_then(|diagnostics| diagnostics.first()),
        );
        assert_eq!(diagnostic.get("code"), Some(&Json::Str("E3001".to_owned())));
        let start_line = diagnostic
            .get("range")
            .and_then(Json::as_obj)
            .and_then(|range| range.get("start"))
            .and_then(Json::as_obj)
            .and_then(|start| start.get("line"))
            .and_then(Json::as_i64);
        assert_eq!(start_line, Some(1));
        remove_temp(path);
    }

    #[test]
    fn publishes_diagnostics_for_imported_sibling_modules() {
        let (entry, module, uri) = temp_project(
            "print(1)\n",
            "module\n\ndef broken(): int\n    return \"bad\"\n",
        );
        let module_uri = crate::lsp::uri::path_to_uri(&module);
        let entry_text = "print(1)\n";
        let messages = [
            request(1, "initialize", initialize_params()),
            did_open(&uri, entry_text),
            request(2, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        let published = responses
            .iter()
            .find(|message| {
                message.get("method").and_then(Json::as_str)
                    == Some("textDocument/publishDiagnostics")
                    && message
                        .get("params")
                        .and_then(Json::as_obj)
                        .and_then(|params| params.get("uri"))
                        .and_then(Json::as_str)
                        == Some(module_uri.as_str())
            })
            .unwrap_or_else(|| panic!("expected sibling diagnostics publication"));
        let diagnostics = published
            .get("params")
            .and_then(Json::as_obj)
            .and_then(|params| params.get("diagnostics"))
            .and_then(Json::as_arr)
            .unwrap_or_else(|| panic!("expected sibling diagnostics"));
        assert!(!diagnostics.is_empty());
        remove_temp_project(entry, module);
    }

    #[test]
    fn clears_sibling_diagnostics_when_their_only_open_root_closes() {
        let (entry, module, uri) = temp_project(
            "print(1)\n",
            "module\n\ndef broken(): int\n    return \"bad\"\n",
        );
        let module_uri = crate::lsp::uri::path_to_uri(&module);
        let messages = [
            request(1, "initialize", initialize_params()),
            did_open(&uri, "print(1)\n"),
            did_close(&uri),
            request(2, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        let sibling_publications: Vec<&Json> = responses
            .iter()
            .filter(|message| {
                message.get("method").and_then(Json::as_str)
                    == Some("textDocument/publishDiagnostics")
                    && message
                        .get("params")
                        .and_then(Json::as_obj)
                        .and_then(|params| params.get("uri"))
                        .and_then(Json::as_str)
                        == Some(module_uri.as_str())
            })
            .collect();
        let last_diagnostics = sibling_publications
            .last()
            .and_then(|message| message.get("params"))
            .and_then(Json::as_obj)
            .and_then(|params| params.get("diagnostics"))
            .and_then(Json::as_arr);
        assert_eq!(last_diagnostics, Some([].as_slice()));
        remove_temp_project(entry, module);
    }

    #[test]
    fn resolves_definition_and_hover_for_checked_source() {
        let text = "def add(a: int, b: int): int\n    return a + b\n\nprint(add(1, 2))\n";
        let (path, uri) = temp_file(text);
        let messages = [
            request(1, "initialize", initialize_params()),
            did_open(&uri, text),
            request(2, "textDocument/definition", position_params(&uri, 3, 6)),
            request(3, "textDocument/hover", position_params(&uri, 3, 6)),
            request(4, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        let definition = responses
            .get(2)
            .and_then(|message| message.get("result"))
            .and_then(Json::as_obj);
        assert_eq!(
            definition
                .and_then(|location| location.get("uri"))
                .and_then(Json::as_str),
            Some(uri.as_str())
        );
        let definition_line = definition
            .and_then(|location| location.get("range"))
            .and_then(Json::as_obj)
            .and_then(|range| range.get("start"))
            .and_then(Json::as_obj)
            .and_then(|start| start.get("line"))
            .and_then(Json::as_i64);
        assert_eq!(definition_line, Some(0));
        let hover_value = responses
            .get(3)
            .and_then(|message| message.get("result"))
            .and_then(Json::as_obj)
            .and_then(|hover| hover.get("contents"))
            .and_then(Json::as_obj)
            .and_then(|contents| contents.get("value"))
            .and_then(Json::as_str);
        assert!(hover_value.is_some_and(|value| value.contains("int")));
        remove_temp(path);
    }

    #[test]
    fn formats_open_document_with_one_full_text_edit() {
        let text = "x   =  5";
        let (path, uri) = temp_file(text);
        let messages = [
            request(1, "initialize", initialize_params()),
            did_open(&uri, text),
            request(2, "textDocument/formatting", formatting_params(&uri)),
            request(3, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        let text_edit = must(
            responses
                .get(2)
                .and_then(|message| message.get("result"))
                .and_then(Json::as_arr)
                .and_then(|edits| edits.first()),
        );
        let new_text = text_edit.get("newText").and_then(Json::as_str);
        let expected = crate::driver::format_file_text(text);
        assert_eq!(new_text, Some(expected.as_str()));
        remove_temp(path);
    }

    #[test]
    fn initialize_advertises_hover_and_shutdown_exits_successfully() {
        let messages = [
            request(1, "initialize", initialize_params()),
            notification("initialized", Json::Null),
            request(2, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        let initialize = &responses[0];
        let hover = must(
            initialize
                .get("result")
                .and_then(Json::as_obj)
                .and_then(|result| result.get("capabilities"))
                .and_then(Json::as_obj)
                .and_then(|capabilities| capabilities.get("hoverProvider")),
        );
        assert_eq!(hover, &Json::Bool(true));
    }

    #[test]
    fn rejects_non_json_rpc_2_requests() {
        let invalid = Json::obj(vec![
            ("jsonrpc", Json::Str("1.0".to_owned())),
            ("id", Json::Int(1)),
            ("method", Json::Str("shutdown".to_owned())),
        ]);
        let messages = [
            invalid,
            request(2, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        assert_eq!(
            responses[0]
                .get("error")
                .and_then(Json::as_obj)
                .and_then(|error| error.get("code")),
            Some(&Json::Int(-32600))
        );
        assert_eq!(responses[1].get("result"), Some(&Json::Null));
    }

    #[test]
    fn rejects_unsupported_position_encoding() {
        let params = Json::obj(vec![(
            "capabilities",
            Json::obj(vec![(
                "general",
                Json::obj(vec![(
                    "positionEncodings",
                    Json::Arr(vec![Json::Str("utf-8".to_owned())]),
                )]),
            )]),
        )]);
        let messages = [
            request(1, "initialize", params),
            request(2, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        assert_eq!(
            responses[0]
                .get("error")
                .and_then(Json::as_obj)
                .and_then(|error| error.get("code")),
            Some(&Json::Int(-32602))
        );
    }

    #[test]
    fn rejects_malformed_feature_request_parameters() {
        let messages = [
            request(1, "textDocument/hover", Json::Null),
            request(2, "textDocument/formatting", Json::obj(vec![
                (
                    "textDocument",
                    Json::obj(vec![("uri", Json::Str("file:///tmp/open.ybm".to_owned()))]),
                ),
            ])),
            request(3, "shutdown", Json::Null),
            notification("exit", Json::Null),
        ];
        let (exit, responses) = run_messages(&messages);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        for response in responses.iter().take(2) {
            assert_eq!(
                response
                    .get("error")
                    .and_then(Json::as_obj)
                    .and_then(|error| error.get("code")),
                Some(&Json::Int(-32602))
            );
        }
    }

    #[test]
    fn malformed_request_gets_parse_error_and_server_continues() {
        let mut input = Vec::new();
        match write_message(&mut input, "{") {
            Ok(()) => {}
            Err(error) => panic!("expected in-memory write: {error}"),
        }
        let shutdown = request(1, "shutdown", Json::Null).to_string();
        match write_message(&mut input, &shutdown) {
            Ok(()) => {}
            Err(error) => panic!("expected in-memory write: {error}"),
        }
        match write_message(
            &mut input,
            &Json::obj(vec![("method", Json::Str("exit".to_owned()))]).to_string(),
        ) {
            Ok(()) => {}
            Err(error) => panic!("expected in-memory write: {error}"),
        }
        let mut output = Vec::new();
        let exit = run_server(Cursor::new(input), &mut output);
        assert_eq!(exit, std::process::ExitCode::SUCCESS);
        assert!(!output.is_empty());
    }
}
