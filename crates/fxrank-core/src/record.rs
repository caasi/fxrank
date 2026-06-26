//! Language-neutral pass-1 intermediate format.
//!
//! Frontends emit `UnitRecord`s during their first pass; the fold (Tasks 6–9)
//! consumes them to propagate cross-file effects.

/// Unique function/unit identifier: `"path:line:col:symbol"`.
pub type UnitId = String;

/// Dedup key for a call site; implements `Eq + Hash` for de-duplication sets.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SiteKey {
    pub unit: UnitId,
    pub line: usize,
    pub col: usize,
    pub kind: String,
}

/// How the callee is referenced at a call site.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RefKind {
    #[default]
    Free,
    Ctor,
    Method,
    Member,
    ModuleInit,
}

/// A re-export alias fact: `alias_path` names the same definition as `target`.
/// Emitted by a frontend per re-export (`pub use`, TS barrel, Python `__init__`);
/// the core indexes it as an extra key in `CanonicalIndex` (spec 025-3e §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasFact {
    pub alias_path: Vec<String>,
    pub target: Vec<String>,
}

/// A single outgoing call / reference inside a function body.
#[derive(Debug, Clone)]
pub struct CallSiteRef {
    pub kind: RefKind,
    /// The resolvable local name or receiver path.
    pub base: String,
    /// Import module string, if the base is an imported name.
    pub module: Option<String>,
    pub line: usize,
    pub col: usize,
    /// Frontend-classified: this reference is a *qualified outward reference* —
    /// eligible to become an external reach if it does not resolve in-scope.
    /// Bare unqualified names and builtin method calls are NOT qualified (they
    /// are intra-language noise, not the app's import surface).
    /// Rust sets this from `::`-qualification; TS from import-specifier
    /// resolution (planned); Python from dotted/import (planned).
    pub qualified: bool,
    /// True when the frontend determined the callee belongs to the same first-party
    /// codebase (e.g. a workspace-local crate, a relative TS import, a same-package
    /// Python module) but is outside the current scan scope.
    /// Controls `resolve_ref`: unresolved + qualified + first_party → `FirstPartyOutOfScope`;
    /// unresolved + qualified + !first_party → `ThirdParty`.
    /// Default `false` preserves the historical `ThirdParty` behaviour.
    pub first_party: bool,
    /// Frontend's import-resolved canonical callee path (spec 025-3e §4.1).
    /// `Some(path)` = resolved against the module tree; `None` = not produced
    /// (frontend not adopted, or attempted-but-out-of-corpus). The
    /// adopted/non-adopted distinction is by the partition gate, not this field.
    pub resolved_target: Option<Vec<String>>,
}

/// All information the resolver needs about one function unit, emitted by a
/// frontend in pass 1.
#[derive(Debug, Clone)]
pub struct UnitRecord {
    pub unit_id: UnitId,
    pub path: String,
    pub line: usize,
    pub col: usize,
    pub symbol: String,
    /// True when this unit's file was an explicit CLI FILE argument (the agent's
    /// observation focus).  Set centrally at the CLI; frontends always emit `false`.
    /// Annotation only — the fold never seeds from it.  See the guideline
    /// *Roots — the agent's observation focus*.
    pub is_root: bool,
    /// Unit's canonical fully-qualified path as segments, e.g.
    /// `["crate","helpers","write"]` (spec 025-3e §4.1). Empty ⇒ the frontend
    /// could not assign one (no crate root in scope, cfg/macro module, …) ⇒
    /// the unit participates only via the degradation rules.
    pub canonical_path: Vec<String>,
    /// Re-export alias facts emitted by the frontend (replaces the old, unused
    /// `export` field). One per detected re-export.
    pub aliases: Vec<AliasFact>,
    pub effects: Vec<crate::effect::Effect>,
    pub risks: Vec<crate::effect::RiskFeature>,
    pub refs: Vec<CallSiteRef>,
    pub async_boundary: bool,
    pub await_count: usize,
    pub language: crate::frontend::Language,
}

/// How an external call site (one that cannot be resolved locally) is reached.
// `Ord` (variant-declaration order) lets `external_reaches` sort deterministically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum ReachKind {
    ThirdParty,
    FirstPartyOutOfScope,
    Dynamic,
    Ambiguous,
}

/// A call site that crosses the corpus boundary — emitted as part of the
/// wire-format `Report` so consumers can see what fell outside the scan.
// Field-declaration order is `(specifier, kind, site)`, so the derived `Ord` is
// exactly #46's stable sort key for `external_reaches`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct ExternalReach {
    pub specifier: String,
    pub kind: ReachKind,
    pub site: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::Language;

    #[test]
    fn unit_record_holds_effects_and_refs() {
        let r = UnitRecord {
            unit_id: "a.rs:1:1:f".into(),
            path: "a.rs".into(),
            line: 1,
            col: 1,
            symbol: "f".into(),
            is_root: true,
            canonical_path: vec![],
            aliases: vec![],
            effects: vec![],
            risks: vec![],
            refs: vec![CallSiteRef {
                kind: RefKind::Free,
                base: "g".into(),
                module: None,
                line: 2,
                col: 3,
                qualified: false,
                first_party: false,
                resolved_target: None,
            }],
            async_boundary: false,
            await_count: 0,
            language: Language::Rust,
        };
        assert_eq!(r.refs.len(), 1);
        assert!(r.is_root);
    }

    #[test]
    fn unit_record_language_field() {
        let r = UnitRecord {
            unit_id: "a.rs:1:1:f".into(),
            path: "a.rs".into(),
            line: 1,
            col: 1,
            symbol: "f".into(),
            is_root: false,
            canonical_path: vec![],
            aliases: vec![],
            effects: vec![],
            risks: vec![],
            refs: vec![],
            async_boundary: false,
            await_count: 0,
            language: Language::Rust,
        };
        assert_eq!(r.language, Language::Rust);
    }

    #[test]
    fn new_neutral_fields_default_to_non_adopted() {
        let r = UnitRecord {
            unit_id: "a.rs:1:1:f".into(),
            path: "a.rs".into(),
            line: 1,
            col: 1,
            symbol: "f".into(),
            is_root: false,
            canonical_path: vec![],
            aliases: vec![],
            effects: vec![],
            risks: vec![],
            refs: vec![CallSiteRef {
                kind: RefKind::Free,
                base: "g".into(),
                module: None,
                line: 2,
                col: 3,
                qualified: false,
                first_party: false,
                resolved_target: None,
            }],
            async_boundary: false,
            await_count: 0,
            language: Language::Rust,
        };
        assert!(r.canonical_path.is_empty());
        assert!(r.aliases.is_empty());
        assert_eq!(r.refs[0].resolved_target, None);
        // AliasFact constructs and compares by value.
        let a = AliasFact {
            alias_path: vec!["m".into(), "x".into()],
            target: vec!["n".into(), "x".into()],
        };
        assert_eq!(a.alias_path, vec!["m".to_string(), "x".to_string()]);
        assert_eq!(RefKind::default(), RefKind::Free);
    }

    #[test]
    fn language_is_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Language::Rust);
        set.insert(Language::Ts);
        set.insert(Language::Python);
        assert_eq!(set.len(), 3);
    }
}
