//! Pre-registration of the Result/Option/Error/Value type definitions, and the int/float/str
//! conversion functions (ARCHITECTURE.md §2.1).
//!
//! Per D-TYPE-09, `Result`/`Option` are defined as ordinary builtin enums with no special-cased
//! syntax. `Error` (STDLIB.md §3.3), and the `Response`/`HttpOptions`/`ProcOutput` structs used
//! by `http`/`proc` (STDLIB.md §6/§8), are likewise pre-registered through the same mechanism as
//! ordinary structs with no dedicated Rust type. `int`/`float`/`str` are not reserved words; they
//! are treated as "names that are both types and callable conversions" pre-registered into the
//! flat namespace (D-TYPE-14).
//!
//! **Known limitation**: `driver.rs::run_pipeline` calls this function after running
//! `build_program_skeleton`, but the comments in `ty_from_ann` in `types/generics.rs`, and in
//! `builtin_struct_field_names` in `eval/call.rs` all still handle Result/Option/Value/Error
//! directly and individually, each under the assumption that "`program.{enums,structs}` has no
//! entity originating from `prelude::install`" -- whether this is consistent with what this
//! function actually registers (i.e. whether it amounts to dual bookkeeping) remains
//! unconfirmed. This function itself exists to provide the "a foundation on which a user
//! definition sharing one of these names is detected as E1001" required by the completion
//! criteria of ARCHITECTURE.md §7.2, registering into `program.enums`/`structs`/`functions`
//! exactly as declared by STDLIB.md/DECISIONS.md.

use crate::ast::{
    Block, EnumDecl, EnumVariant, FunctionDecl, NodeId, Param, StructDecl, TypeAnn, TypeAnnKind,
};
use crate::diagnostics::{FileId, Position, Span};
use crate::eval::env::Program;
use std::sync::Arc;

/// A synthetic `Span` for builtin declarations. Doesn't point at a real file -- since
/// `register_flat_namespace` (module_resolve) calls `program.sources.path(span.file)` when
/// reporting a duplicate-definition diagnostic (E1001) (`SourceMap::file` indexes `FileId`
/// directly, see `src/diagnostics/source_map.rs`), pointing at a nonexistent `FileId` would
/// panic with an out-of-bounds access. `FileId(0)` is "the entry file the CLI loads", and
/// D-CLI-04 (a nonexistent file is rejected upfront) guarantees that at least one file is always
/// already registered in `SourceMap` by the time `install` is called (before module_resolve
/// begins, after the Lex phase completes) (a decision made in this file).
fn builtin_span() -> Span {
    Span {
        file: FileId(0),
        start: Position { line: 1, col: 1 },
        end: Position { line: 1, col: 1 },
    }
}

/// `NodeId` is normally issued by the parser in monotonically increasing order (see
/// `ast/mod.rs`), but builtin declarations don't go through the parser. Since the parser counts
/// up from 0, subtracting an offset from `u32::MAX` avoids collisions for any realistic file
/// size (a decision made in this file).
fn nid(offset: u32) -> NodeId {
    NodeId(u32::MAX - offset)
}

fn generic_ty(name: &str, args: Vec<TypeAnn>) -> TypeAnn {
    TypeAnn {
        kind: TypeAnnKind::Named {
            name: Arc::from(name),
            args,
        },
        span: builtin_span(),
    }
}

fn named_ty(name: &str) -> TypeAnn {
    generic_ty(name, Vec::new())
}

fn void_ty() -> TypeAnn {
    TypeAnn {
        kind: TypeAnnKind::Void,
        span: builtin_span(),
    }
}

fn enum_variant(name: &str, fields: Vec<TypeAnn>) -> EnumVariant {
    let n = fields.len();
    EnumVariant {
        name: Arc::from(name),
        fields,
        field_names: vec![None; n],
        leading_comments: Vec::new(),
        trailing_comment: None,
        span: builtin_span(),
    }
}

fn param(name: &str, ty: TypeAnn) -> Param {
    Param {
        name: Arc::from(name),
        ty,
        span: builtin_span(),
    }
}

