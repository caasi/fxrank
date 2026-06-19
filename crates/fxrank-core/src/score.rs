pub const CLASS_WEIGHTS: [u32; 9] = [0, 1, 2, 3, 5, 8, 13, 21, 34];

pub fn weight_for_class(class: u8) -> u32 {
    CLASS_WEIGHTS[(class as usize).min(8)]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discount {
    None,
    MutParam,
    MutSelf,
}

pub fn rank_key(
    max_class: u8,
    own_score: f64,
    risk_weight: u32,
    confidence: f64,
) -> (u8, u64, u32, u32) {
    (
        max_class,
        (own_score * 2.0).round() as u64,
        risk_weight,
        (confidence * 100.0).round() as u32,
    )
}

pub fn own_score(weights: &[u32]) -> f64 {
    let max = weights.iter().copied().max().unwrap_or(0);
    let rest: u32 = weights.iter().copied().sum::<u32>().saturating_sub(max);
    max as f64 + 0.5 * rest as f64
}

pub fn max_class(effect_classes: &[u8], risk_class: u8) -> u8 {
    effect_classes
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(risk_class)
}

pub fn apply_discount(base_class: u8, discount: Discount, unsafe_enclosed: bool) -> u8 {
    if unsafe_enclosed || discount == Discount::None {
        return base_class;
    }
    let shift = match discount {
        Discount::MutParam => 2,
        Discount::MutSelf => 1,
        Discount::None => 0,
    };
    base_class.saturating_sub(shift).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_key_orders_by_max_class_first() {
        let soup = rank_key(4, 27.5, 0, 0.9); // logging soup
        let io = rank_key(7, 21.0, 0, 0.9); // one IO
        assert!(io > soup);
    }

    #[test]
    fn risk_only_outranks_class_zero() {
        let risk_only = rank_key(4, 0.0, 5, 1.0); // mem::forget => risk_class 4
        let pure = rank_key(0, 0.0, 0, 1.0);
        assert!(risk_only > pure);
    }

    #[test]
    fn own_score_damps_non_max_weights() {
        assert_eq!(own_score(&[21, 8, 1]), 25.5); // 21 + 0.5*(8+1)
        assert_eq!(own_score(&[]), 0.0);
        assert_eq!(own_score(&[5]), 5.0);
    }

    #[test]
    fn max_class_includes_risk() {
        assert_eq!(max_class(&[0], 4), 4);
        assert_eq!(max_class(&[7], 0), 7);
    }

    #[test]
    fn discounts_shift_classes_and_clamp() {
        assert_eq!(apply_discount(3, Discount::MutParam, false), 1); // &mut param: down 2
        assert_eq!(apply_discount(3, Discount::MutSelf, false), 2); // &mut self: down 1
        assert_eq!(apply_discount(1, Discount::MutParam, false), 1); // floor: never below 1
        assert_eq!(apply_discount(3, Discount::MutParam, true), 3); // unsafe-enclosed: cancelled
        assert_eq!(apply_discount(3, Discount::None, false), 3); // no channel: unchanged
    }

    #[test]
    fn weight_map_is_fibonacci() {
        let expected = [0, 1, 2, 3, 5, 8, 13, 21, 34];
        for (class, w) in expected.iter().enumerate() {
            assert_eq!(weight_for_class(class as u8), *w);
        }
    }
}
