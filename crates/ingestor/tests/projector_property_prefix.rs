//! Regression test for the projector's double-prefixed `property` record ids
//! (INT-04, D-15/D-16, phase 28-02).
//!
//! Root cause: emitting adapters (`land_registry_ocod.rs`, `land_registry_ccod.rs`)
//! already prefix property `entity_id`s with `"property:"` before handing them to
//! the projector, and the projector's two independent record-id-construction sites
//! (`upsert_node`'s `$id` bind, `quote_record_id`) each prepended the table name
//! again on top — doubling it to `property:`property:xxx``. Measured live
//! 2026-08-21: 105,809 of 105,809 `property` rows were affected;
//! `offshore_entity` was confirmed clean (0 of 29,944).
//!
//! The fix is `strip_own_table_prefix(table, id)`, applied at both write sites in
//! the same commit — see `crates/ingestor/src/projector.rs`. This test drives one
//! `property:` `NodeAdded` event through the real `Projector` and asserts the
//! record id SurrealDB actually stores is single-prefixed, not doubled.
//!
//! Requires: URBAN_DB_TESTS=1 and a running SurrealDB on localhost:8000 (root/root).

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_ingestor::projector::{Projector, RawSqlConfig};
use urban_integrity_schema::types::node::{NodeWeight, PropertyData};

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Connect to a fresh, uniquely-named namespace and apply the schema.
/// Returns the connected DB handle and the namespace name (the projector's raw
/// HTTP config must target the same ns/db).
async fn make_db() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect_with_prefix("test_propprefix", "main").await
}

async fn seed_node(db: &Surreal<Any>, entity_id: &str, weight: &NodeWeight) {
    let weight_json = serde_json::to_string(weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("node-{entity_id}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": { "entity_id": entity_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed node");
}

/// A `property:` NodeAdded event, exactly as the OCOD/CCOD adapters emit it
/// pre-fix (entity_id already carrying the "property:" prefix), must project
/// into exactly one `property` row whose record id is NOT double-prefixed.
#[tokio::test]
async fn test_project_property_node_is_not_double_prefixed() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_db().await;

    let entity_id = "property:ln1234";
    seed_node(
        &db,
        entity_id,
        &NodeWeight::Property(PropertyData {
            title_number: Some("LN1234".to_string()),
            tenure: Some("Freehold".to_string()),
            price_paid: Some(350_000.0),
            company_number: None,
            country_incorporated: None,
        }),
    )
    .await;

    // Run the real projector against this namespace via the raw HTTP SQL path.
    let raw = RawSqlConfig {
        base_url: "http://localhost:8000".to_string(),
        namespace: db.ns().to_string(),
        database: "main".to_string(),
        auth: Some(("root".to_string(), "root".to_string())),
    };
    let projector = Projector::new(db.arc(), raw);
    projector.run().await.expect("projection run failed");

    let ids: Vec<serde_json::Value> = db
        .query("SELECT type::string(id) AS id_str FROM property")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert_eq!(
        ids.len(),
        1,
        "exactly one property row expected; got {ids:?}"
    );
    let id_str = ids[0]["id_str"].as_str().unwrap_or("");
    assert!(
        !id_str.contains("property:`property:") && !id_str.contains("property:property:"),
        "property record id must NOT be double-prefixed; got {id_str:?}"
    );
}
