//! The donor→contract join in the Donation Correlation detector (Open Item 30, L2).
//!
//! `awarded_to` is `TYPE RELATION FROM entity TO contract` (`crates/schema/src/db.rs`), so
//! `in` is the donor entity and `out` is the contract. The detector queried
//! `WHERE out = $entity_id`, which asks "is this donor a contract?" — always false. It
//! returned zero contracts for every donor, so the revolving-door correlation could never
//! fire, and it did so regardless of whether the projection was complete: this is not a
//! consequence of the truncated `project` run, and re-running `project` does not fix it.
//!
//! The endpoint is only half of it. `identity_links.source_node_id` holds a BARE entity id
//! (`GB-COH:OC307847` — measured live, the code comment claiming `entity:company-number`
//! was wrong), while `in` is a record link. Comparing a string against a record link never
//! matches either, so fixing `out` → `in` alone would leave the detector just as silent.
//! Both halves are needed, which is why this test asserts on an alert rather than on the
//! query text.
//!
//! Requires: URBAN_DB_TESTS=1 and a running SurrealDB on localhost:8000 (root/root).

use urban_integrity_enricher::donation_correlation::{
    run_donation_correlation_detector, PartyAuthorityMap,
};

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

const DONOR: &str = "GB-COH:99001122";
const CONTRACT: &str = "CF:ocds-test-donation-corr-1";

fn party_map() -> PartyAuthorityMap {
    PartyAuthorityMap::from_json_str(
        r#"{ "_meta": { "name_variants": {} }, "Test Party": ["E06000001"] }"#,
    )
    .expect("test party map must parse")
}

/// Seed the smallest graph the detector needs: a donor entity, a contract it won, the
/// `awarded_to` relation between them, and a confirmed EC donation link.
async fn seed(db: &urban_integrity_test_support::TestNs) {
    // Record id == entity_id, exactly as `Projector::upsert_node` writes it
    // (`UPSERT type::record($table, $id)`). Seeding a random record id would make the join
    // untestable: the detector resolves the donor by id, not by scanning the table.
    db.query("CREATE type::record('entity', $id) CONTENT { entity_id: $id, name: 'Donor Test Ltd', entity_status: 'active' }")
        .bind(("id", DONOR))
        .await
        .unwrap()
        .check()
        .unwrap();
    db.query("CREATE type::record('contract', $id) CONTENT { entity_id: $id, name: 'Test Contract', value_gbp: 250000.0 }")
        .bind(("id", CONTRACT))
        .await
        .unwrap()
        .check()
        .unwrap();
    // The relation as the projector writes it: FROM entity TO contract.
    db.query(
        "LET $a = type::record('entity', $donor);
         LET $b = type::record('contract', $contract);
         RELATE $a->awarded_to->$b
         CONTENT { meta: { created_at: d'2024-03-01T00:00:00Z',
                           effective_from: d'2024-03-01T00:00:00Z' },
                   data: { details: 'test award' } }",
    )
    .bind(("donor", DONOR))
    .bind(("contract", CONTRACT))
    .await
    .unwrap()
    .check()
    .unwrap();

    // Donation 61 days before the award — well inside the 730-day window.
    let evidence = serde_json::json!({
        "party_name": "Test Party",
        "accepted_date": "2023-12-31",
    })
    .to_string();
    db.query(
        "CREATE identity_links CONTENT {
             source_node_id: $donor, target_node_id: 'PA:test party',
             resolution_type: 'exact_company_number', confidence: 1.0,
             source: 'electoral_commission', status: 'auto_linked',
             resolved_at: time::now(), evidence_json: $evidence
         }",
    )
    .bind(("donor", DONOR))
    .bind(("evidence", evidence))
    .await
    .unwrap()
    .check()
    .unwrap();
}

/// A donor with a contract inside the window must produce one alert.
///
/// Fails against `WHERE out = $entity_id` (0 contracts found, 0 alerts) and against a
/// bare-string comparison on `in`.
#[tokio::test]
async fn a_donor_with_a_contract_in_window_fires_an_alert() {
    if !db_tests_enabled() {
        return;
    }
    let db =
        urban_integrity_test_support::TestNs::connect_with_prefix("test_doncorr", "main").await;
    seed(&db).await;

    let alerts = run_donation_correlation_detector(&db, &party_map(), "test-run")
        .await
        .expect("detector must not error");

    assert_eq!(
        alerts, 1,
        "a donor holding an awarded_to contract 61 days after its donation must fire exactly \
         one alert; 0 means the donor→contract join found no contracts"
    );

    let motifs: Vec<serde_json::Value> = db
        .query("SELECT payload.result_json AS r FROM graph_events WHERE kind = 'MotifDetected'")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert_eq!(motifs.len(), 1, "exactly one MotifDetected event expected");
    let r = motifs[0]["r"].as_str().unwrap_or_default();
    assert!(
        r.contains("donation_contract_correlation") && r.contains(DONOR),
        "the alert must name the correlation and the donor; got {r}"
    );
}

