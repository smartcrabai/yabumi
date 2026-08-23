# Yabumi Implementation Detail Decisions (DECISIONS)

This document is subordinate to `SPEC.md`. **Where this document conflicts with SPEC.md, SPEC.md takes precedence.** This document consolidates and finalizes the undefined issues (gaps) that four analysts identified by area, covering implementation details that SPEC.md does not explicitly specify.

Each decision was adjudicated against the design priorities stated at the top of SPEC — **"zero-dependency distribution > machine-readable errors > correct on the first try > permission auditing"** — and the overarching philosophy of **"appearance = behavior."** Where multiple analysts' recommendations conflicted, one was adopted based on its consistency with SPEC's philosophy and existing settled rules, with the rationale for the ruling recorded explicitly.

Decision IDs follow the `D-<area>-<sequence>` format. Issues reported as `risk: high` are treated in detail in a dedicated "Ruling" section.

---

## Table of Contents

1. Lexical (LEX)
2. Syntax & Layout (SYN)
3. Type System Fundamentals (TYPE)
4. Collections (COL)
5. Mutability & Memory Model (MUT)
6. Functions, Lambdas & Generics (FUNC)
7. Operators (OP)
8. Effect System (EFF)
9. Error Handling & Panics (ERR)
10. Concurrency (PAR)
11. Modules (MOD)
12. Execution Model & CLI (RUN / CLI)
13. Doc Tests (DOC)
14. fmt (FMT)
15. lint (LINT)
16. Standard Library Design Policy (STDPOL)
17. Diagnostics & Error Code System (DIAG) *Required table

---

## 1. Lexical (LEX)

### D-LEX-01 Reserved word list
**Decision**: The following are fixed as reserved words and cannot be used as variable names, function names, type names, or field names.

```
def struct enum if else match return var uses par
true false self and or not in _ module void
```

`Ok`, `Err`, `Some`, and `None` are not reserved words. They are identifiers that the prelude (the built-in `Result`/`Option` enums) pre-registers into the single flat namespace at startup; if a user defines a new item with the same name, it is rejected as an ordinary name collision (E1001 in D-DIAG). `int`, `float`, `bool`, and `str` are likewise not reserved words — they are treated as type names pre-registered in the flat namespace (and, per D-TYPE-14, also as conversion call names).

The built-in namespace names (`fs`, `http`, `env`, `proc`, `time`, `rand`, `regex`, `math`, `json`, `csv`, `yaml`, `toml`) are not managed by the flat namespace above (D-TYPE-07); they are fixed identifiers belonging to a separate name-resolution system used exclusively for module resolution. Users are permitted to define top-level functions, constants, or variables with the same names in the flat namespace, and this does not constitute a name collision (E1001) — a top-level identifier and a namespace identifier of the same name coexist independently, and this has no effect on resolving the dot-qualified call syntax `namespace.function(...)` (this generalizes, to every other namespace identifier, the existing statement in STDLIB.md that `rand.int` does not collide with the type-conversion `int(x)`).
**Rationale**: Simplifies the hand-written lexer (zero-dependency distribution). Collisions between syntactic keywords and identifiers can be determined uniquely at compile time. `void` was added along with the introduction of the Unit-like type in D-TYPE-08. `in` is currently an unused operator but is reserved for future membership-test use.
**SPEC reference**: §2, §5, §8, §9

### D-LEX-02 Character set for identifiers
**Decision**: Identifiers are ASCII only: `[a-zA-Z_][a-zA-Z0-9_]*`. UTF-8 is permitted only within string literals and comment bodies.
**Rationale**: Ease of grepping LLM-generated code, simplification of the fmt implementation, and avoidance of character-encoding normalization issues when matching type names under nominal typing.
**SPEC reference**: §2, §3

### D-LEX-03 Integer literals
**Decision**: Decimal only. The grammar is `[0-9][0-9_]*`. `_` is a digit separator (has no effect on the value). A leading or trailing `_`, or consecutive `_`, is a lexical error (E0004). Hexadecimal, octal, and binary literals are not supported. A leading zero (`007`) is permitted as an ordinary decimal number; it is not interpreted as octal.
**Rationale**: Decimal is sufficient for the shell-script-replacement use case. Simplifies both the lexical rules and fmt normalization.
**SPEC reference**: §3.1

### D-LEX-04 Floating-point literals
**Decision**: The grammar is `[0-9][0-9_]*\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?`. At least one digit is required both before and after the decimal point (`.5` and `5.` are lexical errors, E0004). Digits in the exponent part also allow `_` separators, using the same separator rules as integer literals (leading, trailing, or consecutive `_` is a lexical error). Unary minus is treated as an operator rather than part of the literal (`-5` is an expression with `-` already applied). The one exception is D-SYN-06 (match literal patterns), where `-<numeric literal>` is specially permitted as a single pattern token.
**Ruling**: Since the integer side (D-LEX-03) has underscore separators, prohibiting them only on the floating-point side would make the lexical rules asymmetric and undermine the consistency of "appearance = behavior." Applying the same separator rule to both unifies them.
**SPEC reference**: §3.1

### D-LEX-05 String literals
**Decision**: Double-quoted `"..."` only. There is no single-quote form and no char type. Multi-line strings containing a literal newline are not allowed (express them with the `\n` escape). Raw strings (`r"..."`) are not supported in v1 (reserved as a future backward-compatible extension).
**Rationale**: Minimizes branching in the lexical rules, assuming a hand-written lexer for zero-dependency distribution.
**SPEC reference**: §3.1, §6.4

### D-LEX-06 Escape sequences
**Decision**: The supported escapes are `\n`, `\t`, `\r`, `\\`, `\"`, `\0`, and `\u{H..H}` (1 to 6 hex digits, a Unicode code point). An unknown escape (e.g. `\z`) is a lexical error (E0003).
**Rationale**: Rust-style escape rules match an LLM's existing knowledge and are self-evident to implementers.
**SPEC reference**: §2, §3.1

### D-LEX-07 f-strings
**Decision**: `{{` -> literal `{`, `}}` -> literal `}`. The `expr` in `{expr}` allows the full expression grammar — identifiers, dotted references, calls, subscripts, arithmetic expressions, and so on — but **writing a string literal (an expression containing a double quote) inside expr is prohibited**. Format specifiers (`{x:.2f}`) are not supported in v1 — use `str(x)` from D-TYPE-14 for numeric formatting. Nested f-strings are prohibited.
**Ruling**: The prohibition on string literals inside expr is a constraint that lets a hand-written lexer scan an f-string using only double-quote matching; there is a practical workaround (bind the value to a variable first, then interpolate it).
**Risk note (risk: high)**: This constraint produces the counterintuitive behavior that a key string cannot be written directly inside an f-string, e.g. `dict.get("key")`. As a workaround, document binding `k = "key"` first and writing `f"{d.get(k)}"`.
**Type constraint on expr**: The type of `expr` is not limited to `str`. `int`/`float`/`bool` are automatically converted to `str` and interpolated, via the same built-in conversion rule as `print` in D-STDPOL-01 (`struct`/`enum` cannot be interpolated per D-STDPOL-02 and are a type error). An explicit call to `str(x)` is needed only when format-specifier-level control (e.g. `.2f`) is desired.
**Scanning algorithm**: Scanning of `{expr}` starts at depth 0, and the `{`/`}` that appear inside expr from things like dict/set literals (which are not subject to the prohibition above, since they are not string literals) count toward the depth: `{` increments the depth by 1, `}` decrements it by 1, and the `}` at which the depth returns to 0 is the end of the interpolation. The `{{`/`}}` escape notation is not interpreted inside expr (escaping applies only to literal portions of the f-string body).
**SPEC reference**: §6.4

### D-LEX-08 Syntax of the module directive
**Decision**: A bare keyword with no accompanying name. The effective first line of a module file (after shebang removal) is `module` alone.
**Rationale**: Given that §10 has no namespace separation and uses a single flat namespace, a module name would carry no meaning. Allowing both a named and an unnamed form would complicate the detection logic.
**SPEC reference**: §2, §10

### D-LEX-09 Processing order of shebang and the module directive
**Decision**: Lexing first skips the shebang (if line 1 begins with `#!`), then treats the following line as the effective first line for the module-directive check.
**Rationale**: Without an explicit processing order, implementers would diverge in their interpretation.
**SPEC reference**: §2, §10

---

## 2. Syntax & Layout (SYN)

### D-SYN-01 Strict indentation checking
**Decision**: The number of leading spaces on each line must be, relative to the preceding non-blank line, one of "the same / +4 / -4 or more in steps of 4" — otherwise it is a syntax error (E0501). Even a single tab character is an immediate error (E0001, detected at the lexical layer).
**SPEC reference**: §2

### D-SYN-02 Semantics of blank lines
**Decision**: Blank lines have no effect on block-structure determination (indentation is judged by comparison with the next non-blank line). fmt normalizes consecutive blank lines to at most one, and unifies the end of the file to exactly one trailing newline (no trailing blank line). The definition of "blank line" is extended to comment-only lines (a line where, after leading whitespace, `#`/`##` is followed by no actual code) — comment-only lines are neither the basis for nor the target of indentation comparison (comparison is done between the surrounding non-comment lines).
**SPEC reference**: §12

### D-SYN-03 if/else layout and multi-way branching
**Decision**: `else` is written at the same indentation column as its corresponding `if`. There is no dedicated `elif`-equivalent construct. Multi-way branching is expressed either by nesting an `if` inside an `else` block (indenting one level deeper on the line after `else` and writing `if`) or by using `match`.
**Rationale**: Minimizes the number of keywords and is consistent with the design policy of steering multi-way branching toward `match`, which has exhaustiveness checking.
**SPEC reference**: §6.1

### D-SYN-04 Newlines inside brackets are ignored
**Decision**: While `(`, `[`, or `{` is open, newlines and leading indentation are syntactically ignored (implicit line continuation). There is no syntactic constraint on the position of the closing bracket; fmt formats it into canonical form.
**SPEC reference**: §3.2, §6.3

### D-SYN-05 Bracket-less line continuation (method chains)
**Decision**: A line is treated as an implicit continuation only when it is indented deeper than the start of the statement and its first token is `.` or `|>`. Any other newline ends the statement. The indentation amount of the continuation is syntactically free as long as it's a multiple of 4, but fmt always normalizes it to the base line +4.
**Rationale**: This is a required rule for making the SPEC §15 sample itself (a multi-line method-chain notation) valid. It explicitly closes off the one exception to "newline = end of statement."
**SPEC reference**: §15 sample

