//! Cross-unit effect propagation — fold phase.
//!
//! The live entry points are [`fold`] (transitive propagation) and [`apply_fold`]
//! (augments frontend-produced `Hotspot`s in place). `boundary_summary` and
//! `to_hotspots` are test-only helpers retained for unit-testing fold algebra in
//! isolation.

use std::collections::{HashMap, HashSet};

use crate::effect::{Effect, EffectKind, RiskFeature, Tier};
use crate::graph::{CallGraph, Edge};
use crate::model::Hotspot;
use crate::record::{ExternalReach, SiteKey, UnitId};

/// Signals that a unit propagates upward to its callers (own ∪ inherited,
/// full set — used for final ranking in Task 10).
pub struct Propagated {
    pub effects: Vec<Effect>,
    pub risks: Vec<RiskFeature>,
    pub inherited: Vec<Inherited>,
    /// Outward-surface reaches; wired in Task 9 (opaque edges). Empty here.
    pub external_reaches: Vec<ExternalReach>,
}

/// A single inherited signal with provenance (which unit it came from and via
/// which call-site description). Provenance is **exemplar** — one representative
/// `via` path per inherited `SiteKey`, never the full path set.
pub struct Inherited {
    pub effect: Option<Effect>,
    pub risk: Option<RiskFeature>,
    pub from: UnitId,
    pub via: String,
}

/// Return the *escaping* subset of `unit`'s own effects and risks — the seed
/// for cross-unit propagation (single-hop; no transitive walk yet).
///
/// An effect escapes when `Effect::escapes()` is true (i.e. `ExternalUnresolved`
/// or `!contained`). A risk escapes when `RiskKind::escapes()` is true.
///
/// Test-only: the live path is `apply_fold` (which drives the full transitive fold
/// via `own_escaping_sigs`); `boundary_summary` is a superseded single-hop entry
/// point retained for unit-testing the escaping-filter logic in isolation.
#[cfg(test)]
pub fn boundary_summary(unit: &UnitId, g: &CallGraph) -> (Vec<Effect>, Vec<RiskFeature>) {
    let Some(record) = g.nodes.get(unit) else {
        return (vec![], vec![]);
    };
    let effects = record
        .effects
        .iter()
        .filter(|e| e.escapes())
        .cloned()
        .collect();
    let risks = record
        .risks
        .iter()
        .filter(|r| r.kind.escapes())
        .cloned()
        .collect();
    (effects, risks)
}

/// One escaping signal travelling through the call graph, keyed by its origin
/// `SiteKey` for dedup. `via` is the exemplar discovery path (origin → … →
/// holder); `from` is the origin unit.
#[derive(Clone)]
struct Sig {
    key: SiteKey,
    effect: Option<Effect>,
    risk: Option<RiskFeature>,
    from: UnitId,
    via: String,
}

/// The escaping own-signals of a unit, each tagged with its `SiteKey` and a
/// trivial single-element `via` (the unit itself, as the path so far).
fn own_escaping_sigs(unit: &UnitId, g: &CallGraph) -> Vec<Sig> {
    let Some(record) = g.nodes.get(unit) else {
        return vec![];
    };
    let mut out = Vec::new();
    for e in record.effects.iter().filter(|e| e.escapes()) {
        out.push(Sig {
            key: SiteKey {
                unit: unit.clone(),
                line: e.line,
                col: e.col,
                kind: e.kind.wire().to_string(),
            },
            effect: Some(e.clone()),
            risk: None,
            from: unit.clone(),
            via: unit.clone(),
        });
    }
    for r in record.risks.iter().filter(|r| r.kind.escapes()) {
        out.push(Sig {
            key: SiteKey {
                unit: unit.clone(),
                line: r.line,
                col: r.col,
                kind: r.kind.wire().to_string(),
            },
            effect: None,
            risk: Some(r.clone()),
            from: unit.clone(),
            via: unit.clone(),
        });
    }
    out
}

/// Resolved (intra-corpus) callees of a unit.
fn resolved_callees<'a>(unit: &UnitId, g: &'a CallGraph) -> Vec<&'a UnitId> {
    g.edges
        .get(unit)
        .into_iter()
        .flatten()
        .filter_map(|edge| match edge {
            Edge::Resolved(id) => Some(id),
            Edge::Opaque(_) => None,
        })
        .collect()
}

/// Insert a signal into the SiteKey-deduped accumulator. The FIRST insertion of
/// a key wins (bounded/exemplar provenance — one representative path per site).
fn insert_dedup(acc: &mut HashMap<SiteKey, Sig>, sig: Sig) {
    acc.entry(sig.key.clone()).or_insert(sig);
}

/// Compute strongly-connected components of the resolved-edge subgraph using an
/// iterative Tarjan. Returns components in **reverse-topological order** (a
/// component appears before the components that call into it), which is exactly
/// the order in which downstream summaries are already available.
fn tarjan_sccs(g: &CallGraph) -> Vec<Vec<UnitId>> {
    #[derive(Clone)]
    struct NodeState {
        index: Option<usize>,
        lowlink: usize,
        on_stack: bool,
    }

    let mut state: HashMap<UnitId, NodeState> = g
        .nodes
        .keys()
        .map(|id| {
            (
                id.clone(),
                NodeState {
                    index: None,
                    lowlink: 0,
                    on_stack: false,
                },
            )
        })
        .collect();
    let mut next_index = 0usize;
    let mut stack: Vec<UnitId> = Vec::new();
    let mut sccs: Vec<Vec<UnitId>> = Vec::new();

    // A deterministic node order so output is stable across runs.
    let mut roots: Vec<UnitId> = g.nodes.keys().cloned().collect();
    roots.sort();

    // Explicit work stack: each frame is (node, child-cursor into its callees).
    for start in roots {
        if state[&start].index.is_some() {
            continue;
        }
        let mut work: Vec<(UnitId, usize)> = vec![(start, 0)];
        while let Some((v, ci)) = work.last().cloned() {
            if ci == 0 {
                // First visit of v: assign index/lowlink, push on stack.
                let s = state.get_mut(&v).unwrap();
                s.index = Some(next_index);
                s.lowlink = next_index;
                s.on_stack = true;
                next_index += 1;
                stack.push(v.clone());
            }
            let callees = resolved_callees(&v, g);
            if ci < callees.len() {
                // Advance this frame's cursor before descending.
                work.last_mut().unwrap().1 = ci + 1;
                let w = callees[ci].clone();
                match state[&w].index {
                    None => work.push((w, 0)),
                    Some(_) => {
                        if state[&w].on_stack {
                            let w_index = state[&w].index.unwrap();
                            let s = state.get_mut(&v).unwrap();
                            if w_index < s.lowlink {
                                s.lowlink = w_index;
                            }
                        }
                    }
                }
            } else {
                // Done with v's children: pop the frame.
                work.pop();
                let v_low = state[&v].lowlink;
                let v_index = state[&v].index.unwrap();
                if v_low == v_index {
                    // v is an SCC root: pop the stack down to v.
                    let mut comp = Vec::new();
                    loop {
                        let w = stack.pop().unwrap();
                        state.get_mut(&w).unwrap().on_stack = false;
                        comp.push(w.clone());
                        if w == v {
                            break;
                        }
                    }
                    comp.sort();
                    sccs.push(comp);
                }
                // Propagate lowlink to the parent frame.
                if let Some((parent, _)) = work.last() {
                    let parent = parent.clone();
                    if v_low < state[&parent].lowlink {
                        state.get_mut(&parent).unwrap().lowlink = v_low;
                    }
                }
            }
        }
    }
    sccs
}

