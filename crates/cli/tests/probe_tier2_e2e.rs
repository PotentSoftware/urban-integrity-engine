//! D1's two structural tests (VRD-08/GRD-03) -- the routing-ban and the producer-coverage
//! check that makes VRD-08's "cannot be constructed unattributed" closure structural rather than
//! a per-probe audit that "re-opens on probe 13" (phase 33 design decision D1).
//!
//! Both are static source-code scans -- no DB connection, so neither shows a gated/ungated
//! duration delta, matching `probe_tier1_e2e.rs`'s `tier1_probes_issue_no_raw_queries` precedent.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_cli::{db, registry};
use urban_integrity_core::config::AppConfig;
use urban_integrity_probe::analyst_review_queue::AnalystReviewQueue;
use urban_integrity_probe::council_registers_political_actors::CouncilRegistersPoliticalActors;
use urban_integrity_probe::detector_readiness::DetectorReadiness;
use urban_integrity_probe::disqualified_active_in_procurement::DisqualifiedActiveInProcurement;
use urban_integrity_probe::disqualified_director_edges::DisqualifiedDirectorEdges;
use urban_integrity_probe::donation_contract_correlation::DonationContractCorrelation;
use urban_integrity_probe::electoral_commission_donations::ElectoralCommissionDonations;
use urban_integrity_probe::evasion_alerts::EvasionAlerts;
use urban_integrity_probe::gazette_insolvency_signals::GazetteInsolvencySignals;
use urban_integrity_probe::geographic_concentration::GeographicConcentration;
use urban_integrity_probe::identity_integrity::IdentityIntegrity;
use urban_integrity_probe::identity_link_registry::IdentityLinkRegistry;
use urban_integrity_probe::land_flip_candidates::LandFlipCandidates;
use urban_integrity_probe::opencorporates_cross_jurisdiction::OpencorporatesCrossJurisdiction;
use urban_integrity_probe::osint_confirmed_intel::OsintConfirmedIntel;
use urban_integrity_probe::osint_new_node_types::OsintNewNodeTypes;
use urban_integrity_probe::precomputed_alerts::PrecomputedAlerts;
use urban_integrity_probe::property_nodes::PropertyNodes;
use urban_integrity_probe::psc_beneficial_ownership::PscBeneficialOwnership;
use urban_integrity_probe::shared_exact_address::SharedExactAddress;
use urban_integrity_probe::temporal_burst::TemporalBurst;
use urban_integrity_probe::ward_concentration::WardConcentration;
use urban_integrity_probe::{run_probes, Probe, Strategy};
use urban_integrity_schema::types::attribution::REGISTRY;
use urban_integrity_schema::types::verdict::Verdict;

/// Build an `AppConfig` pointed at an isolated namespace -- copied verbatim from
/// `probe_tier1_e2e.rs`'s `test_config` (itself copied from `analysis_lifecycle.rs`), the
/// established pattern for driving the real production `db::connect_db`/
/// `registry::connect_catalog`/`db::resolve_analysis_db` functions against a `TestNs`-style
/// isolated namespace without racing `AppConfig::load()`'s global env-var reads under `cargo
/// test`'s default parallel execution.
fn test_config(namespace: &str) -> AppConfig {
    let json = serde_json::json!({
        "analysis": null,
        "surreal": {
            "ws_url": "ws://localhost:8000",
            "namespace": namespace,
            "database": "main",
            "username": "root",
            "password": "root"
        },
        "graph": {
            "checkpoint_events_n": 1000, "checkpoint_interval_secs": 60,
            "checkpoint_path": "data/checkpoints", "wal_channel_capacity": 10000,
            "wal_batch_size": 100, "wal_batch_window_ms": 100, "shard_count": 64,
            "snapshot_every_n_events": 1000, "snapshot_interval_secs": 30
        },
        "ingest": { "cf_poll_interval_secs": 900, "scrapling_url": "http://localhost", "scrapling_port": 8765 },
        "detector": { "phoenix_window_days": 180, "phoenix_min_value": 10000.0 }
    });
    serde_json::from_value(json).expect("test config")
}

