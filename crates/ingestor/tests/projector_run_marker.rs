//! `projection_runs:current` — the marker that tells a partial projection from a finished
//! one (Open Item 30).
//!
//! `project` is destructive-then-rebuild. Before this marker existed, a run that died
//! partway left tables that look exactly like a finished projection of a smaller graph:
//! no error, no log line, nothing in the database saying "this is incomplete". On
//! 2026-08-11 a run was SIGTERMed 12.6% into the edge stream, and the resulting five empty
//! and four partial relation tables were investigated as nine separate per-table bugs for
//! four days. See the projection-truncation investigation notes.
//!
//! `finished_at_ms` is stamped only after the final flush and the skip-ratio gate, so it
//! stays null for any run that terminated early — SIGKILL included, which no in-process
//! signal handler could catch.
//!
//! Requires: URBAN_DB_TESTS=1 and a running SurrealDB on localhost:8000 (root/root).

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_ingestor::projector::{Projector, RawSqlConfig};
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

async fn seed_entity(db: &Surreal<Any>, entity_id: &str) {
    let weight = NodeWeight::Entity(EntityData {
        name: format!("Marker Test {entity_id}"),
        company_number: Some(entity_id.trim_start_matches("GB-COH:").to_string()),
        status: EntityStatus::Active,
        incorporation_date: None,
    });
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("node-{entity_id}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": entity_id,
                    "weight_json": serde_json::to_string(&weight).unwrap(),
                },
            }),
        ))
        .await
        .expect("failed to seed entity");
}

fn projector(db: &urban_integrity_test_support::TestNs) -> Projector {
    Projector::new(
        db.arc(),
        RawSqlConfig {
            base_url: "http://localhost:8000".to_string(),
            namespace: db.ns().to_string(),
            database: "main".to_string(),
            auth: Some(("root".to_string(), "root".to_string())),
        },
    )
}

async fn marker(db: &Surreal<Any>) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = db
        .query("SELECT * FROM projection_runs:current")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    rows.into_iter()
        .next()
        .expect("projection_runs:current must exist after a run")
}

/// A completed run stamps `finished_at_ms` and the stats it actually achieved.
#[tokio::test]
async fn completed_run_stamps_the_marker() {
    if !db_tests_enabled() {
        return;
    }
    let db =
        urban_integrity_test_support::TestNs::connect_with_prefix("test_projmarker", "main").await;
    seed_entity(&db, "GB-COH:88880001").await;
    seed_entity(&db, "GB-COH:88880002").await;

    let stats = projector(&db).run().await.expect("projection run failed");

    let m = marker(&db).await;
    assert!(
        m["finished_at_ms"].as_i64().is_some(),
        "a completed run must stamp finished_at_ms; got {m}"
    );
    assert!(
        m["started_at_ms"].as_i64().unwrap_or(0) <= m["finished_at_ms"].as_i64().unwrap(),
        "finished_at_ms must not precede started_at_ms; got {m}"
    );
    assert_eq!(
        m["nodes_upserted"].as_i64(),
        Some(stats.nodes_upserted as i64),
        "the marker must carry the run's real node count; got {m}"
    );
    assert_eq!(
        m["edges_related"].as_i64(),
        Some(stats.edges_related as i64),
        "the marker must carry the run's real edge count; got {m}"
    );
}

/// A marker left unfinished by an earlier run is replaced, not inherited — and the new
/// run's own marker is only complete once it is.
#[tokio::test]
async fn an_unfinished_marker_is_replaced_by_the_next_run() {
    if !db_tests_enabled() {
        return;
    }
    let db =
        urban_integrity_test_support::TestNs::connect_with_prefix("test_projmarker2", "main").await;
    seed_entity(&db, "GB-COH:88880003").await;

    // Exactly what a killed run leaves behind: started, never stamped.
    db.query(
        "UPSERT projection_runs:current CONTENT {
             started_at_ms: 1, finished_at_ms: NONE,
             nodes_upserted: 0, edges_related: 0, edges_skipped: 0
         }",
    )
    .await
    .unwrap();

    let stale = marker(&db).await;
    assert!(
        stale["finished_at_ms"].is_null(),
        "precondition: the seeded marker must be unfinished; got {stale}"
    );

    projector(&db).run().await.expect("projection run failed");

    let m = marker(&db).await;
    assert!(
        m["finished_at_ms"].as_i64().is_some(),
        "the new run must stamp its own completion; got {m}"
    );
    assert!(
        m["started_at_ms"].as_i64().unwrap_or(0) > 1,
        "the new run must replace the dead run's marker, not merge into it; got {m}"
    );
}
