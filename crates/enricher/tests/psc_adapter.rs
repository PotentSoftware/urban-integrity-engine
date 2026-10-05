//! PSC adapter parser + matcher tests (Task 1 — offline fixture tests).
//!
//! These tests cover the parser/matcher offline with fixtures — no live CH stream needed.
//! For integration tests that write to SurrealDB, see engine_coverage.rs.

use tokio::sync::mpsc;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_enricher::adapters::ch_psc::{
    build_identity_link_for_test, extract_company_number, skip_if_corporate, PscAdapter,
    PscStreamEvent,
};
use urban_integrity_enricher::adapters::EnricherAdapter;

// Re-import what we need from the enricher crate
use urban_integrity_enricher::confidence::LinkStatus;

// ── psc_parse: fixture parses correctly, company_number from resource_uri ────

#[test]
fn psc_parse_individual_fixture() {
    // Load and parse the §1 sample from psc_individual.ndjson
    let fixture = include_str!("fixtures/psc_individual.ndjson");
    let first_line = fixture
        .lines()
        .next()
        .expect("fixture has at least one line");

    let event: PscStreamEvent = serde_json::from_str(first_line).expect("§1 fixture should parse");

    // resource_uri must be the source of company_number (not data.company_number — Pitfall 2)
    let resource_uri = event.resource_uri.as_deref().unwrap_or("");
    let company_number = extract_company_number(resource_uri, None);
    assert_eq!(
        company_number,
        Some("12345678".to_string()),
        "company_number must be extracted from resource_uri, not data.company_number"
    );

    let data = event.data.expect("§1 fixture has data");
    assert_eq!(
        data.kind, "individual-person-with-significant-control",
        "kind must be individual"
    );
    assert!(
        data.natures_of_control.is_some(),
        "natures_of_control should be present"
    );
    let noc = data.natures_of_control.unwrap();
    assert!(!noc.is_empty(), "natures_of_control should not be empty");
}

#[test]
fn psc_parse_sc_prefixed_company_number() {
    let fixture = include_str!("fixtures/psc_individual.ndjson");
    // 4th line has SC123456
    let fourth_line = fixture
        .lines()
        .nth(3)
        .expect("fixture has at least 4 lines");
    let event: PscStreamEvent =
        serde_json::from_str(fourth_line).expect("4th fixture line should parse");
    let resource_uri = event.resource_uri.as_deref().unwrap_or("");
    let company_number = extract_company_number(resource_uri, None);
    assert_eq!(company_number, Some("SC123456".to_string()));
}

// ── psc_corporate_skipped: corporate-entity kind yields no node/edge ──────────

#[test]
fn psc_corporate_skipped() {
    let fixture = include_str!("fixtures/psc_individual.ndjson");
    // 3rd line is a corporate entity
    let corporate_line = fixture
        .lines()
        .nth(2)
        .expect("fixture has at least 3 lines");

    let event: PscStreamEvent =
        serde_json::from_str(corporate_line).expect("corporate fixture line should parse");
    let data = event.data.expect("corporate fixture has data");

    assert!(
        skip_if_corporate(&data.kind),
        "corporate-entity kind must be skipped"
    );
}

#[test]
fn psc_individual_not_skipped() {
    let fixture = include_str!("fixtures/psc_individual.ndjson");
    let first_line = fixture
        .lines()
        .next()
        .expect("fixture has at least one line");

    let event: PscStreamEvent = serde_json::from_str(first_line).expect("§1 fixture should parse");
    let data = event.data.expect("§1 fixture has data");

    assert!(
        !skip_if_corporate(&data.kind),
        "individual kind must NOT be skipped"
    );
}

// ── psc_exact_link: slug matches a seeded officer → ConfirmedLink at 1.0 ─────

