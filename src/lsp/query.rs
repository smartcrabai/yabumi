use crate::ast::{Expr, ExprKind};
use crate::diagnostics::{FileId, Position, Span};
use crate::eval::env::Program;
use crate::lint::{self, Visitor};
use crate::types::Ty;
use std::sync::Arc;

pub(crate) struct ExprAt {
    pub(crate) id: crate::ast::NodeId,
    pub(crate) span: Span,
    kind: QueryKind,
}

enum QueryKind {
    Ident,
    Method {
        receiver: crate::ast::NodeId,
        name: Arc<str>,
    },
    Field {
        receiver: crate::ast::NodeId,
        name: Arc<str>,
    },
    Other,
}

pub(crate) fn expr_at(program: &Program, file: FileId, position: Position) -> Option<ExprAt> {
    let mut finder = ExprFinder {
        file,
        position,
        best: None,
    };
    for function in program.functions.values().filter(|f| f.span.file == file) {
        finder.visit_block(&function.body);
    }
    for structure in program.structs.values().filter(|s| s.span.file == file) {
        for method in &structure.methods {
            finder.visit_block(&method.body);
        }
    }
    finder.best
}

pub(crate) fn definition_span(program: &Program, expr: &ExprAt) -> Option<Span> {
    match &expr.kind {
        QueryKind::Ident => program.resolutions.ident_def.get(&expr.id).copied(),
        QueryKind::Method { receiver, name } => {
            let Ty::Named {
                name: type_name, ..
            } = program.resolutions.expr_ty.get(receiver)?
            else {
                return None;
            };
            program
                .structs
                .get(type_name.as_ref())?
                .methods
                .iter()
                .find(|candidate| candidate.name == *name)
                .map(|method| method.span)
        }
        QueryKind::Field { receiver, name } => {
            let Ty::Named {
                name: type_name, ..
            } = program.resolutions.expr_ty.get(receiver)?
            else {
                return None;
            };
            program
                .structs
                .get(type_name.as_ref())?
                .fields
                .iter()
                .find(|candidate| candidate.name == *name)
                .map(|field| field.span)
        }
        QueryKind::Other => None,
    }
}

struct ExprFinder {
    file: FileId,
    position: Position,
    best: Option<ExprAt>,
}

impl Visitor for ExprFinder {
    fn visit_expr(&mut self, expr: &Expr) {
        if expr.span.file == self.file
            && expr.span.start <= self.position
            && self.position < expr.span.end
            && self.best.as_ref().is_none_or(|best| {
                expr.span.start >= best.span.start && expr.span.end <= best.span.end
            })
        {
            let kind = match &expr.kind {
                ExprKind::Ident(_) => QueryKind::Ident,
                ExprKind::MethodCall {
                    receiver, method, ..
                } => QueryKind::Method {
                    receiver: receiver.id,
                    name: Arc::clone(method),
                },
                ExprKind::FieldAccess { target, field } => QueryKind::Field {
                    receiver: target.id,
                    name: Arc::clone(field),
                },
                _ => QueryKind::Other,
            };
            self.best = Some(ExprAt {
                id: expr.id,
                span: expr.span,
                kind,
            });
        }
        lint::walk_expr(self, expr);
    }
}
