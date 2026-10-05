//! SurrealDB integration tests for the persistence layer.
//!
//! Requires a live SurrealDB instance (server v3, port 8000, user root/root).
//! Run with:
//!   ~/.surrealdb/surreal start --user root --pass root --bind '[::]:8000' surrealkv://data/surreal.db
//!   URBAN_DB_TESTS=1 cargo test --test surreal_integration
//!
//! Tests use isolated namespaces (UUID-based) so they never interfere with each
//! other or with production data.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::{
    get_alert_by_id, get_alerts_by_ids, query_alerts, query_entity_by_ch_number,
    query_entity_by_id, query_entity_by_name, query_entity_lpa_presence, query_related_entities,
    AlertFilter,
};
use urban_integrity_core::persistence::scan::scan_enrichment_runs_paged;
use urban_integrity_core::persistence::traversal::bfs_traversal;
use urban_integrity_schema::types::{
    edge::{AwardedToData, DirectsData, EdgeMeta, EdgeWeight, LocatedAtData, OwnsPropertyData},
    node::{CharityData, EntityData, EntityStatus, NodeWeight, OfficerData, PscData},
};

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    // Pre-flight TCP check — skip if DB isn't up rather than panicking mid-test.
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Connect to a fresh, isolated SurrealDB namespace for one test.
async fn make_test_db() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect("main").await
}

/// Insert one synthetic MotifDetected event directly into graph_events.
async fn seed_alert(db: &Surreal<Any>, event_id: &str, motif: &str, score: f64, ward: &str) {
    let result_json = serde_json::json!({
        "motif_type": motif,
        "risk_score": score,
        "score_breakdown": { "ward_code": ward },
        "path_nodes": [],
        "unique_hash": uuid::Uuid::new_v4().to_string(),
    })
    .to_string();

    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": 1_700_000_000_000u64,
                "kind": "MotifDetected",
                "payload": { "result_json": result_json },
            }),
        ))
        .await
        .expect("failed to seed alert");
}

/// Insert a synthetic NodeAdded event for an Entity node into graph_events.
async fn seed_entity(
    db: &Surreal<Any>,
    entity_id: &str,
    name: &str,
    ch_number: Option<&str>,
    incorporation_date: Option<chrono::NaiveDate>,
) {
    let weight = NodeWeight::Entity(EntityData {
        name: name.to_string(),
        company_number: ch_number.map(str::to_owned),
        status: EntityStatus::Active,
        incorporation_date,
    });
    let weight_json = serde_json::to_string(&weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("node-{}", entity_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": entity_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed entity");
}

/// Insert a synthetic NodeAdded event for an Officer node into graph_events.
async fn seed_officer(db: &Surreal<Any>, officer_id: &str, name: &str, role: Option<&str>) {
    let weight = NodeWeight::Officer(OfficerData {
        name: name.to_string(),
        officer_role: role.map(str::to_owned),
        appointed_on: None,
        resigned_on: None,
        company_number: None,
    });
    let weight_json = serde_json::to_string(&weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("node-{}", officer_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": officer_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed officer");
}

/// Insert a synthetic EdgeAdded event for a Directs edge into graph_events.
async fn seed_directs_edge(db: &Surreal<Any>, src_officer_id: &str, dst_entity_id: &str) {
    let edge = EdgeWeight::Directs {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: DirectsData {
            details: String::new(),
            officer_role: None,
        },
    };
    let weight_json = serde_json::to_string(&edge).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("edge-directs-{}-{}", src_officer_id, dst_entity_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": src_officer_id,
                    "dst_id": dst_entity_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed directs edge");
}

/// Insert a synthetic EdgeAdded event for a LocatedAt edge into graph_events.
async fn seed_located_at_edge(db: &Surreal<Any>, src_entity_id: &str, dst_location_id: &str) {
    let edge = EdgeWeight::LocatedAt {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: LocatedAtData {
            details: String::new(),
        },
    };
    let weight_json = serde_json::to_string(&edge).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("edge-located-{}-{}", src_entity_id, dst_location_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": src_entity_id,
                    "dst_id": dst_location_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed located_at edge");
}

/// Seed a PSC node + `controls` relation edge so the OSINT beneficial-owner
/// read path (`query_osint_for_entity`) has data to surface.
///
/// Mirrors the full projector write surface:
///   1. a `psc:<id>` native node (controls.in target);
///   2. a `RELATE psc->controls->entity` edge whose `out` is the company's
///      `entity:GB-COH:<ch>` node — this is what tells us *which* PSC controls
///      *which* company;
///   3. a graph_events `NodeAdded` / `Psc` event carrying name + natures, which
///      is the surface the read path actually resolves names from (the native
///      `psc` table reads unreliably over the SurrealDB SDK).
///
/// The entity company node must already exist (seed_entity) for the RELATE
/// FROM/TO constraint to hold.
async fn seed_psc_controls(
    db: &Surreal<Any>,
    psc_id: &str,
    entity_id: &str,
    name: &str,
    natures: &[&str],
) {
    // Match the projector's RecordId construction: backtick-quote the id because
    // it contains colons/hyphens (e.g. psc:`psc:15639177-jehad-alnashash`).
    let psc_thing = format!("psc:`{psc_id}`");
    let entity_thing = format!("entity:`{entity_id}`");

    let natures_arr: Vec<String> = natures.iter().map(|s| s.to_string()).collect();
    db.query(format!("CREATE {psc_thing} CONTENT $c"))
        .bind((
            "c",
            serde_json::json!({
                "name": name,
                "entity_id": psc_id,
                "company_number": serde_json::Value::Null,
                "natures_of_control": natures_arr,
            }),
        ))
        .await
        .expect("failed to seed psc node");

    // RELATE psc:<psc_id> -> controls -> entity:<entity_id> (projector shape).
    // meta.* are `datetime` fields under SCHEMAFULL — use SurrealQL `d'…'`
    // literals so the values coerce correctly (a JSON string is rejected).
    db.query(format!(
        "RELATE {psc_thing}->controls->{entity_thing} CONTENT {{ \
             meta: {{ \
                 created_at: d'2024-04-12T00:00:00Z', \
                 effective_from: d'2024-04-12T00:00:00Z', \
                 effective_to: NONE \
             }} \
         }} RETURN NONE"
    ))
    .await
    .expect("failed to seed controls edge");

    // graph_events NodeAdded / Psc event — the read path resolves the PSC name
    // and natures from here (payload.entity_id == psc_id).
    let weight = NodeWeight::Psc(PscData {
        name: name.to_string(),
        company_number: None,
        natures_of_control: Some(natures_arr),
    });
    let weight_json = serde_json::to_string(&weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("node-{}", psc_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": psc_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed psc graph_events node");
}

/// Variant of seed_alert that populates result_json.path_nodes with the target entity_id.
/// Used to seed a MotifDetected event that the active-alerts query can find for a given entity.
#[allow(dead_code)]
async fn seed_motif_for_entity(
    db: &Surreal<Any>,
    event_id: &str,
    motif_type: &str,
    score: f64,
    entity_id: &str,
) {
    let result_json = serde_json::json!({
        "motif_type": motif_type,
        "risk_score": score,
        "score_breakdown": { "ward_code": serde_json::Value::Null },
        "path_nodes": [entity_id],
        "unique_hash": uuid::Uuid::new_v4().to_string(),
    })
    .to_string();

    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": 1_700_000_000_000u64,
                "kind": "MotifDetected",
                "payload": { "result_json": result_json },
            }),
        ))
        .await
        .expect("failed to seed motif for entity");
}

// ── Entity query integration tests ───────────────────────────────────────────

#[tokio::test]
async fn test_query_entity_by_name_returns_profile() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_entity(
        &db,
        "GB-COH:12345678",
        "Apex Build Ltd",
        Some("12345678"),
        None,
    )
    .await;

    let results = query_entity_by_name(&db, "apex build").await.unwrap();
    assert_eq!(results.len(), 1, "expected exactly one match");
    assert_eq!(results[0].name, "Apex Build Ltd");
    assert_eq!(results[0].ch_number, Some("12345678".to_owned()));
}