/// Opaque edges of a unit (third-party / out-of-scope reaches).
fn opaque_edges<'a>(unit: &UnitId, g: &'a CallGraph) -> Vec<&'a ExternalReach> {
    g.edges
        .get(unit)
        .into_iter()
        .flatten()
        .filter_map(|edge| match edge {
            Edge::Opaque(reach) => Some(reach),
            Edge::Resolved(_) => None,
        })
        .collect()
}

/// The separator joining `via` path segments. Centralized so the segment-aware
/// `path_starts_with` check stays in lockstep with how paths are built.
const VIA_SEP: &str = " → ";

/// True when `via`'s FIRST path segment equals `unit` exactly. Segment-aware (not
/// a substring/prefix test) so a unit named `a` does not match a path starting at
/// `ab`.
fn path_starts_with(via: &str, unit: &UnitId) -> bool {
    via.split(VIA_SEP).next() == Some(unit.as_str())
}

/// Dedup key for an `ExternalReach` — (specifier, site) pair.
fn reach_key(r: &ExternalReach) -> (String, String) {
    (r.specifier.clone(), r.site.clone())
}

/// Synthesize an `ExternalUnresolved`/class-2 escaping `Effect` from an opaque reach.
/// Uses `Tier::Heuristic` with the unresolved-call penalty via `detection_confidence`.
///
/// The `SiteKey` uses the **actual call-site location** parsed from `reach.site`
/// (format `"path:line:col"` — parse from the end to handle paths containing `:`).
/// This ensures two opaque calls to the same specifier at different call sites
/// get distinct keys and are counted separately in the propagated set.
/// Falls back to (0, 0) if parsing fails.
fn synthesize_opaque_effect(reach: &ExternalReach, origin: &UnitId) -> (SiteKey, Sig) {
    let kind = EffectKind::ExternalUnresolved;
    let class = kind.base_class();

    // Parse line and col from the END of the site string ("path:line:col").
    // Split from the right so paths containing `:` (e.g. Windows paths or
    // unit-id-shaped strings) don't confuse the parse.
    let (site_line, site_col) = parse_site_line_col(&reach.site);

    let effect = Effect {
        kind,
        class,
        discounted_to: None,
        weight: crate::score::weight_for_class(class),
        line: site_line,
        col: site_col,
        tier: Tier::Heuristic,
        hidden: false,
        contained: false,
        evidence: reach.specifier.clone(),
        discount: None,
        subreason: None,
        confidence: crate::confidence::detection_confidence(Tier::Heuristic, true, false),
    };
    // Include the call-site location in the key so distinct same-specifier
    // sites get distinct SiteKeys and are not collapsed by `or_insert`.
    let key = SiteKey {
        unit: origin.clone(),
        line: site_line,
        col: site_col,
        kind: format!("{}:{}", kind.wire(), reach.specifier),
    };
    let sig = Sig {
        key: key.clone(),
        effect: Some(effect),
        risk: None,
        from: origin.clone(),
        via: origin.clone(),
    };
    (key, sig)
}

/// Parse `(line, col)` from a site string of the form `"path:line:col"`.
/// Splits from the right: last segment = col, second-to-last = line.
/// Returns `(0, 0)` if the string has fewer than 2 `:` delimited segments.
fn parse_site_line_col(site: &str) -> (usize, usize) {
    let mut parts = site.rsplitn(3, ':');
    let col = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let line = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (line, col)
}

