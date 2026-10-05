//! Regression test for the `donates_to` projection skip (debug session
//! `donates-to-projection-skip`).
//!
//! Root cause: the Electoral Commission adapter emitted DonatesTo edges with
//!   1. `src` in SurrealDB *record-id* form (`entity:\`GB-COH:NNN\``) instead of
//!      the bare `entity_id` (`GB-COH:NNN`) the projector keys its entity_table
//!      by — so the projector logged "unknown src entity" and skipped the edge;
//!   2. `dst = party:<slug>` with no backing node, while the schema is
//!      `donates_to TYPE RELATION FROM entity TO political_actor` — so even a
//!      found src had no valid OUT endpoint to RELATE to.
//!
//! Secondary defect (read-path name resolution): baking `political_actor:` into
//! the recipient id made the projector compose a DOUBLED
//! `political_actor:`political_actor:<slug>`` OUT endpoint, so the entity read
//! path rendered the raw id instead of the party display name. The corrected
//! recipient id is the BARE-DOMAIN form `PA:<party-slug>` (matching the live
//! political_actor convention); the projector then yields a single clean
//! `political_actor:`PA:<slug>`` and the read path resolves the name.
//!
//! This test drives the *corrected* event shape (bare entity_id src,
//! `PA:<slug>` recipient + its PoliticalActor NodeAdded) through the real
//! Projector and asserts (a) a `donates_to` row materialises with a single clean
//! `political_actor:`PA:<slug>`` OUT endpoint (no doubled prefix), and (b) the
//! recipient display name resolves via the same strip the read path uses. It
//! FAILS against the pre-fix adapter output (party:/record-id forms) and against
//! the doubled-prefix interim form, and PASSES after the fix.
//!
//! Requires: URBAN_DB_TESTS=1 and a running SurrealDB on localhost:8000 (root/root).

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_ingestor::projector::{Projector, RawSqlConfig};
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::node::{
    EntityData, EntityStatus, NodeWeight, PoliticalActorData,
};

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
    urban_integrity_test_support::TestNs::connect_with_prefix("test_donates", "main").await
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

async fn seed_donates_to_edge(db: &Surreal<Any>, src_id: &str, dst_id: &str) {
    let edge = EdgeWeight::DonatesTo {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
    };
    let weight_json = serde_json::to_string(&edge).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("edge-donates-{src_id}-{dst_id}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed donates_to edge");
}

