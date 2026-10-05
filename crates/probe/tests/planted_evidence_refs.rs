//! The shared DB-gated golden asserting ref-counts-per-finding on the planted fixture (EVM-05).
//!
//! Plan 49-02 established the harness and proved the Phase 49 fixture structures
//! (`identity_links`, `enrichment_runs`, `MotifDetected`, the `Controls` edge, and the
//! `NodeUpdated` status flip) seed correctly through the single `seed_planted` call.
//!
//! Plan 49-11 (this plan) adds the seven per-probe golden tests below: each runs one of D-02's
//! priority probes against the seeded fixture and asserts its total `RecordRef` count equals the
//! literal in `Manifest.evidence_refs` — never a number re-typed in the test body, so the single
//! source of truth stays `crates/test-support/src/planted.rs` (D-01).

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_probe::disqualified_director_edges::DisqualifiedDirectorEdges;
use urban_integrity_probe::electoral_commission_donations::ElectoralCommissionDonations;
use urban_integrity_probe::identity_link_registry::IdentityLinkRegistry;
use urban_integrity_probe::osint_new_node_types::OsintNewNodeTypes;
use urban_integrity_probe::precomputed_alerts::PrecomputedAlerts;
use urban_integrity_probe::property_nodes::PropertyNodes;
use urban_integrity_probe::psc_beneficial_ownership::PscBeneficialOwnership;
use urban_integrity_probe::shared_exact_address::SharedExactAddress;
use urban_integrity_probe::{Probe, Strategy};
use urban_integrity_test_support::planted::{planted_graph, PlantedGraph};

/// Connects to a fresh, prefixed `TestNs` namespace and seeds the whole planted fixture.
///
/// The `TestNs` guard must stay bound for the whole test (the Test Namespace Hygiene
/// rule in docs/TESTING.md) — it is dropped, and the namespace removed, only when the caller's binding goes out of
/// scope.
async fn seeded_fixture() -> (
    urban_integrity_test_support::TestNs,
    Surreal<Any>,
    PlantedGraph,
) {
    let ns = urban_integrity_test_support::TestNs::connect_with_prefix(
        "test_probe_planted_evidence_refs",
        "main",
    )
    .await;
    let db = (*ns.arc()).clone();
    let g = planted_graph();
    urban_integrity_test_support::events::seed_planted(&db, &g)
        .await
        .expect("seed planted graph");
    (ns, db, g)
}

async fn count(db: &Surreal<Any>, query: &str) -> i64 {
    let n: Option<i64> = db
        .query(query)
        .await
        .expect("count query")
        .take((0, "count"))
        .expect("count take");
    n.unwrap_or(0)
}

#[tokio::test]
async fn planted_fixture_seeds_new_evidence_structures_without_error() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_evidence_refs::planted_fixture_seeds_new_evidence_structures_without_error \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;

    let identity_links = count(&db, "SELECT count() FROM identity_links GROUP ALL").await;
    assert_eq!(
        identity_links,
        g.identity_links.len() as i64,
        "identity_links row count must match the fixture's own identity_links.len(), not a \
         hardcoded literal that could drift silently from Task 1's data"
    );

    let enrichment_runs = count(&db, "SELECT count() FROM enrichment_runs GROUP ALL").await;
    assert_eq!(
        enrichment_runs,
        g.enrichment_run_sources.len() as i64,
        "enrichment_runs marker count must match the fixture's own enrichment_run_sources.len()"
    );

    let motif_events = count(
        &db,
        "SELECT count() FROM graph_events WHERE kind = 'MotifDetected' GROUP ALL",
    )
    .await;
    assert_eq!(
        motif_events,
        g.motif_events.len() as i64,
        "MotifDetected graph_events row count must match the fixture's own motif_events.len()"
    );

    let controls_edges = count(
        &db,
        "SELECT count() FROM graph_events WHERE kind = 'EdgeAdded' AND \
         payload.weight_json CONTAINS '\"type\":\"Controls\"' GROUP ALL",
    )
    .await;
    assert_eq!(
        controls_edges, 1,
        "the planted Controls(PSC1, A) edge must land exactly once"
    );

    let node_updated = count(
        &db,
        "SELECT count() FROM graph_events WHERE kind = 'NodeUpdated' GROUP ALL",
    )
    .await;
    assert_eq!(
        node_updated, 1,
        "B's InLiquidation NodeUpdated status flip must land exactly once"
    );
}