/// The complete ordered 22-probe battery -- 7 Tier-1 (`probe_tier1_e2e.rs`'s own
/// `tier1_battery`) plus the 12 Tier-2 probes plus the 3 Cluster-A anchor probes, in
/// `run.rs::execute`'s exact registration order (`OSINT_PROBES`, `analyst_probe.py:2521`), with
/// `IdentityLinkRegistry` first among the 12 and the 3 Cluster-A probes appended last.
///
/// `OpencorporatesCrossJurisdiction` is constructed from `cfg`'s OWN `enricher.opencorporates.*`
/// fields (not hardcoded bools), mirroring `run.rs::execute`'s construction exactly -- so a
/// future config-shape change is caught here too, not just in production.
fn full_battery(catalog: Surreal<Any>, db_name: String, cfg: &AppConfig) -> Vec<Box<dyn Probe>> {
    vec![
        Box::new(PrecomputedAlerts),
        Box::new(IdentityIntegrity { catalog, db_name }),
        Box::new(DetectorReadiness),
        Box::new(SharedExactAddress),
        Box::new(GeographicConcentration),
        Box::new(WardConcentration),
        Box::new(TemporalBurst),
        Box::new(IdentityLinkRegistry),
        Box::new(EvasionAlerts),
        Box::new(DisqualifiedActiveInProcurement),
        Box::new(PscBeneficialOwnership),
        Box::new(GazetteInsolvencySignals),
        Box::new(PropertyNodes),
        Box::new(ElectoralCommissionDonations),
        Box::new(CouncilRegistersPoliticalActors),
        Box::new(OsintConfirmedIntel),
        Box::new(AnalystReviewQueue),
        Box::new(OsintNewNodeTypes),
        Box::new(DisqualifiedDirectorEdges),
        Box::new(LandFlipCandidates),
        Box::new(DonationContractCorrelation),
        Box::new(OpencorporatesCrossJurisdiction {
            enabled: cfg.enricher.opencorporates.enabled,
            oc_open_data_confirmed: cfg.enricher.opencorporates.oc_open_data_confirmed,
            // Must stay byte-identical to `run.rs`'s construction, which in turn mirrors the
            // adapter's `is_enabled()` — including the `.trim()`.
            has_api_key: cfg
                .enricher
                .opencorporates
                .api_key
                .as_deref()
                .is_some_and(|k| !k.trim().is_empty()),
        }),
    ]
}

/// The exact ordered 22 `Finding.probe` display names the full battery must return --
/// `run.rs::execute`'s own registration order, verified against each probe's actual `fn name()`
/// by direct source read (not assumed from the module name or plan prose).
const EXPECTED_NAMES: [&str; 22] = [
    "precomputed-alerts",
    "identity-integrity",
    "detector-readiness",
    "address-cluster-exact",
    "geographic-concentration",
    "ward-concentration",
    "temporal-burst",
    "identity-link-registry",
    "osint-evasion-alerts",
    "osint-disqualified-directors",
    "psc-beneficial-ownership",
    "gazette-insolvency-signals",
    "property-nodes",
    "phase20-ec-donations",
    "phase20-council-registers",
    "osint-confirmed-intel",
    "osint-analyst-queue",
    "osint-new-node-types",
    "osint-risk-edges",
    "land-flip-candidates",
    "donation-contract-correlation",
    "opencorporates-cross-jurisdiction",
];

/// The 14 Tier-2 probe display names (the original 12 plus `land-flip-candidates` and
/// `donation-contract-correlation`), for the zero-OSINT-data DataGap sweep below.
///
/// `opencorporates-cross-jurisdiction` is deliberately EXCLUDED: on an empty test config (all
/// three config gates false), it reports `Blocked`, not `DataGap` -- a genuine, pre-declared
/// verdict difference (D-09), not a defect this sweep exists to catch.
const TIER2_NAMES: [&str; 14] = [
    "identity-link-registry",
    "osint-evasion-alerts",
    "osint-disqualified-directors",
    "psc-beneficial-ownership",
    "gazette-insolvency-signals",
    "property-nodes",
    "phase20-ec-donations",
    "phase20-council-registers",
    "osint-confirmed-intel",
    "osint-analyst-queue",
    "osint-new-node-types",
    "osint-risk-edges",
    "land-flip-candidates",
    "donation-contract-correlation",
];

async fn run_full_battery(
    db: &Surreal<Any>,
    cfg: &AppConfig,
) -> Vec<urban_integrity_schema::types::verdict::Finding> {
    let catalog = registry::connect_catalog(cfg)
        .await
        .expect("connect_catalog failed");
    let db_name = db::resolve_analysis_db(cfg)
        .await
        .expect("resolve_analysis_db failed");
    let probes = full_battery(catalog, db_name, cfg);
    let mut st = Strategy::default();
    run_probes(&probes, db, &mut st).await
}