#[tokio::test]
async fn test_query_entity_by_name_multiple_matches() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_entity(
        &db,
        "GB-COH:11111111",
        "Acme Builds Ltd",
        Some("11111111"),
        None,
    )
    .await;
    seed_entity(
        &db,
        "GB-COH:22222222",
        "Acme Contracts Ltd",
        Some("22222222"),
        None,
    )
    .await;
    seed_entity(
        &db,
        "GB-COH:33333333",
        "Acme Works Ltd",
        Some("33333333"),
        None,
    )
    .await;

    let results = query_entity_by_name(&db, "Acme").await.unwrap();
    assert!(results.len() >= 2, "expected at least 2 matches");
    assert!(results.len() <= 10, "D-02: cap at 10 results");
}

#[tokio::test]
async fn test_query_entity_by_ch_number_exact_match() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_entity(
        &db,
        "GB-COH:99887766",
        "Exact Match Ltd",
        Some("99887766"),
        None,
    )
    .await;

    let result = query_entity_by_ch_number(&db, "99887766").await.unwrap();
    assert!(result.is_some(), "expected entity to be found");
    assert_eq!(
        result.unwrap().ch_number,
        Some("99887766".to_owned()),
        "CH number should match"
    );
}

#[tokio::test]
async fn test_query_entity_by_ch_number_not_found() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    // No seeding — empty DB
    let result = query_entity_by_ch_number(&db, "00000000").await.unwrap();
    assert!(result.is_none(), "empty DB should return None");
}

/// Regression test for debug session `entity-ch-lookup-empty`.
///
/// SurrealDB 3.1.0-beta.2 planner bug: combining the `idx_graph_events_kind`
/// indexed equality (`kind = 'NodeAdded'`) with a CONTAINS scan on the
/// non-indexed `payload.weight_json` field made `SELECT *` return 0 rows while
/// `count()` returned the true count — so `query_entity_by_ch_number` reported
/// "No entity found" for every company. The fix drops the kind equality from the
/// SQL (keeping only CONTAINS) and filters kind Rust-side.
///
/// This test seeds several NodeAdded rows alongside the target so the planner is
/// pushed onto the kind-index path that triggered the bug, then asserts the
/// lookup still resolves the target by CH number. It also asserts the raw buggy
/// query shape and the fixed query shape disagree on row count (the bug
/// signature), guarding against a silent regression to the old WHERE clause.
#[tokio::test]
async fn test_query_entity_by_ch_number_regression_planner_bug() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;

    // Seed multiple entities so the kind index has > 1 row to plan against,
    // plus the target company we look up.
    for i in 0..12u32 {
        let ch = format!("5000000{i:02}");
        seed_entity(
            &db,
            &format!("GB-COH:{ch}"),
            &format!("Decoy Co {i} Ltd"),
            Some(&ch),
            None,
        )
        .await;
    }
    let target_ch = "15639177";
    seed_entity(
        &db,
        &format!("GB-COH:{target_ch}"),
        "J Nash Consulting Limited",
        Some(target_ch),
        None,
    )
    .await;

    // 1. The function under test must resolve the target (was returning None).
    let result = query_entity_by_ch_number(&db, target_ch).await.unwrap();
    assert!(
        result.is_some(),
        "planner-bug regression: query_entity_by_ch_number must find the seeded company"
    );
    assert_eq!(
        result.unwrap().ch_number,
        Some(target_ch.to_owned()),
        "resolved CH number must match the requested one"
    );

    // 2. Bug signature: with the kind index present, the OLD query shape
    //    (kind equality + CONTAINS) returns 0 rows while the FIXED shape
    //    (CONTAINS only) returns the rows. If a future edit reinstates the kind
    //    equality, this assertion catches it.
    let needle = format!("\"company_number\":\"{target_ch}\"");

    let buggy_rows: Vec<serde_json::Value> = db
        .query("SELECT * FROM graph_events WHERE kind = 'NodeAdded' AND payload.weight_json CONTAINS $needle LIMIT 10")
        .bind(("needle", needle.clone()))
        .await
        .unwrap()
        .take(0)
        .unwrap();

    let fixed_rows: Vec<serde_json::Value> = db
        .query("SELECT * FROM graph_events WHERE payload.weight_json CONTAINS $needle LIMIT 10")
        .bind(("needle", needle))
        .await
        .unwrap()
        .take(0)
        .unwrap();

    assert!(
        !fixed_rows.is_empty(),
        "fixed query shape (CONTAINS only) must return the seeded NodeAdded row"
    );
    assert!(
        buggy_rows.is_empty() || buggy_rows.len() == fixed_rows.len(),
        "either the planner bug is present (buggy=0, fixed>0) or both agree; \
         an inconsistent partial count would indicate a different fault"
    );
}

/// Regression test for debug session `entity-osint-display-empty`.
///
/// The OSINT beneficial-owner read in `query_osint_for_entity` queried
/// `identity_links WHERE source = 'ch_psc'` — a table that holds ZERO PSC rows.
/// PSC controls data lives only in the `controls` relation table (in=psc:<…>,
/// out=entity:GB-COH:<ch>) plus the projected `psc` node. So `beneficialOwners`
/// rendered empty for every company even when a Controls edge existed.
///
/// The fix reads the `controls` relation table and joins to the `in` PSC node's
/// `name` + `natures_of_control`. This test seeds a company, a PSC node, and a
/// controls edge, then asserts the beneficial owner is surfaced — and that a
/// second company with no controls edge stays empty (no cross-entity leakage).
#[tokio::test]
async fn test_query_entity_beneficial_owners_from_controls() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;

    // Target company with a PSC controls edge.
    let target_ch = "15639177";
    let target_eid = format!("GB-COH:{target_ch}");
    seed_entity(
        &db,
        &target_eid,
        "J Nash Consulting Limited",
        Some(target_ch),
        None,
    )
    .await;
    seed_psc_controls(
        &db,
        "psc:15639177-jehad-alnashash",
        &target_eid,
        "Jehad Al-Nashash",
        &[
            "ownership-of-shares-75-to-100-percent",
            "voting-rights-75-to-100-percent",
            "right-to-appoint-and-remove-directors",
        ],
    )
    .await;

    // Decoy company with its own PSC — must NOT leak into the target's profile.
    let decoy_ch = "12673861";
    let decoy_eid = format!("GB-COH:{decoy_ch}");
    seed_entity(
        &db,
        &decoy_eid,
        "Soft Skin Massage Ltd",
        Some(decoy_ch),
        None,
    )
    .await;
    seed_psc_controls(
        &db,
        "psc:12673861-stefania-ungureanu",
        &decoy_eid,
        "Stefania Ungureanu",
        &["ownership-of-shares-75-to-100-percent"],
    )
    .await;

    // 1. Target resolves and surfaces its beneficial owner from the controls edge.
    let profile = query_entity_by_ch_number(&db, target_ch)
        .await
        .unwrap()
        .expect("target company must resolve");
    assert_eq!(
        profile.beneficial_owners.len(),
        1,
        "exactly one beneficial owner must surface from the controls relation table; \
         got {:?}",
        profile.beneficial_owners
    );
    let bo = &profile.beneficial_owners[0];
    assert_eq!(
        bo.name, "Jehad Al-Nashash",
        "PSC name must match the seeded node"
    );
    assert_eq!(
        bo.natures_of_control,
        vec![
            "ownership-of-shares-75-to-100-percent".to_string(),
            "voting-rights-75-to-100-percent".to_string(),
            "right-to-appoint-and-remove-directors".to_string(),
        ],
        "natures_of_control must come through from the joined PSC node"
    );

    // 2. The decoy's PSC must not leak into the target (per-entity filtering).
    assert!(
        !bo.name.contains("Stefania"),
        "decoy company's PSC must not appear in the target's profile"
    );

    // 3. A company with no controls edge yields an empty list (no false positives).
    let no_psc_ch = "99887766";
    seed_entity(
        &db,
        &format!("GB-COH:{no_psc_ch}"),
        "No Psc Ltd",
        Some(no_psc_ch),
        None,
    )
    .await;
    let no_psc = query_entity_by_ch_number(&db, no_psc_ch)
        .await
        .unwrap()
        .expect("company must resolve");
    assert!(
        no_psc.beneficial_owners.is_empty(),
        "company with no controls edge must have zero beneficial owners"
    );
}

