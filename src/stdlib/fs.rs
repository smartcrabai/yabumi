//! fs namespace (STDLIB.md §5, ARCHITECTURE.md §2.1). effect: `fs`.

use crate::eval::value::Value;
use crate::stdlib::{err_value, error_value, none_value, ok_value, some_value};
use std::io::Write as _;
use std::sync::Arc;

fn fs_error(e: &std::io::Error) -> Value {
    error_value("fs", e.to_string())
}

/// `read(path: str): Result[str, Error] uses {fs}`.
#[must_use]
pub fn read(path: &str) -> Value {
    match std::fs::read_to_string(path) {
        Ok(s) => ok_value(Value::Str(Arc::from(s.as_str()))),
        Err(e) => err_value(fs_error(&e)),
    }
}

/// `read_bytes(path: str): Result[list[int], Error] uses {fs}`.
#[must_use]
pub fn read_bytes(path: &str) -> Value {
    match std::fs::read(path) {
        Ok(bytes) => {
            let items: Vec<Value> = bytes
                .into_iter()
                .map(|b| Value::Int(i64::from(b)))
                .collect();
            ok_value(Value::List(Arc::new(items)))
        }
        Err(e) => err_value(fs_error(&e)),
    }
}

/// `write(path: str, content: str): Option[Error] uses {fs}`. None = success, Some(e) = failure
/// (D-TYPE-08: since void cannot occupy Result's type-argument position, this is represented as
/// Option[Error]).
#[must_use]
pub fn write(path: &str, content: &str) -> Value {
    match std::fs::write(path, content) {
        Ok(()) => none_value(),
        Err(e) => some_value(fs_error(&e)),
    }
}

/// `append(path: str, content: str): Option[Error] uses {fs}`.
#[must_use]
pub fn append(path: &str, content: &str) -> Value {
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(content.as_bytes()));
    match result {
        Ok(()) => none_value(),
        Err(e) => some_value(fs_error(&e)),
    }
}

/// `list(path: str): Result[list[str], Error] uses {fs}`. A list of full paths.
#[must_use]
pub fn list(path: &str) -> Value {
    let names: std::io::Result<Vec<String>> = std::fs::read_dir(path).and_then(|entries| {
        entries
            .map(|entry| {
                let path = entry?.path();
                path.into_os_string().into_string().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "directory entry path is not valid UTF-8",
                    )
                })
            })
            .collect()
    });
    match names {
        Ok(names) => ok_value(Value::List(Arc::new(
            names
                .into_iter()
                .map(|s| Value::Str(Arc::from(s)))
                .collect(),
        ))),
        Err(e) => err_value(fs_error(&e)),
    }
}

/// `exists(path: str): bool uses {fs}`. An IO error is treated as false (not wrapped in a
/// Result).
#[must_use]
pub fn exists(path: &str) -> Value {
    Value::Bool(std::path::Path::new(path).exists())
}

