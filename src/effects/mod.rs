//! The core of effect row inference (D-FUNC-03), E2001/E2002 detection, and effect
//! polymorphism for higher-order functions (EFFECT-HOF-POLYMORPHISM decision,
//! ARCHITECTURE.md §5.5/§8).
//!
//! This completes purely by reading the `Ty::Function{effects,..}` of each expression
//! that type checking already determined (`Resolutions::expr_ty`) -- it needs to re-walk
//! the AST but does not need to redo type inference. However this module is also the sole
//! phase that writes `Resolutions::hof_forwarding` (§4.2).
//!
//! Judgment call made in this file (an intentional deviation from ARCHITECTURE.md §5.5's
//! pseudocode): the pseudocode shows `infer_effects`/`check_function_effects` taking an
//! `env: &TypeEnv` parameter, but every piece of information actually needed (the
//! resolved types of callee/arguments, namespace resolution, the hof_forwarding mask) can
//! be obtained from just `Program::resolutions` (the `expr_ty`/`namespace_ref` the
//! TypeCheck phase filled in) and `Program::functions`/`structs` -- the `TypeEnv` is
//! discarded once TypeCheck completes anyway and would need to be rebuilt, paying a
//! duplicate name-resolution cost for no additional information, so the 3 functions in
//! this file do not take `env` as a parameter (since no caller for that exists anywhere,
//! the blast radius of this signature choice stays contained within this file).
//!
//! **Adjustment needed in a file outside this Unit's scope (Unit17 driver.rs)**:
//! `Program` (`src/eval/env.rs`) has the entry file's top-level executable statements
//! completely dropped from the `Program` skeleton at ModuleResolve time (the
//! `Item::Stmt(_) => {}` in `module_resolve/mod.rs`). D-LINT-02 (unused function),
//! D-LINT-03 (shadowing), and D-LINT-05 (naming convention) all include "top-level
//! executable statements" as grounds for their determination (several cases under
//! samples/err/lint/ directly verify top-level variables/calls), so this is fundamentally
//! undecidable given only `Program`, the input to the Lint phase (with Unit9's fixed
//! signature `check(program: &Program, ..)`) -- when two unrelated functions share the
//! exact same structure of "never called by any other function", `Program` cannot
//! distinguish at all the fact that only one of them is actually called from the top
//! level (the information is structurally missing). To close this gap without changing
//! `Program`'s structure (`eval/env.rs`), this file defines the [`ENTRY_POINT_NAME`]
//! convention: driver.rs (Unit17) must, among the 6 phases, after TypeCheck
//! (`types::check_decl::check_program`) completes and before running EffectCheck/Lint,
//! assemble a synthesized `FunctionDecl` (`name: ENTRY_POINT_NAME`, other fields
//! empty/`Void`) whose `body` holds the entry file's top-level executable statements
//! (`entry_top_level_stmts`, the one passed to `check_program`), register it via
//! `program.functions.insert(Arc::from(ENTRY_POINT_NAME), Arc::new(entry_decl))`, and
//! only then call EffectCheck/Lint. This file's `check_program_effects` follows this
//! convention and excludes `ENTRY_POINT_NAME` from effect checking (the top level
//! implicitly permits every effect, §8). The lint side (Unit9's 5 files) was also
//! implemented assuming this same convention -- see the comment at the top of each lint
//! file for details. The tests (at the end of this file) reproduce this convention on
//! their own to verify it.
pub const ENTRY_POINT_NAME: &str = "$entry";

use crate::ast::{
    Arg, Block, ElseBranch, Expr, ExprKind, FStringSegment, FunctionDecl, IfExpr, MatchArmBody,
    PipeCallee, Stmt, StmtKind,
};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::eval::env::Program;
use crate::types::{EffectSet, NamespaceId, Ty};
use std::sync::Arc;

/// SPEC §8 "fs, net, env, proc, time, rand" -- each namespace uniquely owns its
/// corresponding effect (STDLIB.md §11.2). regex/math/codec (json/csv/yaml/toml) have no
/// effect (pure).
fn namespace_effect(ns: NamespaceId) -> EffectSet {
    match ns {
        NamespaceId::Fs => EffectSet::FS,
        NamespaceId::Http => EffectSet::NET,
        NamespaceId::Env => EffectSet::ENV,
        NamespaceId::Proc => EffectSet::PROC,
        NamespaceId::Time => EffectSet::TIME,
        NamespaceId::Rand => EffectSet::RAND,
        NamespaceId::Regex
        | NamespaceId::Math
        | NamespaceId::Json
        | NamespaceId::Csv
        | NamespaceId::Yaml
        | NamespaceId::Toml => EffectSet::empty(),
    }
}

