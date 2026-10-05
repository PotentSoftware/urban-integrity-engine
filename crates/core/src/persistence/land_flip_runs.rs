//! `land_flip_runs:current` marker + `land_flip_gate1_survivors` reader/writer (Phase 34,
//! PRB-03, D-01..D-06).
//!
//! `crates/core` cannot import `crates/enricher` (dependency direction: enricher -> core, never
//! the reverse), so the `probe` subcommand needs its own read path for land-flip run state that
//! never existed until now — the detector previously returned `LandFlipStats` in-process from
//! `crates/enricher/src/engine.rs:409` and discarded it (D-01). This module is the only place
//! either new table is written or read; the writer side is called from
//! `crates/enricher::land_flip::run_detection` (Plan 34-03).
//!
//! `land_flip_runs:current` mirrors `projection_runs:current`'s "single/keyed record,
//! success-stamp field checked by readers, never row counts" idiom (D-02): `finished_at_ms` is
//! set ONLY in the same `UPSERT` statement that sets every other field, on the success path, so
//! a reader can never observe a partially-written row (D-02/D-04). See the `DEFINE TABLE`
//! doc comment above `land_flip_runs`/`land_flip_gate1_survivors` in `crates/schema/src/db.rs`
//! for the full schema rationale.

use surrealdb::{engine::any::Any, Surreal};
use tracing::warn;

use super::scan::{record_id_str, SCAN_MAX_ROWS};

/// The single `land_flip_runs:current` record, decoded from a raw SurrealDB row.
///
/// All 11 fields — the 10 `LandFlipStats` fields
/// (`crates/enricher/src/land_flip.rs:278-323`) plus `finished_at_ms` — persisted verbatim
/// (D-03), so the probe can attribute a zero to a specific rejection cause rather than reporting
/// an opaque "nothing found".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandFlipRunMarker {
    pub applications_scanned: i64,
    pub candidates_written: i64,
    pub candidate_writes_failed: i64,
    pub postcode_only_matches: i64,
    pub gate1_no_ppd_match: i64,
    pub gate3_query_failed: i64,
    pub gate3_no_corroboration: i64,
    pub gate2_no_anchor_date: i64,
    pub gate2_outside_window: i64,
    pub candidate_apps: i64,
    pub finished_at_ms: i64,
}

/// UPSERT the `land_flip_runs:current` marker in ONE statement, all 11 fields bound together —
/// `finished_at_ms` is written in the SAME `UPSERT` as every other field, never a second write,
/// so a reader can never observe a partially-written row (D-02/D-04).
///
/// Propagates the `surrealdb::Result` rather than swallowing it (never `let _ = db.query(...)`)
/// — the caller (Plan 34-03's `run_detection`) decides whether a write failure is WARN-only or
/// fatal; this function itself never discards the write's `Result` (the ICIJ-incident
/// rule, docs/DEVELOPMENT.md Standing Rule 1).
pub async fn record_land_flip_run(
    db: &Surreal<Any>,
    marker: &LandFlipRunMarker,
) -> surrealdb::Result<()> {
    db.query(
        "UPSERT land_flip_runs:current SET \
         applications_scanned = $applications_scanned, \
         candidates_written = $candidates_written, \
         candidate_writes_failed = $candidate_writes_failed, \
         postcode_only_matches = $postcode_only_matches, \
         gate1_no_ppd_match = $gate1_no_ppd_match, \
         gate3_query_failed = $gate3_query_failed, \
         gate3_no_corroboration = $gate3_no_corroboration, \
         gate2_no_anchor_date = $gate2_no_anchor_date, \
         gate2_outside_window = $gate2_outside_window, \
         candidate_apps = $candidate_apps, \
         finished_at_ms = $finished_at_ms",
    )
    .bind(("applications_scanned", marker.applications_scanned))
    .bind(("candidates_written", marker.candidates_written))
    .bind(("candidate_writes_failed", marker.candidate_writes_failed))
    .bind(("postcode_only_matches", marker.postcode_only_matches))
    .bind(("gate1_no_ppd_match", marker.gate1_no_ppd_match))
    .bind(("gate3_query_failed", marker.gate3_query_failed))
    .bind(("gate3_no_corroboration", marker.gate3_no_corroboration))
    .bind(("gate2_no_anchor_date", marker.gate2_no_anchor_date))
    .bind(("gate2_outside_window", marker.gate2_outside_window))
    .bind(("candidate_apps", marker.candidate_apps))
    .bind(("finished_at_ms", marker.finished_at_ms))
    .await?
    .take::<Vec<serde_json::Value>>(0)?;
    Ok(())
}

