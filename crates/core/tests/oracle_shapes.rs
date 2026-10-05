//! DB-gated correctness proofs for every `oracle::` counting primitive (38-01 Task 1).
//!
//! Run with:
//!   URBAN_DB_TESTS=1 cargo test -p urban-integrity-core --test oracle_shapes -- --nocapture
//!
//! Per the project's gate discipline (docs/TESTING.md), `URBAN_DB_TESTS=1` skips **by return**, so a green run with
//! the gate closed reports identically to a green run with it open. The evidence that this suite
//! actually ran against a live SurrealDB is the run's **duration** (several seconds, not
//! near-instant) — never the pass count alone.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::oracle::{self, EdgeField};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, LocatedAtData};
use urban_integrity_schema::types::node::{
    ContractData, EntityData, EntityStatus, NodeWeight, PoliticalActorData,
};

fn db_tests_enabled() -> bool {
    urban_integrity_test_support::db_tests_enabled()
}

async fn seed_node(db: &Surreal<Any>, event_id: &str, entity_id: &str, weight: &NodeWeight) {
    urban_integrity_test_support::events::seed_node_event(db, event_id, entity_id, weight)
        .await
        .expect("failed to seed node event");
}

async fn seed_edge(
    db: &Surreal<Any>,
    event_id: &str,
    src_id: &str,
    dst_id: &str,
    weight: &EdgeWeight,
) {
    urban_integrity_test_support::events::seed_edge_event(db, event_id, src_id, dst_id, weight)
        .await
        .expect("failed to seed edge event");
}

async fn seed_motif_event(db: &Surreal<Any>, event_id: &str) {
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": 1_700_000_000_000u64,
                "kind": "MotifDetected",
                "payload": { "result_json": "{}" },
            }),
        ))
        .await
        .expect("failed to seed motif event")
        .check()
        .expect("seed motif event returned a statement error");
}

fn contract(name: &str) -> NodeWeight {
    NodeWeight::Contract(ContractData {
        name: name.to_string(),
        ocid: None,
        value_gbp: None,
        status: None,
        date_published: None,
        ward_code: None,
        tender_ref: None,
        application_ref: None,
        application_received_date: None,
        applicant_provenance: None,
        portal_detail_handle: None,
    })
}

fn entity(name: &str) -> NodeWeight {
    NodeWeight::Entity(EntityData {
        name: name.to_string(),
        company_number: None,
        status: EntityStatus::Active,
        incorporation_date: None,
    })
}

fn political_actor(name: &str, council_name: Option<&str>) -> NodeWeight {
    NodeWeight::PoliticalActor(PoliticalActorData {
        name: name.to_string(),
        party: None,
        council_name: council_name.map(str::to_owned),
    })
}

fn edge_meta() -> EdgeMeta {
    EdgeMeta {
        created_at: chrono::Utc::now(),
        effective_from: None,
        effective_to: None,
    }
}

fn located_at() -> EdgeWeight {
    EdgeWeight::LocatedAt {
        meta: edge_meta(),
        data: LocatedAtData {
            details: "test fixture".to_string(),
        },
    }
}