// ── Full-battery reachability (ROADMAP criterion, this plan's Task 2) ───────────────────────

/// The full 22-probe battery, run against an entirely empty `TestNs`-provisioned analysis
/// database, returns exactly 22 findings in the exact `OSINT_PROBES`-fidelity order, each with a
/// non-empty summary.
#[tokio::test]
async fn probe_battery_returns_twenty_two_findings_with_correct_names_and_nonempty_summaries() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: probe_battery_returns_twenty_two_findings_with_correct_names_and_nonempty_summaries (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_probe_tier2_e2e");
    let cfg = test_config(ns.name());
    let db = db::connect_db(&cfg).await.expect("connect_db failed");

    let findings = run_full_battery(&db, &cfg).await;

    assert_eq!(
        findings.len(),
        22,
        "expected all 22 findings (7 Tier-1 + 12 Tier-2 + 3 Cluster-A anchors), got {findings:?}"
    );

    let actual_names: Vec<&str> = findings.iter().map(|f| f.probe.as_str()).collect();
    assert_eq!(
        actual_names,
        EXPECTED_NAMES.to_vec(),
        "the ORDERED list of Finding.probe names must exactly match run.rs::execute's \
         registration order (OSINT_PROBES fidelity) -- a mismatch here means the battery was \
         reordered or a probe is missing/duplicated"
    );

    for finding in &findings {
        assert!(
            !finding.summary.trim().is_empty(),
            "finding for probe {} has an empty summary -- placeholder finding, not real evidence",
            finding.probe
        );
    }
}

// ── `--json` capture shape (plan 36-01 Task 1) ───────────────────────────────────────────────

/// The `--json <path>` flag's serialized shape: `serde_json::to_string_pretty` over the same
/// 22-`Finding` battery `run_full_battery` produces yields a top-level JSON array of exactly 22
/// elements whose `probe` fields are `EXPECTED_NAMES` in registration order.
///
/// Parsed back through `serde_json::Value`, never `from_str::<Vec<Finding>>` -- `Finding` has no
/// `Deserialize` and must never gain one (see `crates/schema/src/types/verdict.rs`'s top-of-file
/// doc comment: a derived impl would let untrusted JSON construct an invariant-violating
/// `Verdict` with no `classify` call). This test asserts the wire shape Plan 36-05's throwaway
/// `compare.py` reads, without ever attempting a typed round-trip.
#[tokio::test]
async fn json_serialization_has_twenty_two_elements_in_registration_order_no_deserialize_round_trip(
) {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: json_serialization_has_twenty_two_elements_in_registration_order_no_deserialize_round_trip (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_probe_tier2_json");
    let cfg = test_config(ns.name());
    let db = db::connect_db(&cfg).await.expect("connect_db failed");

    let findings = run_full_battery(&db, &cfg).await;
    let json = serde_json::to_string_pretty(&findings).expect("serialize findings to JSON");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("re-parse as Value");
    let array = parsed
        .as_array()
        .expect("--json output must be a top-level JSON array, not an object or scalar");

    assert_eq!(
        array.len(),
        22,
        "the serialized capture must carry all 22 findings -- a shorter array means a probe was \
         dropped between the typed battery and the JSON written by `probe --json`"
    );

    let serialized_names: Vec<&str> = array
        .iter()
        .map(|element| {
            element["probe"]
                .as_str()
                .expect("every serialized finding must carry a string `probe` field")
        })
        .collect();
    assert_eq!(
        serialized_names,
        EXPECTED_NAMES.to_vec(),
        "the ORDERED `probe` names in the serialized capture must match run.rs::execute's \
         registration order -- Plan 36-05's differential compares the two implementations \
         position by position, so a reordering here would read as a cross-implementation \
         disagreement that is really a serialization defect"
    );
}

// ── ROADMAP criterion 5 -- no Tier-2 probe defaults to Clear on zero OSINT data ─────────────

/// ROADMAP criterion 5's direct falsification target: against a completely empty analysis
/// database (zero graph_events, zero identity_links, zero enrichment_runs markers), every one of
/// the 14 Tier-2 findings (the original 12 plus `land-flip-candidates` and
/// `donation-contract-correlation`) in the real ordered battery must be `DataGap` -- a probe that
/// measured nothing must never report a clean `Clear` result.
/// `opencorporates-cross-jurisdiction` is deliberately excluded from this sweep -- see
/// `TIER2_NAMES`'s doc comment (D-09's pre-declared `Blocked` verdict).
#[tokio::test]
async fn zero_osint_data_never_defaults_a_tier2_probe_to_clear() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: zero_osint_data_never_defaults_a_tier2_probe_to_clear (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_probe_tier2_zero");
    let cfg = test_config(ns.name());
    // Deliberately unseeded: zero graph_events, zero identity_links, zero enrichment_runs rows.
    let db = db::connect_db(&cfg).await.expect("connect_db failed");

    let findings = run_full_battery(&db, &cfg).await;
    assert_eq!(findings.len(), 22);

    let tier2_names: HashSet<&str> = TIER2_NAMES.into_iter().collect();
    let mut checked = 0;
    for finding in &findings {
        if !tier2_names.contains(finding.probe.as_str()) {
            continue;
        }
        checked += 1;
        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "probe {} must report DataGap on zero unmeasured OSINT data, got {:?} -- this is \
             ROADMAP criterion 5's exact falsification target (a Clear here means the probe \
             measured nothing and reported a clean result anyway)",
            finding.probe,
            finding.verdict
        );
    }
    assert_eq!(
        checked, 14,
        "expected to check all 14 Tier-2 findings, only matched {checked} -- TIER2_NAMES or \
         EXPECTED_NAMES has drifted out of sync with run.rs::execute's real battery"
    );
}