### D-SYN-06 match pattern grammar
**Decision**: v1 supports only: (1) enum variant destructuring (positional, `Circle(r)`), (2) literal patterns (int/float/bool/str; numeric literals may carry a unary minus, the special case from D-LEX-04), (3) simple binding variables, (4) the wildcard `_`, and (5) tuple destructuring (positional, `(a, b) => ...`; since a tuple's arity is fixed by its type, a single pattern always satisfies exhaustiveness — consistent with the existing description in D-TYPE-06/STDLIB.md §2.4). Guards (`pattern if cond`) and OR patterns (`A | B`) are not supported in v1.
**Rationale**: Simplifies the exhaustiveness-check implementation (a naive exhaustiveness algorithm suffices if only variant destructuring, tuple destructuring, and the wildcard are involved).

**Pattern nesting**: The only pattern kinds permitted at each argument (element) position of (1) enum variant destructuring and (5) tuple destructuring are the three kinds (2) literal, (3) simple binding variable, and (4) wildcard. Nesting another enum-variant-destructuring pattern or tuple-destructuring pattern further inside (e.g. `Some(Circle(r))`) is prohibited in v1 (nest `match` expressions instead if nesting is needed). This textually guarantees the premise (no nesting) behind the "naive exhaustiveness algorithm" rationale above.

**Name resolution for bare identifiers**: A bare identifier token in a pattern (an identifier with no parentheses) is interpreted as a D-SYN-07 unit-variant pattern if it matches one of the field-less (unit variant) names of the scrutinee's enum type, and otherwise as a new binding variable per (3) (the naming convention D-LINT-05 is a lint warning and is not used for syntactic determination).
**SPEC reference**: §6.1

### D-SYN-07 Symmetry between enum variant construction and pattern destructuring (positional arguments)
**Decision**: **Constructing an enum variant is always positional** (no named arguments). In contrast to struct construction, which requires named arguments (§3.5), enum variants pass their declared fields positionally: `Circle(3.0)`. Destructuring a variant in `match` is likewise positional and in the same order: `Circle(r) => ...`. A variant with no fields (a unit variant) omits the parentheses and is constructed/destructured with a bare variant name (`Red`).
**Ruling (risk: high)**: Analysts had separately raised three points: (a) the argument form for enum construction was unspecified, (b) whether `match` destructuring is positional or named was unspecified, and (c) a proposal to treat `Result`/`Option`'s `Ok`/`Err`/`Some`/`None` as "an enum-like special case exempt from the struct rule requiring named arguments." Stacking these up as individual rules would produce "special cases sprouting all over enum that differ from struct," which lacks consistency. They were therefore consolidated into a single rule: **"only struct construction requires named arguments; enum variant construction is, from the outset, consistently positional."** Under this rule, `Ok(v)` / `Err(e)` / `Some(v)` / `None` are not special cases — they are simply one instance of ordinary enum variant construction (see D-TYPE-13). This is also fully symmetric with `match` destructuring being positional, and best fits "appearance = behavior."
**SPEC reference**: §3.5, §6.1, §15

### D-SYN-08 Hoisting of top-level declarations
**Decision**: Declarations (`def` / `struct` / `enum` / module-level constants) are all registered up front at load time, regardless of where they appear in the file (hoisting occurs; order does not matter and mutual recursion is allowed). Only non-declaration statements (assignments, expression statements) execute sequentially in written order. "No main function — execution from the top" (§5) refers only to the execution order of non-declaration statements.
**Rationale**: Since §10 already designs modules to bring in only declarations, independent of import order ("root out the problem where behavior changes with import order"), declarations within the entry file should follow the same rule for consistency. Hoisting declarations is scope construction at load time and does not affect execution order (i.e. the apparent order of statements), so it does not violate "appearance = behavior."
**SPEC reference**: §5, §10

### D-SYN-09 No loop construct exists
**Decision**: The language has no for/while loop construct. Iteration is expressed only through the iterator method chains of §6.2. For side-effect-only sequential iteration, add `xs.each(f)` to the stdlib (the non-parallel counterpart of `par_each`, returning `void`).
**Rationale**: for/while never appear anywhere in the SPEC, and the "appearance = behavior" philosophy disfavors implicit control-flow branching such as break/continue. To keep committing to a unified Rust-style method vocabulary, one piece of vocabulary is added for simple sequential side-effecting processing.
**SPEC reference**: §6.2, §9

### D-SYN-10 Scope of a lambda body's "single expression"
**Decision**: Since if/match are specified in §6.1 as value-returning expressions, a multi-line if/match expression is also included in a lambda body's "single expression." However, a sequence of statements within a lambda (e.g. the equivalent of `;` or multiple assignment statements) remains prohibited.
**SPEC reference**: §5.1, §6.1

### D-SYN-11 Block-value rule for multi-statement arms
**Decision**: When an if/match branch or arm consists of multiple statements (via a newline and indentation after `=>`), if the final statement placed at the end of the block is an expression statement, that expression's value becomes the value of the whole block. If the final statement has no value (e.g. an assignment or a `var` declaration), it is a type error (covered by E1020, the same category as failure to unify branches of if/match).
**Rationale**: SPEC §6.1 specifies if/match as value-returning expressions and permits multi-statement arms, but without a block-value rule defining what value a multi-statement block returns, an expression-oriented language cannot be implemented. The Rust-style convention that "the block's trailing expression statement is the block's value" matches an LLM's existing knowledge.
**SPEC reference**: §5.1, §6.1

---

## 3. Type System Fundamentals (TYPE)

### D-TYPE-01 Single-element tuples
**Decision**: A single-element tuple requires a trailing comma: `(x,)`. `(x)` is not a tuple — it is simply a parenthesized expression.
**SPEC reference**: §3.2

### D-TYPE-02 Trailing commas
**Decision**: list/dict/set/tuple literals and function calls that span multiple lines allow a trailing comma, which fmt inserts automatically. It is optional on a single line, and fmt removes a trailing comma on a single line.
**Rationale**: Codifies the gofmt-style "single canonical form" for verifying fmt's idempotence.
**SPEC reference**: §3.2, §12

### D-TYPE-03 Meaning of empty collection literals
**Decision**: `{}` is always interpreted as an empty dict — a fixed rule. An empty set cannot be written as a literal; it is created by calling `set()` (or explicitly `set[T]()`). For a non-empty `{...}`, it is judged to be a dict if a `:` immediately follows the first element, and a set otherwise.
**Rationale**: Preserving "appearance = behavior" requires that the meaning of `{}` never change based on annotations or runtime context. Deciding it is always a dict is the most predictable choice.
**SPEC reference**: §3.2, §3.4

### D-TYPE-04 Unification of collection element types
**Decision**: Every element of a list/dict/set must be unifiable into a single, declared or inferred, concrete type. If unification fails, it is a type error (E1010). There is no automatic fallback to Any/Union when types are mixed.
**SPEC reference**: §3.2 (derived from nominal typing with no structural subtyping)

### D-TYPE-05 Type constraints on dict keys / set elements
**Decision**: A dict's `K` and a set's `T` are restricted to int/str/bool, and to tuple[...] whose elements are all themselves permitted key types. float/list/dict/set/struct/enum/Result/Option/Error/function types are prohibited as keys/elements (E1011 for dict, E1012 for set).
**Rationale**: A value-semantics, RC-based implementation makes a hash-map implementation natural, but using float or mutable collections as keys breaks equality/hash stability.
**SPEC reference**: §3.2

### D-TYPE-06 Tuple element access
**Decision**: Element access uses 0-based dot notation: `t.0`, `t.1` (avoiding confusion with list's `[]`). Destructuring in `match` is positional: `(a, b) => ...`.
**SPEC reference**: §3.2

### D-TYPE-07 Flat namespace for enum variant names and type names
**Decision**: struct names, enum names, enum variant names, top-level function names, and top-level constant names all belong to the same single flat namespace; a duplicate is E1001 ("duplicate name"). Local variable names occupy a separate namespace scoped to the function (shadowing is subject to the lint warning D-LINT-03). `Ok`/`Err`/`Some`/`None` are also pre-registered by the prelude in this namespace, so a user redefining the same name gets E1001.
**Rationale**: Pins down the scope of application of §10's "no namespace separation; a name collision is a type error."
**SPEC reference**: §10

### D-TYPE-08 The type of a function with no return value (void)
**Decision**: Add `void` to the primitives. It has zero size, and no value of it can ever be written (no literal, cannot be constructed, cannot be stored or compared). A function's return-value annotation cannot be omitted; a function that returns nothing must write `: void` explicitly. Example:
```
def par_each[T](self: list[T], f: (T) -> void): void
```
**Ruling (risk: high)**: Three competing proposals existed for "how to notate a function with no return value": (a) introduce a new `void` type with no value and no literal, (b) introduce a new `Unit` type (with a value `()`), and (c) treat `()` as the default when a function's return-value annotation is omitted. Adopting `()` as a value literal would overlap, both lexically and syntactically, with the existing "`()` in a function-type annotation means zero arguments" (`() -> int`) and with tuple/grouping `()`, increasing ambiguity in the hand-written parser (contrary to the zero-dependency-distribution / implementation-simplicity priority). `Unit` would also implicitly expand the already-fixed count of four capitalized stdlib types (`Result`/`Option`/`Error`/`Value`, §3.3). Therefore, the lowercase primitive **`void`, which can never produce a value** (cannot be compared, cannot be stored, has no literal) was adopted, and the function return-value annotation remains non-omittable, requiring `void` to be written explicitly. This lets the existing rule "a function signature's type annotations are mandatory" (§3.4) hold with zero exceptions.
**Prohibition on placing void in a type-argument position**: Because of the above decision that `void` "cannot be stored or compared," passing `void` as an actual type argument to a generic type parameter — as in `Result[void, E]` / `Option[void]` / `list[void]` — is prohibited outright (since constructing something like `Ok(<a void value>)` would be undefinable). When a no-return-value operation can fail, express it with `Option[Error]` (`None` = success, `Some(e)` = failure; see `fs.write`/`fs.append`/`fs.remove` in STDLIB.md).
**SPEC reference**: §3.1, §3.4, §9

### D-TYPE-09 Nominal typing of struct/enum and the status of Result/Option
**Decision**: `Result[T, E]` / `Option[T]` have no special-cased syntax — they are ordinary built-in enums:
```
enum Result[T, E]
    Ok(T)
    Err(E)

enum Option[T]
    Some(T)
    None
```
The D-SYN-07 rule (enum variant construction/destructuring is always positional) applies as-is: `Ok(v)` / `Err(e)` / `Some(v)` are single-argument positional construction, and `None` is a parenthesis-less unit variant. The type arguments T/E are inferred from the actual arguments at construction, or from the type annotation of the assignment target/return value.
**Rationale**: Under the "appearance = behavior" philosophy, unifying with the existing enum syntax rather than adding more special-cased syntax is most consistent. The consolidated ruling in D-SYN-07 removes the need to treat Result/Option as a special case.
**SPEC reference**: §3.3, §3.5, §7

### D-TYPE-10 Structure of the Value type
**Decision**: Defined as a built-in enum:
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
Dict keys are fixed to `str` (a practical constraint shared by json/csv/yaml/toml). Access is via match destructuring plus companion safe-side helper methods (details in STDLIB.md).
**SPEC reference**: §3.3, §11.1

### D-TYPE-11 Function arguments: default arguments and variadic arguments are unsupported
**Decision**: A type annotation is mandatory on every parameter. Default arguments (`x: int = 0`) and variadic arguments (`*args`/`**kwargs`) are unsupported entirely, whether for user-defined or stdlib functions. An optional value is expressed explicitly with `Option[T]`. A missing annotation is E1002.
**Ruling (risk: high)**: There was a separate proposal to let stdlib's `http.get` etc. make headers/timeout optional via default arguments, but allowing this would break the general rule "no default arguments whatsoever" with an stdlib-only special case, and would also violate "appearance = behavior" (an implicit value being inserted because the caller omitted it). Accordingly, the http functions are split, per D-STDPOL-04, into **two families: `get(url)` (fixed internal settings) and `request(method, url, opts)` (an `HttpOptions` struct explicitly constructed with named arguments)**, and no default-argument mechanism is introduced anywhere in the language. Likewise, `cause: None` when constructing an `Error` cannot be omitted either — it must always be given explicitly (D-STDPOL-05).
**SPEC reference**: §3.4, §5

### D-TYPE-12 enum variants with struct fields, and unit variants
**Decision**: A field-less variant may omit the parentheses (e.g. the body of `enum Color` lists `Red` / `Green` / `Blue` one by one). A variant with fields is declared, constructed, and destructured positionally, as in D-SYN-07.
**SPEC reference**: §3.5

### D-TYPE-13 struct construction requires named arguments (confirmation)
**Decision**: A struct's constructor always passes every field as a named argument. Positional arguments, default values, and partial omission are not allowed (consistent with D-TYPE-11).
**SPEC reference**: §3.5

### D-TYPE-14 Conversion between primitives is explicit only (int/float/str cast calls)
**Decision**: `int`/`float`/`str` are not reserved words; they are treated as names pre-registered in the flat namespace that are both a type and a callable conversion (matching the Python convention derived from `int(x)` / `str(x)`).
- `float(x: int): float` — always succeeds (for a large i64 there can be precision loss from the 53-bit mantissa, but this is not an error)
- `int(x: float): int` — truncates toward zero. If the result is out of i64 range, it is an integer overflow (E6003) and terminates immediately
- `str(x: int): str` / `str(x: float): str` / `str(x: bool): str` — always succeed. It is an implementation invariant that they round-trip with `parse_int(s): Result[int, Error]` / `parse_float(s): Result[float, Error]` (see D-TYPE-15) — i.e. `parse_float(str(x)) == x`
`int`/`float`/`str` cannot be overridden by defining an ordinary function of the same name (name collision E1001, per the flat-namespace rule of D-TYPE-07).
**Ruling**: Analysts had separately proposed two forms: a `float(x)`/`int(x)` prefix-function form, and a `x.to_int()`/`x.to_float()` method form. The Python convention of using the type name itself as the conversion call most strongly matches an LLM's existing knowledge ("correct on the first try") and avoids adding the new vocabulary `to_int`/`to_float`, so this form was made canonical.
**SPEC reference**: §3.1, §6.4

### D-TYPE-15 Conditions for judging type inference as impossible
**Decision**: The following are defined as un-inferable, requiring an annotation (E1003):
1. An empty list/dict/set literal
2. Initialization from `None` alone (`x = None` is disallowed; `x: Option[T] = None` is required)
3. A generic function call whose type argument is determined only by the return-value type variable (avoidable via the explicit type argument `f[Type](...)` of D-FUNC-04)

However, when items 1-3 above occur in a context governed by the assignment-target-driven type inference defined in D-TYPE-16 (a variable declaration's initializer, a `return` statement, a function call's argument position, or a struct/enum constructor's argument position), and the expected type of that context is already resolved to a concrete type, inference uses that expected type and E1003 is not raised (e.g. `return []` is valid if the function's return-value annotation is `list[int]`; see D-TYPE-16). Only outside these contexts (e.g. a bare expression statement) does the un-inferable rule in this item apply as-is.

Failure to unify types across if/match branches is distinguished as a type error (E1020), not as "un-inferable."
**SPEC reference**: §3.4

### D-TYPE-16 Contexts where assignment-target-annotation-driven type inference applies
**Decision**: The contexts where assignment-target-driven type inference (e.g. `json.decode` in §11.1) applies are limited to exactly these four: (1) a variable declaration's initializer, (2) a `return` statement (from the function's return-value annotation), (3) a function call's argument position (from the argument's type annotation), and (4) a struct/enum variant constructor's argument position (from the field's/positional argument's type annotation). Outside these contexts (e.g. a bare expression statement), if there is no information from a type annotation, it is a type error for being un-inferable. When the expected type in these four contexts is a collection type such as `list[T]`/`dict[K,V]`/`set[T]`, the same assignment-target-driven inference propagates recursively into each element position of a collection literal (e.g. in `xs: list[list[int]] = [[], [1, 2]]`, the inner `[]` is also inferred as `list[int]`).
**Rationale**: Reconciling the §3.4 principle "annotate wherever inference is impossible" with the §11.1 exception of assignment-target-annotation-driven inference requires explicitly bounding where the exception applies. Because struct constructors are specified as a separate category from ordinary function calls (D-TYPE-13), making explicit whether they fall under (3) requires splitting them out as an independent item (4).
**SPEC reference**: §3.4, §11.1