/// Read back the `land_flip_runs:current` marker, if one has ever finished.
///
/// Returns `None` when the row is absent OR when `finished_at_ms` cannot be parsed as an `i64`
/// — collapsing "never ran" and "ran but the success stamp is missing" into the same `None`
/// (D-04), mirroring `get_detector_run_marker`'s exact posture. The other 10 fields default to
/// `0` via `unwrap_or(0)` if individually unparseable.
pub async fn get_land_flip_run_marker(
    db: &Surreal<Any>,
) -> surrealdb::Result<Option<LandFlipRunMarker>> {
    let mut res = db
        .query(
            "SELECT applications_scanned, candidates_written, candidate_writes_failed, \
             postcode_only_matches, gate1_no_ppd_match, gate3_query_failed, \
             gate3_no_corroboration, gate2_no_anchor_date, gate2_outside_window, \
             candidate_apps, finished_at_ms FROM land_flip_runs:current",
        )
        .await?;
    let rows: Vec<serde_json::Value> = res.take(0)?;
    let marker = rows.into_iter().next().and_then(|v| {
        let finished_at_ms = v["finished_at_ms"].as_i64()?;
        Some(LandFlipRunMarker {
            applications_scanned: v["applications_scanned"].as_i64().unwrap_or(0),
            candidates_written: v["candidates_written"].as_i64().unwrap_or(0),
            candidate_writes_failed: v["candidate_writes_failed"].as_i64().unwrap_or(0),
            postcode_only_matches: v["postcode_only_matches"].as_i64().unwrap_or(0),
            gate1_no_ppd_match: v["gate1_no_ppd_match"].as_i64().unwrap_or(0),
            gate3_query_failed: v["gate3_query_failed"].as_i64().unwrap_or(0),
            gate3_no_corroboration: v["gate3_no_corroboration"].as_i64().unwrap_or(0),
            gate2_no_anchor_date: v["gate2_no_anchor_date"].as_i64().unwrap_or(0),
            gate2_outside_window: v["gate2_outside_window"].as_i64().unwrap_or(0),
            candidate_apps: v["candidate_apps"].as_i64().unwrap_or(0),
            finished_at_ms,
        })
    });
    Ok(marker)
}

/// One `land_flip_gate1_survivors` row (`crates/schema/src/db.rs`).
#[derive(Debug, Clone)]
pub struct Gate1Survivor {
    pub planning_ref: String,
    pub applicant_id: String,
    pub location_id: String,
}

/// Replace the entire `land_flip_gate1_survivors` table with `survivors` — DELETE every existing
/// row, then CREATE one row per survivor, propagating the first write failure via `?` (never
/// silently continuing past one). A full wipe-and-rebuild per run, not an accumulating upsert:
/// see the `DEFINE TABLE land_flip_gate1_survivors` doc comment in `crates/schema/src/db.rs` for
/// why a survivor that stops matching gate-1 between runs must not linger as a stale "capable"
/// row.
pub async fn write_gate1_survivors(
    db: &Surreal<Any>,
    survivors: &[Gate1Survivor],
) -> surrealdb::Result<()> {
    db.query("DELETE land_flip_gate1_survivors")
        .await?
        .check()?;
    for s in survivors {
        db.query("CREATE land_flip_gate1_survivors CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "planning_ref": s.planning_ref,
                    "applicant_id": s.applicant_id,
                    "location_id": s.location_id,
                }),
            ))
            .await?
            .check()?;
    }
    Ok(())
}

/// Every `land_flip_gate1_survivors` row, paged past [`SCAN_PAGE_SIZE`]-scale tables if needed.
///
/// [`SCAN_PAGE_SIZE`]: super::scan::SCAN_PAGE_SIZE
pub async fn scan_land_flip_gate1_survivors(
    db: &Surreal<Any>,
) -> surrealdb::Result<Vec<Gate1Survivor>> {
    scan_land_flip_gate1_survivors_paged(db, super::scan::SCAN_PAGE_SIZE).await
}

