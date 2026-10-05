//! DB-gated regression test for the replay half of the status-transition fix
//! (Surface B). Requires a live SurrealDB (v3, port 8000, root/root) and
//! `URBAN_DB_TESTS=1`; skips cleanly otherwise.
//!
//! `replay_from_surreal` originally read only `NodeAdded` events, so a persisted
//! `NodeUpdated` (e.g. an `active→dissolved` transition written by the
//! coordinator's UpsertNode path) was ignored on restart and the node reverted to
//! its birth weight. The fix folds `NodeUpdated` over `NodeAdded` by highest
//! timestamp. This test seeds both events for one entity and asserts that replay
//! reconstructs the Dissolved state, not the original Active one.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_core::persistence::{
    count_graph_events, fetch_events_paged, replay_from_surreal,
};
use urban_integrity_schema::types::{
    id::EntityId,
    node::{EntityData, EntityStatus, NodeWeight},
};

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

async fn make_test_db() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect("main").await
}

/// Seed one graph_events row (kind = NodeAdded | NodeUpdated) for an Entity.
async fn seed_node_event(
    db: &Surreal<Any>,
    kind: &str,
    entity_id: &str,
    timestamp: u64,
    weight: &NodeWeight,
) {
    let weight_json = serde_json::to_string(weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("{kind}-{entity_id}-{timestamp}"),
                "timestamp": timestamp,
                "kind": kind,
                "payload": { "entity_id": entity_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed node event");
}

#[tokio::test]
async fn test_replay_folds_nodeupdated_over_nodeadded() {
    if !db_tests_enabled() {
        eprintln!("SKIP: test_replay_folds_nodeupdated_over_nodeadded (set URBAN_DB_TESTS=1)");
        return;
    }

    let db = make_test_db().await;
    let entity_id = "GB-COH:REPLAY0001";

    // Birth: Active at t0.
    let active = NodeWeight::Entity(EntityData {
        name: "Ghost Corp".into(),
        company_number: Some("REPLAY0001".into()),
        status: EntityStatus::Active,
        incorporation_date: None,
    });
    seed_node_event(&db, "NodeAdded", entity_id, 1_700_000_000_000, &active).await;

    // Transition: the coordinator persists the FULL merged weight as NodeUpdated
    // at a later timestamp (Dissolved, CH number preserved by the merge).
    let dissolution = chrono::DateTime::parse_from_rfc3339("2025-04-15T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let dissolved = NodeWeight::Entity(EntityData {
        name: "Ghost Corp".into(),
        company_number: Some("REPLAY0001".into()),
        status: EntityStatus::Dissolved(dissolution),
        incorporation_date: None,
    });
    seed_node_event(&db, "NodeUpdated", entity_id, 1_700_000_001_000, &dissolved).await;

    // Replay into a fresh in-memory graph.
    let storage = GraphStorage::new();
    let replayed = replay_from_surreal(&db, &storage)
        .await
        .expect("replay failed");
    assert!(
        replayed >= 1,
        "replay should have loaded at least the entity node"
    );

    // The reconstructed node must reflect the Dissolved transition, not the birth.
    let weight = storage
        .node_map
        .get(&EntityId(entity_id.to_string()))
        .map(|r| r.1.clone())
        .expect("entity must exist after replay");
    match weight {
        NodeWeight::Entity(e) => {
            assert!(
                matches!(e.status, EntityStatus::Dissolved(_)),
                "replay must fold NodeUpdated: expected Dissolved, got {:?}",
                e.status
            );
            assert_eq!(
                e.company_number.as_deref(),
                Some("REPLAY0001"),
                "merged weight persisted in NodeUpdated must retain the CH number"
            );
        }
        other => panic!("expected Entity node, got {other:?}"),
    }
}

/// Regression for the unbounded-replay-query bug: a single `SELECT ... FROM graph_events`
/// returned every row of a kind in one response, and on a populated graph the SDK's
/// transport failed on that response — replay silently degraded to an empty graph. The fix
/// paginates. This seeds more rows than one page and asserts pagination returns ALL of
/// them across multiple pages (page size 100 over 250 rows → 3 pages).
#[tokio::test]
async fn test_paginated_replay_loads_all_rows_across_pages() {
    if !db_tests_enabled() {
        eprintln!("SKIP: test_paginated_replay_loads_all_rows_across_pages (set URBAN_DB_TESTS=1)");
        return;
    }

    let db = make_test_db().await;
    const N: usize = 250;

    for i in 0..N {
        let w = NodeWeight::Entity(EntityData {
            name: format!("Co {i}"),
            company_number: Some(format!("PAGE{i:06}")),
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        seed_node_event(
            &db,
            "NodeAdded",
            &format!("GB-COH:PAGE{i:06}"),
            1_700_000_000_000 + i as u64,
            &w,
        )
        .await;
    }

    // Direct pagination check: a page size smaller than N must still return every row.
    let rows = fetch_events_paged(&db, "NodeAdded", "id, payload, timestamp", 100)
        .await
        .expect("paged fetch failed");
    assert_eq!(
        rows.len(),
        N,
        "pagination must load every row across pages, not just the first page"
    );

    // count helper agrees.
    assert_eq!(count_graph_events(&db).await.expect("count failed"), N);

    // End-to-end: replay reconstructs all N distinct nodes.
    let storage = GraphStorage::new();
    let replayed = replay_from_surreal(&db, &storage)
        .await
        .expect("replay failed");
    assert_eq!(
        replayed, N,
        "replay must load all {N} nodes, not a truncated page"
    );
}
