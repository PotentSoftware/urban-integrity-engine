//! Evasion detector integration tests (Task 2 — CHA-04).
//!
//! Tests require a live SurrealDB instance at http://localhost:8000.
//! Gated behind `URBAN_DB_TESTS=1` — when unset every test returns early (skip-by-return).
//!
//! Test coverage:
//! - `evasion_fires`: POSITIVE — active officer + 0.95 auto_linked link + today in window
//!   → exactly one MotifDetected row with motif_type="disqualified_director_evasion", risk_score=1.0
//! - `evasion_candidate_suppressed`: NEGATIVE (ENR-11) — 0.72 candidate-status link
//!   → ZERO alerts (status filter excludes candidates)
//! - `evasion_window_guard`: officer outside ban window OR EntityStatus dissolved → ZERO alerts
//!
//! Run with: URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher evasion_fires evasion_candidate_suppressed evasion_window_guard

use chrono::{Duration, Utc};
use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_enricher::evasion::{compute_motif_hash, run_evasion_detector};
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight, OfficerData};

/// Matches `donation_correlation_endpoint.rs`: exact `"1"`, read at run time, plus a
/// reachability probe.
///
/// This was `option_env!("URBAN_DB_TESTS").is_some()`, a **compile-time** constant, which is
/// wrong in both directions. It enabled on any value, so `URBAN_DB_TESTS=0` ran these tests
/// while every sibling file skipped; and because the value is baked into the test binary by
/// whichever build produced it, a `target/` compiled without the variable kept it `false` even
/// when CI exported `URBAN_DB_TESTS=1` — all three DB tests here would skip while the summary
/// still read `3 passed`. CI exists precisely so the gated suite cannot silently rot.
fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Connect to SurrealDB with root credentials; panics with a clear message on failure.
///
/// Mirrors the pattern in `crates/cli/src/db.rs` — signs in as root before
/// selecting the namespace/database.
async fn connect_db() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect("integrity").await
}

// ── Seed helpers ──────────────────────────────────────────────────────────────

/// Seed a NodeAdded event for an active officer into graph_events.
///
/// Serialises a real `NodeWeight::Officer`. Note it carries NO status: `OfficerData` has no
/// such field, so `load_officer_status` falls through to its Active default. This fixture
/// used to hand-build `{"type":"Officer","data":{"status":{"type":"Active"}}}` — a shape the
/// type cannot produce, which agreed with the detector's equally wrong `["data"]["status"]`
/// path and so proved nothing.
async fn seed_active_officer(db: &Surreal<Any>, officer_id: &str) {
    let weight_json = serde_json::to_string(&NodeWeight::Officer(OfficerData {
        name: "Test Officer".to_string(),
        officer_role: Some("director".to_string()),
        appointed_on: None,
        resigned_on: None,
        company_number: None,
    }))
    .expect("NodeWeight must serialise");

    let record = serde_json::json!({
        "event_id": format!("test-officer-{}", uuid::Uuid::new_v4()),
        "timestamp": Utc::now().timestamp_millis() as u64,
        "kind": "NodeAdded",
        "payload": {
            "entity_id": officer_id,
            "weight_json": weight_json,
        }
    });

    // `.expect` not a discarded Result: a failed seed left the negative tests asserting
    // `count == 0` against a database that was never seeded, so they passed vacuously and the
    // ENR-11 gate was untested while green.
    db.query("CREATE graph_events CONTENT $r")
        .bind(("r", record))
        .await
        .expect("seed write failed")
        .check()
        .expect("seed write rejected by schema");
}

