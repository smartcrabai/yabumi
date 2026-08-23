//! proc namespace (STDLIB.md §8, ARCHITECTURE.md §2.1). effect: `proc`.
//!
//! `ProcOutput` is a normal Yabumi-side struct (STDLIB.md) with no dedicated Rust type -- at
//! runtime it's constructed as a `Value::Struct` based on the `StructDecl` that `prelude.rs`
//! pre-registers.

use crate::eval::value::{StructInstance, Value};
use crate::stdlib::{err_value, error_value, ok_value};
use std::process::Stdio;
use std::sync::Arc;

/// `run(cmd: str, args: list[str]): Result[ProcOutput, Error] uses {proc}`. Only a spawn failure
/// is Err; a non-zero exit is still returned as Ok(ProcOutput) (check via exit_code). There is
/// no API for passing standard input to the child process (SAMPLES_PLAN.md §1.4), so stdin is
/// always started closed (`Stdio::null()`) -- this makes `proc.run`'s behavior deterministic
/// regardless of the test harness's own stdin state (pipe/tty/null) (SAMPLES_PLAN.md §1.4.2,
/// "behavior with empty/closed stdin").
#[must_use]
pub fn run(cmd: &str, args: &[Value]) -> Value {
    let arg_strs: Vec<&str> = args
        .iter()
        .map(|v| match v {
            Value::Str(s) => s.as_ref(),
            _ => unreachable!(
                "type-checked already, so proc.run's second argument is always list[str]"
            ),
        })
        .collect();

    let mut command = std::process::Command::new(cmd);
    command.args(&arg_strs).stdin(Stdio::null());
    crate::stdlib::envns::apply_to_command(&mut command);
    let output = command.output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            // A signal termination (Unix) makes code() return None. Since SPEC/STDLIB.md doesn't
            // define a convention for this case, we fall back to -1 instead of panicking (a
            // decision made in this file).
            let exit_code: i64 = out.status.code().map_or(-1, i64::from);
            ok_value(Value::Struct(Arc::new(StructInstance {
                type_name: Arc::from("ProcOutput"),
                fields: vec![
                    Value::Str(Arc::from(stdout.as_str())),
                    Value::Str(Arc::from(stderr.as_str())),
                    Value::Int(exit_code),
                ],
            })))
        }
        Err(e) => err_value(error_value("proc", format!("failed to spawn '{cmd}': {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `env!("CARGO_BIN_EXE_proc_fixture")` is only set by Cargo for external test targets like
    /// `tests/samples.rs` (this file is a unit test under `src/main.rs` and doesn't qualify --
    /// it would fail to compile with "not defined"). Instead, we derive the location of the
    /// `proc_fixture` binary, which sits alongside this same build under `target/<profile>/`,
    /// relative to `current_exe()` (`target/<profile>/deps/ybm-<hash>`).
    fn fixture_bin() -> std::path::PathBuf {
        let mut dir = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("."));
        dir.pop(); // Drop the test binary's own file name.
        if dir.ends_with("deps") {
            dir.pop(); // target/<profile>/deps -> target/<profile>
        }
        let name = if cfg!(windows) {
            "proc_fixture.exe"
        } else {
            "proc_fixture"
        };
        dir.join(name)
    }

    fn ok_fields(v: &Value) -> &[Value] {
        let Value::Enum(inst) = v else {
            panic!("expected Result[ProcOutput, Error]")
        };
        assert_eq!(inst.variant_name.as_ref(), "Ok");
        let Value::Struct(out) = &inst.fields[0] else {
            panic!("expected ProcOutput")
        };
        &out.fields
    }

    #[test]
    fn echo_prints_text_and_exits_zero() {
        let bin = fixture_bin().to_string_lossy().into_owned();
        let result = run(
            &bin,
            &[
                Value::Str(Arc::from("echo")),
                Value::Str(Arc::from("hello")),
            ],
        );
        let fields = ok_fields(&result);
        assert_eq!(fields[0], Value::Str(Arc::from("hello\n"))); // stdout
        assert_eq!(fields[1], Value::Str(Arc::from(""))); // stderr
        assert_eq!(fields[2], Value::Int(0)); // exit_code
    }

    #[test]
    fn fail_exits_with_requested_code_and_writes_stderr() {
        let bin = fixture_bin().to_string_lossy().into_owned();
        let result = run(
            &bin,
            &[Value::Str(Arc::from("fail")), Value::Str(Arc::from("3"))],
        );
        let fields = ok_fields(&result);
        assert_eq!(fields[0], Value::Str(Arc::from(""))); // stdout
        let Value::Str(stderr) = &fields[1] else {
            panic!("expected str")
        };
        assert!(!stderr.is_empty());
        assert_eq!(fields[2], Value::Int(3));
    }

    #[test]
    fn cat_with_closed_stdin_reads_eof_immediately() {
        let bin = fixture_bin().to_string_lossy().into_owned();
        let result = run(&bin, &[Value::Str(Arc::from("cat"))]);
        let fields = ok_fields(&result);
        assert_eq!(fields[0], Value::Str(Arc::from(""))); // stdout
        assert_eq!(fields[2], Value::Int(0));
    }

    #[test]
    fn spawn_failure_is_err_with_proc_kind() {
        let result = run("/definitely/not/a/real/binary/yabumi", &[]);
        let Value::Enum(inst) = &result else {
            panic!("expected Result")
        };
        assert_eq!(inst.variant_name.as_ref(), "Err");
        let Value::Struct(err) = &inst.fields[0] else {
            panic!("expected Error")
        };
        assert_eq!(err.fields[0], Value::Str(Arc::from("proc")));
    }

    /// Verifies SPEC §11.2 / STDLIB.md §8 through the full pipeline
    /// (`samples/ok/11-2_proc/entry_main.ybm`, the SAMPLES_PLAN.md §1.4.2 contract table).
    #[test]
    fn sample_proc_runs_end_to_end() {
        let bin = fixture_bin().to_string_lossy().into_owned();
        // SAFETY: this is the only function in this process that writes this key
        // (no other file reads it either).
        unsafe {
            std::env::set_var("YABUMI_TEST_PROC_BIN", &bin);
        }
        let result = crate::stdlib::builtins::test_pipeline::run_ok_sample("11-2_proc");
        assert!(
            result.is_ok(),
            "sample should run without Abort: {result:?}"
        );
    }
}
