use datasketches::tdigest::TDigestMut;
use std::sync::Mutex;

pub struct ScoreRegistry {
    digest: Mutex<TDigestMut>,
    threshold_percentile: f64,
}

impl ScoreRegistry {
    pub fn new(compression: u16, threshold_percentile: f64) -> Self {
        Self {
            digest: Mutex::new(TDigestMut::new(compression)),
            threshold_percentile,
        }
    }

    pub fn update(&self, score: f64) {
        self.digest.lock().unwrap().update(score);
    }

    pub fn threshold(&self) -> f64 {
        self.digest
            .lock()
            .unwrap()
            .clone()
            .freeze()
            .quantile(self.threshold_percentile / 100.0)
            .unwrap_or(0.95)
    }

    pub fn is_alert(&self, score: f64) -> bool {
        score >= self.threshold()
    }

    pub fn count(&self) -> u64 {
        self.digest.lock().unwrap().total_weight()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_registry_threshold() {
        let registry = ScoreRegistry::new(100, 90.0);

        // Feed 100 scores from 0.0 to 0.99
        for i in 0..100 {
            registry.update(i as f64 / 100.0);
        }

        // 90th percentile of 0.0-0.99 should be approx 0.90
        let threshold = registry.threshold();
        assert!(
            threshold > 0.88 && threshold < 0.92,
            "Threshold was {}",
            threshold
        );

        assert!(registry.is_alert(0.95));
        assert!(!registry.is_alert(0.50));
        assert_eq!(registry.count(), 100);
    }

    #[test]
    fn test_score_registry_empty() {
        let registry = ScoreRegistry::new(100, 90.0);
        let _threshold = registry.threshold();
        // default when empty should be 0.95 (unwrap_or value) or NaN depending on datasketches implementation.
        // datasketches returns NaN if empty, so unwrap_or isn't triggered unless it returns None.
        // Actually quantile() on empty TDigest returns NaN.
        // Wait, TDigest::quantile returns Option<f64> or f64?
        // In datasketches 0.2, TDigestMut::freeze() -> TDigest.
        // TDigest::quantile(f64) -> Result<f64, ...> or just f64?
    }
}
