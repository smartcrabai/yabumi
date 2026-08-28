# Yabumi Implementation Architecture Design Document (ARCHITECTURE)

This document is subordinate to `SPEC.md` (the canonical spec), `docs/DECISIONS.md` (the implementation-detail decision record), `docs/STDLIB.md` (the standard library reference), and `docs/SAMPLES_PLAN.md` (the sample design document). **Where this document conflicts with those four documents, those four documents take precedence.** This document finalizes the design for implementing the Rust-based interpreter `ybm`, and breaks the work down to a granularity fine enough for subsequent implementation phases to proceed in parallel.

This document does not edit SPEC.md. In the process of translating the contents of SPEC.md/DECISIONS.md/STDLIB.md/SAMPLES_PLAN.md into an implementable granularity, implementation-level judgment calls sometimes arise that those four documents do not explicitly settle (e.g., the ordering rule for cross-file diagnostics, the information-passing mechanism between phases, the Rust-side type representation). This document settles those. Such judgments are marked in the text as "**Decision made in this document**".

---

## Table of Contents

1. Selection of dependency crates
2. Module structure
3. Core data structures
4. Pipeline
5. Difficult points and countermeasures
6. Testing strategy
7. Implementation phase breakdown
8. Responses to the critique

---
## 1. Selection of dependency crates

### 1.1 Summary

| Crate | Decision | Purpose | features |
|---|---|---|---|
| `regex` | Adopted | Implementation of the `regex` namespace | Default (`std` + `perf` + the `unicode-*` set) |
| `ureq` | Adopted | Implementation of the `http` namespace (including TLS) | `default-features = false`, `["rustls", "gzip"]` |
| `indexmap` | Adopted | Insertion-order-preserving implementation of `dict[K,V]` / `set[T]` | Default (additional features such as `serde` left disabled) |
| `tokio` | **Removed** | (Originally the only dependency present in `main.rs`) | — |
| `serde` family | Not adopted | codec decode/encode | Hand-rolled per owner's ruling |
| `chrono` / `time` | Not adopted | `time` namespace | Hand-rolled |
| `rand` / `getrandom` | Not adopted | `rand` namespace | Hand-rolled |
| `clap` | Not adopted | CLI argument parsing | Hand-rolled |
| `thiserror` / `anyhow` | Not adopted | Internal error types | Hand-rolled |

The final set of runtime crate dependencies is just three: **`regex` + `ureq` (plus its rustls-family transitive dependencies) + `indexmap`**. The `[dev-dependencies]` used only at development time (for the test harness) are handled separately in §6 — this section covers only the dependencies that ship in the release binary.

The rationale for each crate is given individually below.

### 1.2 regex — Adopted

**Why it's needed**: The `regex` namespace in SPEC §11.2 (`is_match`/`find`/`find_all`/`replace`/`replace_all`/`captures`) is, per the owner's ruling, not to be hand-implemented ("do not hand-roll regular expressions — use a crate").

**Alternatives considered and rejected**:

| Candidate | Reason for rejection |
|---|---|
| `fancy-regex` | Has a backtracking implementation supporting even backreferences and lookaround assertions, but the STDLIB.md API (only `is_match`/`find`/`find_all`/`replace(_all)`/`captures`, with no mention of named captures) doesn't require those features. A backtracking engine can go exponential on patterns like `(a+)+b`, so its DoS resistance against an LLM unintentionally generating a pathological pattern is worse than `regex`'s (which guarantees linear time via a finite automaton). |
| `pcre2` (FFI, `pcre2-sys`) | Requires dynamically/statically linking against the C library libpcre2, meaning a C toolchain must be provisioned for every cross-compilation target (`aarch64-unknown-linux-gnu`, `*-pc-windows-msvc`, etc. — see `workspace.metadata.dist.targets` in Cargo.toml). This undermines the single-binary portability that "zero-dependency distribution" aims for. |
| Hand-rolled implementation | Rejected per the owner's ruling (this document does not reopen that decision). |

**features**: Keep `default-features` (the `std` + `perf` + `unicode-*` set). The Unicode data tables in `regex-syntax` do affect size, but given SPEC's use case (LLM-generated scripts handling arbitrary UTF-8 strings — including Japanese text and emoji — with `\w`/`\d`/`\s` and case handling), correctness of Unicode support takes priority. SPEC's design priority order is "zero-dependency distribution (i.e., distributable as a single binary) > machine-readable errors > getting it right on the first try > permission auditing," and the absolute binary size itself does not appear in that priority list — here, "zero-dependency distribution" means no install step and a self-contained single binary, not shaving off kilobytes. So an optimization like `default-features = false` that strips the Unicode tables would sacrifice correctness ("getting it right on the first try") for no offsetting benefit, and is therefore rejected.

**Scale of transitive dependencies**: `regex` → `regex-automata`, `regex-syntax`, `aho-corasick`, `memchr`. All of these are sibling crates maintained by the `regex` project itself — pure computation libraries that require no external C library, network I/O, or file I/O.

### 1.3 HTTP client — ureq + rustls adopted

**Why it's needed**: The `http` namespace in SPEC §11.2 requires TLS (`https://`). Hand-implementing TLS (certificate verification, handshake) is unreasonable both in implementation cost and security risk, and the owner's ruling has already settled this: "HTTP requires TLS, so it cannot be hand-rolled — use a crate."

**Alternatives considered and rejected** (compared along three axes: single-binary distribution, build time, and rustls integration):

| Candidate | Single binary / distribution | Build time | rustls integration | Overall verdict |
|---|---|---|---|---|
| **ureq** (adopted) | Synchronous API. Bundles rustls by default (TLS provider `ring`) + WebPKI root certificates + gzip. Requires no C library such as OpenSSL, so cross-compilation goes through cleanly on all five targets cargo-dist generates (macOS arm64, Linux x86_64/arm64, Windows x86_64/arm64). | No async runtime (tokio/hyper) required, so the dependency tree is thin and few crates need compiling. | rustls by default. No need to switch to native-tls etc., and we deliberately don't (to avoid pulling in OpenSSL's OS-dependent baggage). | **Adopted**. The synchronous API meshes naturally with the std::thread worker-pool design (§2, §5) built without tokio, which is removed in §1.4. |
| reqwest | Presupposes an async API built on hyper + tokio. TLS can default to rustls, but it's unusable without an async runtime. | Pulls in the tokio runtime, hyper, h2 (HTTP/2), etc. as transitive dependencies, substantially thickening the dependency tree. Build time is noticeably longer. | The rustls integration itself is fine, but using it requires pulling back the entire tokio runtime just for that, which directly contradicts the decision to remove tokio in §1.4. | Rejected. The benefit of being async (handling many concurrent connections without adding threads) doesn't pay off at the concurrency scale `par_map`/`par_each` cap out at with a thread pool (the shell-script-replacement scale SPEC targets). |
| minreq | Has the advantage of a minimal dependency footprint (rustls usable as an option), but support for connection queuing, redirect following, and chunked transfer-encoding is thin, and maintenance activity is also less active than ureq/reqwest. ureq is also more mature on the detailed timeout-control API that STDLIB.md's `http.request` requires, such as `HttpOptions.timeout_ms`. | Good (thin dependencies). | Has a rustls feature, but its integration track record and documentation are thinner than ureq's. | Rejected. In keeping with the "prioritize machine-readable errors" philosophy, we prioritize a library whose behavior around HTTP-layer edge cases (timeouts, redirects, chunked responses) is well battle-tested. |
| isahc | An FFI binding to libcurl (a C library). | Adds a vendored libcurl build, requiring a C toolchain for every cross-compilation environment. | libcurl itself can select OpenSSL/rustls/schannel etc., but pulling in a C dependency at all breaks the single-binary-distribution premise. | Rejected. Same reason as pcre2 in §1.2 (cross-compilation complexity from a C dependency). |
| std::net + hand-rolled TLS | Would effectively mean hand-implementing TLS 1.2/1.3 handshaking, certificate verification, and X.509 parsing ourselves — an enormous security liability. | — | — | Rejected. The owner's ruling — "HTTP requires TLS, so it cannot be hand-rolled" — explicitly rules this option out. |

**features**: Explicitly set `default-features = false, features = ["rustls", "gzip"]` (per ureq's major version 3 documentation as checked as of 2026, the default features — rustls, gzip, and WebPKI root certificates enabled by default — are pinned explicitly in Cargo.toml as well, so we don't depend on upstream default changes). Additional features such as `json` (serde-based convenience methods), `cookies`, `socks-proxy`, and `multipart` are not enabled at all — JSON uses our own codec, and SPEC makes no mention of cookies, SOCKS, or multipart.

**Scale of transitive dependencies**: `ureq` → `rustls` (plus one TLS provider, either `ring` or `aws-lc-rs`), `webpki-roots` (an embedded root certificate store), and roughly the `http` crate (header/method types). All are pure-Rust implementations independent of a C toolchain. tokio is not pulled in at all (ureq has a synchronous API).

### 1.4 tokio — Removed

**Conclusion**: Remove the sole remaining dependency in `main.rs`, `tokio = { features = ["rt", "macros"] }`, from Cargo.toml.

**Rationale**:

SPEC §9's requirements break down into two parts.

1. "No syntactic async/await for I/O. Internally asynchronous, implicitly awaited at the call site (does not block the CPU)."
2. "Runtime: multi-threaded (CPU parallelism available)."

(1) is a **syntactic** constraint — "the user does not write async/await" — not a specification of **implementation means** — "the implementation must use a Rust async runtime." The intent behind "does not block the CPU" is that one I/O wait (e.g., waiting for an `http.get` response) must not halt the computation of **other** branches running concurrently in a `par_map`. Either of the following implementations satisfies this:

- (a) Cooperatively scheduling tasks on an async runtime such as tokio
- (b) Having each thread in an OS-thread worker pool directly call synchronous I/O (`ureq`'s blocking API, `std::fs`, `std::process::Command`)

This architecture adopts (b). `ureq`, settled on in §1.3, has a synchronous API, and `fs`/`proc` can likewise be adequately implemented with the synchronous APIs of `std::fs`/`std::process`. Because `par [f(), g()]` / `xs.par_map(f)` run each branch on a **separate OS thread** via the worker thread pool designed in §2/§5, even if one branch is blocked on `http.get`, the OS threads for the other branches keep making progress concurrently — "does not block the CPU" is achieved at the thread level. When `http.get` is called at the top level (non-`par` sequential execution) too, only the calling thread blocks, and there is no other Yabumi-side work that should otherwise be progressing concurrently (without an explicit `par` construct there is no concurrency — SPEC §9, "concurrency only via explicit constructs") — so there's no need for async here either.

Requirement (2), "multi-threaded," could also be achieved with tokio's multi-thread runtime, but design (b) satisfies it just as well with a worker pool built on `std::thread`.

Keeping tokio would be a clear losing trade: mixing async functions with synchronous stdlib calls (e.g., `std::fs::read`) creates the problem of blocking the async runtime's worker threads (avoidable via `spawn_blocking`, but that workaround code itself would then be needed for every I/O call). Since `ureq` has a synchronous API, it can't take advantage of tokio's non-blocking I/O resources (epoll/io_uring integration) at all, so the very reason to adopt tokio (handling many concurrent connections with few threads) never applies in the first place. All that would result is **a dependency on a runtime whose capabilities go completely unused**, worsening compile time, binary size, and code complexity (managing the boundary between async and synchronous functions) while gaining nothing.

For these reasons, tokio is removed, and `main.rs` consists solely of synchronous functions (the concrete thread layout is described in §4.5, §5.7, and §5.8).

### 1.5 indexmap — Adopted

**Why it's needed**: D-COL-01 requires that iteration order for `dict[K,V]`/`set[T]` always match insertion order, and `ok/3-2_collections/entry_edge_cases.ybm` even verifies the specific behavior "re-inserting the same key after `remove` moves it to the end." This exactly matches the behavior of the `indexmap` crate's `IndexMap::shift_remove` + `insert` (verified experimentally — see below).

**Alternatives considered and rejected**:

| Candidate | Reason for rejection |
|---|---|
| Hand-rolled implementation (`HashMap<K, usize>` + `Vec<Option<(K,V)>>`, etc.) | This would just be reimplementing from scratch the algorithm `indexmap` has run stably in production for years — "how to compact the structure after a removal" (shift-remove: shift everything after the removed position forward by one). Under the constraints "no `unwrap`/`expect`" and "clippy pedantic mandatory," hand-writing this kind of index arithmetic from scratch only increases our own risk of off-by-one bugs. Under the "zero-dependency distribution = a minimal set of crate dependencies is fine" policy, there's no reason to reinvent functionality a battle-tested implementation already provides. |
| A plain linear-scan `Vec<(K,V)>` | Insertion order is trivially preserved, but `get`/`contains_key`/`insert` become O(n). For SPEC's use case (small scripts) this may not matter in practice, but it diverges significantly from the expectation the names `dict`/`set` themselves create — "fast hash-based lookup" (D-TYPE-05's deliberate key constraints are themselves premised on a hash-based implementation). |

**features**: Left at defaults (features such as `serde` are not enabled — since the codec is hand-rolled, indexmap's serde integration isn't needed).

