//! The complete type definitions for runtime values (ARCHITECTURE.md §3.9). Among `Value`'s
//! variants, only the four with destructive methods (struct instance / list / dict / set)
//! are wrapped in `Arc<T>`, and mutation always goes through `Arc::make_mut`. The rest
//! (int/float/bool/str/tuple/enum/closure) are immutable once constructed and never need
//! `Arc::make_mut`.

use crate::ast::{Expr, LambdaParam};
use indexmap::{IndexMap, IndexSet};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// The sole value of type `void`. A marker holding no fields — this represents D-TYPE-08
    /// ("cannot produce any value at all") as a zero-sized variant (the
    /// VOID-VALUE-AND-BLOCK-VALUE-RULE-CONFLICT decision). Used for the value implicitly
    /// returned by a `void`-declared function, and for the evaluation result of
    /// `Return(None)`.
    Void,
    /// str is immutable (SPEC §3.1), so `Arc<str>` suffices — there is no need for the
    /// double indirection of something like `Arc<String>` (since the language has no
    /// growing or in-place mutation operations at all).
    Str(Arc<str>),

    /// Held as `Arc<Vec<Value>>` because it has push/pop/insert/remove/extend/clear
    /// (the destructive section of STDLIB.md §2.1). Mutating operations always go through
    /// `Arc::make_mut`.
    List(Arc<Vec<Value>>),
    /// K is MapKey (below). indexmap satisfies D-COL-01 (insertion order preserved).
    Dict(Arc<IndexMap<MapKey, Value>>),
    Set(Arc<IndexSet<MapKey>>),
    /// A tuple is immutable once constructed (neither SPEC nor STDLIB has any operation
    /// that rewrites a tuple element). Since it is fixed-length, it is represented as a
    /// boxed slice rather than a Vec.
    Tuple(Arc<[Value]>),

    /// The only user-defined value that can be the target of a destructive `var self`
    /// method call or field assignment.
    Struct(Arc<StructInstance>),
    /// enums (user-defined, and Result/Option/Value themselves) are immutable once
    /// constructed — because every Result/Option method in STDLIB.md takes a plain `self`
    /// (not var) and no destructive method exists. Never needs `Arc::make_mut`.
    Enum(Arc<EnumInstance>),

    /// A lambda, or a top-level function referenced as a value (e.g.
    /// `xs.par_map(fetch_repos)`).
    Closure(Arc<Closure>),
}

impl PartialEq for Value {
    /// Implemented by hand (not derived) — deriving `PartialEq` would force it onto
    /// `Closure`/`CallTarget`/`LambdaBody` (i.e. the AST itself) as well, which would in
    /// turn force every AST node such as `Expr`/`Stmt` to derive it too, dragging in
    /// unwanted requirements such as making even the comment strings fmt preserves part of
    /// the comparison (the R7 decision, §8). All variants are compared recursively (D-OP-06:
    /// `==`/`!=` is structural equality across every type).
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a == b,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Void, Self::Void) => true,
            (Self::Str(a), Self::Str(b)) => a == b,
            (Self::List(a), Self::List(b)) => a == b,
            (Self::Dict(a), Self::Dict(b)) => a == b,
            (Self::Set(a), Self::Set(b)) => a == b,
            (Self::Tuple(a), Self::Tuple(b)) => a == b,
            (Self::Struct(a), Self::Struct(b)) => a == b,
            (Self::Enum(a), Self::Enum(b)) => a == b,
            // Everything left is either (a) comparing two Value::Closures, or (b) a variant
            // mismatch. (a) always returns false (not even comparing a closure with itself
            // yields true — neither structural AST comparison nor pointer comparison is
            // performed). Branch (a) is theoretically reachable: D-FUNC-05 always permits
            // `==`/`!=` even on an unconstrained type parameter T, and nothing prevents T
            // from unifying with a function type, so code that compares two values of type
            // T with `==` inside a generic function may, depending on the call site, end up
            // comparing two Closures (making `unreachable!()` unsound here). This encodes
            // the design decision that "a closure has no meaningful value-comparable
            // identity" in the simplest possible form, fixed to false (the R7 decision,
            // §8). (b) is naturally false too.
            _ => false,
        }
    }
}

/// The values permitted as K in dict[K,V] or T in set[T] (D-TYPE-05: int/str/bool, plus a
/// tuple whose every element is itself an allowed key type). Rather than implementing
/// Eq+Hash on `Value` as a whole, "a value that can be a key" is carved out as its own small
/// dedicated type, so that the very state of "a float or a list becomes a dict key" cannot
/// even be constructed as a Rust type. `Value` itself cannot implement Eq/Hash because it
/// contains f64 (NaN comparison is non-reflexive), but `MapKey` holds no f64, so it can
/// straightforwardly derive Eq+Hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Int(i64),
    Bool(bool),
    Str(Arc<str>),
    Tuple(Arc<[MapKey]>),
}

