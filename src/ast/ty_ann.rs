//! Syntactic type annotations (ARCHITECTURE.md §3.6). Distinct from the `Ty` of §3.7 (the
//! semantic type the type-checking phase settles on) -- `TypeAnn` is the syntax tree the
//! parser builds directly from source text, used by fmt to reproduce the original annotation
//! as-is. The type-checking phase translates `TypeAnn` into `Ty`.

use crate::diagnostics::Span;
use std::sync::Arc;

#[derive(Debug)]
pub struct TypeAnn {
    pub kind: TypeAnnKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum TypeAnnKind {
    /// Uniformly represented as "name + type args", whether int/str/User/list[int]/
    /// Result[T,E]/Box[int]. list/dict/set/tuple/Result/Option/Value get no special
    /// treatment either (generalizing the D-TYPE-09 principle -- Result/Option have no
    /// special-cased syntax -- to type annotation syntax as well).
    Named {
        name: Arc<str>,
        args: Vec<TypeAnn>,
    },
    /// `tuple[A, B, ...]`.
    Tuple(Vec<TypeAnn>),
    /// `(int) -> str uses {net}`.
    Function {
        params: Vec<TypeAnn>,
        effects: Vec<Arc<str>>,
        ret: Box<TypeAnn>,
    },
    Void,
}
