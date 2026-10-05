//! Regression test for INT-03/D-14: the OCOD owner-key merge only reaches the graph if the
//! superseded events are purged first.
//!
//! Root cause (28-03): `owner_key`'s Overseas branch included the jurisdiction string even when
//! the owner carried a genuine company number, so one overseas company appeared once per country
//! spelling. EAUBRINK FARM GMBH is the live example — `GB-OCOD:germany-fc025391` and
//! `GB-OCOD:hamburg-fc025391`, two nodes for one company holding one company number.
//!
//! 28-03 fixed the key. That fix alone changes nothing in the live graph, because `graph_events`
//! is append-only: re-running the adapter emits a third, correctly-keyed node ALONGSIDE the two
//! fragmented ones. The merge is only real once the superseded events are retired first — which
//! is what `purge-ocod-owners` (D-14) does.
//!
//! This test drives that whole cycle through the real `Projector`: seed the two pre-fix
//! country-variant nodes and their ownership edges, run the D-14 scan and delete, re-emit the
//! corrected single-key node with both edges re-pointed at it, project, and assert exactly one
//! `offshore_entity` row survives with both properties resolving to it.
//!
//! It FAILS without the purge step (the pre-fix nodes survive the re-emit and project to three
//! rows, which is precisely the defect D-14 exists to prevent) — `merge_without_purge_leaves_...`
//! below asserts that failure mode directly, so the purge is proven load-bearing rather than
//! assumed.
//!
//! Requires: URBAN_DB_TESTS=1 and a running SurrealDB on localhost:8000 (root/root).

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_cli::commands::purge::{
    delete_stale_ocod_events, scan_stale_ocod_owner_events,
};
use urban_integrity_ingestor::projector::{Projector, RawSqlConfig};
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, OwnsPropertyData};
use urban_integrity_schema::types::node::{NodeWeight, OffshoreEntityData, PropertyData};

/// The two pre-fix country-variant keys for one real company (28-03-MERGE-SET-REVIEW.md).
const OLD_KEY_GERMANY: &str = "GB-OCOD:germany-fc025391";
const OLD_KEY_HAMBURG: &str = "GB-OCOD:hamburg-fc025391";
/// The post-fix key D-09 produces once the jurisdiction term is dropped.
const NEW_KEY: &str = "GB-OCOD:fc025391";
const OWNER_NAME: &str = "EAUBRINK FARM GMBH";
const PROPERTY_A: &str = "property:FC025391_AGL111111";
const PROPERTY_B: &str = "property:FC025391_AGL222222";

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

async fn make_db() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect_with_prefix("test_ocodmerge", "main").await
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

async fn seed_owns_property_edge(db: &Surreal<Any>, event_id: &str, src_id: &str, dst_id: &str) {
    let edge = EdgeWeight::OwnsProperty {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: OwnsPropertyData {
            price_paid: None,
            tenure: Some("Freehold".to_string()),
        },
    };
    let weight_json = serde_json::to_string(&edge).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed owns_property edge");
}

fn owner_weight(jurisdiction: Option<&str>) -> NodeWeight {
    NodeWeight::OffshoreEntity(OffshoreEntityData {
        name: OWNER_NAME.to_string(),
        jurisdiction: jurisdiction.map(str::to_string),
        icij_node_id: None,
    })
}

fn property_weight() -> NodeWeight {
    NodeWeight::Property(PropertyData {
        title_number: Some("AGL111111".to_string()),
        tenure: Some("Freehold".to_string()),
        price_paid: None,
        company_number: Some("FC025391".to_string()),
        country_incorporated: Some("Germany".to_string()),
    })
}

/// Seed the pre-fix world: two country-variant owner nodes, one property each, one edge each.
async fn seed_pre_fix_world(db: &Surreal<Any>) {
    seed_node(
        db,
        "node-old-germany",
        OLD_KEY_GERMANY,
        &owner_weight(Some("Germany")),
    )
    .await;
    seed_node(
        db,
        "node-old-hamburg",
        OLD_KEY_HAMBURG,
        &owner_weight(Some("Hamburg")),
    )
    .await;
    seed_node(db, "node-prop-a", PROPERTY_A, &property_weight()).await;
    seed_node(db, "node-prop-b", PROPERTY_B, &property_weight()).await;
    seed_owns_property_edge(db, "edge-old-a", OLD_KEY_GERMANY, PROPERTY_A).await;
    seed_owns_property_edge(db, "edge-old-b", OLD_KEY_HAMBURG, PROPERTY_B).await;
}