fn effects_from_names(names: &[Arc<str>]) -> EffectSet {
    names
        .iter()
        .filter_map(|n| EffectSet::from_name(n))
        .fold(EffectSet::empty(), EffectSet::union)
}

fn as_function_effects(ty: &Ty) -> Option<EffectSet> {
    match ty {
        Ty::Function { effects, .. } => Some(*effects),
        _ => None,
    }
}

/// The fixed set of STDLIB higher-order method names (ARCHITECTURE.md §5.5
/// "a function-typed argument is unconditionally a forwarding target", the list/dict/set
/// method lists in STDLIB.md). `par`/`par_map`/`par_each` are also included in this set
/// under "no special-casing" (D-FUNC-03).
fn is_stdlib_hof_method(name: &str) -> bool {
    matches!(
        name,
        "map"
            | "filter"
            | "fold"
            | "find"
            | "find_by"
            | "any"
            | "all"
            | "flat_map"
            | "sort_by"
            | "par_map"
            | "par_each"
            | "each"
            | "unwrap_or_else"
            | "map_err"
            | "and_then"
    )
}

/// A call site discovered by the syntax walk.
enum CallSite<'a> {
    Call {
        span: Span,
        callee: &'a Expr,
        args: &'a [Arg],
    },
    Method {
        span: Span,
        receiver: &'a Expr,
        method: &'a Arc<str>,
        args: &'a [Arg],
    },
    Pipe {
        span: Span,
        callee: &'a Expr,
        source: &'a Expr,
        args: Option<&'a [Arg]>,
    },
}

impl CallSite<'_> {
    fn span(&self) -> Span {
        match self {
            CallSite::Call { span, .. }
            | CallSite::Method { span, .. }
            | CallSite::Pipe { span, .. } => *span,
        }
    }
}

/// Recursively walks the `block` body exactly once, calling `on_call` for every
/// occurring `Call`/`MethodCall` expression (descends into every level of nesting,
/// including if/match branches, lambda bodies, and par elements). A pipe (`x |> f`)
/// itself is not treated as a call site (judgment call made in this file, see the note
/// at the end), but nested calls appearing within each pipe stage's expressions
/// (non-`_` arguments, the bare-name callee itself) are detected as usual.
fn walk_block_calls<'a>(block: &'a Block, on_call: &mut dyn FnMut(CallSite<'a>)) {
    for stmt in &block.stmts {
        walk_stmt_calls(stmt, on_call);
    }
}

fn walk_stmt_calls<'a>(stmt: &'a Stmt, on_call: &mut dyn FnMut(CallSite<'a>)) {
    match &stmt.kind {
        StmtKind::VarDecl { value, .. } | StmtKind::NameAssign { value, .. } => {
            walk_expr_calls(value, on_call);
        }
        StmtKind::FieldAssign { target, value, .. } => {
            walk_expr_calls(target, on_call);
            walk_expr_calls(value, on_call);
        }
        StmtKind::IndexAssign {
            target,
            index,
            value,
        } => {
            walk_expr_calls(target, on_call);
            walk_expr_calls(index, on_call);
            walk_expr_calls(value, on_call);
        }
        StmtKind::Discard(e) | StmtKind::ExprStmt(e) | StmtKind::Return(Some(e)) => {
            walk_expr_calls(e, on_call);
        }
        StmtKind::Return(None) => {}
    }
}

