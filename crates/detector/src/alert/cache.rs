use moka::sync::Cache;
use std::time::Duration;

use super::result::MotifResult;

pub struct AlertCache {
    inner: Cache<u64, MotifResult>,
}

impl AlertCache {
    pub fn new(max_capacity: u64, ttl_secs: u64) -> Self {
        let cache = Cache::builder()
            .max_capacity(max_capacity)
            .time_to_live(Duration::from_secs(ttl_secs))
            .build();

        Self { inner: cache }
    }

    pub fn insert(&self, hash: u64, result: MotifResult) {
        self.inner.insert(hash, result);
    }

    pub fn get(&self, hash: u64) -> Option<MotifResult> {
        self.inner.get(&hash)
    }

    pub fn contains(&self, hash: u64) -> bool {
        self.inner.contains_key(&hash)
    }

    pub fn len(&self) -> u64 {
        self.inner.entry_count()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.entry_count() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alert::result::{MotifType, ScoreBreakdown};
    use chrono::Utc;

    fn make_result(hash: u64) -> MotifResult {
        MotifResult {
            motif_type: MotifType::PhoenixLoop,
            unique_hash: hash,
            participants: vec![],
            risk_score: 0.99,
            evidence_snapshot: "{}".to_string(),
            detected_at_event: 1,
            version: 1,
            last_seen_at: Utc::now(),
            score_breakdown: ScoreBreakdown {
                s_phoenix: 0.0,
                s_proximity: 0.0,
                s_clustering: 0.0,
                p_base: 0.0,
                m_env: 0.0,
                p_final: 0.0,
                imd_rank: None,
                ward_code: None,
            },
            withdrawn_at: None,
            withdrawn_reason: None,
        }
    }

    #[test]
    fn test_alert_cache_eviction() {
        // max capacity 2
        let cache = AlertCache::new(2, 60);
        cache.insert(1, make_result(1));
        cache.insert(2, make_result(2));
        cache.insert(3, make_result(3)); // should evict one

        cache.inner.run_pending_tasks();

        assert!(cache.len() <= 2);
    }

    #[test]
    fn test_alert_cache_is_empty_on_new_cache() {
        let cache = AlertCache::new(100, 60);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_alert_cache_get_returns_inserted_item() {
        let cache = AlertCache::new(100, 60);
        cache.insert(42, make_result(42));
        let got = cache.get(42);
        assert!(got.is_some());
        assert_eq!(got.unwrap().unique_hash, 42);
    }

    #[test]
    fn test_alert_cache_get_returns_none_for_missing_key() {
        let cache = AlertCache::new(100, 60);
        assert!(cache.get(999).is_none());
    }

    #[test]
    fn test_alert_cache_contains_after_insert() {
        let cache = AlertCache::new(100, 60);
        assert!(!cache.contains(7));
        cache.insert(7, make_result(7));
        assert!(cache.contains(7));
    }

    #[test]
    fn test_alert_cache_len_increases_with_inserts() {
        let cache = AlertCache::new(100, 60);
        assert!(cache.is_empty());
        cache.insert(1, make_result(1));
        cache.insert(2, make_result(2));
        cache.inner.run_pending_tasks();
        assert_eq!(cache.len(), 2);
        assert!(!cache.is_empty());
    }
}
