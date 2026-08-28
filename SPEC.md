# Yabumi Language Specification

A self-contained single-file scripting language for Agent Skills (SKILL.md bundled scripts), intended for LLMs as a bash/python alternative.

Design priorities: **zero-dependency distribution > machine-readable errors > correct on the first try > permission auditing**.
Overall philosophy: **"appearance = behavior"** - eliminate implicit behavior and make concurrency, mutability, and error propagation visible in syntax.

---

## 1. CLI

There are exactly four subcommands.

| Command | Behavior |
|---|---|
| `ybm <file>` | Run type checking, then execute only on success |
| `ybm check <file>` | Type check + fmt diff check (no rewrite) + lint. Also type-check doc-test blocks (do not execute them) |
| `ybm test <file>` | Run doc tests |
| `ybm lsp` | Start a Language Server Protocol server on stdin/stdout. Analyze open files but do not execute them |

- `ybm check --apply <file>`: Rewrite with fmt. Without `--apply`, do not rewrite and show the diff
- `ybm lsp` takes no additional command-line arguments; see [`docs/LSP.md`](./docs/LSP.md) for detailed features and client configuration
- Exit code: success = 0. Type errors, lint warnings, fmt diffs (during the default check), test failures, and runtime Err termination = 1. LSP exits 0 on normal shutdown or stdin EOF, and 1 on communication errors or exit before shutdown
- Diagnostic format: `file:line:col [E0000] message`. Error codes are stable and machine-readable

## 2. Lexical Rules

- Extension: `.ybm`
- Shebang: allow `#!/usr/bin/env ybm` on the first line (ignore it during execution)
- Comments: `#` line comments only. No block comments
- Doc comments: `##` immediately before a declaration
- **Blocks use indentation only**. Trailing colons are forbidden. Indentation is exactly 4 spaces; tabs are syntax errors
- Encoding: UTF-8

## 3. Type System

Statically typed. Nominal typing (no structural subtyping). No inheritance or traits.

### 3.1 Primitives (lowercase)

| Type | Representation |
|---|---|
| `int` | i64 |
| `float` | f64 |
| `bool` | true / false |
| `str` | UTF-8, immutable |

No arbitrary-precision integers or decimal type.

### 3.2 Collections

`list[T]` / `dict[K, V]` / `set[T]` / `tuple[A, B, ...]`. Literals are Python-like:

```
xs = [1, 2, 3]
m = {"a": 1}
s = {1, 2}
t = (1, "a")
```

### 3.3 stdlib Types (uppercase)

`Result[T, E]` / `Option[T]` / `Error` / `Value`.

### 3.4 Required Type Annotations

- Function signatures (arguments and return values) are **required**
- Local variables are inferred. Require annotations where inference is impossible (such as empty collections): `xs: list[int] = []`

### 3.5 struct / enum

Methods live inside structs. Mutability is not specified per field; it follows the binding's `var`.

```
struct User
    name: str
    age: int

    def greet(self): str
        return f"hello {self.name}"

enum Shape
    Circle(radius: float)
    Rect(w: float, h: float)
```

Constructors require **named arguments** (positional arguments are not allowed): `User(name: "a", age: 3)`

### 3.6 Generics

User-defined generics are supported. Use `[T]` notation.

```
def first[T](xs: list[T]): Option[T]
    return xs.get(0)
```

## 4. Variables and Mutability

- **Immutable by default**. Reassignment and field changes require a `var` declaration
- The type is fixed at the first binding

```
x = 5          # Immutable. x = 6 is a type error
var y = 5      # Mutable. y = 6 is OK
var u = User(name: "a", age: 3)
u.age = 4      # OK because the binding is var
```

## 5. Functions

There is no main function. Execution proceeds top to bottom from the top level.

```
def fetch(url: str): Result[str, Error] uses {net}
    return http.get(url)?.body
```

- Return annotations use `:` (not `->`)
- Declare effects with `uses {..}` (see section 8)
- Function type syntax: `(int) -> str uses {net}` (`->` is used in type contexts)

### 5.1 Lambdas

