//! http namespace (a wrapper around `ureq`, STDLIB.md §6, ARCHITECTURE.md §2.1). effect: `net`.
//!
//! `Response`/`HttpOptions` are normal Yabumi-side structs (STDLIB.md) with no dedicated Rust
//! type -- at runtime they're constructed/destructured as `Value::Struct(Arc<StructInstance>)`
//! based on the `StructDecl` that `prelude.rs` pre-registers.
//!
//! By default `ureq` treats a 4xx/5xx status as `Err(ureq::Error::StatusCode(_))`, but
//! STDLIB.md's `Result[Response, Error]` should always be `Ok(Response)` whenever an HTTP-level
//! response actually came back (leaving the caller to check the `status` field) -- since
//! `samples/ok/11-2_http` requires that `http.get(.../status/404)` returns
//! `Ok(Response{status: 404, ..})` -- so every request sets `http_status_as_error(false)`, and
//! `Err` is reserved for transport-layer failures only, such as connection failures or timeouts.

use crate::eval::value::{MapKey, StructInstance, Value};
use crate::stdlib::{err_value, error_value, ok_value};
use indexmap::IndexMap;
use std::sync::Arc;
use std::time::Duration;

/// The fixed internal timeout used by the simple forms `get`/`post`/`put`/`delete` (STDLIB.md
/// §6: "a fixed internal timeout, no extra headers").
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

fn build_agent(timeout_ms: u64) -> ureq::Agent {
    let timeout = Some(Duration::from_millis(timeout_ms));
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(timeout)
        .timeout_per_call(timeout)
        .timeout_connect(timeout)
        .timeout_recv_response(timeout)
        .timeout_recv_body(timeout)
        .build();
    ureq::Agent::new_with_config(config)
}

fn net_error(message: impl std::fmt::Display) -> Value {
    error_value("net", message.to_string())
}

/// Converts a `ureq` response into `Response{status, headers, body}` (the field order from
/// STDLIB.md §6).
fn response_to_value(mut resp: ureq::http::Response<ureq::Body>) -> Value {
    let status = i64::from(resp.status().as_u16());

    let mut headers = IndexMap::new();
    for (name, value) in resp.headers() {
        let value_str = String::from_utf8_lossy(value.as_bytes()).into_owned();
        headers.insert(
            MapKey::Str(Arc::from(name.as_str())),
            Value::Str(Arc::from(value_str.as_str())),
        );
    }

    let body = match resp.body_mut().read_to_string() {
        Ok(b) => b,
        Err(e) => return err_value(net_error(e)),
    };

    ok_value(Value::Struct(Arc::new(StructInstance {
        type_name: Arc::from("Response"),
        fields: vec![
            Value::Int(status),
            Value::Dict(Arc::new(headers)),
            Value::Str(Arc::from(body.as_str())),
        ],
    })))
}