/// The dedup + prefix-filter proof (Pitfall 2's non-vacuity requirement).
///
/// Seeds 3 distinct Contract entities via 2 `NodeAdded` events each (a duplicate re-ingest per
/// entity) plus 1 unrelated `Entity` node. Two of the three Contracts are `planning:`-prefixed,
/// one is `GB-CF:`-prefixed.
#[tokio::test]
async fn count_distinct_entities_dedups_and_cross_checks_against_scan() {
    if !db_tests_enabled() {
        eprintln!("SKIP: count_distinct_entities_dedups_and_cross_checks_against_scan (set URBAN_DB_TESTS=1)");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_oracle", "main").await;

    // 3 distinct Contract entities, each re-ingested once (6 NodeAdded events total).
    for eid in ["planning:c1", "planning:c2", "GB-CF:c3"] {
        seed_node(&db, &format!("NodeAdded-{eid}-1"), eid, &contract(eid)).await;
        seed_node(&db, &format!("NodeAdded-{eid}-2"), eid, &contract(eid)).await;
    }
    // 1 unrelated entity of a different type — must not be counted as a Contract.
    seed_node(
        &db,
        "NodeAdded-entity-x",
        "GB-CO:x",
        &entity("Not A Contract"),
    )
    .await;

    // WR-02 dirty rows: shapes `scan_latest_entities_by_type` DROPS (entity_id_of returns None /
    // weight_json unparseable). Pre-gate, each formed its own GROUP BY bucket in the oracle and
    // produced `oracle == scan + 1` — a structural false disagreement. The Contract marker is
    // present in all three so only the entity_id/JSON gates can exclude them.
    //
    // (a) empty entity_id
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": "NodeAdded-dirty-empty",
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": { "entity_id": "", "weight_json": serde_json::to_string(&contract("planning:dirty")).unwrap() },
            }),
        ))
        .await
        .expect("failed to seed dirty empty-id event")
        .check()
        .expect("dirty empty-id seed returned a statement error");
    // (b) RecordId-shaped (object) entity_id, coerced twin of the EXISTING "planning:c1" row.
    // The two sides handle it by different mechanisms that must land on the same count:
    // `scan` normalises the object to "planning:c1" and collapses it with its string twin
    // ("greatest timestamp wins"); the oracle's `type::is_string` gate drops it outright. Either
    // way it must contribute zero NEW entities. (A coerced id with NO string twin is the
    // documented remaining divergence — an oracle undercount, i.e. a VISIBLE disagreement —
    // see the WR-02 note on `count_distinct_entities`.)
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": "NodeAdded-dirty-recordid",
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": { "tb": "planning", "id": { "String": "c1" } },
                    "weight_json": serde_json::to_string(&contract("planning:c1")).unwrap(),
                },
            }),
        ))
        .await
        .expect("failed to seed dirty recordid event")
        .check()
        .expect("dirty recordid seed returned a statement error");

    // The dedup proof: 3 distinct Contracts, not 6 events.
    let oracle_count =
        oracle::count_distinct_entities(&db, "NodeAdded", "\"type\":\"Contract\"", None)
            .await
            .expect("count_distinct_entities failed");
    assert_eq!(
        oracle_count, 3,
        "expected 3 distinct Contract entities, not 6 events"
    );

    // The Pitfall-2 cross-check: oracle's GROUP BY shape must agree with scan.rs's independently
    // implemented "greatest timestamp wins" dedup on the SAME fixture. This is the one place this
    // cross-check is allowed to live — oracle.rs itself must never import scan::.
    let scan_count = scan::scan_latest_entities_by_type(&db, "NodeAdded", "\"type\":\"Contract\"")
        .await
        .expect("scan_latest_entities_by_type failed")
        .len();
    assert_eq!(
        oracle_count as usize, scan_count,
        "oracle's GROUP BY dedup count must agree with scan.rs's dedup count on the same \
         fixture — INCLUDING the dirty rows (empty / RecordId-shaped entity_id) both sides \
         must drop (WR-02)"
    );

    // The prefix-filter proof: 2 of the 3 Contracts are `planning:`-prefixed.
    let planning_count = oracle::count_distinct_entities(
        &db,
        "NodeAdded",
        "\"type\":\"Contract\"",
        Some("planning:"),
    )
    .await
    .expect("count_distinct_entities with prefix failed");
    assert_eq!(planning_count, 2, "expected 2 planning:-prefixed Contracts");
}