/// Transitive escaping-effect propagation over the call graph.
///
/// `summary(u) = escaping(own(u)) ∪ ⋃ summary(resolved callee)`, deduped by
/// `SiteKey = (origin_unit, line, col, kind)`. Cycles terminate and converge:
/// every member of a strongly-connected component receives the SAME summary.
/// Provenance is bounded — one exemplar `via` path per inherited `SiteKey`.
///
/// Opaque edges (Task 9): each `Edge::Opaque(reach)` contributes
/// (a) a synthesized `ExternalUnresolved`/class-2 effect that propagates up the
/// graph through the normal `comp_summary` machinery, and
/// (b) the `ExternalReach` itself into `comp_reaches`, which also propagates up
/// so every transitive caller's `external_reaches` carries the reach.
pub fn fold(g: &CallGraph) -> HashMap<UnitId, Propagated> {
    // SCCs in reverse-topological order: a component's downstream callees are
    // in earlier components, so their summaries are ready when we reach it.
    let sccs = tarjan_sccs(g);

    // Map each unit to its component index for fast "same SCC?" checks.
    let mut comp_of: HashMap<UnitId, usize> = HashMap::new();
    for (i, comp) in sccs.iter().enumerate() {
        for u in comp {
            comp_of.insert(u.clone(), i);
        }
    }

    // The deduped summary set for each component (one value per SCC).
    let mut comp_summary: Vec<HashMap<SiteKey, Sig>> = vec![HashMap::new(); sccs.len()];
    // The deduped external-reach set for each component (one per SCC).
    // Keyed by (specifier, site) so the same third-party reached via two paths
    // appears only once.
    let mut comp_reaches: Vec<HashMap<(String, String), ExternalReach>> =
        vec![HashMap::new(); sccs.len()];

    for ci in 0..sccs.len() {
        let mut acc: HashMap<SiteKey, Sig> = HashMap::new();
        let mut reach_acc: HashMap<(String, String), ExternalReach> = HashMap::new();
        let members: HashSet<&UnitId> = sccs[ci].iter().collect();

        for member in &sccs[ci] {
            // 1. The member's own escaping signals.
            for sig in own_escaping_sigs(member, g) {
                insert_dedup(&mut acc, sig);
            }
            // 1b. Opaque edges: synthesize ExternalUnresolved effects + collect reaches.
            for reach in opaque_edges(member, g) {
                let (key, sig) = synthesize_opaque_effect(reach, member);
                acc.entry(key).or_insert(sig);
                reach_acc
                    .entry(reach_key(reach))
                    .or_insert_with(|| reach.clone());
            }
            // 2. Summaries of resolved callees in *downstream* components.
            //    Intra-SCC edges are skipped here; the shared component summary
            //    (computed across all members) is what makes the cycle converge.
            for callee in resolved_callees(member, g) {
                if members.contains(callee) {
                    continue; // same SCC — handled by member iteration
                }
                let callee_ci = comp_of[callee];
                for sig in comp_summary[callee_ci].values() {
                    // Extend the exemplar path: member → (callee's path).
                    let extended = Sig {
                        via: format!("{} → {}", member, sig.via),
                        ..sig.clone()
                    };
                    insert_dedup(&mut acc, extended);
                }
                // 2b. Pull downstream reaches up to this component.
                for (k, r) in &comp_reaches[callee_ci] {
                    reach_acc.entry(k.clone()).or_insert_with(|| r.clone());
                }
            }
        }
        comp_summary[ci] = acc;
        comp_reaches[ci] = reach_acc;
    }

    // Build the per-unit Propagated. Every member of a component shares that
    // component's deduped summary (one summary per SCC).
    //
    // OWN signals — taken DIRECTLY from g.nodes (ALL effects and risks, no
    // SiteKey dedup). This preserves same-line duplicate escaping effects that
    // the component summary would collapse (two col-0 effects of the same kind
    // share a SiteKey, so only one survives in the deduped summary).
    //
    // INHERITED signals — taken from the component summary, BUT ONLY where
    // sig.from != unit (i.e. the signal originated in a callee / other SCC
    // member, not this unit itself). The SiteKey dedup on the summary correctly
    // ensures a diamond (same callee reachable via two paths) is counted once.
    //
    // This separation guarantees: propagated_score >= own_score AND
    // propagated_max_class >= own_max_class for every unit, including those
    // with same-line col-0 duplicate escaping effects (Codex-B).
    let mut out: HashMap<UnitId, Propagated> = HashMap::new();
    for unit in g.nodes.keys() {
        let ci = comp_of[unit];
        let summary = &comp_summary[ci];

        let mut effects: Vec<Effect> = Vec::new();
        let mut risks: Vec<RiskFeature> = Vec::new();
        let mut inherited: Vec<Inherited> = Vec::new();

        // OWN part: ALL own effects and risks directly from the node record,
        // undeduped. This is the full body cost that own_score counts.
        if let Some(record) = g.nodes.get(unit) {
            effects.extend(record.effects.iter().cloned());
            risks.extend(record.risks.iter().cloned());
        }

        // INHERITED part: signals from the component summary where the origin
        // is NOT this unit (i.e. came from a downstream callee or another SCC
        // member). The SiteKey dedup on the summary keeps diamond-inherited
        // signals at count-once. The opaque-edge synthesized ExternalUnresolved
        // effects live ONLY in comp_summary (never in g.nodes), so — unlike the
        // unit's real own effects, which come from the node above — they MUST be
        // pulled here from the summary's own-origin (sig.from == unit) entries.
        //
        // Note: opaque synthesized effects are inserted into comp_summary with
        // sig.from == origin (the unit that has the opaque edge). When building
        // own effects from g.nodes above we don't include the synthesized
        // ExternalUnresolved (it lives only in comp_summary, not in the node's
        // own effects vector). So we also pull own-origin opaque effects from
        // the summary here, but they are NOT added to inherited[].
        for sig in summary.values() {
            let is_own = &sig.from == unit;
            if is_own {
                // Only pull from summary what the node record does NOT already
                // have: synthesized ExternalUnresolved effects (opaque edges).
                // Real own effects are already in the effects vec via g.nodes.
                // We identify synthesized effects by kind == ExternalUnresolved
                // AND origin == this unit (the synthesis always sets from = origin).
                if let Some(e) = &sig.effect {
                    if e.kind == EffectKind::ExternalUnresolved {
                        effects.push(e.clone());
                    }
                }
                // Opaque edges produce only effects (no risks), so no risk branch needed.
            } else {
                // Inherited signal from a callee / other SCC member.
                if let Some(e) = &sig.effect {
                    effects.push(e.clone());
                }
                if let Some(r) = &sig.risk {
                    risks.push(r.clone());
                }
                // Build provenance path. `sig.via` is the exemplar path already
                // accumulated during the fold; its FIRST segment is the member of
                // this SCC that folded the signal in (step 2 prepends each folding
                // member). For a singleton SCC that member IS `unit`, so prepending
                // `unit` again would DOUBLE the first segment ("root → root → a → d").
                // Only prepend when `unit` is a *different* member of a multi-member
                // SCC than the folding member, so the path starts at `unit` and
                // routes through the folding member to the origin.
                let via = if path_starts_with(&sig.via, unit) {
                    sig.via.clone()
                } else {
                    format!("{} → {}", unit, sig.via)
                };
                inherited.push(Inherited {
                    effect: sig.effect.clone(),
                    risk: sig.risk.clone(),
                    from: sig.from.clone(),
                    via,
                });
            }
        }

        let external_reaches: Vec<ExternalReach> = comp_reaches[ci].values().cloned().collect();

        out.insert(
            unit.clone(),
            Propagated {
                effects,
                risks,
                inherited,
                external_reaches,
            },
        );
    }
    out
}

/// The computed propagated fields for a single unit, derived from a `Propagated`.
/// Shared between `apply_fold` (the live path, augments existing Hotspots in place)
/// and the test-only `to_hotspots` so the mapping logic lives in exactly ONE place.
struct PropFields {
    propagated_score: f64,
    propagated_max_class: u8,
    inherited: Vec<crate::model::InheritedSignal>,
    external_reaches: Vec<crate::record::ExternalReach>,
}

/// Derive the propagated fields from a `Propagated`. This is the single
/// canonical mapping from `Propagated → (propagated_score, propagated_max_class,
/// inherited, external_reaches)` used by `apply_fold` (and the test-only `to_hotspots`).
fn propagated_fields(prop: &Propagated) -> PropFields {
    use crate::model::InheritedSignal;
    use crate::score;

    let prop_effect_weights: Vec<u32> = prop.effects.iter().map(|e| e.weight).collect();
    let propagated_score = score::own_score(&prop_effect_weights);

    let prop_effect_classes: Vec<u8> = prop.effects.iter().map(|e| e.effective_class()).collect();
    let prop_risk_class = prop.risks.iter().map(|r| r.class).max().unwrap_or(0);
    let propagated_max_class = score::max_class(&prop_effect_classes, prop_risk_class);

    // Map fold::Inherited → model::InheritedSignal
    let inherited: Vec<InheritedSignal> = prop
        .inherited
        .iter()
        .map(|inh| {
            let (kind, class) = if let Some(e) = &inh.effect {
                (e.kind.wire().to_string(), e.effective_class())
            } else if let Some(r) = &inh.risk {
                (r.kind.wire().to_string(), r.class)
            } else {
                // Degenerate: no signal attached — skip by using a placeholder.
                // In practice every Inherited carries either effect or risk (both may
                // be None only if a future variant is added); this arm is unreachable
                // given current constructors.
                ("unknown".to_string(), 0)
            };
            InheritedSignal {
                kind,
                class,
                from: inh.from.clone(),
                via: inh.via.clone(),
            }
        })
        .collect();

    PropFields {
        propagated_score,
        propagated_max_class,
        inherited,
        external_reaches: prop.external_reaches.clone(),
    }
}

