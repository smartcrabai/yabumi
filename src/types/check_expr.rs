//! The core of expression type checking (ARCHITECTURE.md §2.2). Also performs name-
//! resolution priority here (local-scope binding > fixed namespace name > flat namespace,
//! §5.12).
//!
//! The signatures of stdlib (namespace functions, and methods of primitives/collections/
//! Result/Option/Value) are held directly as a fixed table within this file
//! (`namespace_fn_sig`, etc.), rather than going through `src/stdlib/mod.rs`'s
//! value-level runtime dispatch.
//! Type checking (compile-time signature resolution) and evaluation (runtime value
//! dispatch) are deliberately separated because they are different concerns, and this
//! file was designed so type checking never directly calls into stdlib's runtime dispatch
//! (judgment call made in this file). The source of truth for signatures overlaps with
//! `docs/STDLIB.md`, but this is a deliberate design decision to separate the concerns of
//! type checking and evaluation, and the two are not unified into a single table.

use crate::ast::{
    Arg, BinaryOp, Block, ElseBranch, Expr, ExprKind, FStringSegment, IfExpr, LiteralPat, MatchArm,
    MatchArmBody, ParKind, Param, Pattern, PipeCallee, StmtKind, SubPattern, UnaryOp,
};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::eval::env::Program;
use crate::eval::value::Value;
use crate::types::env::TypeEnv;
use crate::types::generics::{self, ty_from_ann};
use crate::types::infer;
use crate::types::mutability;
use crate::types::{BareIdentKind, CallKind, EffectSet, NamespaceId, Ty, WrapKind};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Small helpers for constructing Ty (for readability).
// ---------------------------------------------------------------------------

fn t_list(t: Ty) -> Ty {
    Ty::List(Box::new(t))
}
fn t_set(t: Ty) -> Ty {
    Ty::Set(Box::new(t))
}
fn t_dict(k: Ty, v: Ty) -> Ty {
    Ty::Dict(Box::new(k), Box::new(v))
}
fn t_tuple(items: Vec<Ty>) -> Ty {
    Ty::Tuple(items)
}
fn t_option(t: Ty) -> Ty {
    Ty::Named {
        name: Arc::from("Option"),
        args: vec![t],
    }
}
fn t_result(t: Ty, e: Ty) -> Ty {
    Ty::Named {
        name: Arc::from("Result"),
        args: vec![t, e],
    }
}
fn t_error() -> Ty {
    Ty::Named {
        name: Arc::from("Error"),
        args: vec![],
    }
}
fn t_value() -> Ty {
    Ty::Named {
        name: Arc::from("Value"),
        args: vec![],
    }
}
fn t_response() -> Ty {
    Ty::Named {
        name: Arc::from("Response"),
        args: vec![],
    }
}
fn t_http_options() -> Ty {
    Ty::Named {
        name: Arc::from("HttpOptions"),
        args: vec![],
    }
}
fn t_proc_output() -> Ty {
    Ty::Named {
        name: Arc::from("ProcOutput"),
        args: vec![],
    }
}
fn t_fn(params: Vec<Ty>, ret: Ty) -> Ty {
    Ty::Function {
        params,
        effects: EffectSet::empty(),
        ret: Box::new(ret),
    }
}
fn tv(name: &str) -> Ty {
    Ty::TypeVar(Arc::from(name))
}

/// The signature of a single stdlib item (a namespace function or a builtin method). To
/// ride on the same `generics::unify_collect`/`substitute`/`finalize_ret` foundation as a
/// user-defined generic function, an undetermined type is represented as
/// `Ty::TypeVar("$…")` (a synthesized name; using a `$`-prefixed name that never
/// collides with a user type-parameter name such as `T`/`U` -- safe because D-LEX's
/// identifier grammar disallows a `$` prefix).
struct Sig {
    generics: Vec<Arc<str>>,
    params: Vec<Ty>,
    ret: Ty,
    effects: EffectSet,
    /// D-FUNC-03/§5.5 "a STDLIB higher-order method unconditionally treats a function-
    /// typed argument as a forwarding target" -- if true, each function-typed argument's
    /// `effects` are added into the caller's (the function/lambda currently under check)
    /// effect estimate.
    forward_fn_effects: bool,
    /// The equivalent of D-MUT-01/02's `var self` (STDLIB.md's "the notation
    /// `self: var list[T]`").
    mutates: bool,
}

impl Sig {
    fn new(params: Vec<Ty>, ret: Ty) -> Self {
        Self {
            generics: Vec::new(),
            params,
            ret,
            effects: EffectSet::empty(),
            forward_fn_effects: false,
            mutates: false,
        }
    }
    fn with_generics(mut self, names: &[&str]) -> Self {
        self.generics = names.iter().map(|s| Arc::from(*s)).collect();
        self
    }
    fn with_effects(mut self, e: EffectSet) -> Self {
        self.effects = e;
        self
    }
    fn hof(mut self) -> Self {
        self.forward_fn_effects = true;
        self
    }
    fn mutating(mut self) -> Self {
        self.mutates = true;
        self
    }
}

fn builtin_struct_fields(name: &str) -> Option<Vec<(&'static str, Ty)>> {
    Some(match name {
        "Error" => vec![
            ("kind", Ty::Str),
            ("message", Ty::Str),
            ("cause", t_option(t_error())),
        ],
        "Response" => vec![
            ("status", Ty::Int),
            ("headers", t_dict(Ty::Str, Ty::Str)),
            ("body", Ty::Str),
        ],
        "HttpOptions" => vec![
            ("headers", t_dict(Ty::Str, Ty::Str)),
            ("timeout_ms", Ty::Int),
        ],
        "ProcOutput" => vec![
            ("stdout", Ty::Str),
            ("stderr", Ty::Str),
            ("exit_code", Ty::Int),
        ],
        _ => return None,
    })
}

fn namespace_const_ty(ns: NamespaceId, name: &str) -> Option<Ty> {
    match (ns, name) {
        (NamespaceId::Math, "PI" | "E") => Some(Ty::Float),
        _ => None,
    }
}

fn namespace_fn_sig(ns: NamespaceId, name: &str) -> Option<Sig> {
    use NamespaceId::{Csv, Env, Fs, Http, Json, Math, Proc, Rand, Regex, Time, Toml, Yaml};
    Some(match (ns, name) {
        (Fs, "read") => {
            Sig::new(vec![Ty::Str], t_result(Ty::Str, t_error())).with_effects(EffectSet::FS)
        }
        (Fs, "read_bytes") => Sig::new(vec![Ty::Str], t_result(t_list(Ty::Int), t_error()))
            .with_effects(EffectSet::FS),
        (Fs, "write" | "append") => {
            Sig::new(vec![Ty::Str, Ty::Str], t_option(t_error())).with_effects(EffectSet::FS)
        }
        (Fs, "list") => Sig::new(vec![Ty::Str], t_result(t_list(Ty::Str), t_error()))
            .with_effects(EffectSet::FS),
        (Fs, "exists") => Sig::new(vec![Ty::Str], Ty::Bool).with_effects(EffectSet::FS),
        (Fs, "remove") => Sig::new(vec![Ty::Str], t_option(t_error())).with_effects(EffectSet::FS),

        (Http, "get" | "delete") => {
            Sig::new(vec![Ty::Str], t_result(t_response(), t_error())).with_effects(EffectSet::NET)
        }
        (Http, "post" | "put") => {
            Sig::new(vec![Ty::Str, Ty::Str], t_result(t_response(), t_error()))
                .with_effects(EffectSet::NET)
        }
        (Http, "request") => Sig::new(
            vec![Ty::Str, Ty::Str, t_http_options()],
            t_result(t_response(), t_error()),
        )
        .with_effects(EffectSet::NET),

        (Env, "get") => Sig::new(vec![Ty::Str], t_option(Ty::Str)).with_effects(EffectSet::ENV),
        (Env, "set") => Sig::new(vec![Ty::Str, Ty::Str], Ty::Void).with_effects(EffectSet::ENV),
        (Env, "args") => Sig::new(vec![], t_list(Ty::Str)).with_effects(EffectSet::ENV),
        (Env, "stdin") => {
            Sig::new(vec![], t_result(Ty::Str, t_error())).with_effects(EffectSet::ENV)
        }

        (Proc, "run") => Sig::new(
            vec![Ty::Str, t_list(Ty::Str)],
            t_result(t_proc_output(), t_error()),
        )
        .with_effects(EffectSet::PROC),

        (Time, "now") => Sig::new(vec![], Ty::Int).with_effects(EffectSet::TIME),
        (Time, "sleep") => Sig::new(vec![Ty::Int], Ty::Void).with_effects(EffectSet::TIME),
        (Time, "format") => Sig::new(vec![Ty::Int, Ty::Str], Ty::Str).with_effects(EffectSet::TIME),
        (Time, "parse") => Sig::new(vec![Ty::Str, Ty::Str], t_result(Ty::Int, t_error()))
            .with_effects(EffectSet::TIME),

        (Rand, "int") => Sig::new(vec![Ty::Int, Ty::Int], Ty::Int).with_effects(EffectSet::RAND),
        (Rand, "float") => Sig::new(vec![], Ty::Float).with_effects(EffectSet::RAND),
        (Rand, "bool") => Sig::new(vec![], Ty::Bool).with_effects(EffectSet::RAND),
        (Rand, "choice") => Sig::new(vec![t_list(tv("$T"))], t_option(tv("$T")))
            .with_generics(&["$T"])
            .with_effects(EffectSet::RAND),

        (Regex, "is_match") => Sig::new(vec![Ty::Str, Ty::Str], t_result(Ty::Bool, t_error())),
        (Regex, "find") => Sig::new(
            vec![Ty::Str, Ty::Str],
            t_result(t_option(Ty::Str), t_error()),
        ),
        (Regex, "find_all") => {
            Sig::new(vec![Ty::Str, Ty::Str], t_result(t_list(Ty::Str), t_error()))
        }
        (Regex, "replace" | "replace_all") => Sig::new(
            vec![Ty::Str, Ty::Str, Ty::Str],
            t_result(Ty::Str, t_error()),
        ),
        (Regex, "captures") => Sig::new(
            vec![Ty::Str, Ty::Str],
            t_result(t_option(t_list(Ty::Str)), t_error()),
        ),

        (Math, "checked_div" | "checked_mod" | "checked_add" | "checked_sub" | "checked_mul") => {
            Sig::new(vec![Ty::Int, Ty::Int], t_option(Ty::Int))
        }
        (Math, "abs_int") => Sig::new(vec![Ty::Int], Ty::Int),
        (Math, "abs_float" | "sqrt") => Sig::new(vec![Ty::Float], Ty::Float),
        (Math, "min_int" | "max_int") => Sig::new(vec![Ty::Int, Ty::Int], Ty::Int),
        (Math, "min_float" | "max_float" | "pow") => {
            Sig::new(vec![Ty::Float, Ty::Float], Ty::Float)
        }
        (Math, "floor" | "ceil" | "round") => Sig::new(vec![Ty::Float], Ty::Int),

        (Json | Yaml | Toml, "decode") => {
            Sig::new(vec![Ty::Str], t_result(tv("$T"), t_error())).with_generics(&["$T"])
        }
        (Json | Yaml | Toml, "encode") => Sig::new(vec![tv("$T")], Ty::Str).with_generics(&["$T"]),
        (Csv, "decode") => {
            Sig::new(vec![Ty::Str], t_result(t_list(tv("$T")), t_error())).with_generics(&["$T"])
        }
        (Csv, "encode") => Sig::new(vec![t_list(tv("$T"))], Ty::Str).with_generics(&["$T"]),
        (Csv, "decode_rows") => Sig::new(
            vec![Ty::Str],
            t_result(t_list(t_dict(Ty::Str, t_value())), t_error()),
        ),

        _ => return None,
    })
}

fn primitive_method_sig(recv: &Ty, method: &str) -> Option<Sig> {
    match recv {
        Ty::Int | Ty::Float if method == "to_str" => Some(Sig::new(vec![], Ty::Str)),
        Ty::Str => str_method_sig(method),
        _ => None,
    }
}

fn str_method_sig(method: &str) -> Option<Sig> {
    Some(match method {
        "len" | "count" => Sig::new(vec![], Ty::Int),
        "get" => Sig::new(vec![Ty::Int], t_option(Ty::Str)),
        "bytes" => Sig::new(vec![], t_list(Ty::Int)),
        "trim" | "trim_start" | "trim_end" | "to_upper" | "to_lower" | "to_str" => {
            Sig::new(vec![], Ty::Str)
        }
        "contains" | "starts_with" | "ends_with" => Sig::new(vec![Ty::Str], Ty::Bool),
        "replace" => Sig::new(vec![Ty::Str, Ty::Str], Ty::Str),
        "repeat" => Sig::new(vec![Ty::Int], Ty::Str),
        "is_empty" => Sig::new(vec![], Ty::Bool),
        "find" => Sig::new(vec![Ty::Str], t_option(Ty::Int)),
        "slice" => Sig::new(vec![Ty::Int, Ty::Int], Ty::Str),
        "parse_int" => Sig::new(vec![], t_result(Ty::Int, t_error())),
        "parse_float" => Sig::new(vec![], t_result(Ty::Float, t_error())),
        "map" => Sig::new(vec![t_fn(vec![Ty::Str], tv("$U"))], t_list(tv("$U")))
            .with_generics(&["$U"])
            .hof(),
        "filter" => Sig::new(vec![t_fn(vec![Ty::Str], Ty::Bool)], t_list(Ty::Str)).hof(),
        "fold" => Sig::new(
            vec![tv("$Acc"), t_fn(vec![tv("$Acc"), Ty::Str], tv("$Acc"))],
            tv("$Acc"),
        )
        .with_generics(&["$Acc"])
        .hof(),
        "find_by" => Sig::new(vec![t_fn(vec![Ty::Str], Ty::Bool)], t_option(Ty::Str)).hof(),
        "any" | "all" => Sig::new(vec![t_fn(vec![Ty::Str], Ty::Bool)], Ty::Bool).hof(),
        "enumerate" => Sig::new(vec![], t_list(t_tuple(vec![Ty::Int, Ty::Str]))),
        "zip" => Sig::new(vec![Ty::Str], t_list(t_tuple(vec![Ty::Str, Ty::Str]))),
        "rev" | "chars" => Sig::new(vec![], t_list(Ty::Str)),
        "take" | "skip" => Sig::new(vec![Ty::Int], t_list(Ty::Str)),
        "flat_map" => Sig::new(
            vec![t_fn(vec![Ty::Str], t_list(tv("$U")))],
            t_list(tv("$U")),
        )
        .with_generics(&["$U"])
        .hof(),
        "sort_by" => Sig::new(vec![t_fn(vec![Ty::Str], Ty::Str)], t_list(Ty::Str)).hof(),
        "split" | "chain" => Sig::new(vec![Ty::Str], t_list(Ty::Str)),
        _ => return None,
    })
}

