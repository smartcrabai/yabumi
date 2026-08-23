//! `Interpreter`, the sequential top-level statement execution entry point
//! (ARCHITECTURE.md §5.6/§5.7).
//!
//! `?` needs two kinds of non-local control that unwind different scopes: "early return at
//! a function boundary" and "immediate exit 1 at the top level." On top of that, a panic
//! such as an out-of-range access needs to unwind the entire process, ignoring even
//! function boundaries (D-ERR-06). To implement these three within a single evaluator, this
//! makes use of Rust's own `?` operator and its error-type hierarchy (§5.6).
//!
//! `Program` is wrapped in an `Arc` immediately before the evaluation phase begins (see the
//! `Program` documentation in `env.rs`) — because `par`
//! (concurrency::eval_par_list/eval_par_map) needs to share it with worker threads via
//! `Arc::clone`, the whole evaluator threads `program` consistently as `&Arc<Program>` from
//! `run_top_level` on down (a judgment call made in this file — the pseudocode in the body
//! of ARCHITECTURE.md §3.11 writes `&Program`, but since duplicating `Arc<Program>` is
//! mandatory for spawning `par`'s worker threads, using the same `Arc` throughout the whole
//! of Eval is the only consistent design, and this is a variation within the tolerance that
//! the opening of ARCHITECTURE.md allows — "the meaning and behavior of the fields are not
//! changed").

pub mod call;
pub mod env;
pub mod expr;
pub mod lvalue;
pub mod ops;
pub mod panic;
pub mod stmt;
pub mod value;

pub use panic::Abort;

use crate::ast::Item;
use crate::diagnostics::Span;
use env::{Environment, Program};
use std::cell::Cell;
use std::sync::Arc;
use value::Value;

/// The result of evaluating an expression/statement. Unifies the two kinds of non-local
/// control into a single type.
pub enum Flow {
    Value(Value),
    /// The early-return signal from `return expr` or `expr?`. Caught and converted only at
    /// a function-call boundary (`call::call_function`). If it propagates all the way to
    /// where no calling frame exists (the top level), that structurally confirms it is
    /// Err/None propagation from a top-level `?`.
    Return(Value),
}

pub type EvalResult = Result<Flow, Abort>;

/// A macro that evaluates a subexpression and extracts its value. If the inner `?`
/// (D-ERR-01) has already done an early return, it relays the `Flow::Return` on as-is as
/// the return value of the enclosing function (assuming the calling function's return type
/// is always `EvalResult`). Rust's own `?` is used to propagate `Abort` (§5.6). Shared by
/// `eval/expr.rs`, `eval/stmt.rs`, and `eval/call.rs` — since hand-writing the same relay
/// pattern every time would repeat the same `match` once per expression, it is consolidated
/// into a single macro (a judgment call made in this file).
macro_rules! eval_val {
    ($e:expr, $env:expr, $program:expr) => {
        match $crate::eval::expr::eval_expr($e, $env, $program)? {
            $crate::eval::Flow::Value(v) => v,
            flow @ $crate::eval::Flow::Return(_) => return Ok(flow),
        }
    };
}
pub(crate) use eval_val;

/// The evaluator's own recursion-depth ceiling (§5.7). This relies not at all on Rust's
/// native call stack — combined with running on a thread with a dedicated stack size, it
/// absorbs the difference in default stack size across OSes (cross-platform consistency).
///
/// The pseudocode in the body of ARCHITECTURE.md gives 8,000 as an example value, but when
/// this implementation (the multi-level Rust function call chain per Yabumi call of
/// `eval_call` → `call_function` → `run_body_with_env` → `eval_block` → `eval_stmt` →
/// `eval_expr` → …) was measured on a debug build (the unoptimized profile `cargo test`
/// uses, with the largest frame size) with a dedicated 64MiB stack, it was confirmed that at
/// 8,000, Rust's own native stack was exhausted and the process aborted with SIGABRT before
/// `entry_unbounded_recursion.ybm` (the E6008 sample) ever reached the guard (since this
/// very guard exists precisely to guarantee "no reliance at all on Rust's native stack," the
/// threshold needs to sit below the point of an actual crash). 3,000 is a value confirmed to
/// reach a proper E6008 Abort stably across 5 consecutive runs under the same conditions,
/// and was chosen as a judgment call made in this file, favoring a safety margin from 8,000
/// (reported as needing follow-up — since frame size can grow or shrink with future
/// implementation changes, keeping this value is the safer choice with an eye toward future
/// optimizations and margin in release builds).
const MAX_CALL_DEPTH: u32 = 3_000;

