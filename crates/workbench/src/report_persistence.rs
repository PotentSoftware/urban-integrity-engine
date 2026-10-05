//! `workbench_report:latest` reader/writer — the checkpoint fix (Phase 43 plan 09, amending
//! D-16) that lets the workbench seed its in-memory `report_cache` from the last successful
//! probe battery run instead of starting every process restart from "No probe run yet". See
//! `DEFINE TABLE IF NOT EXISTS workbench_report SCHEMALESS` in `crates/schema/src/db.rs` for the
//! full rationale and the phase 43 design notes' dated amendment under D-16.
//!
//! `persist_report`/`load_report` are the ONLY two places anywhere in the workspace that query
//! `workbench_report` — callers are `crates/workbench::analysis::run_handler` (persist, best
//! effort, after a successful run) and `crates/workbench::boot::boot` (load, at process start).
//!
//! Both functions go through [`to_wire_fields`]/[`from_wire_fields`] — the same {`view`,
//! `markdown`, `run_at`} shape a real UPSERT/SELECT round-trips through SurrealDB — so the
//! ungated unit test below can prove the shape itself round-trips a [`CachedReport`] without a
//! live database; `crates/workbench/tests/report_persistence.rs` is the DB-gated proof that a
//! real SurrealDB instance preserves the same shape.

use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_workbench_dto::ReportView;

use crate::boot::CachedReport;

/// The fixed record id every read/write below targets — never interpolated from analysis-
/// specific input, since one workbench process is always bound to exactly one analysis database
/// (mirrors `detector_runs:current`/`projection_runs:current`'s fixed-literal-id convention).
const RECORD: &str = "workbench_report:latest";

/// Maps a [`CachedReport`] onto the three bound values a write sends over the wire — `view` as a
/// JSON object (never a string: `ReportView` has `Deserialize`, per D-20, unlike `Finding`/
/// `Verdict`), `markdown` as plain text, `run_at` untouched (bound directly as a chrono
/// `DateTime<Utc>` at the call site, never converted to a string first — see this module's
/// top-level doc comment on the datetime-coercion rule).
fn to_wire_fields(cached: &CachedReport) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(&cached.view)
}