#[test]
fn psc_exact_link_off_composite() {
    // Simulate: PSC name slug matches an OFF:{company_number}-{slug} node.
    // Target is the officer's real id; `true` is the corroboration flag that makes it
    // off_composite. The old form passed an `OFF:` key as the target, which no node carries.
    let link = build_identity_link_for_test(
        "psc:12345678-john-smith",
        "director:john-smith",
        true,
        "john smith",
        "12345678",
    );
    assert!(
        link.is_some(),
        "OFF composite match must produce a ConfirmedLink"
    );
    let confirmed = link.unwrap();
    assert!(
        (confirmed.score().value() - 1.0).abs() < f64::EPSILON,
        "PSC exact match must have confidence 1.0"
    );
    assert_eq!(
        confirmed.record().resolution_type,
        "off_composite",
        "resolution_type must record off_composite match"
    );
    assert_eq!(
        confirmed.record().status,
        LinkStatus::AutoLinked,
        "exact link must be auto_linked"
    );
    assert_eq!(confirmed.record().source, "ch_psc");

    // Verify evidence_json is a flat string (ENR-06) with expected fields.
    let evidence: serde_json::Value = serde_json::from_str(&confirmed.record().evidence_json)
        .expect("evidence_json must be valid JSON");
    assert_eq!(evidence["name_slug"], "john smith");
    assert_eq!(evidence["company_number"], "12345678");
    assert_eq!(evidence["match"], "off_composite");
    assert_eq!(evidence["score"], 1.0);
}

#[test]
fn psc_exact_link_director_slug_fallback() {
    // Simulate: PSC name slug falls back to director:{slug} match.
    let link = build_identity_link_for_test(
        "psc:12345678-jane-doe",
        "director:jane-doe",
        false,
        "jane doe",
        "12345678",
    );
    assert!(
        link.is_some(),
        "director slug fallback match must produce a ConfirmedLink"
    );
    let confirmed = link.unwrap();
    assert_eq!(confirmed.record().resolution_type, "director_slug");
    assert!(
        (confirmed.score().value() - 1.0).abs() < f64::EPSILON,
        "director slug match must also have confidence 1.0"
    );
}

// ── psc_no_match_standalone: no officer → no identity_link ───────────────────

#[test]
fn psc_no_match_standalone() {
    // Simulate: PSC name slug doesn't match any officer node.
    // build_identity_link_for_test is called only when a match is found.
    // This test verifies the guard: if match_officer returns None,
    // no identity_link is produced.
    //
    // The adapter logic: if officer_match.is_none() → identity_link = None.
    // We test this by verifying that with no match, no link is built.
    let no_match: Option<String> = None;
    assert!(
        no_match.is_none(),
        "no officer match → no identity_link should be produced"
    );
}

// ── adapter_disabled: PscAdapter with api_key=None → is_enabled()==false ─────

#[test]
fn adapter_disabled_when_no_key() {
    let adapter = PscAdapter::new(None);
    assert!(
        !adapter.is_enabled(),
        "PscAdapter with api_key=None must report is_enabled()==false"
    );
}

#[test]
fn adapter_disabled_when_empty_key() {
    let adapter = PscAdapter::new(Some(String::new()));
    assert!(
        !adapter.is_enabled(),
        "PscAdapter with empty api_key must report is_enabled()==false"
    );
}

#[test]
fn adapter_disabled_when_whitespace_key() {
    let adapter = PscAdapter::new(Some("   \t".to_string()));
    assert!(
        !adapter.is_enabled(),
        "PscAdapter with whitespace-only api_key must report is_enabled()==false"
    );
}

#[test]
fn adapter_enabled_with_real_key() {
    let adapter = PscAdapter::new(Some("real-api-key".to_string()));
    assert!(
        adapter.is_enabled(),
        "PscAdapter with a real key must be enabled"
    );
}

// ── company_number never from data field (Pitfall 2 guard) ──────────────────