/// CR-01 (phase 38 code review) scale check: the other fixtures in this file seed 3 and 4 entities, which
/// cannot distinguish a correct planner path from a truncating one — any truncation ceiling is far
/// above them, so they would pass identically under the bug. This seeds enough distinct entities
/// to sit above the row counts at which the Rule-1 planner bug has actually been observed
/// truncating in this codebase (14,198 counted vs 39 returned; 33,046 vs 6,033), and asserts the
/// oracle's nested-`SELECT` `GROUP BY` dedup still agrees exactly with `scan`'s independently
/// implemented dedup.
///
/// This does not *prove* immunity — the bug is run-to-run non-deterministic, and the ALLOWLIST
/// entries in `query_shape_guard.rs` say so explicitly. What it does is remove the "we only ever
/// tested this at n=3" objection, and give a cheap re-runnable check to point at when the oracle
/// is relied on for a new corpus or a new SurrealDB version.
#[tokio::test]
async fn count_distinct_entities_agrees_with_scan_at_a_scale_that_could_truncate() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: count_distinct_entities_agrees_with_scan_at_a_scale_that_could_truncate \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_oracle", "main").await;

    // 2,000 distinct Contracts, every one re-ingested once, so a per-event count would report
    // 4,000 and a truncating read would report something below 2,000 and non-deterministic.
    const DISTINCT: usize = 2_000;
    for i in 0..DISTINCT {
        let eid = format!("planning:scale-{i:05}");
        seed_node(&db, &format!("NodeAdded-{eid}-1"), &eid, &contract(&eid)).await;
        seed_node(&db, &format!("NodeAdded-{eid}-2"), &eid, &contract(&eid)).await;
    }

    let oracle_count =
        oracle::count_distinct_entities(&db, "NodeAdded", "\"type\":\"Contract\"", None)
            .await
            .expect("count_distinct_entities failed");
    assert_eq!(
        oracle_count as usize,
        DISTINCT,
        "oracle must count {DISTINCT} distinct entities, not {} events and not a truncated subset",
        DISTINCT * 2
    );

    let scan_count = scan::scan_latest_entities_by_type(&db, "NodeAdded", "\"type\":\"Contract\"")
        .await
        .expect("scan_latest_entities_by_type failed")
        .len();
    assert_eq!(
        oracle_count as usize, scan_count,
        "at {DISTINCT} distinct entities the oracle's nested-SELECT GROUP BY must still agree with \
         scan's dedup — a disagreement here is the truncation signature CR-01 flagged"
    );

    // The prefix filter must survive at scale too: every seeded id is `planning:`-prefixed.
    let prefixed = oracle::count_distinct_entities(
        &db,
        "NodeAdded",
        "\"type\":\"Contract\"",
        Some("planning:"),
    )
    .await
    .expect("count_distinct_entities with prefix failed");
    assert_eq!(
        prefixed as usize, DISTINCT,
        "server-side string::starts_with must not truncate at scale either"
    );
}

/// The canary shape: reproduces `identity_integrity.rs`'s own
/// `identity_integrity_catalog_baseline_round_trip_not_mocked` fixture (4 `planning:`-prefixed
/// Contract events, distinct entity_ids) and asserts the oracle's count matches that test's own
/// `total` assertion of 4 — the motivating `260831-ffx` regression this whole phase exists to
/// catch (39,860 vs 99,599 on the live corpus).
#[tokio::test]
async fn count_distinct_entities_matches_identity_integrity_canary_fixture() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: count_distinct_entities_matches_identity_integrity_canary_fixture (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_oracle", "main").await;

    for eid in [
        "planning:c2-1",
        "planning:c2-2",
        "planning:c2-3",
        "planning:c2-4",
    ] {
        seed_node(&db, &format!("NodeAdded-{eid}"), eid, &contract(eid)).await;
    }

    let count = oracle::count_distinct_entities(
        &db,
        "NodeAdded",
        "\"type\":\"Contract\"",
        Some("planning:"),
    )
    .await
    .expect("count_distinct_entities failed");
    assert_eq!(
        count, 4,
        "canary fixture must match identity_integrity's own total of 4"
    );
}