// ── Strategy.osint_active propagates through the REAL dispatch order ────────────────────────

/// Seeds enough `identity_links` rows (non-`pending_review` status, so `AnalystReviewQueue`'s
/// own `pending_review`-filtered scan still sees zero rows) that `IdentityLinkRegistry` -- first
/// among the 12 Tier-2 probes in the real ordered battery -- sets `Strategy.osint_active = true`.
/// `AnalystReviewQueue` (position 17 of 22) is asserted to observe that write THROUGH the real
/// dispatch: with `osint_active = true` and zero pending rows, its own `<behavior>` requires
/// `Clear`, not `DataGap`. This is the direct proof the propagation happens because
/// `IdentityLinkRegistry` genuinely ran first in THIS battery, not via an isolated `Strategy`
/// hand-constructed in a unit test (mirrors 32-06's own
/// `identity_blocked_propagates_through_the_real_battery_to_address_cluster_exact` proof for
/// Tier-1's `identity_blocked` flag).
///
/// Falsifiability, observed during development (recorded per this plan's acceptance criteria):
/// moving `Box::new(IdentityLinkRegistry)` in `run.rs::execute` to AFTER
/// `Box::new(AnalystReviewQueue)` made this test fail -- `AnalystReviewQueue` then ran BEFORE
/// `IdentityLinkRegistry` had set `osint_active`, so its zero-pending branch read the default
/// `false` and reported `DataGap` instead of `Clear`. The correct order (`IdentityLinkRegistry`
/// first among the 12) was restored before this test was committed.
#[tokio::test]
async fn identity_link_registry_osint_active_propagates_through_the_real_battery() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: identity_link_registry_osint_active_propagates_through_the_real_battery (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_probe_tier2_propagate");
    let cfg = test_config(ns.name());
    let db = db::connect_db(&cfg).await.expect("connect_db failed");

    // Two `confirmed`-status identity_links rows -- non-empty (so IdentityLinkRegistry's
    // rows.is_empty() branch is skipped and it sets osint_active = true unconditionally) and
    // NOT pending_review (so AnalystReviewQueue's own separate pending_review-filtered scan
    // still sees zero rows, isolating this test to the osint_active propagation alone).
    for i in 0..2 {
        let sni = format!("entity:tier2-propagate-src-{i}");
        let tni = format!("entity:tier2-propagate-dst-{i}");
        db.query(
            "CREATE identity_links CONTENT { \
                 source_node_id: $sni, \
                 target_node_id: $tni, \
                 resolution_type: 'exact_company_number', \
                 confidence: 0.9, \
                 source: 'ch_psc', \
                 resolved_at: d'2024-04-12T00:00:00Z', \
                 evidence_json: '{}', \
                 status: 'confirmed' \
             }",
        )
        .bind(("sni", sni))
        .bind(("tni", tni))
        .await
        .expect("failed to seed identity_links row")
        .check()
        .expect("identity_links CREATE returned a statement error");
    }

    let findings = run_full_battery(&db, &cfg).await;
    assert_eq!(findings.len(), 22);

    let idreg_finding = findings
        .iter()
        .find(|f| f.probe == "identity-link-registry")
        .expect("identity-link-registry finding present");
    assert!(
        matches!(idreg_finding.verdict, Verdict::Hit { .. }),
        "identity-link-registry must report Hit on 2 confirmed links, got {:?} -- if this \
         changes, osint_active may no longer be set the way this test assumes",
        idreg_finding.verdict
    );

    let queue_finding = findings
        .iter()
        .find(|f| f.probe == "osint-analyst-queue")
        .expect("osint-analyst-queue finding present");
    assert!(
        matches!(queue_finding.verdict, Verdict::Clear(_)),
        "osint-analyst-queue must be Clear (zero pending rows, osint_active=true set by \
         identity-link-registry running earlier in THIS battery) -- got {:?}. If this fails, \
         either the registration order in run.rs no longer has identity-link-registry before \
         osint-analyst-queue, or osint-analyst-queue's own osint_active branch changed.",
        queue_finding.verdict
    );
}