/// The inverse of [`to_wire_fields`] plus `run_at`'s own RFC3339 parse — reconstructs a
/// [`CachedReport`] from one raw SurrealDB row (already read back as a `serde_json::Value`).
/// `run_at` is read as a string and parsed with `chrono::DateTime::parse_from_rfc3339` — the
/// same "read as `serde_json::Value`, extract the datetime field as an RFC3339 string" idiom
/// `crates/core/src/persistence/scan.rs::parse_identity_link_row` already uses for
/// `identity_links.resolved_at`, since SurrealDB serialises a `datetime`-typed field to JSON as
/// an RFC3339 string when read back through the generic `serde_json::Value` path.
fn from_wire_fields(row: &serde_json::Value) -> Result<CachedReport, String> {
    let view: ReportView = serde_json::from_value(row["view"].clone())
        .map_err(|e| format!("workbench_report: stored view failed to deserialize: {e}"))?;
    let markdown = row["markdown"]
        .as_str()
        .ok_or_else(|| "workbench_report: stored markdown was not a string".to_string())?
        .to_string();
    let run_at = row["run_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .ok_or_else(|| "workbench_report: stored run_at failed to parse as RFC3339".to_string())?;
    Ok(CachedReport {
        view,
        markdown,
        run_at,
    })
}

/// Best-effort UPSERT of `workbench_report:latest`. The write is verified to return a row
/// (`crates/ingestor/src/backfill/anomaly_sink.rs::record_anomaly`'s documented pattern: an
/// UPSERT matching nothing still reports `status: OK` with an empty result) — an empty result
/// here is treated as a write failure, not a silent no-op.
///
/// The caller (`analysis::run_handler`) treats an `Err` here as WARN-only: a persistence failure
/// must never fail the probe run that produced the report, and the in-memory `report_cache` is
/// left exactly as it was before this call regardless of the outcome.
pub async fn persist_report(db: &Surreal<Any>, cached: &CachedReport) -> anyhow::Result<()> {
    let view_json = to_wire_fields(cached)
        .map_err(|e| anyhow::anyhow!("workbench_report: ReportView serialization failed: {e}"))?;

    let written: Vec<serde_json::Value> = db
        .query(format!(
            "UPSERT {RECORD} SET view = $view, markdown = $markdown, run_at = $run_at"
        ))
        .bind(("view", view_json))
        .bind(("markdown", cached.markdown.clone()))
        .bind(("run_at", cached.run_at))
        .await?
        .take(0)?;

    if written.is_empty() {
        anyhow::bail!(
            "{RECORD} UPSERT wrote no row — the persisted report would be lost, exactly the \
             phantom-write failure mode record_anomaly's own doc comment warns about"
        );
    }
    Ok(())
}

/// Loads `workbench_report:latest`, if a run has ever persisted successfully.
///
/// `Ok(None)` when the record does not exist yet (a fresh analysis that has never completed a
/// run, or `workbench_report` predates this table's existence). `Err` on a genuine
/// read/deserialize failure — the caller (`boot::boot`) logs it via `tracing::warn!` and starts
/// with `None`, never conflating a read error with the legitimate "no report yet" state.
pub async fn load_report(db: &Surreal<Any>) -> anyhow::Result<Option<CachedReport>> {
    let rows: Vec<serde_json::Value> = db
        .query(format!("SELECT view, markdown, run_at FROM {RECORD}"))
        .await?
        .take(0)?;

    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };

    from_wire_fields(&row)
        .map(Some)
        .map_err(|e| anyhow::anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::{BlufView, ProbeGroupView};

    fn sample_view() -> ReportView {
        ReportView {
            bluf: BlufView {
                could_not_evaluate: 1,
                total_probes: 22,
                skipped: 0,
                population: 358_484,
                capable: 358_483,
                hit_count: 3,
            },
            contradiction_checks: vec![],
            groups: vec![ProbeGroupView {
                name: "Confirmed Findings".to_string(),
                probes: vec![],
            }],
            adapted_notes: vec!["adapted note".to_string()],
            remediation_items: vec!["remediate this".to_string()],
            run_at: "2026-01-01T10:33:00Z".to_string(),
        }
    }

    fn sample_cached_report() -> CachedReport {
        CachedReport {
            view: sample_view(),
            markdown: "# Evidence Report\n\nTest dossier body.\n".to_string(),
            run_at: chrono::Utc::now(),
        }
    }

    /// (a) — the ungated unit test D-24's checkpoint fix item 3 requires: `CachedReport`
    /// round-trips through the exact `{view, markdown, run_at}` shape [`persist_report`]/
    /// [`load_report`] send/receive over the wire, with no live database involved. `run_at`
    /// round-trips through the same RFC3339-string path a real SurrealDB `datetime` field
    /// would produce, proven by formatting it exactly as [`from_wire_fields`] expects and
    /// re-parsing it the same way [`from_wire_fields`] does.
    #[test]
    fn cached_report_round_trips_through_the_persisted_wire_shape() {
        let cached = sample_cached_report();

        let view_json = to_wire_fields(&cached).expect("view must serialize");
        let row = serde_json::json!({
            "view": view_json,
            "markdown": cached.markdown,
            "run_at": cached.run_at.to_rfc3339(),
        });

        let back = from_wire_fields(&row).expect("wire shape must deserialize back");

        assert_eq!(back.view, cached.view);
        assert_eq!(back.markdown, cached.markdown);
        assert_eq!(back.run_at, cached.run_at);
    }

    /// A row missing the `view` key fails loudly rather than silently defaulting — mirrors this
    /// crate's DTO round-trip discipline (`workbench-dto`'s "no field is `#[serde(default)]`
    /// unless documented" rule) even though this internal shape is not itself a DTO type.
    #[test]
    fn from_wire_fields_fails_loudly_on_a_missing_view_key() {
        let row = serde_json::json!({
            "markdown": "body",
            "run_at": chrono::Utc::now().to_rfc3339(),
        });
        assert!(
            from_wire_fields(&row).is_err(),
            "a row with no `view` key must fail to deserialize, not silently default"
        );
    }

    /// A row whose `run_at` is not a parseable RFC3339 string fails loudly — proves
    /// [`from_wire_fields`] never falls back to `chrono::Utc::now()` or any other fabricated
    /// timestamp on a malformed value.
    #[test]
    fn from_wire_fields_fails_loudly_on_an_unparseable_run_at() {
        let view_json = to_wire_fields(&sample_cached_report()).unwrap();
        let row = serde_json::json!({
            "view": view_json,
            "markdown": "body",
            "run_at": "not-a-timestamp",
        });
        assert!(
            from_wire_fields(&row).is_err(),
            "an unparseable run_at must fail to deserialize, not fabricate a timestamp"
        );
    }
}
