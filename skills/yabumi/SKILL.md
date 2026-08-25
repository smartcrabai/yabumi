---
name: yabumi
description: >-
  Write, edit, review, debug, and migrate Yabumi (.ybm) scripts for Agent Skills.
  Use whenever the user mentions Yabumi, ybm, a .ybm file, asks for a script in a
  Yabumi project, or wants to replace Bash/Python with Yabumi's zero-dependency
  skill scripting language.
compatibility: Requires the ybm executable, or a Yabumi source checkout with Cargo.
---

# Yabumi

Write the smallest readable `.ybm` program that satisfies the request and passes `ybm check`. Assume no prior Yabumi knowledge; use the bundled references instead of analogy with Python, Rust, or shell.

## Required references

Read [references/language.md](references/language.md) before writing, editing, reviewing, or debugging any Yabumi code. It defines the complete authoring grammar, unsupported constructs, effects, errors, modules, concurrency, formatter, and lint rules.

Read the relevant sections of [references/stdlib.md](references/stdlib.md) before using any type method or standard-library API. It contains exact signatures and compiler-verified file, JSON, and concurrent HTTP patterns. Never invent an API that is absent from the reference.

## Workflow

1. Inspect the target Agent Skill and nearby `.ybm` files. Identify inputs, outputs, data types, side effects, failure behavior, and permission footprint.
2. Read `references/language.md`, then the needed `references/stdlib.md` sections. Reuse a verified pattern when one matches.
3. Keep one entry file by default. Add sibling `module` files only for declarations genuinely shared by multiple entries.
4. Make mutability, effects, concurrency, and failures explicit. Use safe APIs when input controls indexes, keys, arithmetic, parsing, processes, or I/O.
5. Validate, repair the first diagnostic's root cause, and repeat until clean.

## Critical traps

- Blocks use four spaces and no trailing colon.
- `if` always needs `else`; `for`, `while`, `loop`, `break`, `continue`, and `elif` do not exist.
- Functions require parameter and return types; effectful functions require the exact `uses {...}` set.
- Never ignore a `Result`.
- `fs.write`, `fs.append`, and `fs.remove` return `Option[Error]` with `None` meaning success. Never apply `?` directly; use the conversion pattern in `references/stdlib.md`.
- `print`/`eprint` accept primitives only. Encode or format compound values explicitly.
- Lint warnings fail `ybm check`, including unused bindings/functions and shadowing.

## Validation

Use `ybm` from `PATH`. In a Yabumi source checkout, use `target/release/ybm`, then `target/debug/ybm`; if neither exists, run `cargo build --release`.

For every changed entry:

1. Run `ybm check path/to/script.ybm`. It type-checks, checks fmt without writing, checks doc-test blocks, and lints.
2. If the only failure is a fmt diff and rewriting is authorized, run `ybm check --apply path/to/script.ybm`, then rerun the read-only `ybm check`.
3. Fix every diagnostic and rerun the same command.
4. Run `ybm test path/to/script.ybm` when it contains doc tests.
5. Run `ybm path/to/script.ybm` with representative input when effects are safe and authorized.

`ybm check --apply` is the only check-mode command that rewrites fmt. Diagnostics have the stable format `file:line:col [E0000] message`.

## Delivery

Return changed `.ybm` paths and the exact successful check, test, and runtime commands actually executed. Never claim runtime verification when only static checking ran.
