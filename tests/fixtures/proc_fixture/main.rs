//! The fixture binary pointed to by `YABUMI_TEST_PROC_BIN` (contract table in section 1.4.2 of
//! `SAMPLES_PLAN.md`, section 6.2 of `ARCHITECTURE.md`). A standalone binary with no dependency
//! whatsoever on the `ybm` proper (the crate under `src/`) — the `proc.run` tests spawn it as a
//! child process.
//!
//! Contract:
//! - `["echo", "<text>"]`  -> `<text>` + newline to stdout, exit 0, stderr empty
//! - `["fail", "<code>"]`  -> a fixed message to stderr, exit code `<code>`, stdout empty
//! - `["cat"]`             -> reads stdin to EOF, writes the same content to stdout as-is, exit 0

use std::io::{Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("echo") => {
            let text = args.get(1).map_or("", String::as_str);
            println!("{text}");
            ExitCode::SUCCESS
        }
        Some("fail") => {
            let code_arg = args.get(1).map_or("1", String::as_str);
            eprintln!("failed with code {code_arg}");
            let code: u8 = code_arg.parse().unwrap_or(1);
            ExitCode::from(code)
        }
        Some("cat") => {
            let mut buf = String::new();
            match std::io::stdin().read_to_string(&mut buf) {
                Ok(_) => {
                    if std::io::stdout().write_all(buf.as_bytes()).is_err() {
                        return ExitCode::FAILURE;
                    }
                    ExitCode::SUCCESS
                }
                Err(_) => ExitCode::FAILURE,
            }
        }
        _ => {
            eprintln!("usage: proc_fixture (echo <text> | fail <code> | cat)");
            ExitCode::FAILURE
        }
    }
}
