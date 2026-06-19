use crate::effect::Tier;

pub fn tier_base(t: Tier) -> f64 {
    match t {
        Tier::Exact => 1.0,
        Tier::Path => 0.9,
        Tier::Heuristic => 0.6,
    }
}

/// Per-detection confidence: tier base x penalties (unresolved call, shadowed path).
pub fn detection_confidence(t: Tier, unresolved_call: bool, shadowed_path: bool) -> f64 {
    let mut c = tier_base(t);
    if unresolved_call {
        c *= 0.8;
    }
    if shadowed_path {
        c *= 0.9;
    }
    c
}

/// Function confidence = min over effect/evidence confidences; 1.0 if none.
pub fn function_confidence(detections: &[f64]) -> f64 {
    detections.iter().copied().fold(1.0, f64::min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::Tier;

    #[test]
    fn tier_bases_and_penalties() {
        assert_eq!(tier_base(Tier::Exact), 1.0);
        assert_eq!(tier_base(Tier::Path), 0.9);
        assert_eq!(tier_base(Tier::Heuristic), 0.6);
        assert!((detection_confidence(Tier::Path, true, false) - 0.72).abs() < 1e-9); // x0.8
        assert!((detection_confidence(Tier::Path, false, true) - 0.81).abs() < 1e-9); // x0.9
    }

    #[test]
    fn function_confidence_is_min() {
        assert_eq!(function_confidence(&[1.0, 0.6, 0.9]), 0.6);
        assert_eq!(function_confidence(&[]), 1.0);
    }
}