impl MapKey {
    /// For generating the return value of `dict.keys()` etc.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Int(n) => Value::Int(*n),
            Self::Bool(b) => Value::Bool(*b),
            Self::Str(s) => Value::Str(Arc::clone(s)),
            Self::Tuple(items) => Value::Tuple(
                items
                    .iter()
                    .map(MapKey::to_value)
                    .collect::<Vec<_>>()
                    .into(),
            ),
        }
    }

    /// The entry point for `dict[k]`/`set.insert(x)` etc. A `Value` that does not satisfy
    /// D-TYPE-05's constraints yields `None` (this should already have been excluded by
    /// static checking, so reaching this at runtime would be a bug).
    #[must_use]
    pub fn try_from_value(v: &Value) -> Option<MapKey> {
        match v {
            Value::Int(n) => Some(Self::Int(*n)),
            Value::Bool(b) => Some(Self::Bool(*b)),
            Value::Str(s) => Some(Self::Str(Arc::clone(s))),
            Value::Tuple(items) => {
                let keys: Option<Vec<MapKey>> = items.iter().map(MapKey::try_from_value).collect();
                keys.map(|k| Self::Tuple(k.into()))
            }
            _ => None,
        }
    }
}

/// Fields are held as a Vec in declaration order (D-TYPE-13: construction requires named
/// arguments, but at runtime, representing by index means `.field` access doesn't need a
/// string comparison every time — the index is already recorded in `Resolutions` by the
/// type-checking phase, §3.7).
#[derive(Debug, Clone, PartialEq)]
pub struct StructInstance {
    pub type_name: Arc<str>,
    pub fields: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumInstance {
    /// "Result" / "Option" / "Value" / a user-defined name.
    pub type_name: Arc<str>,
    /// Declaration order (used both for match exhaustiveness checking and fmt
    /// reconstruction).
    pub variant_index: u32,
    /// For diagnostic messages.
    pub variant_name: Arc<str>,
    /// Positional (D-SYN-07). Empty for a unit variant.
    pub fields: Vec<Value>,
}

/// The callable body itself that `CallTarget::Lambda` points to. This type simply bundles
/// together the two fields of `ExprKind::Lambda {params, body}` (ast/expr.rs, §3.4) as-is;
/// it is not an independent AST node (it has no NodeId/Span — it is merely immutable data
/// shared as a value by multiple Closures via `Arc<LambdaBody>`).
pub struct LambdaBody {
    pub params: Vec<LambdaParam>,
    pub body: Expr,
}

impl std::fmt::Debug for LambdaBody {
    // There is no value in recursively Debug-printing the entire AST node (Expr), and the
    // cost of propagating a Debug derive across the whole AST would be greater, so this is
    // written by hand as an opaque display (showing only the parameter count).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LambdaBody({} params)", self.params.len())
    }
}

pub struct Closure {
    pub target: CallTarget,
    /// Non-empty only for a lambda. A capture is always a value copy (D-MUT-04) — even if
    /// the closure is rewritten somewhere, the value already sitting in this array is
    /// unaffected (`Value::clone()` merely bumps an Arc's reference count; even if
    /// `Arc::make_mut` later runs on the original variable's side, the clone this closure
    /// holds remains independent, end of §3.9).
    pub captured: Vec<(Arc<str>, Value)>,
}

impl std::fmt::Debug for Closure {
    // There is little value in recursively printing captured's values, so they are
    // intentionally omitted via finish_non_exhaustive (the standard response to
    // clippy::missing_fields_in_debug).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Closure")
            .field("target", &self.target)
            .field("captured_len", &self.captured.len())
            .finish_non_exhaustive()
    }
}

pub enum CallTarget {
    /// The body of a lambda expression (parameter names + expression), shared across
    /// multiple calls since it comes from the AST.
    Lambda(Arc<LambdaBody>),
    /// A top-level function name (resolved via `Program.functions`).
    Function(Arc<str>),
    /// For when a stdlib function is passed as a value (e.g. `x |> json.encode`).
    Builtin(crate::stdlib::BuiltinFnId),
}

impl std::fmt::Debug for CallTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lambda(body) => write!(f, "CallTarget::Lambda({body:?})"),
            Self::Function(name) => write!(f, "CallTarget::Function({name:?})"),
            Self::Builtin(id) => write!(f, "CallTarget::Builtin({id:?})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MapKey, Value};
    use std::sync::Arc;

    #[test]
    fn value_equality_is_structural() {
        assert_eq!(Value::Int(1), Value::Int(1));
        assert_ne!(Value::Int(1), Value::Int(2));
        assert_eq!(
            Value::Str(std::sync::Arc::from("a")),
            Value::Str(std::sync::Arc::from("a"))
        );
        assert_ne!(Value::Int(1), Value::Bool(true));
    }

    #[test]
    fn map_key_round_trips_through_value() {
        for key in [
            MapKey::Int(42),
            MapKey::Bool(true),
            MapKey::Str(Arc::from("hello")),
            MapKey::Tuple(Arc::from(vec![MapKey::Int(1), MapKey::Str(Arc::from("x"))])),
        ] {
            let v = key.to_value();
            assert_eq!(MapKey::try_from_value(&v), Some(key));
        }
    }

    #[test]
    fn map_key_rejects_disallowed_values() {
        assert_eq!(MapKey::try_from_value(&Value::Float(1.0)), None);
        assert_eq!(MapKey::try_from_value(&Value::List(Arc::new(vec![]))), None);
        assert_eq!(
            MapKey::try_from_value(&Value::Tuple(Arc::from(vec![Value::Float(1.0)]))),
            None
        );
    }
}