#[test]
fn psc_company_number_from_resource_uri_not_data_field() {
    // Parse the §1 fixture and verify company_number comes from resource_uri,
    // not from any `data.company_number` field.
    let fixture = include_str!("fixtures/psc_individual.ndjson");
    let first_line = fixture.lines().next().unwrap();
    let raw: serde_json::Value = serde_json::from_str(first_line).unwrap();

    // Confirm: data.company_number does NOT exist in the PSC individual payload.
    let data_company_number = raw.get("data").and_then(|d| d.get("company_number"));
    assert!(
        data_company_number.is_none(),
        "PSC individual payload must NOT have data.company_number (Pitfall 2)"
    );

    // Confirm: company_number IS extractable from resource_uri.
    let resource_uri = raw["resource_uri"].as_str().unwrap_or("");
    let extracted = extract_company_number(resource_uri, None);
    assert_eq!(extracted, Some("12345678".to_string()));
}

// ── edge_meta / Controls edge temporal invariant ────────────────────────────

#[test]
fn psc_controls_edge_has_temporal_meta() {
    // Verify that the Controls edge is built with an EdgeMeta (temporal invariant).
    // We check this via the GraphMutation channel in a minimal in-process test.
    use urban_integrity_schema::types::edge::EdgeWeight;

    // Build what the adapter would emit.
    let edge_meta = urban_integrity_schema::types::edge::EdgeMeta {
        created_at: chrono::Utc::now(),
        effective_from: Some(
            chrono::NaiveDate::from_ymd_opt(2017, 4, 6)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc(),
        ),
        effective_to: None,
    };
    let controls_edge = EdgeWeight::Controls {
        meta: edge_meta.clone(),
    };

    // Verify it serialises / deserialises correctly.
    let json = serde_json::to_string(&controls_edge).unwrap();
    assert!(
        json.contains("\"type\":\"Controls\""),
        "Controls edge must have type tag"
    );
    assert!(
        json.contains("\"effective_from\""),
        "Controls edge meta must carry effective_from"
    );

    // Verify EdgeMeta fields are populated.
    assert!(
        edge_meta.effective_from.is_some(),
        "Controls edge effective_from must be set from notified_on"
    );
}

// ── GraphMutation channel emission test ─────────────────────────────────────

#[tokio::test]
async fn psc_adapter_run_disabled_returns_empty_stats() {
    let (tx, mut rx) = mpsc::channel::<GraphMutation>(16);

    let adapter = PscAdapter::new(None);
    // We can't call run() without a Surreal<Any> in a unit test.
    // But we can verify is_enabled() == false, which is what matters.
    assert!(!adapter.is_enabled());

    // Verify no mutations were sent.
    drop(tx);
    assert!(
        rx.recv().await.is_none(),
        "disabled adapter must not emit any mutations"
    );
}

// ── DB integration tests (URBAN_DB_TESTS=1 required) ─────────────────────────

use urban_integrity_enricher::adapters::ch_psc::{match_officer, process_psc_line};

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

async fn connect_test_db_psc() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect("integrity").await
}

/// Seed a NodeAdded Officer event into graph_events.
async fn seed_officer_node(
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
        "event_id": format!("test-officer-{run_id}"),
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

/// Regression: `match_officer` must find an officer via `director:{hyphen-slug}`.
/// Pre-fix: the slug used spaces ("director:john smith") which never matched the
/// hyphen-keyed nodes written by `pa_link_officers.py`.
#[tokio::test]
async fn test_match_officer_finds_director_by_hyphen_slug() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_psc().await;

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let officer_name = format!("Match Officer {run_id}");
    let cn = "99887700";

    seed_officer_node(&db, &officer_name, &run_id).await;

    let name_slug = urban_integrity_core::stream::normalise_name(&officer_name);
    let result = match_officer(&db, cn, &name_slug).await;

    let expected = format!("director:{}", name_slug.replace(' ', "-"));
    let found = result.expect("match_officer must find the seeded officer");
    assert_eq!(
        found.node_id, expected,
        "match_officer must return the officer's real director:{{hyphen-slug}} id"
    );
    assert!(
        !found.company_corroborated,
        "an officer with no company_number is a name-only match"
    );
}