/// Seed a NodeAdded event for a director whose ENTITY is dissolved.
///
/// "Dissolved officer" is a category error — officers carry no status. The status lives on
/// `EntityData`, and planning-portal directors are modelled as Entity nodes, which is why
/// `load_officer_status` scans both `"type":"Officer"` and `"type":"Entity"`. So the
/// not-active case must be a real dissolved Entity; the old fixture invented a status field
/// on an Officer, which cannot occur and which the buggy parser happened to read.
async fn seed_dissolved_officer(db: &Surreal<Any>, officer_id: &str) {
    let weight_json = serde_json::to_string(&NodeWeight::Entity(EntityData {
        name: "Dissolved Officer".to_string(),
        company_number: None,
        status: EntityStatus::Dissolved(
            chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
        incorporation_date: None,
    }))
    .expect("NodeWeight must serialise");

    let record = serde_json::json!({
        "event_id": format!("test-dissolved-{}", uuid::Uuid::new_v4()),
        "timestamp": Utc::now().timestamp_millis() as u64,
        "kind": "NodeAdded",
        "payload": {
            "entity_id": officer_id,
            "weight_json": weight_json,
        }
    });

    // `.expect` not a discarded Result: a failed seed left the negative tests asserting
    // `count == 0` against a database that was never seeded, so they passed vacuously and the
    // ENR-11 gate was untested while green.
    db.query("CREATE graph_events CONTENT $r")
        .bind(("r", record))
        .await
        .expect("seed write failed")
        .check()
        .expect("seed write rejected by schema");
}

/// Seed a DisqualifiedFrom EdgeAdded event.
///
/// `today_in_window`: if true, the ban window contains today (POSITIVE test).
/// If false, the window is in the past (WINDOW-GUARD test).
async fn seed_disqualified_from_edge(
    db: &Surreal<Any>,
    officer_id: &str,
    disq_node_id: &str,
    case_number: &str,
    today_in_window: bool,
) {
    let (disq_from, disq_until) = if today_in_window {
        // Window is [today - 1 year, today + 9 years] — today is inside.
        let from = (Utc::now() - Duration::days(365))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let until = (Utc::now() + Duration::days(3650))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        (from, until)
    } else {
        // Window ended 1 year ago — today is OUTSIDE the window.
        let from = (Utc::now() - Duration::days(3650))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let until = (Utc::now() - Duration::days(365))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        (from, until)
    };

    let weight_json = serde_json::json!({
        "type": "DisqualifiedFrom",
        "meta": {
            "created_at": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "effective_from": disq_from,
            "effective_to": disq_until,
        },
        "data": {
            "disqualified_from": disq_from,
            "disqualified_until": disq_until,
            "case_number": case_number,
        }
    })
    .to_string();

    let record = serde_json::json!({
        "event_id": format!("test-edge-{}", uuid::Uuid::new_v4()),
        "timestamp": Utc::now().timestamp_millis() as u64,
        "kind": "EdgeAdded",
        "payload": {
            "src_id": officer_id,
            "dst_id": disq_node_id,
            "weight_json": weight_json,
        }
    });

    // `.expect` not a discarded Result: a failed seed left the negative tests asserting
    // `count == 0` against a database that was never seeded, so they passed vacuously and the
    // ENR-11 gate was untested while green.
    db.query("CREATE graph_events CONTENT $r")
        .bind(("r", record))
        .await
        .expect("seed write failed")
        .check()
        .expect("seed write rejected by schema");
}

/// Seed an identity_link with the given status.
///
/// `status`: "auto_linked" | "confirmed" | "candidate"
async fn seed_identity_link(
    db: &Surreal<Any>,
    source_node_id: &str,
    target_node_id: &str,
    confidence: f64,
    status: &str,
) {
    // Use bound-param INSERT — mirrors the write_identity_link pattern in ch_psc.rs.
    // Pass resolved_at as a DateTime<Utc> so SurrealDB accepts it as a datetime type.
    let evidence = format!("{{\"name_slug\":\"test-officer\",\"score\":{confidence}}}");
    db.query(
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
    .bind(("src", source_node_id))
    .bind(("tgt", target_node_id))
    .bind(("res_type", "name+dob_full"))
    .bind(("confidence", confidence))
    .bind(("source", "ch_disqualified"))
    .bind(("evidence_json", evidence))
    .bind(("status", status))
    .await
    .expect("identity_links seed failed")
    .check()
    .expect("identity_links seed rejected by schema");
}

/// Count MotifDetected alerts carrying `motif_hash`, filtering kind in Rust.
///
/// The previous form combined indexed `kind = 'MotifDetected'` with an unindexed
/// `payload.result_json CONTAINS $frag`. That pairing is the documented SurrealDB 3.x planner
/// bug (docs/DEVELOPMENT.md Standing Rule 4, the `entity --ch-number` incident): it returns 0 rows while `count()` reports
/// the true total, which made every negative test here pass vacuously a second way. A single
/// indexed `kind` equality with no CONTAINS is safe, so scan by kind and compare in Rust — the
/// same workaround the production code uses.
async fn count_evasion_alerts(db: &Surreal<Any>, motif_hash: &str) -> usize {
    let rows = urban_integrity_core::persistence::scan::scan_events_by_kind(db, "MotifDetected")
        .await
        .expect("alert scan failed");
    let needle = format!("\"motif_hash\":\"{motif_hash}\"");
    rows.iter()
        .filter(|r| {
            r["payload"]["result_json"]
                .as_str()
                .is_some_and(|s| s.contains(&needle))
        })
        .count()
}

/// Seed an Entity node-weight event of `kind` carrying `status`.
///
/// `NodeAdded` is the birth the streaming ingest writes; `NodeUpdated` is the correction the
/// coordinator's UpsertNode path writes, carrying the FULL merged weight at update time with a
/// later timestamp. Both shapes are needed to exercise the fold.
async fn seed_entity_status_event(
    db: &Surreal<Any>,
    officer_id: &str,
    kind: &str,
    ts_offset_secs: i64,
    status: EntityStatus,
) {
    let weight_json = serde_json::to_string(&NodeWeight::Entity(EntityData {
        name: "Test Officer".to_string(),
        company_number: None,
        status,
        incorporation_date: None,
    }))
    .expect("NodeWeight must serialise");

    let record = serde_json::json!({
        "event_id": format!("test-status-{}", uuid::Uuid::new_v4()),
        "timestamp": (Utc::now() + Duration::seconds(ts_offset_secs)).timestamp_millis() as u64,
        "kind": kind,
        "payload": {
            "entity_id": officer_id,
            "weight_json": weight_json,
        }
    });

    db.query("CREATE graph_events CONTENT $r")
        .bind(("r", record))
        .await
        .expect("seed write failed")
        .check()
        .expect("seed write rejected by schema");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// POSITIVE test (CHA-04 success criterion 2):
///
/// Seed an Active officer with a 0.95 auto_linked ch_disqualified link and a
/// DisqualifiedFrom edge whose window contains today. Expect exactly one
/// MotifDetected row with motif_type="disqualified_director_evasion" and risk_score=1.0.
#[tokio::test]
async fn evasion_fires() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;

    // Use unique IDs to isolate this test from other runs.
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let officer_id = format!("director:test-evasion-fires-{run_id}");
    let disq_node_id = format!("disq:test-disq-fires-{run_id}");
    let case_number = format!("INS-TEST-{run_id}");

    // Seed: Active officer node
    seed_active_officer(&db, &officer_id).await;

    // Seed: DisqualifiedFrom edge — window contains today (POSITIVE)
    seed_disqualified_from_edge(&db, &officer_id, &disq_node_id, &case_number, true).await;

    // Seed: auto_linked identity_link at confidence 0.95
    seed_identity_link(&db, &disq_node_id, &officer_id, 0.95, "auto_linked").await;

    // Run the evasion detector
    let alerts = run_evasion_detector(&db, "test-run")
        .await
        .expect("evasion detector should not error");

    // Verify: at least one alert was written (this test's alert; others may exist in DB)
    assert!(
        alerts >= 1,
        "Expected at least 1 alert written; got {alerts}"
    );

    // Verify: at least one alert with THIS test's motif_hash exists.
    // count >= 1 rather than == 1 because concurrent test runs (--test-threads > 1)
    // can race and produce a duplicate before the dedup check completes — this is a
    // test-isolation issue, not a production bug. The dedup invariant is proven by
    // running `--test-threads=1`: sequential runs produce exactly 1 alert per hash.
    let motif_hash = compute_motif_hash(&officer_id, &case_number);
    let count = count_evasion_alerts(&db, &motif_hash).await;
    assert!(
        count >= 1,
        "Expected at least 1 alert for this officer+case; got {count}"
    );

    // Verify: the alert has motif_type="disqualified_director_evasion" and risk_score=1.0.
    //
    // Scanned by kind and filtered in Rust, like `count_evasion_alerts` above. The previous
    // form paired indexed `kind` equality with an unindexed CONTAINS — the row-losing shape
    // `query_shape_guard.rs` rejects everywhere except test files, which it skips so that
    // regression tests can still demonstrate it. Here the assertion is positive, so the bug
    // would have failed loudly rather than silently; it is fixed for consistency with the rest
    // of the file, and so no one copies the shape out of a passing test.
    let needle = format!("\"motif_hash\":\"{motif_hash}\"");
    let rows = urban_integrity_core::persistence::scan::scan_events_by_kind(&db, "MotifDetected")
        .await
        .expect("alert scan failed");
    let row = rows
        .iter()
        .find(|r| {
            r["payload"]["result_json"]
                .as_str()
                .is_some_and(|s| s.contains(&needle))
        })
        .expect("Alert row must exist in graph_events");
    let result_json_str = row["payload"]["result_json"]
        .as_str()
        .expect("result_json must be a string");
    let result: serde_json::Value =
        serde_json::from_str(result_json_str).expect("result_json must be valid JSON");
    assert_eq!(
        result["motif_type"].as_str(),
        Some("disqualified_director_evasion")
    );
    let risk_score = result["risk_score"]
        .as_f64()
        .expect("risk_score must be f64");
    assert!(
        (risk_score - 1.0).abs() < f64::EPSILON,
        "risk_score must be 1.0 (forces CRITICAL); got {risk_score}"
    );
}

/// NEGATIVE test (ENR-11 success criterion 4):
///
/// Seed an Active officer with a confidence-0.72 CANDIDATE-status link.
/// The status filter (`status IN ['auto_linked','confirmed']`) must exclude it.
/// Expect ZERO alert rows for this officer.
#[tokio::test]
async fn evasion_candidate_suppressed() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let officer_id = format!("director:test-evasion-candidate-{run_id}");
    let disq_node_id = format!("disq:test-disq-candidate-{run_id}");
    let case_number = format!("INS-CANDIDATE-{run_id}");

    // Seed: Active officer node
    seed_active_officer(&db, &officer_id).await;

    // Seed: DisqualifiedFrom edge — window contains today
    seed_disqualified_from_edge(&db, &officer_id, &disq_node_id, &case_number, true).await;

    // Seed: CANDIDATE link at confidence 0.72 (below AUTO_LINK_THRESHOLD 0.85)
    // The status is "candidate" — ENR-11 filter must exclude this from the detector.
    seed_identity_link(&db, &disq_node_id, &officer_id, 0.72, "candidate").await;

    // Run the evasion detector
    let _alerts = run_evasion_detector(&db, "test-run")
        .await
        .expect("evasion detector should not error");

    // Verify: ZERO alerts for this specific officer+case (candidate link excluded)
    let motif_hash = compute_motif_hash(&officer_id, &case_number);
    let count = count_evasion_alerts(&db, &motif_hash).await;
    assert_eq!(
        count, 0,
        "Candidate link (status='candidate') must NEVER fire an alert (ENR-11); got {count} alerts"
    );
}

/// WINDOW-GUARD test:
///
/// Case A: Active officer with auto_linked link but ban window ended 1 year ago → ZERO alerts.
/// Case B: Dissolved officer with auto_linked link and today-in-window → ZERO alerts.
#[tokio::test]
async fn evasion_window_guard() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;

    // ── Case A: officer outside the ban window ────────────────────────────────
    {
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let officer_id = format!("director:test-evasion-window-a-{run_id}");
        let disq_node_id = format!("disq:test-disq-window-a-{run_id}");
        let case_number = format!("INS-WINDOW-A-{run_id}");

        seed_active_officer(&db, &officer_id).await;
        // Window is in the PAST — today is outside it.
        seed_disqualified_from_edge(&db, &officer_id, &disq_node_id, &case_number, false).await;
        seed_identity_link(&db, &disq_node_id, &officer_id, 0.95, "auto_linked").await;

        let _alerts = run_evasion_detector(&db, "test-run")
            .await
            .expect("evasion detector should not error");

        let motif_hash = compute_motif_hash(&officer_id, &case_number);
        let count = count_evasion_alerts(&db, &motif_hash).await;
        assert_eq!(
            count, 0,
            "Officer outside ban window must NOT fire alert; got {count} (Case A)"
        );
    }

    // ── Case B: dissolved officer (in-window but not Active) ─────────────────
    {
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let officer_id = format!("director:test-evasion-window-b-{run_id}");
        let disq_node_id = format!("disq:test-disq-window-b-{run_id}");
        let case_number = format!("INS-WINDOW-B-{run_id}");

        // Dissolved officer
        seed_dissolved_officer(&db, &officer_id).await;
        // Window contains today
        seed_disqualified_from_edge(&db, &officer_id, &disq_node_id, &case_number, true).await;
        seed_identity_link(&db, &disq_node_id, &officer_id, 0.95, "auto_linked").await;

        let _alerts = run_evasion_detector(&db, "test-run")
            .await
            .expect("evasion detector should not error");

        let motif_hash = compute_motif_hash(&officer_id, &case_number);
        let count = count_evasion_alerts(&db, &motif_hash).await;
        assert_eq!(
            count, 0,
            "Dissolved officer must NOT fire alert (not Active); got {count} (Case B)"
        );
    }
}