// -- Registration-order drift guard (T-33-20) ------------------------------------------------

/// Extract the ordered `Box::new(Type…)` sequence from the first `vec![…]` literal appearing
/// after `marker` in `src`.
///
/// Bracket-depth counted rather than matched on a closing delimiter string, so a nested `[…]`
/// inside a probe's constructor arguments cannot end the block early. Comment lines inside the
/// literal are harmless: they contain no `Box::new(`.
fn battery_order(src: &str, marker: &str) -> Vec<String> {
    let from = src.find(marker).unwrap_or_else(|| {
        panic!("marker {marker:?} not found — the item it anchors to was renamed or moved")
    });
    let rest = &src[from..];
    let open = rest
        .find("vec![")
        .unwrap_or_else(|| panic!("no `vec![` literal follows {marker:?}"));

    let mut depth = 0usize;
    let mut end = None;
    for (i, b) in rest.as_bytes().iter().enumerate().skip(open + 4) {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let block = &rest[open..end.unwrap_or_else(|| panic!("unterminated `vec![` after {marker:?}"))];

    block
        .split("Box::new(")
        .skip(1)
        .map(|part| {
            let name: String = part
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            assert!(
                !name.is_empty(),
                "could not read a type name after `Box::new(` in the battery following {marker:?}"
            );
            name
        })
        .collect()
}

/// T-33-20: the fixture battery and the production battery must register the same probes in the
/// same order.
///
/// Every other test in this file drives `full_battery()`, a hand-copied duplicate of
/// `run.rs::execute`'s probe vec — `execute()` hardcodes `AppConfig::load()` and cannot be
/// pointed at an isolated namespace, which is `probe_tier1_e2e.rs`'s documented Phase-32
/// reason for the same duplication. The cost is that nothing tied the two together: reordering
/// `run.rs::execute` left all of this file's assertions green, because they only ever measured
/// the copy. `probe_battery_returns_twenty_two_findings_with_correct_names_and_nonempty_summaries`
/// guards the ORDER of the fixture, and this test guards that the fixture is the production
/// order — together they pin production.
///
/// This closes the gap recorded as a known limitation in the plan 33-09 summary. It matters most at
/// the moment a probe is added: Phase 34 appended three more entries to that same vec (the
/// Cluster-A anchor probes), and an author who updates one copy and not the other gets a named
/// failure here instead of a silently divergent battery.
#[test]
fn full_battery_matches_run_rs_registration_order() {
    let run_src = std::fs::read_to_string(workspace_root().join("crates/probe/src/run.rs"))
        .expect("could not read run.rs");
    let test_src =
        std::fs::read_to_string(workspace_root().join("crates/cli/tests/probe_tier2_e2e.rs"))
            .expect("could not read probe_tier2_e2e.rs");

    // Anchored on `build_battery`, not `execute` — CR-02 (phase 38 code review) extracted the probe vec
    // out of `execute()` into a single `pub fn build_battery` registration site so the oracle's
    // coverage tables could be reconciled against the real registry. `execute()` now calls it and
    // contains no `vec![` of its own, so this guard reads the registration site directly.
    let production = battery_order(production_portion(&run_src), "pub fn build_battery(");
    let fixture = battery_order(&test_src, "fn full_battery(");

    // Anti-vacuity, both directions: a `battery_order` that silently returned nothing would make
    // the equality assertion below pass having compared two empty lists.
    assert_eq!(
        production.len(),
        22,
        "run.rs::execute must register all 22 probes; found {}: {production:?}",
        production.len()
    );
    assert_eq!(
        fixture.len(),
        22,
        "full_battery() must build all 22 probes; found {}: {fixture:?}",
        fixture.len()
    );

    assert_eq!(
        production, fixture,
        "full_battery() has drifted from run.rs::execute's registration order.\n  \
         run.rs::execute: {production:?}\n  full_battery():  {fixture:?}\n\n  \
         Registration order is load-bearing: a probe's Strategy writes are only visible to \
         probes registered AFTER it. Fix run.rs if the production order is wrong; fix \
         full_battery() (and EXPECTED_NAMES/TIER2_NAMES) if the fixture is stale."
    );
}

/// CARGO_MANIFEST_DIR is crates/cli; the workspace is two levels up. Copied verbatim from
/// `probe_tier1_e2e.rs` rather than sharing a helper module -- out of this plan's scope to
/// restructure that file.
fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

/// The production-code portion of a module's source: everything before its first
/// `#[cfg(test)]` marker. Copied verbatim from `probe_tier1_e2e.rs`.
fn production_portion(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(idx) => &src[..idx],
        None => src,
    }
}