/// The join must be direction-correct, not merely non-empty: a contract that the donor did
/// NOT win must not correlate. Guards against "fixing" the endpoint by dropping the
/// predicate.
#[tokio::test]
async fn an_unrelated_contract_does_not_correlate() {
    if !db_tests_enabled() {
        return;
    }
    let db =
        urban_integrity_test_support::TestNs::connect_with_prefix("test_doncorr2", "main").await;
    seed(&db).await;

    // A second donor, linked to the same party, holding no contract at all.
    db.query("CREATE type::record('entity', 'GB-COH:99003344') CONTENT { entity_id: 'GB-COH:99003344', name: 'Quiet Ltd', entity_status: 'active' }")
        .await
        .unwrap()
        .check()
        .unwrap();
    let evidence = serde_json::json!({
        "party_name": "Test Party",
        "accepted_date": "2023-12-31",
    })
    .to_string();
    db.query(
        "CREATE identity_links CONTENT {
             source_node_id: 'GB-COH:99003344', target_node_id: 'PA:test party',
             resolution_type: 'exact_company_number', confidence: 1.0,
             source: 'electoral_commission', status: 'auto_linked',
             resolved_at: time::now(), evidence_json: $evidence
         }",
    )
    .bind(("evidence", evidence))
    .await
    .unwrap()
    .check()
    .unwrap();

    let alerts = run_donation_correlation_detector(&db, &party_map(), "test-run")
        .await
        .expect("detector must not error");

    assert_eq!(
        alerts, 1,
        "only the donor that actually holds the awarded_to relation may alert — a donor with \
         no contracts must not pick up another donor's"
    );
}

const CONTRACT_2: &str = "CF:ocds-test-donation-corr-2";

/// A second contract for the same donor, also inside the 730-day window.
async fn seed_second_contract(db: &urban_integrity_test_support::TestNs) {
    db.query("CREATE type::record('contract', $id) CONTENT { entity_id: $id, name: 'Test Contract 2', value_gbp: 400000.0 }")
        .bind(("id", CONTRACT_2))
        .await
        .unwrap()
        .check()
        .unwrap();
    db.query(
        "LET $a = type::record('entity', $donor);
         LET $b = type::record('contract', $contract);
         RELATE $a->awarded_to->$b
         CONTENT { meta: { created_at: d'2024-06-01T00:00:00Z',
                           effective_from: d'2024-06-01T00:00:00Z' },
                   data: { details: 'second test award' } }",
    )
    .bind(("donor", DONOR))
    .bind(("contract", CONTRACT_2))
    .await
    .unwrap()
    .check()
    .unwrap();
}

/// The dedup key is `(donor, party, lpa)` — it carries no contract — so a donor holding two
/// in-window contracts must still produce exactly ONE alert.
///
/// `AlertedHashes` is loaded once before the write loop. Without a within-run insert the second
/// contract sees a set that predates the first write, and both alerts land carrying the same
/// `motif_hash`. Dedup then only works across runs, inverting T-17-03-05.
#[tokio::test]
async fn a_donor_with_two_contracts_in_window_fires_one_alert() {
    if !db_tests_enabled() {
        return;
    }
    let db =
        urban_integrity_test_support::TestNs::connect_with_prefix("test_doncorr_dup", "main").await;
    seed(&db).await;
    seed_second_contract(&db).await;

    let alerts = run_donation_correlation_detector(&db, &party_map(), "test-run")
        .await
        .expect("detector must not error");

    assert_eq!(
        alerts, 1,
        "two contracts share one (donor, party, lpa) dedup key — the detector must write one \
         alert, not one per contract"
    );

    let motifs: Vec<serde_json::Value> = db
        .query("SELECT payload.result_json AS r FROM graph_events WHERE kind = 'MotifDetected'")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    assert_eq!(
        motifs.len(),
        1,
        "exactly one MotifDetected event expected, got {}",
        motifs.len()
    );
}
