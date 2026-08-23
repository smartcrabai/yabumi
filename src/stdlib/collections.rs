//! Methods on list[T]/dict[K,V]/set[T]/tuple (STDLIB.md §2; destructive methods go through
//! eval/lvalue.rs, ARCHITECTURE.md §2.1).
//!
//! The higher-order methods (map/filter/fold/find/any/all/flat_map/sort_by/par_map/par_each/
//! each) take `program` and apply the given `Closure` to each element via
//! `eval::call::call_closure` -- unlike the non-higher-order pure methods, their return type is
//! uniformly `Result<Value, Abort>` so that a panic that may occur during the call can be
//! propagated as-is via `?` as an `Abort` (this is also the runtime counterpart of the effect
//! forwarding that the EFFECT-HOF-POLYMORPHISM decision requires, §5.5/§8).
//!
//! `program`'s type is `&Arc<Program>` (changed in this Unit from the skeleton's `&Program`,
//! an acceptable local signature adjustment outside Cargo.toml -- since eval as a whole
//! consistently passes `Arc<Program>` around per ARCHITECTURE.md §3.11, calling
//! `call::call_closure`/`call_function` requires `&Arc<Program>`. The caller `eval/call.rs`
//! already holds `program: &Arc<Program>`, and since `&Arc<Program>` is more specific than
//! `&Program` it can be passed through as-is, so no change is needed at existing call sites).

use crate::diagnostics::Span;
use crate::eval::call::call_closure;
use crate::eval::env::Program;
use crate::eval::value::{Closure, MapKey, Value};
use crate::eval::{Abort, panic};
use crate::stdlib::{none_value, some_value};
use indexmap::{IndexMap, IndexSet};
use std::cmp::Ordering;
use std::sync::Arc;

// --- 2.1 list[T]: non-destructive ---

pub fn list_map(self_: &[Value], f: &Closure, program: &Arc<Program>) -> Result<Value, Abort> {
    let mut out = Vec::with_capacity(self_.len());
    for v in self_ {
        out.push(call_closure(f, vec![v.clone()], program)?);
    }
    Ok(Value::List(Arc::new(out)))
}

pub fn list_filter(self_: &[Value], f: &Closure, program: &Arc<Program>) -> Result<Value, Abort> {
    let mut out = Vec::new();
    for v in self_ {
        let keep = call_closure(f, vec![v.clone()], program)?;
        let Value::Bool(b) = keep else {
            unreachable!("type-checked already, so filter's f always returns bool")
        };
        if b {
            out.push(v.clone());
        }
    }
    Ok(Value::List(Arc::new(out)))
}

pub fn list_fold(
    self_: &[Value],
    init: Value,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let mut acc = init;
    for v in self_ {
        acc = call_closure(f, vec![acc, v.clone()], program)?;
    }
    Ok(acc)
}

pub fn list_find(self_: &[Value], f: &Closure, program: &Arc<Program>) -> Result<Value, Abort> {
    for v in self_ {
        let matched = call_closure(f, vec![v.clone()], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so find's f always returns bool")
        };
        if b {
            return Ok(some_value(v.clone()));
        }
    }
    Ok(none_value())
}

