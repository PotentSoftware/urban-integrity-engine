//! POL-02 regression: a councillor's declared company interest must reach the graph.
//!
//! History (the POL-02 directs-relate-rejected investigation notes): the council-registers adapter
//! emitted this as a `Directs` edge from a `political_actor` node. `directs` is defined
//! `FROM entity | officer TO entity | officer`, so SurrealDB refused the relate outright —
//! `Expected record<entity|officer> but found political_actor:pa1` — and POL-02's edge had never
//! once reached the graph. It went unnoticed because the adapter has never been enabled, so its
//! zero output was indistinguishable from "no declarations found".
//!
//! The fix gives declared interests their own relation rather than widening `directs`. The
//! distinction is load-bearing: "is a director of" is a Companies House fact, while "declared an
//! interest in" is a self-reported disclosure. `charity_crossover.rs` scans `graph_events` for
//! `Directs` edges with no source-kind filter and reports "Officer X directs contract-holder AND
//! is trustee of charity", so a councillor's declaration arriving as a `Directs` edge would be
//! reported as a directorship inside a conflict-of-interest finding.
//!
//! This test is the class of assertion whose absence let the original defect ship: it proves the
//! edge an adapter emits actually materialises in its relation table, rather than stopping at
//! "the adapter wrote an identity_link".
//!
//! Requires: URBAN_DB_TESTS=1 and a running SurrealDB on localhost:8000 (root/root).

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_ingestor::projector::{Projector, RawSqlConfig};
use urban_integrity_schema::types::edge::{DeclaresInterestData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::node::{
    EntityData, EntityStatus, NodeWeight, PoliticalActorData,
};

const COUNCILLOR_ID: &str = "political_actor:cllr-jane-smith";
const COMPANY_ID: &str = "GB-COH:99887766";

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

async fn seed_node(db: &Surreal<Any>, event_id: &str, entity_id: &str, weight: &NodeWeight) {
    let weight_json = serde_json::to_string(weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": { "entity_id": entity_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed node");
}

async fn seed_declares_interest(db: &Surreal<Any>, src_id: &str, dst_id: &str) {
    let edge = EdgeWeight::DeclaresInterest {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: DeclaresInterestData {
            council: Some("W06000001".to_string()),
            category: Some("Employment, office, trade".to_string()),
        },
    };
    let weight_json = serde_json::to_string(&edge).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("edge-declares-{src_id}-{dst_id}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed declares_interest edge");
}

/// A declared interest projects into exactly one `declares_interest` row, councillor → company.
#[tokio::test]
async fn test_declared_interest_materialises_as_a_relation_row() {
    if !db_tests_enabled() {
        return;
    }
    let db =
        urban_integrity_test_support::TestNs::connect_with_prefix("test_declares", "main").await;

    seed_node(
        &db,
        "node-cllr",
        COUNCILLOR_ID,
        &NodeWeight::PoliticalActor(PoliticalActorData {
            name: "Jane Smith".to_string(),
            party: None,
            council_name: Some("W06000001".to_string()),
        }),
    )
    .await;
    seed_node(
        &db,
        "node-company",
        COMPANY_ID,
        &NodeWeight::Entity(EntityData {
            name: "DECLARED INTERESTS LTD".to_string(),
            company_number: Some("99887766".to_string()),
            status: EntityStatus::Active,
            incorporation_date: None,
        }),
    )
    .await;
    seed_declares_interest(&db, COUNCILLOR_ID, COMPANY_ID).await;

    let raw = RawSqlConfig {
        base_url: "http://localhost:8000".to_string(),
        namespace: db.ns().to_string(),
        database: "main".to_string(),
        auth: Some(("root".to_string(), "root".to_string())),
    };
    let stats = Projector::new(db.arc(), raw)
        .run()
        .await
        .expect("projection run failed");

    let rows: Vec<serde_json::Value> = db
        .query("SELECT type::string(in) AS in_str, type::string(out) AS out_str FROM declares_interest")
        .await
        .expect("declares_interest query failed")
        .take(0)
        .expect("declares_interest decode failed");

    assert_eq!(
        rows.len(),
        1,
        "the declared interest must materialise as exactly one declares_interest row; \
         projector stats: related={} skipped={}",
        stats.edges_related,
        stats.edges_skipped,
    );
    let row = &rows[0];
    assert!(
        row["in_str"]
            .as_str()
            .unwrap_or("")
            .contains("cllr-jane-smith"),
        "IN endpoint must be the councillor's political_actor node; got {row:?}"
    );
    assert!(
        row["out_str"]
            .as_str()
            .unwrap_or("")
            .contains("GB-COH:99887766"),
        "OUT endpoint must be the declared company entity; got {row:?}"
    );

    // The declaration must NOT appear as a directorship. charity_crossover scans Directs edges
    // with no source-kind filter and would report it as "Officer X directs contract-holder".
    let directs: Vec<serde_json::Value> = db
        .query("SELECT count() FROM directs GROUP ALL")
        .await
        .expect("directs count failed")
        .take(0)
        .expect("directs decode failed");
    let directs_count = directs
        .first()
        .and_then(|r| r["count"].as_i64())
        .unwrap_or(0);
    assert_eq!(
        directs_count, 0,
        "a declared interest must never land in `directs` — that would assert a directorship \
         the councillor does not hold"
    );
}
