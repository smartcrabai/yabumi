# Yabumi Samples Design Document (SAMPLES_PLAN)

This document is the directory-structure design document for the `.ybm` sample collection placed under `samples/`. The canonical source is `SPEC.md`; the subordinate documents are `docs/DECISIONS.md` (rulings on implementation details) and `docs/STDLIB.md` (standard library signatures). This document defines conventions for sample placement, naming, and recording expected results, to the extent they do not conflict with those.

**Current status**: the sample bodies (`.ybm` files, `expected.toml`) have not yet been created. This document settles only the design for future use as the acceptance test suite for the Rust-based interpreter. It is written at a granularity that lets its listings double directly as a work checklist once implementation begins.

---

## 0. Preconditions (Recap of the SPEC §10 Module Rules)

- There is no import syntax. A directory is effectively the unit of a module.
- A module file has the `module` directive (a bare, nameless directive) as its effective first line, after stripping any shebang.
- Whichever of `ybm <file>` / `ybm check <file>` / `ybm test <file>` is run, it automatically pulls in **all** `.ybm` files with a `module` directive that sit **at the same directory level as the entry file passed in** -- **immediate children only** (it does not recurse into subdirectories, D-MOD-01).
- A module may contain only declarations (top-level executable statements are forbidden; violating this is E5002).
- There is a single flat namespace. A name collision is E1001.
- Writing a `module` directive in the entry file itself normally results in E5002, since it contains executable statements (this is intentional behavior).

An important property follows from this rule: **within the same directory, a `.ybm` file without a `module` directive is never loaded at all unless it is the file actually passed to `ybm`**. This means a layout where "multiple executable files (entries) exist in the same directory" comes in two distinct patterns: (a) entries that cooperate through a shared module, and (b) entries that are fully independent and ignore each other's existence entirely. This design explicitly turns both into samples (see §2).

---

## 1. Directory Conventions

### 1.1 Overall Structure

```
samples/
  ok/            success cases (type-checking and execution both succeed)
  err/
    static/      lexical/syntax/type/effect/mutability/module static errors (E0000-E3999, E5000-E5999)
    lint/        lint warnings only (E4000-E4999; type-checking itself passes)
    runtime/     runtime abnormal termination (panics, top-level `?` propagation; E6000-E6999)
    cli/         pre-launch CLI errors (file not found, invalid extension; E9000-E9999)
  fmt/           fmt (formatting) verification. Read-only `ybm check` diff detection + `--apply` rewrite verification
  doctest/       samples dedicated to the `ybm test` doc-test mechanism
```

The diagnostic code ranges map directly onto the table in `docs/DECISIONS.md` D-DIAG-01 (E0 = lexical/syntax, E1 = type, E2 = effect, E3 = mutability, E4 = lint, E5 = module, E6 = runtime, E9 = CLI). E5xxx (module) errors are included under `err/static/` because module errors are a category clearly distinct from lint and runtime errors in that `ybm`/`ybm check` detect them statically, before execution.

One directory = one theme. Each theme corresponds to a SPEC section or subsection. Directory names are fixed to the following pattern:

```
<spec-tag>_<slug>          # e.g. 3-2_collections, 7-2_question_operator
<code>_<slug>              # some of err/lint, err/runtime, err/cli are named after their diagnostic code
                            # e.g. e4001_unused_variable, e6001_out_of_range_access
```

