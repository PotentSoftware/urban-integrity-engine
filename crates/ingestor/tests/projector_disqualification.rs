//! ENR-11 regression: a director's disqualification must reach the projected graph.
//!
//! History (the POL-02 directs-relate-rejected investigation notes): `ch_disqualified.rs` emitted
//! `DisqualifiedFrom` with `dst = disq:{resource_id}` — an id for which no `AddNode` was ever
//! sent and for which `NodeWeight` had no variant. The relation was meanwhile defined
//! `FROM officer TO entity`, so the edge was doubly unroutable: the destination node could not
//! exist, and had it existed it would have been the wrong kind. The projected
//! `disqualified_from` table was therefore permanently empty (live: 0 rows).
//!
//! The fix models the disqualification as what the source data actually describes: a CDDA court
//! order against a *person*, not a ban "from" a particular company. The CH disqualified stream
//! carries no company at all (`struct Disqualification` holds only `disqualified_from`,
//! `disqualified_until`, `case_identifier`), so pointing the edge at an entity would assert
//! something the source does not say. Instead a `Disqualification` node kind carries the order,
//! and the relation runs `FROM officer TO disqualification`.
//!
//! Requires: URBAN_DB_TESTS=1 and a running SurrealDB on localhost:8000 (root/root).

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_ingestor::projector::{Projector, RawSqlConfig};
use urban_integrity_schema::types::edge::{DisqualifiedFromData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::node::{DisqualificationData, NodeWeight, OfficerData};

const OFFICER_ID: &str = "director:john-banned";
const DISQ_ID: &str = "disq:abc123resource";
const CASE_NUMBER: &str = "CDDA-2024-00417";

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

fn ban_window() -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let from = chrono::DateTime::parse_from_rfc3339("2024-03-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let until = chrono::DateTime::parse_from_rfc3339("2031-03-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    (from, until)
}

async fn seed_disqualified_from(db: &Surreal<Any>, src_id: &str, dst_id: &str) {
    let (from, until) = ban_window();
    let edge = EdgeWeight::DisqualifiedFrom {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: Some(from),
            effective_to: Some(until),
        },
        data: DisqualifiedFromData {
            disqualified_from: Some(from),
            disqualified_until: Some(until),
            case_number: Some(CASE_NUMBER.to_string()),
        },
    };
    let weight_json = serde_json::to_string(&edge).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("edge-disq-{src_id}-{dst_id}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed disqualified_from edge");
}

/// The disqualification node projects, and the officer → disqualification edge materialises.
#[tokio::test]
async fn test_disqualification_materialises_as_node_and_relation() {
    if !db_tests_enabled() {
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_disq", "main").await;

    let (from, until) = ban_window();
    seed_node(
        &db,
        "node-officer",
        OFFICER_ID,
        &NodeWeight::Officer(OfficerData {
            name: "JOHN BANNED".to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: None,
        }),
    )
    .await;
    seed_node(
        &db,
        "node-disq",
        DISQ_ID,
        &NodeWeight::Disqualification(DisqualificationData {
            case_number: Some(CASE_NUMBER.to_string()),
            disqualified_from: Some(from),
            disqualified_until: Some(until),
        }),
    )
    .await;
    seed_disqualified_from(&db, OFFICER_ID, DISQ_ID).await;

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

    // The disqualification node must exist — without it the edge dst is unresolvable and the
    // edge is dropped as `unknown_dst`, which is exactly how this defect stayed invisible.
    let nodes: Vec<serde_json::Value> = db
        .query("SELECT entity_id, case_number FROM disqualification")
        .await
        .expect("disqualification query failed")
        .take(0)
        .expect("disqualification decode failed");
    assert_eq!(
        nodes.len(),
        1,
        "the disqualification order must project as one node row; stats: nodes={} skipped={}",
        stats.nodes_upserted,
        stats.edges_skipped,
    );
    assert_eq!(
        nodes[0]["case_number"].as_str(),
        Some(CASE_NUMBER),
        "the CDDA case number must be queryable on the projected node; got {:?}",
        nodes[0]
    );

    let rows: Vec<serde_json::Value> = db
        .query(
            "SELECT type::string(in) AS in_str, type::string(out) AS out_str, \
             data.case_number AS case_number FROM disqualified_from",
        )
        .await
        .expect("disqualified_from query failed")
        .take(0)
        .expect("disqualified_from decode failed");

    assert_eq!(
        rows.len(),
        1,
        "the disqualification must materialise as exactly one disqualified_from row; \
         projector stats: related={} skipped={}",
        stats.edges_related,
        stats.edges_skipped,
    );
    let row = &rows[0];
    assert!(
        row["in_str"].as_str().unwrap_or("").contains("john-banned"),
        "IN endpoint must be the disqualified officer; got {row:?}"
    );
    assert!(
        row["out_str"]
            .as_str()
            .unwrap_or("")
            .contains("abc123resource"),
        "OUT endpoint must be the disqualification order; got {row:?}"
    );
    assert_eq!(
        row["case_number"].as_str(),
        Some(CASE_NUMBER),
        "the ban's case number must survive projection onto the edge; got {row:?}"
    );
}
