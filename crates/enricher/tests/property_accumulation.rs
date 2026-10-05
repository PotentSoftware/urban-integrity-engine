//! Property Accumulation Anomaly detector integration tests (PROP-03, Plan 18-04).
//!
//! Tests require a live SurrealDB instance at http://localhost:8000.
//! Gated behind `URBAN_DB_TESTS=1` — when unset every test returns early (skip-by-return).
//!
//! Test coverage:
//! - `test_property_accumulation_fires`: POSITIVE — Active entity + 4 OwnsProperty edges
//!   + 1 AwardedTo edge (ratio 8× > 5×) → exactly one HIGH MotifDetected alert.
//! - `test_property_accumulation_inactive_skip`: NEGATIVE (D-11) — InLiquidation entity
//!   → ZERO alerts.
//! - `test_property_accumulation_candidate_suppressed`: NEGATIVE (ENR-11) — entity has
//!   edges but no NodeAdded row → status unknown → ZERO alerts.
//! - `test_property_accumulation_no_contracts`: NEGATIVE (D-10) — Active entity with 4
//!   properties but zero contracts → ZERO alerts.
//!
//! Run with:
//!   URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher property_accumulation 2>&1

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_enricher::property_accumulation::run_property_accumulation_detector;
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

/// Connect to SurrealDB with root credentials; panics with a clear message on failure.
async fn connect_db() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect("integrity").await
}

// ── Seed helpers ──────────────────────────────────────────────────────────────

/// Serialise a REAL `NodeWeight`, not a hand-built JSON shape.
///
/// This fixture used to emit `{"type":"Entity","data":{"status":{"type":"…"}}}` — a shape
/// `NodeWeight` never produces, since it is `#[serde(tag = "type")]` and has no `data` key.
/// The detector read `parsed["data"]["status"]["type"]`, so fixture and parser agreed on a
/// format that exists nowhere in the database, and the D-11 negative test passed while the
/// live gate never skipped a single entity. A fixture that hand-builds a payload cannot
/// catch a parser that misreads the real one — round-trip the actual type.
fn make_entity_node(entity_id: &str, status_type: &str) -> serde_json::Value {
    let timestamp = chrono::Utc::now().timestamp_millis() as u64;
    let event_id = uuid::Uuid::new_v4().to_string();
    let status = match status_type {
        "Active" => EntityStatus::Active,
        "Dissolved" => EntityStatus::Dissolved(chrono::Utc::now()),
        "InLiquidation" => EntityStatus::InLiquidation(chrono::Utc::now()),
        other => panic!("unknown status fixture: {other}"),
    };
    let weight_json = serde_json::to_string(&NodeWeight::Entity(EntityData {
        name: format!("Fixture {entity_id}"),
        company_number: None,
        status,
        incorporation_date: None,
    }))
    .expect("NodeWeight must serialise");
    serde_json::json!({
        "event_id": event_id,
        "timestamp": timestamp,
        "kind": "NodeAdded",
        "payload": { "entity_id": entity_id, "weight_json": weight_json }
    })
}

fn make_owned_property_edge(entity_id: &str, title: &str, price: f64) -> serde_json::Value {
    let event_id = uuid::Uuid::new_v4().to_string();
    let weight_json = serde_json::json!({
        "type": "OwnsProperty",
        "data": { "title_number": title, "price_paid": price }
    })
    .to_string();
    serde_json::json!({
        "event_id": event_id,
        "timestamp": chrono::Utc::now().timestamp_millis() as u64,
        "kind": "EdgeAdded",
        "payload": {
            "src_id": entity_id,
            "dst_id": format!("property:{}", title),
            "weight_json": weight_json
        }
    })
}

fn make_awarded_contract_edge(entity_id: &str, contract_id: &str, value: f64) -> serde_json::Value {
    let event_id = uuid::Uuid::new_v4().to_string();
    let weight_json = serde_json::json!({
        "type": "AwardedTo",
        "data": { "value_gbp": value, "details": contract_id }
    })
    .to_string();
    serde_json::json!({
        "event_id": event_id,
        "timestamp": chrono::Utc::now().timestamp_millis() as u64,
        "kind": "EdgeAdded",
        "payload": {
            "src_id": entity_id,
            "dst_id": format!("contract:{}", contract_id),
            "weight_json": weight_json
        }
    })
}

async fn seed_record(db: &Surreal<Any>, record: serde_json::Value) {
    let _: surrealdb::Result<Vec<serde_json::Value>> = db
        .query("CREATE graph_events CONTENT $r")
        .bind(("r", record))
        .await
        .and_then(|mut r| r.take(0));
}