thread_local! {
    // The R9 decision (§8): make this thread-local. Adding a `depth` argument to
    // `call_function`'s signature would ripple out to every call path in the evaluator and
    // become unwieldy. More fundamentally, the depth is a value that approximates "how much
    // of a single OS thread's actual call stack has been consumed" — each of `par`'s worker
    // threads has its own completely independent, fresh 64MiB stack, and one branch's deep
    // recursion must not affect the depth of another branch or of the thread that spawned
    // the worker threads.
    static CALL_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct DepthGuard(());

impl Drop for DepthGuard {
    fn drop(&mut self) {
        CALL_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

fn enter_call(span: Span) -> Result<DepthGuard, Abort> {
    let n = CALL_DEPTH.with(|d| {
        let n = d.get() + 1;
        d.set(n);
        n
    });
    if n > MAX_CALL_DEPTH {
        CALL_DEPTH.with(|depth| depth.set(depth.get() - 1));
        return Err(panic::stack_overflow(span));
    }
    Ok(DepthGuard(()))
}

/// Sequential execution of top-level statements. Since no calling frame exists, receiving a
/// `Flow::Return` here confirms Err/None propagation from a top-level `?` (SPEC §7.2,
/// E6005/E6006).
pub fn run_top_level(
    items: &[Item],
    env: &mut Environment,
    program: &Arc<Program>,
) -> Result<(), Abort> {
    for item in items {
        let Item::Stmt(stmt) = item else { continue };
        match stmt::eval_stmt(stmt, env, program)? {
            Flow::Value(_) => {}
            Flow::Return(payload) => {
                return Err(toplevel_propagation_abort(stmt.span, &payload));
            }
        }
    }
    Ok(())
}

/// If `Flow::Return` propagates all the way to the top level, it is always the payload of
/// the `Result::Err` or `Option::None` that `?` was targeting (D-ERR-01: the scope with no
/// function boundary is the top level itself). E6005/E6006 are distinguished from the
/// variant name of `Value::Enum`.
fn toplevel_propagation_abort(span: Span, payload: &Value) -> Abort {
    let Value::Enum(inst) = payload else {
        unreachable!(
            "already type-checked, so a top-level `?` payload is always a Result/Option Enum"
        )
    };
    match inst.variant_name.as_ref() {
        "Err" => {
            let message = call::error_message_of(&inst.fields[0]);
            panic::toplevel_err_propagation(span, &message)
        }
        "None" => panic::toplevel_none_propagation(span),
        _ => unreachable!(
            "a Flow::Return payload is only ever produced by a `?` short-circuit, so it is either Err or None"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Stmt;
    use crate::diagnostics::{DiagnosticBag, ErrorCode, SourceMap};
    use crate::lexer::Lexer;
    use crate::module_resolve::build_program_skeleton;
    use crate::parser::parse_module;
    use crate::types::check_decl::check_program;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Runs lex→parse→module_resolve→type check to build a `Program` and the entry's list
    /// of top-level `Item::Stmt`s. Since `stdlib::prelude::install` (Units 12-14) is not yet
    /// implemented, this is used only for source that contains no stdlib calls at all (as
    /// instructed by the task). If a diagnostic is emitted at any phase, the test itself is
    /// failed via `panic!` (`.unwrap()`/`.expect()` are not used since clippy denies them).
    fn parse_and_check(src: &str) -> (Arc<Program>, Vec<Item>) {
        let mut sources = SourceMap::new();
        let file = sources.add(PathBuf::from("entry_main.ybm"), src.to_owned());
        let (tokens, _comments, lex_diags) = Lexer::new(src, file).tokenize();
        assert!(
            !lex_diags.has_any(),
            "lex errors: {:?}",
            lex_diags.into_sorted(&sources)
        );
        let (mut module, parse_diags) = parse_module(&tokens, file);
        assert!(
            !parse_diags.has_any(),
            "parse errors: {:?}",
            parse_diags.into_sorted(&sources)
        );

        // build_program_skeleton registers no Item::Stmt and discards them (see
        // module_resolve/mod.rs), so the entry's executable statements are set aside first.
        let all_items = std::mem::take(&mut module.items);
        let mut entry_stmts: Vec<Stmt> = Vec::new();
        let mut decl_items = Vec::new();
        for item in all_items {
            match item {
                Item::Stmt(s) => entry_stmts.push(s),
                decl @ Item::Decl(_) => decl_items.push(decl),
            }
        }
        module.items = decl_items;

        let sources = Arc::new(sources);
        let mut resolve_diags = DiagnosticBag::new();
        let mut program =
            build_program_skeleton(vec![module], Arc::clone(&sources), &mut resolve_diags);
        assert!(
            !resolve_diags.has_any(),
            "module resolve errors: {:?}",
            resolve_diags.into_sorted(&sources)
        );

        let mut type_diags = DiagnosticBag::new();
        check_program(&mut program, &entry_stmts, &mut type_diags);
        assert!(
            !type_diags.has_any(),
            "type errors: {:?}",
            type_diags.into_sorted(&sources)
        );

        let items: Vec<Item> = entry_stmts.into_iter().map(Item::Stmt).collect();
        (Arc::new(program), items)
    }

    /// A run helper for extracting and verifying values from the `Environment` after
    /// top-level execution (since `assert`/`print` are not yet implemented in Unit 14, this
    /// avoids them and compares values directly on the Rust side).
    fn run_source(src: &str) -> Result<Environment, Abort> {
        let (program, items) = parse_and_check(src);
        let mut env = Environment::with_frame(HashMap::new());
        run_top_level(&items, &mut env, &program)?;
        Ok(env)
    }

    /// Deep recursion (E6008 verification) may exhaust Rust's native stack before reaching
    /// the evaluator's own guard on the default test-thread stack, so this runs on a
    /// dedicated thread with the same 64MiB stack as ARCHITECTURE.md §5.7.
    fn run_source_big_stack(src: &str) -> Result<(), Abort> {
        let (program, items) = parse_and_check(src);
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    let mut env = Environment::with_frame(HashMap::new());
                    run_top_level(&items, &mut env, &program)
                });
            let Ok(handle) = handle else {
                panic!("thread creation failed")
            };
            let Ok(result) = handle.join() else {
                panic!("evaluation thread panicked")
            };
            result
        })
    }

    fn read_sample(rel_path: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_path);
        match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => panic!("sample file read failed: {path:?}: {e}"),
        }
    }

    fn assert_abort_code(result: Result<(), Abort>, expected: ErrorCode) {
        match result {
            Ok(()) => panic!("expected Abort({expected:?}) but evaluation succeeded"),
            Err(Abort(diag)) => assert_eq!(diag.code, expected),
        }
    }

    // ---- D-MUT-01 through 04: mutability / value semantics ----

    #[test]
    fn var_reassignment_and_arithmetic() {
        let src = "x = 1 + 2 * 3\nvar y = 5\ny = y + 1\n";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(env.try_lookup("x"), Some(&Value::Int(7)));
        assert_eq!(env.try_lookup("y"), Some(&Value::Int(6)));
    }

    #[test]
    fn struct_field_assign_and_var_self_method() {
        let src = "\
struct Counter
    value: int

    def increment(var self): void
        self.value = self.value + 1

struct User
    name: str
    age: int

var u = User(name: \"alice\", age: 3)
u.age = 4

var c = Counter(value: 1)
c.increment()
c.increment()
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        let Some(Value::Struct(u)) = env.try_lookup("u") else {
            panic!("u is not a struct")
        };
        assert_eq!(u.fields[1], Value::Int(4));
        let Some(Value::Struct(c)) = env.try_lookup("c") else {
            panic!("c is not a struct")
        };
        assert_eq!(c.fields[0], Value::Int(3));
    }

    #[test]
    fn function_argument_struct_copy_does_not_propagate_mutation() {
        // D-MUT-04: a struct passed as a function argument is a value copy, so calling a
        // var self method on it in the callee does not propagate back to the caller.
        let src = "\
struct Counter
    value: int

    def increment(var self): void
        self.value = self.value + 1

def try_mutate(c: Counter): void
    var local = c
    local.increment()

var counter = Counter(value: 1)
try_mutate(counter)
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        let Some(Value::Struct(counter)) = env.try_lookup("counter") else {
            panic!("counter is not a struct")
        };
        assert_eq!(counter.fields[0], Value::Int(1));
    }

    #[test]
    fn list_and_dict_index_assignment_and_read() {
        let src = "\
var ys: list[int] = [10, 20, 30]
ys[1] = 99

var scores: dict[str, int] = {\"alice\": 1}
scores[\"alice\"] = 2
scores[\"bob\"] = 5
looked_up = scores[\"bob\"]
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(
            env.try_lookup("ys"),
            Some(&Value::List(Arc::new(vec![
                Value::Int(10),
                Value::Int(99),
                Value::Int(30)
            ])))
        );
        assert_eq!(env.try_lookup("looked_up"), Some(&Value::Int(5)));
    }

    #[test]
    fn lambda_capture_is_a_value_copy() {
        // D-MUT-04: a lambda's capture is a value copy. Rewriting an outer var variable
        // after capture has no effect on the lambda's behavior when it's called (verified
        // here in the simple form of merely reading the capture inside the lambda — to
        // avoid stdlib calls like push).
        let src = "\
var n = 10
make_adder = () => n + 1
n = 999
result = make_adder()
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(env.try_lookup("result"), Some(&Value::Int(11)));
        assert_eq!(env.try_lookup("n"), Some(&Value::Int(999)));
    }

    // ---- D-SYN-11 / §6.1: expression-oriented if/match, block-value rule for multi-statement arms ----

    #[test]
    fn if_expression_and_nested_else_if() {
        let src = "\
score = 85
label = if score > 80
    \"high\"
else
    \"low\"

grade = if score >= 90
    \"A\"
else
    if score >= 80
        \"B\"
    else
        \"C\"
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(
            env.try_lookup("label"),
            Some(&Value::Str(Arc::from("high")))
        );
        assert_eq!(env.try_lookup("grade"), Some(&Value::Str(Arc::from("B"))));
    }

    #[test]
    fn multi_statement_block_value_rule() {
        let src = "\
n = 7
category = if n % 2 == 0
    var half = n / 2
    doubled_half = half * 2
    doubled_half
else
    var cube = n * n * n
    adjusted = cube - 1
    adjusted
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(env.try_lookup("category"), Some(&Value::Int(342)));
    }

    // ---- D-SYN-07/D-TYPE-09/§6.1: enum construction, match destructuring, exhaustiveness ----

    #[test]
    #[expect(
        clippy::approx_constant,
        reason = "the Circle area calculation in samples/ok/3-5_struct_and_enum uses, by \
                  spec, the simplified constant 3.14 (not std::f64::consts::PI) — the \
                  expected value must likewise be 3.14, matching the source"
    )]
    fn enum_variant_construction_and_match() {
        let src = "\
enum Shape
    Circle(float)
    Rect(float, float)
    Point

def area(s: Shape): float
    return match s
        Circle(r) => 3.14 * r * r
        Rect(w, h) => w * h
        Point => 0.0

c: Shape = Circle(1.0)
rect_shape: Shape = Rect(3.0, 4.0)
p: Shape = Point

area_c = area(c)
area_r = area(rect_shape)
area_p = area(p)
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(env.try_lookup("area_c"), Some(&Value::Float(3.14)));
        assert_eq!(env.try_lookup("area_r"), Some(&Value::Float(12.0)));
        assert_eq!(env.try_lookup("area_p"), Some(&Value::Float(0.0)));
    }

    #[test]
    fn non_enum_match_with_wildcard_and_bool() {
        let src = "\
score = 72
grade = match score
    100 => \"perfect\"
    0 => \"zero\"
    _ => \"normal\"

flag = true
description = match flag
    true => \"on\"
    false => \"off\"
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(
            env.try_lookup("grade"),
            Some(&Value::Str(Arc::from("normal")))
        );
        assert_eq!(
            env.try_lookup("description"),
            Some(&Value::Str(Arc::from("on")))
        );
    }

    // ---- D-SYN-08: hoisting of top-level declarations, mutual recursion ----

    #[test]
    fn function_hoisting_allows_forward_reference() {
        let src = "\
a = double(3)

def double(n: int): int
    return n * 2
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(env.try_lookup("a"), Some(&Value::Int(6)));
    }

    #[test]
    fn mutual_recursion_works_regardless_of_declaration_order() {
        let src = "\
def is_even(n: int): bool
    return if n == 0
        true
    else
        is_odd(n - 1)

def is_odd(n: int): bool
    return if n == 0
        false
    else
        is_even(n - 1)

r1 = is_even(10)
r2 = is_odd(7)
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(env.try_lookup("r1"), Some(&Value::Bool(true)));
        assert_eq!(env.try_lookup("r2"), Some(&Value::Bool(true)));
    }

    // ---- D-TYPE-17: implicit Ok/Some wrap of a return-target expression ----

    #[test]
    fn implicit_ok_wrap_and_explicit_err() {
        let src = "\
def safe_half(n: int): Result[int, Error]
    if n % 2 != 0
        return Err(Error(kind: \"decode\", message: \"n is not even\", cause: None))
    else
        return n / 2

ok_case = safe_half(10)
err_case = safe_half(3)
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        let Some(Value::Enum(ok_case)) = env.try_lookup("ok_case") else {
            panic!("ok_case is not an enum")
        };
        assert_eq!(ok_case.variant_name.as_ref(), "Ok");
        assert_eq!(ok_case.fields[0], Value::Int(5));

        let Some(Value::Enum(err_case)) = env.try_lookup("err_case") else {
            panic!("err_case is not an enum")
        };
        assert_eq!(err_case.variant_name.as_ref(), "Err");
    }

    #[test]
    fn implicit_some_wrap_and_explicit_none() {
        // To verify D-TYPE-17 without relying on `float(n)` (stdlib::primitives, not yet
        // implemented in Unit 12), this verifies the implicit Some wrap / explicit None
        // using only plain float arithmetic (per the task instructions: run verification
        // only against samples that contain no stdlib calls).
        let src = "\
def safe_reciprocal(n: float): Option[float]
    if n == 0.0
        return None
    else
        return 1.0 / n

zero_case = safe_reciprocal(0.0)
normal_case = safe_reciprocal(4.0)
";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        let Some(Value::Enum(zero_case)) = env.try_lookup("zero_case") else {
            panic!("zero_case is not an enum")
        };
        assert_eq!(zero_case.variant_name.as_ref(), "None");

        let Some(Value::Enum(normal_case)) = env.try_lookup("normal_case") else {
            panic!("normal_case is not an enum")
        };
        assert_eq!(normal_case.variant_name.as_ref(), "Some");
        assert_eq!(normal_case.fields[0], Value::Float(0.25));
    }

    // ---- top-level `?` propagation (E6005/E6006) ----

    #[test]
    fn toplevel_question_err_propagation_is_e6005() {
        let src = "\
def fail(): Result[int, Error]
    return Err(Error(kind: \"decode\", message: \"boom\", cause: None))

def run(): Result[int, Error]
    x = fail()?
    return x

y = run()?
";
        let env = run_source(src);
        match env {
            Ok(_) => panic!("expected top-level Err propagation to abort"),
            Err(Abort(diag)) => assert_eq!(diag.code, ErrorCode::TopLevelErrPropagation),
        }
    }

    // ---- D-PAR-01/02: par's result-order guarantee, nesting ----

    #[test]
    fn par_list_preserves_input_order() {
        let src = "results = par [1 + 1, 2 + 2, 3 + 3]\n";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(
            env.try_lookup("results"),
            Some(&Value::List(Arc::new(vec![
                Value::Int(2),
                Value::Int(4),
                Value::Int(6)
            ])))
        );
    }

    #[test]
    fn par_tuple_and_nested_par() {
        let src = "outer = par (par [1, 2][0], 10 + 10)\n";
        let env = match run_source(src) {
            Ok(e) => e,
            Err(a) => panic!("unexpected abort: {a:?}"),
        };
        assert_eq!(
            env.try_lookup("outer"),
            Some(&Value::Tuple(Arc::from(vec![
                Value::Int(1),
                Value::Int(20)
            ])))
        );
    }

    // ---- samples/err/runtime: operations that terminate as a panic (D-ERR-04), using real files ----

    #[test]
    fn sample_e6001_list_index_out_of_range() {
        let src =
            read_sample("samples/err/runtime/e6001_out_of_range_access/entry_list_index_oob.ybm");
        let result = run_source(&src).map(|_| ());
        assert_abort_code(result, ErrorCode::IndexOutOfRange);
    }

    #[test]
    fn sample_e6001_dict_missing_key() {
        let src =
            read_sample("samples/err/runtime/e6001_out_of_range_access/entry_dict_missing_key.ybm");
        let result = run_source(&src).map(|_| ());
        assert_abort_code(result, ErrorCode::IndexOutOfRange);
    }

    #[test]
    fn sample_e6002_int_div_by_zero() {
        let src = read_sample("samples/err/runtime/e6002_zero_division/entry_int_div_by_zero.ybm");
        let result = run_source(&src).map(|_| ());
        assert_abort_code(result, ErrorCode::DivisionByZero);
    }

    #[test]
    fn sample_e6002_int_mod_by_zero() {
        let src = read_sample("samples/err/runtime/e6002_zero_division/entry_int_mod_by_zero.ybm");
        let result = run_source(&src).map(|_| ());
        assert_abort_code(result, ErrorCode::DivisionByZero);
    }

    #[test]
    fn sample_e6003_arithmetic_overflow() {
        let src =
            read_sample("samples/err/runtime/e6003_integer_overflow/entry_arithmetic_overflow.ybm");
        let result = run_source(&src).map(|_| ());
        assert_abort_code(result, ErrorCode::IntegerOverflow);
    }

    #[test]
    fn sample_e6008_unbounded_recursion_stack_overflow() {
        let src =
            read_sample("samples/err/runtime/e6008_stack_overflow/entry_unbounded_recursion.ybm");
        let result = run_source_big_stack(&src);
        assert_abort_code(result, ErrorCode::StackOverflow);
    }
}
