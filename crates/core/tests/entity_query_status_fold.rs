//! DB-gated regression test for the **query** half of the status-transition fix
//! (L5). Requires a live SurrealDB (v3, port 8000, root/root) and
//! `URBAN_DB_TESTS=1`; skips cleanly otherwise.
//!
//! `replay_from_surreal` folds `NodeUpdated` over `NodeAdded` (see
//! `replay_status_transition.rs`) and so does the projector, but every read path in
//! `persistence::queries` filtered on `kind = 'NodeAdded'` alone. The result was two
//! disagreeing views of the same graph: the detector and the projected `entity` table
//! saw `dissolved`, while `entity --ch-number` and the GraphQL entity resolvers
//! reported `Active`.
//!
//! Measured live on `an_anglesey_aebaa785` (2026-08-16) before the fix: 580 terminal
//! `NodeUpdated` events (579 Dissolved + 1 InLiquidation) were invisible to the CLI.
//! `entity --ch-number 04826936` printed `"status": {"type": "Active"}` for a company
//! whose event log and projected row both said dissolved.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::{
    query_entity_by_ch_number, query_entity_by_id, query_entity_by_name,
};
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

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

fn entity(name: &str, ch: &str, status: EntityStatus) -> NodeWeight {
    NodeWeight::Entity(EntityData {
        name: name.into(),
        company_number: Some(ch.into()),
        status,
        incorporation_date: Some("2003-07-09".parse().unwrap()),
    })
}

/// Seed one entity born Active and later dissolved. Returns its `EntityId` string.
///
/// The `NodeUpdated` row is written FIRST so the fix cannot pass by accident of
/// insertion order: `id` order and `timestamp` order are inverted here, exactly the
/// inversion measured across ~40% of the live log.
async fn seed_dissolved_entity(db: &Surreal<Any>, ch: &str) -> String {
    let entity_id = format!("GB-COH:{ch}");
    let dissolved = entity(
        "Blind Makers Ltd",
        ch,
        EntityStatus::Dissolved("2026-07-28T00:00:00Z".parse().unwrap()),
    );
    seed_node_event(db, "NodeUpdated", &entity_id, 1_785_236_513_911, &dissolved).await;

    let active = entity("Blind Makers Ltd", ch, EntityStatus::Active);
    seed_node_event(db, "NodeAdded", &entity_id, 1_784_796_980_595, &active).await;

    entity_id
}

#[tokio::test]
async fn test_query_entity_by_id_folds_nodeupdated_status() {
    if !db_tests_enabled() {
        eprintln!("SKIP: test_query_entity_by_id_folds_nodeupdated_status (set URBAN_DB_TESTS=1)");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect("main").await;
    let entity_id = seed_dissolved_entity(&db, "QFOLD001").await;

    let profile = query_entity_by_id(&db, &entity_id)
        .await
        .expect("query_entity_by_id failed")
        .expect("entity must be found");

    assert!(
        matches!(profile.status, EntityStatus::Dissolved(_)),
        "query_entity_by_id must fold NodeUpdated: expected Dissolved, got {:?}",
        profile.status
    );
}

#[tokio::test]
async fn test_query_entity_by_ch_number_folds_nodeupdated_status() {
    if !db_tests_enabled() {
        eprintln!("SKIP: test_query_entity_by_ch_number_folds_nodeupdated_status");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect("main").await;
    seed_dissolved_entity(&db, "QFOLD002").await;

    let profile = query_entity_by_ch_number(&db, "QFOLD002")
        .await
        .expect("query_entity_by_ch_number failed")
        .expect("entity must be found");

    assert!(
        matches!(profile.status, EntityStatus::Dissolved(_)),
        "query_entity_by_ch_number must fold NodeUpdated: expected Dissolved, got {:?}",
        profile.status
    );
}

#[tokio::test]
async fn test_query_entity_by_name_folds_nodeupdated_status() {
    if !db_tests_enabled() {
        eprintln!("SKIP: test_query_entity_by_name_folds_nodeupdated_status");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect("main").await;
    seed_dissolved_entity(&db, "QFOLD003").await;

    let profiles = query_entity_by_name(&db, "Blind Makers")
        .await
        .expect("query_entity_by_name failed");

    assert_eq!(
        profiles.len(),
        1,
        "the two events for one entity must fold to ONE profile, got {}",
        profiles.len()
    );
    assert!(
        matches!(profiles[0].status, EntityStatus::Dissolved(_)),
        "query_entity_by_name must fold NodeUpdated: expected Dissolved, got {:?}",
        profiles[0].status
    );
}