/// Clean up test-seeded rows so repeated runs stay idempotent.
async fn cleanup_test_data(db: &Surreal<Any>, entity_prefix: &str) {
    let _ = db
        .query(
            "DELETE graph_events \
             WHERE payload.entity_id CONTAINS $p \
                OR payload.src_id CONTAINS $p \
                OR payload.dst_id CONTAINS $p",
        )
        .bind(("p", entity_prefix))
        .await;
    let _ = db
        .query(
            // result_json is unique to MotifDetected rows; avoid the indexed-kind +
            // CONTAINS planner pitfall (see the SELECT note in the tests below).
            "DELETE graph_events \
             WHERE payload.result_json CONTAINS $p",
        )
        .bind(("p", entity_prefix))
        .await;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// POSITIVE test (PROP-03):
///
/// Active entity + 4 OwnsProperty edges (£100k each → £400k total) + 1 AwardedTo
/// edge (£50k). Ratio = 8× > 5×. D-09, D-10, D-11 all pass.
/// Expect exactly 1 MotifDetected with motif_type="property_accumulation_anomaly"
/// and risk_score=0.75.
#[tokio::test]
async fn test_property_accumulation_fires() {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return;
    }
    let db = connect_db().await;
    let entity_id = "GB-COH:TEST0001";

    // Clean any leftover rows from prior runs.
    cleanup_test_data(&db, entity_id).await;

    // Seed: Active entity node.
    seed_record(&db, make_entity_node(entity_id, "Active")).await;

    // Seed: 4 OwnsProperty edges (£100k each).
    for i in 1..=4 {
        seed_record(
            &db,
            make_owned_property_edge(entity_id, &format!("TITLE{i:04}"), 100_000.0),
        )
        .await;
    }

    // Seed: 1 AwardedTo edge (£50k). Ratio = 400k / 50k = 8× > 5×.
    seed_record(
        &db,
        make_awarded_contract_edge(entity_id, "CON-0001", 50_000.0),
    )
    .await;

    // Run detector — may write alerts for other parallel test entities too; we only
    // check that THIS entity produced an alert.
    run_property_accumulation_detector(&db, "test-run")
        .await
        .expect("detector should not error");

    // Verify an alert for THIS entity was written.
    let rows: Vec<serde_json::Value> = db
        .query(
            // Planner-bug-safe: filter by `result_json` alone (it is unique to
            // MotifDetected rows). Combining indexed `kind` equality with a CONTAINS
            // scan makes `SELECT <fields>` return 0 rows in this SurrealDB version
            // (count() is unaffected) — the same index-planner bug seen in the
            // entity --ch-number lookup. Without this, the negative tests below pass
            // vacuously (every query returns 0).
            "SELECT payload FROM graph_events \
             WHERE payload.result_json CONTAINS $p \
             LIMIT 1",
        )
        .bind(("p", entity_id))
        .await
        .and_then(|mut r| r.take(0))
        .expect("query should succeed");

    assert!(
        !rows.is_empty(),
        "Expected a MotifDetected row for {entity_id} in graph_events"
    );
    let result_json_str = rows[0]["payload"]["result_json"]
        .as_str()
        .expect("result_json must be a string");
    let result: serde_json::Value =
        serde_json::from_str(result_json_str).expect("result_json must be valid JSON");
    assert_eq!(
        result["motif_type"].as_str(),
        Some("property_accumulation_anomaly"),
        "motif_type must be property_accumulation_anomaly"
    );
    let risk_score = result["risk_score"]
        .as_f64()
        .expect("risk_score must be f64");
    assert!(
        (risk_score - 0.75).abs() < f64::EPSILON,
        "risk_score must be 0.75 (HIGH); got {risk_score}"
    );

    cleanup_test_data(&db, entity_id).await;
}

