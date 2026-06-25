//! Call-graph built from `UnitRecord`s via a caller-supplied resolver closure.
//!
//! `CallGraph` is resolution-policy-free: the caller provides a `resolve`
//! closure that maps a `CallSiteRef` to either a known `UnitId` inside the
//! corpus (`Edge::Resolved`) or an `ExternalReach` that crossed the corpus
//! boundary (`Edge::Opaque`).

use std::collections::HashMap;

use crate::record::{CallSiteRef, ExternalReach, UnitId, UnitRecord};

/// A directed edge in the call graph.
pub enum Edge {
    /// The callee was found in the corpus.
    Resolved(UnitId),
    /// The callee could not be resolved within the scanned corpus.
    Opaque(ExternalReach),
}

/// The corpus-wide call graph.
pub struct CallGraph {
    pub nodes: HashMap<UnitId, UnitRecord>,
    pub edges: HashMap<UnitId, Vec<Edge>>,
}

impl CallGraph {
    /// Build a `CallGraph` from a flat list of `UnitRecord`s.
    ///
    /// `resolve` is called for every `CallSiteRef` in every record; it receives
    /// the site, the OWNING `UnitRecord` (so it can build a referencing-path /
    /// `site` string), and the full node map (already indexed) so it can look up
    /// potential callees.  Resolution policy lives entirely in the closure —
    /// `graph.rs` remains parser-free.
    ///
    /// The resolver returns `Option<Edge>`: `None` means "drop this ref" (e.g.
    /// bare single-segment names that are not meaningful outward reaches); only
    /// `Some(_)` results produce edges in the graph.
    pub fn from_records(
        records: Vec<UnitRecord>,
        resolve: impl Fn(&CallSiteRef, &UnitRecord, &HashMap<UnitId, UnitRecord>) -> Option<Edge>,
    ) -> Self {
        // Index nodes first so the resolver can look up callees.
        let nodes: HashMap<UnitId, UnitRecord> = records
            .into_iter()
            .map(|r| (r.unit_id.clone(), r))
            .collect();

        // Build edges by running the resolver over every ref in every node. The
        // owning record is passed so the resolver can attribute the call site to
        // its referencing path. `None` results are filtered out — they are not
        // meaningful outward reaches and produce no edge.
        let edges: HashMap<UnitId, Vec<Edge>> = nodes
            .iter()
            .map(|(id, record)| {
                let resolved: Vec<Edge> = record
                    .refs
                    .iter()
                    .filter_map(|site| resolve(site, record, &nodes))
                    .collect();
                (id.clone(), resolved)
            })
            .collect();

        Self { nodes, edges }
    }

    /// Iterate over the IDs of root nodes (`is_root == true`).
    pub fn roots(&self) -> impl Iterator<Item = &UnitId> {
        self.nodes
            .iter()
            .filter(|(_, r)| r.is_root)
            .map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::*;

    fn rec(id: &str, refs: Vec<&str>) -> UnitRecord {
        UnitRecord {
            unit_id: id.into(),
            path: id.into(),
            line: 1,
            col: 1,
            symbol: id.into(),
            is_root: id == "root",
            canonical_path: vec![],
            aliases: vec![],
            effects: vec![],
            risks: vec![],
            refs: refs
                .into_iter()
                .map(|b| CallSiteRef {
                    kind: RefKind::Free,
                    base: b.into(),
                    module: None,
                    line: 1,
                    col: 1,
                    qualified: false,
                    first_party: false,
                    resolved_target: None,
                })
                .collect(),
            async_boundary: false,
            await_count: 0,
            language: crate::frontend::Language::Rust,
        }
    }

    #[test]
    fn builds_resolved_edges_by_base_name() {
        let recs = vec![rec("root", vec!["b"]), rec("b", vec![])];
        let g = CallGraph::from_records(recs, |r, _owner, nodes| {
            match nodes.keys().find(|k| **k == r.base) {
                Some(id) => Some(Edge::Resolved(id.clone())),
                None => Some(Edge::Opaque(ExternalReach {
                    specifier: r.base.clone(),
                    kind: ReachKind::ThirdParty,
                    site: "x".into(),
                })),
            }
        });
        assert!(matches!(g.edges["root"][0], Edge::Resolved(ref id) if id == "b"));
        assert_eq!(g.roots().count(), 1);
    }
}