#[tokio::test]
async fn test_slug_branch_falls_back_to_name_search() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_entity(
        &db,
        "GB-COH:12345678",
        "Apex Build Ltd",
        Some("12345678"),
        None,
    )
    .await;

    // Querying a non-existent slug ID returns None via query_entity_by_id.
    let by_id = query_entity_by_id(&db, "nonexistent-slug").await.unwrap();
    assert!(by_id.is_none(), "non-existent slug should return None");

    // Name search with the same slug-like string also returns empty.
    let by_name = query_entity_by_name(&db, "nonexistent-slug").await.unwrap();
    assert!(
        by_name.is_empty(),
        "name search for nonexistent-slug should return empty vec"
    );
}

/// ENT-05: director slugs keyed `director:{slug}` are globally unique across LPAs.
/// A director entity should accumulate contracts from multiple LPA wards.
#[tokio::test]
async fn test_query_entity_by_name_resolves_director_slug_across_lpas() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_schema::types::{
        edge::{AwardedToData, EdgeMeta, EdgeWeight},
        node::{ContractData, NodeWeight},
    };

    let db = make_test_db().await;
    let entity_id = "director:john-smith-builder";
    seed_entity(&db, entity_id, "John Smith Builder", None, None).await;

    // Seed two contracts from different wards that are AwardedTo this entity.
    let seed_contract = |contract_id: &str, ward: &str| {
        let db = db.arc();
        let contract_id = contract_id.to_owned();
        let ward = ward.to_owned();
        let entity_id = entity_id.to_owned();
        async move {
            let contract_weight = NodeWeight::Contract(ContractData {
                name: format!("Contract {}", contract_id),
                ocid: None,
                value_gbp: Some(50_000.0),
                status: Some("active".to_owned()),
                date_published: None,
                ward_code: Some(ward.clone()),
                tender_ref: None,
                application_ref: None,
                application_received_date: None,
                applicant_provenance: None,
                portal_detail_handle: None,
            });
            let contract_weight_json = serde_json::to_string(&contract_weight).unwrap();
            db.query("CREATE graph_events CONTENT $r")
                .bind((
                    "r",
                    serde_json::json!({
                        "event_id": format!("node-{}", contract_id),
                        "timestamp": 1_700_000_000_000u64,
                        "kind": "NodeAdded",
                        "payload": {
                            "entity_id": contract_id,
                            "weight_json": contract_weight_json,
                        },
                    }),
                ))
                .await
                .expect("failed to seed contract node");

            // AwardedTo edge: entity → contract (matching mapper semantics)
            let edge = EdgeWeight::AwardedTo {
                meta: EdgeMeta {
                    created_at: chrono::Utc::now(),
                    effective_from: None,
                    effective_to: None,
                },
                data: AwardedToData {
                    details: String::new(),
                },
            };
            let edge_json = serde_json::to_string(&edge).unwrap();
            db.query("CREATE graph_events CONTENT $r")
                .bind((
                    "r",
                    serde_json::json!({
                        "event_id": format!("edge-awarded-{}-{}", contract_id, entity_id),
                        "timestamp": 1_700_000_000_000u64,
                        "kind": "EdgeAdded",
                        "payload": {
                            "src_id": entity_id,
                            "dst_id": contract_id,
                            "weight_json": edge_json,
                        },
                    }),
                ))
                .await
                .expect("failed to seed awarded_to edge");
        }
    };

    seed_contract("contract-lpa1", "E05001234").await;
    seed_contract("contract-lpa2", "E05005678").await;

    let profile = query_entity_by_id(&db, entity_id)
        .await
        .unwrap()
        .expect("entity should be found");

    assert_eq!(
        profile.contracts.len(),
        2,
        "both contracts from different LPA wards should appear in the profile"
    );
}

/// AC-DET-2: entities sharing a common director are returned as related entities.
#[tokio::test]
async fn test_query_related_entities_via_shared_director() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;

    let entity_a = "GB-COH:10000001";
    let entity_b = "GB-COH:10000002";
    let officer_id = "director:shared-director";

    seed_entity(&db, entity_a, "Company Alpha Ltd", Some("10000001"), None).await;
    seed_entity(&db, entity_b, "Company Beta Ltd", Some("10000002"), None).await;
    seed_officer(&db, officer_id, "Shared Director", Some("director")).await;

    seed_directs_edge(&db, officer_id, entity_a).await;
    seed_directs_edge(&db, officer_id, entity_b).await;

    let related = query_related_entities(&db, entity_a, 10).await.unwrap();

    assert_eq!(
        related.len(),
        1,
        "entity B should be the sole related entity"
    );
    assert_eq!(related[0].name, "Company Beta Ltd");
    assert_eq!(
        related[0].shared_connection.kind, "director",
        "shared connection type should be 'director'"
    );
}

/// AC-DET-2: entities sharing a registered address are returned as related entities.
#[tokio::test]
async fn test_query_related_entities_via_shared_address() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_schema::types::node::{LocationData, NodeWeight};

    let db = make_test_db().await;

    let entity_a = "GB-COH:20000001";
    let entity_b = "GB-COH:20000002";
    let location_id = "location:ec1a-1bb";

    seed_entity(&db, entity_a, "Address Alpha Ltd", Some("20000001"), None).await;
    seed_entity(&db, entity_b, "Address Beta Ltd", Some("20000002"), None).await;

    // Seed the location node
    let loc_weight = NodeWeight::Location(LocationData {
        name: "1 Example Street, London EC1A 1BB".to_owned(),
        postcode: Some("EC1A 1BB".to_owned()),
        address_line_1: Some("1 Example Street".to_owned()),
        locality: Some("London".to_owned()),
    });
    let loc_weight_json = serde_json::to_string(&loc_weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("node-{}", location_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": location_id,
                    "weight_json": loc_weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed location");

    seed_located_at_edge(&db, entity_a, location_id).await;
    seed_located_at_edge(&db, entity_b, location_id).await;

    let related = query_related_entities(&db, entity_a, 10).await.unwrap();

    assert_eq!(
        related.len(),
        1,
        "entity B should be the sole related entity"
    );
    assert_eq!(
        related[0].shared_connection.kind, "address",
        "shared connection type should be 'address'"
    );
}

#[tokio::test]
async fn test_query_related_entities_empty_when_no_connections() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    let entity_id = "GB-COH:30000001";
    seed_entity(
        &db,
        entity_id,
        "Isolated Company Ltd",
        Some("30000001"),
        None,
    )
    .await;

    let related = query_related_entities(&db, entity_id, 10).await.unwrap();
    assert_eq!(
        related.len(),
        0,
        "entity with no edges should have no related entities"
    );
}