fn walk_expr_calls<'a>(expr: &'a Expr, on_call: &mut dyn FnMut(CallSite<'a>)) {
    match &expr.kind {
        ExprKind::IntLit(_)
        | ExprKind::FloatLit(_)
        | ExprKind::BoolLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::Ident(_)
        | ExprKind::Lambda { .. } => {}
        ExprKind::FString(segments) => {
            for seg in segments {
                if let FStringSegment::Expr(e) = seg {
                    walk_expr_calls(e, on_call);
                }
            }
        }
        ExprKind::ListLit { elements, .. }
        | ExprKind::SetLit { elements, .. }
        | ExprKind::TupleLit { elements, .. }
        | ExprKind::Par { elements, .. } => {
            for e in elements {
                walk_expr_calls(e, on_call);
            }
        }
        ExprKind::DictLit { entries, .. } => {
            for (k, v) in entries {
                walk_expr_calls(k, on_call);
                walk_expr_calls(v, on_call);
            }
        }
        ExprKind::Unary { operand, .. } => walk_expr_calls(operand, on_call),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr_calls(lhs, on_call);
            walk_expr_calls(rhs, on_call);
        }
        ExprKind::Call { callee, args, .. } => {
            walk_expr_calls(callee, on_call);
            for a in args {
                walk_expr_calls(&a.value, on_call);
            }
            on_call(CallSite::Call {
                span: expr.span,
                callee,
                args,
            });
        }
        ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            walk_expr_calls(receiver, on_call);
            for a in args {
                walk_expr_calls(&a.value, on_call);
            }
            on_call(CallSite::Method {
                span: expr.span,
                receiver,
                method,
                args,
            });
        }
        ExprKind::FieldAccess { target, .. } | ExprKind::TupleIndex { target, .. } => {
            walk_expr_calls(target, on_call);
        }
        ExprKind::Index { target, index } => {
            walk_expr_calls(target, on_call);
            walk_expr_calls(index, on_call);
        }
        ExprKind::Question { target } => walk_expr_calls(target, on_call),
        ExprKind::Pipe(pipe) => walk_pipe_calls(pipe, on_call),
        ExprKind::If(if_expr) => walk_if_calls(if_expr, on_call),
        ExprKind::Match { scrutinee, arms } => {
            walk_expr_calls(scrutinee, on_call);
            for arm in arms {
                match &arm.body {
                    MatchArmBody::Expr(e) => walk_expr_calls(e, on_call),
                    MatchArmBody::Block(b) => walk_block_calls(b, on_call),
                }
            }
        }
        ExprKind::Grouping(inner) => walk_expr_calls(inner, on_call),
    }
}

fn walk_pipe_calls<'a>(pipe: &'a crate::ast::PipeExpr, on_call: &mut dyn FnMut(CallSite<'a>)) {
    walk_expr_calls(&pipe.source, on_call);
    for stage in &pipe.stages {
        match &stage.callee {
            PipeCallee::Bare(callee) => {
                walk_expr_calls(callee, on_call);
                on_call(CallSite::Pipe {
                    span: stage.span,
                    callee,
                    source: &pipe.source,
                    args: None,
                });
            }
            PipeCallee::WithArgs { callee, args } => {
                walk_expr_calls(callee, on_call);
                for arg in args {
                    if !arg.is_placeholder {
                        walk_expr_calls(&arg.value, on_call);
                    }
                }
                on_call(CallSite::Pipe {
                    span: stage.span,
                    callee,
                    source: &pipe.source,
                    args: Some(args),
                });
            }
        }
    }
}

fn walk_if_calls<'a>(if_expr: &'a IfExpr, on_call: &mut dyn FnMut(CallSite<'a>)) {
    walk_expr_calls(&if_expr.cond, on_call);
    walk_block_calls(&if_expr.then_branch, on_call);
    match &if_expr.else_branch {
        ElseBranch::Block(b) => walk_block_calls(b, on_call),
        ElseBranch::ElseIf(inner) => walk_if_calls(inner, on_call),
    }
}

/// EffectCheck's preparatory walk. Walks `decl`'s body once and determines, for each
/// parameter (limited to function-typed ones), "whether this parameter itself appears
/// directly as the callee of a call within the body". Requires no type checking at all
/// -- decided purely by name resolution (which parameter this Ident refers to).
/// Unaffected by mutual recursion or hoisting (EFFECT-HOF-POLYMORPHISM decision).
#[must_use]
pub fn compute_hof_forwarding(decl: &FunctionDecl) -> Vec<bool> {
    fn mark(expr: &Expr, decl: &FunctionDecl, forwarding: &mut [bool]) {
        if let ExprKind::Ident(name) = &expr.kind
            && let Some(index) = decl.params.iter().position(|param| {
                param.name.as_ref() == name.as_ref()
                    && matches!(param.ty.kind, crate::ast::TypeAnnKind::Function { .. })
            })
        {
            forwarding[index] = true;
        }
    }

    let mut forwarding = vec![false; decl.params.len()];
    walk_block_calls(&decl.body, &mut |site: CallSite<'_>| match site {
        CallSite::Call { callee, .. } | CallSite::Pipe { callee, .. } => {
            mark(callee, decl, &mut forwarding);
        }
        CallSite::Method { method, args, .. } if is_stdlib_hof_method(method) => {
            for arg in args {
                mark(&arg.value, decl, &mut forwarding);
            }
        }
        CallSite::Method { .. } => {}
    });
    forwarding
}

