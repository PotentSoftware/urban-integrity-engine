//! `detector_runs` reader/writer — Phase 32's `:current` marker (D1/D1a), inverted by Phase 46
//! (D-05) into an append-only history table: one row per bake where a detector actually ran,
//! keyed by a fresh `run_id`, never overwritten. See the DDL comment above
//! `DEFINE TABLE IF NOT EXISTS detector_runs SCHEMALESS` in `crates/schema/src/db.rs` for the
//! full rationale, and the phase 32 design notes (D1/D1a) plus the phase 46 design notes
//! (D-05/D-06/D-07) for the operator decision record.
//!
//! Called from `crates/core::graph::coordinator::GraphCoordinator::bake_snapshot`, never from
//! `crates/detector` (D1a — `crates/detector` stays free of any SurrealDB dependency).
//!
//! Two genuinely different questions live in this module and must never be conflated:
//! - **"Which specific runs happened, with names/timestamps/motif types attached?"** —
//!   [`list_detector_runs`]. Defensive: a pre-46-04 legacy `{ last_run_at, motif_count }`-shaped
//!   row (no `run_id`) parses to nothing and is silently excluded, because it names no run.
//! - **"Has the detector engine ever run at all?"** — [`detector_has_ever_run`]. A coarse
//!   existence check that counts EVERY row regardless of shape, legacy or new-format alike,
//!   because the legacy row genuinely, correctly answers this question `true`.

use serde::{Deserialize, Serialize};
use surrealdb::{engine::any::Any, Surreal};

/// A single row in the `detector_runs` history table (Phase 46, D-05/D-06/D-07).
///
/// One row per bake where a detector actually ran (`pipeline == "core"`, written from
/// `GraphCoordinator::bake_snapshot`) — the five enricher post-adapter detectors' own history
/// (`pipeline == "enricher"`) is a future plan's job (this phase's 46-05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectorRunRecord {
    pub run_id: String,
    pub started_at_ms: i64,
    /// "core" | "enricher"
    pub pipeline: String,
    /// Wire strings this run could have produced.
    pub motif_types: Vec<String>,
    pub motif_count: i64,
    /// `Some` only for `pipeline == "core"` — whether an IMD/WIMD index was loaded for this run.
    pub imd_loaded: Option<bool>,
    pub imd_csv_path: Option<String>,
    pub imd_denominator_declared: Option<bool>,
}

/// Append one history row. Always a fresh `CREATE` — `run_id` is already unique per invocation,
/// no UPSERT needed here. Propagates the SurrealDB `Result` rather than swallowing it (never
/// `let _ = db.query(...)`) — the caller (`GraphCoordinator::bake_snapshot`) decides whether a
/// write failure is WARN-only or fatal.
pub async fn record_detector_run_history(
    db: &Surreal<Any>,
    record: DetectorRunRecord,
) -> surrealdb::Result<()> {
    let r = serde_json::to_value(&record).expect("DetectorRunRecord must serialise to JSON");
    db.query("CREATE detector_runs CONTENT $r")
        .bind(("r", r))
        .await?
        .take::<Vec<serde_json::Value>>(0)?;
    Ok(())
}

/// Defensively parse one raw `detector_runs` row. Requires `run_id` to be present as a
/// non-empty string; returns `None` (logged at `debug!`, never `warn!`/`error!` — an old-shape
/// row is an expected, benign fact of the 46-04 migration, not an anomaly) otherwise.
fn parse_detector_run_row(v: &serde_json::Value) -> Option<DetectorRunRecord> {
    let run_id = v.get("run_id").and_then(|x| x.as_str())?;
    if run_id.is_empty() {
        return None;
    }
    Some(DetectorRunRecord {
        run_id: run_id.to_string(),
        started_at_ms: v.get("started_at_ms").and_then(|x| x.as_i64()).unwrap_or(0),
        pipeline: v
            .get("pipeline")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        motif_types: v
            .get("motif_types")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        motif_count: v.get("motif_count").and_then(|x| x.as_i64()).unwrap_or(0),
        imd_loaded: v.get("imd_loaded").and_then(|x| x.as_bool()),
        imd_csv_path: v
            .get("imd_csv_path")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        imd_denominator_declared: v.get("imd_denominator_declared").and_then(|x| x.as_bool()),
    })
}