- `spec-tag` is the SPEC chapter/section number (`3-2` is §3.2). When a directory's theme spans multiple sections, the primary section number is used, and secondary sections are captured in the listing/coverage-matrix columns instead.
- `slug` uses only lowercase ASCII letters and underscores (a self-referential alignment with Yabumi's own identifier rule, D-LEX-02).
- When one section needs multiple directories (e.g. §10's modules split across success cases, error cases, and multi-entry patterns), a suffix like `10a_`, `10b_`, `10c_` is appended.

### 1.2 File Naming Rules (Distinguishing Entry Files from Modules)

| Pattern | Meaning |
|---|---|
| `entry_<slug>.ybm` | A file that **does not have** a `module` directive. An executable file meant to be passed to `ybm <file>`. A directory may contain more than one (see §2). |
| `mod_<slug>.ybm` | A file whose effective first line, after stripping any shebang, is `module`. Never used on its own as the test target passed to `ybm` (the only exceptions being when passing it would just be a no-op of declarations only, or when it is deliberately used as a broken module to verify an error). |
| `*.in.ybm` / `*.out.ybm` | Used only under `fmt/`. A pair giving the canonical pre-/post-formatting forms. |
| `stdin_<slug>.txt` | A fixed stdin fixture used to verify `env.stdin()`. |
| `_out/.gitkeep` | The write sandbox for `fs`-effect samples (its contents are cleaned up on every test run; only the empty directory is committed to git). |
| `expected.toml` | Required in every directory. A list of run cases and their expected results (see 1.3). |

**Detection rule**: the test harness can enumerate "files that can potentially be run" with `find samples -name 'entry_*.ybm'`, and "files never used on their own" with `find samples -name 'mod_*.ybm'`. Because the file name `entry_*.ybm` alone does not determine which subcommand runs it or how, it must always be interpreted together with the `entry` field in `expected.toml`.

### 1.3 Format of the Expected-Results File (`expected.toml`)

Each directory has exactly one `expected.toml`. It's an array of `[[case]]` tables enumerating the verification cases for the multiple entries and subcommands within that directory.

```toml
# Common schema
[[case]]
id = "run_ok"                     # a case ID unique within the directory
entry = "entry_main.ybm"          # the target file (never specify a file with a module directive directly)
cmd = "run"                        # run | check | check_diff | test
args = []                          # additional CLI arguments to ybm itself (usually empty)
stdin_file = ""                    # relative path to a stdin fixture (empty input if omitted)
exit_code = 0
diagnostics = []                   # expected diagnostic codes, listed in the order corresponding to ascending file:line:col. Empty = no diagnostics
fmt_diff_expected = false          # only meaningful when cmd = "check_diff"
fmt_result_file = ""               # after cmd = "check" (`--apply` rewrite), the file this result should byte-match
stdout = { mode = "exact", value = "" }     # mode: exact | contains. Takes either value or file
stderr = { mode = "exact", value = "" }
doc_blocks = []                    # only for cmd = "test". [{ line = 12, result = "pass" }, { line = 20, result = "fail", code = "E6004" }, ...]
                                    # code is present only when result = "fail"; the expected diagnostic code corresponding to the [Ennnn] part of D-DOC-05
requires_env = []                  # array of environment variable names needed to run this case (e.g. ["YABUMI_TEST_HTTP_BASE"], ["YABUMI_TEST_PROC_BIN"]). Empty = no environment variables needed.
                                    # the harness may treat this case as skipped if even one listed variable is unset at run time (see 1.4.1/1.4.2)
notes = "a one-line human-readable explanation"
```

- Mapping of `cmd`: `run` = `ybm <entry>` / `check` = `ybm check <entry> --apply` (explicit fmt rewrite; the default postfix form) / `check_diff` = `ybm check <entry>` (read-only fmt diff detection, no rewrite) / `test` = `ybm test <entry>`.
- `diagnostics` is an array of string codes like `E1001`. Per D-CLI-03 (all diagnostics are collected and reported in ascending `file:line:col` order), when there are multiple, they are listed in that order.
- Only `err/cli/` cases may set `entry` to a file name that doesn't actually exist in the directory (this is to verify E9001 = file not found; in this case the file itself is not provided).
- `fmt/` directories set `entry` to `*.in.ybm` / `*.out.ybm` and use `fmt_diff_expected` / `fmt_result_file` (see the listing in §3.6).
- `doctest/` directories use `doc_blocks` on `cmd = "test"` cases, recording the pass/fail of each block along with its executed line number in the source (D-DOC-05: the actual file line, not a line relative to the `##` fence).
- A case that specifies one or more `requires_env` entries assumes that, at run time (`run`/`test`), the harness has a real process ready (a mock HTTP server, a proc fixture binary). If a listed environment variable is unset at run time, the harness may treat that case as skipped (see 1.4.1/1.4.2).

### 1.4 Deterministic Testing Policy for External Dependencies (net / proc / time / rand / fs)

Of the six effects `fs`/`net`/`env`/`proc`/`time`/`rand`, all but `fs` can have results that depend on the environment, the clock, randomness, or reachability. The samples themselves are kept deterministic under the following policy. `net`/`proc` do not take the easy way out of "not verifying execution" -- they are verified all the way through execution (`run`/`test`) via deterministic mock entities (a local HTTP server / a dedicated fixture binary) that the Rust-side test harness provides.

| effect | Policy |
|---|---|
| `net` (http) | Before each test run, the Rust-side test harness starts a mock HTTP server on localhost and passes its base URL (e.g. `http://127.0.0.1:PORT`) to the sample process as the environment variable `YABUMI_TEST_HTTP_BASE`. The sample reads it with `env.get("YABUMI_TEST_HTTP_BASE")` and actually communicates, e.g. `http.get(base + "/json/user")`, asserting on the result (the effect becomes `{net, env}`). The mock server's fixed endpoint contract is the table in §1.4.1. The corresponding `expected.toml` case sets `requires_env = ["YABUMI_TEST_HTTP_BASE"]`, and the harness may skip that case in an environment without that variable. |
| `proc` | Before the test run, the Rust-side test harness builds a dedicated fixture binary and passes its full path to the sample process as the environment variable `YABUMI_TEST_PROC_BIN`. The sample reads it with `env.get("YABUMI_TEST_PROC_BIN")` and actually launches a child process, e.g. `proc.run(bin, ["echo", "hello"])`, asserting on `stdout`/`stderr`/`exit_code` (the effect is `{proc, env}`). The fixture's argument contract is the table in §1.4.2. The corresponding `expected.toml` case sets `requires_env = ["YABUMI_TEST_PROC_BIN"]`, and the harness may skip that case in an environment without that variable. Since `proc.run` has no language-level API for supplying stdin to the child process (see STDLIB.md §8), the fixture's `cat` subcommand can only be verified from the sample side as "launched without passing stdin (closed/empty stdin), immediately outputs an empty string as EOF, and exits 0." |
| `rand` | By the language spec, a degenerate input always determines the output uniquely (`rand.int(5, 6)` is always 5; `rand.choice`/`rand.shuffle` on a single-element `list` always give the same result). This property is used to verify results deterministically all the way through execution, via `assert`. |
| `time` | The value of `time.now()` itself is not verified (only effect propagation and its type are checked). `time.format`/`time.parse` are asserted only against fixed `epoch_ms` literals. `time.sleep` is checked only for "requiring the effect and returning `void`"; elapsed time is not verified. |
| `fs` | Both input and output are deterministic (same content -> same result). Writes are confined to the fixed `_out/` subdirectory under the sample directory, and the test harness deletes the contents of `_out/` (other than `.gitkeep`) before and after each run to guarantee idempotency. Nothing other than relative paths is used (no absolute paths, no paths containing `..`). |

#### 1.4.1 net Mock Server Contract Table (under `YABUMI_TEST_HTTP_BASE`)

The list of fixed endpoints the Rust-side test harness must implement, given as paths relative to the base URL. This contract is defined by this document, and the harness implementation follows it (the implementation does not come first).

| Method | Path | Status | Content-Type | Body |
|---|---|---|---|---|
| GET | `/text/hello` | 200 | `text/plain` | `hello` |
| GET | `/json/user` | 200 | `application/json` | `{"name":"alice","age":30}` |
| GET | `/status/404` | 404 | `text/plain` | `not found` |
| GET | `/slow` | 200 | `text/plain` | `slow-ok` (a fixed delay, dependent on the harness implementation, is inserted before the response is returned; the sample side asserts only the response content, not the delay itself) |
| POST | `/echo` | 200 | reflects the request's `Content-Type` as-is | returns the request body as-is |
| PUT | `/put/target` | 200 | `text/plain` | `put-ok` |
| DELETE | `/delete/target` | 200 | `text/plain` | `deleted` |
| GET | `/headers/echo` | 200 | `text/plain` | returns the value of the request header `X-Test` as-is (empty string if not sent; used to verify `HttpOptions.headers`) |

#### 1.4.2 proc Fixture Binary Contract Table (Argument Spec for the Binary Pointed to by `YABUMI_TEST_PROC_BIN`)

| Invocation (`args`) | Behavior |
|---|---|
| `["echo", "<text>"]` | outputs `<text>` + a newline to stdout, exit code 0, stderr empty |
| `["fail", "<code>"]` | outputs a fixed message to stderr (e.g. `failed with code <code>`), exit code is `<code>` (the numeric string is used directly as the exit code), stdout empty |
| `["cat"]` | reads stdin through EOF and outputs the same content to stdout as-is, exit code 0 (as noted in §1.4, since there is no way to supply stdin from the Yabumi side, the sample only verifies the path "empty/closed stdin -> empty stdout, exit 0") |

---

## 2. Catalog of "Multiple Executable Files in the Same Directory" Patterns

Per the requirements, this pattern is covered in different forms across multiple directories. Each of the four patterns is handled in its own directory.

| Pattern | Directory | Description |
|---|---|---|
| (1) Shared module + independent multiple entries (success case) | `samples/ok/10a_module_shared_by_two_entries/` | `mod_shapes.ybm` declares structs/enums/functions, and the two entries `entry_area_report.ybm` and `entry_shape_filter.ybm` each use it independently. Both entries exit 0. Demonstrates D-MOD-01/03/04's rule that "modules at the same directory level are automatically pulled into every entry in full." |
| (2) Fully independent multiple entries (no module, no name collision) | `samples/ok/10b_independent_entries_same_directory/` | `entry_alpha.ybm` and `entry_beta.ybm` sit in the same directory, and **both independently define a top-level function of the same name**. Since there is no file with a `module` directive, running either entry never loads the other at all, and there is no name collision (E1001). This precisely illustrates the boundary of the module rule: "two functions with the apparently same name exist, yet it is not a type error." |
| (3) Shared module + two entries, one type-check-only and one runnable (different effect profiles) | `samples/ok/15_end_to_end_showcase/` | Shares `mod_repo.ybm`, with two entries: `entry_showcase_typecheck_only.ybm` (uses real `http.get`; only `check` is the verification target) and `entry_showcase_runnable.ybm` (uses deterministic literal data in place of the network, verified through to execution). Adapts SPEC §15's overall sample into a form compatible with the deterministic policy for external dependencies (§1.4). The `toml.encode` call is adapted to follow DECISIONS D-STDPOL-09 (top level must be a table) by wrapping `top` (a `list[Repo]`) in a dict like `{"repos": top}` before encoding. |
| (4) Both entries fail in a chain because the shared module is broken (error case) | `samples/err/static/10c_module_toplevel_statement_cascade/` | `entry_alpha.ybm` and `entry_beta.ybm` are each individually correct, but the `mod_broken.ybm` in the same directory contains a top-level executable statement (E5002), so **running either entry** fails during the check of the automatically-pulled-in `mod_broken.ybm`. Shows the flip side of "a module's automatic inclusion applies to each entry individually": one broken module breaks every entry at that directory level. |

---

## 3. Full Directory Listing

The "Files" column in each table omits `expected.toml` (since it is required in every directory). The "SPEC section" column lists the primary section first, with related sections given after a `/`.

### 3.1 `samples/ok/` (success cases, 40 directories)

| Path | Theme | Files included (role) | SPEC section verified |
|---|---|---|---|
| `1_cli_three_subcommands` | Basics of the CLI's 3 subcommands | `entry_main.ybm` | §1 |
| `2_lexical_basics` | shebang, line comments, doc comments, 4-space indentation | `entry_main.ybm` | §2 |
| `3-1_primitives` | Basic operations on int/float/bool/str; round-trip conversions via `int()`/`float()`/`str()`/`parse_int()`/`parse_float()` (asserting invariants such as `parse_float(str(x)) == x`, based on D-TYPE-14) | `entry_main.ybm`, `entry_conversion_roundtrip.ybm` | §3.1 |
| `3-2_collections` | list/dict/set/tuple literals, empty collections, single-element tuples; dict/set insertion-order preservation (D-COL-01; asserts the `.entries()`/`.to_list()` order after insert/remove/re-insert) | `entry_literals.ybm`, `entry_edge_cases.ybm` | §3.2 |
| `3-3_stdlib_types` | Every method of Result (is_ok/is_err/ok/err/unwrap_or/unwrap_or_else/map/map_err/and_then) and Option (is_some/is_none/unwrap_or/unwrap_or_else/map/and_then/filter/ok_or), plus verification of an `Error.cause` chain of three or more levels | `entry_main.ybm`, `entry_full_method_coverage_and_cause_chain.ybm` | §3.3 |
| `3-4_type_annotations_and_inference` | required annotations on function signatures, local inference, assignment-target-annotation-driven inference | `entry_main.ybm` | §3.4 |
| `3-5_struct_and_enum` | struct (co-located methods, named-argument construction) / enum (positional-argument construction, match) | `entry_struct_methods.ybm`, `entry_enum_variants.ybm` | §3.5 |
| `3-6_generics` | user-defined generic functions, explicit type-argument calls; construction and field/variant access for user-defined generic struct/enum (`struct Box[T]`, `enum Pair[A, B]`) | `entry_main.ybm`, `entry_generic_struct_and_enum.ybm` | §3.6 |
| `4_mutability` | immutable by default; `var` reassignment; field mutation; push/pop; subscript assignment into a var list/dict (`xs[i] = v`/`m[k] = v`, D-COL-02) | `entry_main.ybm` | §4 |
| `5_functions_hoisting_and_toplevel_order` | declaration hoisting; sequential execution order of top-level statements; mutual recursion (two functions that can call each other regardless of declaration order, D-SYN-08) | `entry_main.ybm`, `entry_mutual_recursion.ybm` | §5 |
| `5-1_lambdas` | lambdas require parentheses; single expression (including multi-line if/match) | `entry_main.ybm` | §5.1 |
| `5b_return_implicit_ok_some_wrap` | the three priority cases of the implicit `Ok`/`Some` wrapping rule for `return`'s target expression (D-TYPE-17): (1) returning a bare `T` that gets implicitly wrapped, (2) explicitly constructing and returning `Ok`/`Some`, (3) the boundary where a type mismatch produces E1020 (only this case's `expected.toml` has `exit_code = 1` and `diagnostics = ["E1020"]`; the directory as a whole is structured to show D-TYPE-17's three branches together as one set) | `entry_implicit_wrap.ybm`, `entry_explicit_ok_wrap.ybm`, `entry_type_mismatch.ybm` | §5 / §7.1 / §7.2 |
| `6-1_expression_oriented_if_match` | if/match expressions; multi-branch nested else; exhaustiveness; the rule that a multi-statement arm's tail expression becomes the block's value (D-SYN-11); the positive contrast between mandatory wildcard-`_` exhaustiveness for non-enum scrutinees (int/str) and 2-arm bool exhaustiveness (D-TYPE-18) | `entry_main.ybm`, `entry_multi_statement_block_value.ybm`, `entry_non_enum_match_with_wildcard_and_bool.ybm` | §6.1 |
| `6-2_iterators` | map/filter/fold/.../chain; sequential side-effect iteration via `each` | `entry_main.ybm` | §6.2 |
| `6-3_pipe_operator` | bare-name pipe, `_` placeholder, stage-trailing `?` | `entry_main.ybm` | §6.3 |
| `6-3_operator_precedence_mixed_expression` | empirically exercises D-OP-01's precedence table: mixes arithmetic (`*`/`+`), comparison, logical (`and`/`or`), pipe, and stage-trailing `?` in a single expression, verifying with multiple `assert`s that it evaluates per the precedence table even without parentheses (also includes one case for comparison non-chaining and the boundary where `not` binds tighter than comparison) | `entry_main.ybm` | §6.3 / §7.2 |
| `6-4_strings` | f-strings (including `{{`/`}}`; also verifies interpolation embedding CJK characters and emoji); concatenation; structural equality comparison; char-index-unit verification with multi-byte characters (CJK, emoji) (D-COL-03; asserts that `len`/`get`/`chars` operate in units of Unicode scalar values, not byte counts) | `entry_main.ybm` | §6.4 |
| `7-1_error_type` | `Error` construction (explicit `cause`, chaining) | `entry_main.ybm` | §7.1 |
| `7-2_question_operator` | `?` on both Result/Option; `?` inside a pipe; `?` inside a lambda | `entry_main.ybm` | §7.2 |
| `7-3_result_must_be_used` | every context in which a Result is judged "used"; `_ = f()` | `entry_main.ybm` | §7.3 |
| `7-4_safe_apis` | choosing among the panic-avoiding safe-API alternatives such as `get`/`checked_div` | `entry_main.ybm` | §7.4 |
| `7-5_assert` | successful `assert` cases in ordinary code | `entry_main.ybm` | §7.5 |
| `8_effects` | `uses {..}` declarations; summing into the caller; implicit propagation into higher-order functions; multi-hop transitive propagation (an A->B->C chain of calls); the union of effects for a higher-order function taking multiple function-typed arguments (D-FUNC-03) | `entry_main.ybm`, `entry_transitive_and_hof_effects.ybm` | §8 |
| `9_concurrency_par` | `par [..]`/`par (..)` (fixed arity) versus `par_map`/`par_each` (dynamic); input-order guarantee; no fail-fast (asserts that even if some elements return Err, every element runs to completion and all results come back together as `list[Result[T,E]]`); nesting a `par` call inside `par` (D-PAR-02) | `entry_par_fixed_arity.ybm`, `entry_par_map_and_each.ybm`, `entry_par_nested.ybm` | §9 |
| `10a_module_shared_by_two_entries` | shared module + two independent entries (pattern 1) | `mod_shapes.ybm`, `entry_area_report.ybm`, `entry_shape_filter.ybm` | §10 |
| `10b_independent_entries_same_directory` | two fully independent entries; same-name functions don't collide (pattern 2) | `entry_alpha.ybm`, `entry_beta.ybm` | §10 |
| `10c_module_constants_and_cross_reference` | module-level constants (literals only); cross-references between multiple modules | `mod_constants.ybm`, `mod_helpers.ybm`, `entry_main.ybm` | §10 |
| `11-1_codec_json_yaml_toml_csv` | assignment-target-annotation-driven decode/encode (json/yaml/toml); csv's explicit `[T]`; dynamic decode with `Value`; asserts decode-then-encode round-trip equality (round-trip) for all four formats | `entry_json_yaml_toml.ybm`, `entry_csv.ybm`, `entry_roundtrip_all_codecs_and_value.ybm` | §11.1 |
| `11-2_fs` | read/write/append/list/exists/remove (using the `_out/` sandbox) | `entry_main.ybm` | §11.2 |
| `11-2_http` | verifies get/post/put/delete and `HttpOptions`+`request` (headers/timeout) through real communication with the mock HTTP server via `YABUMI_TEST_HTTP_BASE` (contract table in §1.4.1, effect `{net, env}`) | `entry_main.ybm` | §11.2 |
| `11-2_env` | get/set/args/stdin (using a fixed stdin fixture) | `entry_main.ybm`, `stdin_fixture.txt` | §11.2 |
| `11-2_proc` | actually calls `proc.run` against the fixture binary via `YABUMI_TEST_PROC_BIN`, verifying stdout/stderr/exit_code through execution (the `echo`/`fail`/`cat` subcommands, contract table in §1.4.2, effect `{proc, env}`) | `entry_main.ybm` | §11.2 |
| `11-2_time` | format/parse with a fixed epoch_ms; verifies only sleep's effect/type | `entry_main.ybm` | §11.2 |
| `11-2_rand` | deterministic int/choice/shuffle via degenerate ranges | `entry_main.ybm` | §11.2 |
| `11-2_regex` | is_match/find/find_all/replace/replace_all/captures | `entry_main.ybm` | §11.2 |
| `11-2_math` | checked_*/abs_*/min_*/max_*/floor/ceil/round/sqrt/pow/PI/E | `entry_main.ybm` | §11.2 |
| `11-3_builtins_print_eprint_assert` | the 4-type overloads of print/eprint; stdout/stderr separation | `entry_main.ybm` | §11.3 |
| `12_fmt_lint_clean_baseline` | a positive-contrast example where an already-formatted, lint-clean file exits 0 under both `check` and `check --apply` | `entry_main.ybm` | §12 |
| `14_memory_model_value_semantics` | value copying; `var self` as the sole mutation-propagation path; closure value capture | `entry_main.ybm` | §14 |
| `15_end_to_end_showcase` | an adaptation of the SPEC §15 sample (pattern 3) | `mod_repo.ybm`, `entry_showcase_typecheck_only.ybm`, `entry_showcase_runnable.ybm` | §15 |

### 3.2 `samples/err/static/` (static errors, 19 directories, E0000-E3999, E5000-E5999)

| Path | Theme | Files included (expected code) | SPEC section verified |
|---|---|---|---|
| `1_full_diagnostic_report_ordering` | reporting every one of multiple independent errors within a single file (D-CLI-03) | `entry_multiple_independent_errors.ybm` (3 errors: E1002+E1050+E1021) | §1 |
| `2_lexical_errors` | the full set of lexical errors | `entry_tab_character.ybm` (E0001), `entry_unterminated_string.ybm` (E0002), `entry_invalid_escape.ybm` (E0003), `entry_invalid_int_literal.ybm` (E0004), `entry_invalid_float_literal.ybm` (E0004), `entry_unknown_token.ybm` (E0005), `entry_non_ascii_identifier.ybm` (E0005, D-LEX-02: identifiers are ASCII-only) | §2 |
| `2_syntax_errors` | the full set of syntax errors | `entry_indentation_mismatch.ybm` (E0501), `entry_elif_not_supported.ybm` (E0502), `entry_pipe_missing_placeholder.ybm` (E0503) | §2 / §6.1 / §6.3 |
| `3-2_collection_type_errors` | collection type-constraint violations | `entry_heterogeneous_list_literal.ybm` (E1010), `entry_dict_key_type_not_allowed.ybm` (E1011), `entry_set_element_type_not_allowed.ybm` (E1012) | §3.2 |
| `3-4_type_annotation_and_inference_errors` | 3 cases of missing annotations / uninferable types (D-TYPE-15) | `entry_missing_param_annotation.ybm` (E1002), `entry_uninferable_empty_collection.ybm` (E1003), `entry_uninferable_none_literal.ybm` (E1003), `entry_uninferable_generic_return.ybm` (E1003) | §3.4 |
| `3-6_generic_operator_misuse` | an unsupported operator applied to an unconstrained type parameter | `entry_unconstrained_type_param_operator.ybm` (E1013) | §3.6 |
| `4_mutability_errors` | 5 facets of mutability violations | `entry_reassign_immutable.ybm` (E3001), `entry_field_write_immutable.ybm` (E3001), `entry_var_self_required.ybm` (E3001), `entry_nested_root_not_var.ybm` (E3001, D-MUT-03), `entry_subscript_write_immutable.ybm` (E3001, D-COL-02) | §4 |
| `6-1_match_and_branch_errors` | inability to unify across branches; exhaustiveness violations; the boundary where a multi-statement arm's tail is a statement with no value (D-SYN-11) | `entry_if_branch_type_mismatch.ybm` (E1020), `entry_match_non_exhaustive.ybm` (E1021), `entry_block_tail_non_expression.ybm` (E1020) | §6.1 |
| `6-1_non_enum_match_missing_wildcard` | a `match` on a non-enum scrutinee (int/str) missing the wildcard `_` arm (D-TYPE-18) | `entry_int_match_missing_wildcard.ybm` (E1021), `entry_str_match_missing_wildcard.ybm` (E1021) | §6.1 |
| `6-4_operator_type_errors` | operator type-constraint violations (including D-OP-01 precedence pitfalls) | `entry_int_float_mixed_arithmetic.ybm` (E1050), `entry_struct_ordering_comparison.ybm` (E1051), `entry_not_precedence_pitfall.ybm` (`not x > 3` reports E1020 and E1051) | §6.3 / §6.4 |
| `7-2_question_operator_type_errors` | 2 kinds of type mismatch with `?` | `entry_option_expr_in_result_function.ybm` (E1060), `entry_error_type_mismatch_across_functions.ybm` (E1060) | §7.2 |
| `7-3_unused_result_errors` | 2 contexts where a Result goes unused | `entry_unused_result_statement.ybm` (E1040), `entry_unused_result_pipe_tail.ybm` (E1040) | §7.3 |
| `8_effect_errors` | 3 kinds of effect violation | `entry_pure_function_direct_impure_call.ybm` (E2001), `entry_pure_function_indirect_via_stored_lambda.ybm` (E2001, D-EFF-02), `entry_undeclared_effect_row_overflow.ybm` (E2002) | §8 |
| `9_par_branch_bare_question_operator` | a bare top-level `?` is forbidden in each `par` branch's expression or in a resolved builtin parallel lambda (D-PAR-03) | `entry_par_literal_bare_question.ybm` (E0502), `entry_par_map_lambda_bare_question.ybm` (E1061) | §7.2 / §9 |
| `10a_module_name_collision` | a name collision between an entry and a module | `entry_main.ybm`, `mod_util.ybm` (E1001) | §10 |
| `10b_module_directive_malformed` | a malformed `module` directive | `entry_probe.ybm`, `mod_bad_directive.ybm` (E5001) | §10 |
| `10c_module_toplevel_statement_cascade` | a broken shared module cascades failure to every entry (pattern 4) | `entry_alpha.ybm`, `entry_beta.ybm`, `mod_broken.ybm` (E5002) | §10 |
| `10d_entry_self_module_directive` | confirms that when the file passed to `ybm` itself has a `module` directive, D-MOD-02 (top-level executable statements forbidden) applies and it intentionally becomes E5002 (D-MOD-01) | `entry_with_module_directive.ybm` (a `module` directive on line 1, followed by ordinary executable statements. Its file name follows the `entry_` naming rule of §1.2, but its content is effectively `mod_`-equivalent; this is spelled out in its notes) (E5002) | §10 |
| `11-1_toml_encode_root_type_error` | regression coverage for SPEC §15 list-root TOML encoding | `entry_toml_encode_list_root.ybm` (success) | §11.1 / §15 |

### 3.3 `samples/err/lint/` (lint warnings, 5 directories, E4000-E4999)

| Path | Theme | Files included (expected code) | SPEC section verified |
|---|---|---|---|
| `e4001_unused_variable` | an unused local variable | `entry_unused_local.ybm` (E4001) | §12 |
| `e4002_unused_function` | an unused top-level function (including the contrast that struct methods are exempt) | `entry_with_dead_function.ybm` (E4002) | §12 |
| `e4003_shadowing` | shadowing at if/match/lambda-parameter/function boundaries | `entry_shadowing_various_scopes.ybm` (E4003) | §12 |
| `e4004_unreachable_code` | unreachable code immediately after `return` | `entry_code_after_return.ybm` (E4004) | §12 |
| `e4005_naming_convention` | snake_case/PascalCase violations, and the exemption for generic type variables | `entry_naming_violations.ybm` (E4005) | §12 |

### 3.4 `samples/err/runtime/` (runtime abnormal termination, 8 directories, E6000-E6999)

| Path | Theme | Files included (expected code) | SPEC section verified |
|---|---|---|---|
| `e6001_out_of_range_access` | out-of-range access / access to a non-existent key | `entry_list_index_oob.ybm` (E6001), `entry_dict_missing_key.ybm` (E6001), `entry_slice_out_of_range.ybm` (E6001) | §7.4 |
| `e6002_zero_division` | integer division by zero (`/` and `%`) | `entry_int_div_by_zero.ybm` (E6002), `entry_int_mod_by_zero.ybm` (E6002) | §7.4 |
| `e6003_integer_overflow` | arithmetic overflow; an out-of-range `int(float)` conversion | `entry_arithmetic_overflow.ybm` (E6003), `entry_float_to_int_overflow.ybm` (E6003) | §7.4 |
| `e6004_assert_failure` | an `assert` failure | `entry_assert_fails.ybm` (E6004) | §7.5 |
| `e6005_e6006_toplevel_question_propagation` | top-level `?` propagating Err/None | `entry_toplevel_err_propagation.ybm` (E6005), `entry_toplevel_none_propagation.ybm` (E6006) | §7.2 |
| `e6007_unwrap_failure` | a failure of `Result.unwrap()`/`Option.unwrap()` | `entry_result_unwrap_on_err.ybm` (E6007), `entry_option_unwrap_on_none.ybm` (E6007) | §7.4 |
| `e6008_stack_overflow` | a stack overflow from deep recursion | `entry_unbounded_recursion.ybm` (E6008) | §7.4 |
| `par_panic_aborts_immediately` | a panic inside `par` aborts immediately without waiting for all branches to finish (D-ERR-06, an exception to the fail-fast rule) | `entry_par_branch_panics.ybm` (equivalent to E6002 or E6003) | §9 / §7.4 |

### 3.5 `samples/err/cli/` (pre-launch CLI errors, 1 directory, E9000-E9999)

| Path | Theme | Files included (expected code) | SPEC section verified |
|---|---|---|---|
| `file_and_extension_errors` | file not found; invalid extension | `entry_wrong_extension.notybm` (E9002). For E9001, `expected.toml` simply specifies a nonexistent file name directly (no actual file is provided) | §1 |

### 3.6 `samples/fmt/` (fmt verification, 10 directories)

| Path | Theme | Files included | SPEC section verified |
|---|---|---|---|
| `operator_and_punctuation_spacing` | spacing rules for binary operators, commas, the type-annotation colon, unary operators, and `not` (D-FMT-01) | `sample.in.ybm`, `sample.out.ybm` | §12 |
| `string_quote_and_escape_preservation` | double-quote normalization; preserving `\"` escapes (D-FMT-02) | `sample.in.ybm`, `sample.out.ybm` | §12 |
| `comment_spacing_normalization` | forcing a space immediately after `#`/`##` (D-FMT-03) | `sample.in.ybm`, `sample.out.ybm` | §2 / §12 |
| `pipe_multiline_splitting` | splitting a pipe with 2 or more stages into one stage per line (D-FMT-04) | `sample.in.ybm`, `sample.out.ybm` | §6.3 / §12 |
| `blank_lines_and_trailing_newline` | collapsing consecutive blank lines to one; normalizing the trailing newline (D-SYN-02) | `sample.in.ybm`, `sample.out.ybm` | §2 / §12 |
| `trailing_comma_normalization` | adding a trailing comma to multi-line literals/calls; removing it on a single line (D-TYPE-02) | `sample.in.ybm`, `sample.out.ybm` | §3.2 / §12 |
| `collection_and_call_arg_line_splitting` | deciding "keep on one line vs. expand to one element per line" for list/dict/set/tuple literals and function-call argument lists, based solely on the syntactic signal of whether the input source has a line break (D-FMT-05); remaining idempotent after expansion (fmt after fmt = fmt) | `sample.in.ybm`, `sample.out.ybm` | §3.2 / §12 |
| `method_chain_continuation_indent` | normalizing parenthesis-less line continuation to the base line + 4 (D-SYN-05) | `sample.in.ybm`, `sample.out.ybm` | §6.2 / §12 |
| `idempotency_full_program_and_check_flag_position` | verifying fmt-after-fmt = fmt idempotency in a practical sample involving multiple rules, and the `--apply` flag position (equivalent whether given before or after, D-CLI-02) | `sample.in.ybm`, `sample.out.ybm` | §1 / §12 (overall) |
| `doc_comment_fence_unaffected` | code inside a language-tag-less fence block within a `##` doc comment is exempt from fmt (the in-place rewrite by `ybm check --apply`), and is preserved byte-identical in its non-canonical form even after formatting (D-FMT-06). Contrasted with ordinary code outside the fence, which is formatted normally in the same `check` run | `sample.in.ybm`, `sample.out.ybm` | §12 / §13 |

### 3.7 `samples/doctest/` (doc-test mechanism, 6 directories)

| Path | Theme | Files included | SPEC section verified |
|---|---|---|---|
| `passing_multiple_blocks_same_declaration` | tallies multiple fence blocks within a single doc comment as individual test cases; blocks with a language tag (e.g. ` ```json `) are ignored (D-DOC-01/02) | `entry_main.ybm` | §13 |
| `failing_assert_and_report_line` | immediate termination of a block on `assert` failure; fail reporting; no effect on other blocks (D-DOC-04) | `entry_main.ybm` | §13 / §7.5 |
| `target_declarations_struct_enum_const` | a `##` immediately preceding any of `def`/`struct`/`enum`/a module constant is a valid target (D-DOC-03) | `entry_main.ybm` | §13 |
| `scope_is_whole_file_incl_module` | the scope of doc tests = the entry plus every declaration in same-level modules (D-MOD-04) | `entry_main.ybm`, `mod_helpers.ybm` | §13 / §10 |
| `err_result_propagation_in_block` | a failure via top-level `?` Err propagation inside a block (a failure path other than assert) | `entry_main.ybm` | §13 / §7.2 |
| `check_vs_test_command_difference` | the contrast that `ybm check` only type-checks doc tests without running them, while `ybm test` runs them and can catch runtime bugs detectable only by running | `entry_main.ybm` | §1 / §13 |

---

## 4. SPEC Section -> Sample Reverse-Lookup Coverage Matrix

Lists every subsection of SPEC §1-§15 in the left column, with its corresponding sample directories in the right column (no blanks). Paths omit the `samples/` prefix.

| SPEC Section | Corresponding Samples |
|---|---|
| §1 CLI | `ok/1_cli_three_subcommands`, `err/static/1_full_diagnostic_report_ordering`, `err/cli/file_and_extension_errors`, `fmt/idempotency_full_program_and_check_flag_position`, `doctest/check_vs_test_command_difference` |
| §2 Lexical | `ok/2_lexical_basics`, `err/static/2_lexical_errors`, `err/static/2_syntax_errors`, `fmt/comment_spacing_normalization`, `fmt/blank_lines_and_trailing_newline` |
| §3.1 Primitives | `ok/3-1_primitives` |
| §3.2 Collections | `ok/3-2_collections`, `err/static/3-2_collection_type_errors`, `fmt/trailing_comma_normalization` |
| §3.3 stdlib Types | `ok/3-3_stdlib_types` |
| §3.4 Scope of Mandatory Type Annotations | `ok/3-4_type_annotations_and_inference`, `err/static/3-4_type_annotation_and_inference_errors` |
| §3.5 struct/enum | `ok/3-5_struct_and_enum` |
| §3.6 Generics | `ok/3-6_generics`, `err/static/3-6_generic_operator_misuse` |
| §4 Variables and Mutability | `ok/4_mutability`, `err/static/4_mutability_errors` |
| §5 Functions | `ok/5_functions_hoisting_and_toplevel_order`, `ok/5b_return_implicit_ok_some_wrap` |
| §5.1 Lambdas | `ok/5-1_lambdas` |
| §6.1 Expression-Oriented | `ok/6-1_expression_oriented_if_match`, `err/static/2_syntax_errors` (elif unsupported), `err/static/6-1_match_and_branch_errors`, `err/static/6-1_non_enum_match_missing_wildcard` |
| §6.2 Iterators | `ok/6-2_iterators`, `fmt/method_chain_continuation_indent` |
| §6.3 Pipe | `ok/6-3_pipe_operator`, `ok/6-3_operator_precedence_mixed_expression`, `err/static/2_syntax_errors` (missing `_`), `err/static/6-4_operator_type_errors` (operator precedence pitfall), `fmt/pipe_multiline_splitting` |
| §6.4 Strings | `ok/6-4_strings`, `err/static/6-4_operator_type_errors` |
| §7.1 The Error Type | `ok/7-1_error_type` |
| §7.2 The `?` Operator | `ok/7-2_question_operator`, `ok/5b_return_implicit_ok_some_wrap`, `err/static/7-2_question_operator_type_errors`, `err/runtime/e6005_e6006_toplevel_question_propagation`, `err/static/9_par_branch_bare_question_operator`, `doctest/err_result_propagation_in_block` |
| §7.3 Prohibition on Ignoring Result | `ok/7-3_result_must_be_used`, `err/static/7-3_unused_result_errors` |
| §7.4 Eliminating panics | `ok/7-4_safe_apis`, `err/runtime/e6001_out_of_range_access`, `err/runtime/e6002_zero_division`, `err/runtime/e6003_integer_overflow`, `err/runtime/e6007_unwrap_failure`, `err/runtime/e6008_stack_overflow`, `err/runtime/par_panic_aborts_immediately` |
| §7.5 assert | `ok/7-5_assert`, `err/runtime/e6004_assert_failure`, `doctest/failing_assert_and_report_line` |
| §8 The Effect System | `ok/8_effects`, `err/static/8_effect_errors` |
| §9 Concurrent Execution | `ok/9_concurrency_par`, `err/runtime/par_panic_aborts_immediately`, `err/static/9_par_branch_bare_question_operator` |
| §10 Modules | `ok/10a_module_shared_by_two_entries`, `ok/10b_independent_entries_same_directory`, `ok/10c_module_constants_and_cross_reference`, `err/static/10a_module_name_collision`, `err/static/10b_module_directive_malformed`, `err/static/10c_module_toplevel_statement_cascade`, `err/static/10d_entry_self_module_directive`, `doctest/scope_is_whole_file_incl_module` |
| §11.1 codec | `ok/11-1_codec_json_yaml_toml_csv`, `err/static/11-1_toml_encode_root_type_error` |
| §11.2 Module List (fs/http/env/proc/time/rand/regex/math) | `ok/11-2_fs`, `ok/11-2_http`, `ok/11-2_env`, `ok/11-2_proc`, `ok/11-2_time`, `ok/11-2_rand`, `ok/11-2_regex`, `ok/11-2_math` |
| §11.3 Built-in Functions | `ok/11-3_builtins_print_eprint_assert` |
| §12 fmt/lint | `ok/12_fmt_lint_clean_baseline`, `fmt/*` (all 10 directories), `err/lint/*` (all 5 directories) |
| §13 Doc Tests | `doctest/*` (all 6 directories), `fmt/doc_comment_fence_unaffected` |
| §14 Memory Model | `ok/14_memory_model_value_semantics` |
| §15 Sample | `ok/15_end_to_end_showcase` |

---

## 5. Estimated Scale and Concerns

### 5.1 Scale Estimate

- Directory count (leaf theme directories): `ok` 40 + `err/static` 19 + `err/lint` 5 + `err/runtime` 8 + `err/cli` 1 + `fmt` 10 + `doctest` 6 = **89 directories in total** (not counting structural grouping folders such as `ok/`, `err/`, `err/static/`).
- File count: including body files such as `.ybm`/`.in.ybm`/`.out.ybm`/`stdin_*.txt` plus the `expected.toml` required in every directory, **an estimated 235-245 files**. A rough breakdown: the `ok` group is about 100 (including extra files for D-TYPE-17/D-OP-01/D-TYPE-18 positive examples/D-SYN-11 positive examples/mutual recursion/generic struct & enum/round-trip conversions/transitive effect propagation/codec round-trips/every Result & Option method, etc.), `err/static` is about 64 (including extra files for D-PAR-03/D-TYPE-18/D-SYN-11 negative examples/D-STDPOL-09), `err/lint` is about 10, `err/runtime` is about 22, `err/cli` is about 2, `fmt` is about 28 (in/out pairs, including the extra ones for D-FMT-05/D-FMT-06), and `doctest` is about 13.

### 5.2 Coverage Concerns

1. **Runtime execution verification of `net`/`proc` depends on the mock entities existing**: under the policy in §1.4/1.4.1/1.4.2, `http`/`proc`-related samples are verified through to execution (`run`), but this presupposes that the Rust-side test harness provides entities matching this document's contract tables (the mock HTTP server's endpoint list, the `YABUMI_TEST_PROC_BIN` fixture's argument spec). While the harness-side implementation is not yet started, or is inconsistent with the contract, the harness will skip cases with `requires_env`, effectively regressing coverage to the equivalent of type/effect checking (`ybm check`). Also, since `proc.run` has no API for supplying stdin to the child process, verification of the `cat` subcommand is limited to "behavior with empty/closed stdin" (§1.4.2).
2. **The genuinely non-deterministic path of `time.now()`/`rand` (non-degenerate ranges) is not verified**: no sample is designed to actively confirm that `rand.int`/`rand.float`/`rand.bool` (when not using a degenerate range) or `time.now()` produce "the right type, but a different value every time" (determinism was prioritized instead). Statistical tests that verify value distributions or non-determinism itself are out of scope.
3. **There is no sample confirming that syntax unsupported in v1, such as match guards or OR patterns, is indeed unsupported**: features explicitly marked as non-goals in D-SYN-06 (guarded patterns, OR patterns) are not covered by a sample confirming "writing this produces a syntax error." If needed, this can be absorbed by adding extra entries to `err/static/2_syntax_errors` (extendable within the current directory structure as-is).
4. **In cases involving `ybm check --apply` (fmt in-place), formatting could unintentionally affect the run result**: samples using `cmd = "check"` in `expected.toml` actually invoke the explicit `--apply` rewrite. On the `ok/` side, it is necessary to strictly follow the operating rule of preparing source that needs no reformatting (or whose meaning is unchanged after reformatting); this is spelled out in this document's convention (§1.3), but remains a risk of oversight during the implementation phase.
5. **Diagnostic-code coverage is 100% against the settled table in D-DIAG-02, but future codes not listed in D-DIAG-02 (e.g. post-v1 extensions) are naturally out of scope.** This document's coverage matrix is a snapshot as of the current SPEC/DECISIONS; if either document is updated, this document and the directory listing under `err/static` must be kept in sync with it.
