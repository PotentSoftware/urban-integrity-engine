//! DB-gated proof of `merge_first_seen_last_seen_run_id` (Phase 46, D-06): a fresh unique_hash
//! UPSERTs with `first_seen_run_id == last_seen_run_id == run_id`, and a re-detected unique_hash
//! preserves its `first_seen_run_id` while advancing `last_seen_run_id`.
//!
//! Uses `urban_integrity_test_support::TestNs` for its namespace — never a hand-rolled
//! `format!("test_{uuid}")` (test-namespace hygiene rule, docs/TESTING.md). Exercises the exact
//! production merge function (`urban_integrity_cli::motif_run_attribution`), then performs the
//! same `UPSERT ... CONTENT $r` write `serve`'s WAL writer issues and re-reads the row, so the
//! assertion is against what is actually stored, not just the function's return value.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_cli::motif_run_attribution::merge_first_seen_last_seen_run_id;

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Mirrors the exact `UPSERT type::record('graph_events', $id) CONTENT $r` shape `serve`'s WAL
/// writer issues for `MotifDetected` events (`crates/cli/src/main.rs`), reproduced here since
/// that closure is not `pub`.
async fn upsert_motif_detected(db: &Surreal<Any>, record_id: &str, result_json: &str) {
    let r = serde_json::json!({
        "event_id": record_id,
        "timestamp": 1_700_000_000_000u64,
        "kind": "MotifDetected",
        "payload": { "result_json": result_json },
    });
    db.query("UPSERT type::record('graph_events', $id) CONTENT $r")
        .bind(("id", record_id.to_string()))
        .bind(("r", r))
        .await
        .expect("UPSERT must not error")
        .check()
        .expect("UPSERT must not fail (check)");
}

async fn read_result_json(db: &Surreal<Any>, record_id: &str) -> serde_json::Value {
    let mut resp = db
        .query("SELECT payload.result_json AS result_json FROM type::record('graph_events', $id)")
        .bind(("id", record_id.to_string()))
        .await
        .expect("SELECT must not error");
    let rows: Vec<serde_json::Value> = resp.take(0).expect("SELECT must not fail (check)");
    let row = rows.into_iter().next().expect("row must exist");
    let s = row["result_json"]
        .as_str()
        .expect("result_json must be a string");
    serde_json::from_str(s).expect("result_json must parse as JSON")
}

#[tokio::test]
async fn a_fresh_unique_hash_upserts_with_first_seen_equal_to_last_seen() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: a_fresh_unique_hash_upserts_with_first_seen_equal_to_last_seen (set \
             URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix(
        "test_motif_run_attribution",
        "fresh",
    )
    .await;

    let record_id = "MotifDetected_fresh-hash_";
    let current_result_json = serde_json::json!({
        "motif_type": "PhoenixLoop",
        "unique_hash": "fresh-hash",
        "run_id": "run-a",
    })
    .to_string();

    // No prior row at this record_id — the merge must treat this as a fresh alert.
    let merged = merge_first_seen_last_seen_run_id(&db, record_id, &current_result_json).await;
    upsert_motif_detected(&db, record_id, &merged).await;

    let stored = read_result_json(&db, record_id).await;
    assert_eq!(
        stored["first_seen_run_id"].as_str(),
        Some("run-a"),
        "a fresh alert's first_seen_run_id must equal this write's own run_id, got: {stored}"
    );
    assert_eq!(
        stored["last_seen_run_id"].as_str(),
        Some("run-a"),
        "a fresh alert's last_seen_run_id must equal this write's own run_id, got: {stored}"
    );
}

#[tokio::test]
async fn a_re_detected_unique_hash_preserves_first_seen_and_advances_last_seen() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: a_re_detected_unique_hash_preserves_first_seen_and_advances_last_seen (set \
             URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix(
        "test_motif_run_attribution",
        "redetected",
    )
    .await;

    let record_id = "MotifDetected_redetected-hash_";

    // First bake, run_id_a: writes the row fresh.
    let first_result_json = serde_json::json!({
        "motif_type": "AddressCluster",
        "unique_hash": "redetected-hash",
        "run_id": "run-a",
    })
    .to_string();
    let merged_first = merge_first_seen_last_seen_run_id(&db, record_id, &first_result_json).await;
    upsert_motif_detected(&db, record_id, &merged_first).await;

    let after_first = read_result_json(&db, record_id).await;
    assert_eq!(after_first["first_seen_run_id"].as_str(), Some("run-a"));
    assert_eq!(after_first["last_seen_run_id"].as_str(), Some("run-a"));

    // Second bake, run_id_b: the SAME unique_hash re-fires under a later run.
    let second_result_json = serde_json::json!({
        "motif_type": "AddressCluster",
        "unique_hash": "redetected-hash",
        "run_id": "run-b",
    })
    .to_string();
    let merged_second =
        merge_first_seen_last_seen_run_id(&db, record_id, &second_result_json).await;
    upsert_motif_detected(&db, record_id, &merged_second).await;

    let after_second = read_result_json(&db, record_id).await;
    assert_eq!(
        after_second["first_seen_run_id"].as_str(),
        Some("run-a"),
        "a re-detected alert's first_seen_run_id must be carried forward unchanged from the \
         first bake, got: {after_second}"
    );
    assert_eq!(
        after_second["last_seen_run_id"].as_str(),
        Some("run-b"),
        "a re-detected alert's last_seen_run_id must advance to the current write's run_id, \
         got: {after_second}"
    );
}