fn list_method_sig(t: &Ty, method: &str) -> Option<Sig> {
    Some(match method {
        "map" | "par_map" => Sig::new(vec![t_fn(vec![t.clone()], tv("$U"))], t_list(tv("$U")))
            .with_generics(&["$U"])
            .hof(),
        "filter" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Bool)], t_list(t.clone())).hof(),
        "fold" => Sig::new(
            vec![tv("$Acc"), t_fn(vec![tv("$Acc"), t.clone()], tv("$Acc"))],
            tv("$Acc"),
        )
        .with_generics(&["$Acc"])
        .hof(),
        "find" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Bool)], t_option(t.clone())).hof(),
        "any" | "all" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Bool)], Ty::Bool).hof(),
        "count" | "len" => Sig::new(vec![], Ty::Int),
        "sum" => match t {
            Ty::Int => Sig::new(vec![], Ty::Int),
            Ty::Float => Sig::new(vec![], Ty::Float),
            _ => return None,
        },
        "enumerate" => Sig::new(vec![], t_list(t_tuple(vec![Ty::Int, t.clone()]))),
        "zip" => Sig::new(
            vec![t_list(tv("$U"))],
            t_list(t_tuple(vec![t.clone(), tv("$U")])),
        )
        .with_generics(&["$U"]),
        "rev" => Sig::new(vec![], t_list(t.clone())),
        "take" | "skip" => Sig::new(vec![Ty::Int], t_list(t.clone())),
        "flat_map" => Sig::new(
            vec![t_fn(vec![t.clone()], t_list(tv("$U")))],
            t_list(tv("$U")),
        )
        .with_generics(&["$U"])
        .hof(),
        "sort_by" => Sig::new(vec![t_fn(vec![t.clone()], tv("$K"))], t_list(t.clone()))
            .with_generics(&["$K"])
            .hof(),
        "chain" => Sig::new(vec![t_list(t.clone())], t_list(t.clone())),
        "get" => Sig::new(vec![Ty::Int], t_option(t.clone())),
        "is_empty" => Sig::new(vec![], Ty::Bool),
        "contains" => Sig::new(vec![t.clone()], Ty::Bool),
        "first" | "last" => Sig::new(vec![], t_option(t.clone())),
        "join" => Sig::new(vec![Ty::Str], Ty::Str),
        "slice" => Sig::new(vec![Ty::Int, Ty::Int], t_list(t.clone())),
        "to_set" => Sig::new(vec![], t_set(t.clone())),
        "each" | "par_each" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Void)], Ty::Void).hof(),
        "push" => Sig::new(vec![t.clone()], Ty::Void).mutating(),
        "pop" => Sig::new(vec![], t_option(t.clone())).mutating(),
        "insert" => Sig::new(vec![Ty::Int, t.clone()], Ty::Void).mutating(),
        "remove" => Sig::new(vec![Ty::Int], t.clone()).mutating(),
        "extend" => Sig::new(vec![t_list(t.clone())], Ty::Void).mutating(),
        "clear" => Sig::new(vec![], Ty::Void).mutating(),
        "shuffle" => Sig::new(vec![], Ty::Void)
            .mutating()
            .with_effects(EffectSet::RAND),
        _ => return None,
    })
}

fn dict_method_sig(k: &Ty, v: &Ty, method: &str) -> Option<Sig> {
    let kv = t_tuple(vec![k.clone(), v.clone()]);
    Some(match method {
        "get" => Sig::new(vec![k.clone()], t_option(v.clone())),
        "contains_key" => Sig::new(vec![k.clone()], Ty::Bool),
        "keys" => Sig::new(vec![], t_list(k.clone())),
        "values" => Sig::new(vec![], t_list(v.clone())),
        "entries" => Sig::new(vec![], t_list(kv)),
        "len" => Sig::new(vec![], Ty::Int),
        "is_empty" => Sig::new(vec![], Ty::Bool),
        "map" => Sig::new(vec![t_fn(vec![kv], tv("$U"))], t_list(tv("$U")))
            .with_generics(&["$U"])
            .hof(),
        "filter" => Sig::new(vec![t_fn(vec![kv], Ty::Bool)], t_dict(k.clone(), v.clone())).hof(),
        "any" | "all" => Sig::new(vec![t_fn(vec![kv], Ty::Bool)], Ty::Bool).hof(),
        "find" => Sig::new(
            vec![t_fn(vec![kv], Ty::Bool)],
            t_option(t_tuple(vec![k.clone(), v.clone()])),
        )
        .hof(),
        "fold" => Sig::new(
            vec![tv("$Acc"), t_fn(vec![tv("$Acc"), kv], tv("$Acc"))],
            tv("$Acc"),
        )
        .with_generics(&["$Acc"])
        .hof(),
        "each" => Sig::new(vec![t_fn(vec![kv], Ty::Void)], Ty::Void).hof(),
        "insert" => Sig::new(vec![k.clone(), v.clone()], t_option(v.clone())).mutating(),
        "remove" => Sig::new(vec![k.clone()], t_option(v.clone())).mutating(),
        "clear" => Sig::new(vec![], Ty::Void).mutating(),
        _ => return None,
    })
}

fn set_method_sig(t: &Ty, method: &str) -> Option<Sig> {
    Some(match method {
        "contains" => Sig::new(vec![t.clone()], Ty::Bool),
        "len" | "count" => Sig::new(vec![], Ty::Int),
        "is_empty" => Sig::new(vec![], Ty::Bool),
        "union" | "intersection" | "difference" => {
            Sig::new(vec![t_set(t.clone())], t_set(t.clone()))
        }
        "to_list" => Sig::new(vec![], t_list(t.clone())),
        "map" => Sig::new(vec![t_fn(vec![t.clone()], tv("$U"))], t_list(tv("$U")))
            .with_generics(&["$U"])
            .hof(),
        "filter" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Bool)], t_set(t.clone())).hof(),
        "any" | "all" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Bool)], Ty::Bool).hof(),
        "find" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Bool)], t_option(t.clone())).hof(),
        "fold" => Sig::new(
            vec![tv("$Acc"), t_fn(vec![tv("$Acc"), t.clone()], tv("$Acc"))],
            tv("$Acc"),
        )
        .with_generics(&["$Acc"])
        .hof(),
        "sum" => match t {
            Ty::Int => Sig::new(vec![], Ty::Int),
            Ty::Float => Sig::new(vec![], Ty::Float),
            _ => return None,
        },
        "each" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Void)], Ty::Void).hof(),
        "insert" | "remove" => Sig::new(vec![t.clone()], Ty::Bool).mutating(),
        "clear" => Sig::new(vec![], Ty::Void).mutating(),
        _ => return None,
    })
}

fn result_method_sig(t: &Ty, e: &Ty, method: &str) -> Option<Sig> {
    Some(match method {
        "is_ok" | "is_err" => Sig::new(vec![], Ty::Bool),
        "ok" => Sig::new(vec![], t_option(t.clone())),
        "err" => Sig::new(vec![], t_option(e.clone())),
        "unwrap" => Sig::new(vec![], t.clone()),
        "unwrap_or" => Sig::new(vec![t.clone()], t.clone()),
        "unwrap_or_else" => Sig::new(vec![t_fn(vec![e.clone()], t.clone())], t.clone()).hof(),
        "map" => Sig::new(
            vec![t_fn(vec![t.clone()], tv("$U"))],
            t_result(tv("$U"), e.clone()),
        )
        .with_generics(&["$U"])
        .hof(),
        "map_err" => Sig::new(
            vec![t_fn(vec![e.clone()], tv("$F"))],
            t_result(t.clone(), tv("$F")),
        )
        .with_generics(&["$F"])
        .hof(),
        "and_then" => Sig::new(
            vec![t_fn(vec![t.clone()], t_result(tv("$U"), e.clone()))],
            t_result(tv("$U"), e.clone()),
        )
        .with_generics(&["$U"])
        .hof(),
        _ => return None,
    })
}

fn option_method_sig(t: &Ty, method: &str) -> Option<Sig> {
    Some(match method {
        "is_some" | "is_none" => Sig::new(vec![], Ty::Bool),
        "unwrap" => Sig::new(vec![], t.clone()),
        "unwrap_or" => Sig::new(vec![t.clone()], t.clone()),
        "unwrap_or_else" => Sig::new(vec![t_fn(vec![], t.clone())], t.clone()).hof(),
        "map" => Sig::new(vec![t_fn(vec![t.clone()], tv("$U"))], t_option(tv("$U")))
            .with_generics(&["$U"])
            .hof(),
        "and_then" => Sig::new(
            vec![t_fn(vec![t.clone()], t_option(tv("$U")))],
            t_option(tv("$U")),
        )
        .with_generics(&["$U"])
        .hof(),
        "filter" => Sig::new(vec![t_fn(vec![t.clone()], Ty::Bool)], t_option(t.clone())).hof(),
        "ok_or" => Sig::new(vec![tv("$E")], t_result(t.clone(), tv("$E"))).with_generics(&["$E"]),
        _ => return None,
    })
}

fn value_method_sig(method: &str) -> Option<Sig> {
    Some(match method {
        "as_int" => Sig::new(vec![], t_option(Ty::Int)),
        "as_float" => Sig::new(vec![], t_option(Ty::Float)),
        "as_str" => Sig::new(vec![], t_option(Ty::Str)),
        "as_bool" => Sig::new(vec![], t_option(Ty::Bool)),
        "as_list" => Sig::new(vec![], t_option(t_list(t_value()))),
        "as_dict" => Sig::new(vec![], t_option(t_dict(Ty::Str, t_value()))),
        "is_null" => Sig::new(vec![], Ty::Bool),
        "get" => Sig::new(vec![Ty::Str], t_option(t_value())),
        "at" => Sig::new(vec![Ty::Int], t_option(t_value())),
        _ => return None,
    })
}

/// Reconstructs a `Ty` from `Value::Value` (a module const's evaluated value) (D-MOD-02:
/// since a module-level const is a literal only, Struct/Enum/Closure should never mix in,
/// but this conservatively falls back to `Ty::Unknown`). An empty list/dict/set cannot
/// determine an element type, so `Ty::Unknown` is used as the element type (judgment call
/// made in this file -- expects that when an expression referencing the module const
/// performs a concrete element operation, it will be re-unified at that point against the
/// expected type then in effect).
/// Reconstructs a `Ty` from `eval::value::MapKey` (a dict/set key, D-TYPE-05).
/// `MapKey::to_value` (`eval/value.rs`) is a function that produces a `Value` (a runtime
/// value) and cannot be used for a `Ty` (a compile-time type), so this constructs the
/// `Ty` directly from `MapKey`'s variant (judgment call made in this file -- used when
/// determining the type of an expression referencing a module-level const's dict/set,
/// whose key is always int/bool/str/tuple per D-MOD-02).
fn ty_of_map_key(key: &crate::eval::value::MapKey) -> Ty {
    match key {
        crate::eval::value::MapKey::Int(_) => Ty::Int,
        crate::eval::value::MapKey::Bool(_) => Ty::Bool,
        crate::eval::value::MapKey::Str(_) => Ty::Str,
        crate::eval::value::MapKey::Tuple(items) => {
            Ty::Tuple(items.iter().map(ty_of_map_key).collect())
        }
    }
}

fn ty_of_const_value(value: &Value) -> Ty {
    match value {
        Value::Int(_) => Ty::Int,
        Value::Float(_) => Ty::Float,
        Value::Bool(_) => Ty::Bool,
        Value::Void => Ty::Void,
        Value::Str(_) => Ty::Str,
        Value::List(items) => t_list(items.first().map_or(Ty::Unknown, ty_of_const_value)),
        Value::Set(items) => t_set(items.iter().next().map_or(Ty::Unknown, ty_of_map_key)),
        Value::Dict(map) => {
            let (k, v) = map
                .iter()
                .next()
                .map_or((Ty::Unknown, Ty::Unknown), |(k, v)| {
                    (ty_of_map_key(k), ty_of_const_value(v))
                });
            t_dict(k, v)
        }
        Value::Tuple(items) => Ty::Tuple(items.iter().map(ty_of_const_value).collect()),
        Value::Struct(_) | Value::Enum(_) | Value::Closure(_) => Ty::Unknown,
    }
}

/// Type-checks a single expression and returns its determined type (also recorded into
/// `resolutions.expr_ty`). This function (together with the internal helpers split out
/// per expression kind, per the R5 decision's `too_many_lines` mitigation approach)
/// collectively handles D-MUT-01 through 03 (mutability checking, mutability.rs), D-TYPE-
/// 16 (assignment-target-driven inference, infer.rs), D-SYN-06 bare-identifier resolution,
/// and NAMESPACE-QUALIFIED-ACCESS-NO-RESOLUTION-HOME (§5.12's identifier-resolution
/// priority).
///
/// `ret_ctx` is the return type of the function/lambda currently under check (used for D-
/// ERR-01/02's `?` match determination and D-TYPE-17's `return` implicit-wrap
/// determination -- `check_stmt.rs` uses the same value when checking a `Return`
/// statement).
pub fn check_expr(
    expr: &Expr,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let ty = check_expr_kind(expr, expected, ret_ctx, env, program, effects, diagnostics);
    program.resolutions.expr_ty.insert(expr.id, ty.clone());
    ty
}

