//! Wave 5, plan 35-18 — e2e proof of ROADMAP criterion 6 (RPT-05): both contradiction checks
//! fire when triggered AND state a clean outcome explicitly when nothing is wrong, closing
//! D-15's "no contradiction found" vs "the check did not run" distinction.
//!
//! # Shared scaffolding block
//!
//! `test_config`/`full_battery`/`build_entries` below are this file's own copy of the block
//! documented at the top of `evidence_report_structure.rs` (plan 35-15) — this phase's deliberate
//! per-test-binary duplication convention, copied from what that file ACTUALLY does, including
//! its recorded `NsGuard::with_prefix` deviation (two `TestNs::connect_with_prefix` calls would
//! break `IdentityIntegrity`'s same-namespace invariant, T-32-01).
//!
//! # Deviation: check 1's trigger is exercised by fixture, not by seeded graph data
//!
//! This plan's Task 1 specifies a DB fixture that makes `ward-concentration`'s and
//! `temporal-burst`'s populations diverge, then asserts the rendered Contradictions section
//! reports check 1 firing. The divergence mechanism is real and was confirmed in source rather
//! than guessed: `ward_concentration.rs` counts every Contract `NodeAdded` row
//! (`scan_entities_by_type(...).len()`), while `temporal_burst.rs` counts only rows that produced
//! a usable day bucket, skipping any row whose `graph_events.timestamp` is absent or non-numeric
//! — its own inline comment says `rows.len()` "may exceed this if some rows lacked a usable
//! timestamp". Seeding one timestamp-less Contract therefore makes the two figures differ.
//!
//! That fixture cannot reach check 1's comparison, for a reason that is itself a finding:
//! `check_1` returns "could not evaluate" unless ALL THREE of ward-concentration, temporal-burst
//! and identity-integrity carry a measured population, and `identity-integrity` reports `DataGap`
//! (population `None`) unless the analysis has a `_catalog.analyses` baseline row. It does not on
//! a bare test namespace — and it does not on the live `an_anglesey_aebaa785` analysis either,
//! where a real `probe` run this session rendered exactly:
//!
//! ```text
//! **Check 1** (independently-measured populations): could not evaluate — identity-integrity
//! carried no measured population this run (DataGap/Blocked).
//! ```
//!
//! So check 1's comparison branch is unreachable through seeded graph data alone. Its firing and
//! clean branches are therefore driven directly through `contradictions::render` with hand-built
//! `ReportEntry` fixtures — a pure function of its arguments (D-09), so this exercises the exact
//! code path the assembled document uses. The DB-gated test below still proves the section
//! renders in a real battery and that both checks emit a distinguishable outcome.
//!
//! See the plan 35-18 summary for the two findings this raised.

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
use urban_integrity_probe::report;
use urban_integrity_probe::shared_exact_address::SharedExactAddress;
use urban_integrity_probe::temporal_burst::TemporalBurst;
use urban_integrity_probe::ward_concentration::WardConcentration;
use urban_integrity_probe::{run_probes, MotifKind, Probe, Strategy};
use urban_integrity_schema::types::edge::{
    AwardedToData, DirectsData, EdgeMeta, EdgeWeight, LocatedAtData, RepresentsData,
};
use urban_integrity_schema::types::node::{
    ApplicantProvenance, ContractData, EntityData, LocationData, NodeWeight, OfficerData,
};
use urban_integrity_schema::types::verdict::Finding;
use urban_integrity_schema::types::verdict::PopulationSource;

// ── Shared scaffolding (see module doc comment above) ───────────────────────────────────────

/// Build an `AppConfig` pointed at an isolated namespace — copied verbatim from
/// `probe_tier1_e2e.rs`/`probe_tier2_e2e.rs`'s own `test_config` (itself copied from
/// `analysis_lifecycle.rs`), the established pattern for driving the real production
/// `db::connect_db`/`registry::connect_catalog`/`db::resolve_analysis_db` functions against an
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

