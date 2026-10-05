//! Disqualified Directors adapter offline tests (Task 1).
//!
//! Covers: payload parsing, confidence assignment, edge-meta building, DOB month+year
//! parsing (Pitfall 3), three-field-minimum invariant (CHA-02), and graceful-degradation
//! gate (CHA-03). No live SurrealDB or CH stream needed.

use urban_integrity_enricher::adapters::ch_disqualified::{
    assign_confidence, build_disq_identity_link_for_test, build_disqualified_from_edge, parse_dob,
    DisqStreamEvent, DisqualifiedAdapter, ParsedDob,
};
use urban_integrity_enricher::adapters::EnricherAdapter;
use urban_integrity_enricher::confidence::LinkStatus;
use urban_integrity_schema::types::edge::EdgeWeight;

// ── Fixture parsing ───────────────────────────────────────────────────────────

#[test]
fn disq_fixture_parses_forename_surname_dob() {
    let fixture = include_str!("fixtures/disqualified_natural.ndjson");
    let first_line = fixture
        .lines()
        .next()
        .expect("fixture has at least one line");
    let event: DisqStreamEvent = serde_json::from_str(first_line).expect("§2 fixture should parse");

    let data = event.data.expect("fixture has data");
    assert_eq!(data.forename.as_deref(), Some("John"));
    assert_eq!(data.surname.as_deref(), Some("Smith"));
    // Pitfall 3: DOB is month+year only — confirm the string is "1970-03" not a full date
    assert_eq!(
        data.date_of_birth.as_deref(),
        Some("1970-03"),
        "DOB must be month+year only (Pitfall 3 — no day)"
    );
    let dob = parse_dob(data.date_of_birth.as_deref().unwrap());
    assert_eq!(
        dob,
        Some(ParsedDob {
            year: 1970,
            month: Some(3)
        })
    );
}

#[test]
fn disq_fixture_year_only_dob_parses() {
    let fixture = include_str!("fixtures/disqualified_natural.ndjson");
    // 2nd line has DOB "1985" (year only)
    let second_line = fixture.lines().nth(1).expect("fixture has 2nd line");
    let event: DisqStreamEvent =
        serde_json::from_str(second_line).expect("2nd fixture should parse");
    let data = event.data.expect("2nd fixture has data");
    let dob = parse_dob(data.date_of_birth.as_deref().unwrap());
    assert_eq!(
        dob,
        Some(ParsedDob {
            year: 1985,
            month: None
        })
    );
}

#[test]
fn disq_fixture_no_dob_parses_as_none() {
    let fixture = include_str!("fixtures/disqualified_natural.ndjson");
    // 3rd line has no date_of_birth field
    let third_line = fixture.lines().nth(2).expect("fixture has 3rd line");
    let event: DisqStreamEvent =
        serde_json::from_str(third_line).expect("3rd fixture should parse");
    let data = event.data.expect("3rd fixture has data");
    assert!(
        data.date_of_birth.is_none(),
        "3rd fixture must have no DOB (name-only case)"
    );
}

// ── disq_confidence: confidence assignment (D-08 three-field-minimum) ────────

#[test]
fn disq_confidence_full_month_year() {
    // name + DOB month + year → 0.95
    let conf = assign_confidence(true, Some(3), Some(1970));
    assert_eq!(conf, Some(0.95));
}

#[test]
fn disq_confidence_year_only() {
    // name + year only → 0.90
    let conf = assign_confidence(true, None, Some(1970));
    assert_eq!(conf, Some(0.90));
}

#[test]
fn disq_confidence_name_only_no_link() {
    // name only (no DOB) → None → no link (ENR-11)
    // INVARIANT: three-field minimum
    let conf = assign_confidence(true, None, None);
    assert_eq!(
        conf, None,
        "Name-only match must be discarded (three-field minimum)"
    );
}

#[test]
fn disq_confidence_name_not_matched_is_none() {
    let conf = assign_confidence(false, Some(3), Some(1970));
    assert_eq!(conf, None);
}

// ── disq_edge_meta: DisqualifiedFrom edge populates ban window correctly ──────

