//! London Gazette insolvency adapter tests (PROP-02).
//!
//! Unit tests use mock serde_json::Value responses — no live HTTP needed.
//! Integration tests (URBAN_DB_TESTS=1 gated) test cursor persistence only,
//! not live HTTP, because the Gazette adapter uses reqwest internally and
//! a full mock-HTTP layer would require additional dependencies.

use tokio::sync::mpsc;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_enricher::adapters::london_gazette::LondonGazetteAdapter;
use urban_integrity_enricher::adapters::EnricherAdapter;
use urban_integrity_schema::types::node::EntityStatus;

// ── Unit: is_enabled always returns true (public API, no credential) ─────────

#[test]
fn gazette_adapter_is_always_enabled() {
    // LondonGazetteAdapter needs no API key — public Gazette REST API.
    // is_enabled() must return true unconditionally.
    //
    // We create a mock Arc<Surreal<Any>> — but for this test we just test
    // the structural property. We need a real adapter for the is_enabled test.
    // Use a helper that creates a no-DB adapter for unit tests.
    let adapter = LondonGazetteAdapter::new_for_test();
    assert!(
        adapter.is_enabled(),
        "LondonGazetteAdapter::is_enabled() must return true — Gazette is a public API"
    );
}

// ── Unit: adapter name ────────────────────────────────────────────────────────

#[test]
fn gazette_adapter_name_is_london_gazette() {
    let adapter = LondonGazetteAdapter::new_for_test();
    assert_eq!(adapter.name(), "london_gazette");
}

// ── Unit: parse_gazette_date helper ──────────────────────────────────────────

#[test]
fn gazette_parse_date_iso8601() {
    use urban_integrity_enricher::adapters::london_gazette::parse_gazette_date;
    let result = parse_gazette_date("2024-03-15T00:00:00Z");
    assert!(
        result.is_ok(),
        "ISO 8601 date should parse, got: {result:?}"
    );
    let dt = result.unwrap();
    assert_eq!(dt.format("%Y-%m-%d").to_string(), "2024-03-15");
}

#[test]
fn gazette_parse_date_date_only() {
    use urban_integrity_enricher::adapters::london_gazette::parse_gazette_date;
    // The Gazette `published` field often has date-only format
    let result = parse_gazette_date("2024-03-15");
    assert!(
        result.is_ok(),
        "Date-only string should parse, got: {result:?}"
    );
}

// ── Unit: notice_codes only contains InLiquidation codes (D-06) ──────────────

#[test]
fn gazette_notice_codes_inliquidation_only() {
    use urban_integrity_enricher::adapters::london_gazette::NOTICE_CODES;
    // D-06 ratified: only 2452/2454/2410 (InLiquidation); NO dissolution codes (2460/2408)
    assert!(
        NOTICE_CODES.contains(&2452),
        "must include 2452 (Winding-Up Orders)"
    );
    assert!(
        NOTICE_CODES.contains(&2454),
        "must include 2454 (Appointment of Liquidators)"
    );
    assert!(
        NOTICE_CODES.contains(&2410),
        "must include 2410 (Appointment of Administrators)"
    );
    assert!(
        !NOTICE_CODES.contains(&2460),
        "must NOT include 2460 (dissolution — D-06 ratified scope)"
    );
    assert!(
        !NOTICE_CODES.contains(&2408),
        "must NOT include 2408 (dissolution — D-06 ratified scope)"
    );
    assert_eq!(NOTICE_CODES.len(), 3, "must contain exactly 3 notice codes");
}

// ── Unit: extract_company_number_from_jsonld ──────────────────────────────────

#[test]
fn gazette_extract_company_number_from_graph_array() {
    use urban_integrity_enricher::adapters::london_gazette::extract_company_number_from_jsonld;
    // A2: companyNumber lives in @graph array — find_map over objects
    let jsonld = serde_json::json!({
        "@graph": [
            { "@type": "gaz:Notice", "hasNoticeCode": 2452 },
            { "@type": "org:Organization", "companyNumber": "12345678" }
        ]
    });
    let result = extract_company_number_from_jsonld(&jsonld);
    assert_eq!(result, Some("12345678".to_string()));
}

#[test]
fn gazette_extract_company_number_missing_returns_none() {
    use urban_integrity_enricher::adapters::london_gazette::extract_company_number_from_jsonld;
    // D-05: no companyNumber → None (no fuzzy fallback)
    let jsonld = serde_json::json!({
        "@graph": [
            { "@type": "gaz:Notice", "hasNoticeCode": 2452 }
        ]
    });
    let result = extract_company_number_from_jsonld(&jsonld);
    assert!(
        result.is_none(),
        "Missing companyNumber in @graph must return None (D-05)"
    );
}

