# Yabumi language reference

Read this file before writing or repairing `.ybm` code. Yabumi is not Python, Rust, or shell; use only constructs listed here.

## File layout

- Extension: `.ybm`; encoding: UTF-8.
- Optional first line: `#!/usr/bin/env ybm`.
- Identifiers are ASCII: `[a-zA-Z_][a-zA-Z0-9_]*`.
- `#` starts a line comment. `##` immediately before a declaration is a doc comment.
- Blocks use exactly four spaces. Tabs are errors.
- Block openers never end in `:`. Colons appear only in type annotations, return types, named arguments, and dictionary entries.
- Newlines end statements except inside `()`, `[]`, `{}`, or an indented continuation starting with `.` or `|>`.
- There is no `main`. Top-level declarations are hoisted; other top-level statements execute in source order.
- There are no imports, packages, classes, traits, inheritance, exceptions, `async`/`await`, or user-defined operator overloads.

```ybm
#!/usr/bin/env ybm

def greet(name: str): str
    return f"hello {name}"

print(greet("world"))
```

## Types and literals

Primitive types:

```ybm
count: int = 3
ratio: float = 1.5
enabled: bool = true
name: str = "yabumi"
```

Collection and standard-library types:

```ybm
numbers: list[int] = [1, 2, 3]
scores: dict[str, int] = {"a": 1, "b": 2}
ids: set[int] = {1, 2}
empty_ids: set[int] = set[int]()
pair: tuple[int, str] = (1, "one")
result: Result[int, Error] = Ok(1)
maybe_name: Option[str] = None
```

- Collection elements must have one concrete type; there is no `Any` or implicit union fallback.
- Empty collections need a type annotation when inference has no context: `items: list[str] = []`.
- Tuple fields use `.0`, `.1`, and so on.
- Struct/enum/list/dict/set/tuple/Result/Option comparisons with `==` and `!=` are recursive structural comparisons.
- There is no implicit `int`/`float` conversion.

## Bindings and mutability

Bindings are immutable unless introduced with `var`. The first value fixes the binding's type.

```ybm
count = 1
var total = 0
total = total + count

var values = [1, 2]
values.push(3)
values[0] = 9
```

A `var` root binding permits field and collection mutation below it. Mutating methods declare `self: var Type` and require a `var` receiver.

## Structs, methods, enums, and generics

Struct constructors require named arguments. Enum variants always use positional arguments; unit variants omit parentheses.

```ybm
struct User
    name: str
    age: int

    def label(self): str
        return f"{self.name}:{self.age}"

    def birthday(self: var User): void
        self.age = self.age + 1

enum Shape
    Circle(float)
    Rect(float, float)
    Unknown

user = User(name: "Ada", age: 37)
shape = Circle(2.0)
```

Generic declarations use `[T]`:

```ybm
struct Box[T]
    value: T

def first[T](items: list[T]): Option[T]
    return items.get(0)
```

A generic `T` supports storage, passing, and `==`/`!=`; arithmetic and ordering require a concrete type because Yabumi has no trait constraints.

## Functions, effects, and lambdas

Function parameter and return annotations are required. Use `void` for no return value.

```ybm
def double(value: int): int
    return value * 2

def fetch(url: str): Result[str, Error] uses {net}
    return http.get(url)?.body
```

Effects: `fs`, `net`, `env`, `proc`, `time`, `rand`.

- A function declares the union of its direct and indirect effects with `uses {...}`.
- A function without `uses` is pure.
- Top-level statements implicitly permit all effects.
- Lambda effects are inferred and propagate through higher-order calls.
- Lambdas require parentheses and contain exactly one expression. A multiline `if` or `match` counts as one expression; multiple statements do not.

```ybm
numbers.map((number) => number * 2)
items.filter((item: Item) => item.enabled)
```

## Control flow

There are no `for`, `while`, `loop`, `break`, `continue`, or `elif` constructs. Use eager collection methods. Use `.each(...)` for sequential side effects and `.par_each(...)` for concurrent side effects.

### if expressions

Every `if` requires an `else` because it produces a value. Branch result types must match. For multi-way branching, nest another `if` under `else` or use `match`.

```ybm
label = if score >= 80
    "high"
else
    "low"

category = if score >= 90
    "excellent"
else
    if score >= 70
        "good"
    else
        "retry"
```

### match expressions

Arms use `=>`. Supported patterns: enum variants, literals, a binding name, `_`, and tuple destructuring. Guards, OR patterns, and nested destructuring such as `Some(Ok(value))` are unsupported; nest another `match` instead.

```ybm
message = match result
    Ok(value) => f"value={value}"
    Err(error) => f"error={error.message}"

area = match shape
    Circle(radius) => math.PI * radius * radius
    Rect(width, height) => width * height
    Unknown => 0.0

label = match code
    200 => "ok"
    404 => "missing"
    _ => "other"
```