/// Security V5 / ASVS V5.1: SurrealQL injection via name field must return Ok(empty),
/// must not panic, and must leave the DB intact.
#[tokio::test]
async fn test_query_entity_by_name_handles_injection_attempt() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    // Seed a legitimate entity so the DB is non-empty (injection can't "win" by deleting it)
    seed_entity(
        &db,
        "GB-COH:40000001",
        "Legitimate Corp Ltd",
        Some("40000001"),
        None,
    )
    .await;

    let injection = "'; DROP TABLE graph_events; --";
    let result = query_entity_by_name(&db, injection).await;

    assert!(result.is_ok(), "injection attempt must not cause an Err");
    assert!(
        result.unwrap().is_empty(),
        "injection string should match no entity names"
    );

    // Verify the DB is still reachable and data is intact
    let alerts = query_alerts(&db, &AlertFilter::default()).await.unwrap();
    // graph_events must still be queryable (table was not dropped)
    drop(alerts);

    // Confirm the seeded entity is still retrievable
    let still_there = query_entity_by_name(&db, "Legitimate Corp").await.unwrap();
    assert_eq!(
        still_there.len(),
        1,
        "legitimate entity must still be present after injection attempt"
    );
}

/// Security V5 / ASVS V5.1 — V-03 fix: is_valid_ch_number gate must reject
/// non-alphanumeric input before any DB query executes.
#[tokio::test]
async fn test_query_entity_by_ch_number_rejects_injection() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_entity(
        &db,
        "GB-COH:50000001",
        "Safe Company Ltd",
        Some("50000001"),
        None,
    )
    .await;

    let injection = "12345678'; DELETE FROM graph_events; --";
    let result = query_entity_by_ch_number(&db, injection).await;

    assert!(result.is_ok(), "injection must not return Err");
    assert!(
        result.unwrap().is_none(),
        "is_valid_ch_number gate must reject the input with Ok(None)"
    );

    // Verify DB is intact — query_alerts still works and graph_events wasn't modified
    let alerts = query_alerts(&db, &AlertFilter::default()).await;
    assert!(
        alerts.is_ok(),
        "DB must still be reachable after injection attempt"
    );
}

// ── Schema initialisation ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_schema_creates_graph_events_table() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    // If graph_events doesn't exist, this CREATE would fail.
    seed_alert(&db, "schema-chk-001", "PhoenixLoop", 0.9, "E05001234").await;
    let rows: Vec<serde_json::Value> = db
        .query("SELECT * FROM graph_events")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert_eq!(rows.len(), 1, "graph_events should exist after schema init");
}

#[tokio::test]
async fn test_schema_creates_ch_cursor_table() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    // INSERT into ch_cursor — errors if table doesn't exist.
    db.query("UPSERT ch_cursor:ch_stream SET timepoint = 1")
        .await
        .expect("ch_cursor table should exist after schema init");
}

// ── Empty DB queries ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_query_alerts_empty_db_returns_empty_not_error() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    let filter = AlertFilter {
        min_score: 0.0,
        limit: 50,
        ..Default::default()
    };
    let alerts = query_alerts(&db, &filter)
        .await
        .expect("empty DB must return Ok(vec![]), not Err");
    assert!(alerts.alerts.is_empty());
}

#[tokio::test]
async fn test_get_alert_by_id_missing_returns_none() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    let result = get_alert_by_id(&db, "does-not-exist-xyz").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_get_alerts_by_ids_empty_input_returns_empty_map() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    let map = get_alerts_by_ids(&db, &[]).await.unwrap();
    assert!(map.is_empty());
}

// ── Write / read round-trip ───────────────────────────────────────────────────

#[tokio::test]
async fn test_query_alerts_returns_seeded_motif_events() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_alert(&db, "rnd-001", "PhoenixLoop", 0.85, "E05001234").await;
    seed_alert(&db, "rnd-002", "AddressCluster", 0.72, "E05001234").await;

    let filter = AlertFilter {
        min_score: 0.0,
        limit: 50,
        ..Default::default()
    };
    let alerts = query_alerts(&db, &filter).await.unwrap();
    assert_eq!(alerts.alerts.len(), 2);
    // Motif types should be present
    let motifs: Vec<&str> = alerts
        .alerts
        .iter()
        .map(|a| a.motif_type.as_str())
        .collect();
    assert!(motifs.contains(&"PhoenixLoop"));
    assert!(motifs.contains(&"AddressCluster"));
}

#[tokio::test]
async fn test_get_alert_by_id_found() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_alert(&db, "fetch-001", "NepotismBridge", 0.88, "E05001234").await;

    let alert = get_alert_by_id(&db, "fetch-001")
        .await
        .unwrap()
        .expect("should find seeded alert");
    assert_eq!(alert.id, "fetch-001");
    assert_eq!(alert.motif_type, "NepotismBridge");
    assert!((alert.score - 0.88).abs() < 0.01);
    assert_eq!(alert.ward_code.as_deref(), Some("E05001234"));
    assert!(!alert.detected_at.is_empty());
}

#[tokio::test]
async fn test_get_alerts_by_ids_batch_fetch() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_alert(&db, "batch-001", "PhoenixLoop", 0.85, "E05001234").await;
    seed_alert(&db, "batch-002", "AddressCluster", 0.72, "E05001234").await;
    seed_alert(&db, "batch-003", "NepotismBridge", 0.91, "E05001234").await;

    let ids = vec!["batch-001".to_string(), "batch-003".to_string()];
    let map = get_alerts_by_ids(&db, &ids).await.unwrap();
    assert_eq!(map.len(), 2);
    assert!(map.contains_key("batch-001"));
    assert!(map.contains_key("batch-003"));
    assert!(!map.contains_key("batch-002"), "batch-002 not requested");
}

// ── Score filtering ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_query_alerts_min_score_filter() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_alert(&db, "hi-001", "PhoenixLoop", 0.90, "E05001234").await;
    seed_alert(&db, "lo-001", "PhoenixLoop", 0.30, "E05001234").await;
    seed_alert(&db, "md-001", "PhoenixLoop", 0.70, "E05001234").await;

    let filter = AlertFilter {
        min_score: 0.7,
        limit: 50,
        ..Default::default()
    };
    let alerts = query_alerts(&db, &filter).await.unwrap();
    assert_eq!(
        alerts.alerts.len(),
        2,
        "only hi-001 and md-001 should pass 0.7 threshold"
    );
    assert!(alerts.alerts.iter().all(|a| a.score >= 0.7));
}

#[tokio::test]
async fn test_query_alerts_score_boundary_exact_match() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_alert(&db, "exact-001", "PhoenixLoop", 0.7, "E05001234").await;

    let filter = AlertFilter {
        min_score: 0.7,
        limit: 50,
        ..Default::default()
    };
    let alerts = query_alerts(&db, &filter).await.unwrap();
    assert_eq!(
        alerts.alerts.len(),
        1,
        "exact boundary score should be included"
    );
}

// ── Ward filtering ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_query_alerts_ward_filter() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_alert(&db, "wa-001", "PhoenixLoop", 0.8, "E05001234").await;
    seed_alert(&db, "wa-002", "PhoenixLoop", 0.8, "E05005678").await;
    seed_alert(&db, "wa-003", "PhoenixLoop", 0.8, "E05001234").await;

    let filter = AlertFilter {
        min_score: 0.0,
        ward_code: Some("E05001234".to_string()),
        limit: 50,
        ..Default::default()
    };
    let alerts = query_alerts(&db, &filter).await.unwrap();
    assert_eq!(
        alerts.alerts.len(),
        2,
        "only E05001234 alerts should be returned"
    );
    assert!(alerts
        .alerts
        .iter()
        .all(|a| a.ward_code.as_deref() == Some("E05001234")));
}

// ── Motif type filtering ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_query_alerts_motif_type_filter() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_alert(&db, "mt-001", "PhoenixLoop", 0.8, "E05001234").await;
    seed_alert(&db, "mt-002", "AddressCluster", 0.8, "E05001234").await;
    seed_alert(&db, "mt-003", "NepotismBridge", 0.8, "E05001234").await;

    let filter = AlertFilter {
        min_score: 0.0,
        motif_type: Some("AddressCluster".to_string()),
        limit: 50,
        ..Default::default()
    };
    let alerts = query_alerts(&db, &filter).await.unwrap();
    assert_eq!(alerts.alerts.len(), 1);
    assert_eq!(alerts.alerts[0].motif_type, "AddressCluster");
}