#[expect(
    clippy::too_many_lines,
    reason = "this is the dispatcher that routes every ExprKind variant (D-SYN-06/§3.4) in one \
place, and each arm already delegates in a single line to a dedicated helper such as \
check_call/check_binary -- even after already applying ARCHITECTURE.md §6.4's standard \
too_many_lines mitigation (splitting the check for each kind into a separate function), the line \
count still grows in proportion to the number of ExprKind variants itself"
)]
fn check_expr_kind(
    expr: &Expr,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let span = expr.span;
    match &expr.kind {
        ExprKind::IntLit(_) => Ty::Int,
        ExprKind::FloatLit(_) => Ty::Float,
        ExprKind::BoolLit(_) => Ty::Bool,
        ExprKind::StringLit(_) => Ty::Str,
        ExprKind::FString(segments) => {
            for segment in segments {
                if let FStringSegment::Expr(inner) = segment {
                    let inner_ty =
                        check_expr(inner, None, ret_ctx, env, program, effects, diagnostics);
                    if !matches!(
                        inner_ty,
                        Ty::Str | Ty::Int | Ty::Float | Ty::Bool | Ty::Unknown
                    ) {
                        diagnostics.push(Diagnostic {
                            code: ErrorCode::BranchTypeMismatch,
                            span: inner.span,
                            message: "f-string interpolation requires str, int, float, or bool"
                                .to_owned(),
                        });
                    }
                }
            }
            Ty::Str
        }
        ExprKind::Ident(name) => check_ident(name, span, expected, env, program, diagnostics),
        ExprKind::ListLit { elements, .. } => check_seq_lit(
            elements,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
            span,
            SeqKind::List,
        ),
        ExprKind::SetLit { elements, .. } => check_seq_lit(
            elements,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
            span,
            SeqKind::Set,
        ),
        ExprKind::TupleLit { elements, .. } => {
            let expected_elems = match expected {
                Some(Ty::Tuple(items)) if items.len() == elements.len() => {
                    items.iter().map(Some).collect::<Vec<_>>()
                }
                _ => vec![None; elements.len()],
            };
            let tys = elements
                .iter()
                .zip(expected_elems)
                .map(|(e, ex)| check_expr(e, ex, ret_ctx, env, program, effects, diagnostics))
                .collect();
            Ty::Tuple(tys)
        }
        ExprKind::DictLit { entries, .. } => check_dict_lit(
            entries,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
            span,
        ),
        ExprKind::Unary { op, operand } => check_unary(
            *op,
            operand,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
            span,
        ),
        ExprKind::Binary { op, lhs, rhs } => check_binary(
            *op,
            lhs,
            rhs,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
            span,
        ),
        ExprKind::Call {
            callee,
            type_args,
            args,
            ..
        } => check_call(
            expr,
            callee,
            type_args,
            args,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        ),
        ExprKind::MethodCall {
            receiver,
            method,
            type_args,
            args,
            ..
        } => check_method_call(
            expr,
            receiver,
            method,
            type_args,
            args,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        ),
        ExprKind::FieldAccess { target, field } => check_field_access(
            expr,
            target,
            field,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        ),
        ExprKind::TupleIndex { target, index } => check_tuple_index(
            target,
            *index,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
            span,
        ),
        ExprKind::Index { target, index } => check_index(
            target,
            index,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
            span,
        ),
        ExprKind::Question { target } => check_question(
            target,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
            span,
        ),
        ExprKind::Pipe(pipe) => {
            check_pipe(pipe, expected, ret_ctx, env, program, effects, diagnostics)
        }
        ExprKind::Lambda { params, body } => {
            check_lambda(params, body, expected, env, program, diagnostics)
        }
        ExprKind::If(if_expr) => check_if_expr(
            if_expr,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        ),
        ExprKind::Match { scrutinee, arms } => check_match_expr(
            expr,
            scrutinee,
            arms,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        ),
        ExprKind::Par { kind, elements } => {
            check_par(kind, elements, ret_ctx, env, program, effects, diagnostics)
        }
        ExprKind::Grouping(inner) => {
            check_expr(inner, expected, ret_ctx, env, program, effects, diagnostics)
        }
    }
}

enum SeqKind {
    List,
    Set,
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives all the context needed to check an empty/non-empty collection literal (env/program/effects/diagnostics, etc.). Consolidating into a Ck struct was passed on as an inconsistent, half-done introduction relative to this file's other check_* function family (judgment call made in this file)"
)]
fn check_seq_lit(
    elements: &[Expr],
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
    span: Span,
    kind: SeqKind,
) -> Ty {
    let expected_elem = match (expected, &kind) {
        (Some(Ty::List(t)), SeqKind::List) | (Some(Ty::Set(t)), SeqKind::Set) => {
            Some((**t).clone())
        }
        _ => None,
    };
    if elements.is_empty() {
        let expected_whole = match &kind {
            SeqKind::List => expected.filter(|t| matches!(t, Ty::List(_))),
            SeqKind::Set => expected.filter(|t| matches!(t, Ty::Set(_))),
        };
        let resolved = infer::infer_with_expected(expected_whole, span, diagnostics);
        return resolved;
    }
    let mut unified: Option<Ty> = expected_elem.clone();
    let mut elem_tys = Vec::with_capacity(elements.len());
    for e in elements {
        let ty = check_expr(
            e,
            expected_elem.as_ref().or(unified.as_ref()),
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        elem_tys.push((ty, e.span));
    }
    for (ty, elem_span) in elem_tys {
        match &unified {
            None => unified = Some(ty),
            Some(u) => match infer::unify(u, &ty) {
                Some(merged) => unified = Some(merged),
                None => {
                    diagnostics.push(Diagnostic {
                        code: ErrorCode::CollectionElementTypeMismatch,
                        span: elem_span,
                        message: "cannot unify the collection's element types (D-TYPE-04)"
                            .to_owned(),
                    });
                }
            },
        }
    }
    let elem_ty = unified.unwrap_or(Ty::Unknown);
    match kind {
        SeqKind::List => t_list(elem_ty),
        SeqKind::Set => {
            if !is_allowed_key_type(&elem_ty) && !matches!(elem_ty, Ty::Unknown) {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::SetElementTypeNotAllowed,
                    span,
                    message: "this type is not allowed as a set element type (D-TYPE-05)"
                        .to_owned(),
                });
            }
            t_set(elem_ty)
        }
    }
}

fn is_allowed_key_type(ty: &Ty) -> bool {
    match ty {
        Ty::Int | Ty::Bool | Ty::Str => true,
        Ty::Tuple(items) => items.iter().all(is_allowed_key_type),
        _ => false,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed for D-TYPE-04 unification checking of a dict literal's keys and values respectively"
)]
fn check_dict_lit(
    entries: &[(Expr, Expr)],
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
    span: Span,
) -> Ty {
    let expected_kv = match expected {
        Some(Ty::Dict(k, v)) => Some(((**k).clone(), (**v).clone())),
        _ => None,
    };
    if entries.is_empty() {
        let resolved = infer::infer_with_expected(
            expected.filter(|t| matches!(t, Ty::Dict(..))),
            span,
            diagnostics,
        );
        if let Ty::Dict(key, _) = &resolved
            && !is_allowed_key_type(key)
            && !matches!(**key, Ty::Unknown)
        {
            diagnostics.push(Diagnostic {
                code: ErrorCode::DictKeyTypeNotAllowed,
                span,
                message: "this type is not allowed as a dictionary key".to_owned(),
            });
        }
        return resolved;
    }
    let mut unified_k: Option<Ty> = expected_kv.as_ref().map(|(k, _)| k.clone());
    let mut unified_v: Option<Ty> = expected_kv.as_ref().map(|(_, v)| v.clone());
    for (k, v) in entries {
        let k_expected = unified_k.clone();
        let k_ty = check_expr(
            k,
            k_expected.as_ref(),
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        match &unified_k {
            None => unified_k = Some(k_ty),
            Some(u) => match infer::unify(u, &k_ty) {
                Some(merged) => unified_k = Some(merged),
                None => diagnostics.push(Diagnostic {
                    code: ErrorCode::CollectionElementTypeMismatch,
                    span: k.span,
                    message: "cannot unify the dict's key types (D-TYPE-04)".to_owned(),
                }),
            },
        }
        let v_expected = unified_v.clone();
        let v_ty = check_expr(
            v,
            v_expected.as_ref(),
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        match &unified_v {
            None => unified_v = Some(v_ty),
            Some(u) => match infer::unify(u, &v_ty) {
                Some(merged) => unified_v = Some(merged),
                None => diagnostics.push(Diagnostic {
                    code: ErrorCode::CollectionElementTypeMismatch,
                    span: v.span,
                    message: "cannot unify the dict's value types (D-TYPE-04)".to_owned(),
                }),
            },
        }
    }
    let k_ty = unified_k.unwrap_or(Ty::Unknown);
    let v_ty = unified_v.unwrap_or(Ty::Unknown);
    if !is_allowed_key_type(&k_ty) && !matches!(k_ty, Ty::Unknown) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::DictKeyTypeNotAllowed,
            span,
            message: "this type is not allowed as a dict key type (D-TYPE-05)".to_owned(),
        });
    }
    t_dict(k_ty, v_ty)
}

fn check_ident(
    name: &Arc<str>,
    span: Span,
    expected: Option<&Ty>,
    env: &TypeEnv,
    program: &Program,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if let Some(binding) = env.lookup(name.as_ref()) {
        return binding.ty.clone();
    }
    if let Some((enum_name, generics)) = find_builtin_unit_variant(name) {
        return resolve_unit_variant_ty(enum_name, generics.len(), expected, span, diagnostics).1;
    }
    for e in program.enums.values() {
        if let Some(v) = e.variants.iter().find(|v| v.name.as_ref() == name.as_ref())
            && v.fields.is_empty()
        {
            return resolve_unit_variant_ty(
                e.name.as_ref(),
                e.generics.len(),
                expected,
                span,
                diagnostics,
            )
            .1;
        }
    }
    if let Some(f) = program.functions.get(name.as_ref()) {
        // The case of referencing a top-level function name as a value (a simple case
        // unrelated to D-FUNC-04 -- since no syntax passing a generic function itself as
        // a value appears in samples/, a function with generics simply returns a type
        // still mixed with `Ty::TypeVar` as-is -- judgment call made in this file).
        let params = f
            .params
            .iter()
            .map(|p| ty_from_ann(&p.ty, &f.generics, program).unwrap_or(Ty::Unknown))
            .collect();
        let ret = ty_from_ann(&f.ret, &f.generics, program).unwrap_or(Ty::Unknown);
        let mut effect_set = EffectSet::empty();
        for e in &f.effects {
            if let Some(bit) = EffectSet::from_name(e) {
                effect_set = effect_set.union(bit);
            }
        }
        return Ty::Function {
            params,
            effects: effect_set,
            ret: Box::new(ret),
        };
    }
    if let Some(v) = program.consts.get(name.as_ref()) {
        return ty_of_const_value(v);
    }
    // Undefined identifier: since DECISIONS.md has no dedicated diagnostic code for this
    // (a known limitation), this reuses E1003 and returns the recovery placeholder
    // Ty::Unknown to avoid a diagnostic cascade.
    diagnostics.push(Diagnostic {
        code: ErrorCode::UninferableType,
        span,
        message: format!("undefined identifier '{name}'"),
    });
    Ty::Unknown
}

fn resolve_unit_variant_ty(
    enum_name: &str,
    generics_arity: usize,
    expected: Option<&Ty>,
    span: Span,
    diagnostics: &mut DiagnosticBag,
) -> (Arc<str>, Ty) {
    let name: Arc<str> = Arc::from(enum_name);
    if generics_arity == 0 {
        return (
            name.clone(),
            Ty::Named {
                name,
                args: Vec::new(),
            },
        );
    }
    match expected {
        Some(Ty::Named { name: en, args })
            if en.as_ref() == enum_name && args.len() == generics_arity =>
        {
            (
                name,
                Ty::Named {
                    name: en.clone(),
                    args: args.clone(),
                },
            )
        }
        _ => {
            diagnostics.push(Diagnostic {
                code: ErrorCode::UninferableType,
                span,
                message: format!(
                    "cannot infer the type argument for {enum_name} (add a type annotation)"
                ),
            });
            (name, Ty::Unknown)
        }
    }
}

/// "None" is the unit variant of the builtin enum `Option[T]`. It is absent from the
/// list of user-defined enums (`program.enums`) because prelude is unimplemented -- this
/// helper special-cases it. The return value is `(enum name, its list of type-parameter
/// names)` -- since `Option[T]` has 1 type parameter, `generics.len()==1` is passed to
/// the caller (`resolve_unit_variant_ty`).
fn find_builtin_unit_variant(name: &str) -> Option<(&'static str, &'static [&'static str])> {
    match name {
        "None" => Some(("Option", &["T"])),
        "Null" => Some(("Value", &[])),
        _ => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check construction of a non-unit variant of the builtin enum Value (D-TYPE-10)"
)]
fn check_value_variant_ctor(
    name: &str,
    args: &[Arg],
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let param_ty = match name {
        "Bool" => Ty::Bool,
        "Int" => Ty::Int,
        "Float" => Ty::Float,
        "Str" => Ty::Str,
        "List" => t_list(t_value()),
        "Dict" => t_dict(Ty::Str, t_value()),
        _ => unreachable!("the caller guarantees this is a non-unit variant name of Value"),
    };
    for a in args {
        if a.name.is_some() {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: a.value.span,
                message: "enum variant construction accepts only positional arguments (D-SYN-07)"
                    .to_owned(),
            });
        }
    }
    if args.len() != 1 {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: call_span,
            message: format!("Value::{name} takes exactly 1 positional argument (D-TYPE-10)"),
        });
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
        return t_value();
    }
    let arg_ty = check_expr(
        &args[0].value,
        Some(&param_ty),
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    if infer::unify(&param_ty, &arg_ty).is_none() && !matches!(arg_ty, Ty::Unknown) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: args[0].value.span,
            message: format!("the type of Value::{name}'s argument does not match (D-TYPE-10)"),
        });
    }
    t_value()
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check named-argument construction of a builtin struct (Error/Response/HttpOptions/ProcOutput)"
)]
fn check_builtin_struct_init(
    struct_name: &str,
    fields: &[(&'static str, Ty)],
    args: &[Arg],
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let mut seen: HashSet<&str> = HashSet::new();
    for arg in args {
        let Some(field_name) = &arg.name else {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg.value.span,
                message: "struct construction requires named arguments (D-TYPE-13)".to_owned(),
            });
            check_expr(
                &arg.value,
                None,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            continue;
        };
        let Some((_, field_ty)) = fields.iter().find(|(n, _)| *n == field_name.as_ref()) else {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg.value.span,
                message: format!("'{struct_name}' has no field '{field_name}'"),
            });
            check_expr(
                &arg.value,
                None,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            continue;
        };
        seen.insert(field_name.as_ref());
        let value_ty = check_expr(
            &arg.value,
            Some(field_ty),
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        if infer::unify(field_ty, &value_ty).is_none() && !matches!(value_ty, Ty::Unknown) {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg.value.span,
                message: format!("the type of field '{field_name}' does not match"),
            });
        }
    }
    for (fname, _) in fields {
        if !seen.contains(fname) {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: call_span,
                message: format!("field '{fname}' was not specified"),
            });
        }
    }
    Ty::Named {
        name: Arc::from(struct_name),
        args: Vec::new(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check a unary operator (env/program/effects/diagnostics, etc.)"
)]
fn check_unary(
    op: UnaryOp,
    operand: &Expr,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
    span: Span,
) -> Ty {
    let operand_ty = check_expr(operand, None, ret_ctx, env, program, effects, diagnostics);
    match op {
        UnaryOp::Not => {
            if !matches!(operand_ty, Ty::Bool | Ty::Unknown) {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span,
                    message: "unary not requires a bool operand".to_owned(),
                });
            }
            Ty::Bool
        }
        UnaryOp::Neg => {
            if let Ty::TypeVar(_) = &operand_ty {
                // Equivalent to D-FUNC-05: unary `-` is also an operator defined only for specific concrete types (int/float).
                diagnostics.push(Diagnostic {
                    code: ErrorCode::UnsupportedOperatorForTypeParam,
                    span,
                    message: "an unconstrained type parameter cannot use unary - ".to_owned(),
                });
                return Ty::Unknown;
            }
            match &operand_ty {
                Ty::Int => Ty::Int,
                Ty::Float => Ty::Float,
                Ty::Unknown => Ty::Unknown,
                _ => {
                    diagnostics.push(Diagnostic {
                        code: ErrorCode::BranchTypeMismatch,
                        span,
                        message: "unary - can only be used on int/float".to_owned(),
                    });
                    Ty::Unknown
                }
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check a binary operator (env/program/effects/diagnostics, etc.)"
)]
fn check_binary(
    op: BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
    span: Span,
) -> Ty {
    let lhs_ty = check_expr(lhs, None, ret_ctx, env, program, effects, diagnostics);
    let rhs_ty = check_expr(rhs, None, ret_ctx, env, program, effects, diagnostics);

    let lhs_is_tv = matches!(lhs_ty, Ty::TypeVar(_));
    let rhs_is_tv = matches!(rhs_ty, Ty::TypeVar(_));
    if lhs_is_tv || rhs_is_tv {
        // Even when both operands are type variables, only 1 diagnostic is pushed (in
        // the spirit of D-CLI-03 -- do not double-report the same root cause).
        let representative = if lhs_is_tv { &lhs_ty } else { &rhs_ty };
        if !generics::check_type_param_operator_usage(representative, op, span, diagnostics) {
            return Ty::Unknown;
        }
        // D-OP-06/D-FUNC-05: ==/!= are always permitted even on an unconstrained type parameter.
        if matches!(op, BinaryOp::EqEq | BinaryOp::NotEq) {
            return Ty::Bool;
        }
        return Ty::Unknown;
    }
    if matches!(lhs_ty, Ty::Unknown) || matches!(rhs_ty, Ty::Unknown) {
        return match op {
            BinaryOp::Lt
            | BinaryOp::LtEq
            | BinaryOp::Gt
            | BinaryOp::GtEq
            | BinaryOp::EqEq
            | BinaryOp::NotEq
            | BinaryOp::And
            | BinaryOp::Or => Ty::Bool,
            _ => Ty::Unknown,
        };
    }

    match op {
        BinaryOp::Add => check_add(&lhs_ty, &rhs_ty, span, diagnostics),
        BinaryOp::Sub | BinaryOp::Mul => check_arith(&lhs_ty, &rhs_ty, span, diagnostics),
        BinaryOp::Div => check_div(&lhs_ty, &rhs_ty, span, diagnostics),
        BinaryOp::Mod => check_mod(&lhs_ty, &rhs_ty, span, diagnostics),
        BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => {
            check_ordering(&lhs_ty, &rhs_ty, span, diagnostics)
        }
        BinaryOp::EqEq | BinaryOp::NotEq => {
            if infer::unify(&lhs_ty, &rhs_ty).is_some() {
                Ty::Bool
            } else {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span,
                    message: "the types on both sides of == / != do not match".to_owned(),
                });
                Ty::Bool
            }
        }
        BinaryOp::And | BinaryOp::Or => {
            if matches!(lhs_ty, Ty::Bool) && matches!(rhs_ty, Ty::Bool) {
                Ty::Bool
            } else {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span,
                    message: "and / or can only be used on the bool type".to_owned(),
                });
                Ty::Bool
            }
        }
    }
}