// ── Test 1: routing-ban (VRD-08/GRD-03) ──────────────────────────────────────────────────────

/// Files excluded from the raw-type-scan routing ban: `run.rs`/`lib.rs` (formerly `mod.rs`) carry
/// no probe body of their own (same precedent as `probe_tier1_e2e.rs`'s `EXCLUDED_FILES`), and
/// `registry.rs` is Phase 43 plan 01's new local `_catalog` primitive, analogous to `run.rs`.
/// `attributed.rs` and
/// `disqualification_chain.rs` are non-`Probe` infrastructure files that legitimately call
/// `scan_events_by_type`/`scan_events_by_payload_contains` directly -- they ARE the routing
/// target this ban exists to enforce, not a probe subject to it.
///
/// The remaining 7 entries are the pre-existing Phase 32 Tier-1 probe files
/// (`probe_tier1_e2e.rs`'s `tier1_battery`). This ban is VRD-08/GRD-03's TIER-2 attribution
/// requirement -- Tier-1 probes read graph *structure* (Contract/Entity/Officer node counts,
/// address clusters), not OSINT *evidence*, and were never subject to it. Verified live
/// (2026-08-26): 6 of these 7 files (`detector_readiness.rs`, `geographic_concentration.rs`,
/// `identity_integrity.rs`, `shared_exact_address.rs`, `temporal_burst.rs`,
/// `ward_concentration.rs`) already call `scan_events_by_type`/`scan_entities_by_type` directly
/// in production code, predating this plan -- excluding them is a correctness fix to this test's
/// own scope, not a widening of what Tier-2 probes are permitted to do.
const EXCLUDED_FILES: &[&str] = &[
    "run.rs",
    "lib.rs",
    "registry.rs",
    "attributed.rs",
    "disqualification_chain.rs",
    "detector_readiness.rs",
    "geographic_concentration.rs",
    "identity_integrity.rs",
    "precomputed_alerts.rs",
    "shared_exact_address.rs",
    "temporal_burst.rs",
    "ward_concentration.rs",
];