fn builtin_struct(name: &str, id: NodeId, fields: Vec<(&str, TypeAnn)>) -> StructDecl {
    StructDecl {
        id,
        name: Arc::from(name),
        generics: Vec::new(),
        fields: fields.into_iter().map(|(n, ty)| param(n, ty)).collect(),
        field_leading_comments: Vec::new(),
        field_trailing_comments: Vec::new(),
        methods: Vec::new(),
        leading_comments: Vec::new(),
        doc_comment: None,
        span: builtin_span(),
    }
}

/// A body with no contents that is never executed (a placeholder solely for name-collision
/// detection, see the module doc comment above).
fn builtin_function(name: &str, id: NodeId, params: Vec<Param>, ret: TypeAnn) -> FunctionDecl {
    FunctionDecl {
        id,
        name: Arc::from(name),
        generics: Vec::new(),
        self_param: None,
        params,
        ret,
        effects: Vec::new(),
        body: Block {
            stmts: Vec::new(),
            span: builtin_span(),
        },
        leading_comments: Vec::new(),
        doc_comment: None,
        span: builtin_span(),
    }
}

/// D-TYPE-09: `Result[T, E]` { Ok(T), Err(E) }.
fn result_enum() -> EnumDecl {
    EnumDecl {
        id: nid(0),
        name: Arc::from("Result"),
        generics: vec![Arc::from("T"), Arc::from("E")],
        variants: vec![
            enum_variant("Ok", vec![named_ty("T")]),
            enum_variant("Err", vec![named_ty("E")]),
        ],
        leading_comments: Vec::new(),
        doc_comment: None,
        span: builtin_span(),
    }
}

/// D-TYPE-09: `Option[T]` { Some(T), None }.
fn option_enum() -> EnumDecl {
    EnumDecl {
        id: nid(1),
        name: Arc::from("Option"),
        generics: vec![Arc::from("T")],
        variants: vec![
            enum_variant("Some", vec![named_ty("T")]),
            enum_variant("None", vec![]),
        ],
        leading_comments: Vec::new(),
        doc_comment: None,
        span: builtin_span(),
    }
}

/// D-TYPE-10: `Value` { Null, Bool(bool), Int(int), Float(float), Str(str),
/// List(list[Value]), Dict(dict[str, Value]) }. The declaration order matches
/// `builtin_variant_info` (variant_index) in `eval/call.rs`.
fn value_enum() -> EnumDecl {
    EnumDecl {
        id: nid(2),
        name: Arc::from("Value"),
        generics: Vec::new(),
        variants: vec![
            enum_variant("Null", vec![]),
            enum_variant("Bool", vec![named_ty("bool")]),
            enum_variant("Int", vec![named_ty("int")]),
            enum_variant("Float", vec![named_ty("float")]),
            enum_variant("Str", vec![named_ty("str")]),
            enum_variant("List", vec![generic_ty("list", vec![named_ty("Value")])]),
            enum_variant(
                "Dict",
                vec![generic_ty("dict", vec![named_ty("str"), named_ty("Value")])],
            ),
        ],
        leading_comments: Vec::new(),
        doc_comment: None,
        span: builtin_span(),
    }
}

/// STDLIB.md §3.3.
fn error_struct() -> StructDecl {
    builtin_struct(
        "Error",
        nid(10),
        vec![
            ("kind", named_ty("str")),
            ("message", named_ty("str")),
            ("cause", generic_ty("Option", vec![named_ty("Error")])),
        ],
    )
}

/// STDLIB.md §6.
fn response_struct() -> StructDecl {
    builtin_struct(
        "Response",
        nid(11),
        vec![
            ("status", named_ty("int")),
            (
                "headers",
                generic_ty("dict", vec![named_ty("str"), named_ty("str")]),
            ),
            ("body", named_ty("str")),
        ],
    )
}

/// STDLIB.md §6.
fn http_options_struct() -> StructDecl {
    builtin_struct(
        "HttpOptions",
        nid(12),
        vec![
            (
                "headers",
                generic_ty("dict", vec![named_ty("str"), named_ty("str")]),
            ),
            ("timeout_ms", named_ty("int")),
        ],
    )
}