/// Assemble each `UnitRecord` + its `Propagated` from the fold into a wire `Hotspot`.
///
/// - Own fields (`own_score`, `max_class`, `effects`, `risk_features`, …) come from the
///   `UnitRecord`; they describe the function's LOCAL cost.
/// - Propagated fields (`propagated_score`, `propagated_max_class`, `inherited`,
///   `external_reaches`) come from the fold output; they describe the blast-radius cost.
/// - `root` is set from `record.is_root`.
///
/// The `confidence` field uses the weakest-link minimum over all own effects, matching
/// the single-file frontend convention. `risk_weight` uses the max weight over own risks.
///
/// Test-only: the live path is `apply_fold`, which augments existing frontend-produced
/// `Hotspot`s in place. `to_hotspots` is a superseded alternative entry point that
/// builds `Hotspot`s directly from the `CallGraph`; it is retained for unit-testing
/// the fold-to-hotspot mapping logic in isolation (e.g. `dashboard_scenario_root_blast_radius`).
/// If you add a post-gather step to `analyze_unit`, update `apply_fold`, not this function.
#[cfg(test)]
pub fn to_hotspots(g: &CallGraph, folded: &HashMap<UnitId, Propagated>) -> Vec<Hotspot> {
    use crate::score;

    let mut out = Vec::with_capacity(g.nodes.len());

    for (unit_id, record) in &g.nodes {
        // --- Own-body aggregates ---
        let own_effect_weights: Vec<u32> = record.effects.iter().map(|e| e.weight).collect();
        let own_score = score::own_score(&own_effect_weights);

        let own_effect_classes: Vec<u8> =
            record.effects.iter().map(|e| e.effective_class()).collect();
        let own_risk_class = record.risks.iter().map(|r| r.class).max().unwrap_or(0);
        let max_class = score::max_class(&own_effect_classes, own_risk_class);

        let risk_weight = record.risks.iter().map(|r| r.weight).max().unwrap_or(0);

        let confidence = record
            .effects
            .iter()
            .map(|e| e.confidence)
            .fold(1.0_f64, f64::min);

        // --- Propagated aggregates (via shared helper) ---
        let prop = folded
            .get(unit_id)
            .expect("fold must produce an entry for every graph node");
        let pf = propagated_fields(prop);

        let hotspot = Hotspot {
            id: record.unit_id.clone(),
            symbol: record.symbol.clone(),
            path: record.path.clone(),
            line: record.line,
            max_class,
            own_score,
            risk_weight,
            confidence,
            async_boundary: record.async_boundary,
            await_count: record.await_count,
            effects: record.effects.clone(),
            risk_features: record.risks.clone(),
            propagated_score: pf.propagated_score,
            propagated_max_class: pf.propagated_max_class,
            root: record.is_root,
            inherited: pf.inherited,
            external_reaches: pf.external_reaches,
        };
        out.push(hotspot);
    }
    out
}