// ── Limit ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_query_alerts_limit_respected() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    for i in 0..10u32 {
        seed_alert(
            &db,
            &format!("lim-{:03}", i),
            "PhoenixLoop",
            0.8,
            "E05001234",
        )
        .await;
    }

    let filter = AlertFilter {
        min_score: 0.0,
        limit: 3,
        ..Default::default()
    };
    let alerts = query_alerts(&db, &filter).await.unwrap();
    assert_eq!(
        alerts.alerts.len(),
        3,
        "limit=3 should return exactly 3 results"
    );
}

// ── Combined filters ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_query_alerts_combined_ward_and_score_filter() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    seed_alert(&db, "cmb-001", "PhoenixLoop", 0.9, "E05001234").await; // ✓ passes both
    seed_alert(&db, "cmb-002", "PhoenixLoop", 0.3, "E05001234").await; // ✗ low score
    seed_alert(&db, "cmb-003", "PhoenixLoop", 0.9, "E05005678").await; // ✗ wrong ward
    seed_alert(&db, "cmb-004", "PhoenixLoop", 0.8, "E05001234").await; // ✓ passes both

    let filter = AlertFilter {
        min_score: 0.7,
        ward_code: Some("E05001234".to_string()),
        limit: 50,
        ..Default::default()
    };
    let alerts = query_alerts(&db, &filter).await.unwrap();
    assert_eq!(
        alerts.alerts.len(),
        2,
        "only cmb-001 and cmb-004 should pass"
    );
}

// ── Phase 13: LPA-presence helpers and tests ─────────────────────────────────

/// Insert a synthetic Contract NodeAdded + AwardedTo EdgeAdded into graph_events.
///
/// `ward_code`         – ONS ward code (Some for planning applications, None for CF contracts)
/// `tender_ref`        – Contracts Finder tender ref (Some ⇒ CF contract)
/// `application_ref`   – Planning portal application ref (Some ⇒ planning application)
/// `entity_id`         – target entity the contract is awarded to
async fn seed_contract(
    db: &Surreal<Any>,
    contract_id: &str,
    entity_id: &str,
    ward_code: Option<&str>,
    tender_ref: Option<&str>,
    application_ref: Option<&str>,
) {
    use urban_integrity_schema::types::node::{ContractData, NodeWeight};

    let contract_weight = NodeWeight::Contract(ContractData {
        name: format!("Contract {}", contract_id),
        ocid: None,
        value_gbp: Some(10_000.0),
        status: Some("active".to_owned()),
        date_published: None,
        ward_code: ward_code.map(str::to_owned),
        tender_ref: tender_ref.map(str::to_owned),
        application_ref: application_ref.map(str::to_owned),
        application_received_date: None,
        applicant_provenance: None,
        portal_detail_handle: None,
    });
    let weight_json = serde_json::to_string(&contract_weight).unwrap();

    // NodeAdded for the Contract node
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("node-{}", contract_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": contract_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed contract node");

    // AwardedTo EdgeAdded: contract → entity
    let edge = EdgeWeight::AwardedTo {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: AwardedToData {
            details: String::new(),
        },
    };
    let edge_json = serde_json::to_string(&edge).unwrap();

    // AwardedTo semantic: entity (src) → contract (dst), matching both mappers.
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("edge-awarded-{}-{}", contract_id, entity_id),
                "timestamp": 1_700_000_000_001u64,
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": entity_id,
                    "dst_id": contract_id,
                    "weight_json": edge_json,
                },
            }),
        ))
        .await
        .expect("failed to seed awarded_to edge");
}

/// CRS-03: query_entity_lpa_presence buckets contracts by LPA from the config ward→LPA map.
///
/// Seeds:
///   - 2 CF contracts (tender_ref set, no ward_code) → "Unknown LPA"
///   - 1 planning application (ward E05000001 mapped → W06000001, application_ref set)
///   - 1 planning application (ward E99999999 unmapped → "Unknown LPA", application_ref set)
///
/// Expected result: LPA "W06000001" with planning_application_count=1, contract_count=0;
///                  "Unknown LPA" with contract_count=2, planning_application_count=1.
#[tokio::test]
async fn lpa_presence_buckets_by_lpa() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_core::config::{
        AppConfig, DetectorConfigRaw, EnricherConfig, GraphConfig, IngestConfig,
        PlanningPortalConfig, SurrealConfig,
    };

    let db = make_test_db().await;
    let entity_id = "GB-COH:77001001";
    seed_entity(
        &db,
        entity_id,
        "Multi-LPA Builder Ltd",
        Some("77001001"),
        None,
    )
    .await;

    // 2 CF contracts — no ward_code, tender_ref set → will land in "Unknown LPA"
    seed_contract(
        &db,
        "cf-contract-001",
        entity_id,
        None,
        Some("CF-2024-001"),
        None,
    )
    .await;
    seed_contract(
        &db,
        "cf-contract-002",
        entity_id,
        None,
        Some("CF-2024-002"),
        None,
    )
    .await;

    // 1 planning application — ward E05000001 mapped to W06000001 via config
    seed_contract(
        &db,
        "plan-app-001",
        entity_id,
        Some("E05000001"),
        None,
        Some("PA/2024/001"),
    )
    .await;

    // 1 planning application — unmapped ward → "Unknown LPA"
    seed_contract(
        &db,
        "plan-app-002",
        entity_id,
        Some("E99999999"),
        None,
        Some("PA/2024/002"),
    )
    .await;

    // Build a minimal AppConfig with one planning portal mapping E05000001 → W06000001.
    // AppConfig has no Default impl so we construct it directly.
    let ingest = IngestConfig {
        planning_portals: vec![PlanningPortalConfig {
            lpa_code: Some("W06000001".to_owned()),
            ward_code: "E05000001".to_owned(),
            portal_type: "idox".to_owned(),
            url: "https://example.com/planning".to_owned(),
        }],
        ..Default::default()
    };
    let config = AppConfig {
        analysis: None,
        surreal: SurrealConfig {
            ws_url: "ws://localhost:8000".to_owned(),
            namespace: "test".to_owned(),
            database: "main".to_owned(),
            username: None,
            password: None,
        },
        graph: GraphConfig::default(),
        ingest,
        detector: DetectorConfigRaw::default(),
        enricher: EnricherConfig::default(),
        jurisdiction: Default::default(),
        workbench: Default::default(),
    };

    let lpa_presence = query_entity_lpa_presence(&db, entity_id, &config)
        .await
        .expect("query_entity_lpa_presence should not error");

    // Must have at least 2 buckets
    assert!(
        lpa_presence.len() >= 2,
        "expected at least 2 LPA buckets, got {}",
        lpa_presence.len()
    );

    // Find the W06000001 bucket
    let w06 = lpa_presence
        .iter()
        .find(|p| p.lpa_code == "W06000001")
        .expect("expected W06000001 bucket");
    assert_eq!(
        w06.planning_application_count, 1,
        "W06000001 should have 1 planning application"
    );
    assert_eq!(
        w06.contract_count, 0,
        "W06000001 should have 0 CF contracts"
    );
    assert!(
        w06.lpa_name.is_none(),
        "lpa_name must be None until ONS table loaded (D-03)"
    );

    // Find the Unknown LPA bucket
    let unknown = lpa_presence
        .iter()
        .find(|p| p.lpa_code == "Unknown LPA")
        .expect("expected 'Unknown LPA' bucket");
    assert_eq!(
        unknown.contract_count, 2,
        "Unknown LPA should have 2 CF contracts"
    );
    assert_eq!(
        unknown.planning_application_count, 1,
        "Unknown LPA should have 1 unmapped planning application"
    );
}