/// STDLIB.md §8.
fn proc_output_struct() -> StructDecl {
    builtin_struct(
        "ProcOutput",
        nid(13),
        vec![
            ("stdout", named_ty("str")),
            ("stderr", named_ty("str")),
            ("exit_code", named_ty("int")),
        ],
    )
}

/// D-TYPE-14/D-STDPOL-01: `int`/`float`/`str`/`print`/`eprint`/`assert`/`set` are names
/// pre-registered into the flat namespace, so a user defining a function or variable with the
/// same name gets E1001 (the actual signature checking/dispatch is performed through a separate
/// path by `check_call_named` in `types/check_expr.rs`, the D-STDPOL-01 overload special case;
/// the sole purpose of the `FunctionDecl` registered here is name-collision detection, and the
/// contents of `params`/`ret`/`body` are never actually checked or executed).
///
/// **Why every return type is `void`**: originally `int`/`float`/`str`/`set` were each given
/// their "proper" return type (`int`/`float`/`str`/`set[T]`), but `check_all_decls` in
/// `types/check_decl.rs` unconditionally checks every declaration in `program.functions` via
/// `check_function_decl` (bypassing `check_call_named`'s special-casing), so these placeholders
/// -- registered with an empty body -- ran afoul of the general rule that "a function whose
/// return type isn't void cannot have an empty body" (§5.6), causing TypeCheck to fail with a
/// false-positive BranchTypeMismatch for **every** user program, across every path that calls
/// `prelude::install` followed by `check_program` (which includes `check_all_decls`)
/// (`stdlib::builtins::tests` and `stdlib::time::tests` had `#[ignore]`d their corresponding
/// integration tests for this same reason -- the fix in this file removed the need for that
/// `#[ignore]`, and it has already been lifted). Giving each placeholder a body consistent with
/// its return type (e.g. `return 0` for `int`) was also considered, but `set`'s return type is
/// the generic type `set[T]`, and providing a consistent body for it would also require adding a
/// `generics` field, among other complications; so instead the policy fully commits to treating
/// these as "representative signatures that are never executed" and unifies them all to **void
/// return type and an empty body**, sidestepping the conflict with the general rule entirely (a
/// decision made in this file).
fn builtin_functions() -> Vec<FunctionDecl> {
    vec![
        builtin_function(
            "int",
            nid(20),
            vec![param("x", named_ty("float"))],
            void_ty(),
        ),
        builtin_function(
            "float",
            nid(21),
            vec![param("x", named_ty("int"))],
            void_ty(),
        ),
        builtin_function("str", nid(22), vec![param("x", named_ty("int"))], void_ty()),
        builtin_function(
            "print",
            nid(23),
            vec![param("value", named_ty("str"))],
            void_ty(),
        ),
        builtin_function(
            "eprint",
            nid(24),
            vec![param("value", named_ty("str"))],
            void_ty(),
        ),
        builtin_function(
            "assert",
            nid(25),
            vec![param("cond", named_ty("bool"))],
            void_ty(),
        ),
        builtin_function("set", nid(26), Vec::new(), void_ty()),
    ]
}

pub(crate) fn is_builtin_function(declaration: &FunctionDecl) -> bool {
    declaration.id.0 >= u32::MAX - 26 && BUILTIN_FUNCTION_NAMES.contains(&declaration.name.as_ref())
}
pub(crate) const BUILTIN_FUNCTION_NAMES: &[&str] =
    &["int", "float", "str", "print", "eprint", "assert", "set"];