#[test]
fn disq_edge_meta_effective_from_matches_disqualified_from() {
    use urban_integrity_enricher::adapters::ch_disqualified::Disqualification;

    let disq = Disqualification {
        disqualified_from: Some("2024-01-15".to_string()),
        disqualified_until: Some("2034-01-15".to_string()),
        case_identifier: Some("INS-2023-00123".to_string()),
    };
    let edge = build_disqualified_from_edge(&disq);
    match edge {
        EdgeWeight::DisqualifiedFrom { meta, data } => {
            // effective_from == disqualified_from (Pitfall 5 / temporal-edge invariant)
            assert!(meta.effective_from.is_some());
            assert_eq!(
                meta.effective_from.unwrap().format("%Y-%m-%d").to_string(),
                "2024-01-15"
            );
            assert!(meta.effective_to.is_some());
            assert_eq!(
                meta.effective_to.unwrap().format("%Y-%m-%d").to_string(),
                "2034-01-15"
            );
            // DisqualifiedFromData mirrors EdgeMeta
            assert_eq!(data.disqualified_from, meta.effective_from);
            assert_eq!(data.disqualified_until, meta.effective_to);
            // case_number ← case_identifier
            assert_eq!(data.case_number.as_deref(), Some("INS-2023-00123"));
        }
        _ => panic!("Expected DisqualifiedFrom edge variant"),
    }
}

#[test]
fn disq_edge_meta_case_identifier_to_case_number() {
    use urban_integrity_enricher::adapters::ch_disqualified::Disqualification;

    let disq = Disqualification {
        disqualified_from: Some("2020-06-01".to_string()),
        disqualified_until: Some("2030-06-01".to_string()),
        case_identifier: Some("CRS-2019-98765".to_string()),
    };
    let edge = build_disqualified_from_edge(&disq);
    match edge {
        EdgeWeight::DisqualifiedFrom { data, .. } => {
            assert_eq!(
                data.case_number.as_deref(),
                Some("CRS-2019-98765"),
                "case_number must equal case_identifier"
            );
        }
        _ => panic!("Expected DisqualifiedFrom edge"),
    }
}

// ── Link building: confidence reflected in ConfirmedLink ────────────────────

#[test]
fn disq_link_full_dob_score_is_095() {
    let dob = ParsedDob {
        year: 1970,
        month: Some(3),
    };
    let link = build_disq_identity_link_for_test(
        "disq:officer-id-001",
        "director:john-smith",
        "john smith",
        &dob,
    );
    assert!(link.is_some());
    let confirmed = link.unwrap();
    assert!((confirmed.score().value() - 0.95).abs() < f64::EPSILON);
    assert_eq!(confirmed.record().status, LinkStatus::AutoLinked);
    assert_eq!(confirmed.record().source, "ch_disqualified");
}

#[test]
fn disq_link_year_only_score_is_090() {
    let dob = ParsedDob {
        year: 1985,
        month: None,
    };
    let link = build_disq_identity_link_for_test(
        "disq:officer-id-002",
        "director:jane-doe",
        "jane doe",
        &dob,
    );
    assert!(link.is_some());
    let confirmed = link.unwrap();
    assert!((confirmed.score().value() - 0.90).abs() < f64::EPSILON);
}

#[test]
fn disq_link_evidence_json_is_flat_string() {
    // ENR-06: evidence_json must be a flat string, never a nested object
    let dob = ParsedDob {
        year: 1970,
        month: Some(3),
    };
    let link = build_disq_identity_link_for_test(
        "disq:officer-id-001",
        "director:john-smith",
        "john smith",
        &dob,
    );
    let confirmed = link.unwrap();
    // Must parse as valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&confirmed.record().evidence_json)
        .expect("evidence_json must be valid JSON");
    assert_eq!(parsed["name_slug"], "john smith");
    assert_eq!(parsed["dob_year"], 1970);
    assert_eq!(parsed["dob_month"], 3);
    assert_eq!(parsed["match"], "name+dob_full");
}

// ── DisqualifiedAdapter graceful degradation (CHA-03) ───────────────────────

#[test]
fn disq_adapter_disabled_when_no_key() {
    let adapter = DisqualifiedAdapter::new(None);
    assert!(!adapter.is_enabled());
}

#[test]
fn disq_adapter_disabled_when_empty_key() {
    let adapter = DisqualifiedAdapter::new(Some(String::new()));
    assert!(!adapter.is_enabled());
}

#[test]
fn disq_adapter_enabled_with_real_key() {
    let adapter = DisqualifiedAdapter::new(Some("live-api-key".to_string()));
    assert!(adapter.is_enabled());
}

#[test]
fn disq_adapter_name_is_ch_disqualified() {
    let adapter = DisqualifiedAdapter::new(None);
    assert_eq!(adapter.name(), "ch_disqualified");
}

// ── DB integration tests (URBAN_DB_TESTS=1 required) ─────────────────────────

use urban_integrity_enricher::adapters::ch_disqualified::{
    match_disqualified_officer, process_disq_line,
};

