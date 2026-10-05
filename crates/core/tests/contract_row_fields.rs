//! Integration tests for ContractRow field propagation.
//!
//! Requirements covered:
//! - REP-04: ContractRow carries tender_ref and application_ref (Wave 1 schema wiring)
//! - REP-04: assemble_entity_profile copies fields from ContractData (db-backed, Wave 2)
//!
//! The db-backed test is gated at **runtime** on `URBAN_DB_TESTS=1` plus a reachable
//! SurrealDB, matching every other gated test in the workspace. It was previously
//! `#[ignore]`d with a reason string claiming `URBAN_DB_TESTS=1 cargo test --workspace`
//! exercised it — untrue, because `#[ignore]` is unconditional and only
//! `--include-ignored` overrides it. The body was also empty, so the test asserted
//! nothing in any configuration.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::queries::query_entity_by_id;
use urban_integrity_core::ContractRow;
use urban_integrity_schema::types::edge::{AwardedToData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::node::{ContractData, EntityData, EntityStatus, NodeWeight};

/// REP-04: ContractRow literal with tender_ref Some(...) reads back correctly.
#[test]
fn rep_04_contract_row_carries_tender_ref() {
    let row = ContractRow {
        id: "CF:CF-TEST".to_owned(),
        name: "Test Contract".to_owned(),
        value_gbp: None,
        status: None,
        published: None,
        ward_code: None,
        tender_ref: Some("CF-TEST".to_owned()),
        application_ref: None,
        provenance: urban_integrity_core::Provenance::Unattributed {
            reason: "hand-built test fixture".to_owned(),
        },
        temporal: urban_integrity_core::Temporal::unfiltered(
            urban_integrity_core::EffectiveRange::Unrecorded {
                basis: "hand-built test fixture".to_owned(),
            },
        ),
    };
    assert_eq!(row.tender_ref, Some("CF-TEST".to_owned()));
}

/// REP-04: ContractRow literal with application_ref Some(...) reads back correctly.
#[test]
fn rep_04_contract_row_carries_application_ref() {
    let row = ContractRow {
        id: "planning:FUL/2024/001".to_owned(),
        name: "Test Contract".to_owned(),
        value_gbp: None,
        status: None,
        published: None,
        ward_code: None,
        tender_ref: None,
        application_ref: Some("FUL/2024/001".to_owned()),
        provenance: urban_integrity_core::Provenance::Unattributed {
            reason: "hand-built test fixture".to_owned(),
        },
        temporal: urban_integrity_core::Temporal::unfiltered(
            urban_integrity_core::EffectiveRange::Unrecorded {
                basis: "hand-built test fixture".to_owned(),
            },
        ),
    };
    assert_eq!(row.application_ref, Some("FUL/2024/001".to_owned()));
}

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

async fn seed_node_event(db: &Surreal<Any>, entity_id: &str, weight: &NodeWeight) {
    let weight_json = serde_json::to_string(weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("NodeAdded-{entity_id}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": { "entity_id": entity_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed node event");
}

/// AwardedTo semantic: entity (src) → contract (dst).
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
                "event_id": format!("edge-awarded-{entity_id}-{contract_id}"),
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
        .expect("failed to seed awarded_to edge");
}

fn contract(name: &str, tender_ref: Option<&str>, application_ref: Option<&str>) -> NodeWeight {
    NodeWeight::Contract(ContractData {
        name: name.into(),
        ocid: None,
        value_gbp: Some(125_000.0),
        status: Some("complete".into()),
        date_published: Some("2024-03-01".into()),
        ward_code: None,
        tender_ref: tender_ref.map(str::to_owned),
        application_ref: application_ref.map(str::to_owned),
        application_received_date: None,
        applicant_provenance: None,
        portal_detail_handle: None,
    })
}

/// REP-04: `assemble_entity_profile` copies `tender_ref` and `application_ref` from the
/// stored `ContractData` onto the rows it returns.
///
/// Both reference kinds are seeded on separate destinations because they are mutually
/// exclusive in production — a CF contract has `tender_ref`, a planning application has
/// `application_ref` — and `report.rs` dispatches on exactly that distinction.
///
/// **Updated for the D-03 split (plan 45-03).** The planning destination previously used a
/// fictional `GB-PLAN:` id and was asserted to arrive in `profile.contracts`, which is no longer
/// where the profile puts one. It now uses the id shape the planning mapper actually writes —
/// `planning:<ref>`, the only planning key that occurs in the store — and is asserted against
/// `profile.planning_applications`. The REP-04 claim itself is unchanged: both reference kinds
/// survive the read path onto the row that carries them.
#[tokio::test]
async fn rep_04_assemble_entity_profile_copies_contract_refs() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: rep_04_assemble_entity_profile_copies_contract_refs (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect("main").await;

    let entity_id = "GB-COH:REP04001";
    seed_node_event(
        &db,
        entity_id,
        &NodeWeight::Entity(EntityData {
            name: "Refholder Ltd".into(),
            company_number: Some("REP04001".into()),
            status: EntityStatus::Active,
            incorporation_date: None,
        }),
    )
    .await;

    let cf_id = "GB-CF:REP04-TENDER";
    seed_node_event(
        &db,
        cf_id,
        &contract("CF Awarded Works", Some("CF-2024-0042"), None),
    )
    .await;
    seed_awarded_to_edge(&db, entity_id, cf_id).await;

    let planning_id = "planning:REP04-APP";
    seed_node_event(
        &db,
        planning_id,
        &contract("Planning Application", None, Some("FUL/2024/001234")),
    )
    .await;
    seed_awarded_to_edge(&db, entity_id, planning_id).await;

    let profile = query_entity_by_id(&db, entity_id)
        .await
        .expect("query_entity_by_id failed")
        .expect("entity must be found");

    assert_eq!(
        profile.contracts.len(),
        1,
        "the CF award is the only contract — the planning application has its own section since \
         D-03, got {:?}",
        profile.contracts
    );
    assert_eq!(
        profile.planning_applications.len(),
        1,
        "the planning application must be assembled into its own section, got {:?}",
        profile.planning_applications
    );

    let cf = profile
        .contracts
        .iter()
        .find(|c| c.name == "CF Awarded Works")
        .expect("CF contract must be present");
    assert_eq!(
        cf.tender_ref.as_deref(),
        Some("CF-2024-0042"),
        "assemble_entity_profile must copy tender_ref from ContractData"
    );
    assert_eq!(
        cf.application_ref, None,
        "a CF contract must not acquire an application_ref"
    );

    let planning = profile
        .planning_applications
        .iter()
        .find(|a| a.name == "Planning Application")
        .expect("planning application must be present");
    assert_eq!(
        planning.application_ref.as_deref(),
        Some("FUL/2024/001234"),
        "assemble_entity_profile must copy application_ref from ContractData"
    );
    assert!(
        profile
            .contracts
            .iter()
            .all(|c| c.application_ref.is_none()),
        "no row under contracts may carry a planning application reference"
    );
}
