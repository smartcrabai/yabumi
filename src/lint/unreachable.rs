//! E4004 unreachable code (D-LINT-04).

use crate::ast::{Block, StmtKind};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode};
use crate::eval::env::Program;

use super::{Visitor, walk_block};

/// v1 detects unreachable code only as "a statement immediately following a `return`
/// statement within the same block". Unreachability via match/if branch exhaustiveness
/// or early exit through `?` is out of scope (zero false positives is prioritized).
pub fn check(program: &Program, diagnostics: &mut DiagnosticBag) {
    let mut visitor = Unreachable { diagnostics };
    for f in program.functions.values() {
        if crate::stdlib::prelude::is_builtin_function(f) {
            continue;
        }
        visitor.visit_block(&f.body);
    }
    for s in program.structs.values() {
        for m in &s.methods {
            visitor.visit_block(&m.body);
        }
    }
}

struct Unreachable<'a> {
    diagnostics: &'a mut DiagnosticBag,
}

impl Visitor for Unreachable<'_> {
    fn visit_block(&mut self, block: &Block) {
        for (i, stmt) in block.stmts.iter().enumerate() {
            if matches!(stmt.kind, StmtKind::Return(_))
                && let Some(next) = block.stmts.get(i + 1)
            {
                self.diagnostics.push(Diagnostic {
                    code: ErrorCode::UnreachableCode,
                    span: next.span,
                    message:
                        "code immediately after a `return` statement is unreachable (D-LINT-04)"
                            .to_owned(),
                });
            }
        }
        walk_block(self, block);
    }
}
