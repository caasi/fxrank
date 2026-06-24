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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    Free,
    Ctor,
    Method,
    Member,
    ModuleInit,
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
    /// `(module_id, exported_name)` when the unit is re-exported.
    pub export: Option<(String, String)>,
    pub effects: Vec<crate::effect::Effect>,
    pub risks: Vec<crate::effect::RiskFeature>,
    pub refs: Vec<CallSiteRef>,
    pub async_boundary: bool,
    pub await_count: usize,
    pub language: crate::frontend::Language,
}

/// How an external call site (one that cannot be resolved locally) is reached.
#[derive(Debug, Clone, serde::Serialize)]
pub enum ReachKind {
    ThirdParty,
    FirstPartyOutOfScope,
    Dynamic,
    Ambiguous,
}

/// A call site that crosses the corpus boundary — emitted as part of the
/// wire-format `Report` so consumers can see what fell outside the scan.
#[derive(Debug, Clone, serde::Serialize)]
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
            export: None,
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
            export: None,
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
    fn language_is_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Language::Rust);
        set.insert(Language::Ts);
        set.insert(Language::Python);
        assert_eq!(set.len(), 3);
    }
}