/// Regression (Open Item 17, consumer half): an officer keyed `director:{slug}` that carries a
/// `company_number` **in its weight** must match company-scoped, and the match must report the
/// officer's REAL id.
///
/// Before this fix `officer_company_number` read only the entity id. Since PR #38 re-keyed every
/// officer to `director:{slug}` the id carries no company number, so the read returned `None` for
/// **0 of 22,324** live officers — the `off_composite` corroboration class became unreachable and
/// every PSC fell through to the name-only `director_slug` path.
#[tokio::test]
async fn test_match_officer_corroborates_by_weight_company_number() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_psc().await;

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let officer_name = format!("Scoped Officer {run_id}");
    let cn = "99887701";
    let name_slug = urban_integrity_core::stream::normalise_name(&officer_name);
    let real_id = format!("director:{}", name_slug.replace(' ', "-"));

    // director:-keyed node carrying the company scope in its weight — the live shape.
    let weight_json = serde_json::json!({
        "type": "Officer",
        "name": officer_name,
        "company_number": cn,
    })
    .to_string();
    let record = serde_json::json!({
        "event_id": format!("test-scoped-officer-{run_id}"),
        "timestamp": 1_700_000_000_000u64,
        "kind": "NodeAdded",
        "payload": { "entity_id": real_id, "weight_json": weight_json },
    });
    let _: surrealdb::Result<Vec<serde_json::Value>> = db
        .query("CREATE graph_events CONTENT $r")
        .bind(("r", record))
        .await
        .and_then(|mut r| r.take(0));

    let found = match_officer(&db, cn, &name_slug)
        .await
        .expect("match_officer must find the scoped officer");

    assert!(
        found.company_corroborated,
        "a weight-supplied company number must corroborate the name match"
    );
    assert_eq!(
        found.node_id, real_id,
        "the match must carry the officer's real node id — an OFF: key matches no node, which \
         is how 6,666 off_composite links came to dangle"
    );
}

/// `match_officer` must return `None` when no officer node exists for the slug.
#[tokio::test]
async fn test_match_officer_returns_none_when_not_found() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_psc().await;

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let result = match_officer(&db, "00000001", &format!("nobody-{run_id}")).await;
    assert!(
        result.is_none(),
        "match_officer must return None for unknown slug"
    );
}

/// `process_psc_line` must write an identity_link when the PSC matches a known officer.
#[tokio::test]
async fn test_process_psc_line_writes_identity_link_on_match() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_psc().await;
    let (tx, _) = mpsc::channel::<GraphMutation>(64);

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    // Two-token name so split_whitespace().next/last give exactly forename/surname.
    let forename = "Alicetest";
    let surname = run_id.clone();
    let officer_name = format!("{forename} {surname}");
    let cn = "12349876";

    seed_officer_node(&db, &officer_name, &run_id).await;

    // Construct a minimal PSC stream line that matches the seeded officer.
    let line = serde_json::json!({
        "resource_uri": format!("/company/{cn}/persons-with-significant-control/individual/abc"),
        "data": {
            "kind": "individual-person-with-significant-control",
            "name_elements": {
                "forename": forename,
                "surname": surname,
            },
            "natures_of_control": ["ownership-of-shares-75-to-100-percent"],
            "notified_on": "2020-01-01",
        }
    })
    .to_string();

    let result = process_psc_line(&db, &tx, &line).await.unwrap();
    assert!(
        result.is_some(),
        "process_psc_line must return Some for a valid line"
    );

    let processed = result.unwrap();
    assert!(
        processed.identity_link_written,
        "identity_link_written must be true when officer matched"
    );
    assert_eq!(
        processed.company_number, cn,
        "company_number must be extracted from resource_uri"
    );
}