/// NEGATIVE (WR-02): an officer dissolved by a `NodeUpdated` correction must not alert.
///
/// `load_officer_status` scanned `NodeAdded` only, so the correction was invisible, the officer
/// read Active, and the CRITICAL disqualified-director alert fired against an entity the event
/// log knows is dissolved. The birth row here is a live Entity so the correction has something
/// to correct — reading only the birth is precisely the defect.
#[tokio::test]
async fn a_correction_dissolved_officer_does_not_alert() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let officer_id = format!("director:test-evasion-correction-{run_id}");
    let disq_node_id = format!("disq:test-disq-correction-{run_id}");
    let case_number = format!("INS-TEST-{run_id}");

    // Birth: an ACTIVE entity as a real NodeAdded row. This is what makes the test bite —
    // reading NodeAdded only finds THIS row and reports Active. (Seeding the birth as
    // NodeUpdated too would make the pre-fix scan find nothing, and `load_officer_status`
    // returns NotActive when the officer is absent, so the test would pass vacuously.)
    seed_entity_status_event(&db, &officer_id, "NodeAdded", 0, EntityStatus::Active).await;
    // Correction: dissolved, later timestamp, NodeUpdated.
    seed_entity_status_event(
        &db,
        &officer_id,
        "NodeUpdated",
        1,
        EntityStatus::Dissolved(
            chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
    )
    .await;
    seed_disqualified_from_edge(&db, &officer_id, &disq_node_id, &case_number, true).await;
    seed_identity_link(&db, &disq_node_id, &officer_id, 0.95, "auto_linked").await;

    run_evasion_detector(&db, "test-run")
        .await
        .expect("evasion detector should not error");

    let motif_hash = compute_motif_hash(&officer_id, &case_number);
    let count = count_evasion_alerts(&db, &motif_hash).await;
    assert_eq!(
        count, 0,
        "the officer's dissolution arrived as a NodeUpdated correction; reading NodeAdded only \
         makes them read Active and fires a CRITICAL alert against a dissolved entity"
    );
}

