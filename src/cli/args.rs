//! Hand-written argv parsing (3 subcommands + `--check`, position-independent, ARCHITECTURE.md §2.1).
//! Does not use `clap` (only 3 subcommands and a single `--check` flag, ARCHITECTURE.md §1.6).

use std::path::PathBuf;

#[derive(Debug)]
pub enum Subcommand {
    /// `ybm <file>`.
    Run { file: PathBuf },
    /// `ybm check <file> [--check]`. D-CLI-02: `--check` means the same regardless of flag position.
    Check { file: PathBuf, check_only: bool },
    /// `ybm test <file>`.
    Test { file: PathBuf },
}

#[derive(Debug)]
pub enum CliError {
    /// Usage error (unknown subcommand, missing file argument, etc.).
    Usage(String),
}

/// Builds a `Subcommand` from a string sequence equivalent to `std::env::args()` (excluding the
/// leading executable name).
///
/// - If `argv[0]` is `"check"`/`"test"`, treat it as that subcommand and read `--check`
///   (check only, D-CLI-02: either position is allowed) and the file path (the first
///   non-`--check` token) from the rest.
/// - Otherwise (`argv[0]` is neither `"check"` nor `"test"`), treat it as `ybm <file>` (Run),
///   using `argv[0]` as the file path.
///
/// Extra positional arguments following `Run`/`Test`, or following `Check`'s file path, are
/// left as-is in the real process argv as arguments to the script itself (SPEC §11.2
/// `env.args()`) — `env.args()` (`stdlib/envns.rs`) reads the raw `std::env::args()` directly
/// rather than going through this function's result (`Subcommand`), so this function has no
/// need to collect or retain them and simply ignores them (judgment call made in this file).
pub fn parse_args(argv: &[String]) -> Result<Subcommand, CliError> {
    const USAGE: &str = "usage: ybm <file> | ybm check <file> [--check] | ybm test <file>";

    let Some(first) = argv.first() else {
        return Err(CliError::Usage(USAGE.to_owned()));
    };

    match first.as_str() {
        "check" => {
            let mut check_only = false;
            let mut file: Option<PathBuf> = None;
            for arg in &argv[1..] {
                if arg == "--check" {
                    check_only = true;
                } else if file.is_none() {
                    file = Some(PathBuf::from(arg));
                }
                // Any other extra positional arguments are ignored as arguments to the script
                // itself (see note above).
            }
            let file = file.ok_or_else(|| {
                CliError::Usage("ybm check <file> [--check]: no file path was specified".to_owned())
            })?;
            Ok(Subcommand::Check { file, check_only })
        }
        "test" => {
            let file = argv.get(1).map(PathBuf::from).ok_or_else(|| {
                CliError::Usage("ybm test <file>: no file path was specified".to_owned())
            })?;
            Ok(Subcommand::Test { file })
        }
        _ => Ok(Subcommand::Run {
            file: PathBuf::from(first),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_str(p: &std::path::Path) -> &str {
        p.to_str().unwrap_or_default()
    }

    #[test]
    fn bare_file_parses_as_run() {
        let argv = vec!["entry_main.ybm".to_owned()];
        match parse_args(&argv) {
            Ok(Subcommand::Run { file }) => assert_eq!(path_str(&file), "entry_main.ybm"),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn run_ignores_trailing_script_args() {
        let argv = vec![
            "entry_main.ybm".to_owned(),
            "foo".to_owned(),
            "bar".to_owned(),
        ];
        match parse_args(&argv) {
            Ok(Subcommand::Run { file }) => assert_eq!(path_str(&file), "entry_main.ybm"),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn check_without_flag() {
        let argv = vec!["check".to_owned(), "entry_main.ybm".to_owned()];
        match parse_args(&argv) {
            Ok(Subcommand::Check { file, check_only }) => {
                assert_eq!(path_str(&file), "entry_main.ybm");
                assert!(!check_only);
            }
            other => panic!("expected Check, got {other:?}"),
        }
    }

    #[test]
    fn check_flag_after_file() {
        let argv = vec![
            "check".to_owned(),
            "entry_main.ybm".to_owned(),
            "--check".to_owned(),
        ];
        match parse_args(&argv) {
            Ok(Subcommand::Check { file, check_only }) => {
                assert_eq!(path_str(&file), "entry_main.ybm");
                assert!(check_only);
            }
            other => panic!("expected Check, got {other:?}"),
        }
    }

    #[test]
    fn check_flag_before_file() {
        let argv = vec![
            "check".to_owned(),
            "--check".to_owned(),
            "entry_main.ybm".to_owned(),
        ];
        match parse_args(&argv) {
            Ok(Subcommand::Check { file, check_only }) => {
                assert_eq!(path_str(&file), "entry_main.ybm");
                assert!(check_only);
            }
            other => panic!("expected Check, got {other:?}"),
        }
    }

    #[test]
    fn test_subcommand_parses_file() {
        let argv = vec!["test".to_owned(), "entry_main.ybm".to_owned()];
        match parse_args(&argv) {
            Ok(Subcommand::Test { file }) => assert_eq!(path_str(&file), "entry_main.ybm"),
            other => panic!("expected Test, got {other:?}"),
        }
    }

    #[test]
    fn empty_argv_is_usage_error() {
        let argv: Vec<String> = Vec::new();
        assert!(matches!(parse_args(&argv), Err(CliError::Usage(_))));
    }

    #[test]
    fn check_without_file_is_usage_error() {
        let argv = vec!["check".to_owned(), "--check".to_owned()];
        assert!(matches!(parse_args(&argv), Err(CliError::Usage(_))));
    }

    #[test]
    fn test_without_file_is_usage_error() {
        let argv = vec!["test".to_owned()];
        assert!(matches!(parse_args(&argv), Err(CliError::Usage(_))));
    }
}