/// Registers the builtin declarations (`Result[T,E]`/`Option[T]`/`Error`/`Value`/`Response`/
/// `HttpOptions`/`ProcOutput`, plus conversion/builtin functions such as
/// `int`/`float`/`str`/`print`/`eprint`/`assert`/`set`) into `program`'s `enums`/`structs`/
/// `functions` tables before user code is checked. Called before module_resolve's flat-namespace
/// registration (D-TYPE-07), this forms the foundation on which a user definition sharing one of
/// these names is detected as E1001.
pub fn install(program: &mut Program) {
    for decl in [result_enum(), option_enum(), value_enum()] {
        program
            .enums
            .entry(Arc::clone(&decl.name))
            .or_insert_with(|| Arc::new(decl));
    }
    for decl in [
        error_struct(),
        response_struct(),
        http_options_struct(),
        proc_output_struct(),
    ] {
        program
            .structs
            .entry(Arc::clone(&decl.name))
            .or_insert_with(|| Arc::new(decl));
    }
    for decl in builtin_functions() {
        program
            .functions
            .entry(Arc::clone(&decl.name))
            .or_insert_with(|| Arc::new(decl));
    }
}

#[cfg(test)]
mod tests {
    use super::install;
    use crate::diagnostics::SourceMap;
    use crate::eval::env::Program;
    use std::sync::Arc;

    fn fresh_program() -> Program {
        let mut sources = SourceMap::new();
        sources.add(std::path::PathBuf::from("entry.ybm"), String::new());
        Program::new(Arc::new(sources))
    }

    #[test]
    fn install_registers_result_option_value_enums() {
        let mut program = fresh_program();
        install(&mut program);
        let Some(result) = program.enums.get("Result") else {
            panic!("Result must be registered")
        };
        assert_eq!(result.variants.len(), 2);
        let Some(option) = program.enums.get("Option") else {
            panic!("Option must be registered")
        };
        assert_eq!(option.variants.len(), 2);
        let Some(value) = program.enums.get("Value") else {
            panic!("Value must be registered")
        };
        assert_eq!(value.variants.len(), 7);
    }

    #[test]
    fn install_registers_error_and_http_proc_structs() {
        let mut program = fresh_program();
        install(&mut program);
        let Some(error) = program.structs.get("Error") else {
            panic!("Error must be registered")
        };
        assert_eq!(error.fields.len(), 3);
        let Some(response) = program.structs.get("Response") else {
            panic!("Response must be registered")
        };
        assert_eq!(response.fields.len(), 3);
        let Some(http_options) = program.structs.get("HttpOptions") else {
            panic!("HttpOptions must be registered")
        };
        assert_eq!(http_options.fields.len(), 2);
        let Some(proc_output) = program.structs.get("ProcOutput") else {
            panic!("ProcOutput must be registered")
        };
        assert_eq!(proc_output.fields.len(), 3);
    }

    #[test]
    fn install_registers_flat_namespace_builtin_function_names() {
        let mut program = fresh_program();
        install(&mut program);
        for name in ["int", "float", "str", "print", "eprint", "assert", "set"] {
            assert!(
                program.functions.contains_key(name),
                "expected {name} to be pre-registered for E1001 collision detection"
            );
        }
    }

    /// Verifies that the placeholder `FunctionDecl`s (int/float/str/print/eprint/assert/set)
    /// that `install` registers do not themselves run afoul of the general rule in
    /// `types/check_decl.rs::check_all_decls` (a function whose return type isn't void cannot
    /// have an empty body, §5.6). `int`/`float`/`str`/`set` were originally given their
    /// "proper" respective return types, which -- since their bodies stayed empty -- violated
    /// this rule and caused TypeCheck to incorrectly fail for **every** user program across
    /// every path that calls `prelude::install` followed by `check_program` (which includes
    /// `check_all_decls`); this test caught that bug (the root cause of the integration tests
    /// that `stdlib::builtins::tests`/`stdlib::time::tests` had `#[ignore]`d for the same
    /// reason, see the comment on `builtin_functions` above) -- the fix was to unify every
    /// placeholder's return type to `void`.
    #[test]
    fn install_placeholders_type_check_cleanly_on_their_own() {
        let mut program = fresh_program();
        install(&mut program);
        let mut diagnostics = crate::diagnostics::DiagnosticBag::new();
        crate::types::check_decl::check_all_decls(&mut program, &mut diagnostics);
        assert!(
            !diagnostics.has_any(),
            "prelude placeholders must type-check cleanly on their own: {:?}",
            diagnostics.into_vec()
        );
    }
}