/// Reads `Ty::Function::effects` (via `resolutions.expr_ty`) for the argument expression
/// at each position where the forwarding mask `mask` (in callee-parameter order) is
/// true, and returns their union.
fn forwarded_arg_effects(mask: &[bool], args: &[Arg], program: &Program) -> EffectSet {
    let mut acc = EffectSet::empty();
    for (i, &forwarded) in mask.iter().enumerate() {
        if !forwarded {
            continue;
        }
        let Some(arg) = args.get(i) else { continue };
        if let Some(e) = program
            .resolutions
            .expr_ty
            .get(&arg.value.id)
            .and_then(as_function_effects)
        {
            acc = acc.union(e);
        }
    }
    acc
}

/// The effect of a single `Call` expression. (a) If the callee is a top-level function:
/// its declared `uses` plus effects forwarded via `hof_forwarding`. (b) Otherwise (a
/// closure call on a function value bound to a local variable/parameter, D-EFF-02): uses
/// the callee expression's own determined type as-is (`resolutions.expr_ty`; per D-FUNC-
/// 02 a lambda's actual effects are carried directly in its type). Builtin callees such
/// as print/eprint/assert/int/float/str/set/Ok/Err/Some/struct construction do not go
/// through `check_expr`, so no `expr_ty` gets recorded for the callee (per check_expr.rs's
/// implementation), and this naturally falls back to `EffectSet::empty()` -- which is
/// correct, since these require no effect or are pure per SPEC.
fn effect_of_call(callee: &Expr, args: &[Arg], program: &Program) -> EffectSet {
    if let ExprKind::Ident(name) = &callee.kind
        && let Some(f) = program.functions.get(name.as_ref())
    {
        let mut acc = effects_from_names(&f.effects);
        if let Some(mask) = program.resolutions.hof_forwarding.get(&f.id) {
            acc = acc.union(forwarded_arg_effects(mask, args, program));
        }
        return acc;
    }
    program
        .resolutions
        .expr_ty
        .get(&callee.id)
        .and_then(as_function_effects)
        .unwrap_or_else(EffectSet::empty)
}

/// The effect of a single `MethodCall` expression. Determines among 4 cases: a namespace
/// call (`fs.read(..)`, etc.), a user struct method, a STDLIB higher-order method
/// (map/filter/...), or any other pure STDLIB method.
fn effect_of_method_call(
    receiver: &Expr,
    method: &str,
    args: &[Arg],
    program: &Program,
) -> EffectSet {
    if let Some(ns) = program.resolutions.namespace_ref.get(&receiver.id) {
        return namespace_effect(*ns);
    }
    if let Some(recv_ty) = program.resolutions.expr_ty.get(&receiver.id)
        && let Ty::Named { name, .. } = recv_ty
        && let Some(struct_decl) = program.structs.get(name.as_ref())
        && let Some(m) = struct_decl
            .methods
            .iter()
            .find(|m| m.name.as_ref() == method)
    {
        let mut acc = effects_from_names(&m.effects);
        if let Some(mask) = program.resolutions.hof_forwarding.get(&m.id) {
            acc = acc.union(forwarded_arg_effects(mask, args, program));
        }
        return acc;
    }
    if method == "shuffle" {
        return EffectSet::RAND;
    }
    if is_stdlib_hof_method(method) {
        let mut acc = EffectSet::empty();
        for a in args {
            if let Some(e) = program
                .resolutions
                .expr_ty
                .get(&a.value.id)
                .and_then(as_function_effects)
            {
                acc = acc.union(e);
            }
        }
        return acc;
    }
    EffectSet::empty()
}

