//! Language-neutral symbol-name resolver.
//!
//! `SymbolIndex` maps a simple name (last `::` or `.` segment of a unit's
//! `symbol`) to the set of `UnitId`s that define it.  `resolve_ref` turns a
//! `CallSiteRef` into a graph `Edge` using name-based lookup; module-path
//! precision is deferred to phase 3.

use std::collections::HashMap;

use crate::{
    graph::Edge,
    record::{CallSiteRef, ExternalReach, ReachKind, RefKind, UnitId, UnitRecord},
};

/// Maps simple names → the `UnitId`s that define them.
pub struct SymbolIndex {
    map: HashMap<String, Vec<UnitId>>,
}

impl SymbolIndex {
    /// Build the index from a slice of `UnitRecord`s.
    ///
    /// The simple name of a unit is the last segment of its `symbol` after
    /// splitting on `::` or `.` — e.g. `"S::method"` → `"method"`,
    /// `"C.m"` → `"m"`, `"foo"` → `"foo"`.  When two units share the same
    /// simple name (e.g. `C.m` and `D.m` both → `"m"`), both `UnitId`s are
    /// stored under that key; `resolve_ref` drops ambiguous matches.
    pub fn from_records(records: &[UnitRecord]) -> Self {
        let mut map: HashMap<String, Vec<UnitId>> = HashMap::new();
        for rec in records {
            let simple = simple_name_of(&rec.symbol);
            map.entry(simple.to_owned())
                .or_default()
                .push(rec.unit_id.clone());
        }
        Self { map }
    }
}

/// Return the last `::` or `.` segment of a symbol name (unit-side).
///
/// This mirrors `simple_callee_of` so that dot-qualified symbols (e.g. TS/Python
/// `C.m`) are indexed under the same key (`m`) that the callee-side lookup
/// produces.  Rust `Foo::bar` indexes under `bar` — `::` contains `:`, so the
/// split on `[: .]` yields the same result as the old `"::"` split.
fn simple_name_of(symbol: &str) -> &str {
    symbol.rsplit([':', '.']).next().unwrap_or(symbol)
}

/// Return the last `::` or `.` segment of a call-site base string (callee-side).
fn simple_callee_of(base: &str) -> &str {
    base.rsplit([':', '.']).next().unwrap_or(base)
}

