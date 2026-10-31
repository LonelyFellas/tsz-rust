//! harmonic_v1; mirrors @tsz/shared harmonicProbabilities after eligibility filtering.
pub const POLICY: &str = "harmonic_v1";
pub fn probabilities(count: usize) -> Vec<f64> {
    let total: f64 = (1..=count).map(|rank| 1.0 / rank as f64).sum();
    (1..=count).map(|rank| 1.0 / rank as f64 / total).collect()
}
pub fn select(count: usize, occurrence: i64, random: f64) -> Option<usize> {
    if count == 0 {
        return None;
    }
    if occurrence <= 1 {
        return Some(0);
    }
    let mut cumulative = 0.0;
    for (index, probability) in probabilities(count).iter().enumerate() {
        cumulative += probability;
        if random < cumulative {
            return Some(index);
        }
    }
    Some(count - 1)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_ranks_and_fixed_random_boundaries() {
        assert_eq!(select(0, 2, 0.0), None);
        assert_eq!(select(5, 1, 0.999), Some(0));
        let p = probabilities(5);
        assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!((p[0] - 60.0 / 137.0).abs() < 1e-12);
        let boundary = probabilities(2)[0];
        assert_eq!(select(2, 2, boundary - 1e-10), Some(0));
        assert_eq!(select(2, 2, boundary), Some(1));
        assert_eq!(select(2, 2, 1.0 - f64::EPSILON), Some(1));
    }
}
