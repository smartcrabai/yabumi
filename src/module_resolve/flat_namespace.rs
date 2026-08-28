//! Registers every declaration into a single flat namespace, and detects E1001 (D-TYPE-07, ARCHITECTURE.md §2.1).

use super::module_grammar::module_level_const;
use crate::ast::{Decl, Expr, ExprKind, Item, Module};
use crate::diagnostics::{Diagnostic, DiagnosticBag, ErrorCode, Span};
use crate::eval::env::Program;
use crate::eval::value::{MapKey, Value};
use indexmap::{IndexMap, IndexSet};
use std::collections::HashMap;
use std::sync::Arc;

/// Registers the declarations from every file in the same directory (struct names, enum
/// names, enum variant names, top-level function names, top-level constant names) into
/// `program` as a single flat namespace. A duplicate is reported as E1001 (D-MOD-05's
/// diagnostic format: the second definition's location is the primary location, and both
/// locations' information is embedded in the message).
///
/// **Note on division of responsibility (a decision made in this file)**: because
/// `build_program_skeleton` (`mod.rs`) is given ownership of `modules: Vec<Module>` (passed
/// by value), the logic that actually **moves** `Item::Decl` (`FunctionDecl`/`StructDecl`/
/// `EnumDecl`) into `program.functions`/`structs`/`enums` belongs on that side (since the
/// `ast`-side nodes have no `Clone` implementation, this function -- which only borrows
/// `&[Module]` -- cannot take ownership out of them). This function's responsibility is
/// limited to two things: (a) detecting E1001 across struct/enum/enum-variant/top-level
/// function/module-level constant names, and (b) actually evaluating D-MOD-02's literal-only
/// module-level constants into `Value` and registering them into `program.consts` (since
/// `program.consts: HashMap<Arc<str>, Value>` holds only `Value`s that require no ownership,
/// it can be built even from a borrow -- see the field definition of `eval::env::Program`).
pub fn register_flat_namespace(
    modules: &[Module],
    program: &mut Program,
    diagnostics: &mut DiagnosticBag,
) {
    let mut declared_at: HashMap<Arc<str>, Span> = HashMap::new();
    for (name, declaration) in &program.functions {
        declared_at.insert(Arc::clone(name), declaration.span);
    }
    for (name, declaration) in &program.structs {
        declared_at.insert(Arc::clone(name), declaration.span);
    }
    for (name, declaration) in &program.enums {
        declared_at.insert(Arc::clone(name), declaration.span);
        for variant in &declaration.variants {
            declared_at.insert(Arc::clone(&variant.name), variant.span);
        }
    }

    let mut const_candidates: Vec<(Arc<str>, &Expr)> = Vec::new();

    for module in modules {
        for item in &module.items {
            match item {
                Item::Decl(Decl::Function(f)) => {
                    try_declare(&mut declared_at, diagnostics, program, &f.name, f.span);
                }
                Item::Decl(Decl::Struct(s)) => {
                    try_declare(&mut declared_at, diagnostics, program, &s.name, s.span);
                }
                Item::Decl(Decl::Enum(e)) => {
                    try_declare(&mut declared_at, diagnostics, program, &e.name, e.span);
                    for variant in &e.variants {
                        try_declare(
                            &mut declared_at,
                            diagnostics,
                            program,
                            &variant.name,
                            variant.span,
                        );
                    }
                }
                Item::Stmt(stmt) if module.is_module_directive => {
                    if let Some((name, value)) = module_level_const(stmt) {
                        let is_new =
                            try_declare(&mut declared_at, diagnostics, program, name, stmt.span);
                        if is_new {
                            program.const_spans.insert(name.clone(), stmt.span);
                            const_candidates.push((name.clone(), value));
                        }
                    }
                    // A statement that violates the grammar (an E5002 target) is ignored here
                    // -- `check_module_toplevel_grammar` reports it separately.
                }
                Item::Stmt(_) => {
                    // An ordinary executable statement in the entry file. Not a target of the
                    // flat namespace (D-TYPE-07: local variable names live in a separate
                    // namespace scoped to the function/top level).
                }
            }
        }
    }

    validate_module_const_types(&const_candidates, diagnostics);
    resolve_module_const_values(const_candidates, &mut program.consts);
}