/// Resolve a `CallSiteRef` to an optional graph `Edge`.
///
/// **Method-kind early drop:** `RefKind::Method` refs return `None` unconditionally —
/// before any index lookup.  Without receiver-type info we cannot confirm any match,
/// and a qualified method ref with no local match must NOT become an `Opaque` reach
/// (a method call is not the same as a first/third-party module import).  Real IO from
/// methods is already captured as an `Effect` by the detectors.
///
/// Returns `None` for other non-reach cases:
/// - More than one in-scope match (ambiguous) → `None` (phase-3 module-tree
///   will disambiguate; an internal ambiguity is not an outward reach).
/// - Zero match with `r.qualified == false` → `None` (bare unqualified names
///   like `push`, `clone`, `Some` are not outward reaches).
///
/// Returns `Some(edge)` for:
/// - Exactly one matching unit → `Some(Edge::Resolved(id))`.
/// - Zero match with `r.qualified == true` → `Some(Edge::Opaque(reach))`;
///   `reach.kind` is `FirstPartyOutOfScope` when `r.first_party` is true
///   (the callee is known to belong to the same project but is outside the
///   scan scope), or `ThirdParty` otherwise (external / stdlib dependency).
///   E.g. a Rust `::` path call or a TS/Python import-resolved reference.
///
/// `referencing_path` is the `path` of the unit that contains the call site;
/// it is used to build the `site` string `"path:line:col"`.
///
/// Note: the qualifier judgment (`r.qualified`) is set by each frontend, keeping
/// language-specific syntax (e.g. `::`) out of this language-neutral core.
pub fn resolve_ref(r: &CallSiteRef, idx: &SymbolIndex, referencing_path: &str) -> Option<Edge> {
    // Method refs are a hard drop: no receiver type → can never confirm a match,
    // and a qualified method ref must NOT fall through to an Opaque external reach.
    if r.kind == RefKind::Method {
        return None;
    }

    let site = format!("{referencing_path}:{}:{}", r.line, r.col);
    let callee = simple_callee_of(&r.base);

    match idx.map.get(callee).map(|v| v.as_slice()) {
        Some([id]) => Some(Edge::Resolved(id.clone())),
        Some(_) => None, // ambiguous → drop
        None => {
            // Only record an external reach for frontend-qualified references.
            // Bare names (`r.qualified == false`) are noise —
            // real IO is already captured as an Effect by the detectors.
            if r.qualified {
                let kind = if r.first_party {
                    ReachKind::FirstPartyOutOfScope
                } else {
                    ReachKind::ThirdParty
                };
                Some(Edge::Opaque(ExternalReach {
                    specifier: r.module.clone().unwrap_or_else(|| r.base.clone()),
                    kind,
                    site,
                }))
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::*;

    fn rec(id: &str, sym: &str) -> UnitRecord {
        UnitRecord {
            unit_id: id.into(),
            path: id.split(':').next().unwrap_or(id).into(),
            line: 1,
            col: 1,
            symbol: sym.into(),
            is_root: false,
            export: None,
            effects: vec![],
            risks: vec![],
            refs: vec![],
            async_boundary: false,
            await_count: 0,
            language: crate::frontend::Language::Rust,
        }
    }

    #[test]
    fn resolves_unique_cross_file_symbol_else_reach() {
        let recs = vec![
            rec("a.rs:1:1:helper", "helper"),
            rec("b.rs:1:1:caller", "caller"),
        ];
        let idx = SymbolIndex::from_records(&recs);

        // Unique match → Some(Resolved) regardless of qualified
        let call = CallSiteRef {
            kind: RefKind::Free,
            base: "helper".into(),
            module: None,
            line: 2,
            col: 3,
            qualified: false,
            first_party: false,
        };
        assert!(
            matches!(resolve_ref(&call, &idx, "b.rs"), Some(Edge::Resolved(ref id)) if id == "a.rs:1:1:helper")
        );

        // No match, qualified == true → Some(Opaque ThirdParty)
        let qualified = CallSiteRef {
            kind: RefKind::Free,
            base: "std::fs::write".into(),
            module: Some("std".into()),
            line: 2,
            col: 3,
            qualified: true,
            first_party: false,
        };
        assert!(matches!(
            resolve_ref(&qualified, &idx, "b.rs"),
            Some(Edge::Opaque(ref r)) if matches!(r.kind, ReachKind::ThirdParty)
        ));

        // No match, qualified == false → None (noise — not an outward reach)
        let bare = CallSiteRef {
            kind: RefKind::Free,
            base: "push".into(),
            module: None,
            line: 2,
            col: 3,
            qualified: false,
            first_party: false,
        };
        assert!(resolve_ref(&bare, &idx, "b.rs").is_none());

        // No match, bare constructor-like name, qualified == false → None
        let bare_some = CallSiteRef {
            kind: RefKind::Free,
            base: "Some".into(),
            module: None,
            line: 3,
            col: 5,
            qualified: false,
            first_party: false,
        };
        assert!(resolve_ref(&bare_some, &idx, "b.rs").is_none());

        // Method call, qualified == false → None (method calls are not outward reaches)
        let method_call = CallSiteRef {
            kind: RefKind::Method,
            base: "len".into(),
            module: None,
            line: 4,
            col: 2,
            qualified: false,
            first_party: false,
        };
        assert!(resolve_ref(&method_call, &idx, "b.rs").is_none());

        // Ambiguous match → None (internal ambiguity; phase-3 disambiguates)
        let recs2 = vec![rec("a.rs:1:1:dup", "dup"), rec("b.rs:1:1:dup", "dup")];
        let idx2 = SymbolIndex::from_records(&recs2);
        let dup_call = CallSiteRef {
            kind: RefKind::Free,
            base: "dup".into(),
            module: None,
            line: 5,
            col: 1,
            qualified: false,
            first_party: false,
        };
        assert!(resolve_ref(&dup_call, &idx2, "c.rs").is_none());
    }

    #[test]
    fn opaque_reach_kind_follows_first_party_flag() {
        // No records in index → every qualified ref becomes Opaque.
        let idx = SymbolIndex::from_records(&[]);

        // first_party: true, qualified: true, no match → FirstPartyOutOfScope
        let fp_ref = CallSiteRef {
            kind: RefKind::Free,
            base: "mylib::utils::helper".into(),
            module: Some("mylib".into()),
            line: 10,
            col: 5,
            qualified: true,
            first_party: true,
        };
        let edge = resolve_ref(&fp_ref, &idx, "src/main.rs");
        assert!(
            matches!(
                &edge,
                Some(Edge::Opaque(r)) if matches!(r.kind, ReachKind::FirstPartyOutOfScope)
            ),
            "first_party=true must yield FirstPartyOutOfScope"
        );

        // first_party: false, qualified: true, no match → ThirdParty
        let tp_ref = CallSiteRef {
            kind: RefKind::Free,
            base: "serde::Serialize".into(),
            module: Some("serde".into()),
            line: 11,
            col: 5,
            qualified: true,
            first_party: false,
        };
        let edge2 = resolve_ref(&tp_ref, &idx, "src/main.rs");
        assert!(
            matches!(
                &edge2,
                Some(Edge::Opaque(r)) if matches!(r.kind, ReachKind::ThirdParty)
            ),
            "first_party=false must yield ThirdParty"
        );
    }

    /// T1: a dot-qualified TS/Python symbol `C.m` is indexed under `m` and a
    /// call whose callee resolves to `m` → `Edge::Resolved`.
    #[test]
    fn dot_qualified_symbol_resolves_via_simple_name() {
        // The unit symbol uses dot notation (TS class method / Python method).
        let recs = vec![rec("a.ts:5:3:C.m", "C.m")];
        let idx = SymbolIndex::from_records(&recs);

        // A qualified call whose base is just `m` (callee-side simple segment).
        let call = CallSiteRef {
            kind: RefKind::Free,
            base: "m".into(),
            module: None,
            line: 10,
            col: 1,
            qualified: true,
            first_party: true,
        };
        assert!(
            matches!(
                resolve_ref(&call, &idx, "b.ts"),
                Some(Edge::Resolved(ref id)) if id == "a.ts:5:3:C.m"
            ),
            "dot-qualified symbol C.m must resolve to the C.m unit via the `m` key"
        );
    }

    /// T2: two units `C.m` and `D.m` share the simple name `m`; a call to `m`
    /// is AMBIGUOUS and must be dropped (returns `None`).
    #[test]
    fn dot_qualified_collision_is_ambiguous_not_silently_overwritten() {
        let recs = vec![rec("a.ts:1:1:C.m", "C.m"), rec("b.ts:1:1:D.m", "D.m")];
        let idx = SymbolIndex::from_records(&recs);

        // Both units index under `m`; the lookup must see multiple entries → None.
        let call = CallSiteRef {
            kind: RefKind::Free,
            base: "m".into(),
            module: None,
            line: 5,
            col: 1,
            qualified: false,
            first_party: false,
        };
        assert!(
            resolve_ref(&call, &idx, "c.ts").is_none(),
            "C.m + D.m both map to `m` → ambiguous → must return None, not resolve to one of them"
        );

        // Verify the index actually holds two entries for `m` (not a silent overwrite).
        assert_eq!(
            idx.map.get("m").map(|v| v.len()),
            Some(2),
            "index must store both C.m and D.m under `m`, not overwrite one"
        );
    }

    /// T-method-1: a Method-kind ref with callee "push" that matches a lone in-scope
    /// `fn push()` must return `None` — no receiver type info, so it MUST NOT resolve.
    #[test]
    fn method_kind_ref_does_not_resolve_even_when_unique_match() {
        // A lone `fn push()` is in scope.
        let recs = vec![rec("a.rs:1:1:push", "push")];
        let idx = SymbolIndex::from_records(&recs);

        let method_ref = CallSiteRef {
            kind: RefKind::Method,
            base: "a.push".into(), // caller does `a.push(1)`; simple_callee_of → "push"
            module: None,
            line: 5,
            col: 3,
            qualified: false,
            first_party: false,
        };
        assert!(
            resolve_ref(&method_ref, &idx, "b.rs").is_none(),
            "Method-kind ref must not resolve even when a lone `fn push` is in scope"
        );
    }

    /// T-method-2: a Free-kind ref with callee "helper" + lone in-scope `fn helper()`
    /// must still resolve → `Edge::Resolved`. Free-function intra-file propagation
    /// must NOT be broken by the method guard.
    #[test]
    fn free_kind_ref_still_resolves_when_unique_match() {
        let recs = vec![rec("a.rs:1:1:helper", "helper")];
        let idx = SymbolIndex::from_records(&recs);

        let free_ref = CallSiteRef {
            kind: RefKind::Free,
            base: "helper".into(),
            module: None,
            line: 3,
            col: 5,
            qualified: false,
            first_party: false,
        };
        assert!(
            matches!(
                resolve_ref(&free_ref, &idx, "b.rs"),
                Some(Edge::Resolved(ref id)) if id == "a.rs:1:1:helper"
            ),
            "Free-kind ref must still resolve to the lone in-scope helper"
        );
    }

    /// T-method-3: a Method-kind ref with `qualified=true` and NO local match must
    /// return `None` — it must NOT fall through to an `Opaque` external reach.
    #[test]
    fn method_kind_qualified_no_match_returns_none_not_opaque() {
        // Empty index — no local match at all.
        let idx = SymbolIndex::from_records(&[]);

        let method_ref = CallSiteRef {
            kind: RefKind::Method,
            base: "fetch".into(),
            module: None,
            line: 7,
            col: 5,
            qualified: true,
            first_party: false,
        };
        assert!(
            resolve_ref(&method_ref, &idx, "src/lib.rs").is_none(),
            "Method-kind + qualified=true + no match must return None, not Opaque"
        );
    }

    /// T3: Rust `Foo::bar` still indexes under `bar` — the `::` contains `:`,
    /// so splitting on `[: .]` yields the same simple name as before.
    #[test]
    fn rust_colon_colon_path_still_resolves_by_last_segment() {
        let recs = vec![rec("lib.rs:1:1:Foo::bar", "Foo::bar")];
        let idx = SymbolIndex::from_records(&recs);

        let call = CallSiteRef {
            kind: RefKind::Free,
            base: "Foo::bar".into(),
            module: None,
            line: 2,
            col: 1,
            qualified: true,
            first_party: true,
        };
        assert!(
            matches!(
                resolve_ref(&call, &idx, "main.rs"),
                Some(Edge::Resolved(ref id)) if id == "lib.rs:1:1:Foo::bar"
            ),
            "Rust Foo::bar must still resolve via the `bar` simple name"
        );
    }
}
