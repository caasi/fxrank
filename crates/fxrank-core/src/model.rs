use crate::effect::{Effect, RiskFeature};
use crate::score::rank_key;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Scope {
    pub input: String,
    pub files: usize,
    pub parsed: usize,
    pub functions: usize,
    pub skipped_tests: usize,
    pub risk_features: Vec<RiskFeature>,
}

impl Scope {
    pub fn empty(input: &str) -> Self {
        Scope {
            input: input.into(),
            files: 0,
            parsed: 0,
            functions: 0,
            skipped_tests: 0,
            risk_features: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub own_score: f64,
    pub max_class: u8,
    pub risk_weight: u32,
    pub confidence: f64,
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

impl Report {
    pub fn build(
        scope: Scope,
        mut hotspots: Vec<Hotspot>,
        diagnostics: Vec<Diagnostic>,
        limit: Option<usize>,
    ) -> Report {
        // Sort descending by rank_key, tie-break by id ascending (stable sort preserves equal-key order)
        hotspots.sort_by(|a, b| {
            let ka = rank_key(a.max_class, a.own_score, a.risk_weight, a.confidence);
            let kb = rank_key(b.max_class, b.own_score, b.risk_weight, b.confidence);
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

        let summary = Summary {
            own_score,
            max_class,
            risk_weight,
            confidence,
        };

        // Truncate AFTER computing summary
        if let Some(n) = limit {
            hotspots.truncate(n);
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

    fn hot(id: &str, max_class: u8, own_score: f64, conf: f64) -> Hotspot {
        Hotspot {
            id: id.into(),
            symbol: id.into(),
            path: "f.rs".into(),
            line: 1,
            max_class,
            own_score,
            risk_weight: 0,
            confidence: conf,
            async_boundary: false,
            await_count: 0,
            effects: vec![],
            risk_features: vec![],
        }
    }

    fn risk(kind: RiskKind, path: &str, line: usize) -> RiskFeature {
        RiskFeature {
            kind,
            class: kind.class(),
            weight: weight_for_class(kind.class()),
            path: path.into(),
            line,
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
                risk_features: vec![],
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
            risk_features: vec![risk(RiskKind::ImplDrop, "f.rs", 3)], // class 2, weight 2
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
            risk_features: vec![risk(RiskKind::Transmute, "f.rs", 1)],
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
                risk_features: vec![],
            },
            vec![],
            vec![],
            None,
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"functions\":2,\"skipped_tests\":3,\"risk_features\":"));
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
                risk_features: vec![],
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
}