pub fn list_any(self_: &[Value], f: &Closure, program: &Arc<Program>) -> Result<Value, Abort> {
    for v in self_ {
        let matched = call_closure(f, vec![v.clone()], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so any's f always returns bool")
        };
        if b {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

pub fn list_all(self_: &[Value], f: &Closure, program: &Arc<Program>) -> Result<Value, Abort> {
    for v in self_ {
        let matched = call_closure(f, vec![v.clone()], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so all's f always returns bool")
        };
        if !b {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn sum_values(
    items: impl Iterator<Item = Value>,
    empty_is_float: bool,
    span: crate::diagnostics::Span,
) -> Result<Value, Abort> {
    let mut int_total = 0i64;
    let mut float_total = 0.0f64;
    let mut saw_float = false;
    let mut saw_value = false;
    for value in items {
        saw_value = true;
        match value {
            Value::Int(number) => {
                int_total = int_total
                    .checked_add(number)
                    .ok_or_else(|| crate::eval::panic::overflow(span))?;
            }
            Value::Float(number) => {
                saw_float = true;
                float_total += number;
            }
            _ => unreachable!("sum elements were already checked as uniformly numeric"),
        }
    }
    if saw_float || !saw_value && empty_is_float {
        Ok(Value::Float(float_total))
    } else {
        Ok(Value::Int(int_total))
    }
}

pub fn list_sum(
    values: &[Value],
    empty_is_float: bool,
    span: crate::diagnostics::Span,
) -> Result<Value, Abort> {
    sum_values(values.iter().cloned(), empty_is_float, span)
}

#[must_use]
pub fn list_enumerate(self_: &[Value]) -> Value {
    let items = self_
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let idx = i64::try_from(i).unwrap_or(i64::MAX);
            Value::Tuple(Arc::from(vec![Value::Int(idx), v.clone()]))
        })
        .collect();
    Value::List(Arc::new(items))
}

#[must_use]
pub fn list_zip(self_: &[Value], other: &[Value]) -> Value {
    let items = self_
        .iter()
        .zip(other.iter())
        .map(|(a, b)| Value::Tuple(Arc::from(vec![a.clone(), b.clone()])))
        .collect();
    Value::List(Arc::new(items))
}

#[must_use]
pub fn list_rev(self_: &[Value]) -> Value {
    Value::List(Arc::new(self_.iter().rev().cloned().collect()))
}

#[must_use]
pub fn list_take(self_: &[Value], n: i64) -> Value {
    let n = usize::try_from(n).unwrap_or(0);
    Value::List(Arc::new(self_.iter().take(n).cloned().collect()))
}

#[must_use]
pub fn list_skip(self_: &[Value], n: i64) -> Value {
    let n = usize::try_from(n).unwrap_or(0);
    Value::List(Arc::new(self_.iter().skip(n).cloned().collect()))
}

pub fn list_flat_map(self_: &[Value], f: &Closure, program: &Arc<Program>) -> Result<Value, Abort> {
    let mut out = Vec::new();
    for v in self_ {
        let mapped = call_closure(f, vec![v.clone()], program)?;
        let Value::List(inner) = mapped else {
            unreachable!("type-checked already, so flat_map's f always returns list[U]")
        };
        out.extend(inner.iter().cloned());
    }
    Ok(Value::List(Arc::new(out)))
}

/// Compares the key function's return value (int/float/str, D-OP-05). Since the 3 possible
/// return types have already been resolved by type checking (a fixed table in check_expr.rs), this only trusts that the
/// value is one of those 3 kinds and branches on it. Since float has non-reflexive NaN,
/// `partial_cmp` returning `None` falls back to `Ordering::Equal` (prioritizing not panicking or
/// crashing over the relative order between NaNs, or between a NaN and another value, being
/// slightly inconsistent within the bounds of a stable sort -- a decision made in this file).
fn compare_sort_keys(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => {
            unreachable!("type-checked already, so sort_by's keys are uniformly int, float, or str")
        }
    }
}

/// `sort_by[T](self: list[T], f: (T) -> int|float|str): list[T]`. A stable sort (Rust's
/// `slice::sort_by` is stable).
pub fn list_sort_by(self_: &[Value], f: &Closure, program: &Arc<Program>) -> Result<Value, Abort> {
    let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(self_.len());
    for v in self_ {
        let key = call_closure(f, vec![v.clone()], program)?;
        keyed.push((key, v.clone()));
    }
    keyed.sort_by(|(ka, _), (kb, _)| compare_sort_keys(ka, kb));
    Ok(Value::List(Arc::new(
        keyed.into_iter().map(|(_, v)| v).collect(),
    )))
}

#[must_use]
pub fn list_chain(self_: &[Value], other: &[Value]) -> Value {
    let items = self_.iter().chain(other.iter()).cloned().collect();
    Value::List(Arc::new(items))
}

/// `get[T](self: list[T], i: int): Option[T]` (the safe version of the panicking `xs[i]`).
#[must_use]
pub fn list_get(self_: &[Value], i: i64) -> Value {
    usize::try_from(i)
        .ok()
        .and_then(|idx| self_.get(idx))
        .map_or_else(none_value, |v| some_value(v.clone()))
}

#[must_use]
pub fn list_len(self_: &[Value]) -> Value {
    Value::Int(i64::try_from(self_.len()).unwrap_or(i64::MAX))
}

#[must_use]
pub fn list_is_empty(self_: &[Value]) -> Value {
    Value::Bool(self_.is_empty())
}

/// `contains[T](self: list[T], x: T): bool`. `==` for T is always structural equality (D-OP-06).
#[must_use]
pub fn list_contains(self_: &[Value], x: &Value) -> Value {
    Value::Bool(self_.contains(x))
}

#[must_use]
pub fn list_first(self_: &[Value]) -> Value {
    self_
        .first()
        .map_or_else(none_value, |v| some_value(v.clone()))
}

#[must_use]
pub fn list_last(self_: &[Value]) -> Value {
    self_
        .last()
        .map_or_else(none_value, |v| some_value(v.clone()))
}

/// `join(self: list[str], sep: str): str`.
#[must_use]
pub fn list_join(self_: &[Value], sep: &str) -> Value {
    let parts: Vec<&str> = self_
        .iter()
        .map(|v| {
            let Value::Str(s) = v else {
                unreachable!("type-checked already, so join is restricted to list[str]")
            };
            s.as_ref()
        })
        .collect();
    Value::Str(Arc::from(parts.join(sep)))
}

/// `slice[T](self: list[T], start: int, end: int): list[T]`. panics: out of range (E6001).
pub fn list_slice(self_: &[Value], start: i64, end: i64, span: Span) -> Result<Value, Abort> {
    let len = self_.len();
    let bounds = usize::try_from(start)
        .ok()
        .zip(usize::try_from(end).ok())
        .filter(|(s, e)| s <= e && *e <= len);
    match bounds {
        Some((s, e)) => Ok(Value::List(Arc::new(self_[s..e].to_vec()))),
        None => Err(panic::out_of_range(span, "list slice")),
    }
}

/// `to_set[T](self: list[T]): set[T]`. T is restricted to D-TYPE-05's allowed key types (already
/// excluded by type checking).
#[must_use]
pub fn list_to_set(self_: &[Value]) -> Value {
    let mut set = IndexSet::with_capacity(self_.len());
    for v in self_ {
        let key = MapKey::try_from_value(v).unwrap_or_else(|| {
            unreachable!(
                "type-checked already, so to_set's elements are always an allowed key type"
            )
        });
        set.insert(key);
    }
    Value::Set(Arc::new(set))
}

/// `each[T](self: list[T], f: (T) -> void): void` (D-SYN-09's sequential-side-effect-only
/// iteration).
pub fn list_each(self_: &[Value], f: &Closure, program: &Arc<Program>) -> Result<Value, Abort> {
    for v in self_ {
        call_closure(f, vec![v.clone()], program)?;
    }
    Ok(Value::Void)
}

// --- 2.1 list[T]: destructive (requires var self. The caller passes an &mut Arc<Vec<Value>>
// after resolve_place in eval/lvalue.rs has already checked whether it's a var binding) ---

pub fn list_push(self_: &mut Arc<Vec<Value>>, x: Value) {
    Arc::make_mut(self_).push(x);
}

pub fn list_pop(self_: &mut Arc<Vec<Value>>) -> Value {
    Arc::make_mut(self_)
        .pop()
        .map_or_else(none_value, some_value)
}

/// panics: out of range (E6001).
pub fn list_insert(
    self_: &mut Arc<Vec<Value>>,
    i: i64,
    x: Value,
    span: Span,
) -> Result<Value, Abort> {
    let len = self_.len();
    match usize::try_from(i).ok().filter(|idx| *idx <= len) {
        Some(idx) => {
            Arc::make_mut(self_).insert(idx, x);
            Ok(Value::Void)
        }
        None => Err(panic::out_of_range(span, "list insert")),
    }
}

/// panics: out of range (E6001).
pub fn list_remove(self_: &mut Arc<Vec<Value>>, i: i64, span: Span) -> Result<Value, Abort> {
    let len = self_.len();
    match usize::try_from(i).ok().filter(|idx| *idx < len) {
        Some(idx) => Ok(Arc::make_mut(self_).remove(idx)),
        None => Err(panic::out_of_range(span, "list remove")),
    }
}

pub fn list_extend(self_: &mut Arc<Vec<Value>>, other: &[Value]) {
    Arc::make_mut(self_).extend_from_slice(other);
}

pub fn list_clear(self_: &mut Arc<Vec<Value>>) {
    Arc::make_mut(self_).clear();
}

// --- 2.2 dict[K, V]: non-destructive ---

#[must_use]
pub fn dict_get(self_: &IndexMap<MapKey, Value>, k: &MapKey) -> Value {
    self_
        .get(k)
        .map_or_else(none_value, |v| some_value(v.clone()))
}

#[must_use]
pub fn dict_contains_key(self_: &IndexMap<MapKey, Value>, k: &MapKey) -> Value {
    Value::Bool(self_.contains_key(k))
}

#[must_use]
pub fn dict_keys(self_: &IndexMap<MapKey, Value>) -> Value {
    Value::List(Arc::new(self_.keys().map(MapKey::to_value).collect()))
}

#[must_use]
pub fn dict_values(self_: &IndexMap<MapKey, Value>) -> Value {
    Value::List(Arc::new(self_.values().cloned().collect()))
}

fn pair_value(k: &MapKey, v: &Value) -> Value {
    Value::Tuple(Arc::from(vec![k.to_value(), v.clone()]))
}

#[must_use]
pub fn dict_entries(self_: &IndexMap<MapKey, Value>) -> Value {
    let items = self_.iter().map(|(k, v)| pair_value(k, v)).collect();
    Value::List(Arc::new(items))
}

#[must_use]
pub fn dict_len(self_: &IndexMap<MapKey, Value>) -> Value {
    Value::Int(i64::try_from(self_.len()).unwrap_or(i64::MAX))
}

/// The higher-order method group from STDLIB.md §2.2 (a note carried over from Unit 11: not yet
/// wired into call.rs's dict_method_readonly -- `dict_method_readonly` doesn't currently take
/// `program`, which requires a change on the call.rs side, outside this task's scope; flagged
/// for review).
pub fn dict_map(
    self_: &IndexMap<MapKey, Value>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let mut out = Vec::with_capacity(self_.len());
    for (k, v) in self_ {
        out.push(call_closure(f, vec![pair_value(k, v)], program)?);
    }
    Ok(Value::List(Arc::new(out)))
}

pub fn dict_filter(
    self_: &IndexMap<MapKey, Value>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let mut out = IndexMap::new();
    for (k, v) in self_ {
        let keep = call_closure(f, vec![pair_value(k, v)], program)?;
        let Value::Bool(b) = keep else {
            unreachable!("type-checked already, so dict.filter's f always returns bool")
        };
        if b {
            out.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Dict(Arc::new(out)))
}

pub fn dict_any(
    self_: &IndexMap<MapKey, Value>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    for (k, v) in self_ {
        let matched = call_closure(f, vec![pair_value(k, v)], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so dict.any's f always returns bool")
        };
        if b {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

pub fn dict_all(
    self_: &IndexMap<MapKey, Value>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    for (k, v) in self_ {
        let matched = call_closure(f, vec![pair_value(k, v)], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so dict.all's f always returns bool")
        };
        if !b {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

pub fn dict_find(
    self_: &IndexMap<MapKey, Value>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    for (k, v) in self_ {
        let matched = call_closure(f, vec![pair_value(k, v)], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so dict.find's f always returns bool")
        };
        if b {
            return Ok(some_value(pair_value(k, v)));
        }
    }
    Ok(none_value())
}

pub fn dict_fold(
    self_: &IndexMap<MapKey, Value>,
    init: Value,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let mut acc = init;
    for (k, v) in self_ {
        acc = call_closure(f, vec![acc, pair_value(k, v)], program)?;
    }
    Ok(acc)
}

pub fn dict_each(
    self_: &IndexMap<MapKey, Value>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    for (k, v) in self_ {
        call_closure(f, vec![pair_value(k, v)], program)?;
    }
    Ok(Value::Void)
}

// --- 2.2 dict[K, V]: destructive ---

/// `insert[K, V](self: var dict[K, V], k: K, v: V): Option[V]` (returns the old value,
/// destructive).
pub fn dict_insert(self_: &mut Arc<IndexMap<MapKey, Value>>, k: MapKey, v: Value) -> Value {
    Arc::make_mut(self_)
        .insert(k, v)
        .map_or_else(none_value, some_value)
}

/// `remove[K, V](self: var dict[K, V], k: K): Option[V]` (destructive; uses shift_remove to
/// satisfy D-COL-01's "re-inserting after a removal moves it to the end" behavior).
pub fn dict_remove(self_: &mut Arc<IndexMap<MapKey, Value>>, k: &MapKey) -> Value {
    Arc::make_mut(self_)
        .shift_remove(k)
        .map_or_else(none_value, some_value)
}

pub fn dict_clear(self_: &mut Arc<IndexMap<MapKey, Value>>) {
    Arc::make_mut(self_).clear();
}

// --- 2.3 set[T]: non-destructive ---

#[must_use]
pub fn set_contains(self_: &IndexSet<MapKey>, x: &MapKey) -> Value {
    Value::Bool(self_.contains(x))
}

#[must_use]
pub fn set_len(self_: &IndexSet<MapKey>) -> Value {
    Value::Int(i64::try_from(self_.len()).unwrap_or(i64::MAX))
}

/// Union. Since DECISIONS doesn't specify insertion order, this file settles on a deterministic
/// rule: `self`'s insertion order, followed by the elements found only in `other` in `other`'s
/// insertion order (since `IndexSet::insert` doesn't move an already-present key, simply cloning
/// `self` and inserting `other`'s elements naturally achieves this rule).
#[must_use]
pub fn set_union(self_: &IndexSet<MapKey>, other: &IndexSet<MapKey>) -> Value {
    let mut out = self_.clone();
    for k in other {
        out.insert(k.clone());
    }
    Value::Set(Arc::new(out))
}

/// Intersection. Preserves `self`'s insertion order.
#[must_use]
pub fn set_intersection(self_: &IndexSet<MapKey>, other: &IndexSet<MapKey>) -> Value {
    let out: IndexSet<MapKey> = self_
        .iter()
        .filter(|k| other.contains(*k))
        .cloned()
        .collect();
    Value::Set(Arc::new(out))
}

/// Difference. Preserves `self`'s insertion order.
#[must_use]
pub fn set_difference(self_: &IndexSet<MapKey>, other: &IndexSet<MapKey>) -> Value {
    let out: IndexSet<MapKey> = self_
        .iter()
        .filter(|k| !other.contains(*k))
        .cloned()
        .collect();
    Value::Set(Arc::new(out))
}

#[must_use]
pub fn set_to_list(self_: &IndexSet<MapKey>) -> Value {
    Value::List(Arc::new(self_.iter().map(MapKey::to_value).collect()))
}

/// The higher-order method group from STDLIB.md §2.3 (same as dict -- wiring into call.rs's
/// set_method_readonly is outside this task's scope, flagged for review).
pub fn set_map(
    self_: &IndexSet<MapKey>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let mut out = Vec::with_capacity(self_.len());
    for k in self_ {
        out.push(call_closure(f, vec![k.to_value()], program)?);
    }
    Ok(Value::List(Arc::new(out)))
}

pub fn set_filter(
    self_: &IndexSet<MapKey>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let mut out = IndexSet::new();
    for k in self_ {
        let keep = call_closure(f, vec![k.to_value()], program)?;
        let Value::Bool(b) = keep else {
            unreachable!("type-checked already, so set.filter's f always returns bool")
        };
        if b {
            out.insert(k.clone());
        }
    }
    Ok(Value::Set(Arc::new(out)))
}

pub fn set_any(
    self_: &IndexSet<MapKey>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    for k in self_ {
        let matched = call_closure(f, vec![k.to_value()], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so set.any's f always returns bool")
        };
        if b {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

pub fn set_all(
    self_: &IndexSet<MapKey>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    for k in self_ {
        let matched = call_closure(f, vec![k.to_value()], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so set.all's f always returns bool")
        };
        if !b {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

pub fn set_find(
    self_: &IndexSet<MapKey>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    for k in self_ {
        let matched = call_closure(f, vec![k.to_value()], program)?;
        let Value::Bool(b) = matched else {
            unreachable!("type-checked already, so set.find's f always returns bool")
        };
        if b {
            return Ok(some_value(k.to_value()));
        }
    }
    Ok(none_value())
}

pub fn set_fold(
    self_: &IndexSet<MapKey>,
    init: Value,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    let mut acc = init;
    for k in self_ {
        acc = call_closure(f, vec![acc, k.to_value()], program)?;
    }
    Ok(acc)
}

/// `sum(self: set[int]): int` / `sum(self: set[float]): float` (the D-STDPOL-01 overload special
/// case).
pub fn set_sum(values: &IndexSet<MapKey>, span: crate::diagnostics::Span) -> Result<Value, Abort> {
    sum_values(values.iter().map(MapKey::to_value), false, span)
}

pub fn set_each(
    self_: &IndexSet<MapKey>,
    f: &Closure,
    program: &Arc<Program>,
) -> Result<Value, Abort> {
    for k in self_ {
        call_closure(f, vec![k.to_value()], program)?;
    }
    Ok(Value::Void)
}

// --- 2.3 set[T]: destructive ---

/// `insert[T](self: var set[T], x: T): bool` (true if newly inserted).
pub fn set_insert(self_: &mut Arc<IndexSet<MapKey>>, x: MapKey) -> Value {
    Value::Bool(Arc::make_mut(self_).insert(x))
}

/// `remove[T](self: var set[T], x: T): bool` (true if it existed and was removed; uses
/// shift_remove to preserve D-COL-01's insertion order).
pub fn set_remove(self_: &mut Arc<IndexSet<MapKey>>, x: &MapKey) -> Value {
    Value::Bool(Arc::make_mut(self_).shift_remove(x))
}

pub fn set_clear(self_: &mut Arc<IndexSet<MapKey>>) {
    Arc::make_mut(self_).clear();
}

// --- 2.4 tuple[A, B, ...] ---

/// `t.0`, `t.1`, ... (0-based dot notation, D-TYPE-06). Since the AST's `TupleIndex.index` is
/// already guaranteed at type-check time to be within the tuple's element count range, this is
/// not a panicking case here.
#[must_use]
pub fn tuple_index(self_: &[Value], index: u32) -> Value {
    self_[index as usize].clone()
}

#[cfg(test)]
mod tests {
    use super::{
        compare_sort_keys, dict_entries, dict_get, dict_insert, dict_keys, dict_remove,
        dict_values, list_chain, list_enumerate, list_get, list_join, list_rev, list_slice,
        list_sum, list_take, list_to_set, list_zip, set_difference, set_insert, set_intersection,
        set_remove, set_sum, set_to_list, set_union, tuple_index,
    };
    use crate::diagnostics::{FileId, Position, Span};
    use crate::eval::value::{MapKey, Value};
    use crate::stdlib::builtins::test_pipeline::run_ok_source;
    use crate::stdlib::{none_value, some_value};
    use indexmap::{IndexMap, IndexSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    #[test]
    fn list_enumerate_pairs_index_with_value() {
        let xs = vec![Value::Str(Arc::from("a")), Value::Str(Arc::from("b"))];
        let Value::List(items) = list_enumerate(&xs) else {
            panic!("expected list")
        };
        assert_eq!(
            items.as_ref(),
            &vec![
                Value::Tuple(Arc::from(vec![Value::Int(0), Value::Str(Arc::from("a"))])),
                Value::Tuple(Arc::from(vec![Value::Int(1), Value::Str(Arc::from("b"))])),
            ]
        );
    }

    #[test]
    fn list_zip_stops_at_shorter_length() {
        let a = vec![Value::Int(1), Value::Int(2), Value::Int(3)];
        let b = vec![Value::Int(9), Value::Int(8)];
        let Value::List(items) = list_zip(&a, &b) else {
            panic!("expected list")
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn list_rev_reverses_order() {
        let xs = vec![Value::Int(1), Value::Int(2), Value::Int(3)];
        let Value::List(items) = list_rev(&xs) else {
            panic!("expected list")
        };
        assert_eq!(
            items.as_ref(),
            &vec![Value::Int(3), Value::Int(2), Value::Int(1)]
        );
    }

    #[test]
    fn list_take_negative_n_is_empty() {
        let xs = vec![Value::Int(1), Value::Int(2)];
        let Value::List(items) = list_take(&xs, -1) else {
            panic!("expected list")
        };
        assert!(items.is_empty());
    }

    #[test]
    fn list_chain_concatenates() {
        let a = vec![Value::Int(1)];
        let b = vec![Value::Int(2)];
        let Value::List(items) = list_chain(&a, &b) else {
            panic!("expected list")
        };
        assert_eq!(items.as_ref(), &vec![Value::Int(1), Value::Int(2)]);
    }

    #[test]
    fn list_get_out_of_range_and_negative_are_none() {
        let xs = vec![Value::Int(1)];
        assert_eq!(list_get(&xs, 0), some_value(Value::Int(1)));
        assert_eq!(list_get(&xs, 1), none_value());
        assert_eq!(list_get(&xs, -1), none_value());
    }

    #[test]
    fn list_join_concatenates_strings_with_separator() {
        let xs = vec![Value::Str(Arc::from("a")), Value::Str(Arc::from("b"))];
        assert_eq!(list_join(&xs, ", "), Value::Str(Arc::from("a, b")));
    }

    #[test]
    fn list_slice_panics_out_of_range_and_on_reversed_bounds() {
        let xs = vec![Value::Int(1), Value::Int(2), Value::Int(3)];
        let Ok(v) = list_slice(&xs, 1, 3, dummy_span()) else {
            panic!("expected Ok")
        };
        assert_eq!(v, Value::List(Arc::new(vec![Value::Int(2), Value::Int(3)])));
        assert!(list_slice(&xs, 0, 10, dummy_span()).is_err());
        assert!(list_slice(&xs, 2, 1, dummy_span()).is_err());
    }

    #[test]
    fn list_sum_preserves_numeric_type_and_checks_overflow() {
        assert_eq!(
            list_sum(
                &[Value::Int(1), Value::Int(2), Value::Int(3)],
                false,
                dummy_span()
            )
            .ok(),
            Some(Value::Int(6))
        );
        assert_eq!(
            list_sum(&[Value::Float(1.5), Value::Float(2.5)], true, dummy_span()).ok(),
            Some(Value::Float(4.0))
        );
        assert_eq!(
            list_sum(&[], true, dummy_span()).ok(),
            Some(Value::Float(0.0))
        );
        assert!(list_sum(&[Value::Int(i64::MAX), Value::Int(1)], false, dummy_span()).is_err());
    }

    #[test]
    fn list_to_set_deduplicates() {
        let xs = vec![Value::Int(1), Value::Int(1), Value::Int(2)];
        let Value::Set(s) = list_to_set(&xs) else {
            panic!("expected set")
        };
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn compare_sort_keys_orders_int_float_str() {
        assert_eq!(
            compare_sort_keys(&Value::Int(1), &Value::Int(2)),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_sort_keys(&Value::Str(Arc::from("a")), &Value::Str(Arc::from("b"))),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn dict_basic_readonly_methods() {
        let mut m: IndexMap<MapKey, Value> = IndexMap::new();
        m.insert(MapKey::Str(Arc::from("a")), Value::Int(1));
        m.insert(MapKey::Str(Arc::from("b")), Value::Int(2));
        assert_eq!(
            dict_get(&m, &MapKey::Str(Arc::from("a"))),
            some_value(Value::Int(1))
        );
        assert_eq!(dict_get(&m, &MapKey::Str(Arc::from("z"))), none_value());
        let Value::List(keys) = dict_keys(&m) else {
            panic!("expected list")
        };
        assert_eq!(
            keys.as_ref(),
            &vec![Value::Str(Arc::from("a")), Value::Str(Arc::from("b"))]
        );
        let Value::List(values) = dict_values(&m) else {
            panic!("expected list")
        };
        assert_eq!(values.as_ref(), &vec![Value::Int(1), Value::Int(2)]);
        let Value::List(entries) = dict_entries(&m) else {
            panic!("expected list")
        };
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn dict_insert_returns_old_value_and_remove_shift_removes() {
        let mut m: Arc<IndexMap<MapKey, Value>> = Arc::new(IndexMap::new());
        let key = MapKey::Str(Arc::from("a"));
        assert_eq!(
            dict_insert(&mut m, key.clone(), Value::Int(1)),
            none_value()
        );
        assert_eq!(
            dict_insert(&mut m, key.clone(), Value::Int(2)),
            some_value(Value::Int(1))
        );
        assert_eq!(dict_remove(&mut m, &key), some_value(Value::Int(2)));
        assert_eq!(dict_remove(&mut m, &key), none_value());
        assert!(m.is_empty());
    }

    #[test]
    fn set_union_intersection_difference_preserve_self_order() {
        let mut lhs: IndexSet<MapKey> = IndexSet::new();
        lhs.insert(MapKey::Int(1));
        lhs.insert(MapKey::Int(2));
        let mut rhs: IndexSet<MapKey> = IndexSet::new();
        rhs.insert(MapKey::Int(2));
        rhs.insert(MapKey::Int(3));

        let Value::Set(union) = set_union(&lhs, &rhs) else {
            panic!("expected set")
        };
        assert_eq!(
            union.iter().cloned().collect::<Vec<_>>(),
            vec![MapKey::Int(1), MapKey::Int(2), MapKey::Int(3)]
        );

        let Value::Set(intersection) = set_intersection(&lhs, &rhs) else {
            panic!("expected set")
        };
        assert_eq!(
            intersection.iter().cloned().collect::<Vec<_>>(),
            vec![MapKey::Int(2)]
        );

        let Value::Set(difference) = set_difference(&lhs, &rhs) else {
            panic!("expected set")
        };
        assert_eq!(
            difference.iter().cloned().collect::<Vec<_>>(),
            vec![MapKey::Int(1)]
        );
    }

    #[test]
    fn set_insert_remove_report_whether_they_changed_membership() {
        let mut s: Arc<IndexSet<MapKey>> = Arc::new(IndexSet::new());
        assert_eq!(set_insert(&mut s, MapKey::Int(1)), Value::Bool(true));
        assert_eq!(set_insert(&mut s, MapKey::Int(1)), Value::Bool(false));
        assert_eq!(set_remove(&mut s, &MapKey::Int(1)), Value::Bool(true));
        assert_eq!(set_remove(&mut s, &MapKey::Int(1)), Value::Bool(false));
    }

    #[test]
    fn set_to_list_and_sum() {
        let mut s: IndexSet<MapKey> = IndexSet::new();
        s.insert(MapKey::Int(3));
        s.insert(MapKey::Int(4));
        let Value::List(items) = set_to_list(&s) else {
            panic!("expected list")
        };
        assert_eq!(items.as_ref(), &vec![Value::Int(3), Value::Int(4)]);
        assert_eq!(set_sum(&s, dummy_span()).ok(), Some(Value::Int(7)));
    }

    #[test]
    fn tuple_index_reads_positional_field() {
        let t = vec![Value::Int(1), Value::Str(Arc::from("x"))];
        assert_eq!(tuple_index(&t, 0), Value::Int(1));
        assert_eq!(tuple_index(&t, 1), Value::Str(Arc::from("x")));
    }

    /// Regression test for Task 1: verifies through the full pipeline (lex/parse/
    /// module_resolve/typecheck/effect check/eval) that `dict_method_readonly` in
    /// `eval/call.rs` is actually wired to `dict_map`/`dict_filter`/`dict_any`/`dict_all`/
    /// `dict_find`/`dict_fold`/`dict_each` (this file). Since `samples/**` cannot be modified,
    /// the source string is passed directly to `run_ok_source`
    /// (`stdlib::builtins::test_pipeline`).
    #[test]
    fn dict_higher_order_methods_are_wired_through_full_pipeline() {
        let src = r#"
d = {"a": 1, "b": 2, "c": 3}

mapped = d.map((p) => p.1 * 10)
assert(mapped.len() == 3)
assert(mapped.contains(10))
assert(mapped.contains(20))
assert(mapped.contains(30))

filtered = d.filter((p) => p.1 > 1)
assert(filtered.len() == 2)
assert(filtered.contains_key("b"))
assert(filtered.contains_key("c"))

assert(d.any((p) => p.1 == 2))
assert(d.all((p) => p.1 > 0))

found = d.find((p) => p.0 == "b")
assert(found.unwrap().1 == 2)

summed = d.fold(0, (acc, p) => acc + p.1)
assert(summed == 6)

d.each((p) => print(str(p.1)))
"#;
        let result = run_ok_source(
            "dict_higher_order_methods",
            &PathBuf::from("dict_higher_order_methods.ybm"),
            src,
        );
        assert!(
            result.is_ok(),
            "sample should run without Abort: {result:?}"
        );
    }

    /// Regression test for Task 1: verifies through the full pipeline that
    /// `set_method_readonly` is actually wired to `set_map`/`set_filter`/`set_any`/`set_all`/
    /// `set_find`/`set_fold`/`set_each`/`set_sum` (this file) (the same approach as
    /// `dict_higher_order_methods_are_wired_through_full_pipeline`).
    #[test]
    fn set_higher_order_methods_are_wired_through_full_pipeline() {
        let src = r"
s = {1, 2, 3, 4, 5}

mapped = s.map((x) => x * 2)
assert(mapped.len() == 5)
assert(mapped.contains(2))
assert(mapped.contains(10))

filtered = s.filter((x) => x % 2 == 0)
assert(filtered.len() == 2)
assert(filtered.contains(2))
assert(filtered.contains(4))

assert(s.any((x) => x > 4))
assert(s.all((x) => x > 0))

found = s.find((x) => x == 3)
assert(found.unwrap() == 3)

folded = s.fold(0, (acc, x) => acc + x)
assert(folded == 15)

assert(s.sum() == 15)

s.each((x) => print(str(x)))
";
        let result = run_ok_source(
            "set_higher_order_methods",
            &PathBuf::from("set_higher_order_methods.ybm"),
            src,
        );
        assert!(
            result.is_ok(),
            "sample should run without Abort: {result:?}"
        );
    }
}