fn is_int_float_mix(a: &Ty, b: &Ty) -> bool {
    matches!((a, b), (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int))
}

fn check_add(a: &Ty, b: &Ty, span: Span, diagnostics: &mut DiagnosticBag) -> Ty {
    if is_int_float_mix(a, b) {
        push_int_float_mixed(span, diagnostics);
        return Ty::Unknown;
    }
    match (a, b) {
        (Ty::Int, Ty::Int) => Ty::Int,
        (Ty::Float, Ty::Float) => Ty::Float,
        (Ty::Str, Ty::Str) => Ty::Str,
        (Ty::List(x), Ty::List(y)) => {
            if let Some(elem) = infer::unify(x, y) {
                t_list(elem)
            } else {
                push_type_mismatch(
                    span,
                    diagnostics,
                    "the list element types on both sides of + do not match",
                );
                Ty::Unknown
            }
        }
        _ => {
            push_type_mismatch(
                span,
                diagnostics,
                "this combination of types is not supported by + (D-OP-07)",
            );
            Ty::Unknown
        }
    }
}

fn check_arith(a: &Ty, b: &Ty, span: Span, diagnostics: &mut DiagnosticBag) -> Ty {
    if is_int_float_mix(a, b) {
        push_int_float_mixed(span, diagnostics);
        return Ty::Unknown;
    }
    match (a, b) {
        (Ty::Int, Ty::Int) => Ty::Int,
        (Ty::Float, Ty::Float) => Ty::Float,
        _ => {
            push_type_mismatch(
                span,
                diagnostics,
                "this operator can only be used on int/float",
            );
            Ty::Unknown
        }
    }
}

fn check_div(a: &Ty, b: &Ty, span: Span, diagnostics: &mut DiagnosticBag) -> Ty {
    if is_int_float_mix(a, b) {
        push_int_float_mixed(span, diagnostics);
        return Ty::Unknown;
    }
    match (a, b) {
        (Ty::Int, Ty::Int) => Ty::Int,
        (Ty::Float, Ty::Float) => Ty::Float,
        _ => {
            push_type_mismatch(
                span,
                diagnostics,
                "/ can only be used on int/int or float/float (D-OP-04)",
            );
            Ty::Unknown
        }
    }
}

fn check_mod(a: &Ty, b: &Ty, span: Span, diagnostics: &mut DiagnosticBag) -> Ty {
    if is_int_float_mix(a, b) {
        push_int_float_mixed(span, diagnostics);
        return Ty::Unknown;
    }
    if matches!((a, b), (Ty::Int, Ty::Int)) {
        Ty::Int
    } else {
        push_type_mismatch(span, diagnostics, "% is int-only (D-OP-04)");
        Ty::Unknown
    }
}

fn check_ordering(a: &Ty, b: &Ty, span: Span, diagnostics: &mut DiagnosticBag) -> Ty {
    if is_int_float_mix(a, b) {
        push_int_float_mixed(span, diagnostics);
        return Ty::Bool;
    }
    let orderable = |t: &Ty| matches!(t, Ty::Int | Ty::Float | Ty::Str);
    if orderable(a) && orderable(b) && infer::unify(a, b).is_some() {
        Ty::Bool
    } else {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UnorderableType,
            span,
            message: "an ordering-comparison operator can only be used on int/float/str (D-OP-05)"
                .to_owned(),
        });
        Ty::Bool
    }
}

fn push_int_float_mixed(span: Span, diagnostics: &mut DiagnosticBag) {
    diagnostics.push(Diagnostic {
        code: ErrorCode::IntFloatMixed,
        span,
        message: "int and float cannot be mixed without an explicit conversion (D-OP-03)"
            .to_owned(),
    });
}

pub(crate) fn push_type_mismatch(span: Span, diagnostics: &mut DiagnosticBag, message: &str) {
    diagnostics.push(Diagnostic {
        code: ErrorCode::BranchTypeMismatch,
        span,
        message: message.to_owned(),
    });
}

/// Uniformly checks a stdlib/user-defined function, method, or enum variant with
/// positional arguments (routes through D-FUNC-04's unification foundation).
/// `own_effects` is the callee's own already-declared effect (determined from `uses
/// {..}`, or a fixed value for stdlib) -- always added into `effects`. `forward_fn_effects`
/// is the rule specific to STDLIB higher-order methods (§5.5 "a function-typed argument is
/// unconditionally a forwarding target") -- when true, each function-typed argument's
/// `effects` are also added.
struct CallSig<'a> {
    generics: &'a [Arc<str>],
    params: &'a [Ty],
    ret: &'a Ty,
    own_effects: EffectSet,
    forward_fn_effects: bool,
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check call arguments (D-FUNC-04's unification); consolidated into one place as the common part left after the caller-kind-specific (function/method/enum variant) preprocessing"
)]
fn check_positional_call(
    sig: &CallSig<'_>,
    arg_exprs: &[&Expr],
    explicit_type_args: &[Ty],
    call_span: Span,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    *effects = effects.union(sig.own_effects);
    if sig.params.len() != arg_exprs.len() {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: call_span,
            message: format!(
                "the number of arguments does not match (expected: {}, actual: {})",
                sig.params.len(),
                arg_exprs.len()
            ),
        });
        for a in arg_exprs {
            check_expr(a, None, ret_ctx, env, program, effects, diagnostics);
        }
        return Ty::Unknown;
    }
    let mut subst: HashMap<Arc<str>, Ty> = HashMap::new();
    for (name, ty) in sig.generics.iter().zip(explicit_type_args.iter()) {
        subst.insert(Arc::clone(name), ty.clone());
    }
    let n = arg_exprs.len();
    let is_lambda = |i: usize| matches!(arg_exprs[i].kind, ExprKind::Lambda { .. });
    let mut order: Vec<usize> = (0..n).filter(|&i| !is_lambda(i)).collect();
    order.extend((0..n).filter(|&i| is_lambda(i)));
    for i in order {
        let expected_for_arg = generics::substitute(&sig.params[i], &subst);
        let arg_ty = check_expr(
            arg_exprs[i],
            Some(&expected_for_arg),
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        if sig.forward_fn_effects
            && let Ty::Function { effects: fe, .. } = &arg_ty
        {
            *effects = effects.union(*fe);
        }
        if !generics::unify_collect(&sig.params[i], &arg_ty, &mut subst)
            && !matches!(arg_ty, Ty::Unknown)
        {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg_exprs[i].span,
                message: "the type of the argument does not match".to_owned(),
            });
        }
    }
    generics::finalize_ret(
        sig.ret,
        &mut subst,
        sig.generics,
        expected,
        call_span,
        diagnostics,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to resolve a Call expression (the unified representation of struct construction / enum variant construction / function call / closure call)"
)]
fn check_call(
    call_expr: &Expr,
    callee: &Expr,
    type_args: &[crate::ast::TypeAnn],
    args: &[Arg],
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let call_span = call_expr.span;
    let ExprKind::Ident(name) = &callee.kind else {
        // The callee itself is an expression (e.g. directly calling a closure held in a variable, `(f)(x)`).
        let callee_ty = check_expr(callee, None, ret_ctx, env, program, effects, diagnostics);
        program
            .resolutions
            .call_kind
            .insert(call_expr.id, CallKind::ClosureCall);
        return check_closure_call(
            &callee_ty,
            args,
            call_span,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
    };

    if env.lookup(name.as_ref()).is_some() {
        program
            .resolutions
            .call_kind
            .insert(call_expr.id, CallKind::ClosureCall);
        let callee_ty = check_expr(callee, None, ret_ctx, env, program, effects, diagnostics);
        return check_closure_call(
            &callee_ty,
            args,
            call_span,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
    }

    let explicit_tys: Vec<Ty> = type_args
        .iter()
        .map(|t| ty_from_ann(t, env.generics(), program).unwrap_or(Ty::Unknown))
        .collect();
    check_call_named(
        name,
        call_expr,
        &explicit_tys,
        args,
        expected,
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "every meaning a single bare identifier can take on within the flat namespace \
(D-TYPE-07) -- D-TYPE-14's type-conversion names, D-STDPOL-01's special case (print/eprint/ \
assert), D-TYPE-03's set() pseudo-constructor, the Ok/Err/Some/Value variants, builtin structs, \
and user structs/enums/functions -- must live as one prioritized match in a single place; each \
individual arm already delegates to a check_* helper, and splitting this further would break the \
at-a-glance visibility of the priority order"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "extracted from check_call; receives together all the context needed to resolve a Call expression's bare-identifier callee"
)]
fn check_call_named(
    name: &Arc<str>,
    call_expr: &Expr,
    explicit_tys: &[Ty],
    args: &[Arg],
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let call_span = call_expr.span;

    match name.as_ref() {
        "int" => {
            return check_conversion_call(
                args,
                Ty::Float,
                Ty::Int,
                call_span,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        "float" => {
            return check_conversion_call(
                args,
                Ty::Int,
                Ty::Float,
                call_span,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        "str" => {
            return check_str_conversion(
                args,
                call_span,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        "print" | "eprint" => {
            return check_print_like(args, call_span, ret_ctx, env, program, effects, diagnostics);
        }
        "assert" => {
            return check_assert(args, call_span, ret_ctx, env, program, effects, diagnostics);
        }
        "set" => {
            return check_set_ctor(
                explicit_tys,
                expected,
                call_span,
                args,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        "Ok" | "Err" | "Some" => {
            program
                .resolutions
                .call_kind
                .insert(call_expr.id, CallKind::EnumVariantInit);
            return check_builtin_variant_ctor(
                name.as_ref(),
                args,
                explicit_tys,
                expected,
                call_span,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        "Bool" | "Int" | "Float" | "Str" | "List" | "Dict" => {
            // Construction of a non-unit variant of the builtin enum Value (D-TYPE-10).
            program
                .resolutions
                .call_kind
                .insert(call_expr.id, CallKind::EnumVariantInit);
            return check_value_variant_ctor(
                name.as_ref(),
                args,
                call_span,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        "Error" | "Response" | "HttpOptions" | "ProcOutput" => {
            // A builtin struct (STDLIB.md §3.3/§6; since prelude::install is
            // unimplemented, no entity exists in program.structs, so it is handled
            // directly here).
            program
                .resolutions
                .call_kind
                .insert(call_expr.id, CallKind::StructInit);
            let Some(fields) = builtin_struct_fields(name.as_ref()) else {
                unreachable!(
                    "the preceding match already confirmed this is one of the 4 builtin struct names"
                );
            };
            return check_builtin_struct_init(
                name.as_ref(),
                &fields,
                args,
                call_span,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
        }
        _ => {}
    }

    if let Some(s) = program.structs.get(name.as_ref()).cloned() {
        program
            .resolutions
            .call_kind
            .insert(call_expr.id, CallKind::StructInit);
        return check_struct_init(
            &s,
            args,
            expected,
            call_span,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
    }

    if let Some((enum_decl, variant_fields, variant_generics)) =
        find_enum_variant(program, name.as_ref())
    {
        program
            .resolutions
            .call_kind
            .insert(call_expr.id, CallKind::EnumVariantInit);
        let arg_exprs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        for a in args {
            if a.name.is_some() {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span: a.value.span,
                    message:
                        "enum variant construction accepts only positional arguments (D-SYN-07)"
                            .to_owned(),
                });
            }
        }
        let ret = Ty::Named {
            name: Arc::clone(&enum_decl),
            args: variant_generics.iter().map(|g| tv(g)).collect(),
        };
        let sig = CallSig {
            generics: &variant_generics,
            params: &variant_fields,
            ret: &ret,
            own_effects: EffectSet::empty(),
            forward_fn_effects: false,
        };
        return check_positional_call(
            &sig,
            &arg_exprs,
            explicit_tys,
            call_span,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
    }

    if let Some(f) = program.functions.get(name.as_ref()).cloned() {
        program
            .resolutions
            .call_kind
            .insert(call_expr.id, CallKind::FunctionCall);
        let mut own_effects = EffectSet::empty();
        for e in &f.effects {
            if let Some(bit) = EffectSet::from_name(e) {
                own_effects = own_effects.union(bit);
            }
        }
        let params: Vec<Ty> = f
            .params
            .iter()
            .map(|p| ty_from_ann(&p.ty, &f.generics, program).unwrap_or(Ty::Unknown))
            .collect();
        let ret = ty_from_ann(&f.ret, &f.generics, program).unwrap_or(Ty::Unknown);
        let arg_exprs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        for a in args {
            if a.name.is_some() {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span: a.value.span,
                    message: "a function call accepts only positional arguments (D-TYPE-11)"
                        .to_owned(),
                });
            }
        }
        let sig = CallSig {
            generics: &f.generics,
            params: &params,
            ret: &ret,
            own_effects,
            forward_fn_effects: false,
        };
        return check_positional_call(
            &sig,
            &arg_exprs,
            explicit_tys,
            call_span,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
    }

    diagnostics.push(Diagnostic {
        code: ErrorCode::UninferableType,
        span: call_span,
        message: format!("undefined call target '{name}'"),
    });
    for a in args {
        check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
    }
    Ty::Unknown
}

/// The enum name, the list of the variant's field types, and the enum's own list of generic names.
type EnumVariantInfo = (Arc<str>, Vec<Ty>, Vec<Arc<str>>);

fn find_enum_variant(program: &Program, name: &str) -> Option<EnumVariantInfo> {
    for e in program.enums.values() {
        if let Some(v) = e.variants.iter().find(|v| v.name.as_ref() == name) {
            let fields = v
                .fields
                .iter()
                .map(|t| ty_from_ann(t, &e.generics, program).unwrap_or(Ty::Unknown))
                .collect();
            return Some((Arc::clone(&e.name), fields, e.generics.clone()));
        }
    }
    None
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check a closure call (calling a function value bound to a local variable)"
)]
fn check_closure_call(
    callee_ty: &Ty,
    args: &[Arg],
    call_span: Span,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    for a in args {
        if a.name.is_some() {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: a.value.span,
                message: "a closure call accepts only positional arguments (D-TYPE-11)".to_owned(),
            });
        }
    }
    let arg_exprs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
    match callee_ty {
        Ty::Function { params, ret, .. } => {
            let sig = CallSig {
                generics: &[],
                params,
                ret,
                own_effects: EffectSet::empty(),
                forward_fn_effects: false,
            };
            check_positional_call(
                &sig,
                &arg_exprs,
                &[],
                call_span,
                expected,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            )
        }
        Ty::Unknown => {
            for a in &arg_exprs {
                check_expr(a, None, ret_ctx, env, program, effects, diagnostics);
            }
            Ty::Unknown
        }
        _ => {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: call_span,
                message: "this is not a callable type".to_owned(),
            });
            for a in &arg_exprs {
                check_expr(a, None, ret_ctx, env, program, effects, diagnostics);
            }
            Ty::Unknown
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check an int(x)/float(x) conversion call (D-TYPE-14)"
)]
fn check_conversion_call(
    args: &[Arg],
    expected_input: Ty,
    output: Ty,
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if args.len() != 1 || args[0].name.is_some() {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: call_span,
            message: "a type-conversion call takes exactly 1 positional argument (D-TYPE-14)"
                .to_owned(),
        });
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
        return output;
    }
    let arg_ty = check_expr(
        &args[0].value,
        Some(&expected_input),
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    validate_conversion_arg_ty(&arg_ty, &expected_input, args[0].value.span, diagnostics);
    output
}

/// The core `int(x)`/`float(x)` argument-type validation (D-TYPE-14) shared by both
/// [`check_conversion_call`] (direct call) and [`check_pipe_stdpol01_overload`] (via a
/// pipe).
fn validate_conversion_arg_ty(
    arg_ty: &Ty,
    expected_input: &Ty,
    span: Span,
    diagnostics: &mut DiagnosticBag,
) {
    if infer::unify(arg_ty, expected_input).is_none() && !matches!(arg_ty, Ty::Unknown) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span,
            message: "the argument type of the type-conversion call does not match (D-TYPE-14)"
                .to_owned(),
        });
    }
}

fn check_str_conversion(
    args: &[Arg],
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if args.len() != 1 || args[0].name.is_some() {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: call_span,
            message: "str(x) takes exactly 1 positional argument (D-TYPE-14)".to_owned(),
        });
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
        return Ty::Str;
    }
    let arg_ty = check_expr(
        &args[0].value,
        None,
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    validate_str_conversion_arg_ty(&arg_ty, args[0].value.span, diagnostics);
    Ty::Str
}

/// The core `str(x)` argument-type validation (D-STDPOL-01) shared by both
/// [`check_str_conversion`] (direct call) and [`check_pipe_stdpol01_overload`] (via a
/// pipe).
fn validate_str_conversion_arg_ty(arg_ty: &Ty, span: Span, diagnostics: &mut DiagnosticBag) {
    if !matches!(arg_ty, Ty::Int | Ty::Float | Ty::Bool | Ty::Unknown) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span,
            message: "str(x) only accepts int/float/bool (D-STDPOL-01)".to_owned(),
        });
    }
}

fn check_print_like(
    args: &[Arg],
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if args.len() != 1 || args[0].name.is_some() {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: call_span,
            message: "print/eprint takes exactly 1 positional argument (D-STDPOL-01)".to_owned(),
        });
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
        return Ty::Void;
    }
    let arg_ty = check_expr(
        &args[0].value,
        None,
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    validate_print_like_arg_ty(&arg_ty, args[0].value.span, diagnostics);
    Ty::Void
}

/// The core `print`/`eprint` argument-type validation (D-STDPOL-01) shared by both
/// [`check_print_like`] (direct call) and [`check_pipe_stdpol01_overload`] (via a pipe).
fn validate_print_like_arg_ty(arg_ty: &Ty, span: Span, diagnostics: &mut DiagnosticBag) {
    if !matches!(
        arg_ty,
        Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Unknown
    ) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span,
            message: "print/eprint only accepts str/int/float/bool (D-STDPOL-01)".to_owned(),
        });
    }
}