/// Seed what a post-fix `enrich --source land_registry_ocod` re-run emits: one owner node under
/// the corrected key, and BOTH ownership edges sourced from it.
async fn seed_post_fix_re_emit(db: &Surreal<Any>) {
    seed_node(
        db,
        "node-new-merged",
        NEW_KEY,
        &owner_weight(Some("Germany")),
    )
    .await;
    seed_node(db, "node-prop-a-again", PROPERTY_A, &property_weight()).await;
    seed_node(db, "node-prop-b-again", PROPERTY_B, &property_weight()).await;
    seed_owns_property_edge(db, "edge-new-a", NEW_KEY, PROPERTY_A).await;
    seed_owns_property_edge(db, "edge-new-b", NEW_KEY, PROPERTY_B).await;
}

async fn run_projector(db: &urban_integrity_test_support::TestNs) {
    let raw = RawSqlConfig {
        base_url: "http://localhost:8000".to_string(),
        namespace: db.ns().to_string(),
        database: "main".to_string(),
        auth: Some(("root".to_string(), "root".to_string())),
    };
    Projector::new(db.arc(), raw)
        .run()
        .await
        .expect("projection run failed");
}

async fn offshore_rows_named(db: &Surreal<Any>, name: &str) -> Vec<serde_json::Value> {
    db.query("SELECT entity_id, name, jurisdiction FROM offshore_entity WHERE name = $n ORDER BY entity_id")
        .bind(("n", name.to_string()))
        .await
        .expect("offshore_entity query failed")
        .take(0)
        .expect("offshore_entity decode failed")
}

/// The full D-14 cycle: seed pre-fix → purge → re-emit corrected → project → one node.
#[tokio::test]
async fn test_purge_then_re_emit_merges_eaubrink_to_one_offshore_entity() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_db().await;
    seed_pre_fix_world(&db).await;

    // D-14 purge, driven through the shipped scan + delete layer (no CLI confirmation I/O).
    let scan = scan_stale_ocod_owner_events(&db)
        .await
        .expect("stale OCOD scan failed");
    assert_eq!(
        scan.stale_nodes.len(),
        2,
        "both pre-fix country-variant owner nodes must be in scope"
    );
    assert_eq!(
        scan.stale_edges.len(),
        2,
        "every GB-OCOD:-sourced OwnsProperty edge must be in scope, not just the merged subset"
    );
    let ids = scan.confirmation_ids();
    let removed = delete_stale_ocod_events(&db, &ids)
        .await
        .expect("D-14 delete failed");
    assert_eq!(
        removed,
        ids.len(),
        "the delete layer must remove exactly what it was sent (D-05/D-06)"
    );

    seed_post_fix_re_emit(&db).await;
    run_projector(&db).await;

    let rows = offshore_rows_named(&db, OWNER_NAME).await;
    assert_eq!(
        rows.len(),
        1,
        "EAUBRINK must resolve to exactly ONE offshore_entity after purge + re-emit; got {rows:?}"
    );
    let entity_id = rows[0]["entity_id"].as_str().unwrap_or_default();
    assert_eq!(
        entity_id, NEW_KEY,
        "the surviving node must carry the corrected number-only key"
    );

    // Both properties must still resolve to that one owner — a merge that loses ownership
    // edges is not a merge, it is data loss.
    let out_ids: Vec<serde_json::Value> = db
        .query(
            "SELECT type::string(in) AS in_str, type::string(out) AS out_str \
             FROM owns_property ORDER BY out_str",
        )
        .await
        .expect("owns_property query failed")
        .take(0)
        .expect("owns_property decode failed");
    assert_eq!(
        out_ids.len(),
        2,
        "both ownership edges must survive the cycle exactly once each; got {out_ids:?}"
    );
    for row in &out_ids {
        let in_str = row["in_str"].as_str().unwrap_or_default();
        assert!(
            in_str.contains(NEW_KEY),
            "every surviving ownership edge must be sourced from the merged owner; got {row:?}"
        );
        assert!(
            !in_str.contains("germany-") && !in_str.contains("hamburg-"),
            "no ownership edge may still point at a purged country-variant key; got {row:?}"
        );
    }
}

/// The purge is load-bearing: WITHOUT it, the same corrected re-emit leaves three nodes, not one.
/// This is the defect D-14 exists to prevent, asserted directly so the purge step can never be
/// quietly dropped from the live runbook on the assumption that 28-03's key fix suffices.
#[tokio::test]
async fn test_re_emit_without_purge_leaves_the_old_nodes_behind() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_db().await;
    seed_pre_fix_world(&db).await;
    // No purge here — straight to the corrected re-emit.
    seed_post_fix_re_emit(&db).await;
    run_projector(&db).await;

    let rows = offshore_rows_named(&db, OWNER_NAME).await;
    assert_eq!(
        rows.len(),
        3,
        "without the D-14 purge, append-only graph_events leaves the two pre-fix nodes beside \
         the corrected one — the key fix alone does NOT merge anything; got {rows:?}"
    );
}