/// `remove(path: str): Option[Error] uses {fs}`.
#[must_use]
pub fn remove(path: &str) -> Value {
    match std::fs::remove_file(path) {
        Ok(()) => none_value(),
        Err(e) => some_value(fs_error(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let unique = format!(
            "yabumi-fs-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        p.push(unique);
        std::fs::create_dir_all(&p).unwrap_or(());
        p
    }

    fn as_option_variant(v: &Value) -> &str {
        let Value::Enum(inst) = v else {
            panic!("expected Option[Error]")
        };
        inst.variant_name.as_ref()
    }

    fn as_result_ok(v: &Value) -> &Value {
        let Value::Enum(inst) = v else {
            panic!("expected Result")
        };
        assert_eq!(inst.variant_name.as_ref(), "Ok");
        &inst.fields[0]
    }

    #[test]
    fn write_read_append_round_trip() {
        let dir = temp_dir();
        let path = dir.join("greeting.txt");
        let path_str = path.to_string_lossy().into_owned();

        let write_result = write(&path_str, "hello");
        assert_eq!(as_option_variant(&write_result), "None");

        let content = as_result_ok(&read(&path_str)).clone();
        assert_eq!(content, Value::Str(Arc::from("hello")));

        let append_result = append(&path_str, " world");
        assert_eq!(as_option_variant(&append_result), "None");

        let appended = as_result_ok(&read(&path_str)).clone();
        assert_eq!(appended, Value::Str(Arc::from("hello world")));

        assert_eq!(exists(&path_str), Value::Bool(true));

        let dir_str = dir.to_string_lossy().into_owned();
        let listed = as_result_ok(&list(&dir_str)).clone();
        let Value::List(items) = listed else {
            panic!("expected list[str]")
        };
        assert!(
            items
                .iter()
                .any(|v| matches!(v, Value::Str(s) if s.ends_with("greeting.txt")))
        );

        let remove_result = remove(&path_str);
        assert_eq!(as_option_variant(&remove_result), "None");
        assert_eq!(exists(&path_str), Value::Bool(false));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_missing_file_is_err_with_fs_kind() {
        let dir = temp_dir();
        let path = dir.join("does_not_exist.txt");
        let result = read(&path.to_string_lossy());
        let Value::Enum(inst) = &result else {
            panic!("expected Result")
        };
        assert_eq!(inst.variant_name.as_ref(), "Err");
        let Value::Struct(err) = &inst.fields[0] else {
            panic!("expected Error")
        };
        assert_eq!(err.fields[0], Value::Str(Arc::from("fs")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exists_false_for_missing_path() {
        assert_eq!(exists("/does/not/exist/at/all/yabumi"), Value::Bool(false));
    }

    #[test]
    fn read_bytes_returns_utf8_bytes_as_int_list() {
        let dir = temp_dir();
        let path = dir.join("bytes.txt");
        let path_str = path.to_string_lossy().into_owned();
        let _ = write(&path_str, "AB");
        let bytes = as_result_ok(&read_bytes(&path_str)).clone();
        assert_eq!(
            bytes,
            Value::List(Arc::new(vec![Value::Int(65), Value::Int(66)]))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verifies SPEC §11.2 / STDLIB.md §5 through the full pipeline
    /// (`samples/ok/11-2_fs/entry_main.ybm`).
    // The original issue where "prelude::install()'s placeholders falsely triggered
    // BranchTypeMismatch" has already been resolved (all placeholders now unified to a void
    // return value, see `stdlib::prelude`). The only remaining issue is that this sample
    // assumes the relative path `_out/greeting.txt` -- since `run_ok_sample` doesn't switch the
    // current directory to `samples/ok/11-2_fs`, fs.write fails when resolved relative to the
    // process cwd at `cargo test` time (normally the crate root). Every other test in this
    // codebase (aside from the chdir in `run_all_samples_in_process`) is designed to depend only
    // on absolute paths rooted at `CARGO_MANIFEST_DIR`, not on cwd (see the comment right before
    // `CwdGuard` in `driver.rs`), and adding a chdir to only this unit test would introduce a
    // new race on the process-wide current directory against the concurrently running
    // `driver::tests::run_all_samples_in_process` (which also chdirs), so that approach is
    // skipped. The same `samples/ok/11-2_fs` is already verified through the full pipeline by
    // both `tests/samples.rs::run_all_samples` (which sets `current_dir` per subprocess) and
    // `driver::tests::run_all_samples_in_process` (which chdirs via `CwdGuard` per case, both
    // already enabled), so leaving this test disabled as-is causes no coverage gap.
    #[test]
    #[ignore = "Because this sample assumes the relative path '_out/greeting.txt', fs.write \
                fails when resolved from the process cwd at cargo test time. Adding a chdir to \
                run_ok_sample would introduce a race with driver.rs's \
                run_all_samples_in_process (which also mutates the process-wide cwd) when run \
                concurrently, so this is skipped; those two harnesses already cover the same \
                sample (see the comment above)."]
    fn sample_fs_runs_end_to_end() {
        let result = crate::stdlib::builtins::test_pipeline::run_ok_sample("11-2_fs");
        assert!(
            result.is_ok(),
            "sample should run without Abort: {result:?}"
        );
    }
}