Enum matches must be exhaustive. `bool` needs both `true` and `false`; other non-enum values need a trailing `_` arm.

## Operators

Precedence, tightest first:

| Level | Operators | Notes |
|---|---|---|
| Postfix | `()` `[]` `.` `?` | Applied left to right |
| Unary | `-` `not` | Prefix |
| Multiply | `*` `/` `%` | `%` is int-only |
| Add | `+` `-` | `+`: numbers, strings, same-type lists |
| Ordering | `<` `<=` `>` `>=` | Same-type int, float, or str only |
| Equality | `==` `!=` | Structural for every type |
| Logical | `and`, then `or` | bool operands |
| Pipe | `|>` | Loosest, left-associative |

- Chained comparisons are invalid: write `a < b and b < c`.
- `&&`, `||`, `!`, `**`, bitwise operators, `in`, `is`, `++`, and `--` do not exist.
- Integer `/` truncates toward zero. Convert explicitly for floating division: `float(a) / float(b)`.
- Arithmetic overflow and division by zero terminate immediately. Use `math.checked_*` for untrusted values.

## Method chains and pipes

Collection methods are eager. An indented continuation must start with `.`:

```ybm
selected = values
    .filter((value) => value > 0)
    .map((value) => value * 2)
    .take(10)
```

Pipes never insert an argument implicitly. A stage with other arguments must mark the piped position with `_`:

```ybm
encoded = report |> json.encode
write_error = encoded |> fs.write("report.json", _)
```

A postfix `?` may follow a pipe stage: `input |> parse? |> validate?`.

## Result, Option, and failure

```ybm
enum Result[T, E]
    Ok(T)
    Err(E)

enum Option[T]
    Some(T)
    None

struct Error
    kind: str
    message: str
    cause: Option[Error]
```

Construct `Ok(value)`, `Err(error)`, `Some(value)`, `None`, and `Error(kind: "input", message: "missing path", cause: None)`.

`?` unwraps the success/present value and returns early on `Err`/`None`:

- In `Result[T, E]` functions, use `?` only on `Result[_, E]` with the exact same error type.
- In `Option[T]` functions, use `?` only on `Option[_]`.
- At top level, `Err` or `None` terminates with exit code 1.
- Never ignore a `Result`; bind it, propagate it, inspect it, or explicitly discard it with `_ = call()`.

Important exception: `fs.write`, `fs.append`, and `fs.remove` return `Option[Error]` where `None` means success. **Never apply `?` to these calls**: it would treat successful `None` as failure. Match `Some(error)`/`None`, or convert the value to a `Result` explicitly. See `stdlib.md` for the verified pattern.

Unsafe operations terminate immediately and cannot be caught. Prefer `items.get(index)`, `dict.get(key)`, and `math.checked_*` when failure depends on input.

## Explicit concurrency

```ybm
same_type = par [fetch("a"), fetch("b")]
mixed_type = par (compute_count(), compute_label())
results = urls.par_map((url) => http.get(url))
urls.par_each((url) => print(url))
```

- `par [...]`: fixed homogeneous expressions, returns `list[T]`.
- `par (...)`: fixed heterogeneous expressions, returns a tuple.
- `.par_map(...)` / `.par_each(...)`: dynamic collections.
- Output order matches source/input order.
- `Result` values remain explicit; inspect them after the concurrent operation.
- Branches capture values and cannot share mutable state.

## Modules

There is no import syntax. An entry automatically loads every sibling `.ybm` whose effective first line is `module`:

```ybm
module

struct Item
    name: str
```

Modules may contain only declarations and constants, never top-level executable statements. All entry and module names share one flat namespace; collisions are errors.

## Doc tests

Fenced blocks inside `##` comments immediately before a declaration are tests:

```ybm
## Adds two integers.
##
## ```
## assert(add(1, 2) == 3)
## ```
def add(a: int, b: int): int
    return a + b
```

`ybm check` type-checks doc-test blocks. `ybm test` runs each block independently with access to all declarations.

## Formatter and lint

`ybm check file.ybm` type-checks, formats in place, and lints. Any warning exits 1. Avoid:

- unused local bindings, parameters, or match bindings (`E4001`); names beginning with `_` are exempt;
- entry-file functions unreachable from top-level code or doc tests (`E4002`);
- shadowing any outer variable, parameter, function, struct, or enum name (`E4003`);
- statements immediately after `return` in the same block (`E4004`);
- non-`snake_case` values/functions/fields/parameters or non-`PascalCase` types/variants (`E4005`).

Module functions and struct methods are exempt from unused-function lint.