### D-TYPE-17 Implicit Ok/Some wrapping of a return expression
**Decision**: When a function's return-value annotation is `Result[T, E]` or `Option[T]`, the type of the `return` target expression is judged in the following priority order:
1. If the target expression's type already matches the annotation itself (`Result[T, E]` / `Option[T]`), the value is returned as-is (this covers cases like SPEC §15's `return Ok(repos)`, where `Ok`/`Some` is constructed explicitly).
2. If it does not match (1) but the target expression's type matches the (unwrapped) `T`, it is implicitly wrapped as `Ok(expr)` / `Some(expr)` and returned (this covers cases like SPEC §5's `return http.get(url)?.body`, where a bare `T = str` is returned).
3. If neither matches, it is a type error (the same category as E1020).
**Rationale**: The SPEC §5 sample (`return http.get(url)?.body` has type plain `str` while the return-value annotation is `Result[str, Error]`) holds under (2) above, and the SPEC §15 sample (the explicit `return Ok(repos)`) holds under (1), and both are consistent without contradiction. Without a wrapping rule, the §5 sample itself would be a type error and unimplementable.
**SPEC reference**: §5, §7.1, §7.2, §15

### D-TYPE-18 Exhaustiveness requirement for match on a non-enum scrutinee
**Decision**: A `match` on a non-enum scrutinee (int/str) — which is outside the scope of SPEC §6.1's "match on enum has exhaustiveness checking" — always requires a trailing wildcard `_` arm (syntactically enforced; not itself subject to exhaustiveness checking). For `bool`, having both `true`/`false` literal arms present is considered exhaustive in place of a wildcard (treated as equivalent to a built-in two-valued enum). A `match` satisfying neither condition is E1021 (match exhaustiveness violation).
**Rationale**: Without any exhaustiveness rule for non-enum scrutinees, the behavior when a value matches no arm at runtime (panic, or a type error?) would be undefined. Under the "machine-readable errors first" philosophy, it is preferable to statically enforce a mandatory wildcard and eliminate undefined runtime behavior.
**SPEC reference**: §6.1

---

## 4. Collections (COL)

### D-COL-01 Determinism of iteration order
**Decision**: The iteration order of `dict[K, V]` and `set[T]` always **preserves insertion order** (an indexmap-equivalent implementation). Order-dependent methods such as `enumerate`/`rev`/`sort_by` are not attached directly to dict/set; instead, convert to a list first via `.entries()` (dict) / `.to_list()` (set) and then call them.
**Rationale**: Deterministic execution (reproducibility of LLM-generated scripts) requires a fixed iteration order. Attaching order-dependent methods directly to an unordered collection would produce undefined behavior.
**SPEC reference**: §3.2, §6.2