/// The complete ordered 22-probe battery — 7 Tier-1 plus the 12 Tier-2 probes plus the 3
/// Cluster-A anchor probes, in `run.rs::execute`'s exact registration order, copied verbatim from
/// `probe_tier2_e2e.rs`'s own `full_battery`.
///
/// `OpencorporatesCrossJurisdiction` is constructed from `cfg`'s OWN `enricher.opencorporates.*`
/// fields (not hardcoded bools), mirroring `run.rs::execute`'s construction exactly.
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
            has_api_key: cfg
                .enricher
                .opencorporates
                .api_key
                .as_deref()
                .is_some_and(|k| !k.trim().is_empty()),
        }),
    ]
}

/// Replicates `run.rs::execute`'s zip logic exactly: pairs each probe's `tier()`/`hypothesis()`
/// with its producing `Finding`, in the SAME 1:1 order `run_probes` returns them in.
fn build_entries(probes: &[Box<dyn Probe>], findings: Vec<Finding>) -> Vec<report::ReportEntry> {
    probes
        .iter()
        .zip(findings)
        .map(|(p, finding)| report::ReportEntry {
            tier: p.tier(),
            hypothesis: p.hypothesis(),
            motif_kind: p.motif_kind(),
            finding,
            population_source: PopulationSource::SelfReported {
                reason: "integration test fixture",
            },
        })
        .collect()
}

// ── Fixture seeding — copied from `probe_tier1_e2e.rs` (node/edge shape) and
// `probe_tier2_e2e.rs` (identity_links shape), per this plan's fixture-corpus discretion ───────

async fn seed_node(db: &Surreal<Any>, entity_id: &str, weight: &NodeWeight) {
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
        .expect("failed to seed node event")
        .check()
        .expect("seed node event returned a statement error");
}

async fn seed_edge(
    db: &Surreal<Any>,
    label: &str,
    src_id: &str,
    dst_id: &str,
    weight: &EdgeWeight,
) {
    let weight_json = serde_json::to_string(weight).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("EdgeAdded-{label}-{src_id}-{dst_id}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "EdgeAdded",
                "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed edge event")
        .check()
        .expect("seed edge event returned a statement error");
}

fn edge_meta() -> EdgeMeta {
    EdgeMeta {
        created_at: chrono::Utc::now(),
        effective_from: None,
        effective_to: None,
    }
}