/// A shorter officer id must not pick up a longer one's status row.
///
/// The match was `entity_id.to_string().contains(officer_id)`, so `director:john` matched
/// `director:john-smith`. Here the SHORT id is dissolved and the LONG id is active: a substring
/// match returns the wrong row and the short officer reads Active, firing the alert.
#[tokio::test]
async fn a_prefix_id_does_not_borrow_another_officers_status() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let short_id = format!("director:test-prefix-{run_id}");
    let long_id = format!("{short_id}-smith");
    let disq_node_id = format!("disq:test-prefix-{run_id}");
    let case_number = format!("INS-TEST-{run_id}");

    // The LONG active row is seeded FIRST, so it has the lower record id and a substring
    // `.find()` reaches it before the short id's own row — making the pre-fix behaviour
    // deterministic rather than order-dependent.
    seed_entity_status_event(&db, &long_id, "NodeAdded", 0, EntityStatus::Active).await;
    seed_entity_status_event(
        &db,
        &short_id,
        "NodeAdded",
        1,
        EntityStatus::Dissolved(
            chrono::DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
    )
    .await;
    seed_disqualified_from_edge(&db, &short_id, &disq_node_id, &case_number, true).await;
    seed_identity_link(&db, &disq_node_id, &short_id, 0.95, "auto_linked").await;

    run_evasion_detector(&db, "test-run")
        .await
        .expect("evasion detector should not error");

    let motif_hash = compute_motif_hash(&short_id, &case_number);
    let count = count_evasion_alerts(&db, &motif_hash).await;
    assert_eq!(
        count, 0,
        "the dissolved short id must read its OWN status, not the active longer id's"
    );
}