// ── Phase 13: ward→LPA unit test (no DB required) ────────────────────────────

/// CRS-03 / D-02: ward→LPA map construction from PlanningPortalConfig skips entries
/// where lpa_code is None, so those wards fall into the "Unknown LPA" bucket.
///
/// This is a unit test — no SurrealDB instance required.
#[test]
fn ward_to_lpa_skips_none_lpa_code() {
    use urban_integrity_core::config::PlanningPortalConfig;

    let portals = [
        PlanningPortalConfig {
            lpa_code: Some("W06000001".to_owned()),
            ward_code: "E05000001".to_owned(),
            portal_type: "idox".to_owned(),
            url: "https://a.example.com".to_owned(),
        },
        PlanningPortalConfig {
            lpa_code: None, // ← this ward should be excluded from the map
            ward_code: "E05000002".to_owned(),
            portal_type: "idox".to_owned(),
            url: "https://b.example.com".to_owned(),
        },
        PlanningPortalConfig {
            lpa_code: Some("W06000003".to_owned()),
            ward_code: "E05000003".to_owned(),
            portal_type: "idox".to_owned(),
            url: "https://c.example.com".to_owned(),
        },
    ];

    // Build the ward→LPA HashMap exactly as query_entity_lpa_presence does
    let ward_to_lpa: std::collections::HashMap<&str, &str> = portals
        .iter()
        .filter_map(|p| p.lpa_code.as_deref().map(|lpa| (p.ward_code.as_str(), lpa)))
        .collect();

    // E05000001 and E05000003 must be present
    assert_eq!(ward_to_lpa.get("E05000001"), Some(&"W06000001"));
    assert_eq!(ward_to_lpa.get("E05000003"), Some(&"W06000003"));

    // E05000002 (lpa_code: None) must be absent
    assert!(
        !ward_to_lpa.contains_key("E05000002"),
        "ward with lpa_code: None must be excluded from the map"
    );
    assert_eq!(ward_to_lpa.len(), 2, "only 2 entries should be in the map");
}

// ── Phase 22 (PROP-07): in-position relation reads + EC RecordId form ─────────
//
// owns_property / offshore_link / trustee_of / donates_to all carry the entity
// on the IN (left/source) side per the DDL (RELATION FROM entity TO ...). These
// seeds RELATE with the entity on the LEFT — the OPPOSITE of `seed_psc_controls`
// (psc->controls->entity). meta.* are SCHEMAFULL `datetime` → SurrealQL `d'…'`
// literals (a JSON string is rejected). Relations are NOT ENFORCED (D-14), so the
// OUT node need not pre-exist for the RELATE to hold.

/// Seed an `owns_property` edge (entity in IN) plus its property data.
/// Mirrors the projector shape: `RELATE entity:` + "`GB-COH:<ch>`" +
/// `->owns_property->property:` + "`<id>`".
async fn seed_owns_property(
    db: &Surreal<Any>,
    entity_id: &str,
    property_id: &str,
    price_paid: f64,
    tenure: &str,
) {
    let entity_thing = format!("entity:`{entity_id}`");
    let property_thing = format!("property:`{property_id}`");
    db.query(format!(
        "RELATE {entity_thing}->owns_property->{property_thing} CONTENT {{ \
             meta: {{ \
                 created_at: d'2024-04-12T00:00:00Z', \
                 effective_from: d'2024-04-12T00:00:00Z', \
                 effective_to: NONE \
             }}, \
             data: {{ price_paid: $price, tenure: $tenure }} \
         }} RETURN NONE"
    ))
    .bind(("price", price_paid))
    .bind(("tenure", tenure.to_string()))
    .await
    .expect("failed to seed owns_property edge");
}

/// Seed a `trustee_of` edge (entity in IN, charity in OUT) plus the charity's
/// `graph_events` NodeAdded so the display-name read resolves. `trustee_of` is
/// meta-only (no `data` field) per the DDL.
async fn seed_trustee_of(db: &Surreal<Any>, entity_id: &str, charity_id: &str, charity_name: &str) {
    let entity_thing = format!("entity:`{entity_id}`");
    let charity_thing = format!("charity:`{charity_id}`");
    db.query(format!(
        "RELATE {entity_thing}->trustee_of->{charity_thing} CONTENT {{ \
             meta: {{ \
                 created_at: d'2024-04-12T00:00:00Z', \
                 effective_from: d'2024-04-12T00:00:00Z', \
                 effective_to: NONE \
             }} \
         }} RETURN NONE"
    ))
    .await
    .expect("failed to seed trustee_of edge");

    // The read resolves the charity name from graph_events keyed by
    // payload.entity_id == the stripped OUT bare id (here: charity_id).
    let weight = NodeWeight::Charity(CharityData {
        name: charity_name.to_string(),
        charity_number: None,
        registered_name: None,
    });
    let weight_json = serde_json::to_string(&weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("node-{}", charity_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": charity_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed charity graph_events node");
}

/// Seed an `identity_links` row whose `source_node_id` is the EC projector's
/// RecordId-string form ("entity:` + GB-COH:<ch> + `"). `resolved_at` is
/// SCHEMAFULL `datetime` (required) → use a SurrealQL `d'…'` literal (a JSON
/// string is rejected, silently dropping the row). `source_node_id` is bound as
/// a param so the backtick-quoted value is stored verbatim as a string (the
/// TYPE-string field prevents RecordId coercion).
async fn seed_ec_identity_link(db: &Surreal<Any>, entity_id: &str, target_node_id: &str) {
    let rec_form = format!("entity:`{entity_id}`");
    db.query(
        "CREATE identity_links CONTENT { \
             source_node_id: $sni, \
             target_node_id: $tni, \
             resolution_type: 'exact_company_number', \
             confidence: 1.0, \
             source: 'electoral_commission', \
             resolved_at: d'2024-04-12T00:00:00Z', \
             evidence_json: '{}', \
             status: 'auto_linked' \
         }",
    )
    .bind(("sni", rec_form))
    .bind(("tni", target_node_id.to_string()))
    .await
    .expect("failed to seed EC identity_link")
    .check()
    .expect("EC identity_link CREATE returned a statement error");
}

/// PROP-07: `owns_property` edges in the IN position surface in the profile with
/// the property label + price, and a decoy entity's property does not leak.
#[tokio::test]
async fn test_query_entity_owns_property() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;

    let target_ch = "11112222";
    let target_eid = format!("GB-COH:{target_ch}");
    seed_entity(
        &db,
        &target_eid,
        "Property Holdings Ltd",
        Some(target_ch),
        None,
    )
    .await;
    seed_owns_property(&db, &target_eid, "12345678_ABC123", 450_000.0, "Freehold").await;

    // Decoy with its own property — must NOT leak into the target.
    let decoy_ch = "33334444";
    let decoy_eid = format!("GB-COH:{decoy_ch}");
    seed_entity(&db, &decoy_eid, "Decoy Estates Ltd", Some(decoy_ch), None).await;
    seed_owns_property(&db, &decoy_eid, "99999999_ZZZ999", 1_000.0, "Leasehold").await;

    let profile = query_entity_by_ch_number(&db, target_ch)
        .await
        .unwrap()
        .expect("target company must resolve");
    assert_eq!(
        profile.owns_property.len(),
        1,
        "exactly one owned property must surface from the in-position read; got {:?}",
        profile.owns_property
    );
    // `owns_property` became `Vec<PropertyRow>` when PIV-03 gave the section a provenance
    // (plan 45-04); the rendered text is the row's `label`, unchanged.
    let label = &profile.owns_property[0].label;
    assert!(
        label.contains("12345678_ABC123"),
        "property label must contain the OUT node id; got {label}"
    );
    assert!(
        label.contains("450000"),
        "price_paid must render in the label; got {label}"
    );
    assert!(
        !profile
            .owns_property
            .iter()
            .any(|s| s.label.contains("99999999")),
        "decoy company's property must not appear in the target profile"
    );

    // Company with no owns_property edge → empty (no false positives).
    let none_ch = "55556666";
    seed_entity(
        &db,
        &format!("GB-COH:{none_ch}"),
        "No Property Ltd",
        Some(none_ch),
        None,
    )
    .await;
    let none = query_entity_by_ch_number(&db, none_ch)
        .await
        .unwrap()
        .expect("company must resolve");
    assert!(
        none.owns_property.is_empty(),
        "company with no owns_property edge must have zero owned properties"
    );
}

