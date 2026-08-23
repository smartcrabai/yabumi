//! Expressions (ARCHITECTURE.md §3.4).

use super::NodeId;
use super::decl::LeadingComment;
use super::pattern::Pattern;
use super::stmt::Block;
use super::ty_ann::TypeAnn;
use crate::diagnostics::Span;
use std::sync::Arc;

pub struct Expr {
    pub id: NodeId,
    pub kind: ExprKind,
    pub span: Span,
}

pub enum ExprKind {
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    StringLit(String),
    /// Distinct from the lexical-level `FStringPart` (lexer/fstring.rs) -- here each `{expr}`
    /// holds an already-parsed `Expr`.
    FString(Vec<FStringSegment>),

    Ident(Arc<str>),

    ListLit {
        elements: Vec<Expr>,
        was_multiline: bool,
    },
    DictLit {
        entries: Vec<(Expr, Expr)>,
        was_multiline: bool,
    },
    SetLit {
        elements: Vec<Expr>,
        was_multiline: bool,
    },
    /// A single element requires a trailing comma (D-TYPE-01), already checked by the parser.
    TupleLit {
        elements: Vec<Expr>,
        was_multiline: bool,
    },

    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },

    /// Unifies function calls, struct construction, and enum variant construction
    /// syntactically (`Ident "(" arglist ")"` has the same shape for all three -- a decision
    /// made here). Which meaning applies is settled by the type-checking phase from the
    /// name-resolution result of `callee`, and recorded into `Resolutions::call_kind` (§3.7).
    Call {
        callee: Box<Expr>,
        type_args: Vec<TypeAnn>,
        args: Vec<Arg>,
        was_multiline: bool,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: Arc<str>,
        type_args: Vec<TypeAnn>,
        args: Vec<Arg>,
        was_multiline: bool,
    },

    FieldAccess {
        target: Box<Expr>,
        field: Arc<str>,
    },
    /// `t.0` (the parser validates the numeric token).
    TupleIndex {
        target: Box<Expr>,
        index: u32,
    },
    /// `xs[i]` / `m[k]`.
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    /// `expr?`.
    Question {
        target: Box<Expr>,
    },

    Pipe(PipeExpr),
    Lambda {
        params: Vec<LambdaParam>,
        body: Box<Expr>,
    },
    If(Box<IfExpr>),
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// `par [..]` / `par (..)`.
    Par {
        kind: ParKind,
        elements: Vec<Expr>,
    },

    /// `(expr)`. Kept in the AST too, to distinguish it from a tuple (for fmt's
    /// reproducibility).
    Grouping(Box<Expr>),
}

/// An f-string interpolation segment. The result of recursively parsing the lexical-level
/// `FStringPart` (lexer/fstring.rs) -- `Expr(Box<Expr>)` is a fully parsed expression AST,
/// `Text` is the literal portion with escapes already resolved. At the time of writing,
/// ARCHITECTURE.md's body was missing this type definition, so it was added while building
/// the skeleton (see §8, "items added in this revision that are not in the critique").
pub enum FStringSegment {
    Text(String),
    Expr(Box<Expr>),
}

/// Represents named arguments (struct construction, `name: value`) and positional arguments
/// (function calls, enum variant construction) with the same shape. Which is required is
/// checked by the type-checking phase per callee kind (D-TYPE-13: structs always require
/// named arguments / D-SYN-07: enum variants are always positional / ordinary function calls
/// and closure calls on local variables are always positional, D-TYPE-11).
pub struct Arg {
    pub name: Option<Arc<str>>,
    pub value: Expr,
    /// The pipe's `_` (meaningful only when this Arg is used as a plain function-call
    /// argument).
    pub is_placeholder: bool,
}

pub struct PipeExpr {
    pub source: Box<Expr>,
    pub stages: Vec<PipeStage>,
}

pub struct PipeStage {
    pub callee: PipeCallee,
    /// A trailing `?` on this stage (the pipe itself does not auto-short-circuit a Result,
    /// SPEC §6.3).
    pub question: bool,
    pub span: Span,
}

pub enum PipeCallee {
    /// A bare name: `x |> json.encode`.
    Bare(Expr),
    /// A call that includes `_`. A syntax error (E0503) if not even one `is_placeholder` is
    /// present -- checked by the parser.
    WithArgs { callee: Box<Expr>, args: Vec<Arg> },
}

pub struct LambdaParam {
    pub name: Arc<str>,
    /// The annotation is optional (contextual inference, SPEC §5.1).
    pub ty: Option<TypeAnn>,
    pub span: Span,
}

/// An `if` is always an expression, and nothing in SPEC/DECISIONS says "else can be omitted";
/// every one of the 14 `if`s that appear under samples/ has an else without exception. Based
/// on this, else is made mandatory at the parser level (an `if` without else is a syntax
/// error).
pub struct IfExpr {
    pub cond: Box<Expr>,
    pub then_branch: Block,
    pub else_branch: ElseBranch,
    pub span: Span,
}

pub enum ElseBranch {
    Block(Block),
    /// An `if` nested on the line following `else` (D-SYN-03's multi-branch representation).
    ElseIf(Box<IfExpr>),
}

pub struct MatchArm {
    pub pattern: Pattern,
    pub body: MatchArmBody,
    /// For fmt's general-comment preservation only (§5.9). Not a DocComment (not a D-DOC-03
    /// target).
    pub leading_comments: Vec<LeadingComment>,
    pub trailing_comment: Option<String>,
    pub span: Span,
}

pub enum MatchArmBody {
    /// `=> expr`.
    Expr(Expr),
    /// A multi-statement arm: a newline after `=>` followed by an indented block (a target of
    /// D-SYN-11's block-value rule).
    Block(Block),
}

pub enum ParKind {
    /// `par [f(), g()]` -> list[T] (all elements the same type).
    List,
    /// `par (f(), g())` -> tuple[A, B].
    Tuple,
}

#[derive(Clone, Copy)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    LtEq,
    Gt,
    GtEq,
    EqEq,
    NotEq,
    And,
    Or,
}
