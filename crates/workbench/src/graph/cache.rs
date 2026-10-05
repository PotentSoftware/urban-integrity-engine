//! Plan 44-07: result cache keyed by population timestamp + algorithm + params + seed +
//! edge-type selection, D-05.
//!
//! Mirrors `crate::boot::WorkbenchState.report_cache`'s `RwLock<Option<CachedReport>>`
//! replace-not-merge discipline and `crate::analysis::run_handler`'s exact lock-acquire/
//! replace shape (same `.expect("... lock poisoned")` wording), generalised from a single slot
//! to a keyed `HashMap`: a different `CacheKey` is a different, independently-replaceable
//! cache entry.

use std::collections::HashMap;
use std::sync::RwLock;

use urban_integrity_workbench_dto::AlgorithmRequest;
use urban_integrity_workbench_dto::AlgorithmResult;

/// Every field of an `AlgorithmRequest` that can change an algorithm's output, plus the
/// population's generation timestamp (D-05, GRA-04) — the full cache key. `resolution` (an
/// `f64`, D-10's Louvain resolution) is stored as its `to_bits()` representation so the whole
/// struct can derive `Eq`/`Hash` (`f64` implements neither directly).
///
/// `edge_types_included` is stored SORTED (see [`CacheKey::from_request`]) so that two requests
/// differing only in the internal `Vec` order of an otherwise-identical edge-type checklist
/// still hash/compare equal — a checklist re-rendered in a different order must still hit the
/// same cache entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    algorithm: String,
    population_generated_at: String,
    edge_types_included: Vec<String>,
    seed: u64,
    from_id: Option<String>,
    to_id: Option<String>,
    ward_id: Option<String>,
    /// `highlight_set`'s requested ids, sorted — two links naming the same entities in a
    /// different order are the same query and must share a cache entry.
    ids: Vec<String>,
    /// The finding-detail-route scope narrowing (plan 50-11), sorted and deduped — a scoped run
    /// and an unscoped run over the same otherwise-identical request must never share a cache
    /// entry (they compute over different node sets and would silently return the wrong one's
    /// result to the other), and two scoped runs naming the same subset in a different order must
    /// still hit the same entry, mirroring `ids`'/`edge_types_included`'s own order-independence.
    scope_ids: Vec<String>,
    hop_cap: u32,
    result_cap: u32,
    budget_secs: u32,
    k: u32,
    top_n: u32,
    min_community_size: u32,
    resolution_bits: u64,
}

impl CacheKey {
    /// Builds a `CacheKey` from an `AlgorithmRequest` and the population's generation timestamp
    /// (never taken from the request itself — the caller reads it from `WorkbenchState`'s own
    /// population run, per this crate's "server-side truth for counts" convention).
    ///
    /// `req.edge_types_included` is cloned and sorted before being stored, so ordering never
    /// affects key equality (this task's normalisation requirement).
    pub fn from_request(req: &AlgorithmRequest, population_generated_at: &str) -> Self {
        let mut edge_types_included = req.edge_types_included.clone();
        edge_types_included.sort();

        Self {
            algorithm: req.algorithm.clone(),
            population_generated_at: population_generated_at.to_string(),
            edge_types_included,
            seed: req.seed,
            from_id: req.from_id.clone(),
            to_id: req.to_id.clone(),
            ward_id: req.ward_id.clone(),
            ids: {
                let mut ids = req.ids.clone();
                ids.sort();
                ids
            },
            scope_ids: {
                let mut scope_ids = req.scope_ids.clone();
                scope_ids.sort();
                scope_ids.dedup();
                scope_ids
            },
            hop_cap: req.hop_cap,
            result_cap: req.result_cap,
            budget_secs: req.budget_secs,
            k: req.k,
            top_n: req.top_n,
            min_community_size: req.min_community_size,
            resolution_bits: req.resolution.to_bits(),
        }
    }
}