/// End-to-end: a party recipient (PoliticalActor node + bare-id DonatesTo edge)
/// must project into exactly one `donates_to` relation row.
#[tokio::test]
async fn test_project_donates_to_party_recipient_materialises_row() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_db().await;

    // Donor company — bare entity_id, exactly as the corrected EC adapter emits.
    let donor_id = "GB-COH:99990001";
    seed_node(
        &db,
        donor_id,
        &NodeWeight::Entity(EntityData {
            name: "Test Donor Ltd".to_string(),
            company_number: Some("99990001".to_string()),
            status: EntityStatus::Active,
            incorporation_date: None,
        }),
    )
    .await;

    // Recipient party — modelled as a PoliticalActor node with a BARE-DOMAIN id
    // (`PA:<slug>`), exactly as the corrected EC adapter emits. The projector
    // prepends the table name to compose a single clean `political_actor:`PA:<slug>``.
    let party_id = "PA:test-party";
    let party_display = "Test Party";
    seed_node(
        &db,
        party_id,
        &NodeWeight::PoliticalActor(PoliticalActorData {
            name: party_display.to_string(),
            party: Some(party_display.to_string()),
            council_name: None,
        }),
    )
    .await;

    // The DonatesTo edge: bare src entity_id -> bare PA: recipient id.
    seed_donates_to_edge(&db, donor_id, party_id).await;

    // Run the real projector against this namespace via the raw HTTP SQL path.
    let raw = RawSqlConfig {
        base_url: "http://localhost:8000".to_string(),
        namespace: db.ns().to_string(),
        database: "main".to_string(),
        auth: Some(("root".to_string(), "root".to_string())),
    };
    let projector = Projector::new(db.arc(), raw);
    let stats = projector.run().await.expect("projection run failed");

    // The donor src must be resolved (no "unknown src entity" skip) and the edge
    // related to the political_actor recipient.
    let count: Option<i64> = db
        .query("SELECT count() AS c FROM donates_to GROUP ALL")
        .await
        .unwrap()
        .take((0, "c"))
        .unwrap();

    assert_eq!(
        count,
        Some(1),
        "exactly one donates_to row must materialise after projection; \
         projector stats: related={} skipped={}",
        stats.edges_related,
        stats.edges_skipped,
    );

    // Verify the relation endpoints resolved to the expected records.
    let endpoints: Vec<serde_json::Value> = db
        .query("SELECT type::string(in) AS in_str, type::string(out) AS out_str FROM donates_to")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert_eq!(endpoints.len(), 1, "one donates_to relation expected");
    let row = &endpoints[0];
    assert!(
        row["in_str"]
            .as_str()
            .unwrap_or("")
            .contains("GB-COH:99990001"),
        "donates_to IN endpoint must be the donor entity; got {row:?}"
    );
    let out_str = row["out_str"].as_str().unwrap_or("");
    assert!(
        out_str.contains("PA:test-party"),
        "donates_to OUT endpoint must be the political_actor recipient; got {row:?}"
    );

    // The OUT endpoint must carry a SINGLE clean `political_actor:` prefix — the
    // doubled `political_actor:`political_actor:...`` form is the secondary defect
    // this fix closes (it breaks read-path name resolution).
    assert!(
        !out_str.contains("political_actor:`political_actor:")
            && !out_str.contains("political_actor:political_actor:"),
        "donates_to OUT endpoint must NOT have a doubled political_actor: prefix; got {out_str:?}"
    );

    // The recipient display name must resolve. Mirror the entity read path
    // (queries.rs): strip exactly one `political_actor:` from the stored OUT id,
    // trim backticks, then look the bare id up against the PoliticalActor
    // NodeAdded events. The corrected `PA:<slug>` form must round-trip to the
    // human-readable party name, not the raw id.
    let bare_id = out_str
        .trim_matches(|c: char| c == '\'' || c == '"')
        .strip_prefix("political_actor:")
        .unwrap_or(out_str)
        .trim_matches('`')
        .to_owned();
    assert_eq!(
        bare_id, "PA:test-party",
        "read-path strip must recover the bare PA: id (one prefix only); got {bare_id:?}"
    );

    // `kind` is deliberately NOT in the WHERE clause: pairing the indexed equality with an
    // unindexed CONTAINS is the SurrealDB 3.x row-losing shape (see
    // `crates/core/tests/query_shape_guard.rs`, rule 1). Filtered in Rust below instead.
    let pol_events: Vec<serde_json::Value> = db
        .query(
            "SELECT * FROM graph_events \
             WHERE payload.weight_json CONTAINS '\"type\":\"PoliticalActor\"' LIMIT 100",
        )
        .await
        .unwrap()
        .take(0)
        .unwrap();
    let resolved_name = pol_events.iter().find_map(|ev| {
        if ev["kind"].as_str() != Some("NodeAdded") {
            return None;
        }
        let eid = ev["payload"]["entity_id"].as_str()?;
        let key = eid.strip_prefix("political_actor:").unwrap_or(eid);
        if key == bare_id {
            let wj = ev["payload"]["weight_json"].as_str()?;
            if let Ok(NodeWeight::PoliticalActor(pd)) = serde_json::from_str::<NodeWeight>(wj) {
                return Some(pd.name);
            }
        }
        None
    });
    assert_eq!(
        resolved_name.as_deref(),
        Some(party_display),
        "recipient must resolve to the display name (not the raw id); \
         bare_id={bare_id:?}"
    );
}