**Scale of transitive dependencies**: `indexmap` → `hashbrown` (an open-addressing hash table in the same lineage as std's internal `HashMap` implementation), `equivalent`. Both are minimal, widely used crates requiring no external linking.

### 1.6 Crates considered but not adopted (supplementary)

The following would be "nice to have" but, in keeping with the zero-dependency-distribution spirit (§1.1 summary table), a hand-rolled implementation was chosen instead. In every case the decision was made by this document (unlike regex/HTTP, which carry an owner's ruling).

- **The `serde` family**: Not reopened, per the owner's ruling (retaining the judgment that a hand-rolled implementation is more straightforward given its direct coupling to the dynamic `Value` type and to assignment-target-annotation-driven decoding).
- **`chrono` / `time`**: The formats `time.format`/`time.parse` require are limited to timezone-unaware, `strftime`-style fixed formats (e.g. `%Y-%m-%d %H:%M:%S`), and only UTC is handled (D-STDPOL-06 states "no dedicated type will be added," and the concept of a timezone doesn't exist in SPEC at all). Converting between epoch milliseconds and year/month/day/hour/minute/second is adequately handled by well-known integer arithmetic in the style of Howard Hinnant's `civil_from_days`/`days_from_civil` (Gregorian calendar, including leap-year handling), and requires no timezone database (IANA tzdata) whatsoever. We judged that adding `chrono` (which pulls in `num-traits` etc.) or the `time` crate is not warranted for functionality of this scale, and hand-implement it in `src/stdlib/time.rs` instead (implementation details are settled in Unit 14 of §7.2).
- **`rand` / `getrandom`**: `rand.*` does not require cryptographic security (SPEC §11.2 explicitly states the `crypto` namespace is out of scope). Mixing seeds from `std::time::SystemTime::now()`, `std::process::id()`, and a local variable's stack address (which varies run to run under ASLR), fed into a small hand-rolled xoshiro256**-family PRNG, satisfies SPEC's requirement — "deterministic within the degenerate interval, otherwise just needs to be type-correct." The `rand` crate itself pulls in an ecosystem of `rand_core`/`rand_chacha`/`zerocopy` etc. as dependencies, which is overkill for a requirement of this scale (implementation details are settled in Unit 14 of §7.2).
- **`clap`**: There are only four subcommands — `ybm <file>` / `ybm check <file> [--apply]` / `ybm test <file>` / `ybm lsp` — and only one flag, `--apply`. Hand-dispatching on `std::env::args()` is sufficient, and there's no reason to pull in `clap_builder` (and, if using `clap_derive`, `syn`/`quote`/`proc-macro2`).
- **`thiserror` / `anyhow`**: Rust-side internal errors (file I/O errors, TLS errors, regex compile errors, etc.) all need to be converted by each stdlib module into Yabumi's `Error` (`kind`/`message`/`cause`), and since the conversion target is itself a Yabumi-language type, `thiserror`'s `#[derive(Error)]` benefit (automatic `Display`/`std::error::Error` implementations) doesn't apply. A type-erased error like `anyhow::Error` is likewise unneeded, since the conversion target is always the fixed Yabumi `Error` type. The `Diagnostic`/`ErrorCode` designed in §3.1 is adequately handled by a hand-written `enum` + `impl fmt::Display`.

### 1.7 Finalized Cargo.toml (dependencies section)

```toml
[dependencies]
regex = "1"
ureq = { version = "3", default-features = false, features = ["rustls", "gzip"] }
indexmap = "2"
```

The `tokio` line is removed. `[dev-dependencies]` are settled in §6.3 (crates needed only by the sample acceptance-test harness).

---

## 2. Module structure

### 2.1 Directory tree

```
src/
  main.rs                        — Entry point. Grabs argv → spawns a thread with a dedicated stack size → calls the driver → ExitCode
  cli/
    mod.rs                       — Subcommand enumeration (Run/Check/Test/Lsp), bridges to the driver
    args.rs                      — Hand-written argv parsing (4 subcommands + --apply, position-independent)
  diagnostics/
    mod.rs                       — Diagnostic, DiagnosticBag, rendering (file:line:col [Exxxx] message)
    codes.rs                     — ErrorCode enum (all D-DIAG-02 codes)
    source_map.rs                — SourceFile, SourceMap, FileId, Position, Span
  lexer/
    mod.rs                       — Lexer core. Indent stack, bracket depth, lookahead for line continuation
    cursor.rs                    — Low-level character cursor (peek/bump in Unicode-scalar units)
    token.rs                     — Token, TokenKind
    fstring.rs                   — f-string scanning (brace depth, recursive lexing of the expr portion)
    comments.rs                  — Side-channel collection of comments/doc comments (shared input for fmt and doctest extraction)
  ast/
    mod.rs                       — Re-exports, NodeId definition
    expr.rs                      — Expr, ExprKind, Arg, PipeExpr, LambdaParam
    stmt.rs                      — Stmt, StmtKind, Block
    decl.rs                      — FunctionDecl, StructDecl, EnumDecl, EnumVariant, Param, SelfParam, DocComment, DocFence, Module, Item
    pattern.rs                   — Pattern, SubPattern, LiteralPat
    ty_ann.rs                    — TypeAnn, TypeAnnKind (syntactic type annotations; distinct from the Ty in §3)
  parser/
    mod.rs                       — Recursive-descent parser core, Parser struct, error-recovery policy
    expr.rs                      — Pratt-style expression parser (D-OP-01 precedence table)
    stmt.rs                      — Statement parser, if/match, blocks
    decl.rs                      — Declaration parser for def/struct/enum/constants
    pattern.rs                   — match pattern parser (D-SYN-06's nesting constraint enforced via types, §3.5)
    ty_ann.rs                    — Type annotation parser
    comment_attach.rs            — Attaches side-channel comments to the AST (leading/trailing) by matching line numbers
  module_resolve/
    mod.rs                       — Enumerates `.ybm` files in the same directory, determines module directives, builds the Program skeleton
    flat_namespace.rs             — Registers all declarations into a single flat namespace, detects E1001
    module_grammar.rs             — Detects E5001 (malformed directive) / E5002 (top-level executable statement inside a module file)
  types/
    mod.rs                       — Type definitions for Ty, EffectSet, NamespaceId (foundational types other phases depend on; minimal behavior)
    env.rs                       — TypeEnv (a scope chain holding references to parent scopes)
    infer.rs                     — Assignment-target-annotation-driven inference (D-TYPE-15/16), unify
    generics.rs                  — Type-variable substitution and unification for generic functions/structs/enums
    exhaustiveness.rs             — match exhaustiveness (enum coverage, D-TYPE-18's non-enum wildcard rule)
    mutability.rs                 — D-MUT-01–05 mutability checking (E3001). Performed in the same pass as expression type inference
    check_expr.rs                 — Core expression type-checking
    check_stmt.rs                 — Statement type-checking, block-value rules (D-SYN-11)
    check_decl.rs                 — Type-checking of declarations (def/struct/enum/const)
    resolutions.rs                — Side table mapping NodeId → resolved facts (§3.7)
  effects/
    mod.rs                        — Core effect-row inference (D-FUNC-03), detection of E2001/E2002, effect polymorphism for higher-order functions (§5.5, EFFECT-HOF-POLYMORPHISM)
  lint/
    mod.rs                        — Entry points for the 5 rules, shared walker
    unused_variable.rs            — E4001
    unused_function.rs            — E4002
    shadowing.rs                  — E4003
    unreachable.rs                — E4004
    naming.rs                     — E4005
  fmt/
    mod.rs                        — Driver that regenerates canonical text from the AST (plus attached comments)
    printer.rs                    — Formatting rules for each AST node (D-FMT-01–05)
    doc_fence.rs                  — Excludes code inside doc-comment fences from formatting (D-FMT-06)
  eval/
    mod.rs                        — Interpreter, entry point for sequential execution of top-level statements
    value.rs                      — Value, MapKey, StructInstance, EnumInstance, Closure, CallTarget, LambdaBody
    env.rs                        — Environment (Frame/Scope stack), Program (immutable globals)
    expr.rs                       — Expression evaluation, Flow
    stmt.rs                       — Statement evaluation
    call.rs                       — Function/method call convention, write-back of var self (chained Arc::make_mut)
    lvalue.rs                     — Assignment-target path resolution (implements D-MUT-03's root-variable tracking as a chain of Arc::make_mut)
    ops.rs                        — Arithmetic/comparison/equality operators (including overflow and divide-by-zero checks)
    panic.rs                      — The Abort type, shared constructors for E6001–E6008
  concurrency/
    mod.rs                        — Worker thread pool, implementation of par/par_map/par_each (immediate termination on panic, §5.8, PAR-ABORT-NOT-ACTUALLY-IMMEDIATE)
  stdlib/
    mod.rs                        — Namespace resolution table (dispatch from function/method names to implementations), stdlib-restricted overload resolution (D-STDPOL-01)
    prelude.rs                    — Pre-registration of the Result/Option/Error/Value type definitions, int/float/str conversion functions
    primitives.rs                  — Methods on int/float/bool/str
    collections.rs                  — Methods on list/dict/set/tuple (destructive methods go through lvalue.rs)
    result_option.rs                — Methods on Result[T,E]/Option[T]
    value_type.rs                    — Methods on the dynamic Value type
    math.rs                          — The math namespace (including checked_*)
    regexns.rs                       — The regex namespace (a wrapper around the regex crate)
    fs.rs                             — The fs namespace
    http.rs                           — The http namespace (a wrapper around ureq)
    envns.rs                          — The env namespace
    proc.rs                           — The proc namespace
    time.rs                           — The time namespace (hand-rolled calendar arithmetic)
    rand.rs                           — The rand namespace (hand-rolled PRNG)
    builtins.rs                       — print/eprint/assert
    codec/
      mod.rs                          — Shared decode/encode dispatch, building an intermediate representation from Ty
      json.rs                         — JSON decode/encode
      yaml.rs                         — YAML (safe subset) decode/encode
      toml.rs                         — TOML decode/encode (including D-STDPOL-09's root constraint)
      csv.rs                          — CSV decode/encode/decode_rows
  doctest/
    mod.rs                          — Execution of extracted `##`-fence results, building a standalone program, pass/fail tallying
  lsp/
    mod.rs                          — JSON-RPC/LSP server, document state, diagnostics, hover, definition, and formatting requests
    json.rs                         — Minimal JSON parser/serializer used by the protocol
    pos.rs                          — UTF-16/UTF-32 LSP position conversion
    query.rs                        — AST queries for hover and definition results
    transport.rs                    — Content-Length message framing
    uri.rs                          — File URI/path conversion
  driver.rs                         — Chains lex→parse→module_resolve→check→effects→lint→(fmt|eval|doctest|LSP), determines the exit code
```

### 2.2 Responsibilities by area (mapping to the required minimum list)

For each of the 8 areas required by the task, the responsible files in the tree above and their responsibilities are stated explicitly below.

**Diagnostics** (`diagnostics/`): Generation of the `file:line:col [Exxxx] message` format (`Diagnostic::render`); collection of all findings, with every phase taking a `&mut DiagnosticBag` to append to; ascending `file:line:col` sorting via `DiagnosticBag::into_sorted` (decision made in this document: when spanning multiple files, the file-path string's lexicographic order is the primary key, §4.4); exit-code determination in `driver.rs`.

**Lexing** (`lexer/`): Indent → INDENT/DEDENT conversion (§5.1), suppression of newlines inside brackets (D-SYN-04), f-strings (`fstring.rs`, §5.2), shebang stripping, detection of the module directive (at this stage this is only reduced to an `Module.is_module_directive` flag indicating "is the first line a module token" — the semantic checks for E5001/E5002 are performed later, in the parser and beyond).

**AST** (`ast/`): All node types shown in §3.4/3.5. Plain, behaviorless data — so that fmt can "reproduce the original syntax as-is," the resolved results from the type-checking phase are kept out of it (resolved results are split off into the `resolutions.rs` side table, §3.7).

**Parsing** (`parser/`): Recursive descent + Pratt-style expression-operator parsing based on D-OP-01's precedence table. D-SYN-06's pattern-nesting constraint is enforced at compile time via the type distinction between `Pattern`/`SubPattern` (§3.5). D-PAR-03 (bare `?` forbidden inside par branches) is implemented directly by the parser as a syntax-level flag (§5.6).

**Module resolution** (`module_resolve/`): Automatic inclusion of same-directory `.ybm` files (immediate directory only, D-MOD-01), building the flat namespace, name-collision detection (E1001), checking module-file grammar constraints (declarations only, E5002).

**Type checking** (`types/`): Nominal typing, unification for generics (type erasure, §3.8), assignment-target-annotation-driven inference (D-TYPE-16, §5.3), match exhaustiveness (including D-TYPE-18), unused-Result detection (D-ERR-03), mutability checking (D-MUT-01–05, E3001). Mutability checking runs in the same pass as type checking (rationale given in §4.4).

**Effect checking** (`effects/`): Effect-row inference (D-FUNC-03), propagation through higher-order functions, indirect-call detection (D-EFF-02). This is completed simply by reading the `Ty::Function{effects,..}` that type checking has already determined for each expression — the AST must be walked again, but type inference does not need to be redone.

**Lint** (`lint/`): The 5 rules (unused variable / unused function / shadowing / unreachable code / naming convention). All depend on resolved name information from after type checking, so they run only after type checking succeeds.

**fmt** (`fmt/`): Idempotent formatting (§5.7). Complete using only syntactic information, with no need for type checking, so it can be implemented and tested independently of type checking/effect checking/lint (the basis for the parallel implementation-unit split in §7).

**Evaluator** (`eval/`): Tree-walking, value semantics, Arc+CoW (§3.6).

**Concurrency** (`concurrency/`): Implementation of the various par constructs (§5.6).

**Standard library** (`stdlib/`): Type methods, namespace functions, codecs.

**Doc tests** (`doctest/`): Extraction, standalone execution, tallying (§5.10).

**CLI** (`cli/` + `driver.rs`): The 4 subcommands (`ybm <file>`, `ybm check`, `ybm test`, and `ybm lsp`) + `--apply` (§4). The LSP server uses the shared analysis front end and does not execute source files.

---

## 3. Core data structures

The code examples below are meant to convey the design intent precisely; some deformation at implementation time (adding `#[derive]`s, splitting up `impl` blocks, etc.) is acceptable. However, **the meaning of the fields and the design policy expressed by an enum's set of variants must not be changed**.

### 3.1 Span / SourceMap

```rust
/// 1-indexed. Columns are counted in Unicode scalar value units (so that D-COL-03's
/// "1 char = 1 element" philosophy, counted per char, also matches the column numbers
/// an editor displays. Counting in bytes would make the column number disagree with
/// the editor's on any line containing multi-byte characters — decision made in this document).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

/// Points to a location in some file. Because multiple files (the entry file plus
/// same-directory modules) are handled at once, this always carries a `FileId` in
/// addition to the start/end positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file: FileId,
    pub start: Position,
    pub end: Position, // exclusive
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

pub struct SourceFile {
    pub path: PathBuf,
    pub text: String,
    /// The starting byte offset of each line. Used to convert between Position and
    /// byte offset (e.g. when the 1-argument form of assert needs to extract and
    /// display a slice of the source text).
    line_starts: Vec<u32>,
}

/// Holds every file read in for a single `ybm` invocation (the entry file plus
/// any automatically included same-directory modules). Built only after all files
/// have been read, before lexing begins.
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn add(&mut self, path: PathBuf, text: String) -> FileId { /* .. */ }
    pub fn path(&self, id: FileId) -> &Path { /* .. */ }
    pub fn slice(&self, span: Span) -> &str { /* .. */ } // extracts original text for assert messages
}
```

### 3.2 ErrorCode / Diagnostic / DiagnosticBag

This directly transcribes D-DIAG-02's finalized table into a Rust enum. Why an enum rather than plain numbers: diagnostic codes form a closed, existing set that SPEC requires to be "stable and machine-readable," and using Rust's exhaustiveness checking (which warns on non-exhaustive `match` arms) lets us guarantee, at the code level, the invariant that "this phase emits only these codes."

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    // E0000-E0499: lexical
    TabCharacter,             // E0001
    UnterminatedString,       // E0002
    InvalidEscape,            // E0003
    InvalidNumberLiteral,     // E0004
    UnknownToken,             // E0005

    // E0500-E0999: syntax
    IndentMismatch,           // E0501
    UnexpectedToken,          // E0502
    PipePlaceholderMissing,   // E0503

    // E1000-E1999: type system
    DuplicateName,                    // E1001
    MissingParamAnnotation,           // E1002
    UninferableType,                  // E1003
    CollectionElementTypeMismatch,    // E1010
    DictKeyTypeNotAllowed,            // E1011
    SetElementTypeNotAllowed,         // E1012
    UnsupportedOperatorForTypeParam,  // E1013
    BranchTypeMismatch,               // E1020
    NonExhaustiveMatch,               // E1021
    UnusedResult,                     // E1040
    IntFloatMixed,                    // E1050
    UnorderableType,                  // E1051
    QuestionOperatorMismatch,         // E1060

    // E2000-E2999: effects
    ImpureCallInPureFunction, // E2001
    UndeclaredEffect,         // E2002

    // E3000-E3999: mutability
    ImmutableMutation,        // E3001

    // E4000-E4999: lint
    UnusedVariable,           // E4001
    UnusedFunction,           // E4002
    Shadowing,                // E4003
    UnreachableCode,          // E4004
    NamingConvention,         // E4005

    // E5000-E5999: modules
    ModuleDirectiveMalformed, // E5001
    ModuleTopLevelStatement,  // E5002

    // E6000-E6999: runtime abnormal termination
    IndexOutOfRange,          // E6001
    DivisionByZero,           // E6002
    IntegerOverflow,          // E6003
    AssertFailed,             // E6004
    TopLevelErrPropagation,   // E6005
    TopLevelNonePropagation,  // E6006
    UnwrapFailed,              // E6007
    StackOverflow,             // E6008

    // E9000-E9999: pre-CLI-startup
    FileNotFound,              // E9001
    InvalidExtension,          // E9002
}

impl ErrorCode {
    pub const fn numeric(self) -> u32 {
        use ErrorCode::*;
        match self {
            TabCharacter => 1, UnterminatedString => 2, InvalidEscape => 3,
            InvalidNumberLiteral => 4, UnknownToken => 5,
            IndentMismatch => 501, UnexpectedToken => 502, PipePlaceholderMissing => 503,
            DuplicateName => 1001, MissingParamAnnotation => 1002, UninferableType => 1003,
            CollectionElementTypeMismatch => 1010, DictKeyTypeNotAllowed => 1011,
            SetElementTypeNotAllowed => 1012, UnsupportedOperatorForTypeParam => 1013,
            BranchTypeMismatch => 1020, NonExhaustiveMatch => 1021, UnusedResult => 1040,
            IntFloatMixed => 1050, UnorderableType => 1051, QuestionOperatorMismatch => 1060,
            ImpureCallInPureFunction => 2001, UndeclaredEffect => 2002,
            ImmutableMutation => 3001,
            UnusedVariable => 4001, UnusedFunction => 4002, Shadowing => 4003,
            UnreachableCode => 4004, NamingConvention => 4005,
            ModuleDirectiveMalformed => 5001, ModuleTopLevelStatement => 5002,
            IndexOutOfRange => 6001, DivisionByZero => 6002, IntegerOverflow => 6003,
            AssertFailed => 6004, TopLevelErrPropagation => 6005, TopLevelNonePropagation => 6006,
            UnwrapFailed => 6007, StackOverflow => 6008,
            FileNotFound => 9001, InvalidExtension => 9002,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04}", self.numeric())
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: ErrorCode,
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    /// `file:line:col [Exxxx] message` (SPEC §1; D-ERR-05 requires the same format for panics too)
    pub fn render(&self, sources: &SourceMap) -> String {
        format!(
            "{}:{}:{} [{}] {}",
            sources.path(self.span.file).display(),
            self.span.start.line,
            self.span.start.col,
            self.code,
            self.message,
        )
    }
}

/// The diagnostic container shared by all phases. Implements D-CLI-03 (collect everything, sort ascending).
#[derive(Default)]
pub struct DiagnosticBag {
    diagnostics: Vec<Diagnostic>,
}

impl DiagnosticBag {
    pub fn push(&mut self, d: Diagnostic) { self.diagnostics.push(d); }
    pub fn has_any(&self) -> bool { !self.diagnostics.is_empty() }

    /// Decision made in this document: SPEC/DECISIONS do not specify ordering across files.
    /// The file path string's lexicographic order is used as the primary key, with line
    /// and col as the secondary and tertiary keys (within a single file this reduces exactly
    /// to ascending file:line:col order, satisfying D-CLI-03).
    pub fn into_sorted(mut self, sources: &SourceMap) -> Vec<Diagnostic> {
        self.diagnostics.sort_by(|a, b| {
            sources.path(a.span.file).cmp(sources.path(b.span.file))
                .then(a.span.start.line.cmp(&b.span.start.line))
                .then(a.span.start.col.cmp(&b.span.start.col))
        });
        self.diagnostics
    }
}
```

### 3.3 Token / TokenKind

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    IntLiteral(i64),
    FloatLiteral(f64),
    StringLiteral(String),      // escapes already resolved
    FString(Vec<FStringPart>),
    True,
    False,

    Ident(Arc<str>),

    // Reserved words (D-LEX-01). `Ok`/`Err`/`Some`/`None`/`int`/`float`/`str` are NOT
    // reserved words — they're generated as ordinary Ident tokens and treated as
    // pre-registered identifiers on the flat-namespace side.
    Def, Struct, Enum, If, Else, Match, Return, Var, Uses, Par,
    And, Or, Not, In, Underscore, Module, Void, KwSelf,

    Plus, Minus, Star, Slash, Percent,
    EqEq, NotEq, Lt, LtEq, Gt, GtEq,
    Eq,        // `=` (assignment is a statement, not an expression)
    Arrow,     // `->` (function types inside type annotations only)
    FatArrow,  // `=>`
    PipeOp,    // `|>`
    Question,
    Dot, Comma, Colon,
    LParen, RParen, LBracket, RBracket, LBrace, RBrace,

    Newline,
    Indent,
    Dedent,
    Eof,
}

/// An f-string's `{expr}` portion is first carved out by the outer scan (brace depth,
/// §5.2), then lexed by recursively invoking the same Lexer. Inside the expr, a special
/// mode disallows the start of a string literal (`"`) (D-LEX-07).
#[derive(Debug, Clone, PartialEq)]
pub enum FStringPart {
    Text(String),          // Literal portion with `{{`→`{`, `}}`→`}`, and escapes already resolved
    Expr(Vec<Token>),       // Recursively lexed token sequence (does not include the trailing Eof)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
```

### 3.4 AST

**The unified NodeId mechanism** (decision made in this document): "resolved facts" needed by phases downstream of type checking (evaluator, lint, doctest) — struct field indices, the target `Ty` for assignment-target-annotation-driven decode, whether a bare identifier pattern in a match is a unit variant or a fresh binding, type arguments for generic calls — are externalized into a **side table** (`types::resolutions::Resolutions`) keyed by `NodeId`, rather than being written directly onto the AST nodes themselves (which would mean threading a mutable cell through every node). This policy means:

- The AST remains immutable data representing exactly the syntax the parser produced, so `fmt` (which needs no type checking, §2.2) works without depending on type-checking results at all.
- No matter how many times the type-checking phase is re-run (e.g. a future incremental re-check), the AST itself never needs to be rewritten.
- The evaluator, lint, and doctest can cleanly separate "code that inspects the AST to determine structure" from "code that looks up resolved facts via Resolutions."

```rust
/// A monotonically increasing number the parser assigns to syntactic elements
/// (expressions, declarations, match arms, etc. — any node that will later need
/// resolved information). AST nodes carry no other resolved information themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);
```

The AST proper follows.

```rust
pub struct Module {
    pub file: FileId,
    /// Whether the effective first line after shebang stripping was `module` (D-LEX-08/09).
    /// If true, the module_resolve phase checks "declarations only" (D-MOD-02).
    pub is_module_directive: bool,
    /// Preserves source order exactly. Declarations (Item::Decl) get hoisted and
    /// registered by module_resolve, but the order of this Vec itself is never
    /// changed (D-SYN-08: hoisting is purely a matter of scope construction and
    /// must not break the principle that execution order matches visual order).
    pub items: Vec<Item>,
}

pub enum Item {
    Decl(Decl),
    Stmt(Stmt),
}

pub enum Decl {
    Function(FunctionDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    // There is no Const(ConstDecl) — a module-level constant is syntactically
    // completely identical to an ordinary top-level assignment in the entry file
    // (`Ident (":" TypeAnn)? "=" Expr`), and which meaning applies is decided
    // after the fact by module_resolve inspecting Item::Stmt(NameAssign) (§4.2).
    // Both are built as the same Item::Stmt(Stmt) from the start, so the parser
    // never needs to know whether it's in an entry file or a module file in order
    // to branch the syntax tree's type (the DOC-COMMENT-MISSING-ON-STMT-LEVEL-CONST
    // decision, §8).
}

pub struct FunctionDecl {
    pub id: NodeId,
    pub name: Arc<str>,
    pub generics: Vec<Arc<str>>,        // `[T, U]`
    pub self_param: Option<SelfParam>,  // Some only for struct methods. Enums have no methods
                                          // (neither SPEC §3.5's grammar examples nor anything in
                                          //  DECISIONS mentions enum method syntax, so this document
                                          //  decides not to provide a method slot on enum declarations)
    pub params: Vec<Param>,
    pub ret: TypeAnn,
    pub effects: Vec<Arc<str>>,           // `uses {..}` (empty means pure)
    pub body: Block,
    pub doc_comment: Option<DocComment>,
    pub span: Span,
}

pub struct SelfParam {
    pub mutable: bool,   // whether it's `var self` or `self` (D-MUT-01)
    pub span: Span,
}

pub struct Param {
    pub name: Arc<str>,
    pub ty: TypeAnn,
    pub span: Span,
}

pub struct StructDecl {
    pub id: NodeId,
    pub name: Arc<str>,
    pub generics: Vec<Arc<str>>,
    pub fields: Vec<Param>,             // Reuses Param (name: ty). Declaration order = field index
    pub methods: Vec<FunctionDecl>,      // self_param is always Some
    pub doc_comment: Option<DocComment>,
    pub span: Span,
}

pub struct EnumDecl {
    pub id: NodeId,
    pub name: Arc<str>,
    pub generics: Vec<Arc<str>>,
    pub variants: Vec<EnumVariant>,
    pub doc_comment: Option<DocComment>,
    pub span: Span,
}

pub struct EnumVariant {
    pub name: Arc<str>,
    pub fields: Vec<TypeAnn>,   // empty means a unit variant. Always positional per D-SYN-07 (never named fields)
    /// Solely for fmt's general-comment preservation (§5.9). Not a DocComment — D-DOC-03
    /// does not treat the per-variant level as a doctest target.
    pub leading_comments: Vec<String>,
    pub trailing_comment: Option<String>,
    pub span: Span,
}

/// A `##` doc comment. The structure implementing D-DOC-01–03 (only fences with no
/// language tag are targets, multiple fences, applies immediately before any of
/// def/struct/enum/a constant).
pub struct DocComment {
    pub prose_lines: Vec<String>,   // Explanatory text outside the fences (not a test target)
    pub fences: Vec<DocFence>,
    pub span: Span,
}

pub struct DocFence {
    /// The tag right after ` ``` `. None or an empty string means it's a test target (D-DOC-01).
    /// A non-empty tag such as `json` is ignored.
    pub lang_tag: Option<String>,
    /// The **actual source file** line number of the fence's first line (D-DOC-05).
    pub body_start_line: u32,
    /// The fence's text verbatim (excluded from fmt, D-FMT-06 — so it's kept as raw
    /// text rather than a parsed result of our own, and the doctest phase parses it
    /// via an independent Lexer/Parser invocation).
    pub raw_text: String,
    pub span: Span,
}
```

Next, expressions, statements, patterns, and type annotations.

```rust
pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub span: Span,
}

pub enum ExprKind {
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    StringLit(String),
    FString(Vec<FStringSegment>),     // Distinct from the lexical-level FStringPart (exprs carry a syntax tree)

    Ident(Arc<str>),

    ListLit { elements: Vec<Expr>, was_multiline: bool },
    DictLit { entries: Vec<(Expr, Expr)>, was_multiline: bool },
    SetLit { elements: Vec<Expr>, was_multiline: bool },
    TupleLit { elements: Vec<Expr>, was_multiline: bool }, // A single element requires a trailing comma (D-TYPE-01), already checked by the parser

    Unary { op: UnaryOp, operand: Box<Expr> },
    Binary { op: BinaryOp, lhs: Box<Expr>, rhs: Box<Expr> },

    /// Syntactically unifies function calls, struct construction, and enum variant
    /// construction (`Ident "(" arglist ")"` has the same shape for all three —
    /// decision made in this document, elaborated at the end of §3.4). Which meaning
    /// applies is settled by the type-checking phase from the name resolution of `callee`.
    Call { callee: Box<Expr>, type_args: Vec<TypeAnn>, args: Vec<Arg>, was_multiline: bool },
    MethodCall { receiver: Box<Expr>, method: Arc<str>, type_args: Vec<TypeAnn>, args: Vec<Arg>, was_multiline: bool },

    FieldAccess { target: Box<Expr>, field: Arc<str> },
    TupleIndex { target: Box<Expr>, index: u32 },        // `t.0` (the parser validates the numeric token)
    Index { target: Box<Expr>, index: Box<Expr> },        // `xs[i]` / `m[k]`
    Question { target: Box<Expr> },                        // `expr?`

    Pipe(PipeExpr),
    Lambda { params: Vec<LambdaParam>, body: Box<Expr> },
    If(Box<IfExpr>),
    Match { scrutinee: Box<Expr>, arms: Vec<MatchArm> },
    Par { kind: ParKind, elements: Vec<Expr> },             // `par [..]` / `par (..)`

    Grouping(Box<Expr>),   // `(expr)`. Kept in the AST distinctly from a tuple (for fmt's reproducibility)
}

/// Represents named arguments (struct construction, `name: value`) and positional
/// arguments (function calls, enum variant construction) using the same shape.
/// Which form is required is checked by the type-checking phase per callee kind
/// (D-TYPE-13: structs always require named args / D-SYN-07: enum variants are
/// always positional / ordinary function calls and calling a local-variable closure
/// are always positional, D-TYPE-11).
pub struct Arg {
    pub name: Option<Arc<str>>,
    pub value: Expr,
    pub is_placeholder: bool,   // The pipe's `_` (meaningful only when Arg is used as a plain function-call argument)
}

pub struct PipeExpr {
    pub source: Box<Expr>,
    pub stages: Vec<PipeStage>,
}

pub struct PipeStage {
    pub callee: PipeCallee,
    pub question: bool,   // The `?` trailing this stage (the pipe itself does not auto-short-circuit on Result, SPEC §6.3)
    pub span: Span,
}

pub enum PipeCallee {
    Bare(Expr),                                     // Bare name: `x |> json.encode`
    WithArgs { callee: Box<Expr>, args: Vec<Arg> },   // A call including `_`. A syntax error (E0503) if
                                                        // not even one arg has `is_placeholder` — checked by the parser
}

pub struct LambdaParam {
    pub name: Arc<str>,
    pub ty: Option<TypeAnn>,   // The annotation is optional (contextual inference, SPEC §5.1)
    pub span: Span,
}

/// `if` is always an expression, and nowhere in SPEC/DECISIONS is there wording
/// allowing `else` to be omitted; all 14 occurrences of `if` found under samples/
/// (verified by actually grepping the whole of samples/ while writing this document)
/// have an `else` without exception. On this basis, this document makes `else`
/// mandatory at the parser level (an `if` without `else` is a syntax error).
pub struct IfExpr {
    pub cond: Box<Expr>,
    pub then_branch: Block,
    pub else_branch: ElseBranch,
    pub span: Span,
}

pub enum ElseBranch {
    Block(Block),
    ElseIf(Box<IfExpr>),   // Nesting an `if` on the line after `else` (D-SYN-03's multi-branch expression)
}

pub struct MatchArm {
    pub pattern: Pattern,
    pub body: MatchArmBody,
    /// Solely for fmt's general-comment preservation (§5.9). Not a DocComment (not a target of D-DOC-03).
    pub leading_comments: Vec<String>,
    pub trailing_comment: Option<String>,
    pub span: Span,
}

pub enum MatchArmBody {
    Expr(Expr),     // `=> expr`
    Block(Block),    // A multi-statement arm: newline + indent after `=>` (subject to D-SYN-11's block-value rule)
}

pub enum ParKind {
    List,   // `par [f(), g()]` → list[T] (all elements the same type)
    Tuple,  // `par (f(), g())` → tuple[A, B]
}

#[derive(Clone, Copy)]
pub enum UnaryOp { Neg, Not }

#[derive(Clone, Copy)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod,
    Lt, LtEq, Gt, GtEq, EqEq, NotEq,
    And, Or,
}

pub struct Block {
    /// Only when this Block is the body of an if/match branch does a trailing
    /// ExprStmt become the value of the whole block, per D-SYN-11. When used as
    /// FunctionDecl.body, a different rule applies (§5.6, "the function-body value
    /// rule") — D-SYN-11 is not generalized to FunctionDecl.body (the
    /// VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT decision, §8). Block itself has no
    /// knowledge of which rule applies (the same syntactic type is simply reused
    /// across two contexts, and the caller — the if/match check in check_stmt.rs,
    /// or the function-body check in check_decl.rs — chooses which one applies).
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
    /// A D-DOC-03 doctest target (a `##` fence). Can actually attach only to
    /// `StmtKind::NameAssign` (a module-level constant, or a top-level assignment in
    /// the entry file that looks syntactically identical — the
    /// DOC-COMMENT-MISSING-ON-STMT-LEVEL-CONST decision, §8). If a `##` is written
    /// before any other StmtKind the parser attaches it the same way, but doctest
    /// collection never targets anything other than NameAssign.
    pub doc_comment: Option<DocComment>,
    /// Solely for fmt's general-comment preservation (§5.9). Independent of doc_comment
    /// above (both are often None).
    pub leading_comments: Vec<String>,
    pub trailing_comment: Option<String>,
}

pub enum StmtKind {
    /// `var x = expr` / `var x: T = expr`. Always a fresh mutable binding in the current scope.
    VarDecl { name: Arc<str>, ty: Option<TypeAnn>, value: Expr },
    /// `x = expr` / `x: T = expr` (assignment to a bare identifier). Syntactically a
    /// single form, but the type-checking phase's name-resolution step settles it
    /// into one of the following three cases (decision made in this document — the
    /// parser doesn't know "does `x` already exist in the current scope," which is
    /// needed to decide this, so it can't be settled earlier):
    ///   1. `x` doesn't exist in the current scope → a fresh immutable binding
    ///   2. `x` exists in the current scope as a `var` binding → reassignment (only
    ///      type match is checked; not subject to E3001)
    ///   3. `x` exists in the current scope as an immutable binding → E3001
    NameAssign { name: Arc<str>, ty: Option<TypeAnn>, value: Expr },
    /// `target.field = expr`. Always a write to an existing path (D-MUT-03: recursively
    /// tracks the root variable).
    FieldAssign { target: Expr, field: Arc<str>, value: Expr },
    /// `target[index] = expr` (list/dict only, D-COL-02).
    IndexAssign { target: Expr, index: Expr, value: Expr },
    /// `_ = expr` (explicit discard of an unused Result, D-ERR-03).
    Discard(Expr),
    Return(Option<Expr>),
    /// An expression statement. If its type is Result, it's subject to D-ERR-03's unused-value check.
    ExprStmt(Expr),
}
```

### 3.5 Pattern — enforcing the nesting constraint via types

D-SYN-06 states: "the only three things that may nest inside an enum-variant destructure or tuple destructure are literals, simple bindings, and wildcards; recursively nesting another variant/tuple pattern is forbidden." Rather than having "the parser check the nesting depth at runtime," this is guaranteed by **using a type that cannot syntactically represent nesting in the first place**.

```rust
pub enum Pattern {
    Literal(LiteralPat, Span),
    /// A bare identifier without parentheses. The parser does **not** determine
    /// whether this is a unit-variant name or a fresh binding variable (that needs
    /// the scrutinee's type — D-SYN-06's "name resolution of bare identifiers").
    /// The type-checking phase settles it and records it in Resolutions via `NodeId` (§3.7).
    BareIdent(Arc<str>, NodeId, Span),
    Wildcard(Span),
    Variant { name: Arc<str>, fields: Vec<SubPattern>, span: Span },   // `Circle(r)` (D-SYN-07: positional)
    Tuple { elements: Vec<SubPattern>, span: Span },                  // `(a, b)`
}

/// Only these three kinds may occupy an element position inside Variant/Tuple.
/// Because SubPattern has no Variant/Tuple variant of its own, the syntax tree for
/// "nesting another Variant/Tuple pattern" is simply impossible to construct in the
/// first place — D-SYN-06's prohibition is expressed as a Rust type rather than as
/// a runtime check in the parser.
pub enum SubPattern {
    Literal(LiteralPat, Span),
    BareIdent(Arc<str>, NodeId, Span),
    Wildcard(Span),
}

pub enum LiteralPat {
    Int(i64),   // Including a leading unary minus (the parser folds D-LEX-04's special-casing into the equivalent of one token)
    Float(f64),
    Bool(bool),
    Str(String),
}
```

### 3.6 TypeAnn (syntactic type annotations)

`list[T]`/`dict[K,V]`/`set[T]` and a user-defined generic type (`Box[T]`) are syntactically the exact same shape (a name plus `[..]` type arguments), so they are represented with a single `Named` variant rather than splitting out dedicated ones.

```rust
pub struct TypeAnn {
    pub kind: TypeAnnKind,
    pub span: Span,
}

pub enum TypeAnnKind {
    /// int/str/User/list[int]/Result[T,E]/Box[int] are all uniformly represented as
    /// "name + type arguments." list/dict/set/tuple/Result/Option/Value get no
    /// special-casing either (generalizing D-TYPE-09's philosophy — Result/Option
    /// have no special-cased syntax — to type-annotation syntax as well).
    Named { name: Arc<str>, args: Vec<TypeAnn> },
    Tuple(Vec<TypeAnn>),                                                // `tuple[A, B, ...]`
    Function { params: Vec<TypeAnn>, effects: Vec<Arc<str>>, ret: Box<TypeAnn> },  // `(int) -> str uses {net}`
    Void,
}
```

### 3.7 Ty / EffectSet / Resolutions

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Int, Float, Bool, Str, Void,
    /// list/dict/set/tuple carry many built-in checking rules (D-TYPE-04 element
    /// uniformity, D-TYPE-05 key constraints, D-COL-01 insertion order), so they get
    /// dedicated variants rather than being folded into `Named` (matching via Rust
    /// pattern matching is less error-prone and faster than checking against the
    /// string "list" — decision made in this document).
    List(Box<Ty>),
    Dict(Box<Ty>, Box<Ty>),
    Set(Box<Ty>),
    Tuple(Vec<Ty>),
    /// Uniformly represents user-defined structs/enums as well as the built-in enums
    /// (Result/Option/Value) (this carries D-TYPE-09's decision — "Result/Option are
    /// ordinary enums with no special-cased syntax" — straight through into the Ty
    /// representation as well: the struct/enum registries are shared as one).
    Named { name: Arc<str>, args: Vec<Ty> },
    Function { params: Vec<Ty>, effects: EffectSet, ret: Box<Ty> },
    /// A type variable that appears **only** while type-checking a generic
    /// function/struct/enum declaration. Never remains in a fully unified concrete
    /// type (the starting point for the type erasure described in §3.8).
    TypeVar(Arc<str>),
}

/// SPEC §8: "the granularity is a fixed set of 6 kinds" — since it's a closed set,
/// represent it as bitflags. There's no reason for an open representation like
/// HashSet<String>.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EffectSet(u8);

impl EffectSet {
    pub const FS: Self   = Self(1 << 0);
    pub const NET: Self  = Self(1 << 1);
    pub const ENV: Self  = Self(1 << 2);
    pub const PROC: Self = Self(1 << 3);
    pub const TIME: Self = Self(1 << 4);
    pub const RAND: Self = Self(1 << 5);

    pub const fn empty() -> Self { Self(0) }
    pub const fn union(self, other: Self) -> Self { Self(self.0 | other.0) }
    /// Whether `self` is entirely contained in `superset` (used to check E2002: does
    /// this exceed the declared `uses`?).
    pub const fn is_subset_of(self, superset: Self) -> bool { self.0 & !superset.0 == 0 }
    pub const fn is_empty(self) -> bool { self.0 == 0 }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "fs" => Some(Self::FS), "net" => Some(Self::NET), "env" => Some(Self::ENV),
            "proc" => Some(Self::PROC), "time" => Some(Self::TIME), "rand" => Some(Self::RAND),
            _ => None,
        }
    }
    /// For generating diagnostic messages (e.g. stringifying as "uses {net, fs}").
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        // For each flag f, check "is f contained in self" (self.0 & f.0 != 0) —
        // is_subset_of goes the other direction ("is self a subset of f"), so it's
        // not used here.
        [("fs", Self::FS), ("net", Self::NET), ("env", Self::ENV),
         ("proc", Self::PROC), ("time", Self::TIME), ("rand", Self::RAND)]
            .into_iter().filter(move |(_, f)| self.0 & f.0 != 0)
            .map(|(n, _)| n)
    }
}

/// Fixed identifiers for built-in namespaces (D-LEX-01). Belongs to a name-resolution
/// system separate from the flat namespace (D-TYPE-07) — even if a user defines a
/// top-level function/variable with the same name as `fs`/`json`/etc., it doesn't
/// affect resolution of `.`-qualified access (the
/// NAMESPACE-QUALIFIED-ACCESS-NO-RESOLUTION-HOME decision, §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamespaceId {
    Fs, Http, Env, Proc, Time, Rand, Regex, Math, Json, Csv, Yaml, Toml,
}

impl NamespaceId {
    pub const fn from_name(name: &str) -> Option<Self> {
        match name {
            "fs" => Some(Self::Fs), "http" => Some(Self::Http), "env" => Some(Self::Env),
            "proc" => Some(Self::Proc), "time" => Some(Self::Time), "rand" => Some(Self::Rand),
            "regex" => Some(Self::Regex), "math" => Some(Self::Math), "json" => Some(Self::Json),
            "csv" => Some(Self::Csv), "yaml" => Some(Self::Yaml), "toml" => Some(Self::Toml),
            _ => None,
        }
    }
}

/// Resolved facts the type-checking phase (part of it being the EffectCheck phase,
/// see below) leaves behind for downstream phases (evaluator, lint, doctest, and LSP). The
/// AST nodes themselves are never modified (§3.4). LSP queries consume the resolved facts in this
/// side table for definition locations and hover results.
#[derive(Default)]
pub struct Resolutions {
    /// The declaration-order index of the field a `FieldAccess`/struct-construction `Arg` refers to.
    field_index: HashMap<NodeId, u32>,
    /// The type arguments a generic call (`Call`/`MethodCall`) settled on via unification.
    type_args: HashMap<NodeId, Vec<Ty>>,
    /// The target type settled by assignment-target-annotation-driven inference, as for
    /// `json.decode` etc. (D-TYPE-16, elaborated in §5.3).
    decode_target: HashMap<NodeId, Ty>,
    /// Whether a `Pattern::BareIdent`/`SubPattern::BareIdent` is a unit variant or a fresh binding.
    bare_ident_kind: HashMap<NodeId, BareIdentKind>,
    /// Whether a `Call`'s callee is struct construction, enum variant construction,
    /// or an ordinary call (the resolved outcome of the unified Call representation
    /// described in §3.4).
    call_kind: HashMap<NodeId, CallKind>,
    /// The source span of the declaration resolved for an identifier expression (used by LSP
    /// definition queries).
    ident_def: HashMap<NodeId, Span>,
    /// The settled type of each expression (eval generally doesn't consult Ty, but
    /// some built-ins, such as decode, need it).
    expr_ty: HashMap<NodeId, Ty>,
    /// D-TYPE-17's implicit-wrap determination for a `return` target expression. Keyed
    /// by the `NodeId` of the returned `Expr` (the
    /// IMPLICIT-WRAP-NO-RESOLUTIONS-FIELD decision, §8). No entry = no wrap (priority
    /// 1, matching the annotation as-is).
    implicit_wrap: HashMap<NodeId, WrapKind>,
    /// The resolved outcome when the receiving `Ident` expression of a `.`-qualified
    /// access refers to a built-in namespace (the
    /// NAMESPACE-QUALIFIED-ACCESS-NO-RESOLUTION-HOME decision, §8). No entry = evaluate
    /// as an ordinary local variable/top-level identifier.
    namespace_ref: HashMap<NodeId, NamespaceId>,
    /// For a function/method declaration (`NodeId` is its `FunctionDecl.id`), bit flags
    /// indicating which of its parameters have a function type AND are actually
    /// invoked in the body (i.e. should have effects forwarded to them — the
    /// EFFECT-HOF-POLYMORPHISM decision, §5.5/§8). The only field the EffectCheck
    /// phase writes — still empty when TypeCheck finishes.
    hof_forwarding: HashMap<NodeId, Vec<bool>>,
}

/// The implicit-wrap kind for D-TYPE-17 priority 2. Priority 1 (matches the
/// annotation as-is) needs no wrapping, so it has no variant — represented instead
/// by the absence of an entry in `Resolutions.implicit_wrap`.
pub enum WrapKind { Ok, Some }
pub enum BareIdentKind { UnitVariant, Binding }
pub enum CallKind { StructInit, EnumVariantInit, FunctionCall, ClosureCall }
```

### 3.8 Generics: type erasure rather than monomorphization (making the design decision explicit)

The answer to the question posed in the task — "are generics monomorphized, or handled via runtime polymorphism?" — is **neither**.

D-FUNC-04's "monomorphize to a concrete type at each call site" means that **the type-checking phase** unifies a type variable (`Ty::TypeVar`) from the argument types at each call site down to a concrete type, and verifies the function body is type-correct under that concrete type — it does *not* mean, as in Rust/C++, "generating separate executable code per call site." **The evaluator has no knowledge of generics whatsoever** — for the following reasons:

1. Yabumi's `Value` (§3.9) is already a single, dynamically tagged enum — it doesn't have a different Rust type for `T=int` versus `T=str`. When evaluating the body of a generic function `def first[T](xs: list[T]): Option[T]`, the evaluator doesn't need to know how `T` was instantiated — the elements of `xs` are already stored uniformly as `Value`.
2. Yabumi allows no operator overloading whatsoever (D-FUNC-05), and the operations that can be invoked on an unconstrained type parameter `T` are limited to assignment, storage, passing, and `==`/`!=` (a single structural-equality function shared across all types, D-OP-06). In other words, the situation "which implementation to call depends on `T`" — the problem runtime polymorphism (vtables/trait objects) actually exists to solve — never arises in the first place.

Consequently, once the type-checking phase is passed, no information about generics needs to be passed on to the evaluator, and even the "monomorphized type arguments" recorded in Resolutions exist **purely for verification purposes** (because type checking itself uses the unification result to further check the inner expressions). From the evaluator's point of view, calling a generic function and calling a non-generic function go through exactly the same code path. This is close to Java's generics (type erasure) or ML-family let-polymorphism — neither the overhead of duplicating code per call site, nor the overhead of per-type dispatch, occurs.

### 3.9 Value (runtime values) — the concrete representation via Arc + CoW

This section concretely settles how §14's "value semantics + scope RAII, implemented via reference counting (Arc + copy-on-write)" and D-MUT-01/02/04 (only the `var self` receiver is the sole channel through which mutability propagates; everything else is always copied by value) are realized in Rust.

**Summary of the approach**: of `Value`'s variants, only the "4 kinds that have destructive methods" (struct instance / list / dict / set) are wrapped in `Arc<T>`, and mutating operations go through `Arc::make_mut` without exception. Everything else (int/float/bool/str/tuple/enum/closure) is immutable once constructed and never needs `Arc::make_mut` at all.

```rust
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// The sole value of type `void`. A fieldless marker — represents D-TYPE-08's
    /// "no value can ever be produced" as a zero-sized variant (the
    /// VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT decision, §8). Used both for the
    /// implicit return value of a function declared `void`, and anywhere inside the
    /// evaluator that needs to represent "there is no value" (e.g. the evaluation
    /// result of `Return(None)`).
    Void,
    /// str is immutable (SPEC §3.1), so `Arc<str>` suffices — there's no need for the
    /// double indirection of something like `Arc<String>` (the language has no
    /// growth/in-place-mutation operation whatsoever).
    Str(Arc<str>),

    /// Has push/pop/insert/remove/extend/clear (the destructive section of STDLIB.md
    /// §2.1), so it's `Arc<Vec<Value>>`. Mutating operations always go through
    /// Arc::make_mut (described below).
    List(Arc<Vec<Value>>),
    /// K is MapKey (below). Uses indexmap to satisfy D-COL-01 (insertion-order preservation).
    Dict(Arc<IndexMap<MapKey, Value>>),
    Set(Arc<IndexSet<MapKey>>),
    /// tuple is immutable once constructed (neither SPEC nor STDLIB has any
    /// tuple-element-rewriting operation at all). Since it's fixed-length, it's
    /// represented as a boxed slice rather than a Vec.
    Tuple(Arc<[Value]>),

    /// The only user-defined value that can be the target of a destructive `var self` method call or field assignment.
    Struct(Arc<StructInstance>),
    /// enum (both user-defined, and Result/Option/Value themselves) is immutable once
    /// constructed — every method of Result/Option in STDLIB.md takes plain `self`
    /// (not var), so no destructive method exists (this is also a consequence of the
    /// decision in §3.4 not to give enums a method-definition slot). Never needs
    /// Arc::make_mut at all.
    Enum(Arc<EnumInstance>),

    /// A lambda, or the case where a top-level function is referenced as a value
    /// (e.g. `xs.par_map(fetch_repos)`).
    Closure(Arc<Closure>),
}

/// The values allowed as K in dict[K,V] or T in set[T] (D-TYPE-05: int/str/bool, and
/// a tuple whose elements are all themselves allowed key types). Rather than
/// implementing Eq+Hash on `Value` as a whole, "a value that can be a key" is carved
/// out as its own small dedicated type, so that the very state "a float or a list is
/// a dict key" cannot even be constructed as a Rust type (D-TYPE-05 is already
/// prevented by static checking, so this constraint is never violated at runtime
/// regardless — but making it unrepresentable even in the face of a hypothetical
/// implementation bug is safer — decision made in this document). `Value` itself
/// can't implement Eq/Hash because it contains f64 (NaN comparison isn't reflexive),
/// but MapKey holds no f64 and so can straightforwardly derive Eq+Hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Int(i64),
    Bool(bool),
    Str(Arc<str>),
    Tuple(Arc<[MapKey]>),
}

impl MapKey {
    pub fn to_value(&self) -> Value { /* .. */ }               // For generating return values such as dict.keys()
    pub fn try_from_value(v: &Value) -> Option<MapKey> { /* .. */ } // The entry point for dict[k]/set.insert(x) etc.
}

/// Fields are held as a Vec in declaration order (D-TYPE-13: construction requires
/// named arguments, but the runtime representation uses an index instead, so `.field`
/// access doesn't need a string comparison every time — the index was already
/// recorded in Resolutions by the type-checking phase, §3.7).
#[derive(Debug, Clone, PartialEq)]
pub struct StructInstance {
    pub type_name: Arc<str>,
    pub fields: Vec<Value>,
}

#[derive(Debug, PartialEq)]
pub struct EnumInstance {
    pub type_name: Arc<str>,       // "Result" / "Option" / "Value" / a user-defined name
    pub variant_index: u32,        // Declaration order (also used for match-exhaustiveness checking and fmt reconstruction)
    pub variant_name: Arc<str>,      // For diagnostic messages
    pub fields: Vec<Value>,          // Positional (D-SYN-07). Empty for a unit variant
}

/// The actual callable body of a lambda expression, pointed to by `CallTarget::Lambda`.
/// Just a type that bundles together the two fields of `ExprKind::Lambda{params, body}`
/// (ast/expr.rs, §3.4) as-is — it is not an independent AST node (it carries no
/// NodeId/Span — it's purely immutable data shared by reference, as `Arc<LambdaBody>`,
/// from multiple Closure values). Placed in eval/value.rs since it's a type the eval
/// side depends on (reusing LambdaParam/Expr from ast/expr.rs).
pub struct LambdaBody {
    pub params: Vec<LambdaParam>,
    pub body: Expr,
}

pub struct Closure {
    pub target: CallTarget,
    /// Non-empty only for a lambda. Captures are always copied by value (D-MUT-04) —
    /// even if the closure gets mutated somewhere, the value already stored in this
    /// array is unaffected (Value::clone() merely bumps an Arc's reference count by
    /// one; even if Arc::make_mut later runs on the original variable's side, the
    /// clone held by this closure remains independent — elaborated at the end of §3.9).
    pub captured: Vec<(Arc<str>, Value)>,
    /// The effect row (D-FUNC-02) already settled by the type-checking phase. The
    /// evaluator does not use this for runtime enforcement (effects are checked
    /// statically only, D-EFF-01) — it's used only for diagnostic purposes and
    /// branching on the built-in side.
    pub effects: EffectSet,
}

pub enum CallTarget {
    Lambda(Arc<LambdaBody>),        // A lambda expression body (parameter names + expression); shared across multiple calls since it comes from the AST
    Function(Arc<str>),               // A top-level function name (resolved via Program.functions)
    Builtin(BuiltinFnId),             // For when a stdlib function is passed as a value (e.g. `x |> json.encode`)
}
```

**The only 4 kinds of runtime value that can possibly be mutable are struct instance / list / dict / set**, which exactly matches both the rule D-MUT-01/02 establishes ("only `var self` methods are the sole channel through which mutability propagates") and the list of operations STDLIB.md explicitly labels "destructive" (list/dict/set's `push`/`insert`/`remove`/etc., and a struct's `var self` methods). tuple, enum (including Result/Option/Value), primitives, and strings are all immutable once constructed and never need `Arc::make_mut` at all — this also has the practical benefit, at implementation time, of making it immediately obvious which variants need `Arc::make_mut` code written for them.

`Value` implements only `PartialEq` (not `Eq`/`Hash` — `Float` makes those undeliverable via derive anyway, and there's nowhere `Value` is used as a hash-map key; anywhere hashing is needed, `MapKey` is always used instead), and compares all variants recursively (D-OP-06: `==`/`!=` are structural equality across all types). Because `Arc<T>`'s `PartialEq` compares the pointed-to value rather than pointer identity (the standard library's `impl<T: PartialEq> PartialEq for Arc<T>`), `Value::List(a) == Value::List(b)` recursively compares the contents of the `Vec<Value>` that `a` and `b` point to — anywhere pointer comparison is actually needed (should that ever become desirable in the future), `Arc::ptr_eq` must be used explicitly and must not be confused with ordinary `==`.

**`PartialEq` is hand-written rather than derived** (requiring a `PartialEq` derive all the way down to `Closure`/`CallTarget`/`LambdaBody` — i.e. down into the AST itself — would force a derive onto every AST node such as `Expr`/`Stmt`, dragging in things like the comment strings fmt preserves as comparison targets too — an unwanted requirement that would otherwise propagate). Comparing two `Value::Closure(_)` values **always returns `false`** (even comparing a value against itself never yields `true` — neither structural AST comparison nor pointer comparison is performed). This is a theoretically reachable branch: D-FUNC-05 always permits `==`/`!=` even on an unconstrained type parameter `T`, and nothing prevents `T` from being unified to a function type, so code inside a generic function that compares two values of type `T` with `==` can, depending on the call site, end up comparing two `Closure`s (making `unreachable!()` unsound). The design decision "a closure carries no comparable meaning as a value" is spelled out in the simplest possible form: a fixed `false` (the R7 decision, §8). `Struct`/`Enum` recurse into the inner `StructInstance`/`EnumInstance` (both of which already derive `PartialEq`).

### 3.10 Implementing mutability — writing paths back via chained `Arc::make_mut`

D-MUT-01/02/03 requires "only the root variable of a `var` binding may be rewritten" and "nested field/index assignments determine mutability by recursing all the way to the root." In Rust this is implemented by **chaining `Arc::make_mut` at every step along the path**. Rather than "reconstructing along the path" and writing back element by element, `Arc::make_mut` itself performs "clone if shared, otherwise mutate in place" at each individual step, so the end result is that **only the nodes on the mutated path get copied — sibling fields and sibling elements off the path are never duplicated** (structural sharing).

Illustrated in evaluator code, using `u.tags.push("b")` as an example:

An index or destructive method like `u.tags.push("b")` can have `u` pointing at an
out-of-range index or a nonexistent dict key — per D-COL-02 (SPEC §7.4), this is a
panic target (E6001) that **depends on runtime data** and cannot be ruled out by
static (type) checking. So `resolve_place` cannot unconditionally return `&mut Value`;
it returns `Result<&mut Value, Abort>` instead (the R2 decision, §8 — addressing the
criticism that a design using a raw `vec[i]` index (which triggers Rust's native panic
and doesn't produce a `file:line:col [E6001]`-formatted trace) or handling an actually
reachable missing dict key with `unreachable!()` would be wrong).

```rust
/// Unifies receiver resolution for StmtKind::FieldAssign / IndexAssign / a destructive
/// method call. The return value is not "&mut to the root variable's current value,"
/// but rather &mut to the **terminal node** after walking the entire path (the
/// recursive function itself digs down, applying Arc::make_mut at each step).
/// D-MUT-03 (E3001 if the root variable is not var) has already been guaranteed by
/// the type-checking phase, so the evaluator side does not re-verify mutability
/// itself (it assumes it's evaluating a consistently well-typed program —
/// well-typed programs don't get stuck). An out-of-range index / nonexistent key is
/// a runtime condition type checking cannot rule out, so it's returned explicitly as
/// Abort(E6001) via `Result` (the R2 decision).
fn resolve_place<'env>(expr: &Expr, env: &'env mut Environment) -> Result<&'env mut Value, Abort> {
    match &expr.kind {
        ExprKind::Ident(name) => Ok(env.lookup_mut(name)), // Root: the scope's binding slot itself
        ExprKind::FieldAccess { target, field } => {
            let parent = resolve_place(target, env)?;       // Recurse (resolve the parent first)
            let Value::Struct(arc) = parent else { unreachable!("guaranteed Struct by type checking") };
            let inst = Arc::make_mut(arc);                   // Clone only this one step if shared
            let idx = /* look up the resolved index from Resolutions (§3.7) */ 0usize;
            Ok(&mut inst.fields[idx])
        }
        ExprKind::Index { target, index } => {
            let key = /* evaluate index and convert to Value/MapKey */ todo!();
            let parent = resolve_place(target, env)?;
            match parent {
                Value::List(arc) => {
                    let vec = Arc::make_mut(arc);
                    let i = /* convert key to usize */ 0usize;
                    // Use get_mut rather than a raw index operator — convert an
                    // out-of-range access into an explicit Abort::out_of_range
                    // (E6001) rather than Rust's native panic (the R2 decision).
                    vec.get_mut(i).ok_or_else(|| panic::out_of_range(expr.span, "list index"))
                }
                Value::Dict(arc) => {
                    let map = Arc::make_mut(arc);
                    // D-COL-02: a missing key is a legitimate runtime branch subject to E6001 (unreachable!() would be wrong).
                    map.get_mut(&key).ok_or_else(|| panic::out_of_range(expr.span, "dict key"))
                }
                _ => unreachable!("guaranteed by type checking"),
            }
        }
        _ => unreachable!("only Ident/FieldAccess/Index can pass type checking as an assignment target"),
    }
}
```

Callers (FieldAssign/IndexAssign evaluation in `eval/stmt.rs`, destructive method calls
in `eval/call.rs`) simply propagate the Abort mechanically via Rust's `?` — `resolve_place(..)?`
— keeping the design policy established in §5.6 ("an Abort is always propagated mechanically
via Rust's `?`") consistent here as well.

Evaluating `u.tags.push("b")` calls this `resolve_place` on `u.tags` (a `FieldAccess`), then
applies `Arc::make_mut` to the returned `&mut Value` (whose contents are a `Value::List`)
before calling `Vec::push` — a two-step chain of `make_mut` calls: the first step determines
independently "is `u` itself shared elsewhere," and the second determines independently "is
`u.tags` (that List itself) shared elsewhere." **This chain is exactly the mechanism that
correctly realizes D-MUT-04 (closure/function-argument captures are always copied by
value)**: at the moment a lambda captures `u`, only `Value::clone()` happens (which merely
bumps the `Arc`'s reference count), so the captured `u` and the original variable `u` become
two owners pointing at the same `Arc<StructInstance>`. When `u.tags.push(...)` later runs
against the original `u`, the first-step `Arc::make_mut(&mut u_arc)` detects that the
reference count is 2, clones the `StructInstance` on the spot, and mutates the clone — the
old `Arc` the lambda is holding onto is left completely untouched. **No manual dirty-flag
bookkeeping and no explicit reference-count-checking code needs to be written at all** — this
is entirely delegated to the behavior the standard library guarantees for `Arc::make_mut`.

**An incidental benefit for thread safety** (in relation to `par`, elaborated in §5.6):
because `Arc`'s reference count is implemented with atomic operations, even if each worker
thread of a `par_map` temporarily shares an `Arc` pointing at the same original data
(received via value copy), the clone-or-not decision at the instant one thread calls
`Arc::make_mut` is never a race condition. Each thread calls `Arc::make_mut` only through
**its own private `Environment`** (only that thread holds `&mut` access to that
`Environment`'s binding slots), so even when multiple threads hold the same `Arc<T>`, the
invariant "every thread mutates, via `&mut`, only the `Arc` inside its own slot" is
maintained, and no data race occurs. SPEC §9's "no shared mutable state between `par`
branches" is achieved with no locks or mutexes introduced at all — purely through
**value-copy semantics (bumping an Arc's reference count) plus the natural properties of
Arc::make_mut**.

### 3.11 Environment / Program (the scope representation used at evaluation time)

Yabumi has no mutable upvalues via closures (reference captures) at all (D-MUT-04) — capture is always completed by value copy. So the "environment = a parent-child chain of `Arc<RefCell<HashMap<..>>>`" design many interpreters adopt is **unnecessary**, and the environment can be implemented as a simple **owned** stack of scopes (requiring no interior-mutability cells at all).

```rust
/// A frame corresponding to one function/lambda call. Variable lookup never crosses
/// frames (a top-level function body sees only its own arguments plus global
/// declarations — ordinary static scoping. Only a lambda injects a copy of outer
/// values into its initial scope at frame-creation time, via capture).
struct Frame {
    scopes: Vec<Scope>,   // Pushed/popped per block, e.g. an if/match/lambda body
}

type Scope = HashMap<Arc<str>, Value>;

/// The variable environment at evaluation time. Since whether something is `var` has
/// already been settled by the type-checking phase (a consistently well-typed
/// program contains no illegal mutation), this holds no mutability flag at all —
/// just the `Value` itself.
pub struct Environment {
    frames: Vec<Frame>,
}

impl Environment {
    pub fn lookup_mut(&mut self, name: &str) -> &mut Value {
        // "At least one frame always exists" is an invariant guaranteed by how
        // Environment itself is constructed (only the with_frame-style constructors
        // exist; a zero-frame state can never be produced) — expressed with
        // `unreachable!()` rather than `.expect("frame")` (the R3 decision, §8;
        // expect_used is set to deny, so expect can't be used).
        self.frames.last_mut().unwrap_or_else(|| unreachable!("Environment always has at least one frame"))
            .scopes.iter_mut().rev()
            .find_map(|s| s.get_mut(name))
            .unwrap_or_else(|| unreachable!("the name is guaranteed to exist since this is already type-checked"))
    }
    pub fn bind(&mut self, name: Arc<str>, value: Value) { /* fresh binding into the current innermost scope */ }
    pub fn push_scope(&mut self) { /* .. */ }
    pub fn pop_scope(&mut self) { /* .. */ }
    pub fn push_frame(&mut self, initial: Scope) { self.frames.push(Frame { scopes: vec![initial] }); }
    pub fn pop_frame(&mut self) { self.frames.pop(); }
}

/// The single overall program image per `ybm` invocation, with declarations settled once
/// module_resolve completes and resolution side tables populated by later analysis. Once
/// analysis has finished, it can be safely shared across `par`'s worker threads as
/// `Arc<Program>` (requiring no interior mutability or locks whatsoever). LSP analysis retains
/// this checked image for hover and definition queries. Every field is Arc/HashMap<Arc<str>,_>,
/// and even the identifier fields of the AST-node values themselves (FunctionDecl, etc.) are
/// Arc<str> (the R1 decision, §8) — so `Program` as a whole is Send+Sync, satisfying the
/// `F: Send` requirement of `spawn_scoped`, which moves `Arc<Program>` into `par`'s worker
/// threads.
pub struct Program {
    pub functions: HashMap<Arc<str>, Arc<FunctionDecl>>,
    pub structs: HashMap<Arc<str>, Arc<StructDecl>>,
    pub enums: HashMap<Arc<str>, Arc<EnumDecl>>,
    pub consts: HashMap<Arc<str>, Value>,   // Literals only per D-MOD-02, so already evaluated once at load time
    /// Source spans for module-level constants, used to resolve LSP definition locations.
    pub const_spans: HashMap<Arc<str>, Span>,
    pub resolutions: Resolutions,
    /// All source files, already settled by the Lex phase. When a panic is detected
    /// inside `par`, we need to be able to reach `SourceMap` even from deep within a
    /// worker thread, in order to `Diagnostic::render` it on the spot and terminate
    /// the process immediately (§5.8, the PAR-ABORT-NOT-ACTUALLY-IMMEDIATE decision).
    pub sources: Arc<SourceMap>,
}
```

Sequential execution at the top level (the sequence of `Item::Stmt` in the entry file) takes place inside the **single** implicit frame `Environment` holds (the outermost frame, which has no caller). Calling a function/method/lambda pushes a new frame and pops it once a return value is obtained — the called function's body can never reference the scopes of the caller's frame at all (static scoping, not dynamic scoping).

---

## 4. Pipeline

### 4.1 Overall phase ordering

The three file-execution commands (`ybm <file>`, `ybm check`, and `ybm test`) share Lex → Parse →
ModuleResolve → TypeCheck → EffectCheck. `check` and `test` then run Lint; plain execution does not,
matching SPEC §1's command table. `ybm lsp` uses the same front end and runs EffectCheck/Lint plus
virtual doc-fence type checking for open documents, but never executes source. Within each phase
diagnostics are collected exhaustively, while a failed phase gates every later phase.

```
Lex → Parse → ModuleResolve → TypeCheck(+Mutability) → EffectCheck
                                                               │
                ┌──────────────────────────────────────────────┘
                ▼
  ybm <file>        : Eval
  ybm check <file>  : Lint → type-check doc fences → fmt
  ybm test <file>   : Lint → type-check/run doc fences → tally
  ybm lsp           : analyze open documents → publish diagnostics; serve hover/definition/fmt
```

### 4.2 Input/output of each phase

| Phase | Input | Output | Diagnostic code range |
|---|---|---|---|
| Lex | Each `SourceFile` in `SourceMap` | A `Vec<Token>` per file | E0000–E0999 |
| Parse | The `Vec<Token>` per file | A `Module` (AST) per file | E0500–E0999 (the syntax subset) |
| ModuleResolve | All `Module`s | The `Program` skeleton (declarations registered only; the contents of function bodies etc. are not yet checked) | E1001, E5001, E5002 |
| TypeCheck | The `Program` skeleton | A nearly-complete `Resolutions` (§3.7; all fields except `hof_forwarding`); type checking of every declaration body and top-level statement complete | E1xxx, E3001 |
| EffectCheck | The type-checked `Program` + `Resolutions` | Diagnostics, plus writing `Resolutions.hof_forwarding` (see below — the only field EffectCheck writes into Resolutions) | E2001, E2002 |
| Lint | The type-checked `Program` + `Resolutions` | (Diagnostics only) | E4001–E4005 |

**ModuleResolve in detail** (including decisions made in this document): In a file where `Module.is_module_directive == false` (an entry file), every `Item::Stmt` is treated as an ordinary sequential-execution statement (function calls are allowed, as are `var` declarations). In a file where `Module.is_module_directive == true` (a module file), `Item::Stmt` **should** never be allowed at all — but as an exception, an `Item::Stmt` that is syntactically a `NameAssign` whose right-hand side satisfies D-MOD-02's restricted grammar (only literals, collection literals, and constant references combined — no function calls) is registered into `Program.consts` as "a module-level constant declaration." For any other `Item::Stmt` (`VarDecl`, a `NameAssign` that doesn't satisfy the restricted grammar, `FieldAssign`/`IndexAssign`, `Discard`, `Return`, or a bare `ExprStmt`) — even a single one — E5002 is reported at that `Item`'s `Span`. E5001 (malformed module-directive syntax) is emitted at the Lex/Parse stage when it's detected that "the effective first line after shebang stripping is not a bare `module`" — concretely, this covers the case where the Lexer tries to recognize a `module`-looking token on line 1 but finds what looks like an identifier or argument following it (e.g. `module foo`).

**Why TypeCheck performs mutability checking (E3001) in the same pass**: D-MUT-01–03's mutability checking only works by reusing information that type checking itself already has — "what is this expression's type," "is this method `var self`," "is this variable a `var` binding." Making it a fully separate later pass would mean rebuilding type information twice (re-collecting information type checking has already discarded), which is both inefficient and fragments the implementation. So this document's design has the same function that type-checks an expression also, when that expression is an assignment target (`FieldAssign`/`IndexAssign`/the reassignment form of `NameAssign`/a destructive method-call receiver), perform D-MUT-03's root-variable tracking and `var` determination, pushing E3001 into the same `DiagnosticBag` on violation.

**Why EffectCheck is a separate phase run only after TypeCheck completes**: D-FUNC-03's effect-row inference computes "the union of the effects of every function/method called," which is meaningless unless the callee's **type has already been settled** (there's no way to determine what an expression whose type hasn't been settled is calling). Running effect inference while type checking has even one failure would only produce meaningless diagnostics, so EffectCheck runs only when TypeCheck comes back with zero.

**Why EffectCheck writes `Resolutions.hof_forwarding`** (elaborated in the EFFECT-HOF-POLYMORPHISM decision, §5.5/§8): Correctly checking effect polymorphism for higher-order functions first requires, for each user-defined function/method, a **syntactic fact** — "which of its function-typed parameters are actually invoked in the body" (unrelated to types or effects — determined by name resolution alone). While this fact is naturally the kind of thing type checking generates, the `infer_effects` algorithm itself uses it only in the form "the caller uses this fact to compose effects," so as a matter of responsibility it belongs to EffectCheck. When EffectCheck starts, it first walks every function/method declaration once to write this `hof_forwarding` mask into `Resolutions` (this walk doesn't depend on which effects any function has, so it's unaffected by declaration order or mutual recursion), and only then performs the ordinary effect summation (the existing D-FUNC-03 algorithm) — a two-stage structure.

**Why Lint is a separate phase run only after EffectCheck completes**: Detecting unused functions (E4002) requires determining "a `def` that's never called," which needs a complete call graph (the same information the effect check builds while establishing call relationships), and naming conventions (E4005) and unused variables (E4001) likewise assume that name resolution has fully completed. Running lint against a program whose types/effects are broken tends to produce spurious findings, so Lint runs only when EffectCheck comes back with zero.

### 4.3 Tail behavior per subcommand

**`ybm <file>`**: Eval runs only if Lex, Parse, ModuleResolve, TypeCheck, and EffectCheck succeed. Lint belongs to `check`/`test`. Doc-comment fences are untouched. Runtime panics and top-level `?` propagation produce their E6xxx diagnostic and exit 1.

**`ybm check <file>`**: On top of the 6 phases, each `DocFence` (only those with no language tag, D-DOC-01) is type-checked as an **independent, virtual program** scoped over "every declaration in the entry file plus same-directory modules" (per D-DOC-05, a diagnostic's `line` is the actual file line, not a line relative to the fence). Here too, if any diagnostics come out, execution naturally never proceeds to (fmt). **Nothing is ever executed** (explicitly stated in SPEC §1). If everything comes back zero, fmt runs: with no `--apply` flag, the formatted result is compared byte-for-byte against the original; a match means no write and exit 0, a mismatch (there's a diff) means no write and exit 1 (the diff is shown on stdout; its exact format is unspecified and left to the implementer — this document proposes something like a concise unified diff laying out "before"/"after," but it has been confirmed that every test in SAMPLES_PLAN.md uses `stdout = {mode="contains"}`, i.e. does not verify exact content). With `--apply`, the formatted result is written in-place and it exits 0.

**`ybm test <file>`**: The 6 phases first run against "the entirety of the entry file's plus same-directory modules' declarations"; if even one diagnostic comes out, no doc test is run at all and it exits 1 (running a doc test that depends on a declaration whose types are broken would be meaningless). If the 6 phases come back with zero, then for each `DocFence`:
  1. That fence's text is parsed as an independent sequence of statements and type-checked as a virtual program scoped over "every declaration in the entry file plus same-directory modules." If a diagnostic comes out, it's a `fail` (`code` is that diagnostic's `ErrorCode`, `line` is the actual file line per D-DOC-05).
  2. If type checking passes, the fence's statements are executed sequentially against the **same `Program`** (global declarations are shared) but with a **fresh `Environment`** (D-DOC-02: an independent execution context per block). If an `assert` failure or a panic/`?` propagation occurs, it's a `fail` (that `Diagnostic`'s `code`/`line`); if it runs to completion, it's a `pass`.
  The results across all blocks are tallied; if even one `fail` exists, it exits 1, otherwise it exits 0. A summary of pass/fail counts is printed to stdout (SPEC §1: "the pass/fail tally summary for doc tests is printed to stdout"). Each individual `fail`'s `[Exxxx]` diagnostic line itself goes to stderr just like an ordinary diagnostic (D-CLI-01).

**`ybm lsp`**: Starts a JSON-RPC server over stdin/stdout. `didOpen`/`didChange`/`didSave` reanalyze the open document from the unsaved-content overlay and publish diagnostics; `didClose` removes the overlay and clears diagnostics. Hover and definition queries use resolved types and source spans, while formatting returns a single whole-document edit from the canonical formatter. The server advertises full synchronization and UTF-16 positions by default, selecting UTF-32 when advertised by the client. It does not execute source files.

### 4.4 The exit-code determination rule (summary table)

| Situation | exit code |
|---|---|
| An error before CLI startup (bad extension E9002, file not found E9001, unreadable source E9003) | 1 |
| One or more diagnostics from Lex/Parse/ModuleResolve/TypeCheck/EffectCheck | 1 (no later phase or execution proceeds) |
| `ybm <file>`: shared phases succeed and execution finishes normally | 0 |
| `ybm <file>`: shared phases succeed, but runtime fails | 1 |
| `ybm check`/`ybm test`: Lint fails | 1 |
| `ybm check <file>` (no `--apply`): the above plus doc-fence type checking all come back zero, and the fmt output byte-matches the original without writing | 0 |
| `ybm check <file>` (no `--apply`): the above plus doc-fence type checking all come back zero, but the fmt output differs (nothing written) | 1 |
| `ybm check <file> --apply`: the above plus doc-fence type checking all come back zero and fmt is written in-place | 0 |
| `ybm test <file>`: every doc block passes | 0 |
| `ybm test <file>`: one or more doc blocks fail, or the main program's 6 phases fail | 1 |
| `ybm lsp`: shutdown followed by `exit`, or stdin reaches EOF | 0 |
| `ybm lsp`: transport failure, or `exit` before `shutdown` | 1 |

Diagnostic ordering (D-CLI-03, "ascending file:line:col") is handled solely by §3.2's `DiagnosticBag::into_sorted` — **as decided in this document**, when spanning multiple files, the file path's lexicographic order is used as the top-priority key (this does not conflict with the expected values in the current samples/, which are overwhelmingly single-file cases).

### 4.5 Why the main thread is never used directly (stated up front)

Immediately after reading the command-line arguments, `main.rs` runs the entire pipeline above inside **a separate thread given an explicitly specified stack size**, and determines the `std::process::ExitCode` from that thread's `join()` result (details are given in §5.7's stack-overflow countermeasures — this absorbs the difference in default main-thread stack size across operating systems).

---

## 5. Difficult points and countermeasures

### 5.1 Precisely generating indent/dedent

The lexer advances its indentation state machine not by physical line but by **logical line**. A logical line is a run of physical lines joined together "while a bracket is open" (D-SYN-04) or by "method-chain continuation" (D-SYN-05). The algorithm carries the following state:

- `indent_stack: Vec<u32>` (initial value `[0]`)
- `bracket_depth: u32` (+1 for `(` `[` `{`, -1 for the matching closing bracket)

Processing per physical line:

1. **Tab detection** (decision made in this document: given the context of D-SYN-01, detection is limited to only the **leading whitespace** of each line. A tab inside a string literal or a trailing comment is not covered — SPEC/DECISIONS only state the tab prohibition in the context of indentation checking, and this is the most natural reading). If even a single tab character appears in the leading whitespace, immediately report E0001 and abort lexing of the entire file without processing that physical line or anything after it (fatal).
2. **Determining a transparent line** (D-SYN-02's extended definition): a line whose remainder — after stripping leading whitespace — is empty, or begins with `#` (including `##`), is treated as a fully transparent "blank line": it is never used as an indentation-comparison baseline, nor is it itself subject to one. Such a line is merely recorded into the side comment channel (`comments.rs`, §2.1) and takes no part whatsoever in the indent stack or Newline-token generation.
3. While `bracket_depth > 0`, the leading whitespace of a physical line is never inspected at all (indentation comparison is skipped entirely). Only the tokens within the line are generated; the newline itself generates no token (D-SYN-04) — however, a `#` comment is still ignored as a comment through end of line regardless of bracket depth.
4. Upon reaching a line with `bracket_depth == 0` that is not transparent, **method-chain-continuation lookahead** (D-SYN-05) is performed first: holding off on the fact that the current logical line has not (yet, at this point) ended, if this line's first token is `.` or `|>` and this line's leading whitespace count is greater than **the starting column of the current logical line**, then no Newline is emitted and this line continues as part of the same logical line (its tokens are simply appended). This check repeats across lines (a 5-stage method chain would have the continuation check come out true 4 times in a row). The line where the continuation condition finally comes out false is the true logical-line boundary, and processing proceeds to step 5 from there.
5. Indentation comparison: let `n` be this line's leading whitespace count, and compare it against `*indent_stack.last()`.
   - `n == *indent_stack.last()`: (if there was a preceding logical line) emit Newline and continue tokenizing at the same level.
   - `n == *indent_stack.last() + 4`: emit Newline→Indent, and push `n`.
   - Otherwise, if `n > *indent_stack.last()`: E0501 (an increase not in steps of 4).
   - `n < *indent_stack.last()`: emit Newline, then keep popping elements less than `n` off `indent_stack`, emitting a Dedent for each pop. Success if the pop result lands exactly on `*indent_stack.last() == n`. If the stack is exhausted without hitting a matching value, it's E0501 ("does not match any indentation level" — the equivalent of Python's `unindent does not match`).

This procedure makes physical lines inside brackets or a method-chain continuation completely invisible to the indentation machinery (directly realizing D-SYN-04/05), and comment-only lines / blank lines are never used as an indentation-comparison baseline nor themselves subject to one (directly realizing D-SYN-02's extended definition). At end of file, a Dedent is emitted for each remaining element of the still-open `indent_stack`, followed by Eof.

### 5.2 f-string nesting and brace depth

D-LEX-07's scanning algorithm is implemented as follows. On detecting `f"`, string-scanning mode begins: escapes like `\n` and `\"` are handled the same as in an ordinary string, while `{{`/`}}` are pushed into `FStringPart::Text` as the single characters `{`/`}`. The moment an unescaped, standalone `{` is detected, that's the start of interpolation — scanning starts here at **depth 1**, and from then on: every plain `{` encountered (the start of a dict/set literal) increments depth by 1, every `}` that isn't part of `}}` decrements depth by 1, and **the `}` at which depth reaches 0 is the terminator of the interpolation**. During this, encountering a `"` character (the start of a string literal) is immediately treated as a lexical error (D-LEX-07: "writing a string literal inside expr is forbidden" — this document reports it as E0005, "unknown character/token." Since no dedicated code exists in D-DIAG-02, the decision is to reuse the closest-matching existing code. The prohibition on nested f-strings is likewise automatically achieved by this same rule alone, since nesting would require an f-string's own `"` — no separate prohibition logic is needed).

Yabumi's expression syntax has no block construct requiring `{`/`}` (all blocks are expressed via indentation, §2), so any `{`/`}` appearing inside an f-string's `{expr}` must necessarily be part of a dict/set literal, and the depth-counting above has no ambiguity.

Once the interpolation's terminator is settled, the substring from right after the opening brace to right before the terminator is carved out and **lexed by recursively invoking the same Lexer** into an ordinary token sequence (raising the "string literals forbidden" flag only for this call). The result is pushed as `FStringPart::Expr(Vec<Token>)`. The parser feeds this `Vec<Token>` straight into `parse_expr`, parsing it as an ordinary expression (from that point on it takes no further notice of the fact that it originated from an f-string).

Since an f-string's body can never contain a physical newline (D-LEX-05: multi-line strings are not allowed), this scan always completes within a single physical line and never interacts with §5.1's indentation machinery.

### 5.3 How assignment-target-annotation-driven decode's type information is passed to the evaluator

Take `data: User = json.decode(s)?` as an example. The type-checking phase does the following:

1. Resolves the `NameAssign`'s type annotation `User` to `Ty::Named{name:"User", args:[]}`.
2. When checking the right-hand side `json.decode(s)?`, the return type of `json.decode(s)` — the target of `?` — is `Result[T, Error]` (`T` an as-yet-undetermined type variable). Unifying the `NameAssign`'s expected type (`User`) against the type after applying `?` settles `T = User` (D-TYPE-16: a variable declaration's initializer is one of the 4 contexts where assignment-target-driven inference applies).
3. The settled `T` (`Ty::Named{"User",[]}`) is written into `Resolutions.decode_target`, keyed by the `NodeId` of the `Call` expression `json.decode(s)`.

When the evaluator evaluates this `Call`, once it recognizes the callee is the built-in `json.decode` (knowing from `Resolutions.call_kind` that it's `CallKind::FunctionCall`, and knowing from the name-resolution table that the target is one of the stdlib's decode family), it fetches the `Ty` from `Resolutions.decode_target.get(&call.id)` and **explicitly passes it as a runtime parameter** to the codec implementation:

```rust
// stdlib/codec/mod.rs
pub fn decode(target: &Ty, text: &str, program: &Program) -> Result<Value, YabumiError> {
    match target {
        Ty::Int => decode_int(text),
        Ty::Str => decode_str(text),
        Ty::List(elem_ty) => decode_list(elem_ty, text, program),
        Ty::Named { name, .. } if name.as_ref() == "Value" => decode_dynamic(text),
        Ty::Named { name, .. } => {
            let decl = &program.structs[name];               // The field-name-to-type mapping
            decode_struct(decl, text, program)
        }
        // ..
    }
}
```

This lets the codec body process recursively just by receiving "what is the target shape right now" as a `Ty` **value**, with no need whatsoever for Rust generics (monomorphization along the lines of `decode::<T>`) — the dynamic `Value` case (when `T=Value`) is handled naturally as just one more branch of the same function. Even when the type argument is written explicitly in the syntax, as in `csv.decode[User](s)`, it's likewise passed to the evaluator via the same `Resolutions.decode_target` (or `Resolutions.type_args`) — **the evaluator never once directly references a `TypeAnn`** (a syntactic type-annotation node). Settling a concrete `Ty` from a type annotation is entirely the type-checking phase's responsibility, and the evaluator always receives only an already-settled `Ty` via `Resolutions` — maintaining a consistent layer separation (decision made in this document — this consistency removes any need for the evaluator to reinterpret the meaning of the AST as a whole).

### 5.4 Generics: the relationship between monomorphization and runtime polymorphism

Already settled in §3.8: **type erasure**. The type-checking phase concretizes type variables per call site and verifies against that (this is what D-FUNC-04's "monomorphization" means), but since the evaluator only ever deals with the single dynamic representation `Value`, neither duplication of concretized code nor per-type runtime dispatch ever occurs.

### 5.5 The effect-row inference algorithm (including effect polymorphism for higher-order functions)

**The flaw in the old algorithm** (elaborated in the EFFECT-HOF-POLYMORPHISM decision, §8): naively "just summing up the `Ty::Function.effects` the call expression's callee carries" cannot correctly handle a user-defined, effect-polymorphic higher-order function. In something like `apply[T,U](x: T, f: (T) -> U): U { return f(x) }`, `f`'s **syntactic type annotation** `(T) -> U` can never write a `uses` (SPEC §8: "the user never writes an effect variable"), so at the moment `apply`'s own declaration is checked, `f`'s type always has `effects: EffectSet::empty()`. Using that as-is would mean `apply` always computes "the call effect is empty" no matter what `f` is passed, so even when `read_len_via_apply(path): int uses {fs} { return apply(path, (p) => fs.read(p)...) }` **passes a lambda that actually carries `{fs}` at the call site**, that `{fs}` never gets summed in anywhere (the `apply`/`read_len_via_apply` pair in samples/ok/8_effects/entry_main.ybm is exactly this case). This is precisely the requirement SPEC §8 states explicitly — "if `f` carries `{net}`, then `map(xs,f)`'s caller is also required to carry `{net}`" — i.e. **effect polymorphism determined per call site** — which in principle cannot be computed by looking once at the declaration-time static type.

**The fix: a two-stage structure using an effect-forwarding mask**. For each function/method declaration `g`, a `Vec<bool>` (in parameter order, `Resolutions.hof_forwarding`, §3.7/§4.2) representing "which of the function-typed parameters are actually invoked (not merely held as a value or passed along) within `g`'s body" is computed up front, as **a purely syntactic fact unrelated to types or effects**. This can be computed even while `g`'s own effects are still unsettled (unaffected by mutual recursion or hoisting) — because it's just a single walk determining "does parameter `f` appear as the callee of a `Call`/`ClosureCall`."

```rust
/// Walks g's body once and, for each function-typed parameter, determines "does this
/// parameter itself appear as a call's direct callee within the body." Requires no
/// type checking whatsoever — determined solely by name resolution (which parameter
/// this Ident refers to); this is EffectCheck's first preparatory walk.
fn compute_hof_forwarding(decl: &FunctionDecl) -> Vec<bool> {
    let mut forwarding = vec![false; decl.params.len()];
    walk_direct_callee_idents(&decl.body, &mut |ident_name: &str| {
        if let Some(i) = decl.params.iter().position(|p| p.name.as_ref() == ident_name) {
            forwarding[i] = true;
        }
    });
    forwarding
}
```

This `forwarding` mask is used to compose the actual argument effects at each call site:

```rust
/// Walks a function/method body once and returns the union of (a) the direct
/// effects of concretely known call targets, and (b) the effects of arguments
/// actually passed via forwarding parameters.
fn infer_effects(
    body: &Block,
    type_env: &TypeEnv,
    resolutions: &Resolutions,
    program: &Program,
) -> EffectSet {
    let mut acc = EffectSet::empty();
    walk_calls(body, &mut |call_site: &CallSite| {
        // (a) Ordinary direct effects: sum the effects carried by the callee's type as-is (same as the old algorithm).
        if let Ty::Function { effects, .. } = call_site.callee_ty {
            acc = acc.union(*effects);
        }
        // (b) EFFECT-HOF-POLYMORPHISM: if the callee is a user-defined function with
        //     forwarding parameters, or a higher-order STDLIB method (map/filter/fold/
        //     find/any/all/flat_map/sort_by/par_map/par_each/each, etc.), sum in the
        //     `Ty::Function.effects` of the **actual argument expression itself** passed
        //     at the forwarding position (the type type checking has already settled for
        //     that lambda/function-value expression, D-FUNC-02). The declaration side's
        //     parameter type annotation (always empty effects) is not used.
        for arg_ty in call_site.forwarded_arg_effects(resolutions, program) {
            if let Ty::Function { effects, .. } = arg_ty {
                acc = acc.union(*effects);
            }
        }
    });
    acc
}
```

`CallSite::forwarded_arg_effects` works as follows: if the callee is a user-defined
function/method, it looks up `resolutions.hof_forwarding[callee_id]` and returns the
types of the actual argument expressions at the positions marked true. If the callee
is a higher-order STDLIB method (one whose name is registered as "higher-order" in
the stdlib namespace resolution table — **hardcoded as a fixed rule**, as directed by
the EFFECT-HOF-POLYMORPHISM decision), it returns **all** function-typed actual
arguments (since STDLIB.md's own spec makes it self-evident that such a method's
function-typed parameter is always invoked within its body, no mask computation is
needed — a one-line check of "a function-typed actual argument is unconditionally a
forwarding target" suffices).

Points important to this algorithm working correctly:

- **When a function value is merely stored in a variable/argument/struct field
  (never called)**: per D-EFF-02, it does not propagate. `walk_calls` visits only
  `Call`/`MethodCall`/`ClosureCall` nodes, so an occurrence where a variable `reader`
  is simply passed as an argument or stored into a struct (a reference as an
  `Ident`/`FieldAccess`) is never counted at all. The forwarding determination in (b)
  above likewise only looks at "the expression actually placed at that Call/MethodCall's
  argument position," preserving this distinction.
- **Recursive functions**: per D-FUNC-03, "a recursive function's effects are simply
  summed assuming its own declared `uses`; no fixed-point computation is performed" —
  when a call to `f` itself is encountered inside `f`'s own body, `f`'s type (even
  before its body has finished being checked) is pre-registered in `TypeEnv` **as
  exactly that function's declared `uses`** (every declaration is hoisted, D-SYN-08).
  Computing the `hof_forwarding` mask (syntactic, type-independent) is already
  complete beforehand and is unaffected by this cycle, whether the recursion is
  direct or mutual. After checking the body, if the effects summed from (a) are
  **not a subset** of the declared `uses`, E2002 is reported — **effects originating
  from (b) (forwarded argument effects) are not included in this subset check** —
  `apply` itself can keep no `uses` declaration at all (i.e. `apply` stays "pure"),
  and a forwarded effect only becomes an E2002 target when checking the caller
  (`read_len_via_apply`). This is the part that literally realizes SPEC §8's
  "the caller is also required to carry it."
- **`par`/`par_map`/`par_each`, and every other STDLIB higher-order method**: while
  keeping to the spirit of "no special-casing" (D-FUNC-03), the implementation
  handles them all uniformly via the STDLIB-side hardcoded rule above (a
  function-typed actual argument is unconditionally a forwarding target) — there is
  no `par`-specific branch.
- **A known scope limitation** (decision made in this document): `hof_forwarding`
  looks only one level deep — "is it invoked directly within that function's own
  body." The case where a user-defined higher-order function merely **forwards** the
  function-typed argument it received on to yet another higher-order function/method
  (e.g. `def apply_all[T,U](xs: list[T], f: (T) -> U): list[U] { return xs.map(f) }`,
  where `f` is passed to `map` rather than called directly) means `f` is not included
  in `apply_all`'s `hof_forwarding`, and in that case the effect does not propagate to
  `apply_all`'s caller. Since none of SPEC/DECISIONS/samples/ has a test case
  requiring this multi-level forwarding, it's explicitly documented as a known
  limitation not addressed within the v1 scope (should it become necessary, it can
  be naturally generalized by extending `hof_forwarding`'s computation from this
  single-level judgment to "a small fixed point over the call graph" — since the
  depth of functions and stdlib calls users write is small in practice, such an
  extension would not diverge).

E2001 (an effectful call inside a pure function) is reported at the `Span` of **the
first call expression that brought in a non-empty effect**, whenever checking a
function body whose `uses` declaration is empty (pure) finds that the `EffectSet`
summed from (a)+(b) is non-empty.

### 5.6 The implementation of `?` and the ban on bare `?` inside a par branch

`?` requires two kinds of non-local control that **unwind different scopes**: "early return at a function boundary" and "immediate exit 1 at the top level." On top of that, a panic such as an out-of-range access must unwind the entire process, **ignoring even function boundaries** (D-ERR-06). To implement all three within a single evaluator, we make use of Rust's own `?` operator and its error-type hierarchy.

```rust
/// The result of evaluating an expression/statement. Unifies the two kinds of non-local control into a single type.
pub enum Flow {
    Value(Value),
    /// The early-return signal for `return expr` or `expr?`. Caught and converted
    /// only at a function-call boundary (call_function). If it propagates all the
    /// way to where no caller frame exists (the top level), that structurally
    /// settles it as top-level `?`'s Err/None propagation.
    Return(Value),
}

/// A panic-family signal (D-ERR-04) should abnormally terminate the entire process
/// immediately, with no distinction of function boundaries at all — so, independent
/// of Flow, it's delegated to unwinding via Rust's own `?`.
pub struct Abort(pub Diagnostic);

pub type EvalResult = Result<Flow, Abort>;

fn eval_question(target: &Expr, env: &mut Environment, program: &Program) -> EvalResult {
    let v = match eval_expr(target, env, program)? {   // Rust's `?`: passes an Abort straight through
        Flow::Value(v) => v,
        flow @ Flow::Return(_) => return Ok(flow),       // Pass through if an inner `?` has already early-returned
    };
    match unwrap_result_or_option(&v) {
        Unwrapped::Ok(inner) => Ok(Flow::Value(inner)),
        Unwrapped::ErrOrNone(payload) => Ok(Flow::Return(wrap_for_question(target.span, payload))),
    }
}

/// The function-call boundary. This is the one place where Flow::Return is finally
/// converted into "that call's return value" — the Flow::Return variant itself never
/// leaks out to the caller. Per the function-body value rule (the
/// VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT decision, §8), D-SYN-11's block-value
/// rule (applying only to if/match branches) is not applied to a function body — if
/// the return-type annotation is void, the trailing expression statement (even if it
/// happens to carry some value) is always discarded and Value::Void is implicitly
/// returned. If it's non-void, in a correctly type-checked program eval_block never
/// returns `Flow::Value` (type checking enforces that the body always ends either
/// with an explicit `return`, or with an if/match all of whose branches end in
/// `return` — i.e. "diverges").
fn call_function(decl: &FunctionDecl, args: Vec<Value>, program: &Program) -> Result<Value, Abort> {
    let mut env = Environment::with_frame(bind_params(decl, args));
    match eval_block(&decl.body, &mut env, program)? {
        Flow::Return(v) => Ok(v),
        Flow::Value(v) => Ok(if matches!(decl.ret.kind, TypeAnnKind::Void) { Value::Void } else { v }),
    }
}

/// Sequential execution of top-level statements. Since no caller frame exists here,
/// receiving a Flow::Return at this point settles it as top-level `?`'s Err/None
/// propagation (SPEC §7.2).
fn run_top_level(items: &[Item], env: &mut Environment, program: &Program) -> Result<(), Abort> {
    for item in items {
        let Item::Stmt(stmt) = item else { continue };
        match eval_stmt(stmt, env, program)? {
            Flow::Value(_) => {}
            Flow::Return(payload) => {
                let (code, msg) = toplevel_question_message(&payload); // E6005 or E6006
                return Err(Abort(Diagnostic { code, span: stmt.span, message: msg }));
            }
        }
    }
    Ok(())
}
```

The key point of this design: **Rust's `?` is used directly to propagate `Abort`** (since a panic is free to unconditionally unwind all the way to the top regardless of function-call depth, it exactly matches Rust's native early-return mechanism). Meanwhile, **`Flow::Return` is converted only via an explicit `match`** (converted into "the function's return value" at exactly the one fixed spot, `call_function`; everywhere else — such as the loop in `eval_block` that evaluates statements one by one — it merely relays: "upon receiving a `Flow::Return`, immediately stop evaluating the remaining statements and return that same `Flow::Return` as-is"). The same language feature (Rust's `?`) is deliberately applied at two different granularities to two Yabumi control constructs that unwind different scopes.

**Evaluating a `Return` statement, and D-TYPE-17's implicit wrap** (the IMPLICIT-WRAP-NO-RESOLUTIONS-FIELD decision, §8): here is how `eval/stmt.rs` evaluates `StmtKind::Return(expr_opt)`: if `expr_opt` is `None` (a bare `return`, which type checking allows only for a void function), it's `Flow::Return(Value::Void)`. If it's `Some(expr)`, `expr` is evaluated first to get a Value, and then `resolutions.implicit_wrap.get(&expr.id)` is looked up — if it's `Some(WrapKind::Ok)`/`Some(WrapKind::Some)`, that Value is wrapped in `EnumInstance{type_name: "Result"/"Option", variant_index: 0, variant_name: "Ok"/"Some", fields: vec![v]}` (D-TYPE-17 priority 2) before becoming `Flow::Return`. If there's no entry (priority 1: the annotation already matched the type from the start), it becomes `Flow::Return(v)` as-is — `check_stmt.rs`'s Return-statement check determines which of D-TYPE-17's three cases applies, and writes into `implicit_wrap`, keyed by `expr.id`, only for priority 2 (priority 3 = E1020 never reaches the evaluator).

**The function-body value rule and "divergence"** (the VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT decision, §8): the rule `check_decl.rs` uses when checking a FunctionDecl body is made independent from D-SYN-11 (which applies only to if/match branches), and defined as follows. A block can be determined to "diverge" (syntactically guaranteed to never execute any more of this function, and instead hand control to a `return` or to another diverging block): (1) it diverges if the block's last statement is `Return(_)`. (2) it diverges if the block's last statement is an ExprStmt that merely evaluates an if-expression/match-expression all of whose branches diverge (both the then/else, or every match arm, recursively satisfies (1) or (2)). If a function body's Block can be determined to diverge, type checking requires nothing further — it's considered to satisfy the declared return type even without a `return` (a diverging block, in fact, never "produces" a value of that type at all, so it can't conflict with any return type — treated analogously to Rust's `!` type). If it doesn't diverge: when the return-type annotation is `void`, it's always legal regardless of the kind/type of the last statement (the trailing value is simply discarded). When it's not `void`, the last statement must be `Return(Some(expr))` (its type is checked after going through D-TYPE-17's wrap rule); otherwise (the block ends in an ExprStmt/VarDecl/Discard/etc. and does not diverge) it's E1020. Under this single, consistent rule, both samples/ok/5b_return_implicit_ok_some_wrap (`if n == 0: return None else: return 1.0/float(n)` — a diverging pattern where both branches end in return) and samples/ok/9_concurrency_par (a pattern where a void function's body ends in just a single expression statement carrying a value, `fs.write(...)`) pass type checking without contradiction.

**The ban on bare `?` inside `par` (D-PAR-03)** is implemented not at type checking but at the **syntax level** (D-PAR-03 itself explicitly states "equivalent to a syntax error, E0502"). While parsing each element of `par [..]`/`par (..)`, and a lambda passed directly as an argument (the argument of a call whose **method name matches the string** `.par_map(...)`/`.par_each(...)`), the parser raises a `bare_question_forbidden` flag. This flag:
- **Stays raised as-is** even when entering an `if`/`match` block (which doesn't create a new `?` scope, D-ERR-02).
- Is **cleared** the instant a new `Lambda` body is entered (since that lambda itself becomes the boundary of a new `?` scope — D-ERR-02: "`?`'s scope is the innermost function").
- If a `Question` expression (`expr?`) is parsed while the flag is raised, rather than building an ordinary `ExprKind::Question` node, E0502 is immediately pushed into the `DiagnosticBag` (the expression itself is still built as an ordinary Expr so parsing can continue — in keeping with D-CLI-03's spirit of collecting every finding).

### 5.7 Immediate termination for panics, and trace display

Per D-ERR-05, "the full call stack is never displayed (a single frame only)," so **no call-traceback mechanism is implemented at all** — `Abort(Diagnostic)`'s `Diagnostic.span` is simply the `Span` of the expression that caused the panic (`xs[i]`, `a / b`, `assert(...)`, `.unwrap()`) itself, and it's enough to print that as-is, in one line, as `file:line:col [E6xxx] message`.

Each check's implementation is scattered across `eval/ops.rs` (arithmetic), `eval/lvalue.rs` (indexing), `stdlib/builtins.rs` (assert), and `stdlib/result_option.rs` (unwrap), but all of them build an `Abort` via the same helpers:

```rust
// eval/panic.rs
pub fn overflow(span: Span) -> Abort { Abort(Diagnostic { code: ErrorCode::IntegerOverflow, span, message: "panic: integer overflow".into() }) }
pub fn div_by_zero(span: Span) -> Abort { /* E6002 */ todo!() }
pub fn out_of_range(span: Span, detail: &str) -> Abort { /* E6001 */ todo!() }
// ..
```

The arithmetic operator implementation folds in overflow checking simply by using Rust's `checked_*` family of methods directly:

```rust
BinaryOp::Add => a.checked_add(b).map(Value::Int).ok_or_else(|| panic::overflow(span))?,
BinaryOp::Div => {
    if b == 0 { return Err(panic::div_by_zero(span)); }
    // i64::MIN / -1 is the one division case that goes out of i64's range
    // (overflow) — caught in one shot by checked_div (an easy-to-miss edge
    // case, noted here for the record).
    a.checked_div(b).map(Value::Int).ok_or_else(|| panic::overflow(span))?
}
```

**Stack overflow (E6008)** differs in nature from the other panics — it's Rust's own call stack running out, which manifests as the process being forcibly killed by an OS signal (SIGSEGV, etc.), so left as-is it cannot achieve what D-ERR-05 requires ("exit 1 + the standard diagnostic format," since termination via a signal doesn't produce a normal exit code). So the evaluator **never relies on Rust's native stack at all**, and instead keeps its own explicit recursion-depth counter:

```rust
// eval/mod.rs

/// Made thread-local (the R9 decision, §8): adding a `depth` argument to
/// `call_function`'s signature would ripple out across the evaluator's entire call
/// graph (eval_expr/eval_stmt and the many other mutually recursive functions),
/// becoming unwieldy. More fundamentally, depth is a value approximating "how much
/// of the actual call stack a single OS thread has consumed," and each of `par`'s
/// worker threads (Program is merely Arc-shared via std::thread::scope) has its own
/// completely independent, fresh 64MiB stack — deep recursion in one branch must
/// never affect the depth of another branch or of the thread that spawned the
/// worker. `thread_local!` satisfies exactly this requirement of "independently
/// starting from zero, per thread" (Program is shared via `Arc`, while only the
/// depth counter is never shared).
const MAX_CALL_DEPTH: u32 = 8_000; // A threshold tuned with headroom on the Rust stack in mind

thread_local! {
    static CALL_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct DepthGuard(());
impl Drop for DepthGuard {
    fn drop(&mut self) { CALL_DEPTH.with(|d| d.set(d.get() - 1)); }
}

fn enter_call(span: Span) -> Result<DepthGuard, Abort> {
    let n = CALL_DEPTH.with(|d| {
        let n = d.get() + 1;
        d.set(n);
        n
    });
    if n > MAX_CALL_DEPTH { return Err(panic::stack_overflow(span)); }
    Ok(DepthGuard(()))
}
```

`call_function` acquires this guard first (propagated via `?`, with depth automatically restored on Drop — correctly decremented whether unwinding happens via `Flow::Return` or via `Abort`), so that this threshold is reached **before** Rust's actual call stack genuinely runs out. To keep this threshold comfortably below Rust's real stack limit, the evaluator's execution itself runs on **a dedicated thread with an explicitly specified stack size** (`std::thread::Builder::new().stack_size(64 * 1024 * 1024)`). This isn't merely for extra headroom — it's required **for cross-platform consistency**: the main thread's default stack size is around 8MB on macOS/Linux but only 1MB on Windows, so evaluating directly on the main thread would make the recursion depth at which E6008 triggers wildly different across operating systems (non-determinism that violates "what you see is what happens"). Immediately on startup, `main.rs` spins up a dedicated thread and runs the entire pipeline inside it, deciding the exit code from the `join()` result — this same design applies the same explicit stack size to `par`'s worker threads as well (§5.8), and `thread_local!`'s `CALL_DEPTH` likewise automatically starts fresh (from 0) for each such worker thread.

### 5.8 The implementation of par

**The flaw in the old design** (elaborated in the PAR-ABORT-NOT-ACTUALLY-IMMEDIATE decision, §8): an implementation of "join all threads, then look for the first Abort" cannot prevent another branch's observable side effects (e.g. `print`) from reaching real stdout before the Abort is even detected. samples/err/runtime/par_panic_aborts_immediately/entry_par_branch_panics.ybm is designed so that one branch panics immediately on division by zero while the other branch does `print("slow branch finished")` after `time.sleep(300)`, and its `expected.toml` requires stdout to be exactly empty (exact match) — an implementation that "waits for every thread to join before checking" would only detect the Abort after this print has already been written to real stdout following the 300ms sleep, so this test would reliably fail.

**The fix**: each thread's completion (Ok/Abort) is detected via an `mpsc` channel **in completion order**, and the moment the first Abort is received, the diagnostic is printed immediately and `std::process::exit(1)` is called. The remaining threads are never joined — this is safe because process termination forcibly discards everything, threads included (this bypasses, in this one specific respect — terminating the process itself — the convention `std::thread::scope` normally guarantees, "join every spawned thread before returning"; `process::exit` passes straight through Rust's entire destructor mechanism, including a scope's Drop, so it does not conflict with the scope's contract).

```rust
// concurrency/mod.rs
pub struct WorkerPool {
    // Because std::thread::scope is used, there's actually no fixed-size persistent
    // pool — a scoped thread is spun up on the spot for each execution of a par
    // construct (below). The OS thread-creation cost adds up if there's a huge
    // number of par calls, but this is negligible at the shell-script-replacement
    // scale SPEC targets (the largest branch count among the 89 samples is a
    // handful) — avoiding the implementation cost of a dedicated thread pool and
    // the complexity of reuse logic (decision made in this document, the
    // ponytail-style minimal implementation).
}

/// OS thread-creation failure is a genuinely unrecoverable resource exhaustion at
/// startup, and representing it with `unreachable!()` would be inaccurate (it's not
/// a logic bug). An example applying the R3 decision (§8) — attaching
/// `#[expect(clippy::expect_used)]` at the function level.
#[expect(
    clippy::expect_used,
    reason = "OS thread-creation failure is not a logic bug but startup-time resource exhaustion; unreachable!() would be inaccurate. Since it's unrecoverable, just terminate via expect"
)]
fn spawn_par_branch<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    tx: mpsc::Sender<(usize, EvalResult)>,
    idx: usize,
    expr: &'scope Expr,
    mut local_env: Environment,
    program: Arc<Program>,
) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn_scoped(scope, move || {
            let result = eval_expr(expr, &mut local_env, &program);
            let _ = tx.send((idx, result)); // Fine to ignore a send failure if the receiver has already exited
        })
        .expect("thread-creation failure terminates the whole process immediately (see the expect allowance above)");
}

pub fn eval_par_list(elements: &[Expr], env: &Environment, program: &Arc<Program>) -> EvalResult {
    let n = elements.len();
    let captured: Vec<Environment> = elements.iter().map(|_| env.snapshot_for_par()).collect();
    // snapshot_for_par: Value::clone()s every variable visible in the current frame
    // (merely bumping Arc reference counts — D-MUT-04's value copy). No RefCell/Mutex
    // needed — each element gets a fully independent copy of Environment.
    let (tx, rx) = mpsc::channel::<(usize, EvalResult)>();
    let mut slots: Vec<Option<Value>> = (0..n).map(|_| None).collect();

    std::thread::scope(|scope| {
        for (idx, (expr, local_env)) in elements.iter().zip(captured).enumerate() {
            spawn_par_branch(scope, tx.clone(), idx, expr, local_env, Arc::clone(program));
        }
        let mut received = 0usize;
        while received < n {
            // Arrives in completion order (not declaration order). The moment even
            // one Abort is seen, we stop waiting for the rest and terminate — this
            // is the substance of "immediately," and it literally satisfies D-ERR-06:
            // "no fail-fast applies only to Result/Option value propagation, and does
            // not apply to a panic."
            let (idx, result) = rx.recv()
                .unwrap_or_else(|_| unreachable!("thread count == send count, so exactly n messages are guaranteed to arrive"));
            match result {
                Ok(Flow::Value(v)) => { slots[idx] = Some(v); received += 1; }
                Ok(Flow::Return(_)) => unreachable!("a bare `?` in a par branch is already syntactically excluded via E0502 (D-PAR-03)"),
                Err(abort) => {
                    eprintln!("{}", abort.0.render(&program.sources));
                    std::process::exit(1); // Never joins the remaining threads — they're taken down with the process
                }
            }
        }
    });

    // Only reached once every message has been received — D-PAR-01 (input-order
    // guarantee) is satisfied by writing into slots at the sender-assigned idx,
    // independent of the receive order (completion order).
    let values = slots.into_iter()
        .map(|v| v.unwrap_or_else(|| unreachable!("the receive loop only exits once every slot has been filled")))
        .collect();
    Ok(Flow::Value(Value::List(Arc::new(values))))
}
```

Points worth noting:

- **Copying captures**: `snapshot_for_par` value-copies from the current `Environment` and `move`s an independent `Environment` into each thread. There is no mutable state shared across threads at all (only `Arc<Program>` is shared, and that's safe since it's immutable after construction).
- **Result-ordering guarantee (D-PAR-01)**: reception happens in completion order, but since each message writes into `slots` (an array already allocated in declaration order) at its original `idx`, the final result is always in input order — no reordering logic dependent on completion order itself is needed.
- **How a panic inside `par` is handled (D-ERR-06)**: DECISIONS draws a clear line that the rule "no fail-fast — wait for everything to finish" (SPEC §9) applies **only to the propagation of a Result/Option error value**, and does not apply to a genuine panic (`Abort`). The implementation above calls `process::exit(1)` the moment the first Abort is detected, without waiting for other branches to finish, so any subsequent observable side effect from another branch (a print, etc.) can never occur in principle (that branch's OS thread may technically still be alive, but because the entire process terminates immediately, no further Rust code in it ever runs again). No mechanism to genuinely force-kill other threads at the OS level is implemented, nor is one needed — process termination substitutes for that role.
- `par (f(), g())` (the tuple form) follows the same-shaped code path, differing only in that it packs the result into a `Value::Tuple`. `par_map`/`par_each` differ only in that the element count is determined at runtime, applying the same closure to each element of an already-evaluated `list[T]` rather than to a sequence of expressions (the closure's `captured` is value-copied into each thread exactly as `Closure.captured` from §3.9). `par_each` returns `void`, so `slots` needs only a completion flag rather than `Option<Value>` (it ultimately returns `Value::Void`). Nesting (D-PAR-02, calling `par` inside `par`) is naturally achieved simply by `eval_par_list` recursing — no special handling is needed.

### 5.9 fmt's idempotence

**Approach: AST regeneration rather than token-stream preservation.** Every formatting rule (D-FMT-01–06) reduces "multiple semantically equivalent ways of writing something" to a single canonical form, and the information needed to determine that canonical form is fully covered by the AST (plus the small handful of syntactic hints described below). An approach that keeps the token stream and rewrites whitespace incrementally becomes complicated to implement against a rule where "the token sequence itself can change" — such as repositioning brackets (D-SYN-04 leaves the closing bracket's position free) or converting between multi-line and single-line form (D-FMT-05).

To guarantee idempotence (fmt . fmt = fmt), **all that needs guaranteeing is that every normalization decision is uniquely determined by its input** — AST regeneration inherently has this property (the same AST always produces the same text). The one thing to watch for is that **some rules cannot be reproduced unless the AST itself retains part of the original source's shape**:

1. **D-FMT-05 (the trigger for multi-line expansion)**: "was there one or more newlines between the opening and closing bracket" is a **syntactic fact** that cannot be reconstructed from the AST's semantic structure (what the list's elements are). So, as shown in §3.4, `ListLit`/`DictLit`/`SetLit`/`TupleLit`/`Call`/`MethodCall` carry a `was_multiline: bool` — just a single bit recording whether the parser consumed a `Newline` token between the open and close brackets in the original source. Idempotence holds as follows: when fmt sees `was_multiline=true` and formats it across multiple lines (one element per line), the **output itself** then genuinely contains a newline between the open and close brackets. Reparsing this output, the parser again observes `was_multiline=true` — so a second run of fmt makes the same decision. The single-line case is symmetric (no newline in the output means `false` on reparse too). By keeping just this one bit of input-dependent information — "did the source originally have a newline" — in the AST, the idempotence rationale D-FMT-05 itself states ("the same input always yields the same output") is faithfully reproduced even under the AST-regeneration approach.
2. **Comment preservation**: lexing never discards comments (a standalone `#` line, a trailing comment, an ordinary non-doc comment) — `comments.rs` collects them as a side channel (§2.1). The parser (`comment_attach.rs`) looks at each comment's line number and attaches it either as `leading_comments: Vec<String>` on the very next AST construct (`Stmt`/`MatchArm`/`EnumVariant`, etc.), or as `trailing_comment: Option<String>` on a construct whose preceding token is on the same line (already defined as real fields on `Stmt`/`MatchArm`/`EnumVariant` in §3.4). `##` doc comments (`DocComment` in §3.4) are already designed as a target of this same attachment process, covering `FunctionDecl`/`StructDecl`/`EnumDecl` as well as `Stmt` (limited to `StmtKind::NameAssign`, the DOC-COMMENT-MISSING-ON-STMT-LEVEL-CONST decision, §8) — **this mechanism of "attaching a comment to a following/same-line AST node by line number" is the single, shared implementation behind both D-DOC-03 (determining which declaration a doc comment targets) and fmt's general comment preservation**. When fmt's `printer.rs` formats each node, it prefixes it with a formatted `# ` + one space (D-FMT-03) if `leading_comments` exist, and suffixes it the same way at end of line if `trailing_comment` exists.
3. **Excluding code inside doc fences (D-FMT-06)**: `DocFence.raw_text` is preserved verbatim as raw text, and when `printer.rs` outputs a `DocComment`, it formats `prose_lines` like an ordinary comment, but outputs each `DocFence`'s `raw_text` completely unchanged, byte for byte. This means fence code is preserved exactly as written even if it isn't in canonical form, even after formatting.
4. **A known limitation** (elaborated in the R8 decision, §8): **comments between elements of a literal or argument list are not supported**. The comment-attachment mechanism above only gives `leading_comments`/`trailing_comment` to statement-level nodes that could plausibly have a comment before or after them, such as `Stmt`/`MatchArm`/`EnumVariant`. There is no mechanism to preserve a comment placed **between elements** of a multi-line `list`/`dict`/`set`/`tuple` literal or a function call's argument list (e.g. `[\n  1,  # first\n  2,  # second\n]`) — the elements of `ExprKind::ListLit` etc. are a raw `Vec<Expr>` with no per-element comment field. No sample under samples/fmt/ exercises this case, so it has no impact on the acceptance tests, and it is deliberately accepted as out of scope for v1. Should it become necessary later, this can be addressed by extending the element type of `Arg` or of each literal from `Expr` to a small wrapper type of "`Expr` + 2 comment fields" (an extension point deliberately left open without significantly reshaping the existing AST).

`printer.rs` recursively walks the entire AST, applying as fixed rules: D-FMT-01 (spacing around operators/commas/colons), D-FMT-02 (strings are always double-quoted), D-FMT-03 (comment spacing), D-FMT-04 (one pipe stage per line), and D-TYPE-02 (trailing comma when multi-line) — none of these branch on the input (D-FMT-05 alone is the exception requiring the one bit of input-dependent information, `was_multiline`), so they are trivially idempotent.

### 5.10 Standalone execution of doctests

The concrete means of running each `DocFence` (with no language tag) as an "independent program" is to **set up one new `Environment` frame while sharing the existing `Program` (global declarations) as-is** — there is no need to spin up a new process or OS thread.

```rust
// doctest/mod.rs
pub struct BlockResult { pub line: u32, pub outcome: Outcome }
pub enum Outcome { Pass, Fail(Diagnostic) }

pub fn run_fence(fence: &DocFence, program: &Program) -> BlockResult {
    // 1. Lex and parse fence.raw_text as an independent statement sequence. The
    //    parser interprets it as an ordinary sequence of top-level statements
    //    (struct/enum/def declarations may also be written, though doc tests in
    //    SAMPLES_PLAN.md are chiefly expected to be sequences of assert statements).
    let items = match parse_fence_body(&fence.raw_text, fence.body_start_line, fence.span.file) {
        Ok(items) => items,
        Err(diag) => return BlockResult { line: fence.body_start_line, outcome: Outcome::Fail(diag) },
    };
    // 2. Type-check scoped over "every declaration in the entry file plus
    //    same-directory modules" (D-MOD-04/D-DOC-03). This calls the same
    //    type-checking routine `ybm check` uses, additionally checking just this
    //    items sequence against the already-declared Program.
    if let Err(diag) = typecheck_fence(&items, program) {
        return BlockResult { line: fence.body_start_line, outcome: Outcome::Fail(diag) };
    }
    // 3. Run sequentially with a fresh Environment (D-DOC-04: an independent
    //    execution context per block, unaffected by another block's assert failure).
    let mut env = Environment::with_frame(HashMap::new());
    match run_top_level(&items, &mut env, program) {
        Ok(()) => BlockResult { line: fence.body_start_line, outcome: Outcome::Pass },
        Err(Abort(diag)) => BlockResult { line: fence.body_start_line, outcome: Outcome::Fail(diag) },
    }
}
```

An `assert` failure immediately ends that block's execution (D-DOC-04) — this is a direct consequence of `run_top_level` propagating an `Abort` straight through via `?`, requiring no doctest-specific logic at all (`assert` is evaluated as an ordinary built-in function call and simply returns `Err(Abort(..))` on failure, going through the exact same path as any other panic or top-level `?` propagation). This `run_fence` is called independently for every `DocFence` and the results tallied (a top-level function in `doctest/mod.rs`). One block's `Fail` has no effect whatsoever on another block's execution, since each invocation touches only its own independent `Environment`.

### 5.11 The parser's recursion-depth guard (elaborated in the R4 decision, §8)

The policy established in §5.7 — "the evaluator keeps an explicit depth counter and never relies on Rust's native call-stack limit at all" — had not been applied to the parser's own recursive-descent-plus-Pratt-style parsing. Nested expressions (`((((...))))`), nested list/dict/set/tuple literals, and recursive expression parsing across a method chain are all implemented as recursive calls within `parser/expr.rs`, so pathologically deep input (a deliberate attack, or broken code an LLM generated by mistake) could exhaust the parser's own Rust call stack. Because the entire pipeline runs on a thread with a dedicated stack size (§4.5/§5.7), the ceiling rises to roughly the same level as the evaluator's — but a higher ceiling isn't the same as "no ceiling," and exceeding the threshold would still crash via an OS signal, failing to satisfy the `file:line:col [Exxxx] message` format D-ERR-05 requires.

**The fix**: give the `Parser` struct itself a `depth: u32` field, threaded through an RAII guard that increments/decrements it every time nested expression/literal/bracket parsing is entered (the same pattern as the evaluator's `DepthGuard`, but since a `Parser` never crosses threads and exists as a single value, `thread_local!` isn't needed — a single struct field suffices).

```rust
// parser/mod.rs
const MAX_PARSE_DEPTH: u32 = 2_000; // One level of Pratt-style parsing tends to
                                      // consume more stack frame than one evaluator
                                      // call, so this is set lower than
                                      // MAX_CALL_DEPTH (decision made in this
                                      // document; adjustable based on measurement).

pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: u32,
    bare_question_forbidden: bool, // D-PAR-03 (§5.6)
    diagnostics: DiagnosticBag,
}

struct ParseDepthGuard<'p, 'a>(&'p mut Parser<'a>);
impl Drop for ParseDepthGuard<'_, '_> {
    fn drop(&mut self) { self.0.depth -= 1; }
}

impl<'a> Parser<'a> {
    fn enter_nesting(&mut self, span: Span) -> Result<ParseDepthGuard<'_, 'a>, Diagnostic> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(Diagnostic {
                code: ErrorCode::UnexpectedToken, // Reuses E0502 (no dedicated code is introduced, decision made in this document)
                span,
                message: "expression nesting is too deep".into(),
            });
        }
        Ok(ParseDepthGuard(self))
    }
}
```

`self.enter_nesting(span)?` is called at the entry point of expression parsing (`parse_expr`), of parsing list/dict/set/tuple literal elements, and of recursive descent inside parentheses, respectively, and the returned guard is held for that scope (automatically decremented on Drop). Exceeding the threshold reuses the existing E0502 (the general syntax-error code) rather than adding a new diagnostic code — respecting D-DIAG-02's closed code table.

### 5.12 The priority order of identifier resolution: local variable vs. built-in namespace vs. stdlib-restricted overload

D-LEX-01 establishes that "built-in namespace names (`fs`/`http`/…/`toml`) belong to a name-resolution system separate from the flat namespace (D-TYPE-07), and don't collide with a user's top-level definition of the same name," but it did not itself state the priority order for resolving which meaning applies to `json.decode(s)` (syntactically the same shape as an ordinary `MethodCall{receiver: Ident("json"), ..}`) (elaborated in the NAMESPACE-QUALIFIED-ACCESS-NO-RESOLUTION-HOME decision, §8).

**The identifier-resolution priority order** (decision made in this document): when the receiver of a `.`-qualified access (`MethodCall`/`FieldAccess`) is a bare `Ident`, `check_expr.rs` resolves it in the following order.

1. **Does a local variable/parameter of that name exist in the current lexical scope** (an ordinary scope-chain search)? If so, evaluate it as an ordinary expression (a namespace interpretation is **invalid within this lexical scope** — a local variable can shadow a namespace identifier).
2. If not, does the name match one of the 12 fixed namespace identifiers (`NamespaceId::from_name`)? If so, record `NodeId → NamespaceId` for this `Ident` expression into `resolutions.namespace_ref`, and resolve `.method(...)`/`.field` as that namespace's stdlib function/constant.
3. If neither applies, resolve it as an ordinary Ident from the flat namespace (D-TYPE-07, top-level functions/constants) (an ordinary undefined-identifier error if not found).

This ordering exactly matches D-LEX-01's concrete example (defining a top-level function/constant/variable named `json` doesn't affect the meaning of `json.decode(...)`) — a top-level identifier is only ever referenced in step 3, and step 2 (namespace) matches before that, so `json.decode(...)` is always resolved as a namespace. On the other hand, if a scope happens to have a **local variable** named `json`, step 1 matches first, and it resolves as an ordinary access to that local variable — the reason top-level and local are treated differently is that, more than protecting against unintended shadowing of a namespace name by a local variable, it's safer to make the naming-collision accident SPEC as a whole anticipates in LLM-generated code (accidentally naming a local variable `math` or `time`) behave with the intuitive "works as a local variable, exactly as it looks."

**Resolving stdlib-restricted overloads** (D-STDPOL-01, elaborated in the OVERLOAD-DISPATCH-MECHANISM-UNSPECIFIED decision, §8): `print`/`eprint`/`assert` (4/4/2 signatures) and `list[int].sum`/`list[float].sum` are **exceptions** to D-TYPE-07's general flat-namespace rule, "one name = one definition, duplicates are E1001." This exception is handled via a **separate path** from ordinary user-defined name resolution (one name = one definition, subject to E1001 checking), using a fixed table `stdlib/mod.rs` holds (`(name, tuple of argument types) → implementation`) — module_resolve's E1001 check simply looks at whether "the user is trying to newly define a name `print`/`eprint`/`assert` (regardless of signature)" and immediately flags E1001 (a collision with an already-registered prelude name, per D-TYPE-07). When the user writes a legitimate call (`print(x)`), it isn't even subject to E1001 checking at all — `check_expr.rs` looks at the call argument's type (int/float/bool/str) and selects one signature from the fixed table above, a dispatch that's independent of ordinary function-call resolution. Since `sum` is a MethodCall, the receiver's type (`list[int]` or `list[float]`) directly becomes the dispatch key, and it's naturally handled by the same mechanism used to resolve any other built-in collection method (selecting by the receiver's concrete type) — a dedicated table like the one for `print`/`eprint`/`assert` is unnecessary for `sum`.

---

## 6. Testing strategy

### 6.1 The design of the samples/ acceptance-test harness

`tests/samples.rs` (a Cargo integration test) is the single entry point. The implementation approach:

```rust
// tests/samples.rs (overview)
#[test]
fn run_all_samples() {
    let ybm_bin = env!("CARGO_BIN_EXE_ybm");
    let proc_fixture_bin = env!("CARGO_BIN_EXE_proc_fixture");   // §6.2
    let http_base = support::spawn_mock_http_server();            // §6.2, started once within the test process
    let mut failures = Vec::new();
    for dir in support::discover_sample_dirs("samples") {          // Every directory that has an expected.toml
        for case in support::parse_expected_toml(&dir.join("expected.toml")) {
            if case.requires_env.iter().any(|v| v == "YABUMI_TEST_HTTP_BASE") { /* set http_base */ }
            if case.requires_env.iter().any(|v| v == "YABUMI_TEST_PROC_BIN") { /* set proc_fixture_bin */ }
            match support::run_case(ybm_bin, &dir, &case) {
                Ok(()) => {}
                Err(msg) => failures.push(format!("{}/{}: {msg}", dir.display(), case.id)),
            }
        }
    }
    assert!(failures.is_empty(), "{} case(s) failed:\n{}", failures.len(), failures.join("\n"));
}
```

`support::run_case` converts one case from `expected.toml` into an actual `std::process::Command` invocation:

- It converts `cmd` (`run`/`check`/`check_diff`/`test`) into `["<entry>"]` / `["check", "<entry>", "--apply"]` (if `args` places `--apply` before the entry, that's reflected instead — the default `check` form places it after) / `["check", "<entry>"]` / `["test", "<entry>"]`.
- If `stdin_file` is set, that file's contents are piped into the process's standard input (empty otherwise).
- After execution: it checks that `exit_code` matches, that `stdout`/`stderr` match according to their `mode` (`exact`/`contains`), and extracts lines matching `\[E\d{4}\]` from `stderr` to verify they match `diagnostics` (including order).
- If `cmd = "check"` and `fmt_result_file` is specified, the `entry` file's contents after the `--apply` rewrite are compared byte-for-byte against `fmt_result_file` (**the original `entry` is backed up before execution and always restored once the test finishes** — since `check` applies fmt on a copy, this ensures the test never leaves the repository's sample files dirty; decision made in this document).
- If `cmd = "test"` and `doc_blocks` is specified, for each entry it verifies "does a `[Exxxx]` diagnostic corresponding to that `line` exist in stderr (if a fail is expected) / not exist (if a pass is expected)."
- `notes`/comment lines have no effect on the test (ignored as human-facing explanation).

The sample bodies (`.ybm`/`expected.toml`) are treated as immutable input to this harness (essentially read-only) — the only exception is the in-place rewrite from a `cmd = "check"` case (which invokes `ybm check --apply`), handled via the backup/restore described above.

### 6.2 How YABUMI_TEST_HTTP_BASE / YABUMI_TEST_PROC_BIN are realized

**`YABUMI_TEST_PROC_BIN`**: uses Cargo's multiple-binary-target feature directly. Add to `Cargo.toml`:

```toml
[[bin]]
name = "ybm"
path = "src/main.rs"

[[bin]]
name = "proc_fixture"
path = "tests/fixtures/proc_fixture/main.rs"
```

(`proc_fixture` has no dependency whatsoever on the `ybm` binary itself — an independent binary of a few dozen lines). `cargo test` automatically builds every binary target it depends on, so the integration test can obtain its full path at compile time via `env!("CARGO_BIN_EXE_proc_fixture")` — requiring no manual build-order or path bookkeeping, this is the most idiomatic way to create a "separate test binary" in Rust testing. `proc_fixture/main.rs` is a program of a few dozen lines that dispatches directly on `match std::env::args().collect::<Vec<_>>().as_slice()`, implementing SAMPLES_PLAN.md §1.4.2's contract table (`echo <text>`/`fail <code>`/`cat`) as-is.

**`YABUMI_TEST_HTTP_BASE`**: listens via `std::net::TcpListener::bind("127.0.0.1:0")` (port 0 = let the OS choose a free port), obtains the port number actually assigned from `listener.local_addr()`, and sets `YABUMI_TEST_HTTP_BASE` to `http://127.0.0.1:<port>`. A dedicated thread runs a `for stream in listener.incoming()` loop, hand-writing a minimal HTTP/1.1 parser/responder for each connection sufficient to satisfy SAMPLES_PLAN.md §1.4.1's contract table (8 endpoints) (reading the request line and headers up through `\r\n\r\n`, then reading the body for exactly `Content-Length` more bytes if present, is a sufficiently simple implementation — chunked transfer etc. isn't in the contract table, so it needn't be supported). This is implemented in `tests/support/http_mock.rs`, is purely test code, and has no effect whatsoever on `[dependencies]` (it doesn't grow the product binary's dependencies — for this same reason, adding a small crate such as `tiny_http` to `[dev-dependencies]` would also be architecturally fine for this specific purpose, but since the contract table is small and fixed at 8 endpoints, this document proposes avoiding even that additional dev-dependency and implementing it with `std::net` alone). It's started once for the whole test run and shared across multiple cases (avoiding the cost of restarting it per case).

```rust
// tests/support/http_mock.rs (overview)
// A bind/local_addr failure represents "the test execution environment can't even
// use TCP loopback" — an unrecoverable startup-time abnormality — so representing
// it with `unreachable!()` would be inaccurate. Since the test crate is also subject
// to the unwrap_used/expect_used deny lints (§6.3), a function-level #[expect(...)]
// is used instead (the R3 decision, §8).
#[expect(
    clippy::expect_used,
    reason = "A bind/addr-fetch failure on the test loopback socket is an unrecoverable environment abnormality, not something unreachable!() accurately describes. Just terminate on the spot when the test harness starts up"
)]
pub fn spawn_mock_http_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || handle_one(stream)); // One thread per connection — plenty for test purposes
        }
    });
    format!("http://127.0.0.1:{port}")
}
```

This harness may also need a `[dev-dependencies]` addition solely for TOML-parsing `expected.toml` (it would technically be possible to use the hand-rolled `toml` decode/encode as well, but to avoid a circularity — the test harness depending on the implementation it's testing — the test harness instead uses an independent, lightweight TOML parser, or a minimal hand-written parser needing only `std`. Given that `expected.toml`'s schema is nothing more than a simple `[[case]]` array of tables plus scalar values, this document proposes **implementing a harness-only, hand-written, minimal TOML reader in `tests/support/toml_lite.rs`** — this also has the parallel-implementation benefit of letting harness development start without waiting for the product's `toml` codec implementation to be finished (§7).

### 6.3 Where unit tests are placed

- A `#[cfg(test)] mod tests { .. }` is placed inside each module file (standard Rust convention). Targets: lexing (comparing token sequences against expected values), the parser (comparing AST structure against expected values, plus a dedicated table-driven test exhaustively covering D-OP-01's precedence table), individual type-checking rules (embedding small `.ybm` fragments as string literals and checking them, one per D-TYPE-xx/D-MUT-xx unit), each fmt rule (comparing before/after strings), codecs (round-tripping each of JSON/YAML/TOML/CSV decode/encode), and property checks for the hand-rolled calendar arithmetic and PRNG (leap-year boundaries, known outputs for known seed sequences).
- `tests/samples.rs` sits **above** these unit tests, specializing in catching the kind of inconsistency where "each individual rule is correct but things break once integrated" (e.g. forgetting to pass Resolutions between phases) — it is not meant to rediscover errors the unit tests could already catch (since running it is heavy).
- `cargo clippy --all-targets --all-features -- -D warnings` naturally applies to the code under `tests/` as well, so the harness itself is also subject to the `unwrap`/`expect` ban — test code is broken up into small helper functions that return `Result`, with the final `assert!`/`panic!` (a `panic!` inside test code is exempt from clippy's `unwrap_used`/`expect_used` and is allowed) folding everything into the error message.

### 6.4 The policy on clippy::pedantic (elaborated in the R5 decision, §8)

`Cargo.toml` sets `clippy::pedantic` to `warn`, but since CI runs with `-D warnings`, which **turns every lint into an error**, a pedantic warning also becomes a build-failure cause — this is spelled out explicitly to avoid the misreading "pedantic is just a warn, so it can wait." **Unit 0's (the skeleton's) completion criteria include not just denying `unwrap_used`/`expect_used`/`allow_attributes`/`dbg_macro`, but also `cargo clippy --all-targets --all-features -- -D warnings` — including `clippy::pedantic` — passing with zero warnings** (see Unit 0's completion criteria in §7). The skeleton created this round has itself already been verified against this bar (see the execution results at the end of §7).

The standard responses this document adopts for the pedantic lints that will come up frequently as later implementation units (Units 1–18) implement the rest of the body:

| lint | Common situation | Standard response |
|---|---|---|
| `missing_errors_doc` | `pub fn ... -> Result<_, _>` | Write at minimum a one-line `/// # Errors` section (a brief note on when it returns `Err`). At a stage (Unit 0) where a function is still only `todo!()` and its concrete failure conditions aren't yet settled, place `#![expect(clippy::missing_errors_doc, reason = "still at the skeleton stage; docs will be written during implementation")]` at the top of the module, and have whoever implements that Unit's body add the docs alongside the implementation and remove this `#![expect]`. |
| `missing_panics_doc` | `eval/panic.rs` and stdlib APIs annotated `panics` (STDLIB.md §14) | Document the panic conditions in a `/// # Panics` section (since these actually just return an `Abort` and never trigger a real Rust panic, this is rephrased in context as "the condition under which it terminates immediately with E6xxx per D-ERR-04"). |
| `module_name_repetitions` | `diagnostics::Diagnostic`/`diagnostics::DiagnosticBag`, `ast::decl::FunctionDecl`, etc. | Names are not changed (prioritizing consistency by keeping type names exactly as the terminology SPEC/DECISIONS/this document has settled on). The relevant spots get `#[expect(clippy::module_name_repetitions, reason = "to keep the module name and type name consistent with the terminology in DECISIONS/this document")]` attached per type. |
| `must_use_candidate` | Pure functions such as `Diagnostic::render`, `EffectSet`'s `union`/`is_subset_of` | `#[must_use]` is simply added (since these are side-effect-free functions, there's no real cost, and it's actually useful for catching forgot-to-use-the-result bugs). |
| `too_many_lines` | The body of `check_expr.rs`'s expression checking, `printer.rs`'s formatting body, and other `match`es that tend to grow proportionally with the number of AST variants | The completion criteria for each relevant Unit include a policy, applied at implementation time, of splitting "checking per expression kind" into separate functions (`check_call`/`check_binary`, etc.) (a note handed off to whoever implements Unit 7/10). At the skeleton stage there's no function body long enough for this to apply yet, so it's left untouched for now. |
| `similar_names` | Similar-looking identifiers such as `lhs`/`rhs`, `decl`/`decl2` | At implementation time, rename to names with distinct meaning (`left_ty`/`right_ty`, etc.). The policy is to fix each spot as the lint flags it, rather than establishing a rule up front. |

Of these, `#[expect(...)]` is attached only where a warning has actually fired at the skeleton (Unit 0) stage — no preemptive `#[expect(...)]` is added on the grounds that "it might come up in the future" (if the targeted lint never actually fires, `#[expect(...)]` itself becomes an "unfulfilled lint expectation" warning, so an unnecessary `#[expect(...)]` would actually break CI).

---

## 7. Implementation phase breakdown

This chapter was written at the point the skeleton was built (Unit 0), to break the implementation into 19 units (Unit 0–18) so that multiple agents could implement it in parallel. **All 19 units are now complete**: across `src/`'s 88 files plus `tests/`'s 5 files (about 39,800 lines total), `cargo build --all-targets` / `cargo clippy --all-targets --all-features -- -D warnings` (including pedantic) / `cargo fmt --all -- --check` / `cargo test --all-features` all succeed (507 passed / 0 failed / 1 ignored — the single `#[ignore]` is due to a relative-path dependency in `src/stdlib/fs.rs` and is unrelated to any incomplete implementation). The acceptance tests under `samples/` (89 directories, 254 files; `run_all_samples` in `tests/samples.rs`, included in `cargo test --all-features` with no `#[ignore]`) also all pass, all 171 cases. Actual `todo!()`s number zero.

This chapter is kept as a record of how the subsequent work (§7.2) actually proceeded, and how the original completion criteria were verified. §7.3 (dependency waves) held up structurally exactly as originally described even as implementation proceeded, so its original text is preserved as-is. §7.1's original file-exclusivity rule applied to the initial implementation; the later LSP extension added the `src/lsp/` files and the necessary module wiring in Unit 17. Points that changed from the original design during implementation are summarized in §7.4.

### 7.1 The file-exclusivity principle

So that multiple parallel agents would never edit the same file at the same time, work proceeded under a policy that names exactly one exception up front and otherwise strictly holds to "one file = one unit":

> **Unit 0 (the skeleton, now complete) first created every original file under `src/` (every file listed in the then-current §2.1 tree) as a stub containing only complete, compiling type definitions and signatures, and also settled `Cargo.toml`'s dependency section and `[[bin]]` targets.** As a result, `pub mod` declarations in a file like `mod.rs` — the kind of file one is tempted to touch every time a new submodule gets added — and declarations of dispatch functions spanning multiple stdlib submodules, as in `stdlib/mod.rs`, **were all written out in full at Unit 0**. The later LSP extension was the explicit addition to this rule: Unit 17 added the new `lsp/` module and wired it into the existing module declarations and driver. Apart from that extension, no unit after Unit 0 edited a `mod.rs`/`main.rs`-equivalent file, nor any file owned by another unit — the constraint that each unit only needed to fill in the **function bodies** (replacing `todo!()` with real logic) of the files assigned to it was maintained through to the end.

**A correction to Unit 0's completion criteria (a fix made while building the skeleton)**: the previous version had two mutually contradictory sentences in the same paragraph — "the implementation body is filled with `unreachable!()` rather than `todo!()`" and "`todo!()` triggers a runtime panic, so cargo test would fail across the board." The policy actually adopted is **to use `todo!()` exactly as the task instructions direct**, and `cargo test` does not fail across the board — `#[test]` functions are written to verify only the fully implemented types (§3.1-3.9), and no `#[test]` calling a function that still has `todo!()` is written (or, where one exists, it's explicitly disabled with `#[ignore]`). At the skeleton stage there was indeed a state where running `main()` itself, or `run_all_samples` in `tests/samples.rs`, would reach a `todo!()` and panic — but that was the deliberate, intended "declaration that this is not yet implemented," and none of `cargo build`/`cargo clippy`/`cargo fmt --check`/`cargo test` were made to fail by it. Now that every unit is complete, not a single `todo!()` remains — in substance or in comment.

Once Unit 0 was complete, each of the following units only needed to edit the "files it touches" as declared, with no conflict against any other unit. **Units 1, 3, and 6's type definitions themselves were already fully implemented at the skeleton stage (per the requirements of §3.1/§3.2/§3.4-3.7), so the implementation content in the table below for them is either zero or minimal additional work.**

### 7.2 Unit list (completion status)

Legend: the "Implementation content" column gives representative examples of the functions/logic each unit actually filled in (which, at the skeleton stage, existed as `todo!("...")` calls in each file). The "Completion criteria (status)" column shows the result of confirming, against the corresponding tests/sample directories, that the originally set completion criteria were actually met.

| Unit | Files touched | Implementation content (representative) | Depends on | Completion criteria (status) |
|---|---|---|---|---|
| **Unit 0** Skeleton | `Cargo.toml`, every original file under `src/`, all 5 files under `tests/` | — (complete) | None | **Done.** `cargo build --all-targets` / `cargo clippy --all-targets --all-features -- -D warnings` (including pedantic) / `cargo fmt --all -- --check` / `cargo test --all-features` all succeed. |
| **Unit 1** Diagnostics foundation | `src/diagnostics/{mod.rs,codes.rs,source_map.rs}` | `SourceMap::slice` (Position → byte-offset conversion) | 0 | **Done.** `Diagnostic::render`/`DiagnosticBag::into_sorted`/`ErrorCode` (all 53 D-DIAG-02 codes)/`SourceMap::slice` are implemented and the unit tests pass. |
| **Unit 2** Lexing | `src/lexer/{mod.rs,cursor.rs,fstring.rs}` (`token.rs`/`comments.rs` already complete as types only) | `Cursor::{peek,peek2,bump}`, `scan_fstring`, `Lexer::tokenize` | 1 | **Done.** The unit tests tokenizing `samples/ok/2_lexical_basics`/`6-4_strings` into `Vec<Token>` pass. E0001–E0005 in `samples/err/static/2_lexical_errors`/`2_syntax_errors` come out as expected. Implements the algorithms of §5.1/§5.2. |
| **Unit 3** AST definitions | `src/ast/{mod.rs,expr.rs,stmt.rs,decl.rs,pattern.rs,ty_ann.rs}` | — (complete) | 0 | **Done.** Every node type (Expr/Stmt/Decl/Pattern/TypeAnn and auxiliary types) is settled down to its fields and compiles. The `NodeId`-assignment policy is documented in a comment in `ast/mod.rs`. |
| **Unit 4** Parsing | `src/parser/{mod.rs,expr.rs,stmt.rs,decl.rs,pattern.rs,ty_ann.rs,comment_attach.rs}` | `parse_module`, `Parser::{parse_expr,parse_pipe,parse_logical,parse_comparison,parse_arithmetic,parse_unary,parse_postfix,parse_primary,parse_fstring_segments}`, `Parser::{parse_block,parse_stmt,parse_if,parse_match_arm}`, `Parser::{parse_items,parse_function_decl,parse_struct_decl,parse_enum_decl}`, `Parser::{parse_pattern,parse_sub_pattern}`, `Parser::parse_type_ann`, `attach_comments` | 2, 3 | **Done.** Every `.ybm` across all 40 directories under `samples/ok/` parses into a `Module`. `samples/err/static/2_syntax_errors` (E0501–E0503) and `9_par_branch_bare_question_operator` (E0502, D-PAR-03) come out as expected. Includes a table-driven test exhaustively covering the D-OP-01 precedence table. Also verified that exceeding `MAX_PARSE_DEPTH`'s threshold (the R4 decision) emits E0502. Adds `parse_module_with_offset` here, which wasn't in the original plan (see §7.4). |
| **Unit 5** Module resolution | `src/module_resolve/{mod.rs,flat_namespace.rs,module_grammar.rs}` | `discover_sibling_modules`, `build_program_skeleton`, `register_flat_namespace`, `check_module_toplevel_grammar`, `check_module_directive_syntax` | 3, 4 | **Done.** Every case in `samples/ok/10a`–`10c` and `samples/err/static/10a`–`10d` produces the expected `Program` skeleton, or E1001/E5001/E5002. |
| **Unit 6** Type foundation (Ty/EffectSet/NamespaceId) | `src/types/mod.rs` | — (complete; though `Ty::Unknown`, described below, is added later by Unit 7) | 3 | **Done.** `Ty`/`EffectSet`/`NamespaceId` are settled down to their fields and derives, and the unit tests for `EffectSet::{union,is_subset_of,from_name}`/`NamespaceId::from_name` pass. |
| **Unit 7** Type checking | `src/types/{env.rs(part),infer.rs,generics.rs,exhaustiveness.rs,mutability.rs,check_expr.rs,check_stmt.rs,check_decl.rs}` (`resolutions.rs` already complete as types only) | `infer::{unify,infer_with_expected}`, `generics::{instantiate_generics,substitute,check_type_param_operator_usage}`, `exhaustiveness::check_exhaustiveness`, `mutability::{check_mutable_place,check_self_mutation_allowed}`, `check_expr::check_expr`, `check_stmt::{check_stmt,check_block_value,block_diverges}`, `check_decl::{check_function_decl,check_struct_decl,check_enum_decl,check_all_decls}` | 5, 6 | **Done.** Type checking across everything under `samples/ok/` comes back with zero errors (verified via a unit test calling the type-checking phase alone directly). E1xxx/E3001 under `samples/err/static/` (excluding E2xxx/E5xxx) all come out as expected. `Resolutions`'s `field_index`/`decode_target`/`bare_ident_kind`/`call_kind`/`ident_def`/`expr_ty`/`implicit_wrap`/`namespace_ref` (excluding `hof_forwarding`) get filled in exactly per the §5.3/D-SYN-06/IMPLICIT-WRAP/NAMESPACE-QUALIFIED decisions. `block_diverges` (the "divergence" determination from §5.6) passes both samples/ok/5b_return_implicit_ok_some_wrap and samples/ok/9_concurrency_par without contradiction. Adds `Ty::Unknown` here, which wasn't in the original plan (see §7.4), and changes `TypeEnv` from a lifetime-carrying linked list to an ownership-based scope stack (see §7.4). |
| **Unit 8** Effect checking | `src/effects/mod.rs` | `compute_hof_forwarding`, `infer_effects`, `check_function_effects`, `check_program_effects`, introducing the `ENTRY_POINT_NAME` convention (see §7.4) | 7 | **Done.** `samples/ok/8_effects` (including the EFFECT-HOF-POLYMORPHISM verification via `apply`/`read_len_via_apply`) and `samples/err/static/8_effect_errors` come out as expected. `Resolutions.hof_forwarding` is correctly filled via the two-stage structure (§4.2). |
| **Unit 9** Lint | `src/lint/{unused_variable.rs,unused_function.rs,shadowing.rs,unreachable.rs,naming.rs}` (`mod.rs`'s `check_all` already wired up) | Each file's `check` (all implemented assuming the `ENTRY_POINT_NAME` convention) | 7 | **Done.** All 5 directories under `samples/err/lint/` come out as expected. |
| **Unit 10** fmt | `src/fmt/{printer.rs,doc_fence.rs}` (`mod.rs`'s `format_module`/`has_diff` already wired up) | `printer::print_module`, `doc_fence::render_doc_comment` | 3, 4 | **Done.** Byte-for-byte matches before/after formatting across the 10 directories under `samples/fmt/`, `fmt.fmt=fmt` idempotence, read-only `ybm check` diff detection, and explicit `--apply` rewriting all come out as expected. Satisfies D-FMT-06 (doc-fence contents excluded). |
| **Unit 11** Evaluator core | `src/eval/{mod.rs,env.rs,expr.rs,stmt.rs,call.rs,lvalue.rs,ops.rs,value.rs}` (`panic.rs` already complete) | `run_top_level`, `Environment::{bind,push_scope,pop_scope,snapshot_for_par}`, `eval_expr`/`unwrap_result_or_option`/`wrap_for_question`, `eval_block`/`eval_stmt`, `call_function`/`call_closure`/`bind_params`, `resolve_place`, `eval_binary`/`eval_unary`, `MapKey::{to_value,try_from_value}` | 5, 6, 7 | **Done.** `samples/ok/4_mutability` through `7-5_assert`, and `14_memory_model_value_semantics`, all exit 0 under `ybm` execution with matching assert content. E6001–E6004/E6007/E6008 under `samples/err/runtime/` come out as expected. §3.10's chained Arc::make_mut, the `thread_local!` CALL_DEPTH (the R9 decision), and the Value::Void branch (the VOID-VALUE decision) all work. |
| **Unit 12** stdlib (pure portion) | `src/stdlib/{prelude.rs,primitives.rs,collections.rs,result_option.rs,value_type.rs,math.rs,regexns.rs}` | Each file's stdlib functions (e.g. `math.rs`'s min/max/abs_float/sqrt/pow, `collections.rs`'s list_len/is_empty/contains, and `fs.rs`'s exists — an example already implemented back at the skeleton stage), introducing the `regex` crate, `prelude::install` (unifying every return type to `void`, see §7.4), `stdlib::resolve_overload`/`stdlib::resolve_namespace_function` | 11 | **Done.** `samples/ok/3-1_primitives`, `3-2_collections`, `3-3_stdlib_types`, `3-6_generics`, `6-2_iterators`, `7-4_safe_apis`, `11-2_math`, `11-2_regex` come out as expected. `resolve_overload`/`resolve_namespace_function` (the D-STDPOL-01/NAMESPACE-QUALIFIED decisions) are implemented exactly per the completion criteria, but `types/check_expr.rs`/`eval/call.rs` continue to perform the same determination via their own separate fixed tables and never call these functions (a known duplication, see §7.4). |
| **Unit 13** stdlib (codec) | `src/stdlib/codec/{mod.rs,json.rs,yaml.rs,toml.rs,csv.rs}` | `codec::{decode,encode}`, `json::{parse_to_dynamic,dynamic_to_string}`, `yaml::{parse_to_dynamic,dynamic_to_string}`, `toml::{parse_to_dynamic,dynamic_to_string,is_valid_root_type}`, `csv::{decode,encode,decode_rows}` | 11, 12 | **Done.** `samples/ok/11-1_codec_json_yaml_toml_csv` and `samples/err/static/11-1_toml_encode_root_type_error` come out as expected. Introduces `indexmap` (for Value::Dict), implements §5.3's Ty-driven decode. |
| **Unit 14** stdlib (effectful I/O) | `src/stdlib/{fs.rs,http.rs,envns.rs,proc.rs,time.rs,rand.rs,builtins.rs}` | `fs::{read,read_bytes,write,append,list,remove}`, `http::{get,post,put,delete,request}`, `envns::{get,set,args,stdin}`, `proc::run`, `time::{now,sleep,format,parse,days_from_civil,civil_from_days}`, `rand::{Prng::*,int,float,bool_,choice,shuffle}`, `builtins::{print,eprint,assert_bare,assert_with_message}` | 11, 12 | **Done.** `samples/ok/11-2_fs`, `11-2_env`, `11-2_time`, `11-2_rand`, `11-3_builtins_print_eprint_assert` come out as expected. Introduces `ureq`+rustls here. `11-2_http`/`11-2_proc` are verified as acceptance tests only after Unit 18's mocks/fixtures are set up (`tests/support/http_mock.rs`, `tests/fixtures/proc_fixture`), and come out as expected. |
| **Unit 15** Concurrency | `src/concurrency/mod.rs` (`spawn_par_branch` already wired up) | `eval_par_list`, `eval_par_map` | 11 | **Done.** `samples/ok/9_concurrency_par` and `samples/err/runtime/par_panic_aborts_immediately` (verifying stdout is completely empty, the PAR-ABORT-NOT-ACTUALLY-IMMEDIATE decision) come out as expected. Confirmed that completion-order reception via the mpsc channel plus `process::exit(1)` on the first Abort combines correctly with `spawn_par_branch`. |
| **Unit 16** Doctest | `src/doctest/mod.rs` (`run_all_fences` already wired up) | `run_fence`, `safe_fence_id_base` (a regression countermeasure paired with §7.4's `parse_module_with_offset`) | 7, 11 | **Done.** All 6 directories under `samples/doctest/` come out as expected. The concern that `NodeId`s reassigned by the fence-dedicated Parser might collide with real declarations' `NodeId`s is resolved via `safe_fence_id_base` (verified by the regression test `fence_larger_than_declaration_does_not_corrupt_resolutions`). |
| **Unit 17** CLI / driver / LSP | `src/cli/{mod.rs,args.rs}`, `src/driver.rs`, `src/lsp/` (`src/main.rs` already wired up — the dedicated 64MiB-stack thread launch + join + ExitCode determination is implemented) | `parse_args`, `cli::dispatch`, `driver::run_pipeline`, `driver::analyze`, and the LSP JSON-RPC server | 2,4,5,7,8,9,10,11,12,13,14,15,16 (every unit) | **Done.** The 4 subcommands (`ybm <file>`/`ybm check [--apply] <file>`/`ybm test <file>`/`ybm lsp`) plus the read-only `check` default, explicit `--apply` rewrite, and stdio LSP server behave per §4. The LSP uses the shared front end for diagnostics, hover, definition, and formatting without executing source. `samples/ok/1_cli_subcommands`, `12_fmt_lint_clean_baseline`, `15_end_to_end_showcase`, and `samples/err/cli/file_and_extension_errors` cover the file-based commands. |
| **Unit 18** Acceptance-test harness | `tests/samples.rs`, `tests/support/{mod.rs,http_mock.rs,toml_lite.rs}` (`tests/fixtures/proc_fixture/main.rs` and the in-harness `spawn_mock_http_server` already complete) | `run_case`, `support::discover_sample_dirs`, `toml_lite::parse_expected_toml`, `http_mock::handle_one` | 0 (activated once Unit 17 is complete) | **Done.** The harness is implemented exactly per §6.1/6.2's design, and `run_all_samples` carries no `#[ignore]`, running as part of the ordinary `cargo test --all-features` invocation and verifying every expectation across the 89 directories, 254 files, and 171 cases under `samples/` — all green (the original plan's approach of "remove `#[ignore]` and run via `-- --ignored`" was not adopted; it settled instead into simply running unconditionally). |

### 7.3 Dependency waves (a guide to parallelization)

Grouping each unit by "the earliest point at which it could start, once all of its dependencies are complete" yields the following 10 waves (units within the same wave are mutually independent and can be started in parallel). This breakdown still reflects the module-to-module dependencies exactly as they actually are, now that implementation is complete, and it's kept on record because it helps in understanding the structure of the codebase as a whole.

| Wave | Units that become startable | Units completed immediately before |
|---|---|---|
| 0 | Unit 0 (skeleton) | — |
| 1 | Unit 1 (diagnostics), Unit 3 (AST), Unit 18 (test-harness skeleton) | 0 |
| 2 | Unit 2 (lexing), Unit 6 (Ty/EffectSet) | 1, 3 |
| 3 | Unit 4 (parsing) | 2, 3 |
| 4 | Unit 5 (module resolution), Unit 10 (fmt) | 3, 4 |
| 5 | Unit 7 (type checking) | 5, 6 |
| 6 | Unit 8 (effect checking), Unit 9 (lint), Unit 11 (evaluator core) | 7 (11 also depends on 5, 6) |
| 7 | Unit 12 (stdlib pure portion), Unit 15 (concurrency), Unit 16 (doctest) | 11 (16 also depends on 7) |
| 8 | Unit 13 (stdlib codec), Unit 14 (stdlib effectful I/O) | 11, 12 |
| 9 | Unit 17 (CLI/driver/LSP) | All of 2,4,5,7,8,9,10,11,12,13,14,15,16 |

Unit 18 can be started in wave 1, but `tests/samples.rs` doesn't actually go all green until Unit 17 (wave 9) is complete — this "execution verification" is the sole final convergence point waiting on every unit, while every other unit's startability is determined purely by the local dependencies in the table above. Waves 2, 4, 6, 7, and 8 each allow 2–3 units in parallel, so the maximum degree of parallelism is roughly 3.

### 7.4 Design changes made during implementation, and known duplication

From the original design (§1–§6, §7.2's completion criteria), the following changes/additions arose in the course of implementation. None affect the correctness of the logic or the tests, but they're worth knowing as background when reading the code.

- **Changed `TypeEnv` from a lifetime-carrying linked list to a scope stack** (see the comment at the top of `src/types/env.rs`). At the stub stage the design was "a linked list holding a borrowed reference to its parent" (`parent: Option<&'parent TypeEnv<'parent>>`), but it turned out that in the usage pattern where `check_expr`/`check_stmt` recursively pass around `&mut TypeEnv<'_>` while creating and discarding several levels of child scope, lifetime handling was a constant struggle — so this was changed to the same ownership-based design as `eval::env::Environment`'s `Frame.scopes`: a `Vec<HashMap<_,_>>` stack, pushed/popped at block boundaries.
- **Added `Ty::Unknown`** (`src/types/mod.rs`). Once a single diagnostic has been reported during type checking (an unannotated parameter E1002, an unresolved identifier, etc.), this is a recovery placeholder type — one that "silently conforms to any expected type" — used so that expressions further using that value don't trigger a cascade of unrelated additional diagnostics. The same type is also reused as the type of an if/match expression as a whole when all its branches diverge. It's a purely internal type that never remains in a fully diagnosed program, and no other phase or the evaluator ever references it.
- **Added `parse_module_with_offset`** (`src/parser/mod.rs`). Performs the same work as `parse_module`, but its `NodeId`-assignment counter can start from an arbitrary `start_id`, and it returns the number of `NodeId`s consumed. This was added because, when parsing each doctest `##` fence independently with a dedicated `Parser`, the `NodeId`s that fence reassigns could numerically collide with the `NodeId`s on the real declaration side (`safe_fence_id_base` in `src/doctest/mod.rs` computes a safe starting offset from the real declarations' total node count and passes it in here).
- **The `$entry` synthetic-function convention (`ENTRY_POINT_NAME`)** (`src/effects/mod.rs`). After TypeCheck completes and before EffectCheck/Lint run, driver.rs assembles a synthetic `FunctionDecl` (`name: "$entry"`, other fields empty/`Void`) whose `body` holds the entry file's top-level executable statements, registers it into `program.functions`, and only then calls EffectCheck/Lint. EffectCheck excludes this name from the declared-effect-overrun check (the top level implicitly permits every effect), and lint (`unused_variable.rs`/`unused_function.rs`/`shadowing.rs`/`naming.rs`) likewise assumes this convention when determining "is this directly under the entry point."
- **Unified the return type of the prelude's built-in placeholders to void** (`src/stdlib/prelude.rs`). Originally `int`/`float`/`str`/`set` were each given their "proper" return type, but since `types/check_decl.rs`'s `check_all_decls` unconditionally applies the general rule "a function whose return type isn't void can't have an empty body," these placeholders' empty bodies violated that rule, causing a bug where, along every path calling `prelude::install` → `check_program`, TypeCheck for **every** user program would fail with a false-positive BranchTypeMismatch. Giving each individual placeholder a body consistent with its return type was also considered, but since that would complicate handling `set`'s generic return type `set[T]`, the "a representative signature that's never actually executed" framing was pushed all the way through instead, unifying all of them to `void` with an empty body.
- **A known duplication: dual management of the stdlib signature table** (`resolve_overload`/`resolve_namespace_function` in `src/stdlib/mod.rs`). These were implemented because ARCHITECTURE.md §7.2's Unit 12 completion criteria required them, but `types/check_expr.rs` (type checking) and `eval/call.rs` (evaluation) both continue to perform the same determination directly via their own separate fixed signature tables, and never call these functions. This is partly a consequence of a deliberate design decision to keep type checking (compile-time signature resolution) and evaluation (runtime value dispatch) separate, and it remains as dual management, where multiple locations independently reference the signature's source of truth (`docs/STDLIB.md`).
- **The Unit 18 harness settled on not using `#[ignore]`**. §7.2's original completion criteria envisioned an operational model of "remove `#[ignore]` from `run_all_samples` and run via `cargo test --all-features -- --ignored`," but it was actually implemented with no `#[ignore]` on `run_all_samples` at all, so it's simply included as-is in an ordinary `cargo test --all-features`.

---

## 8. Responses to the critique

All 16 findings raised in the adversarial review (3 blocker, 8 major, 5 minor, of which R1–R9 were 9 findings submitted separately) have been adjudicated. **All 16 were adopted** — none were rejected. Each finding's basis was confirmed against actual files in samples/ before ruling on it (for 4 of them — EFFECT-HOF-POLYMORPHISM, PAR-ABORT-NOT-ACTUALLY-IMMEDIATE, VOID-VALUE, and DOC-COMMENT-MISSING — the relevant sample was read directly while writing this document, confirming the exact contradiction claimed does in fact exist), and in every case the finding's proposed remedy was adopted as-is (or with only a minor correction) rather than being re-adjudicated into "the diagnosis is correct, but a different fix is better."

### blocker (3 findings, all adopted)

- **EFFECT-HOF-POLYMORPHISM**: Adopted. §5.5 was rewritten wholesale, introducing an "effect-forwarding mask" per function/method declaration (`Resolutions.hof_forwarding`, §3.7). A user-defined higher-order function's body is walked once, syntactically, to settle "which of its function-typed parameters are directly invoked," and the actual effects of the actual arguments are composed in at each call site accordingly. STDLIB higher-order methods (map/filter/fold/find/any/all/flat_map/sort_by/par_map/par_each/each, etc.) hardcode the fixed rule the finding proposed, exactly as suggested — "a function-typed actual argument is unconditionally a forwarding target." The limitation that only one level of direct invocation is tracked — multi-level forwarding (a higher-order function merely passing the function it received on to yet another higher-order function) is not tracked — is stated explicitly: since none of samples/ requires this multi-level forwarding, it's documented as a deliberate scope limitation for v1 (at the end of §5.5).
- **PAR-ABORT-NOT-ACTUALLY-IMMEDIATE**: Adopted. §5.8 was rewritten wholesale, changing the design to receive each branch's completion (Ok/Abort) in completion order via `std::sync::mpsc`, printing the `Diagnostic` and calling `std::process::exit(1)` the instant the first Abort is received. The remaining threads are never joined (they're discarded, taken down with the process). `sources: Arc<SourceMap>` was added to `Program` (§3.11), making it possible to render a diagnostic even from deep within a worker thread.
- **IMPLICIT-WRAP-NO-RESOLUTIONS-FIELD**: Adopted. Added `implicit_wrap: HashMap<NodeId, WrapKind>` to `Resolutions` (§3.7). `WrapKind` was kept to just the two variants `Ok`/`Some` — the third variant, `None`, present in the finding's sample code, is adequately expressed, for D-TYPE-17 priority 1 (already matching the annotation, no wrap needed), by "no entry exists"; giving it its own variant would make that variant semantically empty, so while keeping the spirit of the finding (adding a dedicated field), the structure alone was simplified. The design where `eval/stmt.rs`'s Return evaluation (§5.6) consults this to wrap is spelled out explicitly.

### major (8 findings, all adopted)

- **VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT**: Adopted. Added a `Void` variant to `Value` (§3.9). D-SYN-11's block-value rule is now stated explicitly to apply only to if/match branches (in the comment on `Block`'s definition in §3.4), and a separate rule was established for function bodies (§5.6, "the function-body value rule and 'divergence'") — a void-declared function always discards the trailing expression statement's value regardless of its kind and implicitly returns Void, while a non-void function requires the trailing statement to be an explicit `return`, or the body to be a "diverging block" where every branch ends in return. Via the concept of "divergence," it was confirmed that both samples/ok/5b_return_implicit_ok_some_wrap (where both branches of an `if`/`else` end in return) and samples/ok/9_concurrency_par (where a `void` function's body is a single expression statement with no return) pass type checking without contradiction, from a single unified rule.
- **NAMESPACE-QUALIFIED-ACCESS-NO-RESOLUTION-HOME**: Adopted. Added `namespace_ref: HashMap<NodeId, NamespaceId>` to `Resolutions` (§3.7), and established `NamespaceId` (the 12 fixed namespaces) newly in types/mod.rs. Established §5.12, spelling out explicitly the identifier-resolution priority order (local-scope binding > fixed namespace name > flat-namespace top-level identifier).
- **DOC-COMMENT-MISSING-ON-STMT-LEVEL-CONST**: Adopted. Removed `Decl::Const`/`ConstDecl` from the AST (§3.4), unifying the design so that a module-level constant and an entry file's ordinary top-level assignment are always built as the identical `Item::Stmt(Stmt)` (bringing the parser's actual output fully in line with the design §4.2 had originally intended — "module_resolve assigns meaning after the fact"). Added `doc_comment: Option<DocComment>` to `Stmt` (a D-DOC-03 target, meaningful only for `StmtKind::NameAssign`) along with `leading_comments`/`trailing_comment` (for fmt, §5.9). The same kind of fmt comment fields were also added to `EnumVariant`/`MatchArm`.
- **OVERLOAD-DISPATCH-MECHANISM-UNSPECIFIED**: Adopted. Established "resolving stdlib-restricted overloads" in §5.12, spelling out explicitly that the fixed overload tables for `print`/`eprint`/`assert`/`sum` are a separate path from the ordinary E1001 name-collision check (D-TYPE-07).
- **R1 (Rc→Arc)**: Adopted. Unified `Rc<...>` to `Arc<...>` throughout this document (42 locations in total, including identifier-name fields, `Program`'s declaration tables, `Closure.captured`, and `CallTarget`). `Value` itself (§3.9) was already `Arc` from the start and so was out of scope (this very inconsistency was the crux of the finding).
- **R2 (resolve_place returning a Result)**: Adopted. Revised §3.10's `resolve_place` to return `Result<&mut Value, Abort>`, using `Vec::get_mut` for a list index and `IndexMap::get_mut` for a dict key, converting a `None` into `panic::out_of_range` (E6001). This resolved both the Rust-native panic from a raw index operator, and the misuse of `unreachable!()` on a reachable branch that actually depends on runtime data.
- **R3 (expect_used violation)**: Adopted. Changed `Environment::lookup_mut`'s `.expect("frame")` into `unwrap_or_else(|| unreachable!(..))` (since Environment's own construction method itself guarantees the invariant "always at least one frame"). For genuinely unrecoverable startup-time errors (a `par` worker-thread spawn failure, the test harness's TCP bind), of the two options the finding presented, the one adopted was "attach `#[expect(clippy::expect_used, reason=..)]` at the function level" (`spawn_par_branch`, `spawn_mock_http_server`, §5.8/§6.2) — since these are environmental failures rather than logic bugs, and `unreachable!()` would be inaccurate for them.
- **R5 (a pedantic guide)**: Adopted. Established §6.4, spelling out that Unit 0's completion criteria include a clean `-D warnings` including `clippy::pedantic`, and tabulating the standard response for each frequently occurring lint (missing_errors_doc/missing_panics_doc/module_name_repetitions/must_use_candidate/too_many_lines/similar_names).

### minor (5 findings, all adopted)

- **R4 (parser recursion depth)**: Adopted. Established §5.11, introducing a guard of the same shape as the evaluator's `DepthGuard` (a `depth` field on the `Parser` struct; no `thread_local!` needed), designed so that exceeding the threshold reuses the existing E0502 (no new diagnostic code is added).
- **R6 (StructInstance derive Clone)**: Adopted. Corrected to `#[derive(Debug, Clone, PartialEq)]` (§3.9).
- **R7 (Value's PartialEq and Closure comparison)**: Adopted. Clarified the reference to a hand-written implementation, and spelled out explicitly the rule that comparing two `Value::Closure`s always returns `false` (never `true`, even reflexively against itself) — on the grounds that an `==` comparison, where an unconstrained type parameter `T` has been unified to a function type, is theoretically reachable, a fixed `false` was adopted rather than `unreachable!()` (§3.9).
- **R8 (fmt not supporting comments inside a literal's interior)**: Adopted. Documented as a known limitation in §5.9 — no structural change was made; it was simply documented as deliberately out of scope for v1 (judged to need no implementation change, since samples/ has no test exercising this case).
- **R9 (where the call-depth counter lives)**: Adopted. Adopted a thread-local counter via `thread_local!` (§5.7) — since each `par` worker thread has its own independent, brand-new 64MiB stack, depth should likewise start independently from zero, and this was judged superior to adding a field to `Environment` (the alternative the finding presented) in that it avoids introducing a depth argument into `call_function`'s signature at all.

### Items added in this revision that weren't in the critique

- **A missing definition for `LambdaBody`**: the previous version of this document referenced `CallTarget::Lambda(Arc<LambdaBody>)` (§3.9), but the type definition of `LambdaBody` itself was never once shown in the body text. It has been added to `eval/value.rs` as `pub struct LambdaBody { pub params: Vec<LambdaParam>, pub body: Expr }` (§3.9).
