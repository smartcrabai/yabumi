# Yabumi Standard Library Complete Reference (STDLIB)

This document is subordinate to `SPEC.md`. **If anything here conflicts with SPEC.md, SPEC.md takes precedence.** The rationale behind design decisions is canonically recorded in `docs/DECISIONS.md`; this document focuses solely on enumerating the concrete signatures that follow from the policies settled there.

All namespaces and types are available globally without import (SPEC §11 L256). Functions that have effects spell them out in their signature with `uses {..}`. A function with nothing written is pure (no effects).

Notation legend:
- Anything taking `self` as its first argument can be called with method-call syntax (`x.f(...)`)
- `var self` (DECISIONS D-MUT-01/02) is a documentation-only annotation meaning "can only be called if the caller's variable is a `var` binding, and calling it rewrites it in place." Since list/dict/set are built-in types a user cannot reproduce with `struct`, the notation `self: var list[T]` is used to mean "equivalent to `var self`" (it is not executable user syntax)
- An operation annotated `panics` is subject to the immediate-termination behavior (E6xxx) from DECISIONS D-ERR-04; the safe-API counterpart is given immediately after it

---

## Table of Contents

1. Primitive type methods (int / float / bool / str)
2. Collections (list[T] / dict[K,V] / set[T] / tuple)
3. stdlib types (Result[T,E] / Option[T] / Error / Value)
4. codec (json / csv / yaml / toml)
5. fs
6. http
7. env
8. proc
9. time
10. rand
11. regex
12. math
13. Built-in functions (print / eprint / assert)
14. Table of operations that abort immediately and their safe-API counterparts

---

## 1. Primitive Type Methods

### 1.1 Type Conversions (all pure, no effects)

```
def int(x: float): int          # Truncates toward zero. Panics with E6003 (integer overflow) if out of i64 range
def float(x: int): float        # Always succeeds. May lose mantissa precision for large i64 values (not an error)
def str(x: int): str            # Decimal representation
def str(x: float): str          # Shortest round-trip representation. Always includes a decimal point (e.g. 1.0, 3.14, 1e20)
def str(x: bool): str           # "true" / "false"
```

`int`/`float`/`str` are not reserved words; they are pre-registered names in the flat namespace (DECISIONS D-TYPE-14). Defining a function or variable with the same name yields E1001.

### 1.2 int

```
def to_str(self: int): str        # Equivalent to str(x). A synonym for method-call syntax
```

For the arithmetic operators `+ - * / %`, see DECISIONS D-OP-04/08. Ordering comparisons `< <= > >=` apply only to int/float/str (D-OP-05).

### 1.3 float

```
def to_str(self: float): str      # Equivalent to str(x)
```

### 1.4 bool

No additional methods. `and` / `or` / `not` are keyword operators (§6.4, DECISIONS D-LEX-01).

### 1.5 str

Indexing is in units of Unicode scalar values (char) (DECISIONS D-COL-03). There is no bracket syntax `s[i]`.

```
def len(self: str): int                                # number of chars
def get(self: str, i: int): Option[str]                # out of range yields None (does not panic)
def chars(self: str): list[str]                        # splits into a list[str], one char at a time
def bytes(self: str): list[int]                         # UTF-8 byte sequence
def split(self: str, sep: str): list[str]
def trim(self: str): str
def trim_start(self: str): str
def trim_end(self: str): str
def to_upper(self: str): str
def to_lower(self: str): str
def contains(self: str, needle: str): bool
def starts_with(self: str, prefix: str): bool
def ends_with(self: str, suffix: str): bool
def replace(self: str, from: str, to: str): str        # replaces all occurrences
def repeat(self: str, n: int): str
def is_empty(self: str): bool
def find(self: str, needle: str): Option[int]           # substring search, returns a char index
def slice(self: str, start: int, end: int): str          # panics: out of range (E6001). No safe version (check with len() beforehand)
def parse_int(self: str): Result[int, Error]             # kind: "decode"
def parse_float(self: str): Result[float, Error]         # kind: "decode"

# Iterator-style methods (treat self as equivalent to .chars() (list[str]); always returns list[U].
#             To rejoin, explicitly call .join(""))
def map[U](self: str, f: (str) -> U): list[U]
def filter(self: str, f: (str) -> bool): list[str]
def fold[Acc](self: str, init: Acc, f: (Acc, str) -> Acc): Acc
def find_by(self: str, f: (str) -> bool): Option[str]
def any(self: str, f: (str) -> bool): bool
def all(self: str, f: (str) -> bool): bool
def count(self: str): int                                # = len()
def enumerate(self: str): list[tuple[int, str]]
def zip(self: str, other: str): list[tuple[str, str]]
def rev(self: str): list[str]
def take(self: str, n: int): list[str]
def skip(self: str, n: int): list[str]
def flat_map[U](self: str, f: (str) -> list[U]): list[U]
def sort_by(self: str, f: (str) -> str): list[str]        # str keys only (D-OP-05)
def chain(self: str, other: str): list[str]
```