fn check_assert(
    args: &[Arg],
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if args.is_empty() || args.len() > 2 || args.iter().any(|a| a.name.is_some()) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: call_span,
            message: "assert takes the form assert(cond) or assert(cond, msg) (D-STDPOL-01)"
                .to_owned(),
        });
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
        return Ty::Void;
    }
    let cond_ty = check_expr(
        &args[0].value,
        Some(&Ty::Bool),
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    validate_assert_cond_ty(&cond_ty, args[0].value.span, diagnostics);
    if let Some(msg) = args.get(1) {
        let msg_ty = check_expr(
            &msg.value,
            Some(&Ty::Str),
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        validate_assert_msg_ty(&msg_ty, msg.value.span, diagnostics);
    }
    Ty::Void
}

/// The core validation (D-STDPOL-01) of `assert`'s first argument (cond: bool) shared by
/// both [`check_assert`] (direct call) and [`check_pipe_stdpol01_overload`] (via a pipe).
fn validate_assert_cond_ty(cond_ty: &Ty, span: Span, diagnostics: &mut DiagnosticBag) {
    if !matches!(cond_ty, Ty::Bool | Ty::Unknown) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span,
            message: "assert's first argument must be bool".to_owned(),
        });
    }
}

/// The core validation of `assert`'s second argument (msg: str) (D-STDPOL-01). Shared by
/// both the direct call and the pipe path, for the same reason as
/// [`validate_assert_cond_ty`].
fn validate_assert_msg_ty(msg_ty: &Ty, span: Span, diagnostics: &mut DiagnosticBag) {
    if !matches!(msg_ty, Ty::Str | Ty::Unknown) {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span,
            message: "assert's second argument must be str".to_owned(),
        });
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check the set()/set[T]() pseudo-constructor (D-TYPE-03)"
)]
fn check_set_ctor(
    explicit_tys: &[Ty],
    expected: Option<&Ty>,
    call_span: Span,
    args: &[Arg],
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if !args.is_empty() {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: call_span,
            message: "set() takes no arguments (D-TYPE-03)".to_owned(),
        });
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
    }
    let result = if let Some(element) = explicit_tys.first() {
        t_set(element.clone())
    } else {
        infer::infer_with_expected(
            expected.filter(|ty| matches!(ty, Ty::Set(_))),
            call_span,
            diagnostics,
        )
    };
    if let Ty::Set(element) = &result
        && !is_allowed_key_type(element)
        && !matches!(**element, Ty::Unknown)
    {
        diagnostics.push(Diagnostic {
            code: ErrorCode::SetElementTypeNotAllowed,
            span: call_span,
            message: "this type is not allowed as a set element".to_owned(),
        });
    }
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check Ok/Err/Some (construction of a variant of the builtin enum Result/Option)"
)]
fn check_builtin_variant_ctor(
    name: &str,
    args: &[Arg],
    explicit_tys: &[Ty],
    expected: Option<&Ty>,
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    for a in args {
        if a.name.is_some() {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: a.value.span,
                message: "enum variant construction accepts only positional arguments (D-SYN-07)"
                    .to_owned(),
            });
        }
    }
    let arg_exprs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
    let generics_te: Vec<Arc<str>> = vec![Arc::from("$T"), Arc::from("$E")];
    let generics_t: Vec<Arc<str>> = vec![Arc::from("$T")];
    match name {
        "Ok" => {
            let params = [tv("$T")];
            let ret = t_result(tv("$T"), tv("$E"));
            let sig = CallSig {
                generics: &generics_te,
                params: &params,
                ret: &ret,
                own_effects: EffectSet::empty(),
                forward_fn_effects: false,
            };
            check_positional_call(
                &sig,
                &arg_exprs,
                explicit_tys,
                call_span,
                expected,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            )
        }
        "Err" => {
            let params = [tv("$E")];
            let ret = t_result(tv("$T"), tv("$E"));
            let sig = CallSig {
                generics: &generics_te,
                params: &params,
                ret: &ret,
                own_effects: EffectSet::empty(),
                forward_fn_effects: false,
            };
            check_positional_call(
                &sig,
                &arg_exprs,
                explicit_tys,
                call_span,
                expected,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            )
        }
        _ => {
            let params = [tv("$T")];
            let ret = t_option(tv("$T"));
            let sig = CallSig {
                generics: &generics_t,
                params: &params,
                ret: &ret,
                own_effects: EffectSet::empty(),
                forward_fn_effects: false,
            };
            check_positional_call(
                &sig,
                &arg_exprs,
                explicit_tys,
                call_span,
                expected,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            )
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed for struct construction (including D-TYPE-13's named-arguments-required check)"
)]
fn check_struct_init(
    decl: &crate::ast::StructDecl,
    args: &[Arg],
    expected: Option<&Ty>,
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let mut subst: HashMap<Arc<str>, Ty> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let is_lambda = |i: usize| matches!(args[i].value.kind, ExprKind::Lambda { .. });
    let mut order: Vec<usize> = (0..args.len()).filter(|&i| !is_lambda(i)).collect();
    order.extend((0..args.len()).filter(|&i| is_lambda(i)));
    for i in order {
        let arg = &args[i];
        let Some(field_name) = &arg.name else {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg.value.span,
                message: "struct construction requires named arguments (D-TYPE-13)".to_owned(),
            });
            check_expr(
                &arg.value,
                None,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            continue;
        };
        let Some((idx, field)) = decl
            .fields
            .iter()
            .enumerate()
            .find(|(_, f)| f.name.as_ref() == field_name.as_ref())
        else {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg.value.span,
                message: format!("struct '{}' has no field '{field_name}'", decl.name),
            });
            check_expr(
                &arg.value,
                None,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            continue;
        };
        if !seen.insert(field_name.to_string()) {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg.value.span,
                message: format!("field '{field_name}' is duplicated"),
            });
        }
        program
            .resolutions
            .field_index
            .insert(arg.value.id, u32::try_from(idx).unwrap_or(0));
        let pat_ty = ty_from_ann(&field.ty, &decl.generics, program).unwrap_or(Ty::Unknown);
        let expected_for_arg = generics::substitute(&pat_ty, &subst);
        let arg_ty = check_expr(
            &arg.value,
            Some(&expected_for_arg),
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        if !generics::unify_collect(&pat_ty, &arg_ty, &mut subst) && !matches!(arg_ty, Ty::Unknown)
        {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg.value.span,
                message: format!("the type of field '{field_name}' does not match"),
            });
        }
    }
    for f in &decl.fields {
        if !seen.contains(f.name.as_ref()) {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: call_span,
                message: format!("field '{}' was not specified", f.name),
            });
        }
    }
    let declared_ret = Ty::Named {
        name: Arc::clone(&decl.name),
        args: decl.generics.iter().map(|g| tv(g)).collect(),
    };
    generics::finalize_ret(
        &declared_ret,
        &mut subst,
        &decl.generics,
        expected,
        call_span,
        diagnostics,
    )
}

fn is_toml_encodable_root(ty: &Ty, program: &Program) -> bool {
    match ty {
        Ty::Dict(key, _) => matches!(**key, Ty::Str),
        Ty::List(element) => is_toml_encodable_root(element, program),
        Ty::Named { name, .. } => {
            program.structs.contains_key(name.as_ref())
                || builtin_struct_fields(name.as_ref()).is_some()
        }
        Ty::Unknown => true,
        _ => false,
    }
}

fn csv_element_type<'ty>(
    method: &str,
    argument: Option<&'ty Ty>,
    result: &'ty Ty,
) -> Option<&'ty Ty> {
    match method {
        "encode" => match argument {
            Some(Ty::List(element)) => Some(element),
            _ => None,
        },
        "decode" => match result {
            Ty::Named { name, args }
                if name.as_ref() == "Result" && matches!(args.first(), Some(Ty::List(_))) =>
            {
                let Some(Ty::List(element)) = args.first() else {
                    unreachable!()
                };
                Some(element)
            }
            _ => None,
        },
        _ => None,
    }
}

fn is_csv_flat_struct(ty: &Ty, program: &Program) -> bool {
    let Ty::Named { name, args } = ty else {
        return matches!(ty, Ty::Unknown);
    };
    let Some(declaration) = program.structs.get(name.as_ref()) else {
        return false;
    };
    let substitution: HashMap<Arc<str>, Ty> = declaration
        .generics
        .iter()
        .cloned()
        .zip(args.iter().cloned())
        .collect();
    declaration.fields.iter().all(|field| {
        let field_ty = ty_from_ann(&field.ty, &declaration.generics, program)
            .map(|field_ty| generics::substitute(&field_ty, &substitution));
        matches!(field_ty, Some(Ty::Int | Ty::Float | Ty::Bool | Ty::Str))
    })
}

fn validate_csv_type(
    namespace: NamespaceId,
    method: &str,
    argument: Option<&Ty>,
    result: &Ty,
    span: Span,
    program: &Program,
    diagnostics: &mut DiagnosticBag,
) {
    if namespace == NamespaceId::Csv
        && let Some(element) = csv_element_type(method, argument, result)
        && !is_csv_flat_struct(element, program)
    {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span,
            message: "CSV encode/decode requires a flat struct with primitive fields".to_owned(),
        });
    }
}

