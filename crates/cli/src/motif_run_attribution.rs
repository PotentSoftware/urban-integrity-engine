//! First-seen/last-seen run attribution merge for the `serve` WAL writer (Phase 46, D-06).
//!
//! `crates/detector` is DB-free and re-emits a fresh `MotifResult` every bake a motif still
//! matches — there is no in-process notion of "the same alert as last time" beyond the
//! in-memory `AlertCache`. What makes the SAME motif land in the SAME database row across
//! bakes is `serve`'s WAL writer keying `MotifDetected` writes on a deterministic record id
//! (`unique_hash`) and issuing `UPSERT` instead of `CREATE` — built for Open Item 6d
//! (duplicate-row suppression). This module extends that mechanism one step further: read the
//! row a `MotifDetected` UPSERT is about to overwrite, carry its `first_seen_run_id` forward,
//! and always set `last_seen_run_id` to the current run.
//!
//! Exists as a named, `pub` function (mirroring `lib.rs::detector_config_from`'s own
//! "exists purely so the wiring can be tested" precedent) so the merge decision itself — the
//! subtle, risky part — is directly testable against a real SurrealDB via `TestNs`, independent
//! of the WAL writer's surrounding retry/re-auth machinery.

use surrealdb::{engine::any::Any, Surreal};

/// Compute the `result_json` that should actually be written for a `MotifDetected` UPSERT at
/// `record_id`, merging this write's own `run_id` (already embedded in `current_result_json` by
/// `GraphCoordinator::bake_snapshot`, 46-04 Task 2) with whatever `first_seen_run_id` the row
/// this UPSERT is about to overwrite already carries.
///
/// - No prior row, or a prior row with no `first_seen_run_id`: both `first_seen_run_id` and
///   `last_seen_run_id` are set to this write's own `run_id` — a fresh alert.
/// - A prior row WITH a `first_seen_run_id`: it is carried forward unchanged, and
///   `last_seen_run_id` advances to this write's own `run_id` — a re-detected alert.
///
/// A read failure (as opposed to zero rows, the normal "fresh alert" case) falls back to the
/// "no prior row" branch and logs a `tracing::warn!` — degraded but visible (T-46-04-01), never
/// a fatal write. If `current_result_json` itself fails to parse as JSON, it is returned
/// unchanged (nothing to merge, and the caller's own write proceeds with the original string).
pub async fn merge_first_seen_last_seen_run_id(
    db: &Surreal<Any>,
    record_id: &str,
    current_result_json: &str,
) -> String {
    let prior_result_json: Option<serde_json::Value> = db
        .query("SELECT payload.result_json AS result_json FROM type::record('graph_events', $id)")
        .bind(("id", record_id.to_string()))
        .await
        .and_then(|mut resp| resp.take::<Vec<serde_json::Value>>(0))
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                "first_seen_run_id read-before-write failed — treating this MotifDetected \
                 write as a fresh alert"
            );
            Vec::new()
        })
        .into_iter()
        .next()
        .and_then(|row| {
            row.get("result_json")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

    let prior_first_seen_run_id = prior_result_json
        .as_ref()
        .and_then(|v| v.get("first_seen_run_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let Ok(mut current) = serde_json::from_str::<serde_json::Value>(current_result_json) else {
        return current_result_json.to_string();
    };

    // This write's own run_id — injected onto every MotifDetected event's result_json by
    // bake_snapshot before it ever reaches this writer.
    let current_run_id = current
        .get("run_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();

    match prior_first_seen_run_id {
        Some(first_seen) => {
            current["first_seen_run_id"] = serde_json::json!(first_seen);
            current["last_seen_run_id"] = serde_json::json!(current_run_id);
        }
        None => {
            current["first_seen_run_id"] = serde_json::json!(current_run_id.clone());
            current["last_seen_run_id"] = serde_json::json!(current_run_id);
        }
    }

    serde_json::to_string(&current).unwrap_or_else(|_| current_result_json.to_string())
}