/// The `POST /api/graph/algorithm` result cache (D-05, GRA-04): a keyed generalisation of
/// `WorkbenchState.report_cache`'s single-slot `RwLock<Option<T>>` pattern. A `put` with an
/// already-present key REPLACES the entry (`HashMap::insert`'s natural behaviour) — never
/// merges — mirroring `report_cache`'s replace-not-merge discipline per-key instead of globally.
pub struct AlgorithmCache {
    entries: RwLock<HashMap<CacheKey, AlgorithmResult>>,
}

impl AlgorithmCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// The cached result for `key`, if any — cloned out from behind the lock so the guard is
    /// dropped before the caller uses the value (mirrors `report_cache`'s read-path shape).
    pub fn get(&self, key: &CacheKey) -> Option<AlgorithmResult> {
        self.entries
            .read()
            .expect("cache lock poisoned")
            .get(key)
            .cloned()
    }

    /// Inserts or replaces the entry for `key`. A second `put` with the same key replaces the
    /// first value; it never merges the two.
    pub fn put(&self, key: CacheKey, value: AlgorithmResult) {
        self.entries
            .write()
            .expect("cache lock poisoned")
            .insert(key, value);
    }
}

impl Default for AlgorithmCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::{AlgorithmCaps, AlgorithmOutcome, Truncation};

    fn sample_request() -> AlgorithmRequest {
        AlgorithmRequest {
            algorithm: "all_simple_paths".to_string(),
            session_id: "session-abc".to_string(),
            edge_types_included: vec!["Directs".to_string(), "AwardedTo".to_string()],
            seed: 42,
            from_id: Some("director:jane-smith".to_string()),
            to_id: Some("GB-COH:99000001".to_string()),
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        }
    }

    fn sample_result() -> AlgorithmResult {
        AlgorithmResult {
            outcome: AlgorithmOutcome::Paths { paths: vec![] },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(4),
                result_cap: Some(100),
                budget_secs: Some(5),
                k: None,
                top_n: None,
                min_community_size: None,
            },
            truncation: Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            wall_clock_ms: 12,
        }
    }

    /// Two `CacheKey`s built from the same request and the same population timestamp are equal.
    #[test]
    fn same_request_and_timestamp_produce_equal_keys() {
        let req = sample_request();
        let a = CacheKey::from_request(&req, "2026-01-01T10:30:00Z");
        let b = CacheKey::from_request(&req, "2026-01-01T10:30:00Z");
        assert_eq!(a, b);
    }

    /// Changing any single field (algorithm, seed, hop_cap, edge_types_included,
    /// population_generated_at) produces a different key.
    /// Two links naming the same entities in a different order are the same query, so they must
    /// share a cache entry — `ids` is sorted into the key for the same reason
    /// `edge_types_included` is.
    #[test]
    fn reordering_ids_produces_the_same_key() {
        let mut one = sample_request();
        one.ids = vec!["b".to_string(), "a".to_string()];
        let mut other = sample_request();
        other.ids = vec!["a".to_string(), "b".to_string()];

        assert_eq!(
            CacheKey::from_request(&one, "2026-01-01T10:30:00Z"),
            CacheKey::from_request(&other, "2026-01-01T10:30:00Z")
        );
    }

    #[test]
    fn changing_any_field_produces_a_different_key() {
        let req = sample_request();
        let base = CacheKey::from_request(&req, "2026-01-01T10:30:00Z");

        let mut different_algorithm = req.clone();
        different_algorithm.algorithm = "shortest_paths".to_string();
        assert_ne!(
            base,
            CacheKey::from_request(&different_algorithm, "2026-01-01T10:30:00Z")
        );

        let mut different_seed = req.clone();
        different_seed.seed = 43;
        assert_ne!(
            base,
            CacheKey::from_request(&different_seed, "2026-01-01T10:30:00Z")
        );

        let mut different_hop_cap = req.clone();
        different_hop_cap.hop_cap = 6;
        assert_ne!(
            base,
            CacheKey::from_request(&different_hop_cap, "2026-01-01T10:30:00Z")
        );

        let mut different_edge_types = req.clone();
        different_edge_types.edge_types_included = vec!["Controls".to_string()];
        assert_ne!(
            base,
            CacheKey::from_request(&different_edge_types, "2026-01-01T10:30:00Z")
        );

        assert_ne!(base, CacheKey::from_request(&req, "2026-02-01T10:30:00Z"));
        let mut different_ids = req.clone();
        different_ids.ids = vec!["a".to_string()];
        assert_ne!(
            base,
            CacheKey::from_request(&different_ids, "2026-01-01T10:30:00Z"),
            "a deep link's id set must be part of the key, or two different links share a result"
        );

        let mut different_scope_ids = req.clone();
        different_scope_ids.scope_ids = vec!["n0".to_string()];
        assert_ne!(
            base,
            CacheKey::from_request(&different_scope_ids, "2026-01-01T10:30:00Z"),
            "a scoped run must never share a cache entry with the unscoped (whole-population) run"
        );
    }

    /// Two `scope_ids` naming the same subset in a different `Vec` order, or with a duplicate,
    /// still produce the same key — mirroring `edge_types_included`'s own order-independence.
    #[test]
    fn scope_ids_order_and_duplicates_do_not_affect_key_equality() {
        let mut req_a = sample_request();
        req_a.scope_ids = vec!["n1".to_string(), "n0".to_string()];
        let mut req_b = sample_request();
        req_b.scope_ids = vec!["n0".to_string(), "n1".to_string(), "n0".to_string()];

        let key_a = CacheKey::from_request(&req_a, "2026-01-01T10:30:00Z");
        let key_b = CacheKey::from_request(&req_b, "2026-01-01T10:30:00Z");
        assert_eq!(key_a, key_b);
    }

    /// `edge_types_included` order does not matter for cache-key equality — a checklist
    /// re-rendered with a different internal `Vec` order must still hit the same cache entry.
    #[test]
    fn edge_type_order_does_not_affect_key_equality() {
        let mut req_a = sample_request();
        req_a.edge_types_included = vec!["Directs".to_string(), "AwardedTo".to_string()];
        let mut req_b = sample_request();
        req_b.edge_types_included = vec!["AwardedTo".to_string(), "Directs".to_string()];

        let key_a = CacheKey::from_request(&req_a, "2026-01-01T10:30:00Z");
        let key_b = CacheKey::from_request(&req_b, "2026-01-01T10:30:00Z");
        assert_eq!(key_a, key_b);
    }

    /// `AlgorithmCache::get` after a `put` with the same key returns the cached value; a second
    /// `put` with the same key replaces, never merges — `get` returns the second value.
    #[test]
    fn put_then_get_returns_the_cached_value_and_replaces_on_second_put() {
        let cache = AlgorithmCache::new();
        let req = sample_request();
        let key = CacheKey::from_request(&req, "2026-01-01T10:30:00Z");

        let first = sample_result();
        cache.put(key.clone(), first.clone());
        assert_eq!(cache.get(&key), Some(first));

        let mut second = sample_result();
        second.wall_clock_ms = 999;
        cache.put(key.clone(), second.clone());
        assert_eq!(cache.get(&key), Some(second));
    }

    /// `AlgorithmCache::get` with a different key (even one field changed) returns `None`.
    #[test]
    fn get_with_a_different_key_returns_none() {
        let cache = AlgorithmCache::new();
        let req = sample_request();
        let key = CacheKey::from_request(&req, "2026-01-01T10:30:00Z");
        cache.put(key, sample_result());

        let mut different_seed = req.clone();
        different_seed.seed = 999;
        let different_key = CacheKey::from_request(&different_seed, "2026-01-01T10:30:00Z");
        assert_eq!(cache.get(&different_key), None);
    }
}