/// Which specific runs happened, with names/timestamps/motif types attached — excludes any
/// legacy pre-46-04 row (no `run_id`), since such a row names no run. Never a strict whole-vec
/// struct deserialize of the raw query result (`take::<Vec<DetectorRunRecord>>(0)`), which would
/// fail, or silently drop everything, the moment one row does not match the new shape — fetches
/// as `Vec<serde_json::Value>` and maps each row through [`parse_detector_run_row`] instead.
pub async fn list_detector_runs(db: &Surreal<Any>) -> surrealdb::Result<Vec<DetectorRunRecord>> {
    let mut res = db.query("SELECT * FROM detector_runs").await?;
    let rows: Vec<serde_json::Value> = res.take(0)?;
    Ok(rows
        .iter()
        .filter_map(|v| {
            let parsed = parse_detector_run_row(v);
            if parsed.is_none() {
                tracing::debug!(
                    row = %v,
                    "detector_runs row has no run_id — excluded from list_detector_runs \
                     (expected for a pre-46-04 legacy row)"
                );
            }
            parsed
        })
        .collect())
}

/// Has the detector engine ever run at all? A coarse existence check spanning EVERY row shape,
/// legacy or new-format alike — deliberately the opposite exclusion from [`list_detector_runs`]:
/// "has it ever run" and "does a named-run history row exist" are two different questions, and
/// widening `list_detector_runs` itself to also answer this one would start crediting the legacy
/// row with a run identity it does not have.
pub async fn detector_has_ever_run(db: &Surreal<Any>) -> surrealdb::Result<bool> {
    let mut res = db
        .query("SELECT count() FROM detector_runs GROUP ALL")
        .await?;
    let rows: Vec<serde_json::Value> = res.take(0)?;
    Ok(!rows.is_empty() && rows[0]["count"].as_i64().unwrap_or(0) > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_detector_run_row_legacy_shape_returns_none() {
        let legacy = serde_json::json!({ "last_run_at": 123_i64, "motif_count": 5_i64 });
        assert!(
            parse_detector_run_row(&legacy).is_none(),
            "a legacy {{ last_run_at, motif_count }}-shaped row with no run_id must parse to None"
        );
    }

    #[test]
    fn parse_detector_run_row_well_formed_shape_parses() {
        let well_formed = serde_json::json!({
            "run_id": "abc-123",
            "started_at_ms": 1_700_000_000_000_i64,
            "pipeline": "core",
            "motif_types": ["PhoenixLoop", "AddressCluster", "NepotismBridge"],
            "motif_count": 3_i64,
            "imd_loaded": true,
            "imd_csv_path": "data/wimd_2019_wards.csv",
            "imd_denominator_declared": true,
        });
        let parsed = parse_detector_run_row(&well_formed).expect("must parse");
        assert_eq!(parsed.run_id, "abc-123");
        assert_eq!(parsed.motif_count, 3);
        assert_eq!(parsed.pipeline, "core");
        assert_eq!(parsed.motif_types.len(), 3);
        assert_eq!(parsed.imd_loaded, Some(true));
    }

    #[test]
    fn list_detector_runs_tolerates_legacy_row() {
        // Host-side proof of the parsing contract, no DB needed: a mixed input of one legacy
        // value plus two well-formed DetectorRunRecord-shaped values yields exactly 2 parsed
        // records.
        let legacy = serde_json::json!({ "last_run_at": 999_i64, "motif_count": 1_i64 });
        let well_formed_a = serde_json::json!({
            "run_id": "run-a",
            "started_at_ms": 1_i64,
            "pipeline": "core",
            "motif_types": [],
            "motif_count": 1_i64,
            "imd_loaded": null,
            "imd_csv_path": null,
            "imd_denominator_declared": null,
        });
        let well_formed_b = serde_json::json!({
            "run_id": "run-b",
            "started_at_ms": 2_i64,
            "pipeline": "core",
            "motif_types": [],
            "motif_count": 2_i64,
            "imd_loaded": null,
            "imd_csv_path": null,
            "imd_denominator_declared": null,
        });
        let rows = [legacy, well_formed_a, well_formed_b];
        let parsed: Vec<DetectorRunRecord> =
            rows.iter().filter_map(parse_detector_run_row).collect();
        assert_eq!(
            parsed.len(),
            2,
            "exactly 2 of 3 rows are well-formed (named-run) records"
        );
    }
}
