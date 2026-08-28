//! `TypeEnv` (a stack of scopes, ARCHITECTURE.md §2.1). Resolves the types of
//! variables/parameters during type checking. Distinct from the evaluator's
//! `eval::env::Environment` (this one holds only types and lives only during the type
//! checking phase) -- however the implementation approach itself, "hold scopes as a stack
//! of Vec<HashMap<_,_>>, push/pop at block boundaries", is kept aligned with
//! `eval::env::Environment`'s `Frame.scopes` (judgment call made in this file).
//!
//! At the stub stage the design was a "linked list holding a borrowed reference to the
//! parent" (`parent: Option<&'parent TypeEnv<'parent>>`), but it was changed to an
//! ownership-based stack for the main implementation -- because in the usage pattern
//! where `check_expr`/`check_stmt` recursively pass around `&mut TypeEnv<'_>` and create
//! and discard many levels of child scopes per if/match branch or lambda body, it turned
//! out that the borrow lifetime `'parent` required by `child(&'parent self)` and the
//! borrow lifetime of the `&mut` argument actually at hand would conflict, causing
//! constant lifetime-handling trouble across the whole type-check implementation
//! (judgment call made in this file -- specifically, calling `.child()` inside a
//! recursive function that takes `&mut TypeEnv<'_>` as a parameter across a function
//! boundary would require that borrow to have the same length as the struct's own type
//! parameter, which an ordinary `&mut` argument cannot satisfy).

use crate::diagnostics::Span;
use crate::types::Ty;
use std::collections::HashMap;
use std::sync::Arc;

/// The type and mutability of a single binding (whether it is a `var` binding, D-MUT-01
/// through 04).
#[derive(Debug, Clone)]
pub struct Binding {
    pub ty: Ty,
    /// `true` for a binding that should be treated as a "var binding": a `var` declaration
    /// (`var x = ..`) or a `var self` parameter. `false` for an ordinary `x = expr`
    /// (immutable) or an ordinary function parameter (D-MUT-04: always passed by value
    /// copy and cannot even be reassigned).
    pub mutable: bool,
    /// Source span of the binding's declaration.
    pub def_span: Span,
}

type Scope = HashMap<Arc<str>, Binding>;

/// The variable environment during type checking. A stack of scopes (push/pop at each
/// block boundary).
///
/// `generics` is the list of type-parameter names owned by the function/method currently
/// under check (and any enclosing struct/enum declaration) (D-FUNC-04). Used by
/// `check_expr`/`check_stmt` to determine type annotations within the body
/// (the `generics_in_scope` argument of `generics::ty_from_ann`), and whether a
/// pre-unification `Ty::TypeVar` is "an unresolved type parameter of the function
/// currently under check" (D-FUNC-05's E1013 determination). Once set at each function
/// boundary, it is automatically inherited by every child scope pushed afterward -- this
/// avoids adding a separate `generics_in_scope` argument to `check_expr`/`check_stmt`'s
/// parameter list (avoiding clippy::too_many_arguments, judgment call made in this file).
pub struct TypeEnv {
    scopes: Vec<Scope>,
    generics: Vec<Arc<str>>,
}

impl TypeEnv {
    /// For checking top-level statements (belonging to no function, no type parameters).
    #[must_use]
    pub fn root() -> Self {
        Self::for_function(Vec::new())
    }

    /// Used when starting to check a function/method body. `generics` is that function's
    /// own type parameters (for a method, the caller also passes along the enclosing
    /// struct declaration's type parameters).
    #[must_use]
    pub fn for_function(generics: Vec<Arc<str>>) -> Self {
        Self {
            scopes: vec![HashMap::new()],
            generics,
        }
    }

    /// Call this when entering a block boundary (an if/match branch, a lambda body). Be
    /// sure to always call the matching `pop_scope` (once that block's checking finishes)
    /// -- the type-checking counterpart of `Environment::push_scope` (`eval/env.rs`).
    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Closes the innermost scope opened by `push_scope`.
    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Writes into the current (innermost) scope as a `var` binding (reassignable, can be
    /// the root of mutability propagation, D-MUT-01 through 04) if `mutable=true`, or as
    /// an immutable binding if `false`. An existing binding with the same name is
    /// overwritten (as same-scope shadowing) -- determining the legality of the shadowing
    /// itself is lint's responsibility (D-LINT-03), and type checking does not block it
    /// here.
    pub fn bind(&mut self, name: Arc<str>, ty: Ty, mutable: bool, def_span: Span) {
        self.scopes
            .last_mut()
            .unwrap_or_else(|| unreachable!("TypeEnv always has at least one scope"))
            .insert(
                name,
                Binding {
                    ty,
                    mutable,
                    def_span,
                },
            );
    }

    /// Recursively searches from the innermost scope outward. D-LINT-03 (shadowing)
    /// determination is done separately by lint/shadowing.rs, so this simply returns the
    /// innermost binding.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    /// The list of type-parameter names owned by the function/method currently under
    /// check (including any enclosing struct/enum declaration) (D-FUNC-04).
    #[must_use]
    pub fn generics(&self) -> &[Arc<str>] {
        &self.generics
    }
}

#[cfg(test)]
mod tests {
    use super::TypeEnv;
    use crate::diagnostics::{FileId, Position, Span};
    use crate::types::Ty;
    use std::sync::Arc;

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: Position { line: 1, col: 1 },
            end: Position { line: 1, col: 1 },
        }
    }

    #[test]
    fn lookup_finds_binding_in_parent_scope() {
        let mut env = TypeEnv::root();
        env.bind(Arc::from("x"), Ty::Int, false, dummy_span());
        env.push_scope();
        let found = env.lookup("x");
        assert!(matches!(found.map(|b| &b.ty), Some(Ty::Int)));
        assert!(!found.unwrap_or_else(|| unreachable!()).mutable);
        env.pop_scope();
    }

    #[test]
    fn child_binding_shadows_parent_without_mutating_it() {
        let mut env = TypeEnv::root();
        env.bind(Arc::from("x"), Ty::Int, false, dummy_span());
        env.push_scope();
        env.bind(Arc::from("x"), Ty::Str, true, dummy_span());
        assert!(matches!(env.lookup("x").map(|b| &b.ty), Some(Ty::Str)));
        env.pop_scope();
        assert!(matches!(env.lookup("x").map(|b| &b.ty), Some(Ty::Int)));
    }

    #[test]
    fn generics_are_inherited_by_nested_scopes() {
        let mut env = TypeEnv::for_function(vec![Arc::from("T")]);
        env.push_scope();
        assert_eq!(env.generics()[0].as_ref(), "T");
        env.pop_scope();
    }
}
