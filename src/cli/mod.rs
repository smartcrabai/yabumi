//! Subcommand enumeration (Run/Check/Test/Lsp), bridging to the driver (ARCHITECTURE.md §2.1).

pub mod args;

pub use args::{CliError, Subcommand};