JS-like anonymous functions. **Parentheses are required and only one expression is allowed** (extract multiple statements into a `def`). Argument types are inferred from context; annotations are also allowed.

```
xs.map((x) => x * 2)
xs.filter((x: int) => x > 3)
```

## 6. Expressions

### 6.1 Expression-Oriented Constructs

if / match are expressions that return values. No ternary operator.

```
label = if score > 80
    "high"
else
    "low"

area = match shape
    Circle(r) => 3.14 * r * r
    Rect(w, h) => w * h
```

- Match arms use `=>`. For multiple-statement arms, put a newline and indentation after `=>`
- enum matches require **exhaustiveness checking**

### 6.2 Iterators (eager)

Use method chains. No comprehensions. `xs.map(f)` immediately returns `list[U]` (there is no `Iterator[T]` type).

Method vocabulary is Rust-like: `map / filter / fold / find / any / all / count / sum / enumerate / zip / rev / take / skip / flat_map / sort_by / chain`, and so on.

### 6.3 Pipes

`|>`. Lowest precedence and left-associative.

- A unary function may use a bare name: `x |> json.encode`
- Functions with arguments **require** the `_` placeholder: `x |> fs.write("out.json", _)`
- There is **no** implicit insertion of the first argument
- A stage may have a trailing `?`: `x |> parse? |> validate?`. Pipes themselves do not automatically short-circuit Results
- fmt formats long pipes as one stage per line

### 6.4 Strings

- f-string: `f"count: {n}"`
- Concatenation: `+`
- Comparison uses only structural equality `==`. No reference equality

## 7. Error Handling

### 7.1 Error Type

The entire stdlib uses one built-in `Error` type:

```
struct Error
    kind: str            # "net" | "fs" | "decode" | ... user-defined kinds are also allowed
    message: str
    cause: Option[Error]
```