/// Total `RecordRef` count across every evidence row of one `Finding` — the shared measurement
/// every test below asserts against `Manifest.evidence_refs`' literal for that probe.
fn total_refs(finding: &urban_integrity_schema::types::verdict::Finding) -> usize {
    finding.evidence.iter().map(|e| e.refs.len()).sum()
}

/// The literal expectation for `probe_name`, read FROM the manifest — never hardcoded a second
/// time here, per D-01's single-source-of-truth rule.
fn expected_ref_count(g: &PlantedGraph, probe_name: &str) -> usize {
    g.manifest
        .evidence_refs
        .iter()
        .find(|e| e.probe_name == probe_name)
        .unwrap_or_else(|| panic!("no evidence_refs literal for probe `{probe_name}`"))
        .ref_count
}

#[tokio::test]
async fn planted_psc_beneficial_ownership_ref_count_matches_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_psc_beneficial_ownership_ref_count_matches_manifest (set \
             URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = PscBeneficialOwnership.run(&db, &mut st).await;
    assert_eq!(
        total_refs(&finding),
        expected_ref_count(&g, "psc-beneficial-ownership"),
        "psc-beneficial-ownership's total RecordRef count against the planted fixture must equal \
         Manifest.evidence_refs' literal; finding = {finding:?}"
    );
}

#[tokio::test]
async fn planted_osint_risk_edges_ref_count_matches_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_osint_risk_edges_ref_count_matches_manifest (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = DisqualifiedDirectorEdges.run(&db, &mut st).await;
    assert_eq!(
        total_refs(&finding),
        expected_ref_count(&g, "osint-risk-edges"),
        "osint-risk-edges's total RecordRef count against the planted fixture must equal \
         Manifest.evidence_refs' literal; finding = {finding:?}"
    );
}

/// Task 2 (phase 50 plan 2): proves the SHAPE of the widened `osint-risk-edges` refs -- one
/// evidence row per officer, `refs[0]` the officer, `refs[1..]` non-empty targets -- not just a
/// total count. `ExpectedProbeRefs`'s bare `ref_count` above cannot distinguish "2 rows of 2 refs
/// each, officer-first" from any other arrangement that happens to sum to 4; this test closes
/// that gap.
#[tokio::test]
async fn planted_osint_risk_edges_pairs_each_officer_with_its_own_targets() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_osint_risk_edges_pairs_each_officer_with_its_own_targets (set \
             URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, _g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = DisqualifiedDirectorEdges.run(&db, &mut st).await;

    let disq_row = finding
        .evidence
        .iter()
        .find(|r| r.text.starts_with("DisqualifiedFrom officer "))
        .expect("a DisqualifiedFrom officer evidence row must be present");
    assert_eq!(
        disq_row.refs[0].id,
        urban_integrity_test_support::planted::OD,
        "the DisqualifiedFrom officer row's refs[0] must be the planted OD officer"
    );
    assert!(
        !disq_row.refs[1..].is_empty(),
        "the DisqualifiedFrom officer row must carry at least one target ref, got {:?}",
        disq_row.refs
    );

    let controls_row = finding
        .evidence
        .iter()
        .find(|r| r.text.starts_with("Controls officer "))
        .expect("a Controls officer evidence row must be present");
    assert_eq!(
        controls_row.refs[0].id,
        urban_integrity_test_support::planted::PSC1,
        "the Controls officer row's refs[0] must be the planted PSC1 officer"
    );
    assert!(
        !controls_row.refs[1..].is_empty(),
        "the Controls officer row must carry at least one target ref, got {:?}",
        controls_row.refs
    );
}