### D-COL-02 Element-rewrite syntax for list/dict/set
**Decision**: Subscript assignment is permitted: `xs[i] = v` (list), `m[k] = v` (dict, added for symmetry). It is a type error (E3001) unless the target variable (or the root reached by tracing back through it, if it's a `var` binding along that path — see D-MUT-03) is a `var`. `str` has no `[]` syntax (see D-COL-03 — only `.get(i)`).

**The handling of out-of-range subscripts / missing keys is asymmetric between reads and writes**:

| Operation | list | dict |
|---|---|---|
| Read `xs[i]` / `m[k]` | Out of range terminates immediately (E6001) | Missing key terminates immediately (E6001) |
| Write `xs[i] = v` / `m[k] = v` | Out of range terminates immediately (E6001) | **Missing key inserts a new entry** (not an error) |

The reason only a dict write permits a missing key is that `m[k] = v` is the syntax for adding an entry to a dict (synonymous with the `insert` method). `xs[i] = v` for a list replaces an existing element — adding an element is the job of `push` — so out-of-range is consistent between read and write, both terminating immediately.
**Revision**: Originally, reads and writes were not distinguished, and the rule was simply "out of range / a missing key is E6001," but during the implementation phase it became clear that `samples/ok/4_mutability` requires adding a new key via `scores["bob"] = 5`, and under the old rule there was syntactically no way to add an entry to a dict. This was clarified so that only writes permit insertion.
**SPEC reference**: §4, §7.4

### D-COL-03 The indexing unit for str
**Decision**: The indexing unit is fixed to the Unicode scalar value (a "char"), not a byte and not a grapheme cluster. A single character is represented as a `str` of length 1 (no dedicated char type is added). Bracket-index syntax such as `s[i]` is not provided for str (only `.get(i): Option[str]`). Iterator methods such as map/filter treat `self` as the equivalent of `.chars()` (`list[str]`) and always return `list[U]` (there is no implicit re-joining into a str — call `.join("")` explicitly to re-join).
**Rationale**: UTF-8 invariance (§3.1) was specified, but the indexing unit was not. Byte offsets are error-prone, and grapheme clusters are costly to implement. Char-based indexing best matches an LLM's intuition (1 character = 1 element).
**SPEC reference**: §3.1

---

## 5. Mutability & Memory Model (MUT)

### D-MUT-01 Mutability of a method's self — `var self`
**Decision**: The `self` of a method defined inside a struct needs no type annotation (implicitly the Self type). A method that mutates a field writes `var` before `self`: `def rename(var self, name: str)`. Inside a method that does not have `var self`, `self.field = x` is a type error (E3001). A caller cannot call a `var self` method unless the target instance is bound with `var` (E3001). Self-less (namespace-style) static methods are unsupported — if a namespace-like function is needed, write it as a top-level function.
**Ruling (risk: high)**: A separate proposal, aimed at reducing the verbosity of constructing `Error`, suggested introducing self-less static methods on structs (calls of the form `Type.method(...)`), but this directly contradicts the decision that "self is always required explicitly; self-less methods are unsupported." Allowing self-less methods would create two parallel call systems — `.`-based method calls and the new `Type.method()` call syntax — undermining the simplicity of the flat namespace and dot-call convention. Therefore **self-less methods remain unsupported**, and the verbosity of `Error` is instead resolved, per D-STDPOL-05, by "always spelling it out explicitly rather than omitting it" (which is also consistent with D-TYPE-11's decision not to introduce default arguments).
**SPEC reference**: §3.5, §4

### D-MUT-02 A mutable receiver is the sole exception to value semantics
**Decision**: Ordinary function arguments are all passed by value copy, and writes do not propagate back to the caller's variable (D-MUT-04). The **sole exception** to this general rule is a call to a `var self` method from D-MUT-01: when the receiver variable to the left of `.` (or the root of the path recursively containing it, per D-MUT-03) is a `var` binding, a `var self` method mutates that variable directly. The built-in collection types' (list/dict/set) mutating methods (`push`/`pop`/`insert`/`remove`/`extend`/`clear`, etc.) are likewise documented as taking a `var self` receiver, just like a user struct method (since list/dict/set are built-in types a user cannot write with `struct`, STDLIB.md uses the notation `self: var list[T]` — for distinction — as "documentation shorthand equivalent to `var self`," which is not executable user syntax).
**Ruling (risk: high)**: Taken at face value, the decision that "function arguments are passed by value copy and do not propagate back to the caller" (D-MUT-04) contradicts the stdlib requirement that "`push`/`insert`, etc. mutate the caller's list." This contradiction is resolved by singling out a method-call receiver under `var self` from ordinary function arguments and positioning it as **the sole exception to value semantics, made visible in the call syntax itself** (a method cannot be called unless the receiver is a `var` binding). This does not violate "appearance = behavior" — the very appearance of the caller invoking a method like `.push(...)` on a `var`-bound variable is itself what represents the behavior that a mutation occurs. On the other hand, writing `var` on an ordinary function parameter (a non-receiver position) to achieve pass-by-reference is not permitted.
**SPEC reference**: §4, §14

### D-MUT-03 Determining propagation of mutability through nesting
**Decision**: For `xs[i] = v` / a mutating method call (`u.tags.push(x)`) / a nested field assignment (`u.address.city = "x"`), mutability is decided by recursively determining whether **the root variable the whole expression ultimately resolves to is a `var` binding**. If the root is not `var`, it is E3001.
**SPEC reference**: §4

### D-MUT-04 Closures and function arguments always capture by value copy
**Decision**: Capture by ordinary closures/lambdas (not limited to `par`) is uniformly by value copy. Mutating a captured variable inside a lambda (even if it is `var`) does not propagate to the outer scope's variable. A struct/list passed as a function argument is likewise passed by copy; receiving it as a `var` parameter inside the function has no effect on the caller. There is no by-reference-passing syntax equivalent to Rust's `&mut`. The implementation may optimize using copy-on-write, but the observable behavior always acts as a copy (the sole exception being the `var self` receiver of D-MUT-01/02).
**Rationale**: §14's "value semantics" is a memory-model principle for the entire language; specially restricting the by-value-copy rule to only `par` (§9) would split closure behavior according to syntax (inside `par` or not), undermining the consistency of "appearance = behavior."
**SPEC reference**: §9, §14

### D-MUT-05 Generic type parameters and mutability are independent axes
**Decision**: Mutability belongs only to a variable binding, never to a type parameter `T`. `var xs: list[T]` only permits reassigning/mutating `xs` itself; the internal mutability of a value of type `T`, once `T` is actually a struct, follows the ordinary struct mutation rules (D-MUT-01 through 03).
**SPEC reference**: §3.6, §4

---

## 6. Functions, Lambdas & Generics (FUNC)

### D-FUNC-01 Effect declarations on struct methods
**Decision**: `uses {..}` is written on each individual method definition (the struct declaration itself carries no effect annotation). The caller adds the effects of the method it calls into its own effect set as-is (the same propagation rule as for top-level functions).
**SPEC reference**: §5, §8

### D-FUNC-02 Effect inference for lambdas
**Decision**: There is no effect-annotation syntax for lambdas. The type checker analyzes the lambda body, infers the union of effects of the effectful functions it calls, and attaches that to the lambda's function type (e.g. `(x) => http.get(x)` has type `(str) -> Result[Response, Error] uses {net}`). Passing an effect-carrying lambda where a pure function's argument is expected is a type error (E2001).
**SPEC reference**: §5.1, §8

### D-FUNC-03 Effect-row inference algorithm for higher-order functions
**Decision**: The type checker walks a function body once and takes the union of the effect sets of every function/method it calls. If there is a call path through a function-typed parameter (e.g. `f: (T) -> U`), the effects of that function type are added to the caller's effects as-is (if the resulting union is not a superset of the declared `uses`, it is E2002, "undeclared effect"). With multiple function-typed parameters, take the union of each's effects. The effect of a recursive function is computed by simple summation, assuming its own `uses` declaration; no fixed-point computation is performed. `par`/`par_map`/`par_each` follow the same propagation rule as any other higher-order function (no special-casing). A function value retains this effect-carrying type after being assigned/stored; if a code path within a pure function body actually **invokes** it, that is E2001, whereas holding and passing it along without invoking it is legal.
**SPEC reference**: §8, §9, §15

### D-FUNC-04 Type arguments for generic functions
**Decision**: Monomorphized to a concrete type at each call site. Type arguments are inferred by unification from the actual arguments' types. When a type variable appears only in the return type and cannot be determined from the arguments, explicit specification is required at the call site: `csv.decode[User](s)` (`[Type]` after the function name). Without an explicit specification, if inference fails, it is E1003.
**SPEC reference**: §3.6, §11.1

### D-FUNC-05 Using operators on an unconstrained type parameter
**Decision**: Since there are no traits/inheritance, no constraint syntax can be written on a type parameter. The only operations that can be called directly on `T` inside a generic function body are "assignment, storage in a container, passing to a function, and structural equality comparison via `==`/`!=`." A function that directly uses an operator such as `+`/`<`/`>`/`<=`/`>=`, which is defined only for specific concrete types, on an unconstrained `T` cannot be defined (E1013 at the point of use).
**Ruling**: `==` is, per D-OP-06, uniquely defined as a structural, recursive comparison across all types and is not subject to operator overloading. Therefore `==`/`!=` can always be applied to an unconstrained `T` too — this is what makes it possible to write a generic method like list's `contains` without any constraint syntax such as `T: comparable`. This needs to be treated differently from operators like `+` and ordering comparisons, which are "defined only for specific concrete types," and making this distinction explicit resolves the apparent contradiction between the two.
**SPEC reference**: §3.6, §6.4

---

## 7. Operators (OP)

### D-OP-01 Operator precedence table
**Decision**: The following is the canonical precedence table (higher rows bind more tightly).

| Precedence | Kind | Operators | Associativity |
|---|---|---|---|
| 1 (tightest) | Postfix | `()` `[]` `.` `?` | Left-associative (applied sequentially left to right) |
| 2 | Unary | `-` `not` | Prefix |
| 3 | Mul/div/mod | `*` `/` `%` | Left-associative |
| 4 | Add/sub | `+` `-` | Left-associative |
| 5 | Comparison | `<` `<=` `>` `>=` | Left-associative (no chained comparisons) |
| 6 | Equality | `==` `!=` | Left-associative |
| 7 | Logical AND | `and` | Left-associative |
| 8 | Logical OR | `or` | Left-associative |
| 9 (loosest) | Pipe | `\|>` | Left-associative |

Chained comparisons (`a < b < c`) cannot be written — use `a < b and b < c`. There is no exponentiation operator (`**`) — use `math.pow`. There are no bitwise operators (out of scope for v1). Assignment `=` / `var x =` is a statement, not an expression, and nested assignment (`y = (x = 5)`) cannot be written.
**Ruling (risk: high)**: Because unary `not`/`-` bind more tightly than comparison operators, `not x > 3` parses as `(not x) > 3`. The type of `not x` is `bool`, and since D-OP-05's ordering comparisons are defined only for int/float/str, this expression is always a type error (E1051). This differs from the intuition one might have from Python (where `not` binds more loosely than comparison), but because **a mistaken usage is caught immediately as a static type error** (rather than silently taking on some other, "black-magic" implicit meaning), it was judged consistent with the "appearance = behavior" / "machine-readable errors first" design philosophy, and the Rust/C-family precedence was adopted as-is. When the intent is ambiguous, always make it explicit with parentheses.
**SPEC reference**: §6.3 (derived by working backward from the constraint that pipe has the lowest precedence and is left-associative)

### D-OP-02 Binding position of the postfix `?`
**Decision**: `?` is applied sequentially, left to right, at the same level as `.`, `[]`, `()` within the postfix chain. `f(x)?.y` is interpreted as `(f(x)?).y` (`.y` after `?` is applied).
**Rationale**: Required to give a unique interpretation to sample code such as `http.get(url)?.body`.
**SPEC reference**: §15

### D-OP-03 No implicit conversion between int and float
**Decision**: There is no implicit conversion whatsoever. Mixing `int` and `float` in a binary arithmetic or comparison operator is a type error (E1050). Use `float(x)` / `int(x)` from D-TYPE-14 to convert.
**SPEC reference**: §3.1

### D-OP-04 Semantics of `/` and `%`
**Decision**: `/` requires both operands to be the same type: int/int -> int (truncated toward zero, Rust/C style), float/float -> float. When floating-point division of two ints is needed, write it explicitly as `float(a) / float(b)`. `%` is int-only, and the sign of the result follows the left operand (`-7 % 3 == -1`). Division by zero is a panic (E6002), per D-ERR. The safe variants are `math.checked_div` / `math.checked_mod`.
**SPEC reference**: §7.4

### D-OP-05 The types eligible for ordering-comparison operators
**Decision**: `<`, `>`, `<=`, `>=` are defined only for int/float/str (lexicographic). They are undefined for bool/struct/enum/list/dict/set/tuple/Result/Option/Error/Value, and using them is E1051. A key function such as for `sort_by` must return one of int/float/str.
**SPEC reference**: §6.4 (since only `==` is explicitly stated to be structural equality, the eligible types for ordering comparison must be defined separately)

### D-OP-06 `==` is structural equality across all types
**Decision**: `==`/`!=` are uniquely defined as recursive structural equality across every type (including struct/enum/list/dict/set/tuple/Result/Option/Error/Value). Reference equality does not exist in the language. User overloading is not possible (since operator overloading itself is unsupported). It can always be applied to an unconstrained type parameter `T` as well (D-FUNC-05).
**SPEC reference**: §6.4

### D-OP-07 Types supported by `+`
**Decision**: `+` supports only int+int->int, float+float->float, str+str->str (concatenation), and list[T]+list[T]->list[T] (concatenation). `+` is not defined for dict/set (since merge semantics are ambiguous — use an explicit method such as `.union()` instead).
**SPEC reference**: §6.4

### D-OP-08 Integer overflow
**Decision**: An out-of-i64-range overflow from `+`, `-`, `*`, or unary `-` terminates immediately (E6003). There is no wrapping or saturating behavior, and the check is always active regardless of debug/release. The safe variants are `math.checked_add`/`checked_sub`/`checked_mul`.
**SPEC reference**: §7.4

---

## 8. Effect System (EFF)

### D-EFF-01 Effect checking is static only (confirmation)
**Decision**: As stated in §8, effect checking is static-only; there is no runtime enforcement. Both direct and indirect calls from a pure function to an effectful function are detected at type-checking time (D-FUNC-03).
**SPEC reference**: §8

### D-EFF-02 Detecting indirect calls
**Decision**: A function value retains its effect-carrying function type even after being assigned/stored. If, within a pure function body, there is a code path that actually "calls" a value of an effect-carrying function type, that is E2001, the same as a direct call. Holding and passing it along without calling it is legal.
**SPEC reference**: §8

---

## 9. Error Handling & Panics (ERR)

### D-ERR-01 Type-matching rule for `?`
**Decision**: Inside a function that returns `Result[T, E]`, `?` may be used only on a `Result`-typed expression (not on an `Option` expression). Conversely, inside a function that returns `Option[T]`, `?` may be used only on an `Option` expression. The error type `E'` targeted by `?` must exactly match the function's `E` annotation (per §7.1's "propagates through with no conversion," there is no `From` conversion). The value-side type `T'` is the type of the result of applying `?` and is unrelated to the function's return-value `T` (inside a function returning `Result[str, Error]`, it is legal to apply `?` to a `Result[int, Error]` expression and use the result as `int`). A mismatch is E1060.
**SPEC reference**: §7.1, §7.2

### D-ERR-02 `?` inside a lambda
**Decision**: `?` inside a lambda applies the same match check as D-ERR-01, against the return-value type the lambda's type inference has resolved (`Result`/`Option`, determined by the calling context). If the lambda's return-value type cannot be inferred, it is a type error.
**Rationale**: A lambda is itself a function boundary, so scoping `?` to "the innermost enclosing function (including a lambda)" prevents unintended propagation to the outer function.
**SPEC reference**: §5.1, §7.2

### D-ERR-03 Boundaries for judging an unused Result
**Decision**:
1. If an expression written alone as an expression statement has type `Result[T, E]`, it is judged unused (E1040). The only way to opt out is `_ = expr`.
2. It is considered used when it appears as the right-hand side of an assignment, a `return` target, a function-call argument, a collection-literal element, or an operand of an operator.
3. An intermediate stage of a pipe `x |> f()` is considered used because it feeds into the next stage. If the final result of the whole pipe is discarded as an expression statement with no trailing `?`, it is E1040.
4. The same applies when an if/match branch returns a `Result` and the whole expression is discarded as an expression statement (E1040; no per-branch exemption).
5. `Option[T]` is outside the scope of this rule (§7.3 states this explicitly only for `Result`; ignoring an `Option` is allowed).
**SPEC reference**: §7.3

### D-ERR-04 Complete enumeration of operations that terminate immediately as a panic
**Decision**: The following are confirmed as immediate-termination (exit 1, E6xxx per D-DIAG) targets:
1. Out-of-range subscript access on a list/tuple/string (E6001)
2. `[]` access on a dict with a nonexistent key (E6001; the safe variant `dict.get(k): Option[V]` is provided alongside)
3. Division by zero for integer `/` and `%` (E6002)
4. An out-of-i64-range overflow from `+`, `-`, `*`, or unary `-`, and an out-of-range conversion in `int(x: float)` (E6003)
5. An `assert` failure (E6004)
6. Failure of `Result.unwrap()` / `Option.unwrap()` (E6007)
7. Stack overflow from deep recursion (E6008)

A string-to-number parse failure (`parse_int`/`parse_float`) is not a panic — it is provided as an API returning a `Result`, and is excluded from panic targets (the distinction being: panic = a programming mistake, Err = an expected kind of failure).
**SPEC reference**: §7.4

### D-ERR-05 Output format for panics / top-level abnormal termination
**Decision**: A panic, and top-level propagation of `?`'s Err/None, **reuse as-is** the §1 diagnostic format `file:line:col [Exxxx] message` (no dedicated separate format is introduced). `message` contains `"panic: <description>"` or `"unwrapped Err via ?: <Error.message>"` / `"unwrapped None via ?"`. The full call stack is not shown (a single frame only). All output goes to stderr, with exit code 1.
**Ruling (risk: high)**: Analysts had proposed two competing approaches: (a) introduce a dedicated 2-line panic trace format (`panic: ...` / `  at file:line:col`, with no E code), and (b) assign E codes within the §1 diagnostic format to cover runtime aborts in general. DIAG-02's requirement that "error codes are stable and machine-readable" applies across the entire §1 CLI, and exempting only panics into a separate, uncoded format would require two parallel output parsers, contrary to the "machine-readable errors first" philosophy. This was resolved by **unifying to a single diagnostic format** and assigning E6xxx codes to panics as well.
**SPEC reference**: §1, §7.2, §7.4, §7.5

### D-ERR-06 Handling of a panic inside par
**Decision**: If a panic occurs inside `par`, the entire process terminates abnormally immediately, without waiting for the other branches to finish. The "no fail-fast — all branches run to completion" rule (§9) applies only to the propagation of `Result`/`Option` error values, not to panics.
**Rationale**: A panic is "uncatchable" (§7.4), and that same section shows no special mechanism for catching one across a `par` boundary.
**SPEC reference**: §7.4, §9

---

## 10. Concurrency (PAR)

### D-PAR-01 Guarantee of result ordering
**Decision**: The results of `par [f(), g()]` / `par (f(), g())` / `xs.par_map(f)` are guaranteed to be in **input (written) order**, not completion order.
**Rationale**: The type tables in §9 (`list[T]`/`tuple[A,B]`) implicitly assume element correspondence; with unordered results, the caller's code could not match results back up, violating "correct on the first try."
**SPEC reference**: §9

### D-PAR-02 Nesting of `par`
**Decision**: Recursive use of `par`/`par_map`/`par_each` (calling `par` inside `par`) is permitted, with no constraints. Scheduling occurs on the runtime's shared thread pool.
**SPEC reference**: §9

### D-PAR-03 Prohibition of a bare top-level `?` inside a par branch
**Decision**: Writing a bare `?` directly inside a branch expression of `par [...]` / `par (...)` / `par_map` / `par_each` (or in the body of a lambda passed to it) is prohibited (treated as a syntax error, E0502). Handle `Result`/`Option` errors explicitly instead, e.g. via `match` or `.is_ok()`/`.unwrap_or()`.
**Rationale**: Since the scope of `?` is "the innermost function" (D-ERR-02), if `?` inside a par branch propagated an Err/None, the entire outer function enclosing the `par` would return early — but at that moment other branches may still be running on separate threads, conflicting with the "no fail-fast — wait for all branches to complete" rule (§9). Rather than introducing a complex interruption protocol that accounts for the execution state of the other branches, this conflict is resolved simply by prohibiting the use of `?` inside a par branch at all (consistent with the zero-dependency-distribution / implementation-simplicity priority).
**SPEC reference**: §7.2, §9

---

## 11. Modules (MOD)

### D-MOD-01 Scope of the module directive (targets of automatic inclusion)
**Decision**: The targets of automatic inclusion (§10) are ".ybm files carrying a `module` directive that reside in the same directory as the entry file, other than the entry file itself." The scan covers **only the immediate directory** (subdirectories are excluded; no recursion). If the entry file itself carries a `module` directive, the D-MOD-02 rule (no top-level executable statements) applies to it, and since an entry file ordinarily has executable statements, this becomes a type error (this is intentional, accepted behavior).
**Rationale**: A literal reading of "same directory" is the most predictable interpretation. Implicit recursive searching would violate "appearance = behavior."
**SPEC reference**: §10

### D-MOD-02 Syntactic scope of module-level constants
**Decision**: A module-level constant is permitted only in the form `x = <a literal or constant expression>`. The right-hand side is limited to numeric, string, bool, and collection literals and combinations thereof (including references to other constants); an expression containing a function call is prohibited (treated as an executable statement and a type error).
**Rationale**: Preserving §10's goal of "rooting out the problem where behavior changes with import order" requires that a constant's right-hand side never introduce side effects or evaluation-order dependence.
**SPEC reference**: §10

### D-MOD-03 Circular references between modules
**Decision**: The concept of a circular-reference error does not exist. Because the declarations of every file in the same directory are registered together, up front, into a single flat namespace at load time before type checking, mutual references work fine as ordinary function calls.
**SPEC reference**: §10

### D-MOD-04 Automatic inclusion is common to all three commands
**Decision**: Automatic inclusion of same-directory modules applies to all three of `ybm <file>` / `ybm check <file>` / `ybm test <file>`. §10's mention of `ybm check` merely spells out the additional behavior that "fmt/lint are also run against modules."
**Rationale**: If functions defined in a module could not be called from the entry file's executable statements or from doc tests, the module feature itself would be meaningless. This is corroborated by §13's statement that "the scope of doc tests is the whole file (the entry file plus all declarations of same-directory modules)."
**SPEC reference**: §10, §13

### D-MOD-05 Diagnostic format for a name collision
**Decision**: The location of the second (colliding) definition found is treated as the primary location, and both locations are embedded into a single line: `file:line:col [E1001] duplicate definition of 'name' (also defined at other_file:line:col)`.
**Rationale**: Since §1's diagnostic format is fixed to a single line (`file:line:col [Exxxx] message`), the only way to convey a related location is to embed it inside the message string.
**SPEC reference**: §1, §10

---

## 12. Execution Model & CLI (RUN / CLI)

### D-CLI-01 Output destination for diagnostics and output
**Decision**: All error/warning diagnostics (lines in `[Exxxx]` format) are written to stderr. The fmt diff shown by `ybm check --check` and the pass/fail summary from `ybm test` are written to stdout. During normal execution (`ybm`), `print`/`eprint` write to stdout/stderr respectively (as per §11.3).
**Rationale**: CI use cases require diagnostics and program output to be separable for piping, and the Unix convention (diagnostics = stderr, results = stdout) maximizes machine-readability.
**SPEC reference**: §1

### D-CLI-02 Position of the `--check` flag
**Decision**: `ybm check <file> --check` is the canonical syntax. The flag's position may come either before or after (`ybm check --check <file>` is equivalent).
**SPEC reference**: §1

### D-CLI-03 Reporting all diagnostics
**Decision**: Every diagnostic that can be collected is collected, printed in full in ascending `file:line:col` order, and then it exits with 1. For a kind that prevents continuing subsequent analysis (such as a syntax error), it terminates with the diagnostics collected up to that point plus that one.
**Rationale**: Under the simple norm "check passes = clean" (§12), a single-pass full report is the most efficient way to support iterative fixing.
**SPEC reference**: §12

### D-CLI-04 Nonexistent file / invalid extension
**Decision**: Reported as a file I/O error — a message such as `file: not found` is written to stderr and it exits with 1. The diagnostic code is assigned from the E9xxx range (separate from the type/lint ranges E0xxx-E4xxx).
**SPEC reference**: §1

---

## 13. Doc Tests (DOC)

### D-DOC-01 Language tag of a fence block
**Decision**: Only a fenced block with no language tag ( ` ``` ` ) is a doc-test target. A block carrying another language tag (e.g. ` ```json `) is ignored as documentation-only and is not a test-execution target.
**SPEC reference**: §13

### D-DOC-02 Handling multiple fence blocks
**Decision**: Multiple fence blocks within a single doc comment are each extracted, executed, and tallied individually, as independent test cases.
**SPEC reference**: §13

### D-DOC-03 Declarations targeted by doc tests
**Decision**: A `##` block immediately preceding any of `def`/`struct`/`enum`/a module-level constant is generalized as a doc-test target.
**SPEC reference**: §2, §13

### D-DOC-04 Continuation after an assert failure within a block
**Decision**: An `assert` failure immediately ends execution of that block (reported as a failure in the D-ERR-05 panic format), and no subsequent lines execute. That block is judged a failure, but it has no effect on the execution of other blocks (each block is an independent execution context).
**SPEC reference**: §7.5, §13

### D-DOC-05 Diagnostic format and line-number basis
**Decision**: A doc-test failure report uses the same `file:line:col [Ennnn] message` format as §1. `line` refers to the actual line number in the source file, not a line number relative to the `##` fence block.
**SPEC reference**: §1, §13

---

## 14. fmt (FMT)

### D-FMT-01 Spacing rules around operators, commas, and colons
**Decision**: One space before and after a binary operator (`a + b`). A comma has one space after and none before. A type-annotation colon has one space after and none before (`x: int`). The same applies to a dict literal's `:`. No space immediately after `(` or immediately before `)`. A unary operator is flush against its operand (`-x`). However, since `not` is a keyword, it takes one space (`not x`).
**SPEC reference**: §12

### D-FMT-02 Normalizing string quotes
**Decision**: Strings are always emitted with double quotes (single quotes don't exist lexically, so they aren't relevant here). An internal `\"` escape is kept as-is by fmt, not unescaped.
**SPEC reference**: §12

### D-FMT-03 Spacing rules for comments and doc comments
**Decision**: One space is always enforced immediately after `#`/`##` (`#comment` -> `# comment`). A trailing end-of-line comment has one space between it and the code.
**SPEC reference**: §2, §12

### D-FMT-04 Threshold for splitting a long pipe
**Decision**: Fixed by a syntactic metric — the number of pipe operators `|>` — rather than a character-count threshold. When there are **3 or more** `|>`, it is always split to one stage per line; with 2 or fewer (`x |> f`, `x |> f |> g`), it stays on one line. When split, the starting expression goes on the first line, and subsequent lines each start with `|> ` at 4 spaces deeper indentation.
**Rationale**: A character-count threshold introduces environment-dependent factors such as font and editor width, undermining verification of fmt's idempotence. A syntactic metric produces a deterministic formatting result.
The threshold was set to 3 because both SPEC §6.3's `x |> parse? |> validate?` and SPEC §15's `top |> toml.encode |> fs.write("top.toml", _)` are written on one line with 2 `|>`s; with a threshold of 2, fmt would split these, and **fmt itself would break the "appearance" that the SPEC body demonstrates.** SPEC §6.3's phrase "a long pipe" is therefore interpreted as meaning 3 or more `|>`.
**Revision**: Originally the rule was "split at 2 or more stages," but since this conflicted with the SPEC body examples above, as discovered during implementation, the threshold was raised to 3.
**SPEC reference**: §6.3, §12, §15

### D-FMT-05 Line-break trigger for list/dict/set/tuple literals and call argument lists
**Decision**: The same kind of syntactic metric as D-FMT-04 is applied to list/dict/set/tuple literals and to a function call's actual argument list: if the source has one or more newlines between the opening and closing bracket, it is normalized to one element per line; if there is no newline, it stays on a single line. This judgment depends on the input (whether the pre-formatting source has a newline), but for a given input it always produces the same output, so fmt's idempotence (fmt(fmt(x)) = fmt(x)) is preserved.
**Rationale**: D-TYPE-02 (the trailing-comma rule for the multi-line case) covers only the formatting once something is already multi-line; no existing decision defined the trigger itself for deciding "stay single-line vs. expand to multi-line." This generalizes, from D-FMT-04, the policy of using a syntactic metric instead of a character-count threshold.
**SPEC reference**: §3.2, §12

### D-FMT-06 Relationship between fmt and code inside a doc-comment fence
**Decision**: fmt (the in-place rewrite done by `ybm check`) does not apply to the contents of a `##` doc comment's fence block (with no language tag). The Yabumi code inside a fence is preserved exactly as written and is not a target of rewriting by fmt.
**Rationale**: Formatting the code inside a fence could change its line count, creating a complex interaction where the recomputation order for D-DOC-05 (that a failure report's line number is the actual line number in the source file) would depend on the execution order of fmt versus doc tests. Excluding fence contents from formatting eliminates this complexity entirely from the implementation (consistent with the zero-dependency-distribution / implementation-simplicity priority). Even if the code inside a fence is not in canonical form, this has no effect on the doc test's type checking or execution result.
**SPEC reference**: §12, §13

---

## 15. lint (LINT)

### D-LINT-01 Unused variables
**Decision**: The targets are local bindings via `x = ...` / `var x = ...`, function arguments, and binding variables in a match arm. A name that is exactly `_`, or an identifier starting with `_` (e.g. `_tmp`), is excluded from this check. Diagnostic code E4001.
**SPEC reference**: §12

### D-LINT-02 Unused functions
**Decision**: Among the `def`s **declared in the entry file itself**, one is warned about (E4002) if it is not called, directly or indirectly, from a top-level executable statement or a doc-test block. The following are excluded from this check:
- **A `def` declared in a file carrying a module directive (a module)** — a module is an API used by multiple entries in the same directory, and it is normal for a given entry not to use it
- **A struct's methods** — likewise, since they may be kept for API purposes

Reachability itself is determined by walking the entire flat namespace (the entry plus same-directory modules). That is, an indirect reference chain such as entry top-level statement -> module function -> entry function also counts as "used." Only `def`s originating from the entry file are subject to the warning.
**Revision (risk: high)**: Originally, `def`s declared in same-directory modules were also warning targets, but during the implementation phase `samples/ok/15_end_to_end_showcase` turned out to be a counterexample — two entries in the same directory share one module, and the module's `fetch_repos` is called from only one of the entries. Under the old rule, running `ybm check` on the other entry would immediately raise E4002, and the configuration SPEC §10 actively endorses — "share one module across multiple entries" — would fail to pass lint. This conflicts with the norm "check passes = clean" (§12). Since the reasoning for excluding struct methods (they may be kept for API purposes) applies equally to a module's `def`s, they were unified under the same treatment.
**SPEC reference**: §10, §12, §13

### D-LINT-03 Shadowing
**Decision**: A warning is always raised (E4003) whenever an inner scope (if/match/a def body/lambda parameters/a match arm) creates a new binding with the same name as an existing name in an outer scope (including a variable, function, or struct/enum name). A function boundary is also treated as an "inner scope" (a function parameter sharing a name with an outer variable is also a target).
**SPEC reference**: §12

### D-LINT-04 Unreachable code
**Decision**: In v1, only "a statement immediately following a `return` statement within the same block" is detected as unreachable (E4004). Unreachability arising from match/if branch exhaustiveness, or via early termination through `?`, is complex to determine statically and is out of scope for v1, prioritizing zero false positives.
**SPEC reference**: §12

### D-LINT-05 Naming conventions
**Decision**: Variables, functions, struct fields, and function arguments follow `^[a-z][a-z0-9_]*$` (snake_case). struct/enum type names and enum variant names follow `^[A-Z][A-Za-z0-9]*$` (PascalCase). Generic type variables (`[T]`) are exempt from this convention (free identifiers such as `T`, `U`, `K`, `V` are allowed). Diagnostic code E4005.
**SPEC reference**: §12

---

## 16. Standard Library Design Policy (STDPOL)

Individual signatures are recorded in `docs/STDLIB.md`. This chapter deals only with decisions settled as stdlib design policy.

### D-STDPOL-01 The stdlib-only overloading exception
**Decision**: User-defined function overloading remains entirely prohibited, but for stdlib built-ins alone, exactly one exception is carved out where the compiler specially handles "a fixed set of overloads per type." This applies only to the signature groups explicitly named in STDLIB.md in this document (`list[int].sum`/`list[float].sum`, `print(str)`/`print(int)`/`print(float)`/`print(bool)`, `assert(cond: bool)`/`assert(cond: bool, msg: str)`, etc.). For anything else, use a per-type separate name (`abs_int`/`abs_float`, etc.) — no new built-in overloads are to be added. The one-argument form of `assert` is the form used by SPEC §13's doc-test example itself (`assert(add(1, 2) == 3)`); on failure it is a fixed specification that the message automatically shows the source text of the condition expression (this is a separate signature that differs in the number of arguments, distinct from the default-argument prohibition of D-TYPE-11).
**Rationale**: The SPEC allows no user-defined overloading whatsoever (no traits; a matching signature is a name collision, a type error), but Rust-vocabulary operations like `sum` need, in practice, to work across two numeric types. Explicitly fixing the scope prevents unbounded growth.
**SPEC reference**: §3, §6.2, §11.3, §13

### D-STDPOL-02 No implicit stringification of struct/enum is introduced
**Decision**: Implicit stringification of struct/enum (automatic Display-style conversion) is not introduced. `print`/`eprint` support only primitives (str/int/float/bool), per the D-STDPOL-01 exception. To print a struct, make it explicit, e.g. `json.encode(v) |> print`.
**Rationale**: Applies the "eliminate implicit behavior" principle to output display as well.
**SPEC reference**: §11.3

### D-STDPOL-03 Codec decode/encode are unified as assignment-target-annotation-driven
**Decision**: json/yaml/toml follow the same pattern (`decode[T](s): Result[T, Error]` / `encode[T](value: T): str`), with `T` determined by the assignment-target type annotation or by an explicit `[T]`. Specifying `T = Value` makes the same function do a dynamic decode too. Because csv restricts `T` to a flat struct only (§11.1), it is provided separately as `decode[T](s): Result[list[T], Error]` (always requiring an explicit `[T]`) and, for when `T` is unknown, `decode_rows(s): Result[list[dict[str, Value]], Error]` via `Value`.
**SPEC reference**: §11.1

### D-STDPOL-04 http's optional arguments are expressed with a struct, not default arguments
**Decision**: A simple form taking only a url is provided first, as in `http.get(url: str): Result[Response, Error] uses {net}` (internal settings such as the timeout are fixed). When headers/timeout etc. need to be specified, a separate `http.request(method: str, url: str, opts: HttpOptions): Result[Response, Error] uses {net}` is provided, where `HttpOptions` is constructed explicitly, with named arguments, as an ordinary struct (`HttpOptions(headers: {...}, timeout_ms: 5000)`).
**Rationale**: To stay consistent with D-TYPE-11 (default arguments are unsupported anywhere in the language).
**SPEC reference**: §11.2

### D-STDPOL-05 Constructing Error always spells out every field explicitly
**Decision**: No static factory method is provided for constructing `Error`. Even when there is no `cause`, it is always spelled out explicitly, as in `Error(kind: "net", message: "timeout", cause: None)`.
**Rationale**: Kept consistent with D-MUT-01 (self-less methods unsupported) and D-TYPE-11 (default arguments unsupported). Allowing implicit omission would break the consistency of "appearance = behavior," so visibility is prioritized over verbosity.
**SPEC reference**: §7.1

### D-STDPOL-06 time is expressed as epoch milliseconds (int), with no dedicated type
**Decision**: No dedicated type equivalent to `DateTime`/`Duration` is added. `time.now(): int uses {time}` returns UNIX epoch milliseconds. `format`/`parse` take a `strftime`-style format string as their second argument.
**Rationale**: Since these are not among the four stdlib types in §3.3, this expresses them without adding a new capitalized type.
**SPEC reference**: §3.3, §11.2

### D-STDPOL-07 regex passes its pattern as a str each time, with no dedicated type
**Decision**: No `Regex` type is added. The pattern is passed as a str argument every time (the implementation may cache the compiled form internally, but this is invisible at the language-spec level). An invalid pattern is returned via `Result` rather than terminating immediately (since a pattern string is expected to originate from user input, panicking is avoided).
**SPEC reference**: §3.3, §11.2

### D-STDPOL-08 The correspondence table of safe-variant APIs is kept in sync with D-ERR-04
**Decision**: A correspondence table for `xs.get(i)` / `dict.get(k)` / `math.checked_*` / `Result.unwrap_or` / `Option.unwrap_or` must always be listed in STDLIB.md, mapped one-to-one against the panic-target operations in D-ERR-04 (any type with a dangerous operation must have a safe variant alongside it).
**SPEC reference**: §7.4

### D-STDPOL-09 Type-argument constraint on `toml.encode` (document root = table)
**Decision**: `json.encode[T]`/`yaml.encode[T]` remain unconstrained on `T` (D-STDPOL-03), but `toml.encode[T]` alone is valid only when `T` is `dict[str, V]` or a struct (a type whose top level can be represented as a table). A `T` such as `list`/`set` at the top level is a type error (roughly equivalent to E1002, a compile-time error, checked once `T` is determined — either by an explicit `[T]` at the call site or from the assignment-target type).
**Rationale**: Since the TOML format specification requires the document root to be a table, applying the same unconstrained generic signature to toml as to json/yaml would let a `T` that cannot actually be output (e.g. directly encoding `list[Repo]`) slip through type checking. SPEC §15's `top |> toml.encode` (`top: list[Repo]`) is understood, under this constraint, as shorthand for wrapping it in a struct/dict first (e.g. `{"repos": top}`) before encoding (the adapted sample in SAMPLES_PLAN.md performs this explicit wrapping).
**SPEC reference**: §11.1, §15

### D-STDPOL-10 Resolution basis for relative paths passed to `fs` / `proc`
**Decision**: A relative path passed to `fs` or `proc` is resolved relative to the **process's current directory**. The directory containing the entry file is not the basis.
**Rationale**: The opening of the SPEC positions Yabumi as a scripting language "that an LLM writes as a bash/python replacement." Since bash, python, and node all resolve relative paths against the current directory, matching that convention best fits "correct on the first try." Resolving relative to the entry file would offer the benefit that "the same location is read no matter where it's invoked from," but it would violate the intuition of a user who types `ybm ./scripts/foo.ybm` from a shell (expecting it to read a file relative to their current directory) and would make it harder to compose into a pipeline.
**Impact**: Because the `fs` samples under `samples/` use relative paths into `_out/`, they **must be run with their own directory as the cwd**. The acceptance test harness (`tests/samples.rs`) copies each case into a temporary directory and launches `ybm` with that as the cwd.
**SPEC reference**: Opening (design philosophy), §11.2

---

## 17. Diagnostics & Error Code System (DIAG)

### D-DIAG-01 General rules of the code system
**Decision**: Diagnostic codes are a fixed 4 digits, `E<category><nnn>`, fixed to the following category ranges. Every category uses the `[E0000]`-style format (§1 CLI-08), **unified into a single diagnostic format** that also covers lint warnings, panics, and top-level abnormal termination (see D-ERR-05 and the D-LINT entries).

| Range | Category | Contents |
|---|---|---|
| E0000-E0499 | Lexical | Tab detection, invalid literals, unterminated strings, invalid escapes, unknown tokens |
| E0500-E0999 | Syntax | Indentation inconsistency, unexpected tokens, missing pipe `_` placeholder, trailing-comma rule violations, etc. |
| E1000-E1999 | Type system | Name collisions, type mismatches, missing annotations, un-inferable types, exhaustiveness violations, operator type-constraint violations |
| E2000-E2999 | Effect checking | Effectful calls inside a pure function, use of an undeclared effect |
| E3000-E3999 | Mutability | Mutating an immutable variable, `var self` requirement violation |
| E4000-E4999 | lint | Unused variable, unused function, shadowing, unreachable code, naming convention |
| E5000-E5999 | Module | Module-directive syntax error, top-level executable statement inside a module (prohibited) |
| E6000-E6999 | Runtime abnormal termination (panic family) | Out-of-range access, division by zero, overflow, assert failure, unwrap failure, stack overflow, top-level Err/None propagation via `?` |
| E9000-E9999 | Pre-execution CLI error | File not found, invalid extension |

**Ruling (risk: high)**: At least three different kinds of competing category assignments were proposed across four analysts (separating lexical and syntax vs. combining them; putting lint under `E4xxx` vs. a distinct `W1xxx` prefix; putting runtime aborts under `E5xxx` vs. a dedicated code-less format).
- There were two proposals for lexical/syntax — "combine them under E0xxx" and "split into E0 = lexical, E1 = syntax" — but since many individual codes already assign E1 to the type system (below), a compromise was adopted: **treat E0xxx as the shared range for lexical and syntax, internally split in two (E0000-0499 / E0500-0999)**, avoiding renumbering the already-decided type-code group.
- The proposal to give lint a `W1xxx` prefix was not adopted, since §1 shows the diagnostic format as `file:line:col [E0000] message` using **only an E prefix**, and nowhere in the SPEC does a differently-prefixed system (`W`) appear. Since lint consists of a fixed set of 5 kinds and is likewise an exit-1 target, it should be treated on par with other diagnostics, and was placed at `E4xxx`, a separate range from the type system (E1xxx).
- A module name collision is placed within the type-system range (E1xxx, specifically E1001), not the module range (E5xxx), because §10 explicitly states that "a name collision is a type error."
- Runtime abnormal termination (panic, top-level `?` failure) is folded into the diagnostic format as E6xxx, rather than a dedicated code-less format, per D-ERR-05.

### D-DIAG-02 Individual assignment of the major codes
**Decision**: The following are confirmed codes, used by reference within STDLIB.md/DECISIONS.md.

| Code | Meaning |
|---|---|
| E0001 | Tab character present |
| E0002 | Unterminated string |
| E0003 | Invalid escape sequence |
| E0004 | Invalid numeric literal (including underscore-rule violations and missing digits around the decimal point) |
| E0005 | Unknown character/token |
| E0501 | Indentation inconsistency |
| E0502 | Unexpected token |
| E0503 | Missing pipe `_` placeholder |
| E1001 | Duplicate name (collision among struct/enum/variant/function/constant/across modules) |
| E1002 | Missing type annotation on a function argument |
| E1003 | Type inference impossible (empty collection, `None`-only initialization, un-inferable type argument) |
| E1010 | Collection element types cannot be unified |
| E1011 | Disallowed dict key type |
| E1012 | Disallowed set element type |
| E1013 | Use of an unsupported operator on an unconstrained type parameter |
| E1020 | Types cannot be unified across if/match branches |
| E1021 | match exhaustiveness violation (a missing enum arm, or a missing wildcard `_` for a non-enum scrutinee per D-TYPE-18) |
| E1040 | Unused Result return value |
| E1050 | Mixed int/float arithmetic |
| E1051 | Use of `<`/`>`/`<=`/`>=` on a type without ordering comparison |
| E1060 | Result/Option mismatch for `?`, or an error-type mismatch |
| E1061 | Bare `?` inside a resolved builtin parallel lambda |
| E2001 | Effectful call inside a pure function |
| E2002 | Use of an undeclared effect (higher-order function's effect row exceeded) |
| E2003 | Unknown effect name in a `uses` declaration |
| E3001 | Mutating an immutable variable |
| E4001 | Unused variable |
| E4002 | Unused function |
| E4003 | Shadowing |
| E4004 | Unreachable code |
| E4005 | Naming-convention violation |
| E5001 | Module-directive syntax error |
| E5002 | Top-level executable statement inside a module |
| E6001 | Out-of-range access (list/tuple/str/dict) |
| E6002 | Division by zero |
| E6003 | Integer overflow (including arithmetic and float->int conversion) |
| E6004 | assert failure |
| E6005 | Top-level Err propagation via `?` |
| E6006 | Top-level None propagation via `?` |
| E6007 | `unwrap()` failure |
| E6008 | Stack overflow |
| E9001 | File not found |
| E9002 | Invalid extension |
| E9003 | Source file exists but cannot be read or decoded as UTF-8 |

**SPEC reference**: §1

---

## Revision History

### 2026-08-21: Ruling on the adversarial audit (28 findings)

Reflects the ruling on the results of an adversarial audit (28 findings).

**Adopted (27 findings, of which 2 pairs were duplicate findings consolidated into a single fix)**:
- Explicitly prohibited placing `void` in a generic type-argument position, as in `Result[void, E]`/`Option[void]` (added to D-TYPE-08), and changed the return type of `fs.write`/`fs.append`/`fs.remove` from `Result[void, Error]` to `Option[Error]` (`None` = success) (STDLIB.md §5). This resolved the direct contradiction with `void`'s existing definition of "cannot be stored or compared."
- Added `self`-form signatures for `par_map`/`par_each` to `list[T]` (STDLIB.md §2.1). Also unified D-TYPE-08's example code to `self` notation. This resolved a missing method-call syntax required by SPEC §9/§15.
- Introduced a constraint, applying only to `toml.encode[T]`, that it is valid only when `T` is dict/struct (a type whose top level can be represented as a table) (D-STDPOL-09). Made explicit the format constraint that TOML requires the document root to be a table.
- Added a one-argument form of the built-in `assert` (`assert(cond: bool): void`, which on failure automatically shows the source text of the condition expression), incorporating it into the D-STDPOL-01 overloading exception. This made valid the one-argument call used by SPEC §13's doc-test example itself (`assert(add(1, 2) == 3)`).
- Introduced a rule that implicitly wraps a `return` target expression in `Ok`/`Some` when its type does not match the function's return-value annotation `Result[T, E]`/`Option[T]` (i.e. it is the bare `T`) (D-TYPE-17). This made SPEC §5's sample (`return http.get(url)?.body`) valid.
- Documented, in D-LEX-07, that an f-string's `{expr}` may embed int/float/bool directly (automatically stringified via the built-in conversion), and the brace-depth-counting scan rule for locating the end of an interpolation in the presence of dict/set literals inside expr.
- Added tuple-destructuring patterns to the match pattern grammar (D-SYN-06), resolving the internal inconsistency with D-TYPE-06/STDLIB.md §2.4. At the same time, made explicit both the prohibition on nesting a like pattern inside an enum/tuple destructuring pattern, and the name-resolution rule for bare identifiers (a variant pattern if it matches a unit-variant name, otherwise a new binding).
- Resolved the unclear boundary between the un-inferable rule for empty list/dict/set literals (D-TYPE-15) and the three contexts of assignment-target-driven inference (D-TYPE-16), expanding D-TYPE-16's contexts to four (adding the struct/enum constructor argument position) and also documenting the recursive propagation into nested collection-literal elements.
- Introduced a block-value rule defining which value a multi-statement if/match arm returns as the block's value (D-SYN-11).
- Introduced a rule prohibiting a bare top-level `?` in each `par` branch's expression or lambda body (D-PAR-03). This resolved the conflict between the "no fail-fast — all branches run to completion" rule and `?`'s early-return rule, without complicating the implementation.
- Introduced a syntactic trigger deciding "stay single-line vs. expand to multi-line" for list/dict/set/tuple literals and function-call argument lists (D-FMT-05, a generalization of D-FMT-04).
- Documented that fmt does not format the code inside a doc comment's fence (D-FMT-06).
- Introduced a rule that match on a non-enum scrutinee (int/str) requires a trailing wildcard, and that bool is considered exhaustive with both true/false arms present (D-TYPE-18).
- Documented, in STDLIB.md §4.2, the field types csv.decode supports (int/float/bool/str only), the handling of parse failures, and the behavior on a header mismatch.
- Included comment-only lines in D-SYN-02's definition of "blank line," and made explicit that they are neither the basis for nor the target of indentation comparison.
- Documented, in D-LEX-01, that built-in namespace names (`fs`/`http`/`env`, etc.) belong to a name-resolution system separate from the flat namespace (D-TYPE-07) and do not collide with a user's top-level definition of the same name.
- SAMPLES_PLAN.md: added a sample line for the edge case where the entry file itself carries a `module` directive (`10d_entry_self_module_directive`).
- SAMPLES_PLAN.md: added, to `9_concurrency_par`'s description, verification of "no fail-fast (all elements run to completion even with mixed Err)."
- SAMPLES_PLAN.md: added a `code` field (expected diagnostic code) to the `doc_blocks` schema.
- SAMPLES_PLAN.md: added, to `6-4_strings`, verification of char-based indexing (D-COL-03) with multi-byte characters.
- SAMPLES_PLAN.md: added, to `3-2_collections`, verification that dict/set preserve insertion order (D-COL-01, including insert/remove/re-insert).

**Consolidated duplicates (2 pairs, both included on the adopted side)**:
- **F1-match-tuple-pattern and IMPL-01**: Both pointed out the same internal inconsistency (the omission of tuple destructuring from D-SYN-06's pattern enumeration), so they were consolidated and adopted as the single fix to D-SYN-06.
- **F1 and F4-missing-par_map-par_each-signatures**: Both pointed out the same omission (the missing `self`-form signatures for `par_map`/`par_each`), so they were consolidated and adopted as the single addition to STDLIB.md §2.1.

**Rejected (1 finding)**:
- **F2 (a request to add an exception to D-ERR-03)**: As a result of changing the return type of `fs.write`/`fs.append`/`fs.remove` from `Result[void, Error]` to `Option[Error]` (an adopted item above), the tail of SPEC §15, `top |> toml.encode |> fs.write("top.toml", _)`, now returns an `Option`, not a `Result`, and is passed through unchanged as legal under D-ERR-03 rule 5 (`Option[T]` is outside the scope of the unused-value check). Since the situation the finding assumed — "a `Result[void, Error]` being discarded as an expression statement" — was itself resolved by the other adopted item, it was judged that no new exception needs to be added to D-ERR-03 (this is the only rejection; the ID IMPL-06 does not appear in the audit results and is out of scope).

### Additional revisions made during the implementation phase

Decisions discovered and revised while implementation was underway, separate from the audit above. All were discovered during the implementation phase, listed in the order their decision IDs appear across chapters (§4 COL -> §14 FMT -> §15 LINT -> §16 STDPOL).

- **Clarified D-COL-02**: Documented that the handling of out-of-range subscripts / missing keys is asymmetric between reads and writes (only a dict write permits inserting a new entry). Under the old rule there was no syntactic way to add an entry to a dict.
- **Revised D-FMT-04's threshold from 2 to 3**: With a pipe-split threshold of 2 or more `|>`, fmt would split SPEC §6.3's `x |> parse? |> validate?` and SPEC §15's `top |> toml.encode |> fs.write("top.toml", _)` (both written on one line with 2 `|>`s) into 3 lines, with fmt itself breaking the appearance the SPEC body demonstrates.
- **Revised D-LINT-02**: Restricted the unused-function lint's warning target to `def`s originating from the entry file, excluding `def`s declared in a module. Under the old rule, the "share one module across multiple entries" configuration that SPEC §10 actively endorses would fail lint.
- **Introduced D-STDPOL-10**: Documented that relative paths for `fs`/`proc` are resolved against the current directory. Neither SPEC nor DECISIONS had specified this, but the implementation, samples, and test harness all implicitly depended on this assumption, so it was made explicit.