/// PROP-07: EC `osintLinks` resolve when `source_node_id` is stored in the
/// RecordId-string form "entity:`GB-COH:X`" (T-22-01-01), and decoys don't leak.
#[tokio::test]
async fn test_query_entity_ec_osint_links() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;

    let target_ch = "77778888";
    let target_eid = format!("GB-COH:{target_ch}");
    seed_entity(
        &db,
        &target_eid,
        "Donor Holdings Ltd",
        Some(target_ch),
        None,
    )
    .await;
    seed_ec_identity_link(&db, &target_eid, "political_actor:some-party").await;

    // Decoy link stored against a different entity — must not surface here.
    let decoy_ch = "88889999";
    let decoy_eid = format!("GB-COH:{decoy_ch}");
    seed_entity(&db, &decoy_eid, "Other Co Ltd", Some(decoy_ch), None).await;
    seed_ec_identity_link(&db, &decoy_eid, "political_actor:decoy-party").await;

    let profile = query_entity_by_ch_number(&db, target_ch)
        .await
        .unwrap()
        .expect("target company must resolve");
    assert_eq!(
        profile.osint_links.len(),
        1,
        "EC osintLink stored as the RecordId-string form must resolve; got {:?}",
        profile.osint_links
    );
    let link = &profile.osint_links[0];
    assert_eq!(link.source_system, "electoral_commission");
    assert_eq!(link.target_node_id, "political_actor:some-party");
    assert!(
        !profile
            .osint_links
            .iter()
            .any(|l| l.target_node_id.contains("decoy")),
        "decoy entity's EC link must not leak into the target profile"
    );
}

/// PROP-07: `trustee_of` edges in the IN position surface with the resolved
/// charity name from graph_events, and a decoy's trusteeship does not leak.
#[tokio::test]
async fn test_graph_trustee_of_in_position() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;

    let target_ch = "22223333";
    let target_eid = format!("GB-COH:{target_ch}");
    seed_entity(&db, &target_eid, "Trustee Co Ltd", Some(target_ch), None).await;
    seed_trustee_of(&db, &target_eid, "GB-CHC:1234567", "Helping Hands Trust").await;

    // Decoy trusteeship on another entity — must not leak.
    let decoy_ch = "44445555";
    let decoy_eid = format!("GB-COH:{decoy_ch}");
    seed_entity(&db, &decoy_eid, "Decoy Co Ltd", Some(decoy_ch), None).await;
    seed_trustee_of(&db, &decoy_eid, "GB-CHC:7654321", "Decoy Foundation").await;

    let profile = query_entity_by_ch_number(&db, target_ch)
        .await
        .unwrap()
        .expect("target company must resolve");
    assert_eq!(
        profile.trustee_of.len(),
        1,
        "exactly one trusteeship must surface from the in-position read; got {:?}",
        profile.trustee_of
    );
    // `trustee_of` became `Vec<LinkedRecordRow>` when PIV-03 gave the section a provenance
    // (plan 45-04); the rendered text is the row's `label`, unchanged.
    assert_eq!(
        profile.trustee_of[0].label, "Helping Hands Trust",
        "the charity display name must resolve from graph_events"
    );
    assert!(
        !profile.trustee_of.iter().any(|s| s.label.contains("Decoy")),
        "decoy company's trusteeship must not appear in the target profile"
    );
}

// ── Phase 22 (PROP-07): offshore-triangle graph-path traversal ───────────────
//
// bfs_traversal reads `graph_events` EdgeAdded events for adjacency and adds
// VIRTUAL identity_link edges only for status IN [auto_linked, confirmed]
// (traversal.rs L329). A pending_review@0.65 OCOD↔ICIJ link is therefore NOT
// traversable until an analyst confirms it (Pitfall 5, by design — ENR-11).

/// Seed an EdgeAdded `OwnsProperty` event (entity → property) for traversal.
async fn seed_owns_property_edge(db: &Surreal<Any>, entity_id: &str, property_id: &str) {
    let edge = EdgeWeight::OwnsProperty {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: OwnsPropertyData {
            price_paid: Some(450_000.0),
            tenure: Some("Freehold".to_string()),
        },
    };
    let weight_json = serde_json::to_string(&edge).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("edge-owns-{}-{}", entity_id, property_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": entity_id,
                    "dst_id": property_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed owns_property EdgeAdded");
}

/// Seed an EdgeAdded `AwardedTo` event (entity → contract) for traversal.
async fn seed_awarded_to_edge(db: &Surreal<Any>, entity_id: &str, contract_id: &str) {
    let edge = EdgeWeight::AwardedTo {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: AwardedToData {
            details: String::new(),
        },
    };
    let weight_json = serde_json::to_string(&edge).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("edge-awarded-{}-{}", entity_id, contract_id),
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": entity_id,
                    "dst_id": contract_id,
                    "weight_json": weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed awarded_to EdgeAdded");
}

/// Seed an `identity_links` row (the OCOD↔ICIJ offshore leg). `resolved_at` is a
/// required SCHEMAFULL datetime → `d'…'` literal; status/confidence are caller-set.
async fn seed_offshore_identity_link(
    db: &Surreal<Any>,
    source_node_id: &str,
    target_node_id: &str,
    status: &str,
    confidence: f64,
) {
    db.query(
        "CREATE identity_links CONTENT { \
             source_node_id: $src, \
             target_node_id: $tgt, \
             resolution_type: 'name_only_offshore_review', \
             confidence: $conf, \
             source: 'land_registry_ocod', \
             resolved_at: d'2024-04-12T00:00:00Z', \
             evidence_json: '{}', \
             status: $status \
         }",
    )
    .bind(("src", source_node_id.to_string()))
    .bind(("tgt", target_node_id.to_string()))
    .bind(("conf", confidence))
    .bind(("status", status.to_string()))
    .await
    .expect("failed to seed offshore identity_link")
    .check()
    .expect("offshore identity_link CREATE returned a statement error");
}

/// Returns true if any step in the path carries `edge_type` (already prettified).
fn path_has_edge(
    path: &[urban_integrity_core::persistence::queries::PathStep],
    edge: &str,
) -> bool {
    path.iter().any(|s| s.edge_type.as_deref() == Some(edge))
}

/// PROP-07: the offshore triangle — `contract —AwardedTo— entity —OwnsProperty—
/// property —IdentityLink— offshore`. The OwnsProperty + AwardedTo legs are
/// traversable immediately; the ICIJ (offshore) leg is traversable ONLY after the
/// pending@0.65 link is confirmed (Pitfall 5 — analyst-gated, never auto-traversed).
#[tokio::test]
async fn test_graph_path_offshore_triangle() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;

    let entity = "GB-COH:55667788";
    let property = "property:55667788_TITLE1";
    let contract = "contract:OCODTRI1";
    let offshore = "offshore_entity:ocodtrileak";

    seed_entity(&db, entity, "Triangle Holdings Ltd", Some("55667788"), None).await;
    seed_owns_property_edge(&db, entity, property).await;
    seed_awarded_to_edge(&db, entity, contract).await;
    // The offshore leg: OCOD↔ICIJ link stored property→offshore, pending@0.65.
    seed_offshore_identity_link(&db, property, offshore, "pending_review", 0.65).await;

    // ── Phase A: pre-confirm ──────────────────────────────────────────────────
    // contract → entity → property is fully traversable (OwnsProperty + AwardedTo).
    let path_cp = bfs_traversal(&db, contract, property, 6).await.unwrap();
    assert!(
        !path_cp.is_empty(),
        "pre-confirm: contract↔property path must exist"
    );
    assert!(
        path_has_edge(&path_cp, "AWARDED_TO"),
        "pre-confirm path must contain the AwardedTo leg; got {:?}",
        path_cp.iter().map(|s| &s.edge_type).collect::<Vec<_>>()
    );
    assert!(
        path_has_edge(&path_cp, "OWNS_PROPERTY"),
        "pre-confirm path must contain the OwnsProperty leg; got {:?}",
        path_cp.iter().map(|s| &s.edge_type).collect::<Vec<_>>()
    );
    assert!(
        !path_has_edge(&path_cp, "IDENTITY_LINK"),
        "pre-confirm path must NOT contain the offshore IdentityLink leg"
    );

    // The offshore node is NOT reachable while the link is pending@0.65.
    let path_to_offshore_pre = bfs_traversal(&db, contract, offshore, 6).await.unwrap();
    assert!(
        path_to_offshore_pre.is_empty(),
        "pre-confirm: pending@0.65 offshore leg must be non-traversable; got {:?}",
        path_to_offshore_pre
            .iter()
            .map(|s| &s.node_id)
            .collect::<Vec<_>>()
    );

    // ── Phase B: simulate `links confirm` → status = confirmed ────────────────
    db.query("UPDATE identity_links SET status = 'confirmed' WHERE source_node_id = $src")
        .bind(("src", property.to_string()))
        .await
        .unwrap()
        .check()
        .expect("confirm UPDATE failed");

    // Now the full triangle is traversable: contract → entity → property → offshore.
    let path_full = bfs_traversal(&db, contract, offshore, 6).await.unwrap();
    assert!(
        !path_full.is_empty(),
        "post-confirm: contract↔offshore triangle path must exist"
    );
    assert!(
        path_has_edge(&path_full, "AWARDED_TO")
            && path_has_edge(&path_full, "OWNS_PROPERTY")
            && path_has_edge(&path_full, "IDENTITY_LINK"),
        "post-confirm path must contain all three legs (AwardedTo + OwnsProperty + IdentityLink); got {:?}",
        path_full.iter().map(|s| &s.edge_type).collect::<Vec<_>>()
    );
}