#[tokio::test]
async fn planted_identity_link_registry_ref_count_matches_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_identity_link_registry_ref_count_matches_manifest (set \
             URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = IdentityLinkRegistry.run(&db, &mut st).await;
    assert_eq!(
        total_refs(&finding),
        expected_ref_count(&g, "identity-link-registry"),
        "identity-link-registry's total RecordRef count against the planted fixture must equal \
         Manifest.evidence_refs' literal; finding = {finding:?}"
    );
}

#[tokio::test]
async fn planted_precomputed_alerts_ref_count_matches_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_precomputed_alerts_ref_count_matches_manifest (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = PrecomputedAlerts.run(&db, &mut st).await;
    assert_eq!(
        total_refs(&finding),
        expected_ref_count(&g, "precomputed-alerts"),
        "precomputed-alerts's total RecordRef count against the planted fixture must equal \
         Manifest.evidence_refs' literal; finding = {finding:?}"
    );
}

#[tokio::test]
async fn planted_property_nodes_ref_count_matches_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: planted_property_nodes_ref_count_matches_manifest (set URBAN_DB_TESTS=1)");
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = PropertyNodes.run(&db, &mut st).await;
    assert_eq!(
        total_refs(&finding),
        expected_ref_count(&g, "property-nodes"),
        "property-nodes's total RecordRef count against the planted fixture must equal \
         Manifest.evidence_refs' literal; finding = {finding:?}"
    );
}

#[tokio::test]
async fn planted_phase20_ec_donations_ref_count_matches_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_phase20_ec_donations_ref_count_matches_manifest (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = ElectoralCommissionDonations.run(&db, &mut st).await;
    assert_eq!(
        total_refs(&finding),
        expected_ref_count(&g, "phase20-ec-donations"),
        "phase20-ec-donations's total RecordRef count against the planted fixture must equal \
         Manifest.evidence_refs' literal; finding = {finding:?}"
    );
}

/// Phase 50 plan 8: `address-cluster-exact`'s Manifest literal (9) is the L2 star-cluster
/// evidence row's own ref count, NOT `total_refs(&finding)` — this probe's evidence also
/// carries an aggregate row (0 refs) and an incidental second cluster at `L1` (3 refs, structure
/// 1's phoenix-loop predecessor/successor sharing a registered address), which would make a
/// naive `total_refs` sum 12. This test locates the star row by `refs[0].id == L2` and asserts
/// on it specifically — belt-and-braces with `crates/workbench/tests/finding_star_golden.rs`'s
/// full-pipeline proof of the same fixed answer.
#[tokio::test]
async fn planted_address_cluster_exact_star_row_matches_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_address_cluster_exact_star_row_matches_manifest (set \
             URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = SharedExactAddress.run(&db, &mut st).await;

    let star_row = finding
        .evidence
        .iter()
        .find(|row| {
            row.refs.first().map(|r| r.id.as_str())
                == Some(urban_integrity_test_support::planted::L2)
        })
        .unwrap_or_else(|| {
            panic!("no evidence row with refs[0].id == L2 found; finding = {finding:?}")
        });

    assert_eq!(
        star_row.refs.len(),
        expected_ref_count(&g, "address-cluster-exact"),
        "address-cluster-exact's L2 star-cluster row ref count must equal Manifest.evidence_refs' \
         literal; star_row = {star_row:?}"
    );
}

#[tokio::test]
async fn planted_osint_new_node_types_ref_count_matches_manifest() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: planted_osint_new_node_types_ref_count_matches_manifest (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    let (_ns, db, g) = seeded_fixture().await;
    let mut st = Strategy::default();
    let finding = OsintNewNodeTypes.run(&db, &mut st).await;
    assert_eq!(
        total_refs(&finding),
        expected_ref_count(&g, "osint-new-node-types"),
        "osint-new-node-types's total RecordRef count against the planted fixture must equal \
         Manifest.evidence_refs' literal; finding = {finding:?}"
    );
}