/// The `result_json` of the single alert carrying `motif_hash`, parsed.
async fn alert_result_json(db: &Surreal<Any>, motif_hash: &str) -> serde_json::Value {
    let rows = urban_integrity_core::persistence::scan::scan_events_by_kind(db, "MotifDetected")
        .await
        .expect("alert scan failed");
    let needle = format!("\"motif_hash\":\"{motif_hash}\"");
    let raw = rows
        .iter()
        .filter_map(|r| r["payload"]["result_json"].as_str())
        .find(|s| s.contains(&needle))
        .expect("the alert this test wrote must exist");
    serde_json::from_str(raw).expect("result_json must parse")
}

/// WR-01 (phase 46 code review): a second run that re-detects the same condition must advance
/// `last_seen_run_id` on the existing row while leaving `first_seen_run_id` alone — and must
/// still not write a duplicate.
///
/// This is the end-to-end half of the fix. `advancing_last_seen_preserves_first_seen` proves
/// the JSON transform in isolation; only this proves the detector's dedup branch actually
/// reaches it against a real SurrealDB, that the `type::record` key form addresses the right
/// row, and that the UPDATE lands.
///
/// PROVE THIS CAN FAIL: revert the dedup branch in `run_evasion_detector` to a bare `continue`
/// and the `last_seen_run_id` assertion fails with run-001 still in place.
#[tokio::test]
async fn a_redetected_alert_advances_last_seen_without_duplicating() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;

    let unique = uuid::Uuid::new_v4().simple().to_string();
    let officer_id = format!("director:test-evasion-reconfirm-{unique}");
    let disq_node_id = format!("disq:test-disq-reconfirm-{unique}");
    let case_number = format!("INS-TEST-RECONFIRM-{unique}");

    seed_active_officer(&db, &officer_id).await;
    seed_disqualified_from_edge(&db, &officer_id, &disq_node_id, &case_number, true).await;
    seed_identity_link(&db, &disq_node_id, &officer_id, 0.95, "auto_linked").await;

    let motif_hash = compute_motif_hash(&officer_id, &case_number);

    // Run 1 — creates the alert, stamping both ids with this run.
    run_evasion_detector(&db, "run-001")
        .await
        .expect("first run should not error");
    let after_first = alert_result_json(&db, &motif_hash).await;
    assert_eq!(after_first["first_seen_run_id"], "run-001");
    assert_eq!(
        after_first["last_seen_run_id"], "run-001",
        "precondition: a fresh alert is last seen at its own creating run"
    );

    // Run 2 — the condition is still true, so the detector dedups. That is a re-confirmation.
    run_evasion_detector(&db, "run-002")
        .await
        .expect("second run should not error");
    let after_second = alert_result_json(&db, &motif_hash).await;

    assert_eq!(
        after_second["first_seen_run_id"], "run-001",
        "first_seen_run_id is a historical fact and must not move on re-detection"
    );
    assert_eq!(
        after_second["last_seen_run_id"], "run-002",
        "WR-01: the re-detecting run must advance last_seen_run_id, or staleness climbs \
         forever for an alert the detector confirms every single run"
    );

    // The dedup guarantee itself is unchanged — re-confirmation is an UPDATE, never a second row.
    assert_eq!(
        count_evasion_alerts(&db, &motif_hash).await,
        1,
        "re-confirmation must UPDATE in place, never CREATE a duplicate"
    );
}