`join` is a method on `list[str]` (see §2.1, not 1.5): `xs.join(sep)`.

---

## 2. Collections

### 2.1 list[T]

Iteration order is always the order written/inserted (DECISIONS D-COL-01).

**Non-destructive (returns a new list; `self` does not need to be `var`):**

```
def map[T, U](self: list[T], f: (T) -> U): list[U]
def filter[T](self: list[T], f: (T) -> bool): list[T]
def fold[T, Acc](self: list[T], init: Acc, f: (Acc, T) -> Acc): Acc
def find[T](self: list[T], f: (T) -> bool): Option[T]
def any[T](self: list[T], f: (T) -> bool): bool
def all[T](self: list[T], f: (T) -> bool): bool
def count[T](self: list[T]): int
def sum(self: list[int]): int                            # overload special-case per D-STDPOL-01
def sum(self: list[float]): float                        # overload special-case per D-STDPOL-01
def enumerate[T](self: list[T]): list[tuple[int, T]]
def zip[T, U](self: list[T], other: list[U]): list[tuple[T, U]]
def rev[T](self: list[T]): list[T]
def take[T](self: list[T], n: int): list[T]
def skip[T](self: list[T], n: int): list[T]
def flat_map[T, U](self: list[T], f: (T) -> list[U]): list[U]
def sort_by[T](self: list[T], f: (T) -> int): list[T]     # key type must be int/float/str only (D-OP-05)
def sort_by[T](self: list[T], f: (T) -> float): list[T]
def sort_by[T](self: list[T], f: (T) -> str): list[T]
def chain[T](self: list[T], other: list[T]): list[T]
def get[T](self: list[T], i: int): Option[T]              # safe version of the panicking one
def len[T](self: list[T]): int
def is_empty[T](self: list[T]): bool
def contains[T](self: list[T], x: T): bool                 # == on T is always structural equality (D-OP-06)
def first[T](self: list[T]): Option[T]
def last[T](self: list[T]): Option[T]
def join(self: list[str], sep: str): str
def slice[T](self: list[T], start: int, end: int): list[T]  # panics: out of range (E6001)
def to_set[T](self: list[T]): set[T]                        # T must be an allowed set key type only (equivalent to E1012)
def each[T](self: list[T], f: (T) -> void): void            # sequential, non-parallel side-effect-only iteration (D-SYN-09)
def par_map[T, U](self: list[T], f: (T) -> U): list[U]       # concurrent version of map. Input order is guaranteed (D-PAR-01). f's effects propagate to the caller per D-FUNC-03
def par_each[T](self: list[T], f: (T) -> void): void          # concurrent version of each. Side-effect-only; no fail-fast, runs to completion (SPEC §9)
```