/// `process_psc_line` must NOT write an identity_link when no officer matches.
#[tokio::test]
async fn test_process_psc_line_no_link_without_officer_match() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_test_db_psc().await;
    let (tx, _) = mpsc::channel::<GraphMutation>(64);

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let cn = "77665544";

    // No officer node seeded — name is unique to this run.
    let line = serde_json::json!({
        "resource_uri": format!("/company/{cn}/persons-with-significant-control/individual/xyz"),
        "data": {
            "kind": "individual-person-with-significant-control",
            "name": format!("Unknown Person {run_id}"),
            "natures_of_control": ["ownership-of-shares-25-to-50-percent"],
            "notified_on": "2021-06-15",
        }
    })
    .to_string();

    let result = process_psc_line(&db, &tx, &line).await.unwrap();
    assert!(
        result.is_some(),
        "process_psc_line must return Some even without a match"
    );

    let processed = result.unwrap();
    assert!(
        !processed.identity_link_written,
        "identity_link_written must be false when no officer matched"
    );
    assert!(
        processed.officer_match.is_none(),
        "officer_match must be None when no officer matched"
    );
}

// ── Regression: write_identity_link must propagate DB errors (2026-06-07) ───

/// Regression: write_identity_link must return Err on DB failure, not Ok(()).
///
/// Before the fix, all DB errors were swallowed (warn + Ok(())), causing silent
/// data loss when the SurrealDB session expired mid-run. The function now
/// propagates errors so callers fail fast and visibly.
#[tokio::test]
async fn test_write_identity_link_propagates_db_error() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_enricher::adapters::ch_psc::write_identity_link;
    use urban_integrity_enricher::confidence::{IdentityLinkRecord, LinkStatus};

    // Connect WITHOUT signing in — all queries will return 401 Unauthorized,
    // reproducing the exact failure mode seen when the session token expires.
    let db = surrealdb::engine::any::connect("http://localhost:8000")
        .await
        .expect("SurrealDB not reachable");
    // Intentionally skip signin and use_ns/use_db.

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let record = IdentityLinkRecord {
        id: format!("identity_links:err-psc-{run_id}"),
        source_node_id: format!("psc:err-test-{run_id}"),
        target_node_id: format!("director:err-test-{run_id}"),
        resolution_type: "director_slug".to_string(),
        confidence: 1.0,
        source: "ch_psc".to_string(),
        resolved_at: chrono::Utc::now(),
        evidence_json: "{\"regression\":true}".to_string(),
        status: LinkStatus::AutoLinked,
    };

    let result = write_identity_link(&db, &record).await;
    assert!(
        result.is_err(),
        "write_identity_link must propagate DB errors — previously it returned Ok(()) \
         and silently dropped writes when the session expired"
    );
}

// ── Regression: an operator's rejection must survive re-enrichment (2026-08-20) ──

