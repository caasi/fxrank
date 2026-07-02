//! Effect detectors for the Shell frontend.
//!
//! Each submodule classifies one effect family over the shared [`crate::walk::walk`] /
//! [`crate::walk::walk_commands`] descent (Task 2) — no detector re-implements the
//! traversal.

pub mod calls;
pub mod mutation;
pub mod refs;
pub mod risk;

use std::collections::HashSet;

use fxrank_core::confidence::function_confidence;
use fxrank_core::effect::Effect;
use fxrank_core::frontend::Language;
use fxrank_core::model::Hotspot;
use fxrank_core::record::UnitRecord;
use fxrank_core::score::{max_class, own_score, weight_for_class};

use crate::functions::FnUnit;

/// Run every detector over `unit` and gather the own-body `(effects, risks)`.
///
/// Both [`analyze_unit`] and [`build_record`] call this helper so the two callers
/// can never silently diverge — any future detector addition touches one place.
/// Mirrors the private `gather` in `fxrank-lang-rust`/`fxrank-lang-python`.
///
/// Shell has no boundary-typed signature (`BoundaryCoverage::None` always), so
/// `mutation::detect`'s no-op discount already ran — this only wires the tuple's
/// `contained` bool onto `Effect.contained` (needed so cross-file propagation sees
/// the real containment, not the `false` default) without applying any discount.
fn gather(
    unit: &FnUnit,
    fns: &HashSet<String>,
    top: &HashSet<String>,
) -> (Vec<Effect>, Vec<fxrank_core::effect::RiskFeature>) {
    let mut effects: Vec<Effect> = calls::detect(unit, fns);
    effects.extend(
        mutation::detect(unit, top)
            .into_iter()
            .map(|(mut e, contained)| {
                e.contained = contained;
                e
            }),
    );
    let risks = risk::detect(unit, &unit.path);
    (effects, risks)
}

/// Run every detector over `unit` and assemble a scored [`Hotspot`].
pub fn analyze_unit(unit: &FnUnit, fns: &HashSet<String>, top: &HashSet<String>) -> Hotspot {
    let (effects, risks) = gather(unit, fns, top);

    let weights: Vec<u32> = effects.iter().map(|e| e.weight).collect();
    let classes: Vec<u8> = effects.iter().map(|e| e.effective_class()).collect();
    let confidences: Vec<f64> = effects.iter().map(|e| e.confidence).collect();

    let risk_class = risks.iter().map(|r| r.class).max().unwrap_or(0);
    let risk_weight = if risks.is_empty() {
        0
    } else {
        weight_for_class(risk_class)
    };

    let mc = max_class(&classes, risk_class);
    let os = own_score(&weights);
    Hotspot {
        id: unit.id.clone(),
        symbol: unit.symbol.clone(),
        path: unit.path.clone(),
        line: unit.line,
        risk_weight,
        confidence: function_confidence(&confidences),
        effects,
        risk_features: risks,
        // Propagated fields default to own (cross-file folding overwrites them).
        ..Hotspot::own_seed(os, mc)
    }
}

/// Build a language-neutral [`UnitRecord`] for `unit`.
///
/// The record carries the same own-body `effects`/`risks` as the [`analyze_unit`]
/// Hotspot (same `gather` call), plus outgoing call references from [`refs::refs`].
/// No reach side channel: `external_reaches` are produced downstream by the CLI's
/// cross-file fold from the opaque qualified `source` refs this puts in `refs`.
///
/// INVARIANT: this recomputes own-body via the same `gather` as `analyze_unit`. This
/// stays correct only while `analyze_unit` does no post-`gather` mutation of
/// effects/risks. If a future step adds one, switch to copying from the Hotspot or the
/// record's own-body will drift from it.
pub fn build_record(unit: &FnUnit, fns: &HashSet<String>, top: &HashSet<String>) -> UnitRecord {
    let (effects, risks) = gather(unit, fns, top);

    UnitRecord {
        unit_id: unit.id.clone(),
        path: unit.path.clone(),
        line: unit.line,
        col: unit.col,
        symbol: unit.symbol.clone(),
        is_root: false,
        canonical_path: refs::canonical_path(unit),
        aliases: vec![],
        effects,
        risks,
        refs: refs::refs(unit, fns),
        async_boundary: false,
        await_count: 0,
        language: Language::Shell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bindings::script_top_names,
        functions::{collect, defined_function_names},
        parse,
    };

    #[test]
    fn sudo_rm_outranks_bare_rm_on_risk_weight() {
        let one = analyze("f(){ rm -rf /x; }\n", "f");
        let two = analyze("f(){ sudo rm -rf /x; }\n", "f");
        assert_eq!((one.max_class, two.max_class), (7, 7));
        assert_eq!(one.risk_weight, 8); // weight_for_class(5)
        assert_eq!(two.risk_weight, 13); // weight_for_class(6) — PrivilegeEscalation wins
        assert!(two.risk_weight > one.risk_weight);
    }

    #[test]
    fn subshell_contained_mutation_carries_through_to_the_hotspot_effect() {
        // A variable write inside a subshell `( … )` is contained (the write can't
        // escape the subshell), per Task 8's subshell-containment model.
        let hot = analyze("f(){ (x=1); }\n", "f");
        let mutation = hot
            .effects
            .iter()
            .find(|e| {
                matches!(
                    e.kind.wire(),
                    "local.mutation" | "hidden.mutation" | "global.mutation"
                )
            })
            .unwrap_or_else(|| panic!("expected a mutation effect, got: {:?}", hot.effects));
        assert!(
            mutation.contained,
            "subshell-contained write must carry contained=true, got: {mutation:?}"
        );
    }

    fn analyze(src: &str, sym: &str) -> Hotspot {
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == sym)
            .unwrap();
        analyze_unit(&unit, &fns, &top)
    }
}