**Destructive (`var self` required; the caller's variable must be a `var` binding. DECISIONS D-MUT-01/02):**

```
def push[T](self: var list[T], x: T): void
def pop[T](self: var list[T]): Option[T]
def insert[T](self: var list[T], i: int, x: T): void        # panics: out of range (E6001)
def remove[T](self: var list[T], i: int): T                 # panics: out of range (E6001)
def extend[T](self: var list[T], other: list[T]): void
def clear[T](self: var list[T]): void
```

Subscript syntax is also available: `xs[i]` (read, panics: out of range E6001) / `xs[i] = v` (write, requires a `var` binding. DECISIONS D-COL-02).

### 2.2 dict[K, V]

`K` may only be int/str/bool, or a tuple[...] whose elements are all allowed key types (DECISIONS D-TYPE-05). Iteration order is insertion order (D-COL-01).

**Non-destructive:**

```
def get[K, V](self: dict[K, V], k: K): Option[V]
def contains_key[K, V](self: dict[K, V], k: K): bool
def keys[K, V](self: dict[K, V]): list[K]
def values[K, V](self: dict[K, V]): list[V]
def entries[K, V](self: dict[K, V]): list[tuple[K, V]]
def len[K, V](self: dict[K, V]): int
def is_empty[K, V](self: dict[K, V]): bool
def map[K, V, U](self: dict[K, V], f: (tuple[K, V]) -> U): list[U]
def filter[K, V](self: dict[K, V], f: (tuple[K, V]) -> bool): dict[K, V]
def any[K, V](self: dict[K, V], f: (tuple[K, V]) -> bool): bool
def all[K, V](self: dict[K, V], f: (tuple[K, V]) -> bool): bool
def find[K, V](self: dict[K, V], f: (tuple[K, V]) -> bool): Option[tuple[K, V]]
def fold[K, V, Acc](self: dict[K, V], init: Acc, f: (Acc, tuple[K, V]) -> Acc): Acc
def each[K, V](self: dict[K, V], f: (tuple[K, V]) -> void): void
```

`enumerate`/`zip`/`rev`/`take`/`skip`/`sort_by`/`chain` are not provided directly on dict. Convert to `list[tuple[K,V]]` via `.entries()` first, then call them.

**Destructive (`var self` required):**

```
def insert[K, V](self: var dict[K, V], k: K, v: V): Option[V]   # returns the old value
def remove[K, V](self: var dict[K, V], k: K): Option[V]
def clear[K, V](self: var dict[K, V]): void
```

Subscript syntax is also available: `m[k]` (read, panics: missing key E6001) / `m[k] = v` (write, requires a `var` binding).

### 2.3 set[T]

`T` has the same constraints as dict keys (int/str/bool/allowed tuple. DECISIONS D-TYPE-05). Iteration order is insertion order. An empty set has no literal form; create one with `set()` or `set[T]()` (D-TYPE-03).

**Non-destructive:**

```
def contains[T](self: set[T], x: T): bool
def len[T](self: set[T]): int
def is_empty[T](self: set[T]): bool
def union[T](self: set[T], other: set[T]): set[T]
def intersection[T](self: set[T], other: set[T]): set[T]
def difference[T](self: set[T], other: set[T]): set[T]
def to_list[T](self: set[T]): list[T]
def map[T, U](self: set[T], f: (T) -> U): list[U]
def filter[T](self: set[T], f: (T) -> bool): set[T]
def any[T](self: set[T], f: (T) -> bool): bool
def all[T](self: set[T], f: (T) -> bool): bool
def find[T](self: set[T], f: (T) -> bool): Option[T]
def fold[T, Acc](self: set[T], init: Acc, f: (Acc, T) -> Acc): Acc
def count[T](self: set[T]): int
def sum(self: set[int]): int
def sum(self: set[float]): float
def each[T](self: set[T], f: (T) -> void): void
```

`enumerate`/`zip`/`rev`/`take`/`skip`/`sort_by`/`chain` are only available via `.to_list()`.

**Destructive (`var self` required):**

```
def insert[T](self: var set[T], x: T): bool   # true if newly inserted
def remove[T](self: var set[T], x: T): bool   # true if it existed and was removed
def clear[T](self: var set[T]): void
```

### 2.4 tuple[A, B, ...]

```
t.0, t.1, ...        # access elements with 0-based dot notation (DECISIONS D-TYPE-06)
```

Destructuring in `match` is positional: `(a, b) => ...`. No additional methods.

---

## 3. stdlib Types

### 3.1 Result[T, E]

Built-in enum (DECISIONS D-TYPE-09):

```
enum Result[T, E]
    Ok(T)
    Err(E)
```

Constructed via positional-argument enum variant construction (`Ok(v)` / `Err(e)`). Type arguments are inferred from the actual arguments or from the type annotation of the assignment target / return type.

```
def is_ok[T, E](self: Result[T, E]): bool
def is_err[T, E](self: Result[T, E]): bool
def ok[T, E](self: Result[T, E]): Option[T]
def err[T, E](self: Result[T, E]): Option[E]
def unwrap[T, E](self: Result[T, E]): T                       # panics: Err (E6007). Includes the Error's message in the trace
def unwrap_or[T, E](self: Result[T, E], default: T): T
def unwrap_or_else[T, E](self: Result[T, E], f: (E) -> T): T
def map[T, E, U](self: Result[T, E], f: (T) -> U): Result[U, E]
def map_err[T, E, F](self: Result[T, E], f: (E) -> F): Result[T, F]
def and_then[T, E, U](self: Result[T, E], f: (T) -> Result[U, E]): Result[U, E]
```

Exhaustiveness checking in `match` requires both `Ok` and `Err`. A wildcard `_ => ...` can catch both at once (DECISIONS D-TYPE-09's exhaustiveness rules are on par with ordinary enums).

### 3.2 Option[T]

```
enum Option[T]
    Some(T)
    None
```

Constructed as `Some(v)` (one positional argument) or `None` (a parenthesis-less unit variant).

```
def is_some[T](self: Option[T]): bool
def is_none[T](self: Option[T]): bool
def unwrap[T](self: Option[T]): T                              # panics: None (E6007)
def unwrap_or[T](self: Option[T], default: T): T
def unwrap_or_else[T](self: Option[T], f: () -> T): T
def map[T, U](self: Option[T], f: (T) -> U): Option[U]
def and_then[T, U](self: Option[T], f: (T) -> Option[U]): Option[U]
def filter[T](self: Option[T], f: (T) -> bool): Option[T]
def ok_or[T, E](self: Option[T], err: E): Result[T, E]
```

### 3.3 Error

```
struct Error
    kind: str              # "net" | "fs" | "decode" | "proc" | "time" | "regex" | ... user-defined values are also free to use
    message: str
    cause: Option[Error]
```

Per struct construction rules, named arguments are always required. No factory methods are provided (DECISIONS D-STDPOL-05). Even when there is no `cause`, it must be stated explicitly:

```
Error(kind: "net", message: "timeout", cause: None)
```

Fields are publicly accessible (`e.kind` / `e.message` / `e.cause`, following struct's direct field access rules; no getters needed).

Convention for the `kind` values returned by each stdlib module (users may also use arbitrary strings):

| Module | kind |
|---|---|
| `fs` | `"fs"` |
| `http` | `"net"` |
| `json`/`csv`/`yaml`/`toml` | `"decode"` |
| `proc` | `"proc"` |
| `time.parse` | `"time"` |
| `regex` | `"regex"` |
| `str.parse_int`/`parse_float` | `"decode"` |

### 3.4 Value

```
enum Value
    Null
    Bool(bool)
    Int(int)
    Float(float)
    Str(str)
    List(list[Value])
    Dict(dict[str, Value])
```

```
def as_int(self: Value): Option[int]
def as_float(self: Value): Option[float]
def as_str(self: Value): Option[str]
def as_bool(self: Value): Option[bool]
def as_list(self: Value): Option[list[Value]]
def as_dict(self: Value): Option[dict[str, Value]]
def is_null(self: Value): bool
def get(self: Value, key: str): Option[Value]     # only for the Dict variant; None if the key is absent
def at(self: Value, i: int): Option[Value]         # only for the List variant
```

Access via `match` destructuring is also possible (e.g. `Dict(d) => ...` in a `match v`).

---

## 4. codec (json / csv / yaml / toml)

Per DECISIONS D-STDPOL-03, decode/encode are uniformly driven by the assignment-target type annotation.

### 4.1 json / yaml / toml (common pattern)

```
def decode[T](s: str): Result[T, Error]     # json.decode / yaml.decode / toml.decode. T is determined by the assignment-target type annotation or an explicit [T]. T=Value also allows dynamic decoding
def encode[T](value: T): str                 # json.encode / yaml.encode / toml.encode
```

YAML is a safe subset (no anchors, aliases, or multi-document support; SPEC §11.1). No effects (pure).

**Constraint on `T` for `toml.encode` (DECISIONS D-STDPOL-09)**: because TOML requires the document root to be a table, `toml.encode[T]` alone is valid only when `T` is `dict[str, V]` or a struct (a `T` with `list`/`set`, etc. at the top level is a type error). `json.encode`/`yaml.encode` are not subject to this constraint (`T` remains unconstrained).

### 4.2 csv

```
def decode[T](s: str): Result[list[T], Error]                    # T is limited to flat structs. The first-line header is matched against field names. [T] must be given explicitly at the call site
def encode[T](rows: list[T]): str                                  # header = field names in struct declaration order
def decode_rows(s: str): Result[list[dict[str, Value]], Error]     # dynamic decode when T is unknown. Every cell is Value.Str
```

The delimiter is fixed as `,`. Newlines are normalized to LF on output. `,` / `"` / newlines inside a field are escaped per RFC 4180, using double quotes with doubling. No effects (pure).

Each field type of `T` in `decode[T]` may only be `int`/`float`/`bool`/`str` (specifying a struct for `T` that contains a field of any other type is a compile-time error, equivalent to E1002). Each cell undergoes a conversion equivalent to `parse_int`/`parse_float`; on failure it returns an `Err` with `kind: "decode"`, failing the whole decode. If the header is missing any of `T`'s field names, the result is `Err`. Extra columns not present in `T` are ignored.

---

## 5. fs — effect: `fs`

```
def read(path: str): Result[str, Error] uses {fs}
def read_bytes(path: str): Result[list[int], Error] uses {fs}
def write(path: str, content: str): Option[Error] uses {fs}    # None = success, Some(e) = failure (DECISIONS D-TYPE-08: void cannot appear in a Result's type-argument position)
def append(path: str, content: str): Option[Error] uses {fs}   # None = success, Some(e) = failure
def list(path: str): Result[list[str], Error] uses {fs}     # a list of full paths
def exists(path: str): bool uses {fs}                        # IO errors are treated as false. Not wrapped in a Result
def remove(path: str): Option[Error] uses {fs}                 # None = success, Some(e) = failure
```

---

## 6. http — effect: `net`

```
struct Response
    status: int
    headers: dict[str, str]
    body: str

struct HttpOptions
    headers: dict[str, str]
    timeout_ms: int

def get(url: str): Result[Response, Error] uses {net}
def post(url: str, body: str): Result[Response, Error] uses {net}
def put(url: str, body: str): Result[Response, Error] uses {net}
def delete(url: str): Result[Response, Error] uses {net}
def request(method: str, url: str, opts: HttpOptions): Result[Response, Error] uses {net}   # For methods that need a body, it is not taken as a separate argument outside opts. Full control with a body via request is left as a possible future extension
```

`get`/`post`/`put`/`delete` are the simple forms (a fixed internal timeout, no extra headers). To specify headers or a timeout, construct `HttpOptions` with named arguments and use `request` (DECISIONS D-STDPOL-04; default arguments are not used). Client-side only; servers are out of scope (SPEC §11.2).

---

## 7. env — effect: `env`

```
def get(key: str): Option[str] uses {env}     # None if unset; never fails
def set(key: str, value: str): void uses {env}
def args(): list[str] uses {env}                # arguments to the script itself. Does not include the executable path
def stdin(): Result[str, Error] uses {env}      # reads everything through EOF. IO errors are Err
```

Reading stdin is included in the `env` effect (SPEC §8, §11.2).

---

## 8. proc — effect: `proc`

```
struct ProcOutput
    stdout: str
    stderr: str
    exit_code: int

def run(cmd: str, args: list[str]): Result[ProcOutput, Error] uses {proc}   # Only a launch failure is Err. A non-zero exit still returns Ok(ProcOutput) (check exit_code)
```

---

## 9. time — effect: `time`

There is no dedicated DateTime/Duration type; time is represented as epoch milliseconds (int) (DECISIONS D-STDPOL-06).

```
def now(): int uses {time}                                   # UNIX epoch milliseconds
def sleep(ms: int): void uses {time}
def format(epoch_ms: int, fmt: str): str uses {time}           # strftime-style format (%Y-%m-%d %H:%M:%S)
def parse(s: str, fmt: str): Result[int, Error] uses {time}    # kind: "time"
```

---

## 10. rand — effect: `rand`

```
def int(lo: int, hi: int): int uses {rand}       # half-open interval [lo, hi)
def float(): float uses {rand}                    # [0.0, 1.0)
def bool(): bool uses {rand}
def choice[T](xs: list[T]): Option[T] uses {rand}  # an empty list is None (does not abort)
def shuffle[T](self: var list[T]): void uses {rand}
```

`rand.int` / `rand.float` live in a separate namespace (under the `rand.` qualifier) from the type-conversion functions `int(x)`/`float(x)` in 1.1/1.3, so there is no name collision.

---

## 11. regex — effect: none (pure)

There is no dedicated Regex type; patterns are passed as str every time (DECISIONS D-STDPOL-07). An invalid pattern does not panic; it is returned via `Result`.

```
def is_match(pattern: str, s: str): Result[bool, Error]
def find(pattern: str, s: str): Result[Option[str], Error]
def find_all(pattern: str, s: str): Result[list[str], Error]
def replace(pattern: str, s: str, replacement: str): Result[str, Error]        # first match only
def replace_all(pattern: str, s: str, replacement: str): Result[str, Error]
def captures(pattern: str, s: str): Result[Option[list[str]], Error]           # index 0 = the whole match
```

---

## 12. math — effect: none (pure)

```
def checked_div(a: int, b: int): Option[int]
def checked_mod(a: int, b: int): Option[int]
def checked_add(a: int, b: int): Option[int]
def checked_sub(a: int, b: int): Option[int]
def checked_mul(a: int, b: int): Option[int]
def abs_int(x: int): int
def abs_float(x: float): float
def min_int(a: int, b: int): int
def max_int(a: int, b: int): int
def min_float(a: float, b: float): float
def max_float(a: float, b: float): float
def floor(x: float): int
def ceil(x: float): int
def round(x: float): int
def sqrt(x: float): float
def pow(base: float, exp: float): float
```

Constants (module-level constants, SPEC §10):

```
math.PI: float
math.E: float
```

Note that the per-type naming of `abs_int`/`abs_float`, etc. is *not* covered by the overload special-case in DECISIONS D-STDPOL-01 (they use distinct per-type names just like ordinary user-defined functions).

---

## 13. Built-in Functions — effect: none (print/eprint/assert can all be called inside pure functions)

```
def print(value: str): void
def print(value: int): void
def print(value: float): void
def print(value: bool): void
def eprint(value: str): void      # outputs to stderr
def eprint(value: int): void
def eprint(value: float): void
def eprint(value: bool): void
def assert(cond: bool): void             # exits 1 on failure. Automatically displays the condition expression's source text as the message (DECISIONS D-STDPOL-01)
def assert(cond: bool, msg: str): void   # exits 1 on failure, displaying msg (panic-style. E6004, DECISIONS D-ERR-05)
```

All of these are the overload special-case limited to the four primitive types (DECISIONS D-STDPOL-01). There is no implicit stringification of struct/enum (D-STDPOL-02). To print one, be explicit, e.g. `json.encode(v) |> print`. There is no log-level mechanism (SPEC §11.3).

---

## 14. Table of Operations That Abort Immediately and Their Safe-API Counterparts

Mapping between the E6xxx operations covered by DECISIONS D-ERR-04 and their safe-API counterparts.

| Dangerous operation | Abort condition (diagnostic code) | Safe version |
|---|---|---|
| `xs[i]` (list) | i out of range (E6001) | `xs.get(i): Option[T]` |
| `xs.slice(start, end)` (list/str) | start/end out of range (E6001) | check `xs.len()` beforehand (no dedicated safe version) |
| `m[k]` (dict) | k does not exist (E6001) | `m.get(k): Option[V]` |
| `s.slice(start, end)` (str) | out of range (E6001) | check `s.len()` beforehand |
| `xs.insert(i, x)` / `xs.remove(i)` (list) | i out of range (E6001) | check `i < xs.len()` beforehand (no dedicated safe version provided) |
| `a / b` (int) | b == 0 (E6002) | `math.checked_div(a, b): Option[int]` |
| `a % b` (int) | b == 0 (E6002) | `math.checked_mod(a, b): Option[int]` |
| `a + b` / `a - b` / `a * b` (int) | i64 overflow (E6003) | `math.checked_add` / `checked_sub` / `checked_mul` |
| `int(x: float)` | out of i64 range (E6003) | check the range beforehand (no dedicated safe version provided) |
| `r.unwrap()` (Result) | Err (E6007) | `r.unwrap_or(default)` / `r.ok(): Option[T]` |
| `o.unwrap()` (Option) | None (E6007) | `o.unwrap_or(default)` |
| `assert(cond)` / `assert(cond, msg)` | cond is false (E6004) | (no safe version -- this feature is intended for test failures) |
| top-level `expr?` | Err/None (E6005/E6006) | branch explicitly inside a function using `match`/`.is_ok()`, etc. |
| deep recursion | stack overflow (E6008) | replace recursion with iteration (`fold`, etc.) (no safe version) |

`str`'s `[]` syntax does not exist, so it is excluded from the table above (only `s.get(i)` exists, which always returns `Option[str]` and never panics).