/// [`scan_land_flip_gate1_survivors`] with an explicit page size, so tests can force multi-page
/// behaviour.
///
/// Ordering is `ORDER BY id` — never a data column — and the loop terminates only on an empty
/// page (GRD-04), identical shape to `scan_land_flip_candidates_paged`. `id` is included in the
/// projection because SurrealDB 3.x rejects an unselected `ORDER BY` field. `applicant_id`/
/// `location_id` are decoded via [`record_id_str`], tolerating SurrealDB's RecordId
/// auto-coercion on values matching `table:key` syntax (e.g. `director:jane-smith`,
/// `location:xyz`) the same way `identity_links.source_node_id`/`target_node_id` do;
/// `planning_ref` never takes `table:key` shape, so plain string extraction is safe for it. A
/// row whose `applicant_id`/`location_id` cannot be recovered is unusable and is skipped with a
/// warning, mirroring `parse_identity_link_row`'s posture.
pub async fn scan_land_flip_gate1_survivors_paged(
    db: &Surreal<Any>,
    page_size: usize,
) -> surrealdb::Result<Vec<Gate1Survivor>> {
    let mut out: Vec<Gate1Survivor> = Vec::new();
    let mut start = 0usize;
    loop {
        let sql = format!(
            "SELECT planning_ref, applicant_id, location_id, id FROM land_flip_gate1_survivors \
             ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<serde_json::Value> = db.query(sql).await?.take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break; // the only valid termination — GRD-04
        }
        out.extend(rows.iter().filter_map(|v| {
            let planning_ref = v["planning_ref"].as_str().map(str::to_owned);
            let applicant_id = record_id_str(&v["applicant_id"]);
            let location_id = record_id_str(&v["location_id"]);
            match (planning_ref, applicant_id, location_id) {
                (Some(planning_ref), Some(applicant_id), Some(location_id)) => {
                    Some(Gate1Survivor {
                        planning_ref,
                        applicant_id,
                        location_id,
                    })
                }
                _ => {
                    warn!(
                        raw = %v,
                        "land_flip_gate1_survivors row unusable — planning_ref/applicant_id/\
                         location_id could not be parsed; skipping"
                    );
                    None
                }
            }
        }));
        start += returned;
        if start >= SCAN_MAX_ROWS {
            warn!(
                scanned = start,
                "land_flip_gate1_survivors scan hit the safety cap — results are truncated"
            );
            break;
        }
    }
    Ok(out)
}