/// NEGATIVE test — D-11 (InLiquidation skip):
///
/// Entity has InLiquidation status + 4 OwnsProperty + 1 AwardedTo (ratio 8×).
/// Expect ZERO alerts for THIS entity (InLiquidation entities are skipped per D-11).
///
/// Note: other parallel tests may seed Active entities that do fire — we check only
/// that TEST0002 specifically produces no MotifDetected alert.
#[tokio::test]
async fn test_property_accumulation_inactive_skip() {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return;
    }
    let db = connect_db().await;
    let entity_id = "GB-COH:TEST0002";

    cleanup_test_data(&db, entity_id).await;

    // Seed: InLiquidation entity node.
    seed_record(&db, make_entity_node(entity_id, "InLiquidation")).await;

    // Seed: 4 OwnsProperty edges + 1 AwardedTo (same ratio as fires test).
    for i in 1..=4 {
        seed_record(
            &db,
            make_owned_property_edge(entity_id, &format!("TITLEIQ{i:04}"), 100_000.0),
        )
        .await;
    }
    seed_record(
        &db,
        make_awarded_contract_edge(entity_id, "CON-IQ-0001", 50_000.0),
    )
    .await;

    // Run detector — may fire for other Active entities in the DB; we only check ours.
    run_property_accumulation_detector(&db, "test-run")
        .await
        .expect("detector should not error");

    // Assert no MotifDetected alert for THIS entity.
    let rows: Vec<serde_json::Value> = db
        .query(
            // Planner-bug-safe, matching the positive test above: filter on `result_json`
            // alone. Combining indexed `kind` equality with a CONTAINS scan makes
            // `SELECT <fields>` return 0 rows in this SurrealDB version, which for a
            // `rows.is_empty()` assertion means passing without testing anything. The
            // positive test was already written this way and its comment names this exact
            // risk for "the negative tests below" — they had kept the unsafe form.
            "SELECT id FROM graph_events \
             WHERE payload.result_json CONTAINS $p \
             LIMIT 1",
        )
        .bind(("p", entity_id))
        .await
        .and_then(|mut r| r.take(0))
        .expect("query should succeed");

    assert!(
        rows.is_empty(),
        "D-11: InLiquidation entity must not fire — found MotifDetected for {entity_id}"
    );

    cleanup_test_data(&db, entity_id).await;
}

/// NEGATIVE test — ENR-11 (candidate-only entity suppressed):
///
/// Entity has edges (OwnsProperty + AwardedTo) but NO NodeAdded event — only a
/// candidate-status identity_link (confidence=0.72). The detector loads EntityStatus
/// from NodeAdded/NodeUpdated only; no NodeAdded → status unknown → NotActive → zero
/// alerts.
///
/// ENR-11: entity only reachable via candidate link must not fire.
#[tokio::test]
async fn test_property_accumulation_candidate_suppressed() {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return;
    }
    let db = connect_db().await;
    let entity_id = "GB-COH:TEST0003";

    cleanup_test_data(&db, entity_id).await;

    // Deliberately DO NOT seed a NodeAdded event for this entity.
    // Only seed a candidate identity_link (confidence=0.72, status='candidate').
    let evidence = "{\"name_slug\":\"test-cand\",\"score\":0.72}".to_string();
    let _: surrealdb::Result<Vec<serde_json::Value>> = db
        .query(
            "INSERT INTO identity_links { \
               source_node_id: $src, \
               target_node_id: $tgt, \
               resolution_type: $res_type, \
               confidence: $confidence, \
               source: $source, \
               resolved_at: time::now(), \
               evidence_json: $evidence_json, \
               status: $status \
             }",
        )
        .bind(("src", format!("prop-source:{entity_id}")))
        .bind(("tgt", entity_id))
        .bind(("res_type", "name_only"))
        .bind(("confidence", 0.72_f64))
        .bind(("source", "ch_disqualified"))
        .bind(("evidence_json", evidence))
        .bind(("status", "candidate"))
        .await
        .and_then(|mut r| r.take(0));

    // Seed: 4 OwnsProperty edges + 1 AwardedTo (would trigger if entity were Active).
    for i in 1..=4 {
        seed_record(
            &db,
            make_owned_property_edge(entity_id, &format!("TITLECAND{i:04}"), 100_000.0),
        )
        .await;
    }
    seed_record(
        &db,
        make_awarded_contract_edge(entity_id, "CON-CAND-0001", 50_000.0),
    )
    .await;

    // Run detector — may fire for other Active entities in the DB; we only check ours.
    run_property_accumulation_detector(&db, "test-run")
        .await
        .expect("detector should not error");

    // Assert no MotifDetected alert for THIS entity.
    let rows: Vec<serde_json::Value> = db
        .query(
            // Planner-bug-safe, matching the positive test above: filter on `result_json`
            // alone. Combining indexed `kind` equality with a CONTAINS scan makes
            // `SELECT <fields>` return 0 rows in this SurrealDB version, which for a
            // `rows.is_empty()` assertion means passing without testing anything. The
            // positive test was already written this way and its comment names this exact
            // risk for "the negative tests below" — they had kept the unsafe form.
            "SELECT id FROM graph_events \
             WHERE payload.result_json CONTAINS $p \
             LIMIT 1",
        )
        .bind(("p", entity_id))
        .await
        .and_then(|mut r| r.take(0))
        .expect("query should succeed");

    assert!(
        rows.is_empty(),
        "ENR-11: entity only reachable via candidate link must not fire — found MotifDetected for {entity_id}"
    );

    cleanup_test_data(&db, entity_id).await;
    // Clean identity_links for this test entity.
    let _ = db
        .query("DELETE identity_links WHERE target_node_id = $id")
        .bind(("id", entity_id))
        .await;
}