/// Augment existing `Hotspot`s in place with the propagated fields from `folded`.
///
/// For each hotspot whose `id` has an entry in `folded`, sets:
/// - `propagated_score`, `propagated_max_class`, `inherited`, `external_reaches`
///   (computed from the `Propagated` via the shared `propagated_fields` helper), and
/// - `root` (from the corresponding `CallGraph` node's `is_root` flag).
///
/// **Own-body fields are never touched** (`own_score`, `max_class`, `effects`,
/// `risk_features`, `confidence`, `risk_weight`, `async_boundary`, `await_count`,
/// `id`, `symbol`, `path`, `line`). A hotspot with no matching entry in `folded`
/// is left unchanged (own-seeded values remain).
pub fn apply_fold(hotspots: &mut [Hotspot], g: &CallGraph, folded: &HashMap<UnitId, Propagated>) {
    for hs in hotspots.iter_mut() {
        if let Some(prop) = folded.get(&hs.id) {
            let pf = propagated_fields(prop);
            hs.propagated_score = pf.propagated_score;
            hs.propagated_max_class = pf.propagated_max_class;
            hs.inherited = pf.inherited;
            hs.external_reaches = pf.external_reaches;
            hs.root = g.nodes.get(&hs.id).map(|r| r.is_root).unwrap_or(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{EffectKind, RiskFeature, Tier};
    use crate::graph::{CallGraph, Edge};
    use crate::record::{CallSiteRef, ExternalReach, ReachKind, RefKind, UnitRecord};

    /// Build a `UnitRecord` with the given id, call refs, and effects.
    fn rec_with_effects(id: &str, effects: Vec<Effect>, risks: Vec<RiskFeature>) -> UnitRecord {
        UnitRecord {
            unit_id: id.into(),
            path: id.into(),
            line: 1,
            col: 1,
            symbol: id.into(),
            is_root: id == "root",
            canonical_path: vec![],
            aliases: vec![],
            effects,
            risks,
            refs: vec![],
            async_boundary: false,
            await_count: 0,
            language: crate::frontend::Language::Rust,
        }
    }

    fn make_effect(kind: EffectKind, contained: bool) -> Effect {
        Effect {
            kind,
            class: kind.base_class(),
            discounted_to: None,
            weight: 1,
            line: 1,
            col: 1,
            tier: Tier::Exact,
            hidden: false,
            contained,
            evidence: "test".into(),
            discount: None,
            subreason: None,
            confidence: 1.0,
        }
    }

    /// A free-name resolver: a ref resolves to a same-named node if present,
    /// otherwise it is an opaque third-party reach. Always returns `Some` so
    /// the fold tests' graphs are unchanged behaviorally — all their edges stay.
    fn base_name_resolver(
        r: &CallSiteRef,
        _owner: &UnitRecord,
        nodes: &std::collections::HashMap<UnitId, UnitRecord>,
    ) -> Option<Edge> {
        match nodes.keys().find(|k| **k == r.base) {
            Some(id) => Some(Edge::Resolved(id.clone())),
            None => Some(Edge::Opaque(ExternalReach {
                specifier: r.base.clone(),
                kind: ReachKind::ThirdParty,
                site: "x".into(),
            })),
        }
    }

    /// Build a `UnitRecord` with the given id, outgoing call bases, and effects.
    fn rec_full(
        id: &str,
        refs: Vec<&str>,
        effects: Vec<Effect>,
        risks: Vec<RiskFeature>,
    ) -> UnitRecord {
        UnitRecord {
            unit_id: id.into(),
            path: id.into(),
            line: 1,
            col: 1,
            symbol: id.into(),
            is_root: id == "root",
            canonical_path: vec![],
            aliases: vec![],
            effects,
            risks,
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

    /// Count how many effects of a given kind appear in a propagated set.
    fn count_kind(p: &Propagated, kind: EffectKind) -> usize {
        p.effects.iter().filter(|e| e.kind == kind).count()
    }

    /// Look up a unit's `Propagated` by `&str` id (avoids `.into()` ambiguity).
    fn get<'a>(out: &'a HashMap<UnitId, Propagated>, id: &str) -> &'a Propagated {
        out.get(&UnitId::from(id))
            .expect("unit present in fold output")
    }

    #[test]
    fn transitive_io_reaches_root_diamond_counts_once() {
        // root -> a -> d(io);  root -> b -> d(io)  (diamond)
        // root.propagated effects contain exactly ONE net.fs.db (deduped by site).
        let io = make_effect(EffectKind::NetFsDb, false);
        let root = rec_full("root", vec!["a", "b"], vec![], vec![]);
        let a = rec_full("a", vec!["d"], vec![], vec![]);
        let b = rec_full("b", vec!["d"], vec![], vec![]);
        let d = rec_full("d", vec![], vec![io], vec![]);

        let g = CallGraph::from_records(vec![root, a, b, d], base_name_resolver);
        let out = fold(&g);

        // d's single io site reaches root through both a and b, but is one SiteKey.
        assert_eq!(count_kind(get(&out, "root"), EffectKind::NetFsDb), 1);
        // a and b each inherit it once too.
        assert_eq!(count_kind(get(&out, "a"), EffectKind::NetFsDb), 1);
        assert_eq!(count_kind(get(&out, "b"), EffectKind::NetFsDb), 1);
        // d owns it.
        assert_eq!(count_kind(get(&out, "d"), EffectKind::NetFsDb), 1);
        // root's inherited list carries exactly one entry for that site.
        let root_inh: Vec<_> = get(&out, "root")
            .inherited
            .iter()
            .filter(|i| i.effect.as_ref().map(|e| e.kind) == Some(EffectKind::NetFsDb))
            .collect();
        assert_eq!(root_inh.len(), 1);
        assert_eq!(root_inh[0].from, "d");
        // Exact `via` content: the path from the holder (root) to the origin (d),
        // with NO duplicated first segment. root -> a -> d (or root -> b -> d;
        // the exemplar is whichever path wins dedup, but must not double "root").
        let via = &root_inh[0].via;
        assert!(
            via == "root → a → d" || via == "root → b → d",
            "root's inherited io via must be a single clean path, got {via:?}"
        );
    }

    #[test]
    fn cycle_terminates_one_summary_per_scc() {
        // a -> b -> a (cycle), and b -> c(io). fold() must terminate;
        // a.summary == b.summary and both contain c's io.
        let io = make_effect(EffectKind::NetFsDb, false);
        let a = rec_full("a", vec!["b"], vec![], vec![]);
        let b = rec_full("b", vec!["a", "c"], vec![], vec![]);
        let c = rec_full("c", vec![], vec![io], vec![]);

        let g = CallGraph::from_records(vec![a, b, c], base_name_resolver);
        let out = fold(&g); // must return (termination)

        // Both SCC members reach c's io exactly once.
        assert_eq!(count_kind(get(&out, "a"), EffectKind::NetFsDb), 1);
        assert_eq!(count_kind(get(&out, "b"), EffectKind::NetFsDb), 1);

        // a and b converge to the SAME summary (one value per SCC): same set of
        // inherited SiteKeys.
        let a_keys: std::collections::BTreeSet<_> = get(&out, "a")
            .effects
            .iter()
            .map(|e| (e.kind.wire(), e.line, e.col))
            .collect();
        let b_keys: std::collections::BTreeSet<_> = get(&out, "b")
            .effects
            .iter()
            .map(|e| (e.kind.wire(), e.line, e.col))
            .collect();
        assert_eq!(a_keys, b_keys);
    }

    #[test]
    fn provenance_is_bounded_on_cycle() {
        // same cycle; the inherited io on `a` carries exactly ONE `via` path,
        // not unbounded — provenance is exemplar.
        let io = make_effect(EffectKind::NetFsDb, false);
        let a = rec_full("a", vec!["b"], vec![], vec![]);
        let b = rec_full("b", vec!["a", "c"], vec![], vec![]);
        let c = rec_full("c", vec![], vec![io], vec![]);

        let g = CallGraph::from_records(vec![a, b, c], base_name_resolver);
        let out = fold(&g);

        let a_io_provenance: Vec<_> = get(&out, "a")
            .inherited
            .iter()
            .filter(|i| i.effect.as_ref().map(|e| e.kind) == Some(EffectKind::NetFsDb))
            .collect();
        // Exactly one inherited record for c's io — one exemplar path, bounded.
        assert_eq!(a_io_provenance.len(), 1);
        assert_eq!(a_io_provenance[0].from, "c");
        assert!(!a_io_provenance[0].via.is_empty());
    }

    #[test]
    fn via_routes_through_folding_member_in_multi_member_scc() {
        // SCC {a, b} (a -> b -> a), b -> c(io). `b` is the folding member (it has
        // the downstream edge to c). For the NON-folding member `a`, the exemplar
        // via must start at `a`, route through `b`, to the origin `c`:
        //   a's via == "a → b → c"   (NOT "b → c", and NOT a doubled "a → a → …")
        // while `b`'s own via starts at `b`: "b → c".
        let io = make_effect(EffectKind::NetFsDb, false);
        let a = rec_full("a", vec!["b"], vec![], vec![]);
        let b = rec_full("b", vec!["a", "c"], vec![], vec![]);
        let c = rec_full("c", vec![], vec![io], vec![]);

        let g = CallGraph::from_records(vec![a, b, c], base_name_resolver);
        let out = fold(&g);

        let a_via = &get(&out, "a")
            .inherited
            .iter()
            .find(|i| i.effect.as_ref().map(|e| e.kind) == Some(EffectKind::NetFsDb))
            .expect("a inherits c's io")
            .via;
        assert_eq!(*a_via, "a → b → c", "non-folding member routes through b");

        let b_via = &get(&out, "b")
            .inherited
            .iter()
            .find(|i| i.effect.as_ref().map(|e| e.kind) == Some(EffectKind::NetFsDb))
            .expect("b inherits c's io")
            .via;
        assert_eq!(*b_via, "b → c", "folding member is not double-prefixed");
    }

    #[test]
    fn summary_keeps_only_escaping() {
        // b has: net.fs.db (not contained => escapes) + local.mutation (contained => stays)
        let escaping = make_effect(EffectKind::NetFsDb, false);
        let contained = make_effect(EffectKind::LocalMutation, true);
        let b = rec_with_effects("b", vec![escaping, contained], vec![]);
        let root = rec_with_effects("root", vec![], vec![]);

        let g =
            CallGraph::from_records(vec![root, b], |r: &CallSiteRef, _owner, nodes| match nodes
                .keys()
                .find(|k| **k == r.base)
            {
                Some(id) => Some(Edge::Resolved(id.clone())),
                None => Some(Edge::Opaque(ExternalReach {
                    specifier: r.base.clone(),
                    kind: ReachKind::ThirdParty,
                    site: "x".into(),
                })),
            });

        let (eff, _risk) = boundary_summary(&"b".into(), &g);
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].kind, EffectKind::NetFsDb);
    }

    #[test]
    fn dashboard_scenario_root_blast_radius() {
        // root(no own effects, is_root=true) -> useStats -> fetchStats(net.fs.db + opaque "analytics-sdk")
        let io = make_effect(EffectKind::NetFsDb, false);
        let fetch_stats = rec_full("fetchStats", vec!["analytics-sdk"], vec![io], vec![]);
        let use_stats = rec_full("useStats", vec!["fetchStats"], vec![], vec![]);
        // root has is_root=true; rec_full uses id=="root" to set is_root, so use that id
        let root = rec_full("root", vec!["useStats"], vec![], vec![]);

        let g = CallGraph::from_records(vec![fetch_stats, use_stats, root], base_name_resolver);
        let folded = fold(&g);
        let hotspots = to_hotspots(&g, &folded);

        let root_h = hotspots
            .iter()
            .find(|h| h.id.contains("root"))
            .expect("root hotspot present");

        assert_eq!(root_h.own_score, 0.0); // looks pure — no own effects
        assert_eq!(root_h.propagated_max_class, 7); // blast radius from fetchStats
        assert!(
            root_h
                .external_reaches
                .iter()
                .any(|r| r.specifier == "analytics-sdk"),
            "root.external_reaches must include analytics-sdk"
        );
        assert!(root_h.root, "root.is_root must propagate to hotspot.root");
        // root inherited net.fs.db and external.unresolved from fetchStats
        assert!(
            !root_h.inherited.is_empty(),
            "root.inherited must be non-empty"
        );
        assert!(
            root_h
                .inherited
                .iter()
                .any(|i| i.kind == EffectKind::NetFsDb.wire()),
            "root.inherited must include net.fs.db"
        );
    }

    #[test]
    fn opaque_edge_becomes_external_reach_and_propagates() {
        // Graph: a -> opaque("analytics-sdk");  root -> a
        // Expected:
        //   out["a"].external_reaches contains "analytics-sdk"
        //   out["root"].external_reaches contains "analytics-sdk" (transitive)
        //   out["root"].effects contains an external.unresolved/class-2 effect
        let a = rec_full("a", vec!["analytics-sdk"], vec![], vec![]);
        let root = rec_full("root", vec!["a"], vec![], vec![]);

        let g = CallGraph::from_records(vec![a, root], base_name_resolver);
        let out = fold(&g);

        // "a" has the opaque edge directly — its reach set must contain "analytics-sdk".
        assert!(
            get(&out, "a")
                .external_reaches
                .iter()
                .any(|r| r.specifier == "analytics-sdk"),
            "a.external_reaches must contain analytics-sdk"
        );

        // "root" calls "a", so it transitively reaches "analytics-sdk".
        assert!(
            get(&out, "root")
                .external_reaches
                .iter()
                .any(|r| r.specifier == "analytics-sdk"),
            "root.external_reaches must contain analytics-sdk transitively"
        );

        // An external.unresolved/class-2 effect must appear in root's propagated effects.
        assert!(
            get(&out, "root").effects.iter().any(|e| {
                e.kind == EffectKind::ExternalUnresolved
                    && e.class == EffectKind::ExternalUnresolved.base_class()
            }),
            "root.effects must contain external.unresolved/class-2"
        );

        // The synthesized effect must carry the canonical weight for class 2,
        // not the wrong hard-coded weight of 1 — this pins the propagated-score arithmetic.
        let expected_weight =
            crate::score::weight_for_class(EffectKind::ExternalUnresolved.base_class());
        assert!(
            get(&out, "root").effects.iter().any(|e| {
                e.kind == EffectKind::ExternalUnresolved && e.weight == expected_weight
            }),
            "synthesized external.unresolved must carry weight == weight_for_class(class 2) == {}",
            expected_weight
        );
    }

    #[test]
    fn apply_fold_sets_propagated_without_touching_own() {
        // Graph: root(no own effects, is_root=true) -> b -> c(net.fs.db/class-7)
        // After apply_fold: root.own_score == 0.0 (untouched),
        //                   root.propagated_max_class == 7 (inherited from c),
        //                   root.inherited is non-empty.
        use crate::fold::apply_fold;
        use crate::model::Hotspot;

        let io = make_effect(EffectKind::NetFsDb, false);
        let root = rec_full("root", vec!["b"], vec![], vec![]);
        let b = rec_full("b", vec!["c"], vec![], vec![]);
        let c = rec_full("c", vec![], vec![io], vec![]);

        let g = CallGraph::from_records(vec![root, b, c], base_name_resolver);
        let folded = fold(&g);

        // Build an own-seeded Hotspot for "root" with own_score 0 / max_class 0.
        let root_id: UnitId = "root".into();
        let mut hs = vec![Hotspot {
            id: root_id.clone(),
            symbol: "root".into(),
            path: "root".into(),
            line: 1,
            ..Hotspot::own_seed(0.0, 0)
        }];

        apply_fold(&mut hs, &g, &folded);

        assert_eq!(hs[0].own_score, 0.0, "own_score must not be touched");
        assert_eq!(
            hs[0].propagated_max_class, 7,
            "propagated_max_class must reflect inherited net.fs.db (class 7)"
        );
        assert!(
            !hs[0].inherited.is_empty(),
            "inherited must be non-empty after apply_fold"
        );
        assert!(hs[0].root, "root flag must be set for the root node");
    }

    // ── Finding 1 tests ─────────────────────────────────────────────────────

    /// The invariant: propagated_max_class >= own_max_class and
    /// propagated_score >= own_score for every unit.
    ///
    /// A unit whose own effects are ALL contained (non-escaping) used to come
    /// out with propagated_max_class == 0 / propagated_score == 0.0 because
    /// `comp_summary` only accumulates ESCAPING own signals.
    #[test]
    fn propagated_gte_own_for_contained_only_unit() {
        // "f" has only a contained local.mutation (class 1, weight 1).
        // No callers, no callees. After the fix:
        //   propagated_max_class == 1 == own_max_class
        //   propagated_score     == 1.0 == own_score
        let contained_local = make_effect(EffectKind::LocalMutation, true);
        let f = rec_with_effects("f", vec![contained_local], vec![]);
        let g = CallGraph::from_records(vec![f], base_name_resolver);
        let out = fold(&g);
        let p = get(&out, "f");

        // own-body stats (computed by to_hotspots / propagated_fields)
        // LocalMutation class == 1, weight == 1 (CLASS_WEIGHTS[1])
        let own_max_class: u8 = 1;
        let own_score = crate::score::own_score(&[1u32]); // == 1.0

        // propagated must be >= own
        let prop_effect_classes: Vec<u8> = p.effects.iter().map(|e| e.effective_class()).collect();
        let prop_risk_class = p.risks.iter().map(|r| r.class).max().unwrap_or(0);
        let propagated_max_class = crate::score::max_class(&prop_effect_classes, prop_risk_class);
        let propagated_score =
            crate::score::own_score(&p.effects.iter().map(|e| e.weight).collect::<Vec<_>>());

        assert!(
            propagated_max_class >= own_max_class,
            "propagated_max_class ({propagated_max_class}) must be >= own_max_class ({own_max_class})"
        );
        assert!(
            propagated_score >= own_score,
            "propagated_score ({propagated_score}) must be >= own_score ({own_score})"
        );
        // Exact values: contained-only unit's propagated == own
        assert_eq!(
            propagated_max_class, 1,
            "propagated_max_class must equal 1 (class of local.mutation)"
        );
        assert_eq!(
            propagated_score, 1.0,
            "propagated_score must equal 1.0 for a single class-1 weight"
        );
    }

    /// Caller of a contained-only callee: caller's OWN contained effects are in
    /// its propagated; the callee contributes NOTHING to caller's propagated
    /// (contained effects do not escape to callers).
    #[test]
    fn caller_of_contained_only_callee_gets_no_callee_effects() {
        // caller has its own contained local.mutation (class 1).
        // callee has a contained local.mutation (class 1).
        // caller.propagated must:
        //   - contain exactly ONE local.mutation (caller's own, not callee's)
        //   - propagated_max_class == 1
        //   - inherited is empty (callee contributes nothing)
        let caller_effect = Effect {
            line: 2,
            col: 2,
            ..make_effect(EffectKind::LocalMutation, true)
        };
        let callee_effect = Effect {
            line: 3,
            col: 3,
            ..make_effect(EffectKind::LocalMutation, true)
        };
        let caller = rec_full("caller", vec!["callee"], vec![caller_effect], vec![]);
        let callee = rec_with_effects("callee", vec![callee_effect], vec![]);

        let g = CallGraph::from_records(vec![caller, callee], base_name_resolver);
        let out = fold(&g);

        let caller_p = get(&out, "caller");
        let callee_p = get(&out, "callee");

        // Callee: exactly one local.mutation in propagated (its own)
        assert_eq!(
            count_kind(callee_p, EffectKind::LocalMutation),
            1,
            "callee propagated must have exactly 1 local.mutation (own)"
        );

        // Caller: exactly one local.mutation (own), NOT two
        assert_eq!(
            count_kind(caller_p, EffectKind::LocalMutation),
            1,
            "caller propagated must have exactly 1 local.mutation (own, callee's doesn't escape)"
        );

        // Caller has no inherited signals (callee escapes nothing)
        assert!(
            caller_p.inherited.is_empty(),
            "caller.inherited must be empty — callee's contained effect does not escape"
        );

        // Caller's propagated_max_class == 1 (its own contained local.mutation)
        let prop_effect_classes: Vec<u8> = caller_p
            .effects
            .iter()
            .map(|e| e.effective_class())
            .collect();
        let prop_risk_class = caller_p.risks.iter().map(|r| r.class).max().unwrap_or(0);
        let propagated_max_class = crate::score::max_class(&prop_effect_classes, prop_risk_class);
        assert_eq!(
            propagated_max_class, 1,
            "caller propagated_max_class must be 1 (own contained effect only)"
        );
    }

    /// Escaping case stays green: a caller of an IO callee still inherits class 7.
    #[test]
    fn caller_of_io_callee_inherits_class7() {
        let io = make_effect(EffectKind::NetFsDb, false); // not contained => escapes
        let caller = rec_full("caller", vec!["io_fn"], vec![], vec![]);
        let io_fn = rec_with_effects("io_fn", vec![io], vec![]);

        let g = CallGraph::from_records(vec![caller, io_fn], base_name_resolver);
        let out = fold(&g);

        let caller_p = get(&out, "caller");
        let prop_effect_classes: Vec<u8> = caller_p
            .effects
            .iter()
            .map(|e| e.effective_class())
            .collect();
        let prop_risk_class = caller_p.risks.iter().map(|r| r.class).max().unwrap_or(0);
        let propagated_max_class = crate::score::max_class(&prop_effect_classes, prop_risk_class);

        assert_eq!(
            propagated_max_class, 7,
            "caller propagated_max_class must be 7 (inherited net.fs.db from io_fn)"
        );
        assert_eq!(
            count_kind(caller_p, EffectKind::NetFsDb),
            1,
            "caller must inherit exactly 1 net.fs.db"
        );
        assert_eq!(
            caller_p.inherited.len(),
            1,
            "caller.inherited must contain exactly one entry"
        );
    }

    /// General invariant: propagated >= own for every unit in a non-trivial graph.
    #[test]
    fn propagated_gte_own_invariant_multi_unit() {
        // Graph with a mix: contained-only, escaping, no-effects.
        // For ALL units, assert propagated_max_class >= own_max_class.
        let contained = make_effect(EffectKind::LocalMutation, true);
        let escaping = make_effect(EffectKind::NetFsDb, false);

        // a: own contained local.mutation, calls b
        let a = rec_full("a", vec!["b"], vec![contained.clone()], vec![]);
        // b: own escaping net.fs.db, calls c
        let b = rec_full("b", vec!["c"], vec![escaping.clone()], vec![]);
        // c: no own effects
        let c = rec_with_effects("c", vec![], vec![]);

        let g = CallGraph::from_records(vec![a, b, c], base_name_resolver);
        let out = fold(&g);

        for unit_id in ["a", "b", "c"] {
            let p = get(&out, unit_id);
            let prop_effect_classes: Vec<u8> =
                p.effects.iter().map(|e| e.effective_class()).collect();
            let prop_risk_class = p.risks.iter().map(|r| r.class).max().unwrap_or(0);
            let propagated_max_class =
                crate::score::max_class(&prop_effect_classes, prop_risk_class);

            // Compute own_max_class from g.nodes
            let record = g.nodes.get(&UnitId::from(unit_id)).unwrap();
            let own_effect_classes: Vec<u8> =
                record.effects.iter().map(|e| e.effective_class()).collect();
            let own_risk_class = record.risks.iter().map(|r| r.class).max().unwrap_or(0);
            let own_max_class = crate::score::max_class(&own_effect_classes, own_risk_class);

            assert!(
                propagated_max_class >= own_max_class,
                "unit '{unit_id}': propagated_max_class ({propagated_max_class}) < own_max_class ({own_max_class})"
            );
        }
    }

    // ── Finding B (Codex-B) tests — same-line duplicate escaping ────────────

    /// A unit whose two same-kind escaping effects land on the SAME line with
    /// col 0 (as emitted by calls.rs in all three frontends) used to produce a
    /// `propagated_score < own_score` because `comp_summary` deduplicated them
    /// to one SiteKey entry while `own_score` counted both.
    ///
    /// After the fix, own signals come DIRECTLY from `g.nodes` (undeduped),
    /// so `propagated_score == own_score` for a leaf unit.
    #[test]
    fn same_line_duplicate_escaping_propagated_score_gte_own_score() {
        // Two net.fs.db effects, both on line 1 col 0 (same SiteKey in old code).
        // Use class-7 weight (21) so the score difference is large and obvious.
        let e1 = Effect {
            kind: EffectKind::NetFsDb,
            class: EffectKind::NetFsDb.base_class(),
            discounted_to: None,
            weight: crate::score::weight_for_class(EffectKind::NetFsDb.base_class()),
            line: 5,
            col: 0, // col 0 — the detector-emitted "no column" value
            tier: Tier::Exact,
            hidden: false,
            contained: false, // escaping
            evidence: "std::fs::write_a".into(),
            discount: None,
            subreason: None,
            confidence: 1.0,
        };
        let e2 = Effect {
            kind: EffectKind::NetFsDb,
            class: EffectKind::NetFsDb.base_class(),
            discounted_to: None,
            weight: crate::score::weight_for_class(EffectKind::NetFsDb.base_class()),
            line: 5, // SAME line as e1
            col: 0,  // SAME col  as e1 → old SiteKey collapse
            tier: Tier::Exact,
            hidden: false,
            contained: false, // escaping
            evidence: "std::fs::write_b".into(),
            discount: None,
            subreason: None,
            confidence: 1.0,
        };

        // Leaf unit — no callees. own_score counts both weights.
        // Two weights of 21: max=21, rest=21, own_score = 21 + 0.5*21 = 31.5
        // (Before the fix: propagated only had ONE net.fs.db → propagated_score < own_score)
        let f = rec_with_effects("f", vec![e1, e2], vec![]);
        let g = CallGraph::from_records(vec![f], base_name_resolver);
        let out = fold(&g);
        let p = get(&out, "f");

        // Compute own_score from g.nodes directly (ground truth).
        let record = g.nodes.get(&UnitId::from("f")).unwrap();
        let own_weights: Vec<u32> = record.effects.iter().map(|e| e.weight).collect();
        let own_score = crate::score::own_score(&own_weights);

        // Compute propagated_score from the fold output.
        let propagated_score =
            crate::score::own_score(&p.effects.iter().map(|e| e.weight).collect::<Vec<_>>());

        assert!(
            propagated_score >= own_score,
            "propagated_score ({propagated_score}) must be >= own_score ({own_score}) \
             — same-line col-0 duplicate escaping effects must not be deduped for own"
        );

        // For a leaf (no callees), propagated == own exactly.
        assert_eq!(
            propagated_score, own_score,
            "leaf unit: propagated_score must equal own_score exactly (no inherited signals)"
        );

        // Both net.fs.db effects must be present (count == 2).
        assert_eq!(
            count_kind(p, EffectKind::NetFsDb),
            2,
            "both same-line escaping net.fs.db effects must be in propagated"
        );

        // No inherited signals (leaf unit).
        assert!(
            p.inherited.is_empty(),
            "leaf unit must have no inherited signals"
        );
    }

    /// Diamond-dedup is about INHERITED signals (from != unit), not own.
    /// After the Codex-B fix the diamond still counts once because the dedup
    /// applies only to the `comp_summary` (inherited cross-path signals).
    ///
    /// This is the existing `transitive_io_reaches_root_diamond_counts_once`
    /// invariant re-verified with an explicit score assertion.
    #[test]
    fn diamond_inherited_still_counts_once_after_codex_b_fix() {
        // root -> a -> d(io);  root -> b -> d(io)  (diamond)
        // d's io is inherited by root via TWO paths but must appear ONCE.
        let io = Effect {
            kind: EffectKind::NetFsDb,
            class: EffectKind::NetFsDb.base_class(),
            discounted_to: None,
            weight: crate::score::weight_for_class(EffectKind::NetFsDb.base_class()),
            line: 10,
            col: 0, // col 0 — still deduped for inherited (different unit)
            tier: Tier::Exact,
            hidden: false,
            contained: false,
            evidence: "io".into(),
            discount: None,
            subreason: None,
            confidence: 1.0,
        };
        let root = rec_full("root", vec!["a", "b"], vec![], vec![]);
        let a = rec_full("a", vec!["d"], vec![], vec![]);
        let b = rec_full("b", vec!["d"], vec![], vec![]);
        let d = rec_with_effects("d", vec![io], vec![]);

        let g = CallGraph::from_records(vec![root, a, b, d], base_name_resolver);
        let out = fold(&g);

        // root inherits d's io via both paths — dedup keeps exactly ONE.
        assert_eq!(
            count_kind(get(&out, "root"), EffectKind::NetFsDb),
            1,
            "diamond: root must inherit d's io exactly once (inherited dedup preserved)"
        );

        // root's inherited list carries exactly one entry for that site.
        let root_inh_count = get(&out, "root")
            .inherited
            .iter()
            .filter(|i| i.effect.as_ref().map(|e| e.kind) == Some(EffectKind::NetFsDb))
            .count();
        assert_eq!(
            root_inh_count, 1,
            "diamond: exactly one inherited net.fs.db entry"
        );
    }

    /// `propagated_score >= own_score` invariant in a graph that INCLUDES a unit
    /// with same-line duplicate escaping effects.
    #[test]
    fn propagated_gte_own_invariant_includes_same_line_dup_unit() {
        // "f" has two same-line col-0 escaping net.fs.db effects.
        // "caller" calls "f" and adds a contained own effect.
        // For BOTH units: propagated_score >= own_score.
        let dup_e1 = Effect {
            kind: EffectKind::NetFsDb,
            class: EffectKind::NetFsDb.base_class(),
            discounted_to: None,
            weight: crate::score::weight_for_class(EffectKind::NetFsDb.base_class()),
            line: 3,
            col: 0,
            tier: Tier::Exact,
            hidden: false,
            contained: false,
            evidence: "write1".into(),
            discount: None,
            subreason: None,
            confidence: 1.0,
        };
        let dup_e2 = Effect {
            line: 3, // same line, same col
            evidence: "write2".into(),
            ..dup_e1.clone()
        };
        let caller_own = make_effect(EffectKind::LocalMutation, true);

        let f = rec_with_effects("f", vec![dup_e1, dup_e2], vec![]);
        let caller = rec_full("caller", vec!["f"], vec![caller_own], vec![]);

        let g = CallGraph::from_records(vec![f, caller], base_name_resolver);
        let out = fold(&g);

        for unit_id in ["f", "caller"] {
            let p = get(&out, unit_id);
            let propagated_score =
                crate::score::own_score(&p.effects.iter().map(|e| e.weight).collect::<Vec<_>>());

            let record = g.nodes.get(&UnitId::from(unit_id)).unwrap();
            let own_weights: Vec<u32> = record.effects.iter().map(|e| e.weight).collect();
            let own_score = crate::score::own_score(&own_weights);

            assert!(
                propagated_score >= own_score,
                "unit '{unit_id}': propagated_score ({propagated_score}) < own_score ({own_score})"
            );
        }
    }

    // ── Finding 4 tests ─────────────────────────────────────────────────────

    /// A unit with TWO opaque calls to the same specifier at distinct sites must
    /// produce TWO `external.unresolved` effects in its `Propagated` (not one).
    ///
    /// Before the fix, `synthesize_opaque_effect` used line:0/col:0 for the
    /// SiteKey regardless of the call site, so both entries shared the same key
    /// and the second was dropped by `acc.entry(key).or_insert`.
    #[test]
    fn two_opaque_calls_same_specifier_distinct_sites_count_separately() {
        use crate::record::{CallSiteRef, ReachKind, RefKind, UnitRecord};

        // "f" has two opaque calls to "ext-pkg" at different call sites.
        // We build the UnitRecord manually with two refs that will resolve to
        // opaque reaches because "ext-pkg" is not in the node set.
        let f = UnitRecord {
            unit_id: "f".into(),
            path: "f.rs".into(),
            line: 1,
            col: 1,
            symbol: "f".into(),
            is_root: false,
            canonical_path: vec![],
            aliases: vec![],
            effects: vec![],
            risks: vec![],
            refs: vec![
                CallSiteRef {
                    kind: RefKind::Free,
                    base: "ext-pkg".into(),
                    module: None,
                    line: 10,
                    col: 5,
                    qualified: true,
                    first_party: false,
                    resolved_target: None,
                },
                CallSiteRef {
                    kind: RefKind::Free,
                    base: "ext-pkg".into(),
                    module: None,
                    line: 20,
                    col: 5,
                    qualified: true,
                    first_party: false,
                    resolved_target: None,
                },
            ],
            async_boundary: false,
            await_count: 0,
            language: crate::frontend::Language::Rust,
        };

        // A resolver that always returns opaque, using line/col for the site string.
        let g = CallGraph::from_records(vec![f], |r: &CallSiteRef, _owner, _nodes| {
            Some(Edge::Opaque(ExternalReach {
                specifier: r.base.clone(),
                kind: ReachKind::ThirdParty,
                site: format!("f.rs:{}:{}", r.line, r.col),
            }))
        });

        let out = fold(&g);
        let f_prop = get(&out, "f");

        let ext_unresolved_count = f_prop
            .effects
            .iter()
            .filter(|e| e.kind == EffectKind::ExternalUnresolved)
            .count();

        assert_eq!(
            ext_unresolved_count, 2,
            "two opaque calls to the same specifier at distinct sites must produce 2 \
             external.unresolved effects, got {ext_unresolved_count}"
        );
    }
}
