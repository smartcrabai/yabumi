//! env namespace (STDLIB.md §7, ARCHITECTURE.md §2.1). effect: `env` (also covers reading
//! stdin).

use crate::eval::value::Value;
use crate::stdlib::{err_value, error_value, none_value, ok_value, some_value};
use std::collections::HashMap;
use std::io::Read as _;
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex};

static ENV_OVERLAY: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `get(key: str): Option[str] uses {env}`. Unset returns None; never fails.
#[must_use]
pub fn get(key: &str) -> Value {
    let values = ENV_OVERLAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(value) = values.get(key) {
        return some_value(Value::Str(Arc::from(value.as_str())));
    }
    drop(values);
    match std::env::var(key) {
        Ok(value) => some_value(Value::Str(Arc::from(value.as_str()))),
        Err(_) => none_value(),
    }
}

/// `set(key: str, value: str): void uses {env}`.
pub fn set(key: &str, value: &str) {
    ENV_OVERLAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key.to_owned(), value.to_owned());
}

pub(crate) fn apply_to_command(command: &mut Command) {
    let values = ENV_OVERLAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    command.envs(values.iter());
}

/// `args(): list[str] uses {env}`. Arguments to the script itself; does not include the
/// executable path, subcommand, entry path, or `check` command's `--apply` flag.
#[must_use]
pub fn args() -> Value {
    let items = script_args(std::env::args().collect())
        .into_iter()
        .map(|arg| Value::Str(Arc::from(arg)))
        .collect();
    Value::List(Arc::new(items))
}

fn script_args(argv: Vec<String>) -> Vec<String> {
    let mut rest = argv.into_iter();
    let _binary = rest.next();
    match rest.next().as_deref() {
        Some("check") => {
            let mut found_file = false;
            rest.filter_map(|arg| {
                if arg == "--apply" {
                    None
                } else if found_file {
                    Some(arg)
                } else {
                    found_file = true;
                    None
                }
            })
            .collect()
        }
        Some("test") => {
            let _file = rest.next();
            rest.collect()
        }
        Some(_) => rest.collect(),
        None => Vec::new(),
    }
}

/// `stdin(): Result[str, Error] uses {env}`. Reads everything up to EOF. An IO error becomes
/// Err.
#[must_use]
pub fn stdin() -> Value {
    let mut buf = String::new();
    match std::io::stdin().read_to_string(&mut buf) {
        Ok(_) => ok_value(Value::Str(Arc::from(buf.as_str()))),
        Err(e) => err_value(error_value("env", e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_missing_key_is_none() {
        let key = "YABUMI_TEST_ENVNS_UNSET_KEY_XYZ";
        // SAFETY: a test-only, process-local key that is not shared with other threads.
        unsafe {
            std::env::remove_var(key);
        }
        let v = get(key);
        let Value::Enum(inst) = &v else {
            panic!("expected Option[str]")
        };
        assert_eq!(inst.variant_name.as_ref(), "None");
    }

    #[test]
    fn set_then_get_round_trips() {
        let key = "YABUMI_TEST_ENVNS_ROUNDTRIP_KEY";
        set(key, "sample-value");
        let v = get(key);
        let Value::Enum(inst) = &v else {
            panic!("expected Option[str]")
        };
        assert_eq!(inst.variant_name.as_ref(), "Some");
        assert_eq!(inst.fields[0], Value::Str(Arc::from("sample-value")));
    }

    #[test]
    fn script_args_follow_each_cli_shape() {
        fn argv(items: &[&str]) -> Vec<String> {
            items.iter().map(|item| (*item).to_owned()).collect()
        }

        assert_eq!(
            script_args(argv(&["ybm", "entry_main.ybm", "foo", "bar"])),
            ["foo", "bar"]
        );
        assert_eq!(
            script_args(argv(&["ybm", "entry_main.ybm", "--check"])),
            ["--check"]
        );
        assert_eq!(
            script_args(argv(&["ybm", "check", "--apply", "entry_main.ybm", "foo"])),
            ["foo"]
        );
        assert_eq!(
            script_args(argv(&["ybm", "check", "entry_main.ybm", "foo", "--apply"])),
            ["foo"]
        );
        assert_eq!(
            script_args(argv(&["ybm", "test", "entry_main.ybm", "--check"])),
            ["--check"]
        );
    }

    #[test]
    fn stdin_reads_current_process_stdin_to_eof() {
        // Since the test process's actual stdin cannot be swapped out, this only verifies the
        // contract that "the call returns a Result enum value" (real data is verified on the
        // samples/ok/11-2_env side, via stdin_fixture.txt, SAMPLES_PLAN.md §1.4). Since
        // read_to_string is expected not to fail immediately (hitting EOF or being empty)
        // whether the test harness's own stdin is a pipe or a terminal, this only checks that
        // the result has the shape of Result[str, Error], regardless of whether it's the Ok or
        // Err variant.
        let v = stdin();
        let Value::Enum(inst) = &v else {
            panic!("expected Result[str, Error]")
        };
        assert!(inst.variant_name.as_ref() == "Ok" || inst.variant_name.as_ref() == "Err");
    }

    // `samples/ok/11-2_env/entry_main.ybm` is deliberately not verified through the full
    // pipeline (unlike fs/http/proc/time/rand, it hasn't been added to this unit test): this
    // sample asserts that `env.args() == ["foo", "bar"]` (the args in expected.toml) and that
    // `env.stdin()` matches the contents of `stdin_fixture.txt` exactly, but both of these
    // checks depend on "the real process's actual argv/actual stdin" -- in this test approach,
    // which calls eval::run_top_level directly in-process, the cargo test test binary's own
    // argv/stdin get observed as-is, so the sample's assumptions (args=["foo","bar"],
    // stdin=a fixed fixture) cannot be reproduced. These two points can only be verified
    // correctly with process isolation (actually launching `ybm entry_main.ybm foo bar` as a
    // subprocess and feeding the fixture into its stdin), so that responsibility belongs to
    // `tests/samples.rs` (the existing acceptance test harness, enabled once Unit 17's
    // driver.rs/cli are complete) (in addition, the BranchTypeMismatch false positive caused by
    // check_all_decls, which the other similarly-`#[ignore]`d tests in
    // stdlib::builtins::test_pipeline mention, is also still unresolved, so running the full
    // pipeline isn't possible yet regardless).
}