/// If `name` is unused, records it into `declared_at` and returns true. If it is already
/// used, reports E1001 in D-MOD-05's diagnostic format
/// (`duplicate definition of 'name' (also defined at other_file:line:col)`) and returns
/// false.
fn try_declare(
    declared_at: &mut HashMap<Arc<str>, Span>,
    diagnostics: &mut DiagnosticBag,
    program: &Program,
    name: &Arc<str>,
    span: Span,
) -> bool {
    if let Some(&first_span) = declared_at.get(name) {
        let other_file = program.sources.path(first_span.file).display();
        diagnostics.push(Diagnostic {
            code: ErrorCode::DuplicateName,
            span,
            message: format!(
                "duplicate definition of '{name}' (also defined at {other_file}:{}:{})",
                first_span.start.line, first_span.start.col,
            ),
        });
        false
    } else {
        declared_at.insert(name.clone(), span);
        true
    }
}

/// Evaluates D-MOD-02's literal-only module-level constants into a `Value`. `known` is the
/// set of constants resolved so far, used to resolve references (identifiers) to other
/// constants. When an unresolved identifier is encountered (a forward reference, a circular
/// reference, or a nonexistent identifier), returns `None` and leaves it to a later pass of
/// the caller's fixpoint iteration.
pub(crate) fn eval_const_expr(expr: &Expr, known: &HashMap<Arc<str>, Value>) -> Option<Value> {
    match &expr.kind {
        ExprKind::IntLit(v) => Some(Value::Int(*v)),
        ExprKind::FloatLit(v) => Some(Value::Float(*v)),
        ExprKind::BoolLit(v) => Some(Value::Bool(*v)),
        ExprKind::StringLit(s) => Some(Value::Str(Arc::from(s.as_str()))),
        ExprKind::Ident(name) => known.get(name.as_ref()).cloned(),
        ExprKind::Grouping(inner) => eval_const_expr(inner, known),
        ExprKind::ListLit { elements, .. } => {
            let mut values = Vec::with_capacity(elements.len());
            for element in elements {
                values.push(eval_const_expr(element, known)?);
            }
            Some(Value::List(Arc::new(values)))
        }
        ExprKind::TupleLit { elements, .. } => {
            let mut values = Vec::with_capacity(elements.len());
            for element in elements {
                values.push(eval_const_expr(element, known)?);
            }
            Some(Value::Tuple(Arc::from(values)))
        }
        ExprKind::SetLit { elements, .. } => {
            let mut set = IndexSet::with_capacity(elements.len());
            for element in elements {
                let value = eval_const_expr(element, known)?;
                set.insert(MapKey::try_from_value(&value)?);
            }
            Some(Value::Set(Arc::new(set)))
        }
        ExprKind::DictLit { entries, .. } => {
            let mut map = IndexMap::with_capacity(entries.len());
            for (k, v) in entries {
                let key = eval_const_expr(k, known)?;
                let value = eval_const_expr(v, known)?;
                map.insert(MapKey::try_from_value(&key)?, value);
            }
            Some(Value::Dict(Arc::new(map)))
        }
        // module_level_const/is_module_const_value_expr already rejects anything other than
        // the above, so this should be unreachable, but conservatively returns None for
        // exhaustiveness.
        _ => None,
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ConstType {
    Int,
    Float,
    Bool,
    Str,
    List(Box<Self>),
    Dict(Box<Self>, Box<Self>),
    Set(Box<Self>),
    Tuple(Vec<Self>),
}

impl ConstType {
    fn is_key(&self) -> bool {
        matches!(self, Self::Int | Self::Bool | Self::Str)
            || matches!(self, Self::Tuple(items) if items.iter().all(Self::is_key))
    }
}

fn validate_module_const_types(candidates: &[(Arc<str>, &Expr)], diagnostics: &mut DiagnosticBag) {
    let mut pending = candidates.to_vec();
    let mut known = HashMap::new();
    while !pending.is_empty() {
        let mut progressed = false;
        let mut remaining = Vec::new();
        for (name, expression) in pending {
            match infer_const_type(expression, &known) {
                Ok(const_type) => {
                    known.insert(name, const_type);
                    progressed = true;
                }
                Err(Some(diagnostic)) => {
                    diagnostics.push(diagnostic);
                    progressed = true;
                }
                Err(None) => remaining.push((name, expression)),
            }
        }
        if !progressed {
            for (name, expression) in remaining {
                diagnostics.push(Diagnostic {
                    code: ErrorCode::UninferableType,
                    span: expression.span,
                    message: format!(
                        "module constant '{name}' has an undefined or circular reference"
                    ),
                });
            }
            return;
        }
        pending = remaining;
    }
}

fn infer_const_type(
    expression: &Expr,
    known: &HashMap<Arc<str>, ConstType>,
) -> Result<ConstType, Option<Diagnostic>> {
    match &expression.kind {
        ExprKind::IntLit(_) => Ok(ConstType::Int),
        ExprKind::FloatLit(_) => Ok(ConstType::Float),
        ExprKind::BoolLit(_) => Ok(ConstType::Bool),
        ExprKind::StringLit(_) => Ok(ConstType::Str),
        ExprKind::Ident(name) => known.get(name).cloned().ok_or(None),
        ExprKind::Grouping(inner) => infer_const_type(inner, known),
        ExprKind::ListLit { elements, .. } => {
            infer_homogeneous(elements.iter(), known).map(|item| ConstType::List(Box::new(item)))
        }
        ExprKind::SetLit { elements, .. } => {
            let item = infer_homogeneous(elements.iter(), known)?;
            if !item.is_key() {
                return Err(Some(Diagnostic {
                    code: ErrorCode::SetElementTypeNotAllowed,
                    span: expression.span,
                    message: "module constant set element type is not allowed".to_owned(),
                }));
            }
            Ok(ConstType::Set(Box::new(item)))
        }
        ExprKind::TupleLit { elements, .. } => elements
            .iter()
            .map(|element| infer_const_type(element, known))
            .collect::<Result<Vec<_>, _>>()
            .map(ConstType::Tuple),
        ExprKind::DictLit { entries, .. } => {
            let key = infer_homogeneous(entries.iter().map(|(key, _)| key), known)?;
            if !key.is_key() {
                return Err(Some(Diagnostic {
                    code: ErrorCode::DictKeyTypeNotAllowed,
                    span: expression.span,
                    message: "module constant dictionary key type is not allowed".to_owned(),
                }));
            }
            let value = infer_homogeneous(entries.iter().map(|(_, value)| value), known)?;
            Ok(ConstType::Dict(Box::new(key), Box::new(value)))
        }
        _ => Err(None),
    }
}

fn infer_homogeneous<'expr>(
    mut elements: impl Iterator<Item = &'expr Expr>,
    known: &HashMap<Arc<str>, ConstType>,
) -> Result<ConstType, Option<Diagnostic>> {
    let Some(first) = elements.next() else {
        return Err(None);
    };
    let expected = infer_const_type(first, known)?;
    for element in elements {
        let actual = infer_const_type(element, known)?;
        if actual != expected {
            return Err(Some(Diagnostic {
                code: ErrorCode::CollectionElementTypeMismatch,
                span: element.span,
                message: "module constant collection elements have different types".to_owned(),
            }));
        }
    }
    Ok(expected)
}

/// Resolves constant candidates via fixpoint iteration. Under D-MOD-03 the very notion of a
/// circular reference does not exist (every declaration is registered all at once at load
/// time), but because a constant's value itself "may reference other constants" (D-MOD-02),
/// this iterates until resolved, regardless of declaration order or file order (a decision
/// made in this file: a genuine circular reference or a reference to an undefined identifier
/// can never settle a `Value`, so in that case insertion into `known` is given up without
/// emitting a diagnostic -- detecting a nonexistent identifier is the
/// responsibility of the TypeCheck phase, and module_resolve is responsible only for
/// D-MOD-02's literal evaluation).
pub(crate) fn resolve_module_const_values(
    mut pending: Vec<(Arc<str>, &Expr)>,
    known: &mut HashMap<Arc<str>, Value>,
) {
    loop {
        let mut progressed = false;
        let mut still_pending = Vec::new();
        for (name, expr) in pending {
            match eval_const_expr(expr, known) {
                Some(value) => {
                    known.insert(name, value);
                    progressed = true;
                }
                None => still_pending.push((name, expr)),
            }
        }
        pending = still_pending;
        if pending.is_empty() || !progressed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::SourceMap;
    use crate::lexer::Lexer;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc as StdArc;

    fn sample_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    fn read_sample(rel: &str) -> String {
        let path = sample_path(rel);
        match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => panic!("failed to read sample file {}: {e}", path.display()),
        }
    }

    fn lex_and_parse(sources: &mut SourceMap, path: PathBuf, src: String) -> Module {
        let file = sources.add(path, src.clone());
        let (tokens, _comments, lex_diag) = Lexer::new(&src, file).tokenize();
        assert!(lex_diag.is_empty(), "lex diagnostics: {lex_diag:?}");
        let (module, parse_diag) = crate::parser::parse_module(&tokens, file);
        assert!(
            parse_diag.is_empty(),
            "parse diagnostics: {:?}",
            parse_diag.into_vec()
        );
        module
    }

    /// samples/err/static/10a_module_name_collision: entry_main.ybm and mod_util.ybm both
    /// declare a top-level function named `format_greeting`; this should become E1001
    /// (D-MOD-05).
    #[test]
    fn register_flat_namespace_detects_duplicate_function_across_files() {
        let dir = "samples/err/static/10a_module_name_collision";
        let mut sources = SourceMap::new();
        let entry_src = read_sample(&format!("{dir}/entry_main.ybm"));
        let mod_src = read_sample(&format!("{dir}/mod_util.ybm"));
        let entry = lex_and_parse(&mut sources, PathBuf::from("entry_main.ybm"), entry_src);
        let module = lex_and_parse(&mut sources, PathBuf::from("mod_util.ybm"), mod_src);

        let mut program = Program::new(StdArc::new(sources));
        let mut diagnostics = DiagnosticBag::new();
        register_flat_namespace(&[entry, module], &mut program, &mut diagnostics);

        let diags = diagnostics.into_vec();
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].code, ErrorCode::DuplicateName);
        assert!(
            diags[0].message.contains("format_greeting"),
            "{}",
            diags[0].message
        );
        assert!(
            diags[0].message.contains("also defined at"),
            "should contain D-MOD-05's diagnostic format (also defined at...): {}",
            diags[0].message
        );
    }

    /// samples/ok/10c_module_constants_and_cross_reference/mod_constants.ybm:
    /// literal-only module-level constants actually get evaluated and registered into
    /// Program.consts.
    #[test]
    fn register_flat_namespace_evaluates_literal_module_consts() {
        let path = "samples/ok/10c_module_constants_and_cross_reference/mod_constants.ybm";
        let src = read_sample(path);
        let mut sources = SourceMap::new();
        let module = lex_and_parse(&mut sources, PathBuf::from(path), src);
        assert!(module.is_module_directive);

        let mut program = Program::new(StdArc::new(sources));
        let mut diagnostics = DiagnosticBag::new();
        register_flat_namespace(&[module], &mut program, &mut diagnostics);

        assert!(diagnostics.is_empty(), "{:?}", diagnostics.into_vec());
        assert_eq!(program.consts.get("max_retries"), Some(&Value::Int(3)));
        assert_eq!(
            program.consts.get("default_timeout_ms"),
            Some(&Value::Int(5000))
        );
        assert_eq!(
            program.consts.get("app_name"),
            Some(&Value::Str(Arc::from("yabumi-sample")))
        );
        assert_eq!(
            program.consts.get("retry_delays_ms"),
            Some(&Value::List(Arc::new(vec![
                Value::Int(100),
                Value::Int(200),
                Value::Int(400),
            ])))
        );
    }

    /// D-MOD-02 "involving a reference to another constant": verifies via fixpoint
    /// iteration that evaluation succeeds even when a constant references another constant
    /// (regardless of declaration order) -- since samples/ has no such combination, this is
    /// verified with a hand-written minimal source.
    #[test]
    fn register_flat_namespace_resolves_const_to_const_reference_regardless_of_order() {
        let mut sources = SourceMap::new();
        // Confirms this can be resolved even when `derived` is written before `base`
        // (verifying that fixpoint iteration does not depend on declaration order).
        let module = lex_and_parse(
            &mut sources,
            PathBuf::from("mod_ref.ybm"),
            "module\n\nderived = base\nbase = 7\n".to_owned(),
        );

        let mut program = Program::new(StdArc::new(sources));
        let mut diagnostics = DiagnosticBag::new();
        register_flat_namespace(&[module], &mut program, &mut diagnostics);

        assert!(diagnostics.is_empty(), "{:?}", diagnostics.into_vec());
        assert_eq!(program.consts.get("base"), Some(&Value::Int(7)));
        assert_eq!(program.consts.get("derived"), Some(&Value::Int(7)));
    }
}