/// Checks a single stdlib (namespace function) call from an already-determined list of
/// argument types (a lightweight version dedicated to a call via a pipe -- the position
/// of the `_` placeholder has already been replaced by the caller with the type of the
/// piped value, and no recursive expression checking such as expected-type hints for
/// lambda arguments is needed, so this can be simpler than `check_positional_call`).
fn check_typed_args_call(
    sig: &Sig,
    arg_tys: &[Ty],
    arg_spans: &[Span],
    call_span: Span,
    expected: Option<&Ty>,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    *effects = effects.union(sig.effects);
    if sig.params.len() != arg_tys.len() {
        diagnostics.push(Diagnostic {
            code: ErrorCode::BranchTypeMismatch,
            span: call_span,
            message: "the number of arguments to the pipe target does not match".to_owned(),
        });
        return Ty::Unknown;
    }
    let mut subst: HashMap<Arc<str>, Ty> = HashMap::new();
    for (i, (pt, at)) in sig.params.iter().zip(arg_tys.iter()).enumerate() {
        if sig.forward_fn_effects
            && let Ty::Function { effects: fe, .. } = at
        {
            *effects = effects.union(*fe);
        }
        if !generics::unify_collect(pt, at, &mut subst) && !matches!(at, Ty::Unknown) {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: arg_spans[i],
                message: "the type of an argument to the pipe target does not match".to_owned(),
            });
        }
    }
    generics::finalize_ret(
        &sig.ret,
        &mut subst,
        &sig.generics,
        expected,
        call_span,
        diagnostics,
    )
}

fn check_pipe(
    pipe: &crate::ast::PipeExpr,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let mut current = check_expr(
        &pipe.source,
        None,
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    let last = pipe.stages.len().saturating_sub(1);
    for (index, stage) in pipe.stages.iter().enumerate() {
        let stage_expected = if index == last {
            if stage.question {
                expected.map(|expected| t_result(expected.clone(), t_error()))
            } else {
                expected.cloned()
            }
        } else {
            None
        };
        current = check_pipe_stage(
            stage,
            &current,
            stage_expected.as_ref(),
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        if stage.question {
            current = unwrap_question(&current, ret_ctx, stage.span, diagnostics);
        }
    }
    current
}

/// When a pipe stage's bare-name callee (a unary call such as `x |> str`, SPEC §6.3
/// "a unary function may be a bare name") matches one of the names subject to D-STDPOL-
/// 01's stdlib-only overload (`int`/`float`/`str`/`print`/`eprint`/`assert`), determines
/// it by reusing the same argument-type validation (the `validate_*_arg_ty` family) that
/// the direct call (`check_call_named`) uses. Because `program.functions` holds only a
/// placeholder dedicated to E1001 collision detection, with a fixed `void` return type
/// (see the comment on `stdlib::prelude::install`), directly deferring these names to
/// `check_pipe_stage`'s `program.functions` fallback would incorrectly always return
/// `void` regardless of the actual argument types (breaking the type the pipe's next
/// stage receives) -- calling this function before consulting `program.functions` avoids
/// that. `Some(return type)` indicates this name was resolved as a D-STDPOL-01 special
/// case; `None` means this name is not a special case, so it proceeds to normal
/// `program.functions` resolution (a user-defined function).
///
/// `set`/`Ok`/`Err`/`Some` are excluded: `check_set_ctor`/`check_builtin_variant_ctor`
/// infer generic type arguments not only from `piped_ty` but also from the original
/// argument expression (`Arg`), so they do not fit naturally into this function's shape
/// of receiving only types. `set` is a 0-argument pseudo-constructor (D-TYPE-03) to begin
/// with and can never be the target of a unary pipe. Currently no case in samples/ uses
/// these 4 names as a pipe target.
fn check_pipe_stdpol01_overload(
    name: &str,
    arg_tys: &[Ty],
    arg_spans: &[Span],
    call_span: Span,
    diagnostics: &mut DiagnosticBag,
) -> Option<Ty> {
    match name {
        "int" | "float" => {
            let (expected_input, output) = if name == "int" {
                (Ty::Float, Ty::Int)
            } else {
                (Ty::Int, Ty::Float)
            };
            if arg_tys.len() != 1 {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span: call_span,
                    message:
                        "a type-conversion call takes exactly 1 positional argument (D-TYPE-14)"
                            .to_owned(),
                });
                return Some(output);
            }
            validate_conversion_arg_ty(&arg_tys[0], &expected_input, arg_spans[0], diagnostics);
            Some(output)
        }
        "str" => {
            if arg_tys.len() != 1 {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span: call_span,
                    message: "str(x) takes exactly 1 positional argument (D-TYPE-14)".to_owned(),
                });
                return Some(Ty::Str);
            }
            validate_str_conversion_arg_ty(&arg_tys[0], arg_spans[0], diagnostics);
            Some(Ty::Str)
        }
        "print" | "eprint" => {
            if arg_tys.len() != 1 {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span: call_span,
                    message: "print/eprint takes exactly 1 positional argument (D-STDPOL-01)"
                        .to_owned(),
                });
                return Some(Ty::Void);
            }
            validate_print_like_arg_ty(&arg_tys[0], arg_spans[0], diagnostics);
            Some(Ty::Void)
        }
        "assert" => {
            if arg_tys.is_empty() || arg_tys.len() > 2 {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span: call_span,
                    message:
                        "assert takes the form assert(cond) or assert(cond, msg) (D-STDPOL-01)"
                            .to_owned(),
                });
                return Some(Ty::Void);
            }
            validate_assert_cond_ty(&arg_tys[0], arg_spans[0], diagnostics);
            if let Some(msg_ty) = arg_tys.get(1) {
                validate_assert_msg_ty(msg_ty, arg_spans[1], diagnostics);
            }
            Some(Ty::Void)
        }
        _ => None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "extracted from check_pipe_stage; receives together all the context needed to \
resolve a namespace-function (`ns.method`) call via a pipe"
)]
fn check_pipe_stage_namespace_call(
    ns: NamespaceId,
    ns_name: &Arc<str>,
    field: &Arc<str>,
    callee_span: Span,
    arg_tys: &[Ty],
    arg_spans: &[Span],
    expected: Option<&Ty>,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if let Some(sig) = namespace_fn_sig(ns, field.as_ref()) {
        let ret = check_typed_args_call(
            &sig,
            arg_tys,
            arg_spans,
            callee_span,
            expected,
            effects,
            diagnostics,
        );
        if ns == NamespaceId::Toml
            && field.as_ref() == "encode"
            && let Some(t0) = arg_tys.first()
            && !is_toml_encodable_root(t0, program)
        {
            diagnostics.push(Diagnostic {
                code: ErrorCode::MissingParamAnnotation,
                span: callee_span,
                message:
                    "toml.encode[T] is valid only when T is dict[str,V] or a struct (D-STDPOL-09)"
                        .to_owned(),
            });
        }
        validate_csv_type(
            ns,
            field,
            arg_tys.first(),
            &ret,
            callee_span,
            program,
            diagnostics,
        );
        return ret;
    }
    diagnostics.push(Diagnostic {
        code: ErrorCode::UninferableType,
        span: callee_span,
        message: format!("undefined namespace function '{ns_name}.{field}'"),
    });
    Ty::Unknown
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "pipe-stage resolution carries the shared type-check context across every target kind"
)]
fn check_pipe_stage(
    stage: &crate::ast::PipeStage,
    piped_ty: &Ty,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let (callee_expr, extra_args): (&Expr, &[Arg]) = match &stage.callee {
        PipeCallee::Bare(e) => (e, &[]),
        PipeCallee::WithArgs { callee, args } => (callee, args.as_slice()),
    };
    let mut arg_tys: Vec<Ty> = Vec::new();
    let mut arg_spans: Vec<Span> = Vec::new();
    if extra_args.is_empty() {
        arg_tys.push(piped_ty.clone());
        arg_spans.push(callee_expr.span);
    } else {
        for a in extra_args {
            if a.is_placeholder {
                arg_tys.push(piped_ty.clone());
            } else {
                arg_tys.push(check_expr(
                    &a.value,
                    None,
                    ret_ctx,
                    env,
                    program,
                    effects,
                    diagnostics,
                ));
            }
            arg_spans.push(a.value.span);
        }
    }

    if let ExprKind::Ident(name) = &callee_expr.kind
        && env.lookup(name.as_ref()).is_none()
        && let Some(ret) = check_pipe_stdpol01_overload(
            name.as_ref(),
            &arg_tys,
            &arg_spans,
            callee_expr.span,
            diagnostics,
        )
    {
        return ret;
    }

    if let ExprKind::FieldAccess { target, field } = &callee_expr.kind
        && let ExprKind::Ident(ns_name) = &target.kind
        && env.lookup(ns_name.as_ref()).is_none()
        && let Some(ns) = NamespaceId::from_name(ns_name.as_ref())
    {
        program.resolutions.namespace_ref.insert(target.id, ns);
        if ns == NamespaceId::Csv
            && field.as_ref() == "encode"
            && let Some(Ty::List(element)) = arg_tys.first()
        {
            program
                .resolutions
                .csv_encode_target
                .insert(callee_expr.id, (**element).clone());
        }
        return check_pipe_stage_namespace_call(
            ns,
            ns_name,
            field,
            callee_expr.span,
            &arg_tys,
            &arg_spans,
            expected,
            program,
            effects,
            diagnostics,
        );
    }

    if let ExprKind::Ident(name) = &callee_expr.kind
        && let Some(binding) = env.lookup(name.as_ref())
        && let Ty::Function {
            params,
            effects: function_effects,
            ret,
        } = &binding.ty
    {
        let sig = Sig {
            generics: Vec::new(),
            params: params.clone(),
            ret: (**ret).clone(),
            effects: *function_effects,
            forward_fn_effects: false,
            mutates: false,
        };
        return check_typed_args_call(
            &sig,
            &arg_tys,
            &arg_spans,
            callee_expr.span,
            expected,
            effects,
            diagnostics,
        );
    }

    if let ExprKind::Ident(name) = &callee_expr.kind
        && let Some(f) = program.functions.get(name.as_ref()).cloned()
    {
        let mut own_effects = EffectSet::empty();
        for e in &f.effects {
            if let Some(bit) = EffectSet::from_name(e) {
                own_effects = own_effects.union(bit);
            }
        }
        let params: Vec<Ty> = f
            .params
            .iter()
            .map(|p| ty_from_ann(&p.ty, &f.generics, program).unwrap_or(Ty::Unknown))
            .collect();
        let ret_ty = ty_from_ann(&f.ret, &f.generics, program).unwrap_or(Ty::Unknown);
        let sig = Sig {
            generics: f.generics.clone(),
            params,
            ret: ret_ty,
            effects: own_effects,
            forward_fn_effects: false,
            mutates: false,
        };
        return check_typed_args_call(
            &sig,
            &arg_tys,
            &arg_spans,
            callee_expr.span,
            expected,
            effects,
            diagnostics,
        );
    }

    diagnostics.push(Diagnostic {
        code: ErrorCode::UninferableType,
        span: callee_expr.span,
        message: "cannot resolve the pipe target".to_owned(),
    });
    Ty::Unknown
}

