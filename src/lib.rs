//! Yabumi interpreter library shared by the `ybm` and compatibility binaries.

#![expect(
    dead_code,
    reason = "symmetric helpers and extension points are intentionally retained"
)]
#![expect(
    unused_imports,
    reason = "module re-exports are retained for internal consumers"
)]
#![expect(
    clippy::needless_pass_by_value,
    reason = "owned values keep evaluator call signatures uniform and cross thread boundaries"
)]
#![expect(
    clippy::doc_markdown,
    reason = "documentation uses specification IDs and filenames as prose"
)]

mod ast;
mod cli;
mod concurrency;
mod diagnostics;
mod doctest;
mod driver;
mod effects;
mod eval;
mod fmt;
mod lexer;
mod lint;
mod lsp;
mod module_resolve;
mod parser;
mod stdlib;
mod types;

use std::process::ExitCode;

#[must_use]
pub fn main_entry() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let subcommand = match cli::args::parse_args(&argv) {
        Ok(subcommand) => subcommand,
        Err(cli::CliError::Usage(message)) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || crate::driver::run_pipeline(&subcommand));

    match handle {
        Ok(handle) => handle.join().unwrap_or(ExitCode::FAILURE),
        Err(_) => ExitCode::FAILURE,
    }
}
