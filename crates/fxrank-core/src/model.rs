use crate::effect::{Effect, RiskFeature};
use crate::record::ExternalReach;
use crate::score::rank_key;
use serde::Serialize;

/// One escaping effect or risk a hotspot inherited from a transitive callee,
/// carrying bounded (exemplar) provenance. The `kind`/`class` describe the
/// signal; `from`/`via` describe where it came from and one representative
/// discovery path. Compact wire form — see the fold's `Inherited` for the source.
// Field-declaration order is `(kind, class, from, via)`, so the derived `Ord` is
// exactly #46's stable sort key for each hotspot's `inherited[]`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct InheritedSignal {
    /// Effect or risk wire string (e.g. `"net.fs.db"`, `"ffi.call"`).
    pub kind: String,
    pub class: u8,
    /// Origin unit id (`path:line:col:symbol`) where the signal was first seen.
    pub from: String,
    /// One exemplar discovery path from this hotspot to the origin.
    pub via: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Scope {
    pub input: String,
    pub files: usize,
    pub parsed: usize,
    pub functions: usize,
    pub skipped_tests: usize,
    pub skipped_excluded: usize,
    pub risk_features: Vec<RiskFeature>,
    /// Corpus-boundary reaches (third-party / out-of-scope call sites) observed
    /// across the whole scan, deduped. Cross-file propagation surface.
    pub external_reaches: Vec<ExternalReach>,
}

