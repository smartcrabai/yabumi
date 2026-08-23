//! Side table of `NodeId` -> resolved facts (ARCHITECTURE.md §3.7). Resolved facts left
//! by the type checking phase (part of it is the EffectCheck phase, see §4.2) for later
//! phases (evaluator, lint, doctest) to consume. The AST nodes themselves are never
//! mutated (§3.4).

use super::Ty;
use crate::ast::NodeId;
use std::collections::HashMap;

#[derive(Default)]
pub struct Resolutions {
    /// Declaration-order index of the field referenced by `FieldAccess` / a struct-construction `Arg`.
    pub field_index: HashMap<NodeId, u32>,
    /// Type arguments determined by unification for a generic call (`Call`/`MethodCall`).
    pub type_args: HashMap<NodeId, Vec<Ty>>,
    /// Target type determined by the assignment-target annotation, e.g. for `json.decode` (D-TYPE-16, detailed in §5.3).
    pub decode_target: HashMap<NodeId, Ty>,
    /// Whether a `Pattern::BareIdent` / `SubPattern::BareIdent` is a unit variant or a new binding.
    pub bare_ident_kind: HashMap<NodeId, BareIdentKind>,
    /// Whether a `Call`'s callee is a struct construction / enum variant construction /
    /// ordinary call (the resolution result of the unified `Call` representation described in §3.4).
    pub call_kind: HashMap<NodeId, CallKind>,
    /// The determined type of each expression (eval generally does not consult `Ty`, but some builtins such as decode need it).
    pub expr_ty: HashMap<NodeId, Ty>,
    /// D-TYPE-17 implicit-wrap decision for a `return` target expression. The key is the
    /// `NodeId` of the returned `Expr` (IMPLICIT-WRAP-NO-RESOLUTIONS-FIELD decision, §8).
    /// No entry = no wrap (priority 1, matches the annotation as-is).
    pub implicit_wrap: HashMap<NodeId, WrapKind>,
    /// Resolution result when the receiver `Ident` expression of a `.`-qualified access
    /// refers to a builtin namespace (NAMESPACE-QUALIFIED-ACCESS-NO-RESOLUTION-HOME decision,
    /// §8). No entry = evaluate as an ordinary local variable / top-level identifier.
    pub namespace_ref: HashMap<NodeId, super::NamespaceId>,
    /// Static row type for `list[Row] |> csv.encode` when the runtime list is empty.
    pub csv_encode_target: HashMap<NodeId, Ty>,
    /// For a function/method declaration (`NodeId` is its `FunctionDecl.id`), bit flags
    /// marking which parameters have a function type and are actually invoked within the
    /// body (= should be forwarded for effect polymorphism) (EFFECT-HOF-POLYMORPHISM
    /// decision, §5.5/§8). The only field written by the EffectCheck phase -- still empty
    /// when TypeCheck completes.
    pub hof_forwarding: HashMap<NodeId, Vec<bool>>,
}

impl Resolutions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// D-TYPE-17 priority-2 implicit-wrap kind. Priority 1 (matches the annotation as-is)
/// needs no wrap, so it has no variant -- it is represented by the absence of an entry
/// in `Resolutions::implicit_wrap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapKind {
    Ok,
    Some,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareIdentKind {
    UnitVariant,
    Binding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    StructInit,
    EnumVariantInit,
    FunctionCall,
    ClosureCall,
}