fn unwrap_question(
    target_ty: &Ty,
    ret_ctx: Option<&Ty>,
    span: Span,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    match target_ty {
        Ty::Named { name, args } if name.as_ref() == "Result" && args.len() == 2 => {
            match ret_ctx {
                Some(Ty::Named {
                    name: rn,
                    args: rargs,
                }) if rn.as_ref() == "Result" && rargs.len() == 2 => {
                    if infer::unify(&args[1], &rargs[1]).is_none() {
                        diagnostics.push(Diagnostic {
                            code: ErrorCode::QuestionOperatorMismatch,
                            span,
                            message:
                                "the error type of ? does not match the error type of the function's return-type annotation (D-ERR-01)"
                                    .to_owned(),
                        });
                    }
                }
                Some(_) => diagnostics.push(Diagnostic {
                    code: ErrorCode::QuestionOperatorMismatch,
                    span,
                    message: "? on a Result expression can only be used inside a function that returns Result (D-ERR-01)"
                        .to_owned(),
                }),
                None => {}
            }
            args[0].clone()
        }
        Ty::Named { name, args } if name.as_ref() == "Option" && args.len() == 1 => {
            match ret_ctx {
                Some(Ty::Named { name: rn, .. }) if rn.as_ref() == "Option" => {}
                Some(_) => diagnostics.push(Diagnostic {
                    code: ErrorCode::QuestionOperatorMismatch,
                    span,
                    message: "? on an Option expression can only be used inside a function that returns Option (D-ERR-01)"
                        .to_owned(),
                }),
                None => {}
            }
            args[0].clone()
        }
        Ty::Unknown => Ty::Unknown,
        _ => {
            diagnostics.push(Diagnostic {
                code: ErrorCode::QuestionOperatorMismatch,
                span,
                message: "? can only be used on a Result/Option type (D-ERR-01)".to_owned(),
            });
            Ty::Unknown
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check the ? operator (D-ERR-01/02's Result/Option match determination)"
)]
fn check_question(
    target: &Expr,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
    span: Span,
) -> Ty {
    // When ret_ctx tells us whether it is Result or Option, that shape is used as-is
    // (the most precise case, since even the concrete E-side type is correctly conveyed).
    // Even when ret_ctx is absent (the top level, or something other than Option/Result),
    // as long as `expected` (D-TYPE-16's assignment-target-driven inference) is known, a
    // best-guess hint of "target is Result[expected, ?]" is supplied -- needed to resolve
    // T for a top-level `data: User = json.decode(s)?` such as `json.decode(s)?`
    // (harmless to fill the E side with a provisional type variable, since
    // `unify_collect` always treats a `Ty::TypeVar` as compatible -- judgment call made in
    // this file). If target actually turns out to be Option, this hint's shape simply
    // fails to match and is ignored (since `finalize_ret` never consults `expected_ret`
    // once the call's own type variable is already determined from an argument, passing
    // an unrelated hint causes no harm).
    let target_expected: Option<Ty> = match ret_ctx {
        Some(Ty::Named { name, args }) if name.as_ref() == "Result" && args.len() == 2 => {
            let t_expected = expected.cloned().unwrap_or_else(|| tv("$Question"));
            Some(t_result(t_expected, args[1].clone()))
        }
        Some(Ty::Named { name, .. }) if name.as_ref() == "Option" => {
            let t_expected = expected.cloned().unwrap_or_else(|| tv("$Question"));
            Some(t_option(t_expected))
        }
        _ => expected
            .cloned()
            .map(|t_expected| t_result(t_expected, tv("$QuestionErr"))),
    };
    let target_ty = check_expr(
        target,
        target_expected.as_ref(),
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    unwrap_question(&target_ty, ret_ctx, span, diagnostics)
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check list/tuple index access"
)]
fn check_index(
    target: &Expr,
    index: &Expr,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
    span: Span,
) -> Ty {
    let target_ty = check_expr(target, None, ret_ctx, env, program, effects, diagnostics);
    match &target_ty {
        Ty::List(t) => {
            let idx_ty = check_expr(
                index,
                Some(&Ty::Int),
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            if !matches!(idx_ty, Ty::Int | Ty::Unknown) {
                push_type_mismatch(span, diagnostics, "a list index must be int (D-COL-02)");
            }
            (**t).clone()
        }
        Ty::Dict(k, v) => {
            let idx_ty = check_expr(index, Some(k), ret_ctx, env, program, effects, diagnostics);
            if infer::unify(&idx_ty, k).is_none() && !matches!(idx_ty, Ty::Unknown) {
                push_type_mismatch(
                    span,
                    diagnostics,
                    "the type of the dict index does not match the key type (D-COL-02)",
                );
            }
            (**v).clone()
        }
        Ty::Unknown => {
            check_expr(index, None, ret_ctx, env, program, effects, diagnostics);
            Ty::Unknown
        }
        _ => {
            push_type_mismatch(
                span,
                diagnostics,
                "[] can only be used on list/dict (D-COL-02/03)",
            );
            check_expr(index, None, ret_ctx, env, program, effects, diagnostics);
            Ty::Unknown
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check a tuple's `.N` index access"
)]
fn check_tuple_index(
    target: &Expr,
    index: u32,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
    span: Span,
) -> Ty {
    let target_ty = check_expr(target, None, ret_ctx, env, program, effects, diagnostics);
    match &target_ty {
        Ty::Tuple(items) => {
            if let Some(t) = items.get(index as usize) {
                t.clone()
            } else {
                push_type_mismatch(
                    span,
                    diagnostics,
                    "this access exceeds the tuple's element count (D-TYPE-06)",
                );
                Ty::Unknown
            }
        }
        Ty::Unknown => Ty::Unknown,
        _ => {
            push_type_mismatch(
                span,
                diagnostics,
                ".N access can only be used on a tuple (D-TYPE-06)",
            );
            Ty::Unknown
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to resolve field access (the 3 families: namespace constant / struct / builtin-struct field)"
)]
fn check_field_access(
    field_expr: &Expr,
    target: &Expr,
    field: &Arc<str>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if let ExprKind::Ident(name) = &target.kind
        && env.lookup(name.as_ref()).is_none()
        && let Some(ns) = NamespaceId::from_name(name.as_ref())
    {
        program.resolutions.namespace_ref.insert(target.id, ns);
        return namespace_const_ty(ns, field.as_ref()).unwrap_or_else(|| {
            diagnostics.push(Diagnostic {
                code: ErrorCode::UninferableType,
                span: field_expr.span,
                message: format!("undefined namespace constant '{name}.{field}'"),
            });
            Ty::Unknown
        });
    }
    let target_ty = check_expr(target, None, ret_ctx, env, program, effects, diagnostics);
    match &target_ty {
        Ty::Named { name, .. } => {
            if let Some((idx, ty)) = resolve_struct_field(&target_ty, field.as_ref(), program) {
                program.resolutions.field_index.insert(field_expr.id, idx);
                ty
            } else {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::UninferableType,
                    span: field_expr.span,
                    message: format!("'{name}' has no field '{field}'"),
                });
                Ty::Unknown
            }
        }
        Ty::Unknown => Ty::Unknown,
        _ => {
            diagnostics.push(Diagnostic {
                code: ErrorCode::UninferableType,
                span: field_expr.span,
                message: "this type has no fields".to_owned(),
            });
            Ty::Unknown
        }
    }
}

/// Resolves `(declaration-order index, concrete type)` for `field` on `target_ty` (always
/// `Ty::Named` -- a struct or builtin struct). Shared by both `FieldAccess` (has a
/// NodeId) and `FieldAssign` (has no NodeId, used from `check_stmt.rs`).
pub(crate) fn resolve_struct_field(
    target_ty: &Ty,
    field: &str,
    program: &Program,
) -> Option<(u32, Ty)> {
    let Ty::Named { name, args } = target_ty else {
        return None;
    };
    if let Some(fields) = builtin_struct_fields(name.as_ref()) {
        let idx = fields.iter().position(|(n, _)| *n == field)?;
        return Some((u32::try_from(idx).unwrap_or(0), fields[idx].1.clone()));
    }
    let decl = program.structs.get(name.as_ref())?;
    let (idx, f) = decl
        .fields
        .iter()
        .enumerate()
        .find(|(_, f)| f.name.as_ref() == field)?;
    let subst: HashMap<Arc<str>, Ty> = decl
        .generics
        .iter()
        .cloned()
        .zip(args.iter().cloned())
        .collect();
    let raw = ty_from_ann(&f.ty, &decl.generics, program).unwrap_or(Ty::Unknown);
    Some((
        u32::try_from(idx).unwrap_or(0),
        generics::substitute(&raw, &subst),
    ))
}

fn contains_bare_question(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Question { .. } => true,
        ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::Ident(_)
        | ExprKind::Lambda { .. } => false,
        ExprKind::FString(segments) => segments.iter().any(|segment| {
            matches!(segment, FStringSegment::Expr(inner) if contains_bare_question(inner))
        }),
        ExprKind::ListLit { elements, .. }
        | ExprKind::SetLit { elements, .. }
        | ExprKind::TupleLit { elements, .. }
        | ExprKind::Par { elements, .. } => elements.iter().any(contains_bare_question),
        ExprKind::DictLit { entries, .. } => entries
            .iter()
            .any(|(key, value)| contains_bare_question(key) || contains_bare_question(value)),
        ExprKind::Unary { operand, .. }
        | ExprKind::FieldAccess {
            target: operand, ..
        }
        | ExprKind::TupleIndex {
            target: operand, ..
        }
        | ExprKind::Grouping(operand) => contains_bare_question(operand),
        ExprKind::Binary { lhs, rhs, .. } => {
            contains_bare_question(lhs) || contains_bare_question(rhs)
        }
        ExprKind::Call { callee, args, .. } => {
            contains_bare_question(callee)
                || args
                    .iter()
                    .any(|argument| contains_bare_question(&argument.value))
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            contains_bare_question(receiver)
                || args
                    .iter()
                    .any(|argument| contains_bare_question(&argument.value))
        }
        ExprKind::Index { target, index } => {
            contains_bare_question(target) || contains_bare_question(index)
        }
        ExprKind::Pipe(pipe) => {
            contains_bare_question(&pipe.source)
                || pipe.stages.iter().any(|stage| {
                    stage.question
                        || match &stage.callee {
                            PipeCallee::Bare(callee) => contains_bare_question(callee),
                            PipeCallee::WithArgs { callee, args } => {
                                contains_bare_question(callee)
                                    || args.iter().any(|argument| {
                                        !argument.is_placeholder
                                            && contains_bare_question(&argument.value)
                                    })
                            }
                        }
                })
        }
        ExprKind::If(if_expression) => if_contains_bare_question(if_expression),
        ExprKind::Match { scrutinee, arms } => {
            contains_bare_question(scrutinee)
                || arms.iter().any(|arm| match &arm.body {
                    MatchArmBody::Expr(expression) => contains_bare_question(expression),
                    MatchArmBody::Block(block) => block_contains_bare_question(block),
                })
        }
    }
}

fn if_contains_bare_question(if_expression: &IfExpr) -> bool {
    contains_bare_question(&if_expression.cond)
        || block_contains_bare_question(&if_expression.then_branch)
        || match &if_expression.else_branch {
            ElseBranch::Block(block) => block_contains_bare_question(block),
            ElseBranch::ElseIf(nested) => if_contains_bare_question(nested),
        }
}

fn block_contains_bare_question(block: &Block) -> bool {
    block.stmts.iter().any(|statement| match &statement.kind {
        StmtKind::VarDecl { value, .. } | StmtKind::NameAssign { value, .. } => {
            contains_bare_question(value)
        }
        StmtKind::FieldAssign { target, value, .. } => {
            contains_bare_question(target) || contains_bare_question(value)
        }
        StmtKind::IndexAssign {
            target,
            index,
            value,
        } => {
            contains_bare_question(target)
                || contains_bare_question(index)
                || contains_bare_question(value)
        }
        StmtKind::Discard(expression)
        | StmtKind::ExprStmt(expression)
        | StmtKind::Return(Some(expression)) => contains_bare_question(expression),
        StmtKind::Return(None) => false,
    })
}
#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to resolve a MethodCall expression (a namespace-function call / a builtin or user-defined method call)"
)]
fn check_method_call(
    call_expr: &Expr,
    receiver: &Expr,
    method: &Arc<str>,
    type_args: &[crate::ast::TypeAnn],
    args: &[Arg],
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let call_span = call_expr.span;
    let explicit_tys: Vec<Ty> = type_args
        .iter()
        .map(|t| ty_from_ann(t, env.generics(), program).unwrap_or(Ty::Unknown))
        .collect();

    if let ExprKind::Ident(recv_name) = &receiver.kind
        && env.lookup(recv_name.as_ref()).is_none()
        && let Some(ns) = NamespaceId::from_name(recv_name.as_ref())
    {
        program.resolutions.namespace_ref.insert(receiver.id, ns);
        let Some(sig) = namespace_fn_sig(ns, method.as_ref()) else {
            diagnostics.push(Diagnostic {
                code: ErrorCode::UninferableType,
                span: call_span,
                message: format!("undefined namespace function '{recv_name}.{method}'"),
            });
            for a in args {
                check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
            }
            return Ty::Unknown;
        };
        for a in args {
            if a.name.is_some() {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::BranchTypeMismatch,
                    span: a.value.span,
                    message: "a namespace-function call accepts only positional arguments"
                        .to_owned(),
                });
            }
        }
        let arg_exprs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
        let call_sig = CallSig {
            generics: &sig.generics,
            params: &sig.params,
            ret: &sig.ret,
            own_effects: sig.effects,
            forward_fn_effects: sig.forward_fn_effects,
        };
        let ret = check_positional_call(
            &call_sig,
            &arg_exprs,
            &explicit_tys,
            call_span,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
        let argument_type = args
            .first()
            .and_then(|argument| program.resolutions.expr_ty.get(&argument.value.id))
            .cloned();
        validate_csv_type(
            ns,
            method,
            argument_type.as_ref(),
            &ret,
            call_span,
            program,
            diagnostics,
        );
        record_decode_target_if_applicable(call_expr.id, ns, method.as_ref(), &ret, program);
        if ns == NamespaceId::Toml
            && method.as_ref() == "encode"
            && let Some(a0) = args.first()
            && let Some(t0) = program.resolutions.expr_ty.get(&a0.value.id).cloned()
            && !is_toml_encodable_root(&t0, program)
        {
            diagnostics.push(Diagnostic {
                code: ErrorCode::MissingParamAnnotation,
                span: call_span,
                message:
                    "toml.encode[T] is valid only when T is dict[str,V] or a struct (D-STDPOL-09)"
                        .to_owned(),
            });
        }
        return ret;
    }

    let receiver_ty = check_expr(receiver, None, ret_ctx, env, program, effects, diagnostics);
    dispatch_method(
        receiver,
        &receiver_ty,
        method,
        &explicit_tys,
        args,
        expected,
        call_span,
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    )
}

fn record_decode_target_if_applicable(
    call_id: crate::ast::NodeId,
    ns: NamespaceId,
    method: &str,
    ret: &Ty,
    program: &mut Program,
) {
    if method != "decode" {
        return;
    }
    let Ty::Named { name, args } = ret else {
        return;
    };
    if name.as_ref() != "Result" || args.len() != 2 {
        return;
    }
    match ns {
        NamespaceId::Json | NamespaceId::Yaml | NamespaceId::Toml => {
            program
                .resolutions
                .decode_target
                .insert(call_id, args[0].clone());
        }
        NamespaceId::Csv => {
            if let Ty::List(elem) = &args[0] {
                program
                    .resolutions
                    .decode_target
                    .insert(call_id, (**elem).clone());
            }
        }
        _ => {}
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "method dispatch shares resolution, mutability, generic, and effect context"
)]
fn dispatch_method(
    receiver: &Expr,
    receiver_ty: &Ty,
    method: &Arc<str>,
    explicit_tys: &[Ty],
    args: &[Arg],
    expected: Option<&Ty>,
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    if matches!(receiver_ty, Ty::Unknown) {
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
        return Ty::Unknown;
    }
    if let Ty::Named { name, args: nargs } = receiver_ty
        && !matches!(name.as_ref(), "Result" | "Option" | "Value")
        && let Some(decl) = program.structs.get(name.as_ref()).cloned()
    {
        return dispatch_struct_method(
            &decl,
            nargs,
            receiver,
            method,
            explicit_tys,
            args,
            expected,
            call_span,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        );
    }
    if let Ty::List(element) = receiver_ty {
        if method.as_ref() == "join" && !matches!(**element, Ty::Str | Ty::Unknown) {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: call_span,
                message: "join is available only on list[str]".to_owned(),
            });
        }
        if method.as_ref() == "to_set"
            && !is_allowed_key_type(element)
            && !matches!(**element, Ty::Unknown)
        {
            diagnostics.push(Diagnostic {
                code: ErrorCode::SetElementTypeNotAllowed,
                span: call_span,
                message: "list element type cannot be converted to a set".to_owned(),
            });
        }
        if matches!(method.as_ref(), "par_map" | "par_each")
            && let Some(argument) = args.first()
            && let ExprKind::Lambda { body, .. } = &argument.value.kind
            && contains_bare_question(body)
        {
            diagnostics.push(Diagnostic {
                code: ErrorCode::ParallelQuestionOperator,
                span: body.span,
                message: "a bare question operator is forbidden in a parallel lambda".to_owned(),
            });
        }
    }
    let sig_opt = match receiver_ty {
        Ty::Int | Ty::Float | Ty::Bool | Ty::Str => {
            primitive_method_sig(receiver_ty, method.as_ref())
        }
        Ty::List(t) => list_method_sig(t, method.as_ref()),
        Ty::Dict(k, v) => dict_method_sig(k, v, method.as_ref()),
        Ty::Set(t) => set_method_sig(t, method.as_ref()),
        Ty::Named { name, args: nargs } if name.as_ref() == "Result" && nargs.len() == 2 => {
            result_method_sig(&nargs[0], &nargs[1], method.as_ref())
        }
        Ty::Named { name, args: nargs } if name.as_ref() == "Option" && nargs.len() == 1 => {
            option_method_sig(&nargs[0], method.as_ref())
        }
        Ty::Named { name, .. } if name.as_ref() == "Value" => value_method_sig(method.as_ref()),
        _ => None,
    };
    let Some(sig) = sig_opt else {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UninferableType,
            span: call_span,
            message: format!("undefined method '.{method}'"),
        });
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
        return Ty::Unknown;
    };
    if sig.mutates {
        mutability::check_mutable_place(receiver, env, diagnostics);
    }
    for a in args {
        if a.name.is_some() {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: a.value.span,
                message: "a method call accepts only positional arguments".to_owned(),
            });
        }
    }
    let arg_exprs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
    let call_sig = CallSig {
        generics: &sig.generics,
        params: &sig.params,
        ret: &sig.ret,
        own_effects: sig.effects,
        forward_fn_effects: sig.forward_fn_effects,
    };
    let result = check_positional_call(
        &call_sig,
        &arg_exprs,
        explicit_tys,
        call_span,
        expected,
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    if matches!(receiver_ty, Ty::List(_))
        && method.as_ref() == "sort_by"
        && let Some(argument) = args.first()
        && let Some(Ty::Function { ret, .. }) = program.resolutions.expr_ty.get(&argument.value.id)
        && !matches!(ret.as_ref(), Ty::Int | Ty::Float | Ty::Str | Ty::Unknown)
    {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UnorderableType,
            span: argument.value.span,
            message: "sort_by key must be int, float, or str".to_owned(),
        });
    }
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to resolve a user-defined struct method (including the struct's own type-parameter substitution and the var self check)"
)]
fn dispatch_struct_method(
    decl: &crate::ast::StructDecl,
    receiver_args: &[Ty],
    receiver: &Expr,
    method: &Arc<str>,
    explicit_tys: &[Ty],
    args: &[Arg],
    expected: Option<&Ty>,
    call_span: Span,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let Some(m) = decl
        .methods
        .iter()
        .find(|m| m.name.as_ref() == method.as_ref())
    else {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UninferableType,
            span: call_span,
            message: format!("struct '{}' has no method '.{method}'", decl.name),
        });
        for a in args {
            check_expr(&a.value, None, ret_ctx, env, program, effects, diagnostics);
        }
        return Ty::Unknown;
    };
    let outer_subst: HashMap<Arc<str>, Ty> = decl
        .generics
        .iter()
        .cloned()
        .zip(receiver_args.iter().cloned())
        .collect();
    let combined_scope: Vec<Arc<str>> = decl
        .generics
        .iter()
        .cloned()
        .chain(m.generics.iter().cloned())
        .collect();
    let params: Vec<Ty> = m
        .params
        .iter()
        .map(|p| {
            generics::substitute(
                &ty_from_ann(&p.ty, &combined_scope, program).unwrap_or(Ty::Unknown),
                &outer_subst,
            )
        })
        .collect();
    let ret = generics::substitute(
        &ty_from_ann(&m.ret, &combined_scope, program).unwrap_or(Ty::Unknown),
        &outer_subst,
    );
    let mut own_effects = EffectSet::empty();
    for e in &m.effects {
        if let Some(bit) = EffectSet::from_name(e) {
            own_effects = own_effects.union(bit);
        }
    }
    if m.self_param.as_ref().is_some_and(|sp| sp.mutable) {
        mutability::check_mutable_place(receiver, env, diagnostics);
    }
    for a in args {
        if a.name.is_some() {
            diagnostics.push(Diagnostic {
                code: ErrorCode::BranchTypeMismatch,
                span: a.value.span,
                message: "a method call accepts only positional arguments".to_owned(),
            });
        }
    }
    let arg_exprs: Vec<&Expr> = args.iter().map(|a| &a.value).collect();
    let call_sig = CallSig {
        generics: &m.generics,
        params: &params,
        ret: &ret,
        own_effects,
        forward_fn_effects: false,
    };
    check_positional_call(
        &call_sig,
        &arg_exprs,
        explicit_tys,
        call_span,
        expected,
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    )
}