/// `count_events` with an empty marker slice — the bare kind-only count.
#[tokio::test]
async fn count_events_counts_by_kind_with_no_markers() {
    if !db_tests_enabled() {
        eprintln!("SKIP: count_events_counts_by_kind_with_no_markers (set URBAN_DB_TESTS=1)");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_oracle", "main").await;

    for i in 0..4 {
        seed_motif_event(&db, &format!("MotifDetected-{i}")).await;
    }

    let count = oracle::count_events(&db, "MotifDetected", &[])
        .await
        .expect("count_events failed");
    assert_eq!(count, 4);
}

/// `count_events` with two ANDed markers — the multi-marker AND proof
/// `phase20-council-registers`'s comparison (Plan 38-02) depends on.
#[tokio::test]
async fn count_events_ands_multiple_markers() {
    if !db_tests_enabled() {
        eprintln!("SKIP: count_events_ands_multiple_markers (set URBAN_DB_TESTS=1)");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_oracle", "main").await;

    // 2 of 3 PoliticalActor rows carry a council_name; the third has none.
    seed_node(
        &db,
        "NodeAdded-pa-1",
        "GB-PA:pa1",
        &political_actor("Councillor One", Some("Anglesey Council")),
    )
    .await;
    seed_node(
        &db,
        "NodeAdded-pa-2",
        "GB-PA:pa2",
        &political_actor("Councillor Two", Some("Gwynedd Council")),
    )
    .await;
    seed_node(
        &db,
        "NodeAdded-pa-3",
        "GB-PA:pa3",
        &political_actor("Councillor Three", None),
    )
    .await;

    // WR-06 falsifiability rows. The three PA rows above all satisfy marker 0
    // ("type":"PoliticalActor"), so on their own the assertion below constrains only marker 1 —
    // an implementation that dropped marker 0 entirely would still return 2 and pass. These two
    // rows make the count wrong (3) if EITHER predicate is dropped:
    //
    // (a) satisfies marker 1 but NOT marker 0 — an Entity-typed row carrying a council_name.
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": "NodeAdded-rogue-council",
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": "GB-CO:rogue",
                    "weight_json": "{\"type\":\"Entity\",\"name\":\"Rogue Ltd\",\"council_name\":\"Rogue Council\"}",
                },
            }),
        ))
        .await
        .expect("failed to seed rogue-council event")
        .check()
        .expect("rogue-council seed returned a statement error");
    // (b) satisfies BOTH markers but is a different `kind` — pins the `WHERE kind = $kind`
    // predicate, which no other row in this file constrained.
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": "NodeUpdated-pa-1",
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeUpdated",
                "payload": {
                    "entity_id": "GB-PA:pa1",
                    "weight_json": "{\"type\":\"PoliticalActor\",\"name\":\"Councillor One\",\"council_name\":\"Anglesey Council\"}",
                },
            }),
        ))
        .await
        .expect("failed to seed NodeUpdated PA event")
        .check()
        .expect("NodeUpdated PA seed returned a statement error");

    let count = oracle::count_events(
        &db,
        "NodeAdded",
        &["\"type\":\"PoliticalActor\"", "\"council_name\":\""],
    )
    .await
    .expect("count_events with two markers failed");
    assert_eq!(
        count, 2,
        "expected exactly the 2 NodeAdded PoliticalActor rows carrying a council_name — 3 \
         means either the type marker or the kind predicate was dropped (WR-06)"
    );
}

