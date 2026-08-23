# Yabumi standard library reference

Read the relevant section before calling an API. All namespaces are built in; there are no imports. Signatures below are exact for the current language.

## Contents

1. [Conversions and strings](#conversions-and-strings)
2. [Collections](#collections)
3. [Result, Option, Error, and Value](#result-option-error-and-value)
4. [Codecs](#codecs)
5. [Effects: fs, http, env, proc, time, rand](#effectful-apis)
6. [Pure APIs: regex and math](#pure-apis)
7. [Built-ins](#built-ins)
8. [Verified patterns](#verified-patterns)

## Conversions and strings

```ybm
def int(value: float): int
def float(value: int): float
def str(value: int): str
def str(value: float): str
def str(value: bool): str

def to_str(self: int): str
def to_str(self: float): str

def len(self: str): int
def get(self: str, index: int): Option[str]
def chars(self: str): list[str]
def bytes(self: str): list[int]
def split(self: str, separator: str): list[str]
def trim(self: str): str
def trim_start(self: str): str
def trim_end(self: str): str
def to_upper(self: str): str
def to_lower(self: str): str
def contains(self: str, needle: str): bool
def starts_with(self: str, prefix: str): bool
def ends_with(self: str, suffix: str): bool
def replace(self: str, from: str, to: str): str
def repeat(self: str, count: int): str
def is_empty(self: str): bool
def find(self: str, needle: str): Option[int]
def slice(self: str, start: int, end: int): str
def parse_int(self: str): Result[int, Error]
def parse_float(self: str): Result[float, Error]
```

String indexing uses Unicode scalar values. There is no `text[index]`; use `.get(index)`. `.slice(...)` aborts on invalid bounds.

A string also supports eager character-based `map`, `filter`, `fold`, `find_by`, `any`, `all`, `count`, `enumerate`, `zip`, `rev`, `take`, `skip`, `flat_map`, `sort_by`, and `chain`. These return lists where appropriate; rejoin with `.join("")`.

## Collections

### list[T]

Non-mutating:

```ybm
def map[T, U](self: list[T], function: (T) -> U): list[U]
def filter[T](self: list[T], predicate: (T) -> bool): list[T]
def fold[T, Acc](self: list[T], initial: Acc, function: (Acc, T) -> Acc): Acc
def find[T](self: list[T], predicate: (T) -> bool): Option[T]
def any[T](self: list[T], predicate: (T) -> bool): bool
def all[T](self: list[T], predicate: (T) -> bool): bool
def count[T](self: list[T]): int
def sum(self: list[int]): int
def sum(self: list[float]): float
def enumerate[T](self: list[T]): list[tuple[int, T]]
def zip[T, U](self: list[T], other: list[U]): list[tuple[T, U]]
def rev[T](self: list[T]): list[T]
def take[T](self: list[T], count: int): list[T]
def skip[T](self: list[T], count: int): list[T]
def flat_map[T, U](self: list[T], function: (T) -> list[U]): list[U]
def sort_by[T](self: list[T], key: (T) -> int): list[T]
def sort_by[T](self: list[T], key: (T) -> float): list[T]
def sort_by[T](self: list[T], key: (T) -> str): list[T]
def chain[T](self: list[T], other: list[T]): list[T]
def get[T](self: list[T], index: int): Option[T]
def len[T](self: list[T]): int
def is_empty[T](self: list[T]): bool
def contains[T](self: list[T], value: T): bool
def first[T](self: list[T]): Option[T]
def last[T](self: list[T]): Option[T]
def join(self: list[str], separator: str): str
def slice[T](self: list[T], start: int, end: int): list[T]
def to_set[T](self: list[T]): set[T]
def each[T](self: list[T], function: (T) -> void): void
def par_map[T, U](self: list[T], function: (T) -> U): list[U]
def par_each[T](self: list[T], function: (T) -> void): void
```

Mutating; receiver binding must use `var`:

```ybm
def push[T](self: var list[T], value: T): void
def pop[T](self: var list[T]): Option[T]
def insert[T](self: var list[T], index: int, value: T): void
def remove[T](self: var list[T], index: int): T
def extend[T](self: var list[T], other: list[T]): void
def clear[T](self: var list[T]): void
```

`items[index]` reads and `items[index] = value` writes. Invalid indexes abort; use `.get(index)` for input-dependent reads.

### dict[K, V]

Keys may be `int`, `str`, `bool`, or tuples containing only valid key types.

```ybm
def get[K, V](self: dict[K, V], key: K): Option[V]
def contains_key[K, V](self: dict[K, V], key: K): bool
def keys[K, V](self: dict[K, V]): list[K]
def values[K, V](self: dict[K, V]): list[V]
def entries[K, V](self: dict[K, V]): list[tuple[K, V]]
def len[K, V](self: dict[K, V]): int
def is_empty[K, V](self: dict[K, V]): bool
def map[K, V, U](self: dict[K, V], function: (tuple[K, V]) -> U): list[U]
def filter[K, V](self: dict[K, V], predicate: (tuple[K, V]) -> bool): dict[K, V]
def any[K, V](self: dict[K, V], predicate: (tuple[K, V]) -> bool): bool
def all[K, V](self: dict[K, V], predicate: (tuple[K, V]) -> bool): bool
def find[K, V](self: dict[K, V], predicate: (tuple[K, V]) -> bool): Option[tuple[K, V]]
def fold[K, V, Acc](self: dict[K, V], initial: Acc, function: (Acc, tuple[K, V]) -> Acc): Acc
def each[K, V](self: dict[K, V], function: (tuple[K, V]) -> void): void

def insert[K, V](self: var dict[K, V], key: K, value: V): Option[V]
def remove[K, V](self: var dict[K, V], key: K): Option[V]
def clear[K, V](self: var dict[K, V]): void
```

`values[key]` reads and `values[key] = value` writes. Missing-key reads abort; use `.get(key)` for input-dependent reads.

### set[T] and tuple

```ybm
def contains[T](self: set[T], value: T): bool
def len[T](self: set[T]): int
def is_empty[T](self: set[T]): bool
def union[T](self: set[T], other: set[T]): set[T]
def intersection[T](self: set[T], other: set[T]): set[T]
def difference[T](self: set[T], other: set[T]): set[T]
def to_list[T](self: set[T]): list[T]
def map[T, U](self: set[T], function: (T) -> U): list[U]
def filter[T](self: set[T], predicate: (T) -> bool): set[T]
def any[T](self: set[T], predicate: (T) -> bool): bool
def all[T](self: set[T], predicate: (T) -> bool): bool
def find[T](self: set[T], predicate: (T) -> bool): Option[T]
def fold[T, Acc](self: set[T], initial: Acc, function: (Acc, T) -> Acc): Acc
def count[T](self: set[T]): int
def sum(self: set[int]): int
def sum(self: set[float]): float
def each[T](self: set[T], function: (T) -> void): void

def insert[T](self: var set[T], value: T): bool
def remove[T](self: var set[T], value: T): bool
def clear[T](self: var set[T]): void
```

Create an empty set with `set[T]()`. Tuple values have no methods; access fields with `.0`, `.1`, and so on.

## Result, Option, Error, and Value

```ybm
def is_ok[T, E](self: Result[T, E]): bool
def is_err[T, E](self: Result[T, E]): bool
def ok[T, E](self: Result[T, E]): Option[T]
def err[T, E](self: Result[T, E]): Option[E]
def unwrap[T, E](self: Result[T, E]): T
def unwrap_or[T, E](self: Result[T, E], default: T): T
def unwrap_or_else[T, E](self: Result[T, E], function: (E) -> T): T
def map[T, E, U](self: Result[T, E], function: (T) -> U): Result[U, E]
def map_err[T, E, F](self: Result[T, E], function: (E) -> F): Result[T, F]
def and_then[T, E, U](self: Result[T, E], function: (T) -> Result[U, E]): Result[U, E]

def is_some[T](self: Option[T]): bool
def is_none[T](self: Option[T]): bool
def unwrap[T](self: Option[T]): T
def unwrap_or[T](self: Option[T], default: T): T
def unwrap_or_else[T](self: Option[T], function: () -> T): T
def map[T, U](self: Option[T], function: (T) -> U): Option[U]
def and_then[T, U](self: Option[T], function: (T) -> Option[U]): Option[U]
def filter[T](self: Option[T], predicate: (T) -> bool): Option[T]
def ok_or[T, E](self: Option[T], error: E): Result[T, E]
```

`unwrap()` aborts on `Err`/`None`; prefer `?`, `match`, `unwrap_or`, or `ok_or` for input-dependent failure.

```ybm
struct Error
    kind: str
    message: str
    cause: Option[Error]

enum Value
    Null
    Bool(bool)
    Int(int)
    Float(float)
    Str(str)
    List(list[Value])
    Dict(dict[str, Value])
```

`Value` methods:

```ybm
def as_int(self: Value): Option[int]
def as_float(self: Value): Option[float]
def as_str(self: Value): Option[str]
def as_bool(self: Value): Option[bool]
def as_list(self: Value): Option[list[Value]]
def as_dict(self: Value): Option[dict[str, Value]]
def is_null(self: Value): bool
def get(self: Value, key: str): Option[Value]
def at(self: Value, index: int): Option[Value]
```

## Codecs

All codec calls are pure.

```ybm
def json.decode[T](text: str): Result[T, Error]
def json.encode[T](value: T): str
def yaml.decode[T](text: str): Result[T, Error]
def yaml.encode[T](value: T): str
def toml.decode[T](text: str): Result[T, Error]
def toml.encode[T](value: T): str

def csv.decode[T](text: str): Result[list[T], Error]
def csv.encode[T](rows: list[T]): str
def csv.decode_rows(text: str): Result[list[dict[str, Value]], Error]
```

The written syntax is `json.decode(text)`, not a declaration containing a dotted function name; the signatures above show namespaces compactly.

- `json`/`yaml`/`toml` infer `T` from the assignment target or explicit `[T]`.
- `toml.encode` requires a struct or `dict[str, V]` at the document root.
- `csv.decode[T]` requires explicit `T`; `T` must be a flat struct containing only primitive fields matching header names.
- YAML excludes anchors, aliases, and multiple documents.

## Effectful APIs

### fs — `uses {fs}`

```ybm
def fs.read(path: str): Result[str, Error]
def fs.read_bytes(path: str): Result[list[int], Error]
def fs.write(path: str, content: str): Option[Error]
def fs.append(path: str, content: str): Option[Error]
def fs.list(path: str): Result[list[str], Error]
def fs.exists(path: str): bool
def fs.remove(path: str): Option[Error]
```

`write`/`append`/`remove`: `None` is success; `Some(error)` is failure. Never apply `?` directly.

### http — `uses {net}`

```ybm
struct Response
    status: int
    headers: dict[str, str]
    body: str

struct HttpOptions
    headers: dict[str, str]
    timeout_ms: int

def http.get(url: str): Result[Response, Error]
def http.post(url: str, body: str): Result[Response, Error]
def http.put(url: str, body: str): Result[Response, Error]
def http.delete(url: str): Result[Response, Error]
def http.request(method: str, url: str, options: HttpOptions): Result[Response, Error]
```

Simple methods use fixed internal options. `request` sets headers and timeout but has no separate body argument.

### env — `uses {env}`

```ybm
def env.get(key: str): Option[str]
def env.set(key: str, value: str): void
def env.args(): list[str]
def env.stdin(): Result[str, Error]
```

`env.args()` excludes the executable path. `env.stdin()` reads through EOF.

### proc — `uses {proc}`

```ybm
struct ProcOutput
    stdout: str
    stderr: str
    exit_code: int

def proc.run(command: str, args: list[str]): Result[ProcOutput, Error]
```

Only launch failure is `Err`; a nonzero command exit is `Ok(ProcOutput)`. Always inspect `exit_code` when success matters.

### time — `uses {time}`

```ybm
def time.now(): int
def time.sleep(milliseconds: int): void
def time.format(epoch_ms: int, format: str): str
def time.parse(text: str, format: str): Result[int, Error]
```

Times are Unix epoch milliseconds. Formats use `strftime` syntax.

### rand — `uses {rand}`

```ybm
def rand.int(low: int, high: int): int
def rand.float(): float
def rand.bool(): bool
def rand.choice[T](items: list[T]): Option[T]
def shuffle[T](self: var list[T]): void
```

`rand.int` uses `[low, high)`. Call shuffle as `var items = [...]; items.shuffle()`.

## Pure APIs

### regex

```ybm
def regex.is_match(pattern: str, text: str): Result[bool, Error]
def regex.find(pattern: str, text: str): Result[Option[str], Error]
def regex.find_all(pattern: str, text: str): Result[list[str], Error]
def regex.replace(pattern: str, text: str, replacement: str): Result[str, Error]
def regex.replace_all(pattern: str, text: str, replacement: str): Result[str, Error]
def regex.captures(pattern: str, text: str): Result[Option[list[str]], Error]
```

Invalid patterns return `Err`. `captures` index 0 is the whole match.

### math

```ybm
def math.checked_div(left: int, right: int): Option[int]
def math.checked_mod(left: int, right: int): Option[int]
def math.checked_add(left: int, right: int): Option[int]
def math.checked_sub(left: int, right: int): Option[int]
def math.checked_mul(left: int, right: int): Option[int]
def math.abs_int(value: int): int
def math.abs_float(value: float): float
def math.min_int(left: int, right: int): int
def math.max_int(left: int, right: int): int
def math.min_float(left: float, right: float): float
def math.max_float(left: float, right: float): float
def math.floor(value: float): int
def math.ceil(value: float): int
def math.round(value: float): int
def math.sqrt(value: float): float
def math.pow(base: float, exponent: float): float
```

Constants: `math.PI: float`, `math.E: float`.

## Built-ins

```ybm
def print(value: str): void
def print(value: int): void
def print(value: float): void
def print(value: bool): void
def eprint(value: str): void
def eprint(value: int): void
def eprint(value: float): void
def eprint(value: bool): void
def assert(condition: bool): void
def assert(condition: bool, message: str): void
```

There is no implicit struct/enum/collection stringification. Encode a value or build a string explicitly before printing it.

## Verified patterns

### Convert `Option[Error]` to `Result`

Use this helper for `fs.write`, `fs.append`, and `fs.remove` when the caller should terminate on failure:

```ybm
def write_text(path: str, content: str): Result[bool, Error] uses {fs}
    return match fs.write(path, content)
        Some(error) => Err(error)
        None => Ok(true)
```

`bool` is the success payload because `void` cannot be a `Result` type argument.

### Complete JSON file transformation

```ybm
#!/usr/bin/env ybm

struct Task
    title: str
    done: bool

## Keeps unfinished tasks.
##
## ```
## sample_tasks = [Task(title: "done", done: true), Task(title: "open", done: false)]
## assert(pending_tasks(sample_tasks) == [Task(title: "open", done: false)])
## ```
def pending_tasks(items: list[Task]): list[Task]
    return items.filter((task) => task.done == false)

def write_text(path: str, content: str): Result[bool, Error] uses {fs}
    return match fs.write(path, content)
        Some(error) => Err(error)
        None => Ok(true)

input_path = env.args()
    .get(0)
    .ok_or(Error(kind: "input", message: "expected a JSON file path", cause: None))?
raw = fs.read(input_path)?
decoded_tasks: list[Task] = json.decode(raw)?
pending = pending_tasks(decoded_tasks)
_ = write_text("pending.json", json.encode(pending))?
print(f"wrote {pending.count()} tasks")
```

### Concurrent HTTP with per-item errors

```ybm
#!/usr/bin/env ybm

def fetch_url(url: str): Result[Response, Error] uses {net}
    return http.get(url)

urls = env.args()
results = urls.par_map((url) => fetch_url(url))
urls.zip(results).each((pair) => match pair.1
    Ok(response) => print(f"{pair.0} {response.status}")
    Err(error) => eprint(f"{pair.0} {error.message}")
)
```

There is no loop statement. `.each(...)` performs sequential output after concurrent fetching, preserving argument order.
