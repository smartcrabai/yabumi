//! Abstract syntax tree (ARCHITECTURE.md §3.4-3.6). Plain data with no behavior, so that fmt
//! can "reproduce the original syntax exactly" without mixing in resolution results from the
//! type-checking phase (those results are separated into the `types::resolutions::Resolutions`
//! side table, §3.7).

pub mod decl;
pub mod expr;
pub mod pattern;
pub mod stmt;
pub mod ty_ann;

pub use decl::{
    Decl, DocComment, DocFence, DocPart, EnumDecl, EnumVariant, FunctionDecl, Item, LeadingComment,
    Module, Param, SelfParam, StructDecl,
};
pub use expr::{
    Arg, BinaryOp, ElseBranch, Expr, ExprKind, FStringSegment, IfExpr, LambdaParam, MatchArm,
    MatchArmBody, ParKind, PipeCallee, PipeExpr, PipeStage, UnaryOp,
};
pub use pattern::{LiteralPat, Pattern, SubPattern};
pub use stmt::{Block, Stmt, StmtKind};
pub use ty_ann::{TypeAnn, TypeAnnKind};

/// A monotonically increasing number the parser assigns to syntax elements (expressions,
/// declarations, match arms, etc. -- nodes that later need resolved information). AST nodes
/// themselves hold no other resolved information (§3.4, "the NodeId unification mechanism").
///
/// Allocation policy: the `Parser` in `parser/mod.rs` holds a single `next_id: u32` counter
/// and consumes one by calling `self.next_id()` each time it constructs
/// `Expr`/`FunctionDecl`/`StructDecl`/`EnumDecl`/`Pattern::BareIdent`/`SubPattern::BareIdent`
/// -- that is, every node kind that carries an `id: NodeId` or a `NodeId` argument. It only
/// increases monotonically in parse order (from the start of the file); the value itself has
/// no meaning (it is used only as a hash map key into `Resolutions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);