/// `count_distinct_edge_field` — 3 `LocatedAt` `EdgeAdded` rows, 2 distinct `dst_id` values (one
/// dst shared by 2 rows, one dst used once).
#[tokio::test]
async fn count_distinct_edge_field_dedups_dst_id() {
    if !db_tests_enabled() {
        eprintln!("SKIP: count_distinct_edge_field_dedups_dst_id (set URBAN_DB_TESTS=1)");
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_oracle", "main").await;

    seed_edge(
        &db,
        "EdgeAdded-1",
        "GB-CO:e1",
        "GB-LOC:shared",
        &located_at(),
    )
    .await;
    seed_edge(
        &db,
        "EdgeAdded-2",
        "GB-CO:e2",
        "GB-LOC:shared",
        &located_at(),
    )
    .await;
    seed_edge(
        &db,
        "EdgeAdded-3",
        "GB-CO:e3",
        "GB-LOC:unique",
        &located_at(),
    )
    .await;

    let count = oracle::count_distinct_edge_field(&db, "\"type\":\"LocatedAt\"", EdgeField::Dst)
        .await
        .expect("count_distinct_edge_field failed");
    assert_eq!(count, 2, "expected 2 distinct dst_id values");
}

/// Seeds a `NodeAdded`/`NodeUpdated` row with an explicit `timestamp` (unlike [`seed_node`],
/// which hardcodes one) — needed so this file's `max_timestamp_for_prefix` fixture can seed
/// several distinct timestamps per prefix and assert the exact max survives.
async fn seed_node_at(db: &Surreal<Any>, event_id: &str, kind: &str, entity_id: &str, ts: u64) {
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": ts,
                "kind": kind,
                "payload": { "entity_id": entity_id, "weight_json": "{}" },
            }),
        ))
        .await
        .expect("failed to seed timestamped node event")
        .check()
        .expect("timestamped node event returned a statement error");
}

/// `oracle::max_timestamp_for_prefix` (43-05 Task 1): seeds known timestamps across three
/// prefixes (`GB-COH:`, `CF:`, `planning:`), including a `NodeUpdated` row that carries the
/// TRUE max for one prefix (proving both `NodeAdded` and `NodeUpdated` must be queried and
/// maxed by the caller — this function alone is scoped to one `kind` at a time), and asserts
/// the correct max per prefix plus `None` for a prefix with no matching rows.
#[tokio::test]
async fn max_timestamp_for_prefix_returns_the_correct_max_per_prefix_and_none_when_absent() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: max_timestamp_for_prefix_returns_the_correct_max_per_prefix_and_none_when_absent \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_oracle", "main").await;

    // GB-COH: two NodeAdded rows, max = 1_700_000_000_300.
    seed_node_at(&db, "coh-1", "NodeAdded", "GB-COH:1", 1_700_000_000_100).await;
    seed_node_at(&db, "coh-2", "NodeAdded", "GB-COH:1", 1_700_000_000_300).await;
    // CF: one NodeAdded, one NodeUpdated with a LATER timestamp — the true max for this prefix
    // and `kind` combination is on the NodeAdded row alone (this function is scoped to one kind).
    seed_node_at(&db, "cf-1", "NodeAdded", "CF:1", 1_700_000_000_200).await;
    seed_node_at(&db, "cf-2", "NodeUpdated", "CF:1", 1_700_000_000_999).await;
    // planning: single row.
    seed_node_at(
        &db,
        "planning-1",
        "NodeAdded",
        "planning:1",
        1_700_000_000_050,
    )
    .await;

    let coh_max = oracle::max_timestamp_for_prefix(&db, "NodeAdded", "GB-COH:")
        .await
        .expect("max_timestamp_for_prefix (GB-COH:, NodeAdded) failed");
    assert_eq!(coh_max, Some(1_700_000_000_300));

    let cf_added_max = oracle::max_timestamp_for_prefix(&db, "NodeAdded", "CF:")
        .await
        .expect("max_timestamp_for_prefix (CF:, NodeAdded) failed");
    assert_eq!(cf_added_max, Some(1_700_000_000_200));

    let cf_updated_max = oracle::max_timestamp_for_prefix(&db, "NodeUpdated", "CF:")
        .await
        .expect("max_timestamp_for_prefix (CF:, NodeUpdated) failed");
    assert_eq!(cf_updated_max, Some(1_700_000_000_999));

    let planning_max = oracle::max_timestamp_for_prefix(&db, "NodeAdded", "planning:")
        .await
        .expect("max_timestamp_for_prefix (planning:, NodeAdded) failed");
    assert_eq!(planning_max, Some(1_700_000_000_050));

    // A prefix with no matching rows at all: genuine None, not a fabricated 0.
    let absent = oracle::max_timestamp_for_prefix(&db, "NodeAdded", "GB-CO:")
        .await
        .expect("max_timestamp_for_prefix (absent prefix) failed");
    assert_eq!(absent, None);
}
