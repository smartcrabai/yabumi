//! `Environment` (the Frame/Scope stack), `Program` (immutable globals) (ARCHITECTURE.md §3.11).
//!
//! Yabumi has no mutable upvalues (reference capture) via closures (D-MUT-04) — a capture
//! is always completed by copying the value. So the "environment = a parent-child chain of
//! `Arc<RefCell<HashMap<..>>>`" design that many interpreters adopt is unnecessary, and the
//! environment can be implemented as a simple owned stack of scopes (requiring no interior
//! mutability cells at all).

use super::value::Value;
use crate::ast::{EnumDecl, FunctionDecl, StructDecl};
use crate::diagnostics::{SourceMap, Span};
use crate::types::Resolutions;
use std::collections::HashMap;
use std::sync::Arc;

/// The frame corresponding to a single function/lambda call. Variable lookup never crosses
/// frame boundaries (a top-level function's body only sees its own arguments plus global
/// declarations, ordinary static scoping. Only a lambda injects a copy of the outer values
/// into its initial scope at frame-creation time, via capture).
struct Frame {
    /// Pushed/popped per block, for if/match/lambda bodies etc.
    scopes: Vec<Scope>,
}

type Scope = HashMap<Arc<str>, Value>;

/// The variable environment at evaluation time. Whether something is a `var` has already
/// been settled during the type-checking phase (a consistently compiled program contains
/// no invalid mutation), so this holds no mutability flag at all here — it holds only the
/// `Value` itself.
pub struct Environment {
    frames: Vec<Frame>,
}

impl Environment {
    #[must_use]
    pub fn with_frame(initial: Scope) -> Self {
        Self {
            frames: vec![Frame {
                scopes: vec![initial],
            }],
        }
    }

    /// At least one frame always exists (an invariant guaranteed by the very way
    /// `Environment` is constructed, the R3 decision, §8), so this is expressed with
    /// `unreachable!()`.
    fn current_frame(&self) -> &Frame {
        self.frames
            .last()
            .unwrap_or_else(|| unreachable!("Environment always has at least one frame"))
    }

    fn current_frame_mut(&mut self) -> &mut Frame {
        self.frames
            .last_mut()
            .unwrap_or_else(|| unreachable!("Environment always has at least one frame"))
    }

    pub fn lookup_mut(&mut self, name: &str) -> &mut Value {
        self.current_frame_mut()
            .scopes
            .iter_mut()
            .rev()
            .find_map(|s| s.get_mut(name))
            .unwrap_or_else(|| unreachable!("already type-checked, so the name must exist: {name}"))
    }

    /// Checks whether `name` is visible as a local variable in the current frame (if not
    /// found, this is deferred to name resolution on the flat-namespace side — top-level
    /// functions/constants/enum variants etc. — per the name-resolution priority order in
    /// ARCHITECTURE.md §5.12, where a local variable takes highest priority).
    #[must_use]
    pub fn try_lookup(&self, name: &str) -> Option<&Value> {
        self.current_frame()
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
    }

    /// Creates a new binding in the current innermost scope (shared by `var` declarations,
    /// new bindings from a bare assignment, pattern bindings in a match arm, and
    /// function/lambda parameter bindings).
    pub fn bind(&mut self, name: Arc<str>, value: Value) {
        self.current_frame_mut()
            .scopes
            .last_mut()
            .unwrap_or_else(|| {
                unreachable!(
                    "a Frame always has at least one scope (guaranteed by with_frame/push_scope)"
                )
            })
            .insert(name, value);
    }

    /// Pushes a new scope onto the current frame (the boundary of an if/match/lambda body).
    pub fn push_scope(&mut self) {
        self.current_frame_mut().scopes.push(Scope::new());
    }

    /// Pops the current frame's innermost scope.
    pub fn pop_scope(&mut self) {
        self.current_frame_mut().scopes.pop();
    }

    pub fn push_frame(&mut self, initial: Scope) {
        self.frames.push(Frame {
            scopes: vec![initial],
        });
    }

    pub fn pop_frame(&mut self) {
        self.frames.pop();
    }

    /// Enumerates by value copy every variable visible in the current frame (merging all
    /// scopes outer-to-inner, with inner scopes shadowing same-named outer ones). This
    /// implementation is shared by lambda capture (D-MUT-04, eval/expr.rs) and `par`'s
    /// snapshot (`snapshot_for_par` below, §5.8) — both are the same operation of "copy by
    /// value every binding currently visible from this scope".
    pub(super) fn visible_bindings(&self) -> Vec<(Arc<str>, Value)> {
        let mut merged: HashMap<Arc<str>, Value> = HashMap::new();
        for scope in &self.current_frame().scopes {
            for (k, v) in scope {
                merged.insert(Arc::clone(k), v.clone());
            }
        }
        merged.into_iter().collect()
    }