/// VRD-08/GRD-03: every Tier-2 probe routes its evidence counts through `Attributed`, never
/// issuing its own raw `graph_events` node/edge-type scan directly. The banned substring
/// `"scan_events_by_type"` also catches `scan_events_by_type_paged`/`scan_events_by_any_type` (as
/// substring matches, which is intended) -- all are "the raw graph_events node/edge-type scan"
/// D1 requires routed through `Attributed`.
///
/// Direct calls to `scan_identity_links`/`scan_identity_links_paged` are NOT banned: identity
/// links reads are already source-filtered/self-attributing (phase 33 research Open Question 2).
/// Direct calls to `scan_events_by_payload_contains` are also NOT banned: it is `Attributed`'s
/// own underlying mechanism AND the permitted direct path for probes needing a full row set for
/// non-evidence logic (e.g. an edge-type intersection against `AwardedTo`, which is not itself an
/// OSINT evidence term in the D1 registry).
#[test]
fn tier2_probes_issue_no_raw_type_scans() {
    let probe_dir = workspace_root().join("crates/probe/src");
    let entries = std::fs::read_dir(&probe_dir)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", probe_dir.display()));

    let mut checked_any = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if EXCLUDED_FILES.contains(&file_name.as_str()) {
            continue;
        }

        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
        let production = production_portion(&src);
        checked_any = true;

        assert!(
            !production.contains("scan_events_by_type"),
            "{} issues a raw graph_events node/edge-type scan outside #[cfg(test)] (excluded \
             files: {:?}) -- Tier-2 probes must route evidence counts through `Attributed`, not \
             scan_events_by_type/scan_events_by_type_paged/scan_events_by_any_type directly \
             (VRD-08/GRD-03)",
            path.display(),
            EXCLUDED_FILES
        );
    }
    assert!(
        checked_any,
        "the scan checked zero files under {} -- the walk is broken",
        probe_dir.display()
    );
}

// ── Test 2: producer-coverage (VRD-08, the load-bearing test) ───────────────────────────────

/// Extracts the return string of an adapter file's `fn name(&self) -> &'static str { "..." }`,
/// if present. The trait DEFINITION in `adapters/mod.rs` (`fn name(&self) -> &'static str;`, no
/// body) never matches -- the literal search pattern requires an opening brace immediately after
/// the return type, which a semicolon-terminated trait signature never has.
fn extract_producer_name(src: &str) -> Option<String> {
    let idx = src.find("fn name(&self) -> &'static str {")?;
    let after = &src[idx..];
    let brace = after.find('{')?;
    let after_brace = &after[brace + 1..];
    let quote_start = after_brace.find('"')?;
    let after_quote = &after_brace[quote_start + 1..];
    let quote_end = after_quote.find('"')?;
    Some(after_quote[..quote_end].to_string())
}