/// Type-checks a lambda expression (D-SYN-10: the body is a single expression).
/// Parameter types use an explicit annotation if present, otherwise borrow from the call
/// context's expected function type (`expected`) (D-FUNC-02). The body's effects are
/// computed with a fresh accumulator independent of the caller's `effects` accumulation
/// (merely defining this lambda executes no effect on the caller's part), and stored
/// into the determined `Ty::Function`'s `effects` field -- EffectCheck later reads this
/// value at each higher-order-function call site and adds it in (§5.5).
fn check_lambda(
    params: &[crate::ast::LambdaParam],
    body: &Expr,
    expected: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let expected_fn = match expected {
        Some(Ty::Function {
            params: ep, ret, ..
        }) if ep.len() == params.len() => Some((ep.clone(), (**ret).clone())),
        _ => None,
    };
    env.push_scope();
    let mut param_tys = Vec::with_capacity(params.len());
    for (i, p) in params.iter().enumerate() {
        let ty = if let Some(ann) = &p.ty {
            ty_from_ann(ann, env.generics(), program).unwrap_or(Ty::Unknown)
        } else if let Some((ep, _)) = &expected_fn {
            ep[i].clone()
        } else {
            Ty::Unknown
        };
        env.bind(Arc::clone(&p.name), ty.clone(), false);
        param_tys.push(ty);
    }
    let body_ret_ctx: Option<Ty> = expected_fn
        .as_ref()
        .map(|(_, r)| r.clone())
        .filter(|r| !generics::contains_type_var(r));
    let mut lambda_effects = EffectSet::empty();
    let body_ty = check_expr(
        body,
        body_ret_ctx.as_ref(),
        body_ret_ctx.as_ref(),
        env,
        program,
        &mut lambda_effects,
        diagnostics,
    );
    env.pop_scope();
    let ret_ty = match &body_ret_ctx {
        Some(r) => infer::unify(r, &body_ty).unwrap_or(body_ty),
        None => body_ty,
    };
    Ty::Function {
        params: param_tys,
        effects: lambda_effects,
        ret: Box::new(ret_ty),
    }
}

fn check_if_expr(
    if_expr: &IfExpr,
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let cond_ty = check_expr(
        &if_expr.cond,
        Some(&Ty::Bool),
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    if !matches!(cond_ty, Ty::Bool | Ty::Unknown) {
        push_type_mismatch(
            if_expr.cond.span,
            diagnostics,
            "the condition of an if must be bool",
        );
    }
    env.push_scope();
    let then_opt = crate::types::check_stmt::check_block_value(
        &if_expr.then_branch,
        expected,
        ret_ctx,
        env,
        program,
        effects,
        diagnostics,
    );
    env.pop_scope();
    let else_opt = match &if_expr.else_branch {
        crate::ast::ElseBranch::Block(b) => {
            env.push_scope();
            let t = crate::types::check_stmt::check_block_value(
                b,
                expected,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            );
            env.pop_scope();
            t
        }
        crate::ast::ElseBranch::ElseIf(inner) => Some(check_if_expr(
            inner,
            expected,
            ret_ctx,
            env,
            program,
            effects,
            diagnostics,
        )),
    };
    match (then_opt, else_opt) {
        (Some(t), Some(e)) => {
            if let Some(u) = infer::unify(&t, &e) {
                u
            } else {
                push_type_mismatch(
                    if_expr.span,
                    diagnostics,
                    "the types of if's branches do not match (D-SYN-11)",
                );
                Ty::Unknown
            }
        }
        (Some(t), None) | (None, Some(t)) => t,
        (None, None) => Ty::Unknown,
    }
}

fn is_known_unit_variant(name: &str, ty: &Ty, program: &Program) -> bool {
    match ty {
        Ty::Named { name: tn, .. } if tn.as_ref() == "Option" => name == "None",
        Ty::Named { name: tn, .. } => program.enums.get(tn.as_ref()).is_some_and(|e| {
            e.variants
                .iter()
                .any(|v| v.name.as_ref() == name && v.fields.is_empty())
        }),
        _ => false,
    }
}

fn variant_field_tys(scrutinee_ty: &Ty, variant_name: &str, program: &Program) -> Option<Vec<Ty>> {
    match scrutinee_ty {
        Ty::Named { name, args } if name.as_ref() == "Result" && args.len() == 2 => {
            match variant_name {
                "Ok" => Some(vec![args[0].clone()]),
                "Err" => Some(vec![args[1].clone()]),
                _ => None,
            }
        }
        Ty::Named { name, args } if name.as_ref() == "Option" && args.len() == 1 => {
            match variant_name {
                "Some" => Some(vec![args[0].clone()]),
                _ => None,
            }
        }
        Ty::Named { name, args } => {
            let decl = program.enums.get(name.as_ref())?;
            let variant = decl
                .variants
                .iter()
                .find(|v| v.name.as_ref() == variant_name)?;
            let subst: HashMap<Arc<str>, Ty> = decl
                .generics
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            Some(
                variant
                    .fields
                    .iter()
                    .map(|t| {
                        generics::substitute(
                            &ty_from_ann(t, &decl.generics, program).unwrap_or(Ty::Unknown),
                            &subst,
                        )
                    })
                    .collect(),
            )
        }
        _ => None,
    }
}

fn literal_pattern_type(literal: &LiteralPat) -> Ty {
    match literal {
        LiteralPat::Int(_) => Ty::Int,
        LiteralPat::Float(_) => Ty::Float,
        LiteralPat::Bool(_) => Ty::Bool,
        LiteralPat::Str(_) => Ty::Str,
    }
}

fn bind_subpattern(
    subpattern: &SubPattern,
    expected: &Ty,
    env: &mut TypeEnv,
    program: &mut Program,
    diagnostics: &mut DiagnosticBag,
) {
    match subpattern {
        SubPattern::Literal(literal, span) => {
            let actual = literal_pattern_type(literal);
            if infer::unify(expected, &actual).is_none() && !matches!(expected, Ty::Unknown) {
                push_type_mismatch(*span, diagnostics, "pattern literal type does not match");
            }
        }
        SubPattern::Wildcard(_) => {}
        SubPattern::BareIdent(name, node_id, _) => {
            if is_known_unit_variant(name, expected, program) {
                program
                    .resolutions
                    .bare_ident_kind
                    .insert(*node_id, BareIdentKind::UnitVariant);
            } else {
                program
                    .resolutions
                    .bare_ident_kind
                    .insert(*node_id, BareIdentKind::Binding);
                env.bind(Arc::clone(name), expected.clone(), false);
            }
        }
    }
}

fn bind_pattern(
    pattern: &Pattern,
    scrutinee_ty: &Ty,
    env: &mut TypeEnv,
    program: &mut Program,
    diagnostics: &mut DiagnosticBag,
) {
    match pattern {
        Pattern::Literal(literal, span) => {
            let actual = literal_pattern_type(literal);
            if infer::unify(scrutinee_ty, &actual).is_none() && !matches!(scrutinee_ty, Ty::Unknown)
            {
                push_type_mismatch(*span, diagnostics, "pattern literal type does not match");
            }
        }
        Pattern::Wildcard(_) => {}
        Pattern::BareIdent(name, node_id, _) => {
            if is_known_unit_variant(name, scrutinee_ty, program) {
                program
                    .resolutions
                    .bare_ident_kind
                    .insert(*node_id, BareIdentKind::UnitVariant);
            } else {
                program
                    .resolutions
                    .bare_ident_kind
                    .insert(*node_id, BareIdentKind::Binding);
                env.bind(Arc::clone(name), scrutinee_ty.clone(), false);
            }
        }
        Pattern::Tuple { elements, span } => {
            let Ty::Tuple(items) = scrutinee_ty else {
                if !matches!(scrutinee_ty, Ty::Unknown) {
                    push_type_mismatch(*span, diagnostics, "tuple pattern requires a tuple");
                }
                return;
            };
            if elements.len() != items.len() {
                push_type_mismatch(*span, diagnostics, "tuple pattern arity does not match");
                return;
            }
            for (subpattern, item) in elements.iter().zip(items) {
                bind_subpattern(subpattern, item, env, program, diagnostics);
            }
        }
        Pattern::Variant { name, fields, span } => {
            let Some(field_types) = variant_field_tys(scrutinee_ty, name, program) else {
                push_type_mismatch(
                    *span,
                    diagnostics,
                    "variant does not belong to the scrutinee",
                );
                return;
            };
            if fields.len() != field_types.len() {
                push_type_mismatch(*span, diagnostics, "variant pattern arity does not match");
                return;
            }
            for (subpattern, field_type) in fields.iter().zip(&field_types) {
                bind_subpattern(subpattern, field_type, env, program, diagnostics);
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "receives together all the context needed to check a match expression (exhaustiveness checking, per-arm pattern binding, and D-SYN-11's block value rule)"
)]
fn check_match_expr(
    match_expr: &Expr,
    scrutinee: &Expr,
    arms: &[MatchArm],
    expected: Option<&Ty>,
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    let scrutinee_ty = check_expr(scrutinee, None, ret_ctx, env, program, effects, diagnostics);
    let enum_decl_for_exhaustiveness = match &scrutinee_ty {
        Ty::Named { name, .. }
            if !matches!(name.as_ref(), "Result" | "Option" | "Value" | "Error") =>
        {
            program.enums.get(name.as_ref()).cloned()
        }
        _ => None,
    };
    let arm_pattern_refs: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
    crate::types::exhaustiveness::check_exhaustiveness(
        &scrutinee_ty,
        enum_decl_for_exhaustiveness.as_deref(),
        &arm_pattern_refs,
        match_expr.span,
        diagnostics,
    );

    let mut result_ty: Option<Ty> = None;
    for arm in arms {
        env.push_scope();
        bind_pattern(&arm.pattern, &scrutinee_ty, env, program, diagnostics);
        let arm_value = match &arm.body {
            MatchArmBody::Expr(e) => {
                let ty = check_expr(e, expected, ret_ctx, env, program, effects, diagnostics);
                if crate::types::check_stmt::expr_diverges(e) {
                    None
                } else {
                    Some(ty)
                }
            }
            MatchArmBody::Block(b) => crate::types::check_stmt::check_block_value(
                b,
                expected,
                ret_ctx,
                env,
                program,
                effects,
                diagnostics,
            ),
        };
        env.pop_scope();
        if let Some(ty) = arm_value {
            result_ty = Some(match result_ty {
                None => ty,
                Some(u) => {
                    if let Some(merged) = infer::unify(&u, &ty) {
                        merged
                    } else {
                        push_type_mismatch(
                            arm.span,
                            diagnostics,
                            "the types of match's branches do not match (D-SYN-11)",
                        );
                        u
                    }
                }
            });
        }
    }
    result_ty.unwrap_or(Ty::Unknown)
}

fn check_par(
    kind: &ParKind,
    elements: &[Expr],
    ret_ctx: Option<&Ty>,
    env: &mut TypeEnv,
    program: &mut Program,
    effects: &mut EffectSet,
    diagnostics: &mut DiagnosticBag,
) -> Ty {
    match kind {
        ParKind::Tuple => {
            let tys = elements
                .iter()
                .map(|e| check_expr(e, None, ret_ctx, env, program, effects, diagnostics))
                .collect();
            Ty::Tuple(tys)
        }
        ParKind::List => {
            let mut unified: Option<Ty> = None;
            let mut checked = Vec::with_capacity(elements.len());
            for e in elements {
                let t = check_expr(
                    e,
                    unified.as_ref(),
                    ret_ctx,
                    env,
                    program,
                    effects,
                    diagnostics,
                );
                checked.push((t, e.span));
            }
            for (t, elem_span) in checked {
                unified = Some(match unified {
                    None => t,
                    Some(u) => {
                        if let Some(merged) = infer::unify(&u, &t) {
                            merged
                        } else {
                            diagnostics.push(Diagnostic {
                                code: ErrorCode::CollectionElementTypeMismatch,
                                span: elem_span,
                                message: "cannot unify the element types of par [..] (D-TYPE-04)"
                                    .to_owned(),
                            });
                            u
                        }
                    }
                });
            }
            t_list(unified.unwrap_or(Ty::Unknown))
        }
    }
}