impl Scope {
    pub fn empty(input: &str) -> Self {
        Scope {
            input: input.into(),
            files: 0,
            parsed: 0,
            functions: 0,
            skipped_tests: 0,
            skipped_excluded: 0,
            risk_features: vec![],
            external_reaches: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub own_score: f64,
    pub max_class: u8,
    pub risk_weight: u32,
    pub confidence: f64,
    /// Propagated aggregates (own ∪ inherited): the max effect class and score
    /// after cross-file folding. These drive the ranking; the own-body fields
    /// above describe each function's local cost.
    pub propagated_score: f64,
    pub propagated_max_class: u8,
    /// Deduped union of corpus-boundary reaches across all hotspots + scope.
    pub external_reaches: Vec<ExternalReach>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hotspot {
    pub id: String,
    pub symbol: String,
    pub path: String,
    pub line: usize,
    pub max_class: u8,
    pub own_score: f64,
    pub risk_weight: u32,
    pub confidence: f64,
    pub async_boundary: bool,
    pub await_count: usize,
    pub effects: Vec<Effect>,
    pub risk_features: Vec<RiskFeature>,
    /// Propagated aggregates (own ∪ inherited). Before cross-file folding wires
    /// these (a later task), they mirror the own-body fields — see `own_seed`.
    pub propagated_score: f64,
    pub propagated_max_class: u8,
    /// True when this unit's file was an explicit CLI FILE argument (or stdin) —
    /// the agent's observation focus. Set centrally by the CLI; `apply_fold`
    /// copies it from `record.is_root`. Annotation only (the fold never seeds
    /// from it). See the guideline *Roots — the agent's observation focus*.
    pub root: bool,
    /// Escaping signals folded in from transitive callees, each with bounded
    /// provenance. Empty until cross-file folding wires it.
    pub inherited: Vec<InheritedSignal>,
    /// Corpus-boundary reaches contributed by this function (own + transitive).
    /// Empty until cross-file folding wires it.
    pub external_reaches: Vec<ExternalReach>,
}

impl Hotspot {
    /// Seed the propagated fields for a not-yet-folded hotspot: propagated
    /// mirrors own (`propagated_score == own_score`, `propagated_max_class ==
    /// max_class`), `root == false` (stub — phase-3 roots wiring sets `true` for
    /// actual roots), and the inherited/reach sets are empty. Used as the `..`
    /// base in frontend construction so a frontend only supplies the own-body
    /// fields; cross-file folding overwrites these later.
    pub fn own_seed(own_score: f64, max_class: u8) -> Hotspot {
        Hotspot {
            id: String::new(),
            symbol: String::new(),
            path: String::new(),
            line: 0,
            max_class,
            own_score,
            risk_weight: 0,
            confidence: 1.0,
            async_boundary: false,
            await_count: 0,
            effects: vec![],
            risk_features: vec![],
            propagated_score: own_score,
            propagated_max_class: max_class,
            root: false,
            inherited: vec![],
            external_reaches: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub path: String,
    pub parsed: bool,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub scope: Scope,
    pub summary: Summary,
    pub hotspots: Vec<Hotspot>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Deduped union of the external reaches carried by `scope` and every hotspot,
/// keyed by `(specifier, site)`. First occurrence wins. The caller (`Report::build`)
/// sorts the result before serialization: insertion order here follows hash-container
/// iteration upstream and is NOT stable across runs (#46).
fn dedup_reaches(scope: &Scope, hotspots: &[Hotspot]) -> Vec<ExternalReach> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut push = |r: &ExternalReach, out: &mut Vec<ExternalReach>| {
        if seen.insert((r.specifier.clone(), r.site.clone())) {
            out.push(r.clone());
        }
    };
    for r in &scope.external_reaches {
        push(r, &mut out);
    }
    for h in hotspots {
        for r in &h.external_reaches {
            push(r, &mut out);
        }
    }
    out
}

impl Report {
    pub fn build(
        mut scope: Scope,
        mut hotspots: Vec<Hotspot>,
        diagnostics: Vec<Diagnostic>,
        limit: Option<usize>,
    ) -> Report {
        // Sort descending by the PROPAGATED rank_key (own ∪ inherited cost),
        // tie-break by id ascending (stable sort preserves equal-key order).
        hotspots.sort_by(|a, b| {
            let ka = rank_key(
                a.propagated_max_class,
                a.propagated_score,
                a.risk_weight,
                a.confidence,
            );
            let kb = rank_key(
                b.propagated_max_class,
                b.propagated_score,
                b.risk_weight,
                b.confidence,
            );
            kb.cmp(&ka).then_with(|| a.id.cmp(&b.id))
        });

        // Compute summary over ALL hotspots AND scope.risk_features BEFORE truncation
        let own_score = hotspots.iter().map(|h| h.own_score).fold(0.0_f64, f64::max);

        let max_class_hotspots = hotspots.iter().map(|h| h.max_class).max().unwrap_or(0);
        let max_class_scope = scope
            .risk_features
            .iter()
            .map(|r| r.class)
            .max()
            .unwrap_or(0);
        let max_class = max_class_hotspots.max(max_class_scope);

        let risk_weight_hotspots = hotspots.iter().map(|h| h.risk_weight).max().unwrap_or(0);
        let risk_weight_scope = scope
            .risk_features
            .iter()
            .map(|r| r.weight)
            .max()
            .unwrap_or(0);
        let risk_weight = risk_weight_hotspots.max(risk_weight_scope);

        let confidence = if hotspots.is_empty() {
            1.0
        } else {
            hotspots
                .iter()
                .map(|h| h.confidence)
                .fold(f64::INFINITY, f64::min)
        };

        // Propagated aggregates: max over hotspots' propagated values, with the
        // scope risk class still able to dominate `propagated_max_class` (a risk
        // feature carries no propagated/own distinction — it is a scope-level
        // fact, just like for the own-body `max_class`).
        let propagated_score = hotspots
            .iter()
            .map(|h| h.propagated_score)
            .fold(0.0_f64, f64::max);
        let propagated_max_class = hotspots
            .iter()
            .map(|h| h.propagated_max_class)
            .max()
            .unwrap_or(0)
            .max(max_class_scope);

        // Deduped union of external reaches across all hotspots + scope. Sort by the
        // derived `Ord` (specifier, kind, site) so the serialized list is stable
        // run-to-run (#46) — the union itself came from hash iteration upstream.
        let mut external_reaches = dedup_reaches(&scope, &hotspots);
        external_reaches.sort();

        let summary = Summary {
            own_score,
            max_class,
            risk_weight,
            confidence,
            propagated_score,
            propagated_max_class,
            external_reaches,
        };

        // Truncate AFTER computing summary
        if let Some(n) = limit {
            hotspots.truncate(n);
        }

        // Determinism (#46): the list fields below are accumulated in hash containers
        // upstream, so their order varies run-to-run even on identical input. Sort
        // each by its type's derived `Ord` (the #46 stable keys) right before
        // serialization. The per-hotspot sort runs AFTER truncation, so only the
        // retained top-N pay for it.
        scope.external_reaches.sort();
        for h in &mut hotspots {
            h.inherited.sort();
            h.external_reaches.sort();
        }

        Report {
            scope,
            summary,
            hotspots,
            diagnostics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{RiskFeature, RiskKind, Tier};
    use crate::score::weight_for_class;

    /// A legacy own-only hotspot: propagated mirrors own (via `own_seed`), so the
    /// pre-propagation ranking/summary assertions still hold unchanged.
    fn hot(id: &str, max_class: u8, own_score: f64, conf: f64) -> Hotspot {
        Hotspot {
            id: id.into(),
            symbol: id.into(),
            path: "f.rs".into(),
            line: 1,
            confidence: conf,
            ..Hotspot::own_seed(own_score, max_class)
        }
    }

    /// A hotspot with DISTINCT own vs propagated values — used to prove ranking
    /// keys off the propagated aggregates, not the own-body ones.
    fn hot_prop(
        id: &str,
        own_max_class: u8,
        own_score: f64,
        prop_max_class: u8,
        prop_score: f64,
        conf: f64,
    ) -> Hotspot {
        Hotspot {
            id: id.into(),
            symbol: id.into(),
            path: "f.rs".into(),
            line: 1,
            max_class: own_max_class,
            own_score,
            confidence: conf,
            propagated_score: prop_score,
            propagated_max_class: prop_max_class,
            ..Hotspot::own_seed(own_score, own_max_class)
        }
    }

    fn risk(kind: RiskKind, path: &str, line: usize) -> RiskFeature {
        RiskFeature {
            kind,
            class: kind.class(),
            weight: weight_for_class(kind.class()),
            path: path.into(),
            line,
            col: 1,
            evidence: kind.wire().into(),
            tier: Tier::Exact,
        }
    }

    #[test]
    fn summary_takes_max_and_min_over_two_hotspots() {
        let report = Report::build(
            Scope {
                input: "f.rs".into(),
                files: 1,
                parsed: 1,
                functions: 2,
                skipped_tests: 0,
                skipped_excluded: 0,
                risk_features: vec![],
                external_reaches: vec![],
            },
            vec![hot("a", 4, 5.0, 0.9), hot("b", 7, 25.5, 0.6)],
            vec![],
            None,
        );
        assert_eq!(report.summary.own_score, 25.5); // max, not sum
        assert_eq!(report.summary.max_class, 7);
        assert_eq!(report.summary.confidence, 0.6); // min
        assert_eq!(report.hotspots[0].id, "b"); // ranked first
    }

    #[test]
    fn whole_own_score_serializes_with_point_zero() {
        let report = Report::build(
            Scope::empty("f.rs"),
            vec![hot("x", 3, 3.0, 0.6)],
            vec![],
            None,
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains('\n'));
        assert!(json.contains("\"own_score\":3.0"));
    }

    #[test]
    fn zero_hotspots_defaults() {
        let report = Report::build(Scope::empty("stdin"), vec![], vec![], None);
        assert_eq!(report.summary.own_score, 0.0);
        assert_eq!(report.summary.confidence, 1.0);
        assert_eq!(report.summary.max_class, 0);
    }

    #[test]
    fn scope_risk_feeds_summary_even_with_zero_hotspots() {
        let scope = Scope {
            input: "f.rs".into(),
            files: 1,
            parsed: 1,
            functions: 0,
            skipped_tests: 0,
            skipped_excluded: 0,
            risk_features: vec![risk(RiskKind::ImplDrop, "f.rs", 3)], // class 2, weight 2
            external_reaches: vec![],
        };
        let report = Report::build(scope, vec![], vec![], None);
        assert_eq!(report.summary.max_class, 2); // from scope risk, not a hotspot
        assert_eq!(report.summary.risk_weight, 2);
    }

    #[test]
    fn scope_risk_dominates_hotspot_class_when_higher() {
        // RiskKind::Transmute => class 7; hotspot max_class = 4
        let scope = Scope {
            input: "f.rs".into(),
            files: 1,
            parsed: 1,
            functions: 1,
            skipped_tests: 0,
            skipped_excluded: 0,
            risk_features: vec![risk(RiskKind::Transmute, "f.rs", 1)],
            external_reaches: vec![],
        };
        let report = Report::build(scope, vec![hot("a", 4, 5.0, 0.9)], vec![], None);
        assert_eq!(report.summary.max_class, 7); // scope risk wins over hotspot's 4
        assert_eq!(report.summary.risk_weight, 21); // weight_for_class(7)
    }

    #[test]
    fn scope_serializes_skipped_tests_between_functions_and_risk_features() {
        let report = Report::build(
            Scope {
                input: "f".into(),
                files: 1,
                parsed: 1,
                functions: 2,
                skipped_tests: 3,
                skipped_excluded: 5,
                risk_features: vec![],
                external_reaches: vec![],
            },
            vec![],
            vec![],
            None,
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains(
            "\"functions\":2,\"skipped_tests\":3,\"skipped_excluded\":5,\"risk_features\":"
        ));
    }

    #[test]
    fn limit_truncates_hotspots_not_summary() {
        let report = Report::build(
            Scope {
                input: "f.rs".into(),
                files: 1,
                parsed: 1,
                functions: 3,
                skipped_tests: 0,
                skipped_excluded: 0,
                risk_features: vec![],
                external_reaches: vec![],
            },
            vec![
                hot("a", 4, 5.0, 0.9),
                hot("b", 7, 25.5, 0.6),
                hot("c", 6, 13.0, 0.8),
            ],
            vec![],
            Some(1),
        );
        assert_eq!(report.hotspots.len(), 1); // truncated
        assert_eq!(report.hotspots[0].id, "b"); // the top-ranked one kept
        assert_eq!(report.summary.max_class, 7); // summary still over ALL functions
    }

    #[test]
    fn ranks_by_propagated_not_own() {
        // "a": own class 0 / score 0 BUT propagated class 7 / score 28.5 — a pure
        // wrapper whose callee does IO. "b": own class 4 / score 5, no callees so
        // propagated == own. Ranking is by propagated, so "a" must come first.
        let report = Report::build(
            Scope::empty("x"),
            vec![
                hot_prop("a", 0, 0.0, 7, 28.5, 0.9),
                hot_prop("b", 4, 5.0, 4, 5.0, 0.9),
            ],
            vec![],
            None,
        );
        assert_eq!(report.hotspots[0].id, "a");
        assert_eq!(report.summary.propagated_max_class, 7);
        assert_eq!(report.summary.propagated_score, 28.5);
        // own-body summary still reflects the LOCAL cost, unchanged by propagation.
        assert_eq!(report.summary.max_class, 4);
        assert_eq!(report.summary.own_score, 5.0);
    }

    #[test]
    fn scope_risk_dominates_propagated_max_class() {
        // RiskKind::Transmute => class 7; both hotspots propagate at most class 4.
        let scope = Scope {
            input: "f.rs".into(),
            files: 1,
            parsed: 1,
            functions: 1,
            skipped_tests: 0,
            skipped_excluded: 0,
            risk_features: vec![risk(RiskKind::Transmute, "f.rs", 1)],
            external_reaches: vec![],
        };
        let report = Report::build(
            scope,
            vec![hot_prop("a", 4, 5.0, 4, 5.0, 0.9)],
            vec![],
            None,
        );
        assert_eq!(report.summary.propagated_max_class, 7); // scope risk wins
    }

    #[test]
    fn summary_external_reaches_dedup_union() {
        use crate::record::{ExternalReach, ReachKind};
        let reach = |spec: &str, site: &str| ExternalReach {
            specifier: spec.into(),
            kind: ReachKind::ThirdParty,
            site: site.into(),
        };
        let mut h1 = hot("a", 7, 21.0, 0.9);
        h1.external_reaches = vec![reach("axios", "a.ts:1"), reach("fs", "a.ts:2")];
        let mut h2 = hot("b", 7, 21.0, 0.9);
        // "axios" at the same (specifier, site) is a duplicate; "lodash" is new.
        h2.external_reaches = vec![reach("axios", "a.ts:1"), reach("lodash", "b.ts:9")];

        let report = Report::build(Scope::empty("x"), vec![h1, h2], vec![], None);
        let specs: Vec<&str> = report
            .summary
            .external_reaches
            .iter()
            .map(|r| r.specifier.as_str())
            .collect();
        // Deduped, then sorted by (specifier, kind, site) per #46 — here the sorted
        // order happens to coincide with insertion order (axios < fs < lodash).
        assert_eq!(specs, vec!["axios", "fs", "lodash"]);
    }

    #[test]
    fn report_build_sorts_list_fields_for_determinism() {
        use crate::record::{ExternalReach, ReachKind};
        let reach = |spec: &str, kind: ReachKind, site: &str| ExternalReach {
            specifier: spec.into(),
            kind,
            site: site.into(),
        };
        let inh = |kind: &str, class: u8, from: &str, via: &str| InheritedSignal {
            kind: kind.into(),
            class,
            from: from.into(),
            via: via.into(),
        };
        // A hotspot whose inherited[] and external_reaches[] are deliberately out of
        // order (as hash-container iteration upstream would deliver them).
        let h = Hotspot {
            id: "h".into(),
            inherited: vec![
                inh("net.fs.db", 7, "z.rs:1:1:z", "h->z"),
                inh("env.read", 4, "a.rs:1:1:a", "h->a"),
                inh("net.fs.db", 7, "a.rs:1:1:a", "h->a"),
            ],
            external_reaches: vec![
                reach("lodash", ReachKind::ThirdParty, "b.ts:2"),
                reach("axios", ReachKind::FirstPartyOutOfScope, "a.ts:1"),
                reach("axios", ReachKind::ThirdParty, "a.ts:1"),
            ],
            ..Hotspot::own_seed(10.0, 7)
        };
        let scope = Scope {
            external_reaches: vec![
                reach("react", ReachKind::ThirdParty, "c.ts:3"),
                reach("@app/x", ReachKind::FirstPartyOutOfScope, "c.ts:9"),
            ],
            ..Scope::empty("f")
        };

        let report = Report::build(scope, vec![h], vec![], None);

        // Every serialized list field comes out sorted by its type's derived `Ord`
        // (#46's stable keys), regardless of the input order above.
        let sorted_reaches = |v: &[ExternalReach]| v.windows(2).all(|w| w[0] <= w[1]);
        let hs = &report.hotspots[0];
        assert!(
            hs.inherited.windows(2).all(|w| w[0] <= w[1]),
            "hotspot.inherited must be sorted: {:?}",
            hs.inherited
        );
        assert!(
            sorted_reaches(&hs.external_reaches),
            "hotspot.external_reaches must be sorted"
        );
        assert!(
            sorted_reaches(&report.scope.external_reaches),
            "scope.external_reaches must be sorted"
        );
        assert!(
            sorted_reaches(&report.summary.external_reaches),
            "summary.external_reaches must be sorted"
        );
    }

    #[test]
    fn propagated_fields_serialize_after_own_body() {
        let report = Report::build(
            Scope::empty("f.rs"),
            vec![hot_prop("x", 3, 3.0, 7, 28.0, 0.9)],
            vec![],
            None,
        );
        let json = serde_json::to_string(&report).unwrap();
        // own-body fields appear before the propagated ones in the hotspot object.
        let own = json.find("\"own_score\":3.0").unwrap();
        let prop = json.find("\"propagated_score\":28.0").unwrap();
        assert!(
            own < prop,
            "own-body fields must serialize before propagated"
        );
        // whole f64 renders with point-zero (spec-001 convention).
        assert!(json.contains("\"propagated_score\":28.0"));
    }
}