/// Whether the DB-gated tests in this file should run.
///
/// This was `const URBAN_DB_TESTS: bool = option_env!("URBAN_DB_TESTS").is_some()`, which is
/// wrong in both directions. `option_env!` is expanded at COMPILE time, so the value is baked
/// into the test binary by whichever build produced it: a `target/` built without the variable
/// keeps it `false` even when CI exports `URBAN_DB_TESTS=1`, and every DB test here skips while
/// the summary still reads `N passed`. It also enabled on ANY value, so `URBAN_DB_TESTS=0` ran
/// them while every sibling file skipped. CI exists so the gated suite cannot silently rot.
fn db_tests_enabled() -> bool {
    std::env::var("URBAN_DB_TESTS").as_deref() == Ok("1")
}

async fn connect_test_db_disq() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect("integrity").await
}

async fn seed_officer_node_disq(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    name: &str,
    run_id: &str,
) {
    let weight_json = serde_json::json!({
        "type": "Officer",
        "name": name,
    })
    .to_string();
    let record = serde_json::json!({
        "event_id": format!("test-disq-officer-{run_id}"),
        "timestamp": 1_700_000_000_000u64,
        "kind": "NodeAdded",
        "payload": {
            "entity_id": format!("director:{}", urban_integrity_core::stream::normalise_name(name).replace(' ', "-")),
            "weight_json": weight_json,
        }
    });
    let _: surrealdb::Result<Vec<serde_json::Value>> = db
        .query("CREATE graph_events CONTENT $r")
        .bind(("r", record))
        .await
        .and_then(|mut r| r.take(0));
}

/// `match_disqualified_officer` must find an officer by `director:{hyphen-slug}`.
/// Regression: pre-fix the slug used spaces which never matched hyphen-keyed nodes.
#[tokio::test]
async fn test_match_disqualified_officer_by_hyphen_slug() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_disq().await;

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let officer_name = format!("Disq Match Test {run_id}");

    seed_officer_node_disq(&db, &officer_name, &run_id).await;

    let name_slug = urban_integrity_core::stream::normalise_name(&officer_name);
    let result = match_disqualified_officer(&db, &name_slug).await;

    let expected = format!("director:{}", name_slug.replace(' ', "-"));
    assert_eq!(
        result,
        Some(expected),
        "match_disqualified_officer must return director:{{hyphen-slug}}; got {result:?}"
    );
}

/// `match_disqualified_officer` must return `None` for an unknown officer.
#[tokio::test]
async fn test_match_disqualified_officer_none_when_not_found() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_disq().await;
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let result = match_disqualified_officer(&db, &format!("nobody-{run_id}")).await;
    assert!(
        result.is_none(),
        "must return None for unknown officer slug"
    );
}

/// `process_disq_line` must write an identity_link when the officer is found and
/// DOB (year at minimum) is present.
#[tokio::test]
async fn test_process_disq_line_writes_identity_link_on_match() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_disq().await;
    let (tx, _) =
        tokio::sync::mpsc::channel::<urban_integrity_core::graph::types::GraphMutation>(64);

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let officer_name = format!("Bob Disq {run_id}");
    let forename = "Bob";
    let surname = format!("Disq {run_id}");

    seed_officer_node_disq(&db, &officer_name, &run_id).await;

    // Construct a synthetic disq line that matches the seeded officer.
    let line = serde_json::json!({
        "resource_id": format!("disq-test-{run_id}"),
        "data": {
            "kind": "natural-disqualification",
            "forename": forename,
            "surname": surname,
            "date_of_birth": "1975-06",
            "disqualifications": [{
                "disqualified_from": "2020-01-01",
                "disqualified_until": "2030-01-01",
                "case_identifier": format!("INS-TEST-{run_id}"),
            }]
        }
    })
    .to_string();

    let result = process_disq_line(&db, &tx, &line).await.unwrap();
    assert!(
        result.is_some(),
        "process_disq_line must return Some for valid line"
    );

    let processed = result.unwrap();
    assert!(
        processed.identity_link_written,
        "identity_link_written must be true when officer matched and DOB present"
    );
    assert!(
        processed.officer_match.is_some(),
        "officer_match must be Some when officer found"
    );
}

/// `process_disq_line` must discard (no identity_link) when DOB is absent —
/// the three-field-minimum invariant (ENR-11).
#[tokio::test]
async fn test_process_disq_line_name_only_discarded() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_disq().await;
    let (tx, _) =
        tokio::sync::mpsc::channel::<urban_integrity_core::graph::types::GraphMutation>(64);

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let officer_name = format!("No Dob Officer {run_id}");
    seed_officer_node_disq(&db, &officer_name, &run_id).await;

    // No date_of_birth field — must be discarded (ENR-11).
    let line = serde_json::json!({
        "resource_id": format!("disq-nodob-{run_id}"),
        "data": {
            "kind": "natural-disqualification",
            "forename": "No",
            "surname": format!("Dob Officer {run_id}"),
            "disqualifications": []
        }
    })
    .to_string();

    let result = process_disq_line(&db, &tx, &line).await.unwrap();
    // Returns Some(ProcessedDisq { identity_link_written: false, ... }) per ENR-11 path.
    if let Some(processed) = result {
        assert!(
            !processed.identity_link_written,
            "name-only match must NOT write identity_link (ENR-11)"
        );
    }
}

