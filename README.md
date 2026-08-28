# yabumi

Yabumi is a single-file, self-contained scripting language for Agent Skills (scripts bundled with `SKILL.md`). It is designed for LLMs to write as an alternative to bash / python.

Design priorities, in order: **zero-dependency distribution > machine-readable errors > write-it-right-the-first-time > permission auditability**. The overarching philosophy is "**what you see is what happens**" -- eliminating implicit behavior and making concurrency, mutability, and error propagation all visible in the syntax. See [`SPEC.md`](./SPEC.md) for the full language specification.

## Install

### Homebrew

```sh
brew install smartcrabai/tap/yabumi
```

### Shell installer

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/smartcrabai/yabumi/releases/latest/download/yabumi-installer.sh | sh
```
### Agent Skill

This repository includes the `yabumi` skill under `skills/yabumi/`.

#### npx skills

```sh
npx skills add smartcrabai/yabumi --skill yabumi
```

#### GitHub CLI

Requires GitHub CLI 2.90.0 or later.

```sh
gh skill install smartcrabai/yabumi yabumi
```

## Build

```sh
cargo build --release
# produces ./target/release/ybm
```

## Release

One-time setup: add a `HOMEBREW_TAP_TOKEN` repository secret with write access to
`smartcrabai/homebrew-tap`.

For each release, update the version in `Cargo.toml`, merge it into `main`, then
push the matching tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow builds archives for all configured targets, creates the
GitHub Release and shell installer, then updates the Homebrew tap.

## CLI

There are only four subcommands.

| Command | Behavior | Main exit codes |
|---|---|---|
| `ybm <file>` | Type-checks, then runs only on success | 0 = success / 1 = type error or runtime `Err` termination |
| `ybm check <file>` | Type-check + read-only fmt diff check + lint. Also type-checks doc-test blocks (without running them) | 0 = clean / 1 = fmt diff, type error, or lint warning |
| `ybm test <file>` | Runs doc tests | 0 = all pass / 1 = type error or doc-test failure |
| `ybm lsp` | Starts the Language Server Protocol server over stdio | 0 = clean shutdown or EOF / 1 = transport error or unclean exit |

- `ybm check --apply <file>`: rewrites fmt in place; exits 1 if type-check or lint fails
- `ybm lsp` takes no additional command-line arguments; see [`docs/LSP.md`](./docs/LSP.md) for protocol features and editor setup
- Diagnostic format: `file:line:col [E0000] message`. Error codes are stable and machine-readable

```sh
$ ybm check samples/err/static/4_mutability_errors/entry_reassign_immutable.ybm
samples/err/static/4_mutability_errors/entry_reassign_immutable.ybm:5:1 [E3001] 'x' cannot be reassigned because it is not a var binding (D-MUT-01-03)
$ echo $?
1
```

## Language Features

All examples below are quoted from real files under `samples/` and have been verified to work with `./target/release/ybm`.

### Immutable by Default

Bindings without `var` cannot be reassigned. Only variables you want to be mutable need an explicit `var` ([`samples/ok/4_mutability/entry_main.ybm`](./samples/ok/4_mutability/entry_main.ybm)).

```
x = 5
assert(x == 5)

var y = 5
y = 6
assert(y == 6)
```

Reassigning a binding without `var` is a type-check error (E3001) ([`samples/err/static/4_mutability_errors/entry_reassign_immutable.ybm`](./samples/err/static/4_mutability_errors/entry_reassign_immutable.ybm)).

```
x = 5
x = 6
print(x)
```

```
$ ybm check samples/err/static/4_mutability_errors/entry_reassign_immutable.ybm
samples/err/static/4_mutability_errors/entry_reassign_immutable.ybm:5:1 [E3001] 'x' cannot be reassigned because it is not a var binding (D-MUT-01-03)
```

### Effect System

Functions declare what side effects they may cause (`fs`/`net`/`env`/`proc`/`time`/`rand`, etc.) via `uses {...}`. If the declaration doesn't match the effects of the functions actually called, it's a static error (excerpt from [`samples/ok/8_effects/entry_main.ybm`](./samples/ok/8_effects/entry_main.ybm)).

```
def write_note(path: str, content: str): Option[Error] uses {fs}
    return fs.write(path, content)

def try_connect(url: str): Result[int, Error] uses {net}
    resp = http.get(url)?
    return Ok(resp.status)
```

### Error Propagation with `?`

Appending `?` to an expression that returns `Result[T, E]` / `Option[T]` propagates failure as an early return (excerpt from [`samples/ok/7-2_question_operator/entry_main.ybm`](./samples/ok/7-2_question_operator/entry_main.ybm)).

```
def parse_and_double(s: str): Result[int, Error]
    n = s.parse_int()?
    return n * 2

r = parse_and_double("21")
assert(r.is_ok(), "parse_and_double(\"21\") should succeed")
assert(r.unwrap() == 42, "21 parsed then doubled == 42")
```

### Explicit Concurrency with `par`

`par [...]` / `par (...)` run multiple expressions concurrently. Results come back in the order they're written, not the order of completion, and a panic partway through immediately terminates the whole thing without waiting for the other branches ([`samples/ok/9_concurrency_par/entry_par_fixed_arity.ybm`](./samples/ok/9_concurrency_par/entry_par_fixed_arity.ybm)).

```
def double(x: int): int
    return x * 2

def shout(s: str): str
    return s.to_upper()

results_list = par [double(3), double(4), double(5)]
assert(results_list == [6, 8, 10])

results_tuple = par (double(7), shout("hi"))
assert(results_tuple.0 == 14)
assert(results_tuple.1 == "HI")
```

### Doc Tests

A \`\`\` fence inside a `##` comment immediately preceding a declaration becomes a test case as-is, and is run by `ybm test`. Multiple fences within a single doc comment are tallied for pass/fail independently ([`samples/doctest/passing_multiple_blocks_same_declaration/entry_main.ybm`](./samples/doctest/passing_multiple_blocks_same_declaration/entry_main.ybm)).

````
## Adds two ints.
##
## ```
## assert(add(1, 2) == 3)
## ```
##
## Also verify with another addition pattern.
##
## ```
## assert(add(10, 20) == 30)
## ```
def add(a: int, b: int): int
    return a + b
````

```
$ ybm test entry_main.ybm
doctest: 2 passed, 0 failed
```

## Repository Structure

| Path | Contents |
|---|---|
| [`SPEC.md`](./SPEC.md) | Canonical language specification |
| [`docs/DECISIONS.md`](./docs/DECISIONS.md) | Record of decisions (decision IDs D-\*\*\*) resolving details left unspecified in the spec |
| [`docs/STDLIB.md`](./docs/STDLIB.md) | Standard library (stdlib) reference |
| [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) | Interpreter implementation design (module structure, pipeline, implementation history) |
| [`docs/LSP.md`](./docs/LSP.md) | Language Server Protocol features and editor setup |
| `samples/` | Sample collection that doubles as acceptance tests (`ok`/`err`/`fmt`/`doctest`, 89 directories, 254 files) |
| `src/` | The implementation itself (lexer -> parser -> module_resolve -> type checking -> effect checking -> lint/fmt -> evaluator/LSP server) |

## Tests

```sh
# Run all unit tests + acceptance tests (254 files under samples/)
cargo test --all-features

# lint / format check
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```
