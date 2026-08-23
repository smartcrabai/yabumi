//! Mock HTTP server for `YABUMI_TEST_HTTP_BASE` (contract table in `SAMPLES_PLAN.md` §1.4.1,
//! ARCHITECTURE.md §6.2). Implemented with `std::net` only, adding no extra dev-dependency.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

/// Listens on `127.0.0.1:0` (letting the OS pick a free port) and runs an HTTP/1.1 responder
/// satisfying the 8 endpoints from `SAMPLES_PLAN.md` §1.4.1 on a dedicated thread. Returns
/// `http://127.0.0.1:<port>`. Started once for the whole test run and shared across cases.
///
/// A `bind`/`local_addr` failure means "the test environment can't even do TCP loopback" — an
/// unrecoverable startup-time abnormality, which `unreachable!()` would misrepresent. The test
/// crate is also subject to the `unwrap_used`/`expect_used` deny lint (ARCHITECTURE.md §6.3),
/// so a function-level `#[expect(...)]` is used instead (decision R3, §8).
#[expect(
    clippy::expect_used,
    reason = "A bind/addr failure on the test loopback socket is an unrecoverable environment \
              abnormality, and unreachable!() would misrepresent it. Let the test harness \
              terminate immediately on startup."
)]
#[must_use]
pub fn spawn_mock_http_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || handle_one(stream));
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A minimal HTTP/1.1 responder satisfying the 8-endpoint contract from `SAMPLES_PLAN.md`
/// §1.4.1. Reads the request line plus headers up to `\r\n\r\n`, then reads the body for the
/// length given by `Content-Length` if present (chunked transfer etc. is not supported, as it
/// is not in the contract table). I/O errors while handling a connection can be ignored as the
/// discarding of a single test connection (`run_case` ultimately verifies the result on the
/// HTTP client side, so this function itself returns `()`).
fn handle_one(stream: std::net::TcpStream) {
    let _ = respond(stream);
}

fn respond(stream: std::net::TcpStream) -> io::Result<()> {
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(()); // Empty connection (closed without sending anything).
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let path = parts.next().unwrap_or("").to_string();

    let headers = read_headers(&mut reader)?;
    let content_length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }

    let (status, content_type, response_body) = route(&method, &path, &headers, &body);
    write_response(reader.get_mut(), status, &content_type, &response_body)
}

fn read_headers(
    reader: &mut BufReader<std::net::TcpStream>,
) -> io::Result<HashMap<String, String>> {
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break; // Client closed the connection mid-headers.
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // Blank line = end of headers.
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Ok(headers)
}

