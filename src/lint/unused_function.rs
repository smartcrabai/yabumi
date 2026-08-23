//! E4002 unused function (D-LINT-02).
//!
//! Reachability determination itself targets the entire flat namespace (the entry file
//! plus sibling modules), tracing whether something is called directly or indirectly from
//! a top-level executable statement or a doctest block (including an indirect reference
//! such as entry's top-level statement -> a module's function -> entry's function).
//! However **the target of the warning is limited to only a `def` declared in the entry
//! file itself**. A `def` declared in a file with a module directive (a module) is
//! excluded from the check, the same as a struct's method (since it may be retained for
//! its API) -- **revision (D-LINT-02, see DECISIONS)**: originally a module's `def` was
//! also subject to the warning, but a configuration such as
//! `samples/ok/15_end_to_end_showcase`, where two sibling entries share one module and
//! only one entry calls a given def (a configuration SPEC §10 actively permits), would
//! fail to pass lint under the old rule, so this was unified to exclude module `def`s.
//!
//! **On top-level executable statements**: `Program` does not retain the entry's top-
//! level executable statements (the `Item::Stmt(_) => {}` in `module_resolve/mod.rs`).
//! When two unrelated top-level functions share the exact same structure of "never
//! called by any other function", the fact that only one of them is called from the top
//! level is structurally indistinguishable from `Program` (function/struct/enum
//! declarations only) -- per the `crate::effects::ENTRY_POINT_NAME` convention (see the
//! comment at the top of `src/effects/mod.rs`; needs adjustment for driver.rs = Unit17),
//! if the synthesized `FunctionDecl` for that name exists in `program.functions`, its
//! body is treated as the "call origin" derived from the top-level executable statements.
//!
//! **Determination method (judgment call made in this file)**: rather than limiting the
//! "called, directly or indirectly" determination to the syntactic position of a call
//! (`Call`), it is relaxed all the way to "does that function name appear anywhere in the
//! body as an identifier" -- even syntax that passes a function name as-is to a stdlib
//! higher-order method (such as `xs.sort_by(compare)`) is a reference as a value rather
//! than a call, so a strict "call graph" alone cannot capture it; this rule therefore also
//! applies the same "prioritize zero false positives" policy as D-LINT-04 (DECISIONS
//! D-LINT-04), using the looser "is it referenced" as the reachability criterion. A
//! doctest block (an untagged fence) is also lexed/parsed under the same criterion to
//! collect referenced names -- a fence that fails to parse is ignored (diagnostics for
//! the doctest phase itself are Unit15's responsibility). Since this referenced-name
//! collection is itself shared with `unused_variable.rs`, it was factored out into
//! `doc_fence.rs` (this file merely calls `doc_fence`'s public functions).
//!
//! **Fix for a known bug (judgment call made in this file)**: a struct's/enum's/struct
//! method's own `##` doc comment was unconditionally added as a reachability root
//! (regardless of whether it is called from elsewhere), but a `def`'s own doc comment was
//! only ever fed into `call_graph` (an outgoing edge only traced once that `def` has
//! already become "reached"), so when that `def` itself was never called from anywhere
//! else, a self-reference from its own doctest (such as `assert(square(3) == 100)`) would
//! not end up in `used`, causing a false-positive E4002. Per D-LINT-02's explicit
//! criterion, "never called, directly or indirectly, from a top-level executable statement
//! **or a doctest block**", this was fixed (in the `check` function) so a `def`'s own doc
//! comment's referenced names are also unconditionally added to roots, the same as
//! struct/enum. Likewise, a module-level const's (a top-level assignment directly under
//! the entry file, held under the `ENTRY_POINT_NAME` convention as a `Stmt` inside
//! entry's synthesized `FunctionDecl.body`) own doc comment is also added to roots the
//! same way (`doc_fence::collect_doc_fence_names_from_top_level_consts`).

use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, FileId, Span};
use crate::eval::env::Program;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::doc_fence::{
    collect_doc_fence_names, collect_doc_fence_names_from_top_level_consts,
    collect_referenced_names,
};

