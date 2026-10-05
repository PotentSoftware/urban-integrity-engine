//! DB-gated test proving `detector_runs`'s dual-branch behaviour (Phase 32, Task 2, operator
//! decisions D1/D1a in the phase 32 design notes), rewritten for
//! the Phase 46 (D-05) history-table inversion: this test now polls the coarse existence check
//! (`detector_has_ever_run`) rather than a singleton marker, and additionally proves the
//! history-table path (`list_detector_runs`) is genuinely exercised.
//!
//! Test 1: a bake with a detector configured writes a `detector_runs` history row with the
//! correct `motif_count`, and `detector_has_ever_run` reports `true`.
//! Test 2: a bake with no detector configured (`detector: None`) leaves `detector_runs`
//! completely untouched — no row, `detector_has_ever_run` reports `false`, and
//! `list_detector_runs` reports empty.
//!
//! Gated by `URBAN_DB_TESTS=1`; ungated runs SKIP both tests (see docs/TESTING.md's skip-by-return
//! caveat — both the gated and ungated durations for this binary are recorded in the plan's
//! SUMMARY, not just the pass count, since the two report identical pass counts either way).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::graph::coordinator::{
    CoordinatorConfig, GraphCoordinator, TierBDetector,
};
use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_core::graph::types::GraphEventKind;
use urban_integrity_core::persistence::detector_runs::{detector_has_ever_run, list_detector_runs};

/// Minimal `TierBDetector` stub returning a fixed, non-empty set of `MotifDetected` events —
/// this test only needs the write path to observe a non-zero `Vec::len()`, not a real motif.
struct StubDetector;

impl TierBDetector for StubDetector {
    fn run_tier_b(
        &self,
        _snapshot: &CsrSnapshot,
        _delta_nodes: &[u32],
        _id_map: &HashMap<u32, String>,
    ) -> Vec<GraphEventKind> {
        vec![
            GraphEventKind::MotifDetected {
                result_json: "{}".to_string(),
            },
            GraphEventKind::MotifDetected {
                result_json: "{}".to_string(),
            },
            GraphEventKind::MotifDetected {
                result_json: "{}".to_string(),
            },
        ]
    }

    fn imd_state(&self) -> urban_integrity_core::graph::coordinator::ImdRunState {
        urban_integrity_core::graph::coordinator::ImdRunState {
            loaded: false,
            csv_path: None,
            denominator_declared: false,
        }
    }
}

/// Poll `detector_has_ever_run` until it returns `true` or `timeout` elapses. The history row
/// write happens asynchronously after `bake_snapshot`'s `spawn_blocking` join, so a fixed short
/// sleep risks flakiness under load — poll instead, matching `coordinator_update_node.rs`'s
/// "give the coordinator time to process" idiom but bounded and adaptive.
async fn poll_for_run(db: &Surreal<Any>, timeout: Duration) -> bool {
    let start = tokio::time::Instant::now();
    loop {
        let has_run = detector_has_ever_run(db)
            .await
            .expect("detector_has_ever_run read must not error");
        if has_run || start.elapsed() >= timeout {
            return has_run;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn detector_run_marker_written_when_detector_ran() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: detector_run_marker_written_when_detector_ran (set URBAN_DB_TESTS=1)");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix(
        "test_detector_marker",
        "written",
    )
    .await;

    let (wal_tx, _wal_rx) = crossbeam_channel::unbounded();
    let config = CoordinatorConfig {
        snapshot_every_n_events: 100_000,
        snapshot_interval_secs: 10_000,
    };
    let (coordinator, handle) =
        GraphCoordinator::new_with_db(config, wal_tx, Some(Arc::new(StubDetector)), Some(db.arc()));

    let task = tokio::spawn(async move { coordinator.run().await });
    handle.checkpoint_tx.send(()).await.unwrap();

    let has_run = poll_for_run(&db, Duration::from_secs(5)).await;
    task.abort();

    assert!(
        has_run,
        "detector_runs must report has-ever-run after a bake with a detector configured"
    );

    // Proves the NEW history-table path is exercised, not just the coarse existence check.
    let runs = list_detector_runs(&db)
        .await
        .expect("list_detector_runs read must not error");
    assert_eq!(
        runs.len(),
        1,
        "exactly one history row must be written for one bake"
    );
    assert_eq!(
        runs[0].motif_count, 3,
        "motif_count must equal the number of events returned by run_tier_b"
    );
    assert!(
        !runs[0].run_id.is_empty(),
        "the history row's run_id must be non-empty"
    );
}

#[tokio::test]
async fn detector_run_marker_absent_when_no_detector_configured() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: detector_run_marker_absent_when_no_detector_configured (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db =
        urban_integrity_test_support::TestNs::connect_with_prefix("test_detector_marker", "absent")
            .await;

    let (wal_tx, _wal_rx) = crossbeam_channel::unbounded();
    let config = CoordinatorConfig {
        snapshot_every_n_events: 100_000,
        snapshot_interval_secs: 10_000,
    };
    let (coordinator, handle) = GraphCoordinator::new_with_db(config, wal_tx, None, Some(db.arc()));

    let task = tokio::spawn(async move { coordinator.run().await });
    handle.checkpoint_tx.send(()).await.unwrap();

    // No detector configured this bake: there is no "appears" signal to poll for, so wait a
    // generous fixed duration for the bake to complete before asserting absence.
    tokio::time::sleep(Duration::from_millis(500)).await;
    task.abort();

    let has_run = detector_has_ever_run(&db)
        .await
        .expect("detector_has_ever_run read must not error");
    assert!(
        !has_run,
        "detector_runs must stay untouched when bake_snapshot ran with no detector configured"
    );
    let runs = list_detector_runs(&db)
        .await
        .expect("list_detector_runs read must not error");
    assert!(
        runs.is_empty(),
        "list_detector_runs must be empty when bake_snapshot ran with no detector configured"
    );
}