fn effect_of_pipe_call(
    callee: &Expr,
    source: &Expr,
    args: Option<&[Arg]>,
    program: &Program,
) -> EffectSet {
    if let ExprKind::FieldAccess { target, .. } = &callee.kind
        && let Some(namespace) = program.resolutions.namespace_ref.get(&target.id)
    {
        return namespace_effect(*namespace);
    }
    if let ExprKind::Ident(name) = &callee.kind
        && let Some(function) = program.functions.get(name.as_ref())
    {
        let mut effects = effects_from_names(&function.effects);
        if let Some(mask) = program.resolutions.hof_forwarding.get(&function.id) {
            for (index, forwarded) in mask.iter().copied().enumerate() {
                if !forwarded {
                    continue;
                }
                let expression = match args {
                    None if index == 0 => Some(source),
                    Some(args) => args.get(index).map(|arg| {
                        if arg.is_placeholder {
                            source
                        } else {
                            &arg.value
                        }
                    }),
                    None => None,
                };
                if let Some(effect) = expression
                    .and_then(|expr| program.resolutions.expr_ty.get(&expr.id))
                    .and_then(as_function_effects)
                {
                    effects = effects.union(effect);
                }
            }
        }
        return effects;
    }
    program
        .resolutions
        .expr_ty
        .get(&callee.id)
        .and_then(as_function_effects)
        .unwrap_or_else(EffectSet::empty)
}

fn effect_of_call_site(site: &CallSite<'_>, program: &Program) -> EffectSet {
    match site {
        CallSite::Call { callee, args, .. } => effect_of_call(callee, args, program),
        CallSite::Method {
            receiver,
            method,
            args,
            ..
        } => effect_of_method_call(receiver, method, args, program),
        CallSite::Pipe {
            callee,
            source,
            args,
            ..
        } => effect_of_pipe_call(callee, source, *args, program),
    }
}

/// The core of `decl`'s effect checking. Reports E2002 if the combined effects from (a)
/// are not a subset of the declared `uses` (the forwarding effects from (b) are not
/// included in this subset check, per the end of §5.5).
///
/// The E2001/E2002 split (judgment call made in this file): D-DIAG-02 defines E2001 as
/// "an effectful call inside a pure function" and E2002 as "use of an undeclared effect
/// (a higher-order function's effect row overflow)". Since a "pure function" is, by SPEC
/// §8's definition, precisely a function with "no uses declaration", this uses E2001 when
/// `declared.is_empty()` (uses is empty), and E2002 otherwise (some effects are declared
/// but still exceeded) -- this matches the 3 cases in
/// samples/err/static/8_effect_errors (a pure function's direct/indirect violation is
/// E2001; overflow via a higher-order function in a function declaring `uses {fs}` is
/// E2002).
pub fn check_function_effects(
    decl: &FunctionDecl,
    program: &Program,
    diagnostics: &mut DiagnosticBag,
) {
    for effect in &decl.effects {
        if EffectSet::from_name(effect).is_none() {
            diagnostics.push(Diagnostic {
                code: ErrorCode::InvalidEffectName,
                span: decl.span,
                message: format!("unknown effect '{effect}' in uses declaration"),
            });
            return;
        }
    }
    let declared = effects_from_names(&decl.effects);
    let mut acc = EffectSet::empty();
    let mut violation_span: Option<Span> = None;
    walk_block_calls(&decl.body, &mut |site: CallSite<'_>| {
        let site_effect = effect_of_call_site(&site, program);
        acc = acc.union(site_effect);
        if violation_span.is_none() && !site_effect.is_subset_of(declared) {
            violation_span = Some(site.span());
        }
    });
    if acc.is_subset_of(declared) {
        return;
    }
    let span = violation_span.unwrap_or(decl.span);
    if declared.is_empty() {
        diagnostics.push(Diagnostic {
            code: ErrorCode::ImpureCallInPureFunction,
            span,
            message: format!(
                "function '{}' is a pure function with no uses declaration, but contains a call with effect {{{}}} (D-EFF-01)",
                decl.name,
                acc.names().collect::<Vec<_>>().join(", ")
            ),
        });
    } else {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UndeclaredEffect,
            span,
            message: format!(
                "the effect {{{}}} required by calls in function '{}' is not a superset of the declared uses {{{}}} (D-FUNC-03)",
                decl.name,
                acc.names().collect::<Vec<_>>().join(", "),
                declared.names().collect::<Vec<_>>().join(", ")
            ),
        });
    }
}