/// The contract table from `SAMPLES_PLAN.md` §1.4.1 itself. Unknown method/path combinations
/// fall back to `404` (a combination not in the contract table, which no sample is expected to
/// reach).
fn route(
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> (u16, String, Vec<u8>) {
    match (method, path) {
        ("GET", "/text/hello") => (200, "text/plain".to_string(), b"hello".to_vec()),
        ("GET", "/json/user") => (
            200,
            "application/json".to_string(),
            br#"{"name":"alice","age":30}"#.to_vec(),
        ),
        ("GET", "/slow") => {
            // Contract: "insert a harness-implementation-defined fixed delay before returning
            // the response." Since the sample side only asserts on content and does not verify
            // the delay duration, a fixed value is chosen that does not slow the whole test
            // suite down excessively.
            std::thread::sleep(Duration::from_millis(1_500));
            (200, "text/plain".to_string(), b"slow-ok".to_vec())
        }
        ("POST", "/echo") => {
            let content_type = headers
                .get("content-type")
                .cloned()
                .unwrap_or_else(|| "text/plain".to_string());
            (200, content_type, body.to_vec())
        }
        ("PUT", "/put/target") => (200, "text/plain".to_string(), b"put-ok".to_vec()),
        ("DELETE", "/delete/target") => (200, "text/plain".to_string(), b"deleted".to_vec()),
        ("GET", "/headers/echo") => {
            let x_test = headers.get("x-test").cloned().unwrap_or_default();
            (200, "text/plain".to_string(), x_test.into_bytes())
        }
        // Combinations not in the contract table (§1.4.1), and `/status/404` itself (a fixed
        // endpoint that returns 404, not 200), are all folded into this fallback.
        _ => (404, "text/plain".to_string(), b"not found".to_vec()),
    }
}

fn write_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let status_text = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Unknown",
    };
    let header = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    /// Sends one request and verifies the expected status, Content-Type, and body come back.
    fn request(base: &str, raw_request: &str) -> (u16, String, Vec<u8>) {
        let addr = base.strip_prefix("http://").unwrap_or(base).to_string();
        let mut stream = TcpStream::connect(&addr).unwrap_or_else(|e| {
            panic!("failed to connect to mock server ({addr}): {e}");
        });
        stream
            .write_all(raw_request.as_bytes())
            .unwrap_or_else(|e| panic!("failed to send request: {e}"));
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .unwrap_or_else(|e| panic!("failed to read response: {e}"));
        parse_response(&response)
    }

    fn parse_response(raw: &[u8]) -> (u16, String, Vec<u8>) {
        let text = String::from_utf8_lossy(raw);
        let Some(header_end) = text.find("\r\n\r\n") else {
            panic!("response has no blank line (end of headers): {text:?}");
        };
        let header_part = &text[..header_end];
        let mut lines = header_part.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let mut content_type = String::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':')
                && k.trim().eq_ignore_ascii_case("content-type")
            {
                content_type = v.trim().to_string();
            }
        }
        let body_start = header_end + 4;
        let body = raw.get(body_start..).unwrap_or_default().to_vec();
        (status, content_type, body)
    }

    #[test]
    fn text_hello_endpoint() {
        let base = spawn_mock_http_server();
        let (status, content_type, body) =
            request(&base, "GET /text/hello HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(content_type, "text/plain");
        assert_eq!(body, b"hello");
    }

    #[test]
    fn json_user_endpoint() {
        let base = spawn_mock_http_server();
        let (status, content_type, body) =
            request(&base, "GET /json/user HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert_eq!(body, br#"{"name":"alice","age":30}"#);
    }

    #[test]
    fn status_404_endpoint() {
        let base = spawn_mock_http_server();
        let (status, _content_type, body) =
            request(&base, "GET /status/404 HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 404);
        assert_eq!(body, b"not found");
    }

    #[test]
    fn slow_endpoint_still_returns_expected_body() {
        let base = spawn_mock_http_server();
        let (status, _content_type, body) = request(&base, "GET /slow HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(body, b"slow-ok");
    }

    #[test]
    fn echo_endpoint_reflects_body_and_content_type() {
        let base = spawn_mock_http_server();
        let payload = "hello=world";
        let raw = format!(
            "POST /echo HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{payload}",
            payload.len()
        );
        let (status, content_type, body) = request(&base, &raw);
        assert_eq!(status, 200);
        assert_eq!(content_type, "application/x-www-form-urlencoded");
        assert_eq!(body, payload.as_bytes());
    }

    #[test]
    fn put_target_endpoint() {
        let base = spawn_mock_http_server();
        let (status, _content_type, body) =
            request(&base, "PUT /put/target HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(body, b"put-ok");
    }

    #[test]
    fn delete_target_endpoint() {
        let base = spawn_mock_http_server();
        let (status, _content_type, body) =
            request(&base, "DELETE /delete/target HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(body, b"deleted");
    }

    #[test]
    fn headers_echo_reflects_x_test_header() {
        let base = spawn_mock_http_server();
        let (status, _content_type, body) = request(
            &base,
            "GET /headers/echo HTTP/1.1\r\nHost: x\r\nX-Test: abc123\r\n\r\n",
        );
        assert_eq!(status, 200);
        assert_eq!(body, b"abc123");
    }

    #[test]
    fn headers_echo_empty_when_header_absent() {
        let base = spawn_mock_http_server();
        let (status, _content_type, body) =
            request(&base, "GET /headers/echo HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 200);
        assert_eq!(body, b"");
    }
}
