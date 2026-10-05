//! Ingestor last-event summary (Phase 43 plan 05, D-21/D-28) — the Home page Ingestors card's
//! per-source and overall "last event" timestamps.
//!
//! Derives exclusively from `graph_events` via `urban_integrity_core::persistence::oracle`'s
//! `max_timestamp_for_prefix` — never hand-written SurrealQL in this crate (D-21/D-28). Computed
//! once at boot (`crates/workbench::boot::boot`) and cached on `WorkbenchState`, mirroring the
//! existing `PopulationSummary` pattern: a per-request re-query would repeat 6 SurrealDB
//! round-trips (3 prefixes x 2 `kind`s) on every Home page load.
//!
//! Each of the 6 sub-queries (3 prefixes x `NodeAdded`/`NodeUpdated`) is wrapped in its own 2s
//! timeout. A timeout or query error degrades ONLY that one per-source field to `None` — the
//! other sources that succeeded still populate normally (D-28's "per-source line replaced by
//! overall only" applies per-source, not as an all-or-nothing outcome). `overall` is always the
//! max of whichever per-source values succeeded, computed with no extra query.

use std::time::Duration;

use chrono::{DateTime, Utc};
use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_core::persistence::oracle;

/// Per-source ingestor last-event timestamps plus an overall rollup. `None` means that source has
/// never produced an event (or its query degraded — see this module's doc comment); still a
/// meaningful, honest value, never fabricated.
#[derive(Debug, Clone, Default)]
pub struct LastEventSummary {
    pub companies_house: Option<DateTime<Utc>>,
    pub contracts_finder: Option<DateTime<Utc>>,
    pub planning: Option<DateTime<Utc>>,
    pub overall: Option<DateTime<Utc>>,
}

/// One prefix's max timestamp across both `NodeAdded` and `NodeUpdated` — each `kind` queried and
/// timed out independently so a slow/failing query for one `kind` cannot mask a healthy result
/// from the other.
async fn max_for_prefix(db: &Surreal<Any>, prefix: &str, label: &str) -> Option<DateTime<Utc>> {
    let added = query_one(db, "NodeAdded", prefix, label).await;
    let updated = query_one(db, "NodeUpdated", prefix, label).await;

    let max_ms = match (added, updated) {
        (None, None) => return None,
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (Some(a), Some(b)) => a.max(b),
    };
    DateTime::from_timestamp_millis(max_ms as i64)
}

/// One `(kind, prefix)` sub-query, 2s-timeout-guarded. A timeout or query error is logged and
/// degrades to `None` for this one sub-query only — never propagated as a hard failure, since a
/// single slow ingestor source must not take down the whole Home page.
async fn query_one(db: &Surreal<Any>, kind: &str, prefix: &str, label: &str) -> Option<u64> {
    match tokio::time::timeout(
        Duration::from_secs(2),
        oracle::max_timestamp_for_prefix(db, kind, prefix),
    )
    .await
    {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            tracing::warn!(
                error = %e,
                source = label,
                kind,
                "workbench: last_event query failed — this source's last-event timestamp will \
                 read as None on the Home page"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                source = label,
                kind,
                "workbench: last_event query timed out after 2s — this source's last-event \
                 timestamp will read as None on the Home page"
            );
            None
        }
    }
}

/// Computes the Home page Ingestors card's per-source and overall last-event timestamps. Called
/// once at boot; the result is cached on `WorkbenchState::last_events`, never re-queried per
/// request.
pub async fn compute(db: &Surreal<Any>) -> LastEventSummary {
    let companies_house = max_for_prefix(db, "GB-COH:", "companies_house").await;
    let contracts_finder = max_for_prefix(db, "CF:", "contracts_finder").await;
    let planning = max_for_prefix(db, "planning:", "planning").await;
    let overall = [companies_house, contracts_finder, planning]
        .into_iter()
        .flatten()
        .max();

    LastEventSummary {
        companies_house,
        contracts_finder,
        planning,
        overall,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `compute`'s `overall` field is the max of whichever per-source values succeeded — proven
    /// here as a pure, DB-free fact about the aggregation shape (the DB-gated correctness proof
    /// against a real SurrealDB instance lives in
    /// `crates/workbench/tests/report_view_equals_finding_list.rs`).
    #[test]
    fn overall_is_the_max_of_whichever_sources_are_present() {
        let a = DateTime::from_timestamp_millis(1_700_000_000_100).unwrap();
        let b = DateTime::from_timestamp_millis(1_700_000_000_300).unwrap();
        let c = DateTime::from_timestamp_millis(1_700_000_000_200).unwrap();

        let overall = [Some(a), Some(b), Some(c)].into_iter().flatten().max();
        assert_eq!(overall, Some(b));

        // One source missing: overall is still the max of the remaining two.
        let overall_partial = [Some(a), None, Some(c)].into_iter().flatten().max();
        assert_eq!(overall_partial, Some(c));

        // All sources missing: overall is None, not a fabricated timestamp.
        let overall_none: Option<DateTime<Utc>> = [None, None, None].into_iter().flatten().max();
        assert_eq!(overall_none, None);
    }
}