fn run_request(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Value {
    match result {
        Ok(resp) => response_to_value(resp),
        Err(e) => err_value(net_error(e)),
    }
}

/// `get(url: str): Result[Response, Error] uses {net}`. The simple form (fixed internal timeout,
/// no extra headers).
#[must_use]
pub fn get(url: &str) -> Value {
    let agent = build_agent(DEFAULT_TIMEOUT_MS);
    run_request(agent.get(url).call())
}

/// `post(url: str, body: str): Result[Response, Error] uses {net}`.
#[must_use]
pub fn post(url: &str, body: &str) -> Value {
    let agent = build_agent(DEFAULT_TIMEOUT_MS);
    run_request(agent.post(url).send(body))
}

/// `put(url: str, body: str): Result[Response, Error] uses {net}`.
#[must_use]
pub fn put(url: &str, body: &str) -> Value {
    let agent = build_agent(DEFAULT_TIMEOUT_MS);
    run_request(agent.put(url).send(body))
}

/// `delete(url: str): Result[Response, Error] uses {net}`.
#[must_use]
pub fn delete(url: &str) -> Value {
    let agent = build_agent(DEFAULT_TIMEOUT_MS);
    run_request(agent.delete(url).call())
}

/// `request(method: str, url: str, opts: HttpOptions): Result[Response, Error] uses {net}`. The
/// fully-controlled version for specifying headers/timeout (D-STDPOL-04: represented with a
/// struct rather than default arguments). Only builds requests without a body (GET-equivalent)
/// -- STDLIB.md's `request` doesn't take a body as a separate argument (its purpose is only
/// headers/timeout control, see the comment in §6).
#[must_use]
pub fn request(method: &str, url: &str, opts: &Value) -> Value {
    let Value::Struct(opts) = opts else {
        unreachable!("type-checked already, so request's third argument is always HttpOptions")
    };
    let Value::Dict(headers) = &opts.fields[0] else {
        unreachable!("type-checked already, so HttpOptions.headers is always dict[str,str]")
    };
    let Value::Int(timeout_ms) = opts.fields[1] else {
        unreachable!("type-checked already, so HttpOptions.timeout_ms is always int")
    };

    let timeout_ms = u64::try_from(timeout_ms).unwrap_or(DEFAULT_TIMEOUT_MS);
    let agent = build_agent(timeout_ms);

    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    for (k, v) in headers.iter() {
        let MapKey::Str(key) = k else {
            unreachable!("type-checked already, so HttpOptions.headers keys are always str")
        };
        let Value::Str(value) = v else {
            unreachable!("type-checked already, so HttpOptions.headers values are always str")
        };
        builder = builder.header(key.as_ref(), value.as_ref());
    }

    let built = builder.body(());
    let req = match built {
        Ok(req) => req,
        Err(e) => return err_value(net_error(e)),
    };
    let timeout = Some(Duration::from_millis(timeout_ms));
    let request = agent
        .configure_request(req)
        .timeout_global(timeout)
        .timeout_per_call(timeout)
        .timeout_recv_response(timeout)
        .timeout_recv_body(timeout)
        .build();
    let result = agent.run(request);
    run_request(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_base() -> Option<String> {
        std::env::var("YABUMI_TEST_HTTP_BASE").ok()
    }

    fn ok_fields(v: &Value) -> &[Value] {
        let Value::Enum(inst) = v else {
            panic!("expected Result[Response, Error]")
        };
        assert_eq!(inst.variant_name.as_ref(), "Ok", "was Err: {inst:?}");
        let Value::Struct(resp) = &inst.fields[0] else {
            panic!("expected Response")
        };
        &resp.fields
    }

    /// This unit test only runs when `YABUMI_TEST_HTTP_BASE` is set (so that the mock-server
    /// contract table in SAMPLES_PLAN.md §1.4.1 can also be verified by a plain `cargo test`,
    /// against a server the test side spins up itself, in addition to the one
    /// `tests/support/http_mock.rs` already implements). Since `tests/support` is a module of
    /// the integration-test-only crate and can't be referenced directly from `src/`, this sets
    /// up a minimal, throwaway mock here using `std::net::TcpListener`.
    fn with_mock_server<R>(f: impl FnOnce(&str) -> R) -> R {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => panic!("failed to bind the test mock server: {e}"),
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(e) => panic!("failed to get local_addr for the test mock server: {e}"),
        };
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_ascii_uppercase();
                let path = parts.next().unwrap_or("").to_string();
                let mut headers = std::collections::HashMap::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = trimmed.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let content_length: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0_u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body);
                }
                let (status, content_type, resp_body): (u16, &str, Vec<u8>) =
                    match (method.as_str(), path.as_str()) {
                        ("GET", "/text/hello") => (200, "text/plain", b"hello".to_vec()),
                        // Referenced by `samples/ok/11-2_http/entry_main.ybm` (the
                        // SAMPLES_PLAN.md §1.4.1 contract table, the same fixed response as the
                        // route of the same name in `tests/support/http_mock.rs`), so this
                        // in-process minimal mock provides the same content too.
                        ("GET", "/json/user") => (
                            200,
                            "application/json",
                            br#"{"name":"alice","age":30}"#.to_vec(),
                        ),
                        // The sample only asserts on the response content and doesn't verify the
                        // delay duration, so a fixed value (50ms) is used, small enough not to
                        // slow the whole test suite down excessively, matching the route of the
                        // same name in `tests/support/http_mock.rs`.
                        ("GET", "/slow") => {
                            std::thread::sleep(Duration::from_millis(1_500));
                            (200, "text/plain", b"slow-ok".to_vec())
                        }
                        // "/status/404" deliberately has no explicit arm -- it gets the same
                        // response as the `_` fallback below (404 not found), unifying "an
                        // unknown path is 404" into one rule, which also avoids
                        // match_same_arms.
                        ("POST", "/echo") => (200, "text/plain", body.clone()),
                        ("PUT", "/put/target") => (200, "text/plain", b"put-ok".to_vec()),
                        ("DELETE", "/delete/target") => (200, "text/plain", b"deleted".to_vec()),
                        ("GET", "/headers/echo") => {
                            let x = headers.get("x-test").cloned().unwrap_or_default();
                            (200, "text/plain", x.into_bytes())
                        }
                        _ => (404, "text/plain", b"not found".to_vec()),
                    };
                let stream = reader.get_mut();
                let header = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp_body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&resp_body);
                let _ = stream.flush();
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        f(&base)
    }

    #[test]
    fn get_text_hello_returns_status_200_and_body() {
        with_mock_server(|base| {
            let result = get(&format!("{base}/text/hello"));
            let fields = ok_fields(&result);
            assert_eq!(fields[0], Value::Int(200));
            assert_eq!(fields[2], Value::Str(Arc::from("hello")));
        });
    }

    #[test]
    fn get_404_is_still_ok_with_status_404() {
        with_mock_server(|base| {
            let result = get(&format!("{base}/status/404"));
            let fields = ok_fields(&result);
            assert_eq!(fields[0], Value::Int(404));
        });
    }

    #[test]
    fn post_echoes_request_body() {
        with_mock_server(|base| {
            let result = post(&format!("{base}/echo"), "echo-payload");
            let fields = ok_fields(&result);
            assert_eq!(fields[0], Value::Int(200));
            assert_eq!(fields[2], Value::Str(Arc::from("echo-payload")));
        });
    }

    #[test]
    fn put_and_delete_hit_their_fixed_endpoints() {
        with_mock_server(|base| {
            let put_result = put(&format!("{base}/put/target"), "ignored");
            assert_eq!(ok_fields(&put_result)[2], Value::Str(Arc::from("put-ok")));

            let delete_result = delete(&format!("{base}/delete/target"));
            assert_eq!(
                ok_fields(&delete_result)[2],
                Value::Str(Arc::from("deleted"))
            );
        });
    }

    #[test]
    fn generic_request_honors_method_headers_and_timeout() {
        with_mock_server(|base| {
            let mut headers = IndexMap::new();
            headers.insert(MapKey::Str(Arc::from("X-Test")), Value::Str(Arc::from("v")));
            let options = Value::Struct(Arc::new(StructInstance {
                type_name: Arc::from("HttpOptions"),
                fields: vec![Value::Dict(Arc::new(headers)), Value::Int(3000)],
            }));
            let result = request("GET", &format!("{base}/headers/echo"), &options);
            let fields = ok_fields(&result);
            assert_eq!(fields[0], Value::Int(200));
            assert_eq!(fields[2], Value::Str(Arc::from("v")));
            let Value::Dict(response_headers) = &fields[1] else {
                panic!("Response.headers must be a dictionary")
            };
            assert!(response_headers.iter().any(|(key, value)| {
                matches!(key, MapKey::Str(name) if name.eq_ignore_ascii_case("content-type"))
                    && value == &Value::Str(Arc::from("text/plain"))
            }));

            let deleted = request("DELETE", &format!("{base}/delete/target"), &options);
            assert_eq!(ok_fields(&deleted)[2], Value::Str(Arc::from("deleted")));

            let timeout_options = Value::Struct(Arc::new(StructInstance {
                type_name: Arc::from("HttpOptions"),
                fields: vec![Value::Dict(Arc::new(IndexMap::new())), Value::Int(100)],
            }));
            let timed_out = request("GET", &format!("{base}/slow"), &timeout_options);
            let Value::Enum(result) = timed_out else {
                panic!("request must return Result")
            };
            assert_eq!(result.variant_name.as_ref(), "Err");
        });
    }

    #[test]
    fn get_connection_refused_is_err_with_net_kind() {
        // A fixed port expected to have nothing listening on 127.0.0.1 (connecting to the local
        // loopback fails fast since there's no DNS resolution involved).
        let result = get("http://127.0.0.1:1");
        let Value::Enum(inst) = &result else {
            panic!("expected Result")
        };
        assert_eq!(inst.variant_name.as_ref(), "Err");
        let Value::Struct(err) = &inst.fields[0] else {
            panic!("expected Error")
        };
        assert_eq!(err.fields[0], Value::Str(Arc::from("net")));
    }

    /// In an execution environment where the mock server's real base URL was passed via
    /// `requires_env` (SAMPLES_PLAN.md §1.3), this also does a pass-through connectivity check
    /// against it (redundant with the self-hosted mock above).
    #[test]
    fn optional_real_mock_base_env_smoke_test() {
        let Some(base) = http_base() else {
            return; // Treated as a skip if the environment variable isn't set (SAMPLES_PLAN.md §1.3).
        };
        let result = get(&format!("{base}/text/hello"));
        let fields = ok_fields(&result);
        assert_eq!(fields[0], Value::Int(200));
    }

    /// Verifies SPEC §11.2 / STDLIB.md §6 through the full pipeline
    /// (`samples/ok/11-2_http/entry_main.ybm`, the SAMPLES_PLAN.md §1.4.1 contract table).
    #[test]
    fn sample_http_runs_end_to_end() {
        with_mock_server(|base| {
            // SAFETY: this is the only function in this process that writes this key
            // (`optional_real_mock_base_env_smoke_test` only reads it, and even if it reads
            // this temporary server's base URL, a GET to /text/hello just returns 200 with no
            // real harm).
            unsafe {
                std::env::set_var("YABUMI_TEST_HTTP_BASE", base);
            }
            let result = crate::stdlib::builtins::test_pipeline::run_ok_sample("11-2_http");
            assert!(
                result.is_ok(),
                "sample should run without Abort: {result:?}"
            );
        });
    }
}