/// Collects, among all the `FileId`s `program.functions` references, those originating
/// from a file whose effective first token is a `module` directive (D-LEX-08/09). The
/// determination itself is delegated to `lexer::text_starts_with_module_keyword`; the
/// candidate files are deduplicated first so the same file is not re-lexed repeatedly.
fn compute_module_file_ids(program: &Program) -> HashSet<FileId> {
    program
        .functions
        .values()
        .map(|f| f.span.file)
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|&file| {
            crate::lexer::text_starts_with_module_keyword(program.sources.file(file).text())
        })
        .collect()
}

/// Determines reachability (whether something is called directly or indirectly from a
/// top-level executable statement or a doctest block) targeting the entire flat
/// namespace, but limits the target of the warning to **only a `def` declared in the
/// entry file itself** (the revised D-LINT-02). A `def` declared in a file with a module
/// directive, and a struct's methods, are excluded from the check.
pub fn check(program: &Program, diagnostics: &mut DiagnosticBag) {
    let mut call_graph: HashMap<Arc<str>, HashSet<Arc<str>>> = HashMap::new();
    for (name, f) in &program.functions {
        if name.as_ref() == crate::effects::ENTRY_POINT_NAME
            || crate::stdlib::prelude::is_builtin_function(f)
        {
            continue;
        }
        let mut referenced = HashSet::new();
        collect_referenced_names(&f.body, &mut referenced);
        call_graph.insert(Arc::clone(name), referenced);
    }

    let mut roots: HashSet<Arc<str>> = HashSet::new();
    if let Some(entry) = program.functions.get(crate::effects::ENTRY_POINT_NAME) {
        collect_referenced_names(&entry.body, &mut roots);
        // References from a module-level const's (a top-level assignment directly under
        // the entry) own doc comment are also unconditionally added to roots, just like a
        // def (see "fix for a known bug" at the top of this file).
        collect_doc_fence_names_from_top_level_consts(&entry.body, &mut roots);
    }
    // References from a `def`'s own doc comment are unconditionally added to roots
    // regardless of whether that `def` is called from elsewhere -- aligning with how
    // struct/enum/struct methods are handled (the loop right after this) satisfies
    // "called, directly or indirectly, from a doctest block" (D-LINT-02) (see "fix for a
    // known bug" at the top of this file).
    for f in program.functions.values() {
        if crate::stdlib::prelude::is_builtin_function(f) {
            continue;
        }
        if let Some(doc) = &f.doc_comment {
            collect_doc_fence_names(doc, &mut roots);
        }
    }
    for s in program.structs.values() {
        for m in &s.methods {
            collect_referenced_names(&m.body, &mut roots);
            if let Some(doc) = &m.doc_comment {
                collect_doc_fence_names(doc, &mut roots);
            }
        }
        if let Some(doc) = &s.doc_comment {
            collect_doc_fence_names(doc, &mut roots);
        }
    }
    for e in program.enums.values() {
        if let Some(doc) = &e.doc_comment {
            collect_doc_fence_names(doc, &mut roots);
        }
    }

    let mut used: HashSet<Arc<str>> = roots
        .into_iter()
        .filter(|n| program.functions.contains_key(n))
        .collect();
    let mut frontier: Vec<Arc<str>> = used.iter().cloned().collect();
    while let Some(name) = frontier.pop() {
        let Some(callees) = call_graph.get(&name) else {
            continue;
        };
        for callee in callees {
            if program.functions.contains_key(callee) && used.insert(Arc::clone(callee)) {
                frontier.push(Arc::clone(callee));
            }
        }
    }

    let module_files = compute_module_file_ids(program);

    let mut unused: Vec<(Arc<str>, Span)> = program
        .functions
        .iter()
        .filter(|(name, f)| {
            name.as_ref() != crate::effects::ENTRY_POINT_NAME
                && !crate::stdlib::prelude::is_builtin_function(f)
                && !used.contains(*name)
                && !module_files.contains(&f.span.file)
        })
        .map(|(name, f)| (Arc::clone(name), f.span))
        .collect();
    unused.sort_by_key(|(_, span)| (span.start.line, span.start.col));
    for (name, span) in unused {
        diagnostics.push(Diagnostic {
            code: ErrorCode::UnusedFunction,
            span,
            message: format!("function '{name}' is never called from anywhere (D-LINT-02)"),
        });
    }
}