// ── Regression: write_disq_identity_link must propagate DB errors (2026-06-07) ─

/// Regression: write_disq_identity_link must return Err on DB failure, not Ok(()).
///
/// Mirrors the same fix in ch_psc::write_identity_link — both functions
/// previously swallowed errors (warn + Ok(())), causing the Disqualified
/// adapter to silently drop all writes when the SurrealDB session expired.
#[tokio::test]
async fn test_write_disq_identity_link_propagates_db_error() {
    if !db_tests_enabled() {
        return;
    }
    use chrono::Utc;
    use urban_integrity_enricher::adapters::ch_disqualified::write_disq_identity_link;
    use urban_integrity_enricher::confidence::{IdentityLinkRecord, LinkStatus};

    // Connect WITHOUT signing in — all queries return 401 Unauthorized.
    let db = surrealdb::engine::any::connect("http://localhost:8000")
        .await
        .expect("SurrealDB not reachable");
    // Intentionally skip signin and use_ns/use_db.

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let record = IdentityLinkRecord {
        id: format!("identity_links:err-disq-{run_id}"),
        source_node_id: format!("disq:err-test-{run_id}"),
        target_node_id: format!("director:err-test-{run_id}"),
        resolution_type: "name_dob".to_string(),
        confidence: 0.9,
        source: "ch_disqualified".to_string(),
        resolved_at: Utc::now(),
        evidence_json: "{\"regression\":true}".to_string(),
        status: LinkStatus::AutoLinked,
    };

    let result = write_disq_identity_link(&db, &record).await;
    assert!(
        result.is_err(),
        "write_disq_identity_link must propagate DB errors — previously it returned Ok(()) \
         and silently dropped writes when the session expired"
    );
}

/// Regression: the disqualified writer must honour a rejection too (2026-08-20).
///
/// `write_disq_identity_link` carried its own byte-identical copy of the identity-link
/// UPSERT, which is why the missing `status` guard had to be fixed in two places. Both now
/// share `IDENTITY_LINK_UPSERT`; this test pins that, so the copy cannot quietly come back.
///
/// A disqualified-director link is the most damaging kind to resurrect: it asserts that a
/// named person is disqualified, and rejection is the operator's only way to disown it.
#[tokio::test]
async fn a_rejected_disqualified_link_is_not_resurrected() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_enricher::adapters::ch_disqualified::write_disq_identity_link;
    use urban_integrity_enricher::confidence::IdentityLinkRecord;

    let _ns = connect_test_db_disq().await;
    let db = _ns.arc();

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let record = IdentityLinkRecord {
        id: format!("identity_links:disq-reject-{run_id}"),
        source_node_id: format!("disq:reject-{run_id}"),
        target_node_id: format!("director:disq-reject-{run_id}"),
        resolution_type: "director_slug".to_string(),
        confidence: 0.95,
        source: "ch_disqualified".to_string(),
        resolved_at: chrono::Utc::now(),
        evidence_json: "{\"regression\":\"disq-reject-survives\"}".to_string(),
        status: LinkStatus::AutoLinked,
    };

    write_disq_identity_link(&db, &record)
        .await
        .expect("first write must succeed");

    let _: Vec<serde_json::Value> = db
        .query(
            "UPDATE identity_links SET status = 'rejected' \
             WHERE source_node_id = $src AND target_node_id = $tgt AND source = $source",
        )
        .bind(("src", record.source_node_id.clone()))
        .bind(("tgt", record.target_node_id.clone()))
        .bind(("source", record.source.clone()))
        .await
        .expect("reject update must run")
        .take(0)
        .expect("reject update must return");

    write_disq_identity_link(&db, &record)
        .await
        .expect("second write must succeed");

    let rows: Vec<serde_json::Value> = db
        .query(
            "SELECT status FROM identity_links \
             WHERE source_node_id = $src AND target_node_id = $tgt AND source = $source",
        )
        .bind(("src", record.source_node_id.clone()))
        .bind(("tgt", record.target_node_id.clone()))
        .bind(("source", record.source.clone()))
        .await
        .expect("status read must run")
        .take(0)
        .expect("status read must return");

    assert_eq!(
        rows.len(),
        1,
        "must update in place, not duplicate: {rows:?}"
    );
    assert_eq!(
        rows[0].get("status").and_then(|v| v.as_str()),
        Some("rejected"),
        "a rejected disqualification link must not be restored by the next run"
    );
}
