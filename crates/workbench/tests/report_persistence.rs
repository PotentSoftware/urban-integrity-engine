//! DB-gated round-trip proof for `workbench_report:latest` persistence (checkpoint fix item 3,
//! Phase 43 plan 09, amending D-16): `report_persistence::persist_report` writes a
//! [`CachedReport`], `report_persistence::load_report` reads it back, and `view`/`markdown` come
//! back byte-identical while `run_at` survives as a real `DateTime<Utc>` close to what was
//! written.
//!
//! Uses `TestNs::connect` per the mandatory test-namespace-hygiene rule (docs/TESTING.md) — the guard stays
//! bound for the whole test, exactly as `crates/workbench/tests/
//! report_view_equals_finding_list.rs` already does.

use urban_integrity_test_support::TestNs;
use urban_integrity_workbench::boot::CachedReport;
use urban_integrity_workbench::report_persistence::{load_report, persist_report};
use urban_integrity_workbench_dto::{BlufView, ProbeGroupView, ReportView};

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
        markdown: "# Evidence Report\n\nTest dossier body from the plan 09 checkpoint fix.\n"
            .to_string(),
        run_at: chrono::Utc::now(),
    }
}

/// (b) — the DB-gated integration test the checkpoint fix requires: `persist_report` followed by
/// `load_report` against a real SurrealDB instance returns `view`/`markdown` byte-identical and
/// `run_at` surviving as a real datetime close to what was written.
#[tokio::test]
async fn persist_then_load_round_trips_against_a_live_store() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "[skip] persist_then_load_round_trips_against_a_live_store (set URBAN_DB_TESTS=1 \
             with SurrealDB running to execute)"
        );
        return;
    }

    let db = TestNs::connect("test_workbench_report").await;
    let cached = sample_cached_report();

    persist_report(&db, &cached)
        .await
        .expect("persist_report must succeed against a live store");

    let loaded = load_report(&db)
        .await
        .expect("load_report must succeed against a live store")
        .expect("a record was just persisted — load_report must return Some");

    assert_eq!(
        loaded.view, cached.view,
        "ReportView must round-trip byte-identical through workbench_report:latest"
    );
    assert_eq!(
        loaded.markdown, cached.markdown,
        "markdown must round-trip byte-identical through workbench_report:latest"
    );

    let delta_ms = (loaded.run_at - cached.run_at).num_milliseconds().abs();
    assert!(
        delta_ms < 2_000,
        "run_at must survive as a real datetime close to what was written, got delta={delta_ms}ms \
         (loaded={:?}, written={:?})",
        loaded.run_at,
        cached.run_at
    );
}

/// `load_report` on a namespace where `workbench_report` has never been written returns `Ok(None)`
/// — never an error, and never a fabricated empty `CachedReport`.
#[tokio::test]
async fn load_report_returns_none_when_nothing_has_ever_been_persisted() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "[skip] load_report_returns_none_when_nothing_has_ever_been_persisted (set \
             URBAN_DB_TESTS=1 with SurrealDB running to execute)"
        );
        return;
    }

    let db = TestNs::connect("test_workbench_report_empty").await;

    let loaded = load_report(&db)
        .await
        .expect("load_report must succeed on a namespace with no persisted report");
    assert!(loaded.is_none());
}

/// A second `persist_report` call REPLACES `workbench_report:latest` rather than accumulating —
/// mirrors `projection_runs:current`/`detector_runs:current`'s single-record UPSERT idiom.
#[tokio::test]
async fn a_second_persist_replaces_the_first_rather_than_accumulating() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "[skip] a_second_persist_replaces_the_first_rather_than_accumulating (set \
             URBAN_DB_TESTS=1 with SurrealDB running to execute)"
        );
        return;
    }

    let db = TestNs::connect("test_workbench_report_replace").await;

    let first = sample_cached_report();
    persist_report(&db, &first)
        .await
        .expect("first persist_report must succeed");

    let mut second = sample_cached_report();
    second.markdown = "# Evidence Report\n\nSecond, replacing run.\n".to_string();
    second.view.bluf.hit_count = 9;
    persist_report(&db, &second)
        .await
        .expect("second persist_report must succeed");

    let loaded = load_report(&db)
        .await
        .expect("load_report must succeed")
        .expect("a record was persisted");

    assert_eq!(loaded.markdown, second.markdown);
    assert_eq!(loaded.view.bluf.hit_count, 9);

    let count: Vec<serde_json::Value> = db
        .query("SELECT count() FROM workbench_report GROUP ALL")
        .await
        .expect("count query must succeed")
        .take(0)
        .expect("count query must decode");
    assert_eq!(
        count.len(),
        1,
        "exactly one workbench_report row must exist after two persists"
    );
}