/// NEGATIVE test — D-10 (zero contracts):
///
/// Active entity with 4 OwnsProperty edges but zero AwardedTo edges.
/// Expect ZERO alerts (no denominator for ratio; D-10 requires ≥1 contract).
#[tokio::test]
async fn test_property_accumulation_no_contracts() {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return;
    }
    let db = connect_db().await;
    let entity_id = "GB-COH:TEST0004";

    cleanup_test_data(&db, entity_id).await;

    // Seed: Active entity node.
    seed_record(&db, make_entity_node(entity_id, "Active")).await;

    // Seed: 4 OwnsProperty edges — but no AwardedTo edge.
    for i in 1..=4 {
        seed_record(
            &db,
            make_owned_property_edge(entity_id, &format!("TITLENOC{i:04}"), 100_000.0),
        )
        .await;
    }

    // Run detector — may fire for other Active entities in the DB; we only check ours.
    run_property_accumulation_detector(&db, "test-run")
        .await
        .expect("detector should not error");

    // Assert no MotifDetected alert for THIS entity.
    let rows: Vec<serde_json::Value> = db
        .query(
            // Planner-bug-safe, matching the positive test above: filter on `result_json`
            // alone. Combining indexed `kind` equality with a CONTAINS scan makes
            // `SELECT <fields>` return 0 rows in this SurrealDB version, which for a
            // `rows.is_empty()` assertion means passing without testing anything. The
            // positive test was already written this way and its comment names this exact
            // risk for "the negative tests below" — they had kept the unsafe form.
            "SELECT id FROM graph_events \
             WHERE payload.result_json CONTAINS $p \
             LIMIT 1",
        )
        .bind(("p", entity_id))
        .await
        .and_then(|mut r| r.take(0))
        .expect("query should succeed");

    assert!(
        rows.is_empty(),
        "D-10: entity with zero contracts must not fire — found MotifDetected for {entity_id}"
    );

    cleanup_test_data(&db, entity_id).await;
}

/// WR-03: an entity with no status row of its own must not borrow a longer id's Active status.
///
/// The D-11 gate matched with `*eid == entity_id || eid.contains(entity_id) || entity_id
/// .contains(eid)`, so `GB-COH:TEST0009` matched `GB-COH:TEST00091`. Here only the LONGER id
/// carries a status node, so pre-fix the shorter one inherits its Active status and fires;
/// post-fix the exact `.get` misses, the status defaults to NotActive, and it is skipped.
///
/// Deterministic in both directions: with a single status row there is no HashMap-order
/// ambiguity about which row a substring `.find()` would reach.
#[tokio::test]
async fn a_prefix_id_does_not_borrow_a_longer_entitys_active_status() {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return;
    }
    let db = connect_db().await;
    let short_id = "GB-COH:TEST0009";
    let long_id = "GB-COH:TEST00091";

    cleanup_test_data(&db, short_id).await;

    // Only the LONGER id gets a status node. The shorter id has none.
    seed_record(&db, make_entity_node(long_id, "Active")).await;

    // The shorter id gets the property/contract profile that would fire if it read Active:
    // 4 x 100k owned = 400k > 5 x 50k contract = 250k.
    for i in 1..=4 {
        seed_record(
            &db,
            make_owned_property_edge(short_id, &format!("TITLEPFX{i:04}"), 100_000.0),
        )
        .await;
    }
    seed_record(
        &db,
        make_awarded_contract_edge(short_id, "CON-PFX-0001", 50_000.0),
    )
    .await;

    run_property_accumulation_detector(&db, "test-run")
        .await
        .expect("detector should not error");

    // Count in Rust rather than with `kind = ... AND payload.result_json CONTAINS ...`: that
    // pairing is the documented planner-bug shape, and it returns 0 rows when it bites — which
    // would make this negative assertion pass vacuously.
    let rows = urban_integrity_core::persistence::scan::scan_events_by_kind(&db, "MotifDetected")
        .await
        .expect("alert scan failed");
    let hits = rows
        .iter()
        .filter(|r| {
            r["payload"]["result_json"]
                .as_str()
                .is_some_and(|s| s.contains(short_id) && !s.contains(long_id))
        })
        .count();

    assert_eq!(
        hits, 0,
        "{short_id} has no status row of its own; it must not inherit {long_id}'s Active status"
    );

    cleanup_test_data(&db, short_id).await;
}