/// The entry point of the EffectCheck phase. First runs `compute_hof_forwarding` over
/// every declaration to fill in `resolutions.hof_forwarding`, then applies
/// `check_function_effects` to each declaration (a 2-stage structure, §4.2 "why
/// EffectCheck writes Resolutions.hof_forwarding"). When, per the [`ENTRY_POINT_NAME`]
/// convention, the top level exists in `program.functions` as a synthesized function, it
/// is excluded from effect checking since the top level implicitly permits every effect
/// (SPEC §8).
pub fn check_program_effects(program: &mut Program, diagnostics: &mut DiagnosticBag) {
    let function_names: Vec<Arc<str>> = program
        .functions
        .keys()
        .filter(|n| n.as_ref() != ENTRY_POINT_NAME)
        .cloned()
        .collect();
    let struct_names: Vec<Arc<str>> = program.structs.keys().cloned().collect();

    // Stage 1: computing hof_forwarding (a purely syntactic fact unrelated to types/effects, §5.5).
    for name in &function_names {
        let decl = Arc::clone(&program.functions[name]);
        let mask = compute_hof_forwarding(&decl);
        program.resolutions.hof_forwarding.insert(decl.id, mask);
    }
    for name in &struct_names {
        let decl = Arc::clone(&program.structs[name]);
        for m in &decl.methods {
            let mask = compute_hof_forwarding(m);
            program.resolutions.hof_forwarding.insert(m.id, mask);
        }
    }

    // Stage 2: the ordinary combined-effect check (D-FUNC-03).
    for name in &function_names {
        let decl = Arc::clone(&program.functions[name]);
        check_function_effects(&decl, program, diagnostics);
    }
    for name in &struct_names {
        let decl = Arc::clone(&program.structs[name]);
        for m in &decl.methods {
            check_function_effects(m, program, diagnostics);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{FunctionDecl, Item, NodeId, Stmt, TypeAnn, TypeAnnKind};
    use crate::diagnostics::{Diagnostic, ErrorCode, Position, SourceMap, Span};
    use crate::lexer::Lexer;
    use crate::module_resolve::{build_program_skeleton, discover_sibling_modules};
    use crate::types::check_decl::check_program;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn sample_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn dummy_span(file: crate::diagnostics::FileId) -> Span {
        Span {
            file,
            start: Position { line: 0, col: 0 },
            end: Position { line: 0, col: 0 },
        }
    }

    /// Runs `entry_path` (plus sibling modules) all the way through lex/parse/
    /// module_resolve/TypeCheck, registers the entry's top-level executable statements
    /// into `program.functions` as a synthesized `FunctionDecl` per the
    /// [`ENTRY_POINT_NAME`] convention, and then runs [`check_program_effects`].
    /// Reproduces for testing purposes the wiring driver.rs (Unit17) should ultimately
    /// perform (see "adjustment needed in a file outside this Unit's scope" at the top of
    /// this file; the same approach as check_decl.rs's existing tests).
    fn run_effect_check(entry_path: &Path) -> (Vec<Diagnostic>, Arc<SourceMap>) {
        let mut sibling_paths = discover_sibling_modules(entry_path);
        let mut all_paths = vec![entry_path.to_path_buf()];
        all_paths.append(&mut sibling_paths);

        let mut sources = SourceMap::new();
        let mut modules = Vec::new();
        for path in &all_paths {
            let text = fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
            let file = sources.add(path.clone(), text.clone());
            let (tokens, _comments, lex_diag) = Lexer::new(&text, file).tokenize();
            assert!(
                lex_diag.is_empty(),
                "{}: unexpected lex error",
                path.display()
            );
            let (module, parse_diag) = crate::parser::parse_module(&tokens, file);
            assert!(
                parse_diag.is_empty(),
                "{}: unexpected parse error",
                path.display()
            );
            modules.push(module);
        }

        let entry_text = fs::read_to_string(entry_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", entry_path.display()));
        let mut entry_sources = SourceMap::new();
        let entry_file = entry_sources.add(entry_path.to_path_buf(), entry_text.clone());
        let (entry_tokens, _c, entry_lex_diag) = Lexer::new(&entry_text, entry_file).tokenize();
        assert!(
            entry_lex_diag.is_empty(),
            "lex error while re-parsing the entry"
        );
        let (entry_module, entry_parse_diag) =
            crate::parser::parse_module(&entry_tokens, entry_file);
        assert!(
            entry_parse_diag.is_empty(),
            "parse error while re-parsing the entry"
        );
        let entry_stmts: Vec<Stmt> = entry_module
            .items
            .into_iter()
            .filter_map(|item| match item {
                Item::Stmt(s) => Some(s),
                Item::Decl(_) => None,
            })
            .collect();

        let mut diagnostics = DiagnosticBag::new();
        let sources_arc = Arc::new(sources);
        let mut program =
            build_program_skeleton(modules, Arc::clone(&sources_arc), &mut diagnostics);
        check_program(&mut program, &entry_stmts, &mut diagnostics);

        // ENTRY_POINT_NAME convention: register the top-level executable statements as a
        // synthesized FunctionDecl (the wiring driver.rs should ultimately perform, see
        // the comment at the top of this file).
        let dummy = dummy_span(entry_file);
        let entry_decl = FunctionDecl {
            id: NodeId(u32::MAX),
            name: Arc::from(ENTRY_POINT_NAME),
            generics: Vec::new(),
            self_param: None,
            params: Vec::new(),
            ret: TypeAnn {
                kind: TypeAnnKind::Void,
                span: dummy,
            },
            effects: Vec::new(),
            body: Block {
                stmts: entry_stmts,
                span: dummy,
            },
            leading_comments: Vec::new(),
            doc_comment: None,
            span: dummy,
        };
        program
            .functions
            .insert(Arc::from(ENTRY_POINT_NAME), Arc::new(entry_decl));

        check_program_effects(&mut program, &mut diagnostics);
        let final_sources = Arc::clone(&program.sources);
        let sorted = diagnostics.into_sorted(&final_sources);
        (sorted, final_sources)
    }

    fn e2xxx_codes(diags: &[Diagnostic]) -> Vec<String> {
        let mut codes: Vec<String> = diags
            .iter()
            .filter(|d| d.code.numeric() / 1000 == 2)
            .map(|d| d.code.to_string())
            .collect();
        codes.sort();
        codes
    }

    fn subdirs(rel: &str) -> Vec<PathBuf> {
        let base = sample_path(rel);
        let Ok(entries) = fs::read_dir(&base) else {
            panic!("cannot read {}", base.display());
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs
    }

    #[test]
    fn ok_8_effects_sample_has_zero_e2xxx() {
        let dir = sample_path("samples/ok/8_effects");
        for entry in ["entry_main.ybm", "entry_transitive_and_hof_effects.ybm"] {
            let (diags, sources) = run_effect_check(&dir.join(entry));
            let codes = e2xxx_codes(&diags);
            assert!(
                codes.is_empty(),
                "{entry}: unexpected E2xxx: {codes:?}\n  all: {:?}",
                diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn err_static_8_effect_errors_match_expected() {
        let dir = sample_path("samples/err/static/8_effect_errors");
        let cases = [
            ("entry_pure_function_direct_impure_call.ybm", vec!["E2001"]),
            (
                "entry_pure_function_indirect_via_stored_lambda.ybm",
                vec!["E2001"],
            ),
            ("entry_undeclared_effect_row_overflow.ybm", vec!["E2002"]),
        ];
        for (entry, expected) in cases {
            let (diags, sources) = run_effect_check(&dir.join(entry));
            let codes = e2xxx_codes(&diags);
            assert_eq!(
                codes,
                expected,
                "{entry}: all diagnostics: {:?}",
                diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
            );
        }
    }

    /// Verifies that effect checking reports zero errors across every directory under
    /// samples/ok/ (per the Unit8 task instruction "zero effect-check errors across every
    /// directory in samples/ok/").
    #[test]
    fn all_ok_samples_have_zero_e2xxx() {
        let mut failures = Vec::new();
        for dir in subdirs("samples/ok") {
            let Ok(read_dir) = fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<PathBuf> = read_dir
                .filter_map(std::result::Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ybm"))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("entry_"))
                })
                .collect();
            entries.sort();
            for entry_path in entries {
                let (diags, sources) = run_effect_check(&entry_path);
                let codes = e2xxx_codes(&diags);
                if !codes.is_empty() {
                    failures.push(format!(
                        "{}: unexpected E2xxx: {:?}\n  all: {:?}",
                        entry_path.display(),
                        codes,
                        diags.iter().map(|d| d.render(&sources)).collect::<Vec<_>>()
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n---\n"));
    }
}