    /// Builds the independent copy passed to each branch of `par`/`par_map`/`par_each`.
    /// `Value::clone()`s every variable visible in the current frame (just bumping the Arc
    /// reference count, a D-MUT-04 value copy). No RefCell/Mutex needed — each element
    /// holds a fully independent copy of `Environment` (§5.8).
    #[must_use]
    pub fn snapshot_for_par(&self) -> Self {
        let scope: Scope = self.visible_bindings().into_iter().collect();
        Self::with_frame(scope)
    }
}

/// The single whole-program picture per `ybm` invocation, finalized once
/// `module_resolve` completes. Since it is never modified after construction, it can be
/// safely shared between `par`'s worker threads as an `Arc<Program>` (no interior
/// mutability or locking needed at all). Every field is an Arc/`HashMap<Arc<str>,_>`, and
/// even the identifier fields of the AST nodes that are its values (e.g. `FunctionDecl`)
/// are themselves `Arc<str>` (the R1 decision), so `Program` as a whole is Send+Sync — this
/// satisfies the `F: Send` requirement of `spawn_scoped` for moving `Arc<Program>` into
/// `par`'s worker threads.
///
/// The fields are completed in stages as the pipeline progresses: at the point where
/// `ModuleResolve` has built the skeleton, `resolutions` is empty; `TypeCheck` fills
/// everything except `hof_forwarding`; `EffectCheck` fills `hof_forwarding` (driver.rs
/// passes each phase `&mut Program` in turn). Only once evaluation is about to begin is it
/// wrapped in `Arc::new(program)` and shared for the first time.
pub struct Program {
    pub functions: HashMap<Arc<str>, Arc<FunctionDecl>>,
    pub structs: HashMap<Arc<str>, Arc<StructDecl>>,
    pub enums: HashMap<Arc<str>, Arc<EnumDecl>>,
    /// Per D-MOD-02, this holds only literals, so it is evaluated once, at load time.
    pub consts: HashMap<Arc<str>, Value>,
    /// Source spans for module-level constants, keyed by their names.
    pub const_spans: HashMap<Arc<str>, Span>,
    pub resolutions: Resolutions,
    /// All source files, finalized during the Lex phase. Needed so that even from deep
    /// inside a worker thread, a `SourceMap` can be reached in order to `Diagnostic::render`
    /// on the spot and immediately terminate the process when a panic is detected within
    /// `par` (§5.8, the PAR-ABORT-NOT-ACTUALLY-IMMEDIATE decision).
    pub sources: Arc<SourceMap>,
    /// Normal CLI execution terminates immediately on a parallel panic; doctest clones disable
    /// this so the abort can become that fence's failure and later fences can still run.
    pub abort_process_on_par_panic: bool,
}

impl Program {
    #[must_use]
    pub fn new(sources: Arc<SourceMap>) -> Self {
        Self {
            functions: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
            consts: HashMap::new(),
            const_spans: HashMap::new(),
            resolutions: Resolutions::new(),
            sources,
            abort_process_on_par_panic: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Environment, Scope, Value};
    use std::sync::Arc;

    #[test]
    fn bind_and_lookup_roundtrip() {
        let mut env = Environment::with_frame(Scope::new());
        env.bind(Arc::from("x"), Value::Int(1));
        assert_eq!(env.try_lookup("x"), Some(&Value::Int(1)));
        *env.lookup_mut("x") = Value::Int(2);
        assert_eq!(env.try_lookup("x"), Some(&Value::Int(2)));
    }

    #[test]
    fn push_pop_scope_hides_inner_bindings() {
        let mut env = Environment::with_frame(Scope::new());
        env.bind(Arc::from("x"), Value::Int(1));
        env.push_scope();
        env.bind(Arc::from("y"), Value::Int(2));
        assert_eq!(env.try_lookup("y"), Some(&Value::Int(2)));
        env.pop_scope();
        assert_eq!(env.try_lookup("y"), None);
        assert_eq!(env.try_lookup("x"), Some(&Value::Int(1)));
    }

    #[test]
    fn try_lookup_missing_name_returns_none() {
        let env = Environment::with_frame(Scope::new());
        assert_eq!(env.try_lookup("missing"), None);
    }

    #[test]
    fn snapshot_for_par_is_independent_copy() {
        let mut env = Environment::with_frame(Scope::new());
        env.bind(Arc::from("shared"), Value::Int(10));
        let mut snap = env.snapshot_for_par();
        assert_eq!(snap.try_lookup("shared"), Some(&Value::Int(10)));
        *snap.lookup_mut("shared") = Value::Int(99);
        // The original env is unaffected (value copy, D-MUT-04).
        assert_eq!(env.try_lookup("shared"), Some(&Value::Int(10)));
    }
}