/// A small but non-trivial fixture: one Entity/Officer/Location/Contract node each, one edge
/// each, and two `confirmed`-status `identity_links` rows — enough to give several probes real
/// (non-`DataGap`) verdicts, so the twice-run diff test exercises more than the trivial all-empty
/// path.
async fn seed_small_fixture(db: &Surreal<Any>) {
    seed_node(
        db,
        "GB-COH:E2E18001",
        &NodeWeight::Entity(EntityData {
            name: "Plan 35-18 Test Entity Ltd".to_string(),
            company_number: Some("E2E18001".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    )
    .await;
    seed_node(
        db,
        "director:e2e-35-18-director",
        &NodeWeight::Officer(OfficerData {
            name: "Plan 35-18 Test Director".to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: Some("E2E18001".to_string()),
        }),
    )
    .await;
    seed_node(
        db,
        "location:e2e-35-18-loc",
        &NodeWeight::Location(LocationData {
            name: "1 Plan Street".to_string(),
            postcode: Some("SW1A1AA".to_string()),
            address_line_1: Some("1 Plan Street".to_string()),
            locality: Some("Testville".to_string()),
        }),
    )
    .await;
    seed_node(
        db,
        "planning:E2E18-C1",
        &NodeWeight::Contract(ContractData {
            name: "Plan 35-18 Test Planning Application".to_string(),
            ocid: None,
            value_gbp: None,
            status: Some("approved".to_string()),
            date_published: Some("2026-01-01".to_string()),
            ward_code: Some("E05001001".to_string()),
            tender_ref: None,
            application_ref: Some("E2E/2026/0018".to_string()),
            application_received_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            applicant_provenance: Some(ApplicantProvenance::Published),
            portal_detail_handle: None,
        }),
    )
    .await;

    seed_edge(
        db,
        "directs",
        "director:e2e-35-18-director",
        "GB-COH:E2E18001",
        &EdgeWeight::Directs {
            meta: edge_meta(),
            data: DirectsData {
                details: String::new(),
                officer_role: Some("director".to_string()),
            },
        },
    )
    .await;
    seed_edge(
        db,
        "awarded-to",
        "GB-COH:E2E18001",
        "planning:E2E18-C1",
        &EdgeWeight::AwardedTo {
            meta: edge_meta(),
            data: AwardedToData {
                details: String::new(),
            },
        },
    )
    .await;
    seed_edge(
        db,
        "located-at",
        "planning:E2E18-C1",
        "location:e2e-35-18-loc",
        &EdgeWeight::LocatedAt {
            meta: edge_meta(),
            data: LocatedAtData {
                details: String::new(),
            },
        },
    )
    .await;
    seed_edge(
        db,
        "represents",
        "ward:E05001001",
        "planning:E2E18-C1",
        &EdgeWeight::Represents {
            meta: edge_meta(),
            data: RepresentsData {
                details: String::new(),
            },
        },
    )
    .await;

    // Two `confirmed`-status identity_links rows — non-empty and NOT `pending_review`, matching
    // `probe_tier2_e2e.rs`'s own `identity_link_registry_osint_active_propagates_through_the_
    // real_battery` shape, so `IdentityLinkRegistry` sets `Strategy.osint_active = true`.
    for i in 0..2 {
        let sni = format!("entity:e2e-35-18-src-{i}");
        let tni = format!("entity:e2e-35-18-dst-{i}");
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
}

// ── Fixture builders for the pure-function checks ────────────────────────────────────────────

use std::collections::HashMap;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding as VFinding, Severity, SourceAttribution, Verdict,
};

const CONTRADICTION: &str = "CONTRADICTION";
const NO_CONTRADICTION: &str = "no contradiction found";
const COULD_NOT_EVALUATE: &str = "could not evaluate";

/// A `ReportEntry` whose verdict carries an explicit measured population/capable pair.
fn entry_with_population(probe: &str, population: u64) -> report::ReportEntry {
    report::ReportEntry {
        population_source: PopulationSource::SelfReported {
            reason: "integration test fixture",
        },
        tier: "Tier-1",
        hypothesis: "fixture hypothesis",
        motif_kind: MotifKind::Absence,
        finding: VFinding {
            probe: probe.to_string(),
            verdict: Verdict::classify(population, population, 0, u64::MAX, ""),
            summary: format!("{probe} fixture summary."),
            cause: None,
            severity: Severity::Low,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        },
    }
}

/// A `ReportEntry` carrying an attribution, for check 2.
fn entry_with_attribution(probe: &str, source: &'static str, value: u64) -> report::ReportEntry {
    report::ReportEntry {
        population_source: PopulationSource::SelfReported {
            reason: "integration test fixture",
        },
        tier: "Tier-2",
        hypothesis: "fixture hypothesis",
        motif_kind: MotifKind::Absence,
        finding: VFinding {
            probe: probe.to_string(),
            verdict: Verdict::classify(10, 10, 1, u64::MAX, ""),
            summary: format!("{probe} fixture summary."),
            cause: None,
            severity: Severity::Low,
            evidence: vec![EvidenceRow {
                text: "fixture evidence row".to_string(),
                refs: vec![],
            }],
            attributions: vec![SourceAttribution { source, value }],
            remediation_hint: None,
        },
    }
}

/// The three probes check 1 compares, all agreeing.
fn agreeing_population_entries(n: u64) -> Vec<report::ReportEntry> {
    vec![
        entry_with_population("ward-concentration", n),
        entry_with_population("temporal-burst", n),
        entry_with_population("identity-integrity", n),
    ]
}

fn strategy_with_runs(sources: &[&str]) -> Strategy {
    let mut st = Strategy::default();
    let mut runs = HashMap::new();
    for s in sources {
        runs.insert(
            (*s).to_string(),
            urban_integrity_core::persistence::scan::RunMarker {
                source: (*s).to_string(),
                last_run_at: 1_700_000_000_000,
                candidates_found: 1,
                auto_linked: 1,
                pending_review: 0,
                discarded: 0,
            },
        );
    }
    st.enrichment_runs = Some(runs);
    st
}

/// Returns just the named check's own line from a rendered Contradictions section.
///
/// Assertions MUST be scoped this way: the section carries both checks, so a bare
/// `section.contains("no contradiction found")` is satisfied by the OTHER check and silently
/// tests nothing. For a firing check the contradiction detail lines follow its header line, so
/// this returns the header plus every following line up to the next `**Check` header.
fn check_block(section: &str, header: &str) -> String {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in section.lines() {
        if line.starts_with(header) {
            in_block = true;
            out.push(line);
            continue;
        }
        if in_block {
            if line.starts_with("**Check") {
                break;
            }
            out.push(line);
        }
    }
    assert!(!out.is_empty(), "{header} not found in section:\n{section}");
    out.join("\n")
}

/// Returns just the named SUB-check's own line from a rendered Contradictions section.
///
/// Check 1 states its outcome on two independent sub-check lines, one per sub-comparison, so
/// `check_block(section, "**Check 1**")` spans both and a bare `.contains()` over it is satisfied
/// by whichever sub-check the test is not about — the same scoping trap, one level down.
fn sub_check(section: &str, prefix: &str) -> String {
    section
        .lines()
        .find(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("{prefix} not found in section:\n{section}"))
        .to_string()
}

const SUB_1A: &str = "- **1a**";
const SUB_1B: &str = "- **1b**";

fn contradictions_section(entries: &[report::ReportEntry], st: &Strategy) -> String {
    report::contradictions::render(entries, st)
}

// ── Check 1 — fires ──────────────────────────────────────────────────────────────────────────

/// Sub-check 1a: ward-concentration and temporal-burst disagreeing is reported as a
/// CONTRADICTION naming both probes and both figures.
///
/// The divergence this reproduces is the real one described in the module doc: a Contract row
/// without a usable `graph_events.timestamp` is counted by ward-concentration and skipped by
/// temporal-burst.
#[test]
fn contradiction_check1_population_disagreement() {
    let entries = vec![
        entry_with_population("ward-concentration", 106),
        entry_with_population("temporal-burst", 105),
        entry_with_population("identity-integrity", 40),
    ];
    let st = strategy_with_runs(&[]);
    let section = check_block(&contradictions_section(&entries, &st), "**Check 1**");

    assert!(
        section.contains(CONTRADICTION),
        "check 1 did not report a contradiction although ward-concentration (106) and \
         temporal-burst (105) disagree; section:\n{section}"
    );
    for needle in ["ward-concentration", "temporal-burst", "106", "105"] {
        assert!(
            section.contains(needle),
            "check 1's contradiction text omits {needle:?}; section:\n{section}"
        );
    }
}

/// Sub-check 1b: a planning-filtered subset exceeding its unfiltered superset is reported, and is
/// a DIFFERENT contradiction from 1a — proving the two sub-comparisons are independent rather
/// than one equality test wearing two names.
#[test]
fn contradiction_check1_subset_exceeds_superset() {
    let entries = vec![
        entry_with_population("ward-concentration", 100),
        entry_with_population("temporal-burst", 100),
        entry_with_population("identity-integrity", 250),
    ];
    let st = strategy_with_runs(&[]);
    let section = check_block(&contradictions_section(&entries, &st), "**Check 1**");

    assert!(section.contains(CONTRADICTION));
    assert!(
        section.contains("250"),
        "1b's text must name the offending identity-integrity figure; section:\n{section}"
    );
    assert!(
        section.contains("never exceed"),
        "1b's text must state the invariant it violates; section:\n{section}"
    );
    // 1a must NOT also fire here — ward and temporal agree.
    assert!(
        !section.contains("must agree"),
        "1a fired although ward-concentration and temporal-burst both measured 100 — the two \
         sub-comparisons are not independent; section:\n{section}"
    );
}

// ── Check 1 — clean and could-not-evaluate are distinguishable ───────────────────────────────

/// D-15's core requirement: "no contradiction found" is stated explicitly, and is textually
/// distinct from "could not evaluate".
#[test]
fn contradiction_check1_states_clean_outcome_explicitly() {
    let entries = agreeing_population_entries(100);
    let st = strategy_with_runs(&[]);
    let section = check_block(&contradictions_section(&entries, &st), "**Check 1**");

    assert!(
        section.contains(NO_CONTRADICTION),
        "check 1 must state a clean outcome explicitly, never render silence; section:\n{section}"
    );
    assert!(
        !section.contains(CONTRADICTION),
        "check 1 reported a contradiction on agreeing populations; section:\n{section}"
    );
    assert!(
        section.contains("ward-concentration=100"),
        "the clean outcome must show the figures it compared, so a reader can check the claim; \
         section:\n{section}"
    );
}

/// A probe carrying no measured population makes the sub-check that READS it report "could not
/// evaluate" — never a clean result, and never a clean result mislabelled.
///
/// This is the live-corpus shape: `identity-integrity` reports DataGap on `an_anglesey_aebaa785`
/// whenever the `_catalog.analyses` coverage baseline is absent or regressed. Only 1b reads that
/// probe, so only 1b may go unevaluated — see the companion test below for the other half.
#[test]
fn contradiction_check1_unmeasured_population_is_not_a_clean_result() {
    let mut entries = agreeing_population_entries(100);
    entries[2].finding.verdict = Verdict::data_gap("no baseline row");
    let st = strategy_with_runs(&[]);
    let section = check_block(&contradictions_section(&entries, &st), "**Check 1**");
    let b = sub_check(&section, SUB_1B);

    assert!(
        b.contains(COULD_NOT_EVALUATE),
        "an unmeasured population must render 'could not evaluate'; 1b:\n{b}"
    );
    assert!(
        !b.contains(NO_CONTRADICTION),
        "'could not evaluate' must never be reported as 'no contradiction found' — that is \
         exactly the confusion D-15 exists to prevent; 1b:\n{b}"
    );
    assert!(
        b.contains("identity-integrity"),
        "the could-not-evaluate text must name which probe was unmeasured; 1b:\n{b}"
    );
}

/// The other half, and the regression guard for the defect the sub-check split fixes: an
/// unmeasured `identity-integrity` must NOT stop 1a comparing `ward-concentration` against
/// `temporal-burst`.
///
/// A single population guard covering all three probes made 1a unreachable on every real run —
/// RPT-05's headline check rendered "could not evaluate" in production without ever having
/// compared anything. 1a reads neither `identity-integrity` nor anything derived from it.
#[test]
fn contradiction_check1a_evaluates_although_identity_integrity_is_unmeasured() {
    let mut entries = agreeing_population_entries(105_613);
    entries[2].finding.verdict = Verdict::data_gap("no baseline row");
    let st = strategy_with_runs(&[]);
    let section = check_block(&contradictions_section(&entries, &st), "**Check 1**");
    let a = sub_check(&section, SUB_1A);

    assert!(
        a.contains(NO_CONTRADICTION),
        "1a must reach its comparison and state a clean outcome; 1a:\n{a}"
    );
    assert!(
        !a.contains(COULD_NOT_EVALUATE),
        "1a was aborted by a probe it does not read — this is the defect the sub-check split \
         exists to fix; 1a:\n{a}"
    );
    assert!(
        a.contains("ward-concentration=105613") && a.contains("temporal-burst=105613"),
        "1a's clean outcome must show both figures it compared; 1a:\n{a}"
    );
}

// ── Check 2 — fires, clean, and could-not-evaluate ───────────────────────────────────────────

/// A non-zero attribution whose source carries no `enrichment_runs` marker is reported as a
/// contradiction naming both the source and the attributed value.
#[test]
fn contradiction_check2_markerless_attribution() {
    let mut entries = agreeing_population_entries(100);
    entries.push(entry_with_attribution(
        "council-registers-political-actors",
        "council_registers",
        7,
    ));
    // Marker deliberately absent for `council_registers`.
    let st = strategy_with_runs(&["ch_psc"]);
    let section = check_block(&contradictions_section(&entries, &st), "**Check 2**");

    assert!(
        section.contains(CONTRADICTION),
        "check 2 did not fire on an attributed source with no run marker; section:\n{section}"
    );
    for needle in ["council_registers", "7"] {
        assert!(
            section.contains(needle),
            "check 2's contradiction text omits {needle:?}; section:\n{section}"
        );
    }
}

/// The same attribution WITH its marker present is reported clean, explicitly.
#[test]
fn contradiction_check2_states_clean_outcome_explicitly() {
    let mut entries = agreeing_population_entries(100);
    entries.push(entry_with_attribution(
        "council-registers-political-actors",
        "council_registers",
        7,
    ));
    let st = strategy_with_runs(&["council_registers"]);
    let section = check_block(&contradictions_section(&entries, &st), "**Check 2**");

    assert!(
        section.contains(NO_CONTRADICTION),
        "check 2 must state a clean outcome explicitly; section:\n{section}"
    );
    assert!(
        !section.contains(CONTRADICTION),
        "check 2 fired although the attributed source carries a marker; section:\n{section}"
    );
}

/// No run markers read at all is "could not evaluate", never a clean result.
#[test]
fn contradiction_check2_absent_markers_is_not_a_clean_result() {
    let mut entries = agreeing_population_entries(100);
    entries.push(entry_with_attribution(
        "council-registers-political-actors",
        "council_registers",
        7,
    ));
    let st = Strategy::default(); // enrichment_runs = None
    let section = check_block(&contradictions_section(&entries, &st), "**Check 2**");

    assert!(section.contains(COULD_NOT_EVALUATE));
    assert!(
        !section.contains(NO_CONTRADICTION),
        "absent markers must not be reported as a clean result; section:\n{section}"
    );
}

/// A zero-valued attribution is not a contradiction — it attributes nothing, so there is nothing
/// for a missing marker to contradict.
#[test]
fn contradiction_check2_zero_valued_attribution_does_not_fire() {
    let mut entries = agreeing_population_entries(100);
    entries.push(entry_with_attribution(
        "council-registers-political-actors",
        "council_registers",
        0,
    ));
    let st = strategy_with_runs(&["ch_psc"]);
    let section = check_block(&contradictions_section(&entries, &st), "**Check 2**");

    assert!(
        !section.contains(CONTRADICTION),
        "a zero-valued attribution must not fire check 2; section:\n{section}"
    );
}

// ── Both checks always render, in a real battery ─────────────────────────────────────────────

/// Against a real 22-probe battery run, the Contradictions section exists and BOTH checks emit
/// exactly one of the three distinguishable outcomes — neither check is ever silently omitted.
#[tokio::test]
async fn both_checks_always_render_a_distinguishable_outcome() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: both_checks_always_render_a_distinguishable_outcome (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_evidence_report_contra");
    let cfg = test_config(ns.name());
    let db = db::connect_db(&cfg).await.expect("connect_db failed");
    seed_small_fixture(&db).await;
    let catalog = registry::connect_catalog(&cfg)
        .await
        .expect("connect_catalog failed");
    let db_name = db::resolve_analysis_db(&cfg)
        .await
        .expect("resolve_analysis_db failed");

    let probes = full_battery(catalog, db_name, &cfg);
    let mut st = Strategy::default();
    let findings = run_probes(&probes, &db, &mut st).await;
    let entries = build_entries(&probes, findings);
    let rendered = report::render_evidence_report(&entries, &st);

    assert!(
        rendered.contains("## Contradiction Checks"),
        "the assembled document has no Contradiction Checks section:\n{rendered}"
    );

    for check in ["**Check 1**", "**Check 2**"] {
        assert!(
            rendered.contains(check),
            "{check} is absent from the assembled document — a contradiction check was silently \
             omitted, which D-15 forbids"
        );
    }

    // Check 1 states its outcome once per sub-check, not once on its header line — each
    // sub-comparison is evaluated independently, so each has its own outcome to state.
    let section = report::contradictions::render(&entries, &st);
    for (check, prefix) in [
        ("Check 1 sub-check 1a", SUB_1A),
        ("Check 1 sub-check 1b", SUB_1B),
        ("Check 2", "**Check 2**"),
    ] {
        let text = sub_check(&section, prefix);
        let outcomes = [CONTRADICTION, NO_CONTRADICTION, COULD_NOT_EVALUATE]
            .iter()
            .filter(|o| text.contains(*o))
            .count();
        assert_eq!(
            outcomes, 1,
            "{check} must state exactly one of contradiction / clean / could-not-evaluate, \
             stated {outcomes}: {text}"
        );
    }
}

// ── Post-phase fix: sub-check 1a's two populations are the same population ───────────────────

/// `ward-concentration` and `temporal-burst` report the SAME population against a fixture that
/// contains a Contract with no usable timestamp — the exact divergence 1a used to fire on.
///
/// 1a's stated grounds are that both probes "scan the identical unfiltered Contract population
/// independently and must agree". That was not true: `ward_concentration` counted every Contract,
/// while `temporal_burst` reported only the rows it could bucket by ingestion day. One
/// timestamp-less Contract was enough to make 1a report a CONTRADICTION for an expected
/// divergence — the cry-wolf failure the check's whole design exists to avoid. `temporal_burst`
/// now reports that row as population-but-not-capable, so the two denominators match and the
/// exclusion is stated instead of hidden.
#[tokio::test]
async fn ward_and_temporal_populations_agree_despite_an_untimestamped_contract() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: ward_and_temporal_populations_agree_despite_an_untimestamped_contract (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_contradictions_1a_align");
    let cfg = test_config(ns.name());
    let db = db::connect_db(&cfg).await.expect("connect_db failed");

    let contract = |name: &str| {
        NodeWeight::Contract(ContractData {
            name: name.to_string(),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: Some("E05001001".to_string()),
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        })
    };
    for i in 0..3 {
        seed_node(
            &db,
            &format!("planning:ALIGN-{i}"),
            &contract("Timestamped"),
        )
        .await;
    }
    // The odd one out: same shape, but `timestamp` is null, so no ingestion day can be derived.
    let weight_json = serde_json::to_string(&contract("Untimestamped")).unwrap();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": "NodeAdded-planning:ALIGN-NOTS",
                "timestamp": serde_json::Value::Null,
                "kind": "NodeAdded",
                "payload": { "entity_id": "planning:ALIGN-NOTS", "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed untimestamped contract")
        .check()
        .expect("seed untimestamped contract returned a statement error");
    // `ward-concentration` reports DataGap without at least one Represents edge, which would make
    // 1a unevaluable and the assertion below vacuous.
    seed_edge(
        &db,
        "represents",
        "ward:E05001001",
        "planning:ALIGN-0",
        &EdgeWeight::Represents {
            meta: edge_meta(),
            data: RepresentsData {
                details: String::new(),
            },
        },
    )
    .await;

    let probes: Vec<Box<dyn Probe>> = vec![Box::new(WardConcentration), Box::new(TemporalBurst)];
    let mut st = Strategy::default();
    let findings = run_probes(&probes, &db, &mut st).await;
    let entries = build_entries(&probes, findings);

    let ward = entries[0].finding.population();
    let temporal = entries[1].finding.population();
    assert_eq!(
        (ward, temporal),
        (Some(4), Some(4)),
        "both probes must report the same 4-Contract population; the timestamp-less row is \
         excluded from temporal-burst's CAPABLE count, not from its population"
    );

    let section = check_block(&contradictions_section(&entries, &st), "**Check 1**");
    let a = sub_check(&section, SUB_1A);
    assert!(
        a.contains(NO_CONTRADICTION),
        "1a fired on an expected divergence — the cry-wolf case; 1a:\n{a}"
    );
}