/// Whether `ppd_transactions` has at least one row — D-08's second `DataGap` cause ("no staged
/// PPD transactions").
///
/// `SELECT count() FROM ppd_transactions GROUP ALL` is guard-exempt by construction
/// (`query_shape_guard.rs`'s Pitfall 4 — `count()` is not a row-losing shape), but still routes
/// through this dedicated persistence function rather than a bespoke query written inline in the
/// probe, per PRB-03's stronger-than-the-guard bar (no bespoke SQL in the probe subcommand).
///
/// An empty `ppd_transactions` table produces zero grouped rows, not a single row with
/// `count: 0` — `Ok(false)` covers "table empty". A row WITHOUT an integer `count` is a
/// response-shape change and errors instead (WR-01 class): pre-fix it collapsed into
/// `Ok(false)`, and the land-flip probe then reported "no staged PPD transactions" — a wrong
/// data claim about a populated table — rather than routing to its distinct
/// "ppd_transactions check failed" arm.
pub async fn ppd_transactions_populated(db: &Surreal<Any>) -> anyhow::Result<bool> {
    let mut res = db
        .query("SELECT count() FROM ppd_transactions GROUP ALL")
        .await?;
    let rows: Vec<serde_json::Value> = res.take(0)?;
    let count = match rows.first() {
        None => 0,
        Some(r) => r["count"].as_u64().ok_or_else(|| {
            anyhow::anyhow!(
                "ppd_transactions_populated: response row carried no integer `count` field \
                 (SurrealDB response shape changed?): {r}"
            )
        })?,
    };
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_tests_enabled() -> bool {
        urban_integrity_test_support::db_tests_enabled()
    }

    fn sample_marker(seed: i64) -> LandFlipRunMarker {
        LandFlipRunMarker {
            applications_scanned: seed,
            candidates_written: seed + 1,
            candidate_writes_failed: seed + 2,
            postcode_only_matches: seed + 3,
            gate1_no_ppd_match: seed + 4,
            gate3_query_failed: seed + 5,
            gate3_no_corroboration: seed + 6,
            gate2_no_anchor_date: seed + 7,
            gate2_outside_window: seed + 8,
            candidate_apps: seed + 9,
            finished_at_ms: seed + 10,
        }
    }

    #[tokio::test]
    async fn db_run_marker_round_trips_all_eleven_fields() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_runs",
            "marker_roundtrip",
        )
        .await;

        let marker = sample_marker(1_000);
        record_land_flip_run(&db, &marker).await.expect("record");

        let read_back = get_land_flip_run_marker(&db)
            .await
            .expect("read")
            .expect("marker must be present after a write");
        assert_eq!(
            read_back, marker,
            "every one of the 11 fields must round-trip exactly, not just be non-zero"
        );
    }

    #[tokio::test]
    async fn db_run_marker_absent_on_empty_ns_is_none() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_runs",
            "marker_absent",
        )
        .await;

        let marker = get_land_flip_run_marker(&db).await.expect("read");
        assert_eq!(
            marker, None,
            "no row written must read back as None, not a zeroed marker"
        );
    }

    #[tokio::test]
    async fn db_survivors_are_wiped_and_rebuilt_not_accumulated() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_runs",
            "survivors_rebuild",
        )
        .await;

        let first: Vec<Gate1Survivor> = (0..3)
            .map(|i| Gate1Survivor {
                planning_ref: format!("PLAN/{i:04}"),
                applicant_id: format!("director:applicant-{i}"),
                location_id: format!("location:loc-{i}"),
            })
            .collect();
        write_gate1_survivors(&db, &first).await.expect("write 1");

        let read_back = scan_land_flip_gate1_survivors_paged(&db, 20)
            .await
            .expect("scan 1");
        assert_eq!(read_back.len(), 3, "all 3 seeded survivors must round-trip");
        for s in &first {
            assert!(
                read_back.iter().any(|r| r.planning_ref == s.planning_ref
                    && r.applicant_id == s.applicant_id
                    && r.location_id == s.location_id),
                "survivor {s:?} missing from scan"
            );
        }

        let second: Vec<Gate1Survivor> = (0..2)
            .map(|i| Gate1Survivor {
                planning_ref: format!("PLAN/{i:04}"),
                applicant_id: format!("director:second-{i}"),
                location_id: format!("location:second-loc-{i}"),
            })
            .collect();
        write_gate1_survivors(&db, &second).await.expect("write 2");

        let read_back_2 = scan_land_flip_gate1_survivors_paged(&db, 20)
            .await
            .expect("scan 2");
        assert_eq!(
            read_back_2.len(),
            2,
            "the second write must DELETE-then-rebuild, not accumulate on top of the first"
        );
    }

    #[tokio::test]
    async fn db_survivors_scan_pages_past_a_forced_page_size() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_runs",
            "survivors_paging",
        )
        .await;

        let survivors: Vec<Gate1Survivor> = (0..5)
            .map(|i| Gate1Survivor {
                planning_ref: format!("PLAN/{i:04}"),
                applicant_id: format!("director:applicant-{i}"),
                location_id: format!("location:loc-{i}"),
            })
            .collect();
        write_gate1_survivors(&db, &survivors).await.expect("write");

        // Page size below the seeded row count so the paging path is exercised.
        let all = scan_land_flip_gate1_survivors_paged(&db, 2)
            .await
            .expect("scan");
        assert_eq!(
            all.len(),
            5,
            "all 5 rows must be returned across page boundaries"
        );
    }

    #[tokio::test]
    async fn db_ppd_transactions_populated_reflects_row_presence() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_runs",
            "ppd_populated",
        )
        .await;

        assert!(
            !ppd_transactions_populated(&db).await.expect("check empty"),
            "an empty ppd_transactions table must report false"
        );

        db.query(
            "CREATE ppd_transactions SET transaction_id = 'txn-1', price_gbp = 100000, \
             date_of_transfer = $date, postcode_raw = 'AB1 2CD', postcode_norm = 'AB12CD', \
             property_type = 'D', old_new = 'N', tenure = 'F', paon = '1', paon_norm = '1', \
             street = 'Test Street', street_norm = 'teststreet', town = 'Testville', \
             district = 'Test District', county = 'Test County', ppd_category = 'A', \
             district_code = 'W05000001'",
        )
        .bind(("date", chrono::Utc::now()))
        .await
        .expect("seed ppd row")
        .check()
        .expect("seed ppd row check");

        assert!(
            ppd_transactions_populated(&db).await.expect("check seeded"),
            "a seeded ppd_transactions table must report true"
        );
    }
}