#[test]
fn gazette_extract_company_number_empty_graph_returns_none() {
    use urban_integrity_enricher::adapters::london_gazette::extract_company_number_from_jsonld;
    let jsonld = serde_json::json!({ "@graph": [] });
    let result = extract_company_number_from_jsonld(&jsonld);
    assert!(result.is_none());
}

#[test]
fn gazette_extract_company_number_no_graph_field_returns_none() {
    use urban_integrity_enricher::adapters::london_gazette::extract_company_number_from_jsonld;
    let jsonld = serde_json::json!({ "companyNumber": "12345678" }); // top-level — not valid (Pitfall 3)
    let result = extract_company_number_from_jsonld(&jsonld);
    assert!(
        result.is_none(),
        "companyNumber at JSON-LD top level must NOT be extracted — only from @graph (Pitfall 3)"
    );
}

// ── Unit: gazette_adapter_skips_notice_without_company_number (D-05) ─────────

#[tokio::test]
async fn gazette_adapter_skips_notice_without_company_number() {
    // When JSON-LD has no companyNumber in @graph, zero UpdateNode mutations sent.
    use urban_integrity_enricher::adapters::london_gazette::process_gazette_entry_for_test;
    let (tx, mut rx) = mpsc::channel::<GraphMutation>(10);
    // Mock JSON-LD without companyNumber
    let jsonld = serde_json::json!({
        "@graph": [{ "@type": "gaz:Notice", "hasNoticeCode": 2452 }]
    });
    process_gazette_entry_for_test(&tx, &jsonld, "2024-03-15", 2452, None).await;
    drop(tx);
    // No mutations should have been sent
    let msgs: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        msgs.is_empty(),
        "No mutations must be emitted when companyNumber is absent (D-05)"
    );
}

// ── Unit: gazette_adapter_emits_update_node ───────────────────────────────────

#[tokio::test]
async fn gazette_adapter_emits_update_node() {
    // When JSON-LD has companyNumber + we provide a mock weight, UpdateNode emitted.
    use urban_integrity_enricher::adapters::london_gazette::process_gazette_entry_for_test;
    use urban_integrity_schema::types::node::{EntityData, NodeWeight};
    let (tx, mut rx) = mpsc::channel::<GraphMutation>(10);

    let jsonld = serde_json::json!({
        "@graph": [
            { "@type": "org:Organization", "companyNumber": "12345678" }
        ]
    });

    // Provide a mock current weight for the entity
    let existing_weight = NodeWeight::Entity(EntityData {
        name: "Test Corp Ltd".to_string(),
        company_number: Some("12345678".to_string()),
        status: EntityStatus::Active,
        incorporation_date: None,
    });

    process_gazette_entry_for_test(
        &tx,
        &jsonld,
        "2024-03-15T00:00:00Z",
        2452,
        Some(existing_weight),
    )
    .await;
    drop(tx);

    let msgs: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert_eq!(
        msgs.len(),
        1,
        "Exactly one UpdateNode mutation should be emitted"
    );

    match &msgs[0] {
        GraphMutation::UpdateNode { id, weight } => {
            assert_eq!(id.0, "GB-COH:12345678", "Entity ID must be GB-COH:12345678");
            match weight {
                NodeWeight::Entity(data) => {
                    assert!(
                        matches!(data.status, EntityStatus::InLiquidation(_)),
                        "Status must be InLiquidation after Gazette winding-up notice, got: {:?}",
                        data.status
                    );
                }
                other => panic!("Expected NodeWeight::Entity, got: {other:?}"),
            }
        }
        other => panic!("Expected GraphMutation::UpdateNode, got: {other:?}"),
    }
}

// ── Integration: cursor advances after successful run ─────────────────────────

#[cfg(test)]
#[tokio::test]
async fn gazette_adapter_cursor_advances() {
    // Integration test — only runs with URBAN_DB_TESTS=1
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return;
    }
    use urban_integrity_core::stream::{CursorStore, SurrealStreamCursor};

    let db = connect_test_db().await;

    // Create a cursor and persist a timepoint
    let cursor = SurrealStreamCursor::new(db.arc(), "enricher_cursors", "london_gazette_test");
    let before = cursor.load_timepoint().await;
    let now_ts = chrono::Utc::now().timestamp();
    cursor.persist_timepoint(now_ts).await;
    let after = cursor.load_timepoint().await;

    assert!(
        after.is_some(),
        "Cursor should have a timepoint after persist"
    );
    assert!(
        after.unwrap() >= now_ts,
        "Cursor timepoint must be >= persisted value"
    );
    assert_ne!(before, after, "Timepoint should change after persist");

    // Cleanup
    cursor.clear_timepoint().await;
}

// ── Helper: connect_test_db (URBAN_DB_TESTS=1 guard) ─────────────────────────

#[cfg(test)]
async fn connect_test_db() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect("integrity").await
}
