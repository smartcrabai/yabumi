//! Foundation of the type system (ARCHITECTURE.md §3.7). `Ty`/`EffectSet`/`NamespaceId`
//! are the foundation other phases depend on, and carry minimal behavior (the actual
//! algorithms for unification, inference, etc. live in infer.rs/generics.rs/check_*.rs).

pub mod check_decl;
pub mod check_expr;
pub mod check_stmt;
pub mod env;
pub mod exhaustiveness;
pub mod generics;
pub mod infer;
pub mod mutability;
pub mod resolutions;

pub use resolutions::{BareIdentKind, CallKind, Resolutions, WrapKind};

use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Void,
    /// list/dict/set/tuple carry many builtin checking rules (D-TYPE-04 element
    /// unification, D-TYPE-05 key constraint, D-COL-01 insertion order), so they get
    /// dedicated variants instead of being embedded in `Named` (judgment call made here:
    /// checking via Rust pattern matching is less error-prone and faster than checking
    /// via a string comparison against "list").
    List(Box<Ty>),
    Dict(Box<Ty>, Box<Ty>),
    Set(Box<Ty>),
    Tuple(Vec<Ty>),
    /// Uniformly represents user-defined structs/enums as well as builtin enums
    /// (Result/Option/Value) (this carries the D-TYPE-09 decision, "Result/Option are
    /// ordinary enums with no special-cased syntax", straight through into the `Ty`
    /// representation -- they share a single struct/enum registry).
    Named {
        name: Arc<str>,
        args: Vec<Ty>,
    },
    Function {
        params: Vec<Ty>,
        effects: EffectSet,
        ret: Box<Ty>,
    },
    /// A type variable that appears **only while** type-checking a generic function/
    /// struct/enum declaration. Never remains in a concrete type once unification has
    /// completed (the starting point of the type erasure described in §3.8).
    TypeVar(Arc<str>),
    /// An internal-only recovery placeholder introduced as the minimal addition by Unit7
    /// (type checking). Right after a single diagnostic is reported (an unannotated
    /// parameter E1002, an unresolved identifier, etc.), this is the "always compatible
    /// with the other side" type that keeps any expression using that value afterward from
    /// triggering a cascade of unrelated additional diagnostics (in the spirit of
    /// D-CLI-03 "collect everything" -- limit to one diagnostic per root cause).
    /// Additionally reused as the type of the whole expression when all branches of an
    /// if/match diverge (equivalent to Rust's `!` type) -- the two cases have different
    /// reasons but identical "may silently conform to any expected type" behavior, so
    /// they are represented by this single variant instead of adding another dedicated
    /// one. An internal-only type that never remains in a fully diagnosed program; no
    /// evaluator or other phase ever consults it.
    Unknown,
}

/// SPEC §8 "granularity is fixed at 6 kinds" -- a closed set, so represented as a
/// bitflag. No reason to use an open representation such as `HashSet<String>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EffectSet(u8);

impl EffectSet {
    pub const FS: Self = Self(1 << 0);
    pub const NET: Self = Self(1 << 1);
    pub const ENV: Self = Self(1 << 2);
    pub const PROC: Self = Self(1 << 3);
    pub const TIME: Self = Self(1 << 4);
    pub const RAND: Self = Self(1 << 5);

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `self` is fully contained in `superset` (used for E2002: checking that
    /// nothing exceeds the declared `uses`).
    #[must_use]
    pub const fn is_subset_of(self, superset: Self) -> bool {
        self.0 & !superset.0 == 0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "fs" => Some(Self::FS),
            "net" => Some(Self::NET),
            "env" => Some(Self::ENV),
            "proc" => Some(Self::PROC),
            "time" => Some(Self::TIME),
            "rand" => Some(Self::RAND),
            _ => None,
        }
    }

    /// For generating diagnostic messages (e.g. stringifying as "uses {net, fs}").
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        // For each flag f, check "is f contained in self" (self.0 & f.0 != 0) --
        // is_subset_of goes the opposite direction ("is self a subset of f"), so it isn't
        // used here.
        [
            ("fs", Self::FS),
            ("net", Self::NET),
            ("env", Self::ENV),
            ("proc", Self::PROC),
            ("time", Self::TIME),
            ("rand", Self::RAND),
        ]
        .into_iter()
        .filter(move |(_, f)| self.0 & f.0 != 0)
        .map(|(n, _)| n)
    }
}

/// Fixed identifiers for builtin namespaces (D-LEX-01). Belongs to a name-resolution
/// system separate from the flat namespace (D-TYPE-07) -- even if a user defines a
/// top-level function or variable with the same name as `fs`/`json`/etc., it does not
/// affect the resolution of a `.`-qualified access
/// (NAMESPACE-QUALIFIED-ACCESS-NO-RESOLUTION-HOME decision, ARCHITECTURE.md §5.12/§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamespaceId {
    Fs,
    Http,
    Env,
    Proc,
    Time,
    Rand,
    Regex,
    Math,
    Json,
    Csv,
    Yaml,
    Toml,
}

impl NamespaceId {
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "fs" => Some(Self::Fs),
            "http" => Some(Self::Http),
            "env" => Some(Self::Env),
            "proc" => Some(Self::Proc),
            "time" => Some(Self::Time),
            "rand" => Some(Self::Rand),
            "regex" => Some(Self::Regex),
            "math" => Some(Self::Math),
            "json" => Some(Self::Json),
            "csv" => Some(Self::Csv),
            "yaml" => Some(Self::Yaml),
            "toml" => Some(Self::Toml),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EffectSet, NamespaceId};

    #[test]
    fn effect_set_union_and_subset() {
        let fs_net = EffectSet::FS.union(EffectSet::NET);
        assert!(EffectSet::FS.is_subset_of(fs_net));
        assert!(!fs_net.is_subset_of(EffectSet::FS));
        assert!(EffectSet::empty().is_subset_of(EffectSet::empty()));
    }

    #[test]
    fn effect_set_from_name_round_trips_known_names() {
        for name in ["fs", "net", "env", "proc", "time", "rand"] {
            assert!(
                EffectSet::from_name(name).is_some(),
                "expected {name} to be recognized"
            );
        }
        assert_eq!(EffectSet::from_name("bogus"), None);
    }

    #[test]
    fn namespace_id_from_name_covers_all_twelve() {
        let names = [
            "fs", "http", "env", "proc", "time", "rand", "regex", "math", "json", "csv", "yaml",
            "toml",
        ];
        for name in names {
            assert!(
                NamespaceId::from_name(name).is_some(),
                "expected {name} to be a namespace"
            );
        }
        assert_eq!(NamespaceId::from_name("not_a_namespace"), None);
    }
}