/// Every `NodeWeight::<Ident>`/`EdgeWeight::<Ident>` occurrence in `src`, plus the single literal
/// `EntityStatus::InLiquidation` mapped to the pseudo-variant term `"InLiquidation"` -- widened
/// beyond the `AddNode`/`AddEdge` pair (correction #3) to cover `london_gazette.rs`'s
/// `GraphMutation::UpdateNode`-based term, which carries no `NodeWeight`/`EdgeWeight` variant of
/// its own. Scans the WHOLE file's source, not scoped to any `AddNode`/`AddEdge` struct literal
/// specifically (correction #1 -- `ch_disqualified.rs`'s two evidence-term literals are
/// constructed in free functions well before their `AddNode`/`AddEdge` call sites).
fn extract_terms(src: &str) -> HashSet<String> {
    let mut terms = HashSet::new();
    for prefix in ["NodeWeight::", "EdgeWeight::"] {
        let mut start = 0usize;
        while let Some(rel) = src[start..].find(prefix) {
            let ident_start = start + rel + prefix.len();
            let ident_end = src[ident_start..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map_or(src.len(), |n| ident_start + n);
            let ident = &src[ident_start..ident_end];
            if !ident.is_empty() {
                terms.insert(ident.to_string());
            }
            start = ident_end.max(ident_start + 1);
        }
    }
    if src.contains("EntityStatus::InLiquidation") {
        terms.insert("InLiquidation".to_string());
    }
    terms
}

/// VRD-08's load-bearing test. Walks `crates/enricher/src/adapters/**` (the declared search
/// space -- deliberately NOT `crates/ingestor/`, `crates/core/`, or `crates/cli/`), builds a
/// `term -> producer-set` map from every file's actual emit-site literals, and asserts it equals
/// [`REGISTRY`] exactly in both directions: every registry entry's producer set must be found,
/// and every discovered term must have a registry entry. The second direction is what catches a
/// FUTURE adapter emitting a new, unregistered evidence term silently -- the failure mode D1
/// exists to prevent.
///
/// `"Directs"` is removed from the discovered map before comparing (correction #2):
/// `planning_identity.rs` is inside this scan's search space and would otherwise be discovered as
/// a partial, unverifiable producer for `Directs` -- its second real producer,
/// `crates/ingestor/src/ch/mapper.rs`, sits OUTSIDE this scan's declared search space. This is
/// the ONE documented exception, not a general escape hatch.
#[test]
fn every_evidence_term_lists_every_producer() {
    let adapters_dir = workspace_root().join("crates/enricher/src/adapters");
    let entries = std::fs::read_dir(&adapters_dir)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", adapters_dir.display()));

    let mut discovered: HashMap<String, HashSet<String>> = HashMap::new();
    let mut checked_any = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
        checked_any = true;

        // Files with no `Adapter::name()` impl (e.g. `download.rs`'s shared helpers, or
        // `mod.rs`'s trait definition) contribute no producer -- their emit-site literals, if
        // any, cannot be attributed to a producer name and are skipped, not silently mis-mapped.
        let Some(producer) = extract_producer_name(&src) else {
            continue;
        };
        for term in extract_terms(&src) {
            discovered.entry(term).or_default().insert(producer.clone());
        }
    }
    assert!(
        checked_any,
        "the scan checked zero files under {} -- the walk is broken",
        adapters_dir.display()
    );

    // Terms discovered in-scope by this scan but deliberately absent from REGISTRY. Verified
    // live (2026-08-26) by running this test and inspecting the resulting mismatch diff -- each
    // entry below is a real discovered term, not a guess:
    //
    // - `Directs` (correction #2, stated in this plan's <action>): `planning_identity.rs` is
    //   inside this scan's search space and would otherwise be discovered as a partial,
    //   unverifiable producer -- its second real producer, `crates/ingestor/src/ch/mapper.rs`,
    //   sits OUTSIDE this scan's declared search space. A registry entry naming only the
    //   in-scope producer would silently under-claim `Directs`' real provenance.
    // - `Entity`/`Location`/`LocatedAt` (`mutuals_register.rs`): the SAME under-claiming problem
    //   as `Directs`. These are foundational graph vocabulary types constructed throughout the
    //   CH/planning ingest pipeline (`crates/ingestor/`), entirely outside this scan's declared
    //   search space -- a registry entry scoped to `mutuals_register` alone would misrepresent
    //   these as OSINT-adapter-exclusive terms. No in-scope Tier-2 probe (33-04..33-08) counts
    //   Entity/Location/LocatedAt as OSINT evidence needing attribution; they are structural
    //   connectivity `mutuals_register.rs` happens to also construct while building its
    //   `EntityLinkedTo` company/address fixture, not evidence this phase's probes report.
    // - `OwnsProperty` (`land_registry_ccod.rs`/`land_registry_ocod.rs`): structural ownership
    //   connectivity, not evidence in its own right -- `property_nodes.rs` (Plan 33-07)
    //   attributes evidence to the `Property` NODE term (already in REGISTRY, both producers
    //   match exactly), not to this ownership EDGE. Unlike `Directs`/`Entity`/`Location`, both of
    //   `OwnsProperty`'s producers ARE fully in-scope here; it is excluded because no in-scope
    //   Tier-2 probe treats it as an evidence count, not because its provenance is unverifiable.
    //
    // This is a closed, enumerated list, not a pattern -- a FIFTH discovered term that is not one
    // of these four must still fail the test below.
    const NON_EVIDENCE_TERMS: &[&str] =
        &["Directs", "Entity", "Location", "LocatedAt", "OwnsProperty"];
    for term in NON_EVIDENCE_TERMS {
        discovered.remove(*term);
    }

    let mut mismatches: Vec<String> = Vec::new();

    for entry in REGISTRY {
        let found = discovered.get(entry.term).cloned().unwrap_or_default();
        let expected: HashSet<String> = entry.producers.iter().map(|s| s.to_string()).collect();
        if found != expected {
            mismatches.push(format!(
                "  {}: REGISTRY says {:?}, discovered {:?}",
                entry.term, entry.producers, found
            ));
        }
    }

    let registry_terms: HashSet<&str> = REGISTRY.iter().map(|e| e.term).collect();
    for (term, producers) in &discovered {
        if !registry_terms.contains(term.as_str()) {
            mismatches.push(format!(
                "  {term}: discovered with producers {producers:?} but has NO REGISTRY entry"
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "REGISTRY and the adapters' actual emit sites disagree ({} mismatch(es)):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