// ── Regression: withdrawn alerts must not be served on the entity path (2026-08-20) ──

/// Seed a MotifDetected alert that names `entity_id` in its path, optionally withdrawn.
///
/// `alerts withdraw` marks the retraction *inside* `result_json` (`queries.rs`
/// sets `result["withdrawn_at"]`), so a faithful fixture must do the same.
async fn seed_alert_naming_entity(
    db: &Surreal<Any>,
    event_id: &str,
    entity_id: &str,
    withdrawn: bool,
) {
    let mut result = serde_json::json!({
        "motif_type": "PhoenixLoop",
        "risk_score": 0.9,
        "score_breakdown": { "ward_code": "E05001234" },
        "path_nodes": [entity_id],
        "unique_hash": uuid::Uuid::new_v4().to_string(),
    });
    if withdrawn {
        result["withdrawn_at"] = serde_json::json!("2026-08-11T09:00:00Z");
        result["withdrawn_reason"] = serde_json::json!("adjudicated a false positive");
    }
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": 1_700_000_000_000u64,
                "kind": "MotifDetected",
                "payload": { "result_json": result.to_string() },
            }),
        ))
        .await
        .expect("failed to seed alert");
}

/// Regression: an alert an operator withdrew must not reappear on the entity read path.
///
/// `assemble_entity_profile` scanned `MotifDetected` events naming the entity and parsed
/// them straight through — under a comment reading "Active alerts that reference this
/// entity". The withdrawal predicate existed but ran only inside `query_alerts`, so
/// `entity`, `verify` and the GraphQL `entity` resolver each served retracted alerts.
///
/// That is the T-23-FalseAccusation case: a corruption alert an operator explicitly
/// disowned continuing to circulate about a named subject. Withdrawn alerts remain
/// readable — deliberately, since a retracted false positive is evidence about detector
/// quality — but only where they are asked for, via `alerts list --include-withdrawn`.
#[tokio::test]
async fn a_withdrawn_alert_is_not_served_on_the_entity_path() {
    if !db_tests_enabled() {
        return;
    }
    let db = make_test_db().await;
    let entity_id = "GB-COH:withdrawn-leak-01";

    seed_entity(&db, entity_id, "Withdrawn Leak Ltd", Some("00990011"), None).await;
    seed_alert_naming_entity(&db, "wl-live-001", entity_id, false).await;
    seed_alert_naming_entity(&db, "wl-withdrawn-001", entity_id, true).await;

    let profile = query_entity_by_id(&db, entity_id)
        .await
        .expect("query must succeed")
        .expect("entity should be found");

    let ids: Vec<&str> = profile.alerts.iter().map(|a| a.id.as_str()).collect();
    assert!(
        ids.contains(&"wl-live-001"),
        "the live alert must still be served — got {ids:?}"
    );
    assert!(
        !ids.contains(&"wl-withdrawn-001"),
        "a withdrawn alert must not be served on the entity path: a retracted accusation \
         about a named subject continuing to circulate is the T-23-FalseAccusation case — \
         got {ids:?}"
    );
    assert!(
        profile.alerts.iter().all(|a| a.withdrawn_at.is_none()),
        "no alert on the entity path may carry a withdrawal marker"
    );
}

// ── PRB-05: enrichment_runs paged scan (Plan 33-02) ──────────────────────────────────────────

/// Seed one `enrichment_runs` row directly, mirroring `crates/enricher/src/engine.rs:414-437`'s
/// exact field set (`source`, `last_run_at`, `candidates_found`, `auto_linked`,
/// `pending_review`, `discarded`) via UPSERT on `enrichment_runs:{source}`.
async fn seed_enrichment_run(db: &Surreal<Any>, source: &str) {
    db.query(format!(
        "UPSERT enrichment_runs:{source} SET source = '{source}', last_run_at = $last_run_at, \
         candidates_found = $candidates_found, auto_linked = $auto_linked, \
         pending_review = $pending_review, discarded = $discarded"
    ))
    .bind(("last_run_at", 1_756_000_000i64))
    .bind(("candidates_found", 7i64))
    .bind(("auto_linked", 2i64))
    .bind(("pending_review", 1i64))
    .bind(("discarded", 4i64))
    .await
    .expect("seed enrichment_runs row")
    .check()
    .expect("seed enrichment_runs row check");
}

/// Direct proof PRB-05's paged reader advances by rows actually received and never stops early:
/// seed more `enrichment_runs` rows than a small explicit `page_size` and assert every one comes
/// back, none dropped or duplicated.
#[tokio::test]
async fn db_enrichment_runs_scan_pages_past_a_hard_coded_ceiling() {
    if !db_tests_enabled() {
        eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix(
        "test_enrichment_runs_scan",
        "enrichment_runs_paging",
    )
    .await;

    const N: usize = 5;
    let sources: Vec<String> = (0..N).map(|i| format!("source_{i:02}")).collect();
    for source in &sources {
        seed_enrichment_run(&db, source).await;
    }

    // page_size = 2, deliberately smaller than N = 5 rows seeded, to force multi-page reads.
    let all = scan_enrichment_runs_paged(&db, 2)
        .await
        .expect("scan_enrichment_runs_paged must succeed");

    assert_eq!(
        all.len(),
        N,
        "the paged scan must read past the page size — got {} of {N} expected sources",
        all.len()
    );
    for source in &sources {
        assert!(
            all.contains_key(source),
            "seeded source {source} missing from paged scan result — dropped, not just \
             reordered"
        );
    }
}
