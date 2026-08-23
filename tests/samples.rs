//! The single entry point of the `samples/` acceptance test harness (ARCHITECTURE.md §6.1).
//!
//! Verifies everything under `samples/` by spawning `ybm` itself as a child subprocess (Unit
//! 17 complete). Because the process is isolated, even the case where a `par` worker thread
//! panic calls `std::process::exit(1)`
//! (`samples/err/runtime/par_panic_aborts_immediately`) can be observed as a single case
//! failure without taking down the test binary itself — an advantage this harness has that the
//! in-process version (`run_all_samples_in_process` in `src/driver.rs`) lacks, which is why
//! this one is the primary acceptance test included in ordinary `cargo test`.

mod support;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use support::toml_lite::{DocBlockExpectation, ExpectedCase, StdioExpectation, StdioMode};

/// The result of running one case. A case whose `requires_env` cannot be satisfied is treated
/// as a skip rather than a failure (`SAMPLES_PLAN.md` §1.3: "the harness may treat a case as
/// skipped if one of its listed environment variables is unset at run time").
enum CaseOutcome {
    Passed,
    Skipped(String),
}

#[test]
fn run_all_samples() {
    let ybm_bin = PathBuf::from(env!("CARGO_BIN_EXE_ybm"));
    let proc_fixture_bin = PathBuf::from(env!("CARGO_BIN_EXE_proc_fixture"));
    let http_base = support::http_mock::spawn_mock_http_server();

    let samples_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    let mut failures = Vec::new();
    let mut skipped = Vec::new();
    let mut passed = 0usize;

    for dir in support::discover_sample_dirs(samples_root.to_str().unwrap_or("samples")) {
        let expected_path = dir.join("expected.toml");
        let Ok(text) = fs::read_to_string(&expected_path) else {
            failures.push(format!("{}: failed to read expected.toml", dir.display()));
            continue;
        };
        for case in support::toml_lite::parse_expected_toml(&text) {
            let label = format!("{}/{}", dir.display(), case.id);
            match run_case(&ybm_bin, &proc_fixture_bin, &http_base, &dir, &case) {
                Ok(CaseOutcome::Passed) => passed += 1,
                Ok(CaseOutcome::Skipped(reason)) => skipped.push(format!("{label}: {reason}")),
                Err(msg) => failures.push(format!("{label}: {msg}")),
            }
        }
    }

    eprintln!(
        "samples: {passed} passed, {} skipped, {} failed",
        skipped.len(),
        failures.len()
    );
    for s in &skipped {
        eprintln!("  skipped: {s}");
    }
    assert!(
        failures.is_empty(),
        "{} case(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn cli_usage_errors_exit_nonzero_on_stderr() {
    let ybm_bin = env!("CARGO_BIN_EXE_ybm");
    for args in [Vec::<&str>::new(), vec!["check", "--check"], vec!["test"]] {
        let output = Command::new(ybm_bin)
            .args(&args)
            .output()
            .unwrap_or_else(|error| panic!("failed to run ybm {args:?}: {error}"));
        assert!(
            !output.status.success(),
            "ybm {args:?} unexpectedly succeeded"
        );
        assert!(
            !output.stderr.is_empty(),
            "ybm {args:?} must explain the usage error on stderr"
        );
        assert!(
            output.stdout.is_empty(),
            "ybm {args:?} must not print usage errors on stdout"
        );
    }
}

/// Converts one `expected.toml` case into an actual `ybm` process invocation and verifies it
/// (ARCHITECTURE.md §6.1, `SAMPLES_PLAN.md` §1.3/§1.4).
///
/// `cmd = "check"` rewrites the file in place via fmt, so rather than using `dir` directly, it
/// runs from a copy of the whole thing made in a temp directory (including `_out/` — an fs
/// sample's sandbox is also rewritten on the copy side, leaving `samples/` itself untouched).
fn run_case(
    ybm_bin: &Path,
    proc_fixture_bin: &Path,
    http_base: &str,
    dir: &Path,
    case: &ExpectedCase,
) -> Result<CaseOutcome, String> {
    if let Some(reason) = missing_required_env(case, http_base) {
        return Ok(CaseOutcome::Skipped(reason));
    }

    let work_dir = copy_dir_to_temp(dir)?;
    let work_dir_path = work_dir.path();
    let before_check = if case.cmd == "check_diff" {
        Some(snapshot_ybm_files(work_dir_path)?)
    } else {
        None
    };

    let mut command = build_command(ybm_bin, work_dir_path, case)?;
    command.env("YABUMI_TEST_HTTP_BASE", http_base);
    command.env(
        "YABUMI_TEST_PROC_BIN",
        proc_fixture_bin
            .to_str()
            .ok_or_else(|| "proc_fixture binary path is not UTF-8".to_string())?,
    );

    let stdin_bytes = read_stdin_fixture(work_dir_path, case)?;
    let output = run_with_stdin(&mut command, &stdin_bytes)
        .map_err(|e| format!("failed to spawn process: {e}"))?;

    let actual_exit_code = output.status.code().unwrap_or(-1);
    if actual_exit_code != case.exit_code {
        return Err(format!(
            "exit_code mismatch: expected {}, got {actual_exit_code}",
            case.exit_code
        ));
    }

    let stdout_text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr_text = String::from_utf8_lossy(&output.stderr).into_owned();

    check_stdio("stdout", &case.stdout, &stdout_text)?;
    check_stdio("stderr", &case.stderr, &stderr_text)?;
    if wants_top_level_diagnostics_check(&case.cmd) {
        check_diagnostics(&case.diagnostics, &stderr_text)?;
    }

    if case.cmd == "check_diff" {
        let has_diff_output = stdout_text.contains("--- ")
            && stdout_text.contains("-- before --")
            && stdout_text.contains("-- after --");
        if has_diff_output != case.fmt_diff_expected {
            return Err(format!(
                "formatter diff output mismatch: expected {}, got {has_diff_output}",
                case.fmt_diff_expected
            ));
        }
        let after_check = snapshot_ybm_files(work_dir_path)?;
        if before_check.as_ref() != Some(&after_check) {
            return Err("check --check modified one or more .ybm files".to_owned());
        }
    }

    if case.cmd == "check" && !case.fmt_result_file.is_empty() {
        check_fmt_result(work_dir_path, case)?;
    }

    if case.cmd == "test" && !case.doc_blocks.is_empty() {
        check_doc_blocks(&case.doc_blocks, &stderr_text)?;
    }

    Ok(CaseOutcome::Passed)
}

/// Returns the skip reason if any environment variable listed in `requires_env` is one this
/// harness cannot provide.
///
/// `run_all_samples` always sets up `YABUMI_TEST_HTTP_BASE`/`YABUMI_TEST_PROC_BIN`, so for now
/// this always returns `None` (i.e. always satisfied), but it explicitly checks anyway to guard
/// against an unexpected `requires_env` value in the future.
fn missing_required_env(case: &ExpectedCase, _http_base: &str) -> Option<String> {
    for var in &case.requires_env {
        match var.as_str() {
            "YABUMI_TEST_HTTP_BASE" | "YABUMI_TEST_PROC_BIN" => {}
            other => {
                return Some(format!(
                    "requires_env specifies unknown environment variable '{other}'"
                ));
            }
        }
    }
    None
}

/// Copies the whole contents of `dir` into a temp directory (protecting `samples/` itself from
/// `check`'s in-place rewrite and from `fs` samples' writes under `_out/`, `SAMPLES_PLAN.md`
/// §1.4).
fn copy_dir_to_temp(dir: &Path) -> Result<tempdir_shim::TempDir, String> {
    let work = tempdir_shim::TempDir::new()
        .map_err(|e| format!("failed to create temp directory: {e}"))?;
    copy_dir_recursive(dir, work.path())?;
    Ok(work)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("failed to create {}: {e}", dst.display()))?;
    let entries =
        fs::read_dir(src).map_err(|e| format!("failed to read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to get directory entry: {e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|e| format!("failed to get file type of {}: {e}", src_path.display()))?;
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).map_err(|e| {
                format!(
                    "failed to copy {} -> {}: {e}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn snapshot_ybm_files(root: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    fn collect(root: &Path, dir: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<(), String> {
        let entries = fs::read_dir(dir)
            .map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("failed to get directory entry: {error}"))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
            if file_type.is_dir() {
                collect(root, &path, files)?;
            } else if file_type.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("ybm")
            {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?
                    .to_path_buf();
                let content = fs::read(&path)
                    .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
                files.push((relative, content));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

/// Converts `case.cmd` (`run`/`check`/`check_diff`/`test`) into a `ybm` CLI argument list, per
/// the mapping table in `SAMPLES_PLAN.md` §6.1.
fn build_command(ybm_bin: &Path, work_dir: &Path, case: &ExpectedCase) -> Result<Command, String> {
    let entry_arg = case.entry.clone();

    // D-CLI-02: `--check` may go either before or after entry. `cmd = "check_diff"`'s default
    // form places it after (`ybm check <entry> --check`). Only when `case.args` explicitly
    // spells out `--check` is it invoked in the prefix form (`ybm check --check <entry>`)
    // (`SAMPLES_PLAN.md` §6.1: "use `case.args` as-is for the actual flag position").
    let has_prefix_check_flag = case.args.iter().any(|a| a == "--check");
    let extra_args: Vec<String> = case
        .args
        .iter()
        .filter(|a| a.as_str() != "--check")
        .cloned()
        .collect();

    let args: Vec<String> = match case.cmd.as_str() {
        "run" => {
            let mut a = vec![entry_arg];
            a.extend(extra_args);
            a
        }
        "check" => {
            let mut a = vec!["check".to_string(), entry_arg];
            a.extend(extra_args);
            a
        }
        "check_diff" if has_prefix_check_flag => {
            let mut a = vec!["check".to_string(), "--check".to_string(), entry_arg];
            a.extend(extra_args);
            a
        }
        "check_diff" => {
            let mut a = vec!["check".to_string(), entry_arg, "--check".to_string()];
            a.extend(extra_args);
            a
        }
        "test" => {
            let mut a = vec!["test".to_string(), entry_arg];
            a.extend(extra_args);
            a
        }
        other => return Err(format!("unknown cmd '{other}'")),
    };

    let mut command = Command::new(ybm_bin);
    command.args(&args);
    command.current_dir(work_dir);
    Ok(command)
}

fn read_stdin_fixture(work_dir: &Path, case: &ExpectedCase) -> Result<Vec<u8>, String> {
    if case.stdin_file.is_empty() {
        return Ok(Vec::new());
    }
    let path = work_dir.join(&case.stdin_file);
    fs::read(&path).map_err(|e| format!("failed to read stdin_file {}: {e}", path.display()))
}

fn run_with_stdin(
    command: &mut Command,
    stdin_bytes: &[u8],
) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_bytes);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "ybm sample exceeded 30 seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn check_stdio(label: &str, expected: &StdioExpectation, actual: &str) -> Result<(), String> {
    match expected.mode {
        StdioMode::Exact if actual != expected.value => Err(format!(
            "{label} mismatch (exact): expected {:?}, got {:?}",
            expected.value, actual
        )),
        StdioMode::Contains if !actual.contains(&expected.value) => Err(format!(
            "{label} mismatch (contains): expected substring {:?} not found in {:?}",
            expected.value, actual
        )),
        StdioMode::Exact | StdioMode::Contains => Ok(()),
    }
}

/// Whether `check_diagnostics` should be run against `case.diagnostics`.
///
/// For `cmd = "test"`, individual doc-test blocks' fail reports (`[Exxxx]`, D-DOC-05) show up
/// in stderr (ARCHITECTURE.md §4.3: "each individual fail's `[Exxxx]` diagnostic line goes to
/// stderr just like an ordinary diagnostic"). This is not what `case.diagnostics` is meant for
/// — it is dedicated to the 6-phase static diagnostics only (per §4.3: "if the 6 phases produce
/// even a single diagnostic, the doc tests never run at all", backed up by the fact that every
/// case under `samples/doctest/` has diagnostics = []). A `test` case with a fail-expecting doc
/// block has that `[Exxxx]` show up in stderr while `case.diagnostics` stays empty, so comparing
/// the two here would always mismatch and produce a false positive. Since per-doc-block
/// pass/fail judgment is left to `check_doc_blocks`, this comparison is not performed at all for
/// the `test` command.
fn wants_top_level_diagnostics_check(cmd: &str) -> bool {
    cmd != "test"
}

/// Extracts `[Exxxx]` diagnostic codes from `stderr` in order of appearance and compares them
/// (order included) against `expected` (D-CLI-03: diagnostics are ascending by
/// `file:line:col`, which corresponds exactly to their output order to stderr).
fn check_diagnostics(expected: &[String], stderr_text: &str) -> Result<(), String> {
    let actual = extract_diagnostic_codes(stderr_text);
    if actual != expected {
        return Err(format!(
            "diagnostics mismatch: expected {expected:?}, got {actual:?}"
        ));
    }
    Ok(())
}

fn extract_diagnostic_codes(text: &str) -> Vec<String> {
    let mut codes = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        let is_diagnostic_code_at_i = i + 6 < len
            && chars[i] == '['
            && chars[i + 1] == 'E'
            && chars[i + 2].is_ascii_digit()
            && chars[i + 3].is_ascii_digit()
            && chars[i + 4].is_ascii_digit()
            && chars[i + 5].is_ascii_digit()
            && chars[i + 6] == ']';
        if is_diagnostic_code_at_i {
            let code: String = chars[i + 1..i + 6].iter().collect();
            codes.push(code);
            i += 7;
        } else {
            i += 1;
        }
    }
    codes
}

fn check_fmt_result(work_dir: &Path, case: &ExpectedCase) -> Result<(), String> {
    let entry_path = work_dir.join(&case.entry);
    let expected_path = work_dir.join(&case.fmt_result_file);
    let actual = fs::read(&entry_path)
        .map_err(|e| format!("failed to read {}: {e}", entry_path.display()))?;
    let expected = fs::read(&expected_path)
        .map_err(|e| format!("failed to read {}: {e}", expected_path.display()))?;
    if actual != expected {
        return Err(format!(
            "fmt_result_file mismatch: {} (after formatting) is not byte-identical to {}",
            entry_path.display(),
            expected_path.display()
        ));
    }
    Ok(())
}

/// For each element of `doc_blocks`, verifies that a `[Exxxx]` diagnostic corresponding to
/// `line` is present in stderr (fail expected) or absent (pass expected) (D-DOC-05).
fn check_doc_blocks(expected: &[DocBlockExpectation], stderr_text: &str) -> Result<(), String> {
    for block in expected {
        let line_prefix_matches: Vec<&str> = stderr_text
            .lines()
            .filter(|line| line_number_matches(line, block.line))
            .collect();
        match block.result.as_str() {
            "fail" => {
                let code = block.code.as_deref().unwrap_or("");
                let has_matching_diag = line_prefix_matches
                    .iter()
                    .any(|line| code.is_empty() || line.contains(&format!("[{code}]")));
                if !has_matching_diag {
                    return Err(format!(
                        "doc_blocks: no fail diagnostic [{code}] found on line {} (stderr: {stderr_text:?})",
                        block.line
                    ));
                }
            }
            "pass" => {
                if !line_prefix_matches.is_empty() {
                    return Err(format!(
                        "doc_blocks: line {} expected pass but a diagnostic exists ({line_prefix_matches:?})",
                        block.line
                    ));
                }
            }
            other => return Err(format!("doc_blocks has unknown result '{other}'")),
        }
    }
    Ok(())
}

/// Determines whether a `file:line:col [Exxxx] ...`-format diagnostic line reports the given
/// `line` number.
fn line_number_matches(diagnostic_line: &str, line: u32) -> bool {
    // Extracts just the `line` portion of `file:line:col [...]`. The possibility that the
    // `file` portion itself contains a `:` (e.g. a Windows drive letter) is not considered
    // relevant to this harness's target environments.
    let parts: Vec<&str> = diagnostic_line.split(':').collect();
    parts
        .get(1)
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|n| n == line)
}

/// A minimal `TempDir`-style wrapper dedicated to tests, using only `std` and adding no
/// `tempfile` crate dependency (applies the same policy as `toml_lite`/`http_mock` — "test code
/// avoids extra dev-dependencies", §6.2 — to `run_case` as well). Best-effort deletion happens
/// via `Drop`.
mod tempdir_shim {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new() -> io::Result<Self> {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = std::env::temp_dir().join(format!("yabumi-samples-{pid}-{n}"));
            std::fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(overrides: impl FnOnce(&mut ExpectedCase)) -> ExpectedCase {
        let mut c = ExpectedCase {
            id: "t".to_string(),
            entry: "entry_main.ybm".to_string(),
            cmd: "run".to_string(),
            args: Vec::new(),
            stdin_file: String::new(),
            exit_code: 0,
            diagnostics: Vec::new(),
            fmt_diff_expected: false,
            fmt_result_file: String::new(),
            stdout: StdioExpectation {
                mode: StdioMode::Exact,
                value: String::new(),
            },
            stderr: StdioExpectation {
                mode: StdioMode::Exact,
                value: String::new(),
            },
            doc_blocks: Vec::new(),
            requires_env: Vec::new(),
        };
        overrides(&mut c);
        c
    }

    // -- extract_diagnostic_codes ------------------------------------------------

    #[test]
    fn extract_diagnostic_codes_finds_all_codes_in_order() {
        let text = "a.ybm:3:5 [E1001] first\nb.ybm:9:1 [E4002] second\n";
        assert_eq!(extract_diagnostic_codes(text), vec!["E1001", "E4002"]);
    }

    #[test]
    fn extract_diagnostic_codes_ignores_lowercase_and_wrong_length() {
        // Lowercase 'e', 3 digits, and 5 digits are all ignored, since none matches D-DIAG-02's
        // 4-digit uppercase code format.
        let text = "[e1001] [E100] [E10012] [E1001]";
        assert_eq!(extract_diagnostic_codes(text), vec!["E1001"]);
    }

    #[test]
    fn extract_diagnostic_codes_empty_for_no_brackets() {
        assert_eq!(
            extract_diagnostic_codes("hello\nworld\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn extract_diagnostic_codes_handles_multibyte_text_around_code() {
        // Scanning is char-based, so multibyte characters immediately before/after a code
        // cause no offset drift.
        let text = "file:1:1 [E6001] panic occurred";
        assert_eq!(extract_diagnostic_codes(text), vec!["E6001"]);
    }

    // -- check_diagnostics / wants_top_level_diagnostics_check --------------------

    #[test]
    fn check_diagnostics_ok_when_order_and_codes_match() {
        let stderr = "f:1:1 [E1001] a\nf:2:1 [E1002] b\n";
        assert_eq!(
            check_diagnostics(&["E1001".to_string(), "E1002".to_string()], stderr),
            Ok(())
        );
    }

    #[test]
    fn check_diagnostics_fails_on_order_mismatch() {
        let stderr = "f:1:1 [E1002] a\nf:2:1 [E1001] b\n";
        match check_diagnostics(&["E1001".to_string(), "E1002".to_string()], stderr) {
            Ok(()) => panic!("order mismatch should be an error"),
            Err(err) => assert!(err.contains("diagnostics mismatch")),
        }
    }

    #[test]
    fn wants_top_level_diagnostics_check_is_false_only_for_test_cmd() {
        assert!(!wants_top_level_diagnostics_check("test"));
        assert!(wants_top_level_diagnostics_check("run"));
        assert!(wants_top_level_diagnostics_check("check"));
        assert!(wants_top_level_diagnostics_check("check_diff"));
    }

    /// Regression test: in a case equivalent to
    /// `samples/doctest/failing_assert_and_report_line` (`diagnostics = []`, but the doc
    /// block's fail report `[E6004]` shows up in stderr), confirms that naively applying
    /// `check_diagnostics` would incorrectly report a mismatch — exactly the reason
    /// `wants_top_level_diagnostics_check` makes this case `false` so that `run_case` skips it.
    #[test]
    fn doctest_fail_block_stderr_would_wrongly_fail_naive_diagnostics_check() {
        let stderr = "entry_main.ybm:11:5 [E6004] panic: assertion failed\n";
        let c = case(|c| {
            c.cmd = "test".to_string();
            c.diagnostics = Vec::new();
            c.doc_blocks = vec![DocBlockExpectation {
                line: 11,
                result: "fail".to_string(),
                code: Some("E6004".to_string()),
            }];
        });
        // Naively applying check_diagnostics fails (expected=[] vs actual=["E6004"]).
        assert!(check_diagnostics(&c.diagnostics, stderr).is_err());
        // But since `cmd = "test"`, `run_case` never performs this comparison in the first
        // place.
        assert!(!wants_top_level_diagnostics_check(&c.cmd));
        // The doc_blocks-side verification passes correctly.
        assert_eq!(check_doc_blocks(&c.doc_blocks, stderr), Ok(()));
    }

    // -- check_stdio ---------------------------------------------------------------

    #[test]
    fn check_stdio_exact_match_and_mismatch() {
        let exp = StdioExpectation {
            mode: StdioMode::Exact,
            value: "hello\n".to_string(),
        };
        assert_eq!(check_stdio("stdout", &exp, "hello\n"), Ok(()));
        match check_stdio("stdout", &exp, "hell\n") {
            Ok(()) => panic!("should mismatch"),
            Err(err) => assert!(err.contains("stdout mismatch (exact)")),
        }
    }

    #[test]
    fn check_stdio_contains_match_and_mismatch() {
        let exp = StdioExpectation {
            mode: StdioMode::Contains,
            value: "E6004".to_string(),
        };
        assert_eq!(check_stdio("stderr", &exp, "prefix E6004 suffix"), Ok(()));
        match check_stdio("stderr", &exp, "no code here") {
            Ok(()) => panic!("should mismatch"),
            Err(err) => assert!(err.contains("stderr mismatch (contains)")),
        }
    }

    // -- check_doc_blocks / line_number_matches -------------------------------------

    #[test]
    fn check_doc_blocks_fail_requires_matching_code_on_that_line() {
        let stderr = "e.ybm:11:5 [E6004] panic: assertion failed\n";
        let blocks = vec![DocBlockExpectation {
            line: 11,
            result: "fail".to_string(),
            code: Some("E6004".to_string()),
        }];
        assert_eq!(check_doc_blocks(&blocks, stderr), Ok(()));

        let blocks_wrong_code = vec![DocBlockExpectation {
            line: 11,
            result: "fail".to_string(),
            code: Some("E6005".to_string()),
        }];
        assert!(check_doc_blocks(&blocks_wrong_code, stderr).is_err());
    }

    #[test]
    fn check_doc_blocks_pass_requires_no_diagnostic_on_that_line() {
        let stderr = "e.ybm:11:5 [E6004] panic: assertion failed\n";
        let ok_blocks = vec![DocBlockExpectation {
            line: 19,
            result: "pass".to_string(),
            code: None,
        }];
        assert_eq!(check_doc_blocks(&ok_blocks, stderr), Ok(()));

        let bad_blocks = vec![DocBlockExpectation {
            line: 11,
            result: "pass".to_string(),
            code: None,
        }];
        assert!(check_doc_blocks(&bad_blocks, stderr).is_err());
    }

    #[test]
    fn line_number_matches_extracts_second_colon_field() {
        assert!(line_number_matches("e.ybm:11:5 [E6004] x", 11));
        assert!(!line_number_matches("e.ybm:11:5 [E6004] x", 12));
        assert!(!line_number_matches("no colons here", 1));
    }

    // -- build_command --------------------------------------------------------------

    #[test]
    fn build_command_run_appends_extra_args() {
        let c = case(|c| {
            c.cmd = "run".to_string();
            c.args = vec!["--foo".to_string()];
        });
        let cmd = build_command(Path::new("/bin/ybm"), Path::new("/work"), &c)
            .unwrap_or_else(|e| panic!("build_command failed: {e}"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["entry_main.ybm", "--foo"]);
    }

    #[test]
    fn build_command_check_prepends_check_subcommand() {
        let c = case(|c| c.cmd = "check".to_string());
        let cmd = build_command(Path::new("/bin/ybm"), Path::new("/work"), &c)
            .unwrap_or_else(|e| panic!("build_command failed: {e}"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["check", "entry_main.ybm"]);
    }

    #[test]
    fn build_command_check_diff_defaults_to_suffix_check_flag() {
        let c = case(|c| c.cmd = "check_diff".to_string());
        let cmd = build_command(Path::new("/bin/ybm"), Path::new("/work"), &c)
            .unwrap_or_else(|e| panic!("build_command failed: {e}"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["check", "entry_main.ybm", "--check"]);
    }

    #[test]
    fn build_command_check_diff_honors_explicit_prefix_flag_position() {
        // D-CLI-02: --check may go either before or after file. When case.args spells it out
        // explicitly, it is invoked in the prefix form (SAMPLES_PLAN.md §6.1).
        let c = case(|c| {
            c.cmd = "check_diff".to_string();
            c.args = vec!["--check".to_string()];
        });
        let cmd = build_command(Path::new("/bin/ybm"), Path::new("/work"), &c)
            .unwrap_or_else(|e| panic!("build_command failed: {e}"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["check", "--check", "entry_main.ybm"]);
    }

    #[test]
    fn build_command_test_prepends_test_subcommand() {
        let c = case(|c| c.cmd = "test".to_string());
        let cmd = build_command(Path::new("/bin/ybm"), Path::new("/work"), &c)
            .unwrap_or_else(|e| panic!("build_command failed: {e}"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["test", "entry_main.ybm"]);
    }

    #[test]
    fn build_command_rejects_unknown_cmd() {
        let c = case(|c| c.cmd = "bogus".to_string());
        match build_command(Path::new("/bin/ybm"), Path::new("/work"), &c) {
            Ok(_) => panic!("unknown cmd should be an error"),
            Err(err) => assert!(err.contains("bogus")),
        }
    }

    // -- missing_required_env -------------------------------------------------------

    #[test]
    fn missing_required_env_accepts_known_vars() {
        let c = case(|c| {
            c.requires_env = vec![
                "YABUMI_TEST_HTTP_BASE".to_string(),
                "YABUMI_TEST_PROC_BIN".to_string(),
            ];
        });
        assert_eq!(missing_required_env(&c, "http://127.0.0.1:0"), None);
    }

    #[test]
    fn missing_required_env_rejects_unknown_var() {
        let c = case(|c| {
            c.requires_env = vec!["SOME_UNKNOWN_VAR".to_string()];
        });
        assert!(missing_required_env(&c, "http://127.0.0.1:0").is_some());
    }

    // -- copy_dir_recursive -----------------------------------------------------------

    #[test]
    fn copy_dir_recursive_copies_nested_files_byte_for_byte() {
        let src = tempdir_shim::TempDir::new().unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        let dst = tempdir_shim::TempDir::new().unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        fs::create_dir_all(src.path().join("sub")).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        fs::write(src.path().join("top.txt"), b"top")
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        fs::write(src.path().join("sub/nested.txt"), b"nested")
            .unwrap_or_else(|e| panic!("write failed: {e}"));

        copy_dir_recursive(src.path(), dst.path()).unwrap_or_else(|e| panic!("copy failed: {e}"));

        assert_eq!(
            fs::read(dst.path().join("top.txt")).unwrap_or_default(),
            b"top"
        );
        assert_eq!(
            fs::read(dst.path().join("sub/nested.txt")).unwrap_or_default(),
            b"nested"
        );
    }
}