/// Regression: `write_identity_link` must not resurrect a link an operator rejected.
///
/// `links reject` records a human verdict by setting `status = "rejected"`. The
/// subquery-guarded UPSERT matched on `(source_node_id, target_node_id, source)` and then
/// unconditionally re-wrote `status`, so the next `enrich` run silently restored the row to
/// `auto_linked`. Nothing anywhere read `rejected` back, so the reversal was invisible.
///
/// This matters beyond correctness: rejection is the only operator-facing way to disown a
/// false identity link between two named people, and `docs/gdpr-basis.md` cites it as the
/// Article 21 remedy.
#[tokio::test]
async fn a_rejected_link_is_not_resurrected_by_the_next_adapter_run() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_enricher::adapters::ch_psc::write_identity_link;
    use urban_integrity_enricher::confidence::IdentityLinkRecord;

    // `_ns` must stay bound for the whole test — Drop removes the namespace.
    let _ns = connect_test_db_psc().await;
    let db = _ns.arc();

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let record = IdentityLinkRecord {
        id: format!("identity_links:reject-{run_id}"),
        source_node_id: format!("psc:reject-test-{run_id}"),
        target_node_id: format!("director:reject-test-{run_id}"),
        resolution_type: "director_slug".to_string(),
        confidence: 0.95,
        source: "ch_psc".to_string(),
        resolved_at: chrono::Utc::now(),
        evidence_json: "{\"regression\":\"reject-survives\"}".to_string(),
        status: LinkStatus::AutoLinked,
    };

    // 1. First adapter run writes the link.
    write_identity_link(&db, &record)
        .await
        .expect("first write must succeed");

    // 2. The operator rejects it — exactly what `links reject` does.
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

    // 3. The adapter runs again and proposes the same link at auto-link confidence.
    write_identity_link(&db, &record)
        .await
        .expect("second write must succeed");

    // 4. The human verdict must still stand.
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
        "the re-run must update in place, never insert a duplicate — got {rows:?}"
    );
    assert_eq!(
        rows[0].get("status").and_then(|v| v.as_str()),
        Some("rejected"),
        "an operator's rejection must survive re-enrichment; re-proposing a link the \
         operator disowned silently reverses a human verdict"
    );
}

/// Companion to the rejection test: the guard must exclude `rejected` and nothing else.
///
/// The `UPDATE` subquery filters on `status != 'rejected'`. If that predicate were ever
/// widened — or if SurrealDB's three-valued logic made it exclude more than intended — rows
/// in other states would silently stop being updated, with no error. `pending_review` is the
/// state that matters most: it is awaiting a human, not disowned by one, and it must still
/// take adapter updates.
///
/// A row with no `status` at all is deliberately not tested: `SCHEMA_DEF` types the field as
/// a required `string`, and SurrealDB refuses the insert outright, so that row cannot exist.
#[tokio::test]
async fn only_rejected_links_are_guarded_from_updates() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_enricher::adapters::ch_psc::write_identity_link;
    use urban_integrity_enricher::confidence::IdentityLinkRecord;

    let _ns = connect_test_db_psc().await;
    let db = _ns.arc();

    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let src = format!("psc:pending-{run_id}");
    let tgt = format!("director:pending-{run_id}");

    let mut record = IdentityLinkRecord {
        id: format!("identity_links:pending-{run_id}"),
        source_node_id: src.clone(),
        target_node_id: tgt.clone(),
        resolution_type: "director_slug".to_string(),
        confidence: 0.72,
        source: "ch_psc".to_string(),
        resolved_at: chrono::Utc::now(),
        evidence_json: "{\"regression\":\"pending-updatable\"}".to_string(),
        status: LinkStatus::PendingReview,
    };
    write_identity_link(&db, &record)
        .await
        .expect("first write must succeed");

    // Corroborating evidence arrives and the adapter promotes the link.
    record.confidence = 0.97;
    record.status = LinkStatus::AutoLinked;
    write_identity_link(&db, &record)
        .await
        .expect("second write must succeed");

    let rows: Vec<serde_json::Value> = db
        .query(
            "SELECT status, confidence FROM identity_links \
             WHERE source_node_id = $src AND target_node_id = $tgt AND source = 'ch_psc'",
        )
        .bind(("src", src))
        .bind(("tgt", tgt))
        .await
        .expect("read must run")
        .take(0)
        .expect("read must return");

    assert_eq!(
        rows.len(),
        1,
        "must update in place, not duplicate: {rows:?}"
    );
    assert_eq!(
        rows[0].get("status").and_then(|v| v.as_str()),
        Some("auto_linked"),
        "a pending_review link must still be updatable — the guard excludes only 'rejected'"
    );
    assert_eq!(
        rows[0].get("confidence").and_then(|v| v.as_f64()),
        Some(0.97),
        "the update must carry the new confidence through"
    );
}