`?` propagates without type conversion (avoiding Rust's `From` conversion maze).

### 7.2 The `?` Operator

- Supports both `Result` and `Option` (as in Rust, usable inside functions with a matching return type)
- Top-level `?`: print the error to stderr and exit 1 on Err

### 7.3 Ignoring Results Is Forbidden

Unused Result return values are a **type error**. Explicitly discard one with `_ = f()`.

### 7.4 No Panics

There are no catchable panics. Out-of-bounds access, division by zero, and integer overflow cause **immediate process termination** (exit 1 with a trace; not catchable). Safe APIs are provided:

- `xs[i]` -> terminate immediately out of bounds / `xs.get(i): Option[T]`
- `a / b` -> terminate immediately on division by zero / `math.checked_div(a, b): Option[int]`

### 7.5 assert

Built-in `assert(cond, msg)`. Failure exits 1. This is the primary verification tool for doc tests.

## 8. Effect System

**Static checking only** (no runtime enforcement). There are exactly six effect kinds:

`fs, net, env, proc, time, rand`

- Reading stdin is included in `env`
- `print` / `eprint` require no effect (debug output is allowed in pure functions)
- A function without an effect declaration is pure. Calling an effectful function from a pure function is a **type error**
- All effects are implicitly allowed at the top level
- Higher-order functions: effects of function arguments **propagate implicitly** (the checker infers the effect row internally; users do not write effect variables)

```
def map[T, U](xs: list[T], f: (T) -> U): list[U]
    # If f has {net}, callers of map(xs, f) also require {net}
```

## 9. Concurrent Execution

- IO has no syntactic async/await. It is internally asynchronous and implicitly awaited at the call site (without blocking the CPU)
- Concurrency uses **explicit constructs only**. No implicit concurrency

| Syntax | Type | Use |
|---|---|---|
| `par [f(), g()]` | `list[T]` (all elements have the same type) | Fixed count, homogeneous |
| `par (f(), g())` | `tuple[A, B]` | Fixed count, heterogeneous |
| `xs.par_map((x) => ...)` | `list[U]` | Dynamic collection |
| `xs.par_each((x) => ...)` | None | Side effects only |

- No fail-fast. **Wait for all tasks to finish**. If elements are `Result`, receive them as `list[Result[T, E]]`
- Closure captures are copied by value. No shared mutable state exists between par branches
- Runtime: multithreaded (CPU parallelism supported)

```
results = par [fetch(url1), fetch(url2)]
pages = urls.par_map((u) => http.get(u))
```

## 10. Modules

- No import syntax. No package concept
- A module file must have a `module` directive on the **first line**
- All `.ybm` files with a directive in the **same directory** as the entry file (the file passed to `ybm`) are **automatically** included
- No namespace separation (one flat namespace). Name collisions are type errors
- Modules contain **declarations only** (def / struct / enum / constants). Top-level executable statements are forbidden (eliminating order-dependent behavior)
- `ybm check` checks, formats, and lints all modules in the same directory together

## 11. Standard Library

Built-in namespaces that require no import.

### 11.1 codec (4 Formats)

`json` / `csv` / `yaml` / `toml` (XML and JSON5 are out of scope).

- decode is **driven by the destination annotation**: `data: User = json.decode(s)?`
- encode: `json.encode(value): str`
- CSV: `csv.decode[T](s): Result[list[T], Error]`. T is a flat struct; names match the first-row header
- YAML: safe subset (anchors, aliases, and multi-document input are unsupported)
- A dynamic `Value` type is provided for data with unknown schemas (shared by all codecs)

### 11.2 Module List

| Namespace | Contents | Effect |
|---|---|---|
| `fs` | read / write / append / list / exists / remove | `fs` |
| `http` | Client only: get / post / put / delete, headers, timeout, body | `net` |
| `env` | get / set / args / stdin | `env` |
| `proc` | Run commands; retrieve stdout / stderr / exit code | `proc` |
| `time` | now / sleep / format / parse | `time` |
| `rand` | Random numbers | `rand` |
| `regex` | Regular expressions | None (pure) |
| `math` | Math functions and checked operations | None (pure) |

HTTP server / sqlite / crypto are out of scope.

### 11.3 Built-in Functions

`print` / `eprint` (stderr) / `assert(cond, msg)`. No log-level mechanism.

## 12. fmt / lint

- fmt: `ybm check --apply` rewrites in place (gofmt style). `ybm check` only checks the diff. Idempotent (fmt o fmt = fmt)
- Lint rules: unused variables / unused functions / shadowing / unreachable code / naming conventions (snake_case variables and functions, PascalCase types)
- Lint warnings also exit 1 (the simple convention that "passing check = clean")

## 13. Doc Tests

Fenced ``` blocks inside `##` doc comments are tests.

```
## Add two ints.
##
## ```
## assert(add(1, 2) == 3)
## ```
def add(a: int, b: int): int
    return a + b
```

- Run with `ybm test <file>`. `ybm check` only type-checks (it does not execute)
- Run each block as an independent program. Its scope is the entire file (the entry file plus all declarations from same-directory modules)
- No effect restrictions (equivalent to the top level)
- An `assert` failure or abnormal termination from Err means fail. Aggregate pass/fail per block; exit 1 if any block fails

## 14. Memory Model

- No GC. **Value semantics + scope RAII**
- The implementation uses reference counting (Arc + copy-on-write). Ownership and borrowing are not exposed to users
- "No GC" is an implementation detail; users never encounter ownership errors

## 15. Example

```
#!/usr/bin/env ybm

struct Repo
    name: str
    stars: int

def fetch_repos(user: str): Result[list[Repo], Error] uses {net}
    body = http.get(f"https://api.example.com/users/{user}/repos")?.body
    repos: list[Repo] = json.decode(body)?
    return Ok(repos)

users = ["alice", "bob", "carol"]
results = users.par_map((u) => fetch_repos(u))

top = results
    .filter((r) => r.is_ok())
    .flat_map((r) => r.unwrap_or([]))
    .filter((repo) => repo.stars > 100)
    .sort_by((repo) => repo.stars)
    .rev()
    .take(10)

top |> toml.encode |> fs.write("top.toml", _)
print(f"done: {top.count()} repos")
```
