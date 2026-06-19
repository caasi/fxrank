//! Effect detection for the TypeScript frontend.
//!
//! This module owns turning a [`FnUnit`] into a scored [`Hotspot`], mirroring
//! `fxrank-lang-rust`'s `detect::analyze_unit`. Today it is a walking-skeleton
//! stub: it emits a zero-effect `Hotspot` so the end-to-end CLI path works.

use fxrank_core::model::Hotspot;

use crate::functions::FnUnit;

pub mod calls;

/// Build a zero-effect [`Hotspot`] from a [`FnUnit`].
///
/// TODO(Task 7): wire the real gather (call/macro/mutation/risk detectors) and
/// fold (own_score, max_class, risk_weight, confidence) steps here.
pub fn analyze_unit(unit: &FnUnit) -> Hotspot {
    Hotspot {
        id: unit.id.clone(),
        symbol: unit.symbol.clone(),
        path: unit.path.clone(),
        line: unit.line,
        max_class: 0,
        own_score: 0.0,
        risk_weight: 0,
        confidence: 1.0,
        async_boundary: unit.is_async,
        await_count: 0,
        effects: Vec::new(),
        risk_features: Vec::new(),
    }
}
