//! Wave 5, plan 35-15 — e2e proof that the finished report writer (35-13's wired `run.rs` +
//! Wave 3's real section bodies) satisfies ROADMAP success criteria 1 ("deterministic across
//! reruns"), 3 ("verdict legend enumerates all six variants") and 4 ("Full Probe Ledger has
//! exactly 22 rows").
//!
//! # Shared scaffolding block — this is the shape plans 35-16/35-17/35-18 duplicate
//!
//! `test_config`/`full_battery`/`build_entries` below are THIS file's own copy of the pattern
//! established by `probe_tier1_e2e.rs`/`probe_tier2_e2e.rs`: `execute()` hardcodes
//! `AppConfig::load()` and cannot be pointed at an isolated test namespace (the research notes' Pitfall
//! 2), so every DB-gated e2e test file in this crate builds an `AppConfig` directly and drives
//! the real production `db::connect_db`/`registry::connect_catalog`/`db::resolve_analysis_db`
//! functions with it, then reconstructs the ordered probe battery and `run.rs::execute`'s
//! zip-into-`ReportEntry` logic locally. This is a DELIBERATE per-test-binary duplication
//! (documented, not accidental) — plans 35-16 through 35-18 each open their own tasks by reading
//! this exact file and copying this block rather than inventing their own variant or importing
//! from here (integration test binaries under `tests/` compile independently and cannot share
//! private items across files without a `tests/common/` module, which this convention
//! deliberately avoids so each e2e file stays self-contained and independently readable).
//!
//! Known limitation inherited unchanged from the plan 33-09 summary: because this hand-copied
//! `full_battery` cannot literally BE `run.rs::execute`'s own probe list, a future registration-
//! order edit to production `run.rs` is unguarded by this file's own fixtures unless it also
//! happens to break `probe_tier2_e2e.rs`'s `full_battery_matches_run_rs_registration_order` test
//! (unaffected by this phase). This plan does not attempt to close that gap.
//!
//! # Deviation from this plan's literal text — recorded prominently per this plan's own
//! instruction, because 35-16/17/18 will copy whatever is actually here
//!
//! Plan 35-15's Task 1 action says to use `TestNs::connect_with_prefix` "for BOTH the analysis
//! DB and the `_catalog` DB". This file does NOT do that. `TestNs::connect_with_prefix` creates a
//! brand-new namespace (a fresh `NsGuard`) on every call, and `IdentityIntegrity` requires its
//! `catalog` connection and its analysis-DB connection to share the SAME namespace — the
//! `_catalog.analyses` row `IdentityIntegrity` reads is looked up by `db_name`, and
//! `registry::connect_catalog`/`db::resolve_analysis_db` both resolve off `cfg.surreal.namespace`
//! (see `identity_integrity.rs`'s own module doc comment on T-32-01: "no second, independently-
//! read config value that could target the wrong analysis's baseline row"). Calling
//! `TestNs::connect_with_prefix` twice would hand `IdentityIntegrity` a catalog connection in a
//! DIFFERENT namespace from the analysis DB it is meant to describe, silently breaking that
//! invariant. Instead, this file follows `probe_tier1_e2e.rs`/`probe_tier2_e2e.rs`'s own already-
//! passing precedent — and `test-support`'s own documented convention (`crates/test-support/src/
//! lib.rs`: "`NsGuard` ... name only, for tests driving the DB through ... a raw HTTP config",
//! which is exactly this file's shape) — of reserving ONE namespace name via
//! `urban_integrity_test_support::NsGuard::with_prefix`, threading that single name through
//! `test_config`, and letting the real production `db::connect_db`/`registry::connect_catalog`
//! functions derive both connections from the same `AppConfig.surreal.namespace` value. `NsGuard`
//! is still sourced from `urban-integrity-test-support`, per this codebase's DB-gated-test
//! namespace-hygiene rule — only the specific constructor differs from the plan's literal text.

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
use urban_integrity_probe::{run_probes, Probe, Strategy};
use urban_integrity_schema::types::edge::{
    AwardedToData, DirectsData, EdgeMeta, EdgeWeight, LocatedAtData, RepresentsData,
};
use urban_integrity_schema::types::node::{
    ApplicantProvenance, ContractData, EntityData, LocationData, NodeWeight, OfficerData,
};
use urban_integrity_schema::types::verdict::PopulationSource;
use urban_integrity_schema::types::verdict::{Finding, Verdict};

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
    urban_integrity_test_support::events::seed_node_event(
        db,
        &format!("NodeAdded-{entity_id}"),
        entity_id,
        weight,
    )
    .await
    .expect("failed to seed node event");
}

async fn seed_edge(
    db: &Surreal<Any>,
    label: &str,
    src_id: &str,
    dst_id: &str,
    weight: &EdgeWeight,
) {
    urban_integrity_test_support::events::seed_edge_event(
        db,
        &format!("EdgeAdded-{label}-{src_id}-{dst_id}"),
        src_id,
        dst_id,
        weight,
    )
    .await
    .expect("failed to seed edge event");
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
        "GB-COH:E2E15001",
        &NodeWeight::Entity(EntityData {
            name: "Plan 35-15 Test Entity Ltd".to_string(),
            company_number: Some("E2E15001".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    )
    .await;
    seed_node(
        db,
        "director:e2e-35-15-director",
        &NodeWeight::Officer(OfficerData {
            name: "Plan 35-15 Test Director".to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: Some("E2E15001".to_string()),
        }),
    )
    .await;
    seed_node(
        db,
        "location:e2e-35-15-loc",
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
        "planning:E2E15-C1",
        &NodeWeight::Contract(ContractData {
            name: "Plan 35-15 Test Planning Application".to_string(),
            ocid: None,
            value_gbp: None,
            status: Some("approved".to_string()),
            date_published: Some("2026-01-01".to_string()),
            ward_code: Some("E05001001".to_string()),
            tender_ref: None,
            application_ref: Some("E2E/2026/0015".to_string()),
            application_received_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            applicant_provenance: Some(ApplicantProvenance::Published),
            portal_detail_handle: None,
        }),
    )
    .await;

    seed_edge(
        db,
        "directs",
        "director:e2e-35-15-director",
        "GB-COH:E2E15001",
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
        "GB-COH:E2E15001",
        "planning:E2E15-C1",
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
        "planning:E2E15-C1",
        "location:e2e-35-15-loc",
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
        "planning:E2E15-C1",
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
        let sni = format!("entity:e2e-35-15-src-{i}");
        let tni = format!("entity:e2e-35-15-dst-{i}");
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

// ── Report-structure helpers, local to this file ────────────────────────────────────────────

/// Returns the slice of `document` from `heading`'s own line (inclusive) up to, but not
/// including, the next `"\n## "` heading — or to the end of the string if `heading` is the last
/// section. Panics if `heading` is absent (a missing section is this test's own bug, not a
/// tolerable skip).
fn extract_section<'a>(document: &'a str, heading: &str) -> &'a str {
    let start = document
        .find(heading)
        .unwrap_or_else(|| panic!("heading {heading:?} not found in document:\n{document}"));
    let after_heading = &document[start..];
    let heading_line_end = after_heading
        .find('\n')
        .map(|i| i + 1)
        .unwrap_or(after_heading.len());
    let rest = &after_heading[heading_line_end..];
    match rest.find("\n## ") {
        Some(offset) => &after_heading[..heading_line_end + offset],
        None => after_heading,
    }
}

/// Counts the Full Probe Ledger's content rows WITHIN the ledger section only (never the whole
/// document, which also contains the Verdict Legend's own `| ... |` table) — excludes the header
/// row (`| Tier | Probe | ... |`) and the `|---|` separator row.
fn ledger_content_row_count(document: &str) -> usize {
    extract_section(document, "## Full Probe Ledger")
        .lines()
        .filter(|l| l.starts_with('|'))
        .filter(|l| !l.contains("| Tier | Probe |"))
        .filter(|l| !l.trim_start_matches('|').starts_with('-'))
        .count()
}

/// Strips every line starting with the literal `"Generated: "` prefix — the one line
/// `render_evidence_report` documents as timestamp-varying — before a byte-identical comparison.
fn strip_generated_line(rendered: &str) -> String {
    rendered
        .lines()
        .filter(|l| !l.starts_with("Generated: "))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Task 1 (criterion 4, PRB-01 closure) ─────────────────────────────────────────────────────

/// The full 22-probe battery, run through the real report writer, produces a Full Probe Ledger
/// with EXACTLY 22 content rows — one per ported probe, no more, no fewer.
#[tokio::test]
async fn ledger_row_count_is_22() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: ledger_row_count_is_22 (set URBAN_DB_TESTS=1)");
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_evidence_report_ledger");
    let cfg = test_config(ns.name());
    let db = db::connect_db(&cfg).await.expect("connect_db failed");
    let catalog = registry::connect_catalog(&cfg)
        .await
        .expect("connect_catalog failed");
    let db_name = db::resolve_analysis_db(&cfg)
        .await
        .expect("resolve_analysis_db failed");

    let probes = full_battery(catalog, db_name, &cfg);
    let mut st = Strategy::default();
    let findings = run_probes(&probes, &db, &mut st).await;
    assert_eq!(
        findings.len(),
        22,
        "expected 22 findings from the full battery, got {}",
        findings.len()
    );

    let entries = build_entries(&probes, findings);
    let rendered = report::render_evidence_report(&entries, &st);

    let count = ledger_content_row_count(&rendered);
    assert_eq!(
        count,
        22,
        "expected exactly 22 Full Probe Ledger content rows, got {count}; ledger section:\n{}",
        extract_section(&rendered, "## Full Probe Ledger")
    );
}

// ── Task 2 (criterion 3, no DB needed) ───────────────────────────────────────────────────────

/// `report::legend::render()`'s output, read from the crate's public surface the way the
/// assembled `evidence-report.md` document actually uses it, enumerates all 6 `Verdict` variant
/// names.
#[test]
fn legend_has_all_six_variants() {
    let rendered = report::legend::render();
    for label in [
        "Hit",
        "Weak",
        "Clear",
        "NotApplicable",
        "DataGap",
        "Blocked",
    ] {
        assert!(
            rendered.contains(label),
            "legend is missing variant label {label}; rendered:\n{rendered}"
        );
    }
}

// ── Task 3 (criterion 1, GRD-07) ─────────────────────────────────────────────────────────────

/// Running the full battery TWICE in immediate succession against the SAME frozen, seeded
/// namespace (no writes between the two runs) produces byte-identical rendered report output,
/// modulo the `"Generated: "` timestamp line.
#[tokio::test]
async fn twice_run_diff_is_byte_identical_modulo_timestamp() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: twice_run_diff_is_byte_identical_modulo_timestamp (set URBAN_DB_TESTS=1)");
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_evidence_report_twice");
    let cfg = test_config(ns.name());
    let db = db::connect_db(&cfg).await.expect("connect_db failed");
    seed_small_fixture(&db).await;

    // Run 1.
    let catalog1 = registry::connect_catalog(&cfg)
        .await
        .expect("connect_catalog failed (run 1)");
    let db_name1 = db::resolve_analysis_db(&cfg)
        .await
        .expect("resolve_analysis_db failed (run 1)");
    let probes1 = full_battery(catalog1, db_name1, &cfg);
    let mut st1 = Strategy::default();
    let findings1 = run_probes(&probes1, &db, &mut st1).await;
    let entries1 = build_entries(&probes1, findings1);
    let rendered1 = report::render_evidence_report(&entries1, &st1);

    // Run 2 — same frozen namespace, no writes between the two runs.
    let catalog2 = registry::connect_catalog(&cfg)
        .await
        .expect("connect_catalog failed (run 2)");
    let db_name2 = db::resolve_analysis_db(&cfg)
        .await
        .expect("resolve_analysis_db failed (run 2)");
    let probes2 = full_battery(catalog2, db_name2, &cfg);
    let mut st2 = Strategy::default();
    let findings2 = run_probes(&probes2, &db, &mut st2).await;
    let entries2 = build_entries(&probes2, findings2);
    let rendered2 = report::render_evidence_report(&entries2, &st2);

    let stripped1 = strip_generated_line(&rendered1);
    let stripped2 = strip_generated_line(&rendered2);

    if stripped1 != stripped2 {
        let lines1: Vec<&str> = stripped1.lines().collect();
        let lines2: Vec<&str> = stripped2.lines().collect();
        let mut first_diff: Option<(usize, &str, &str)> = None;
        for (i, (a, b)) in lines1.iter().zip(lines2.iter()).enumerate() {
            if a != b {
                first_diff = Some((i, a, b));
                break;
            }
        }
        let diff_msg = match first_diff {
            Some((i, a, b)) => {
                format!("first differing line index {i}:\n  run 1: {a:?}\n  run 2: {b:?}")
            }
            None => format!(
                "line counts differ: run 1 has {} lines, run 2 has {} lines",
                lines1.len(),
                lines2.len()
            ),
        };
        panic!(
            "two immediate-succession battery runs against the same frozen namespace produced \
             different report output (modulo the `Generated: ` line):\n{diff_msg}"
        );
    }
}

// ── Post-phase fixes: findings 3 and 4 ───────────────────────────────────────────────────────

/// Every finding the document renders leads with the hypothesis of the probe that produced it.
///
/// `Probe::hypothesis()` was carried from the trait onto `ReportEntry` (D-01) precisely so the
/// report could state what each probe was testing, and was then rendered nowhere: a reader met a
/// bare summary paragraph with no way to tell which probe produced it or what question it
/// answers. The examined count pins non-vacuity — a document that silently drops a whole verdict
/// group would otherwise pass this by having fewer entries to check.
#[tokio::test]
async fn every_rendered_finding_states_its_probes_hypothesis() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: every_rendered_finding_states_its_probes_hypothesis (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_evidence_report_hypothesis");
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

    let mut checked = 0usize;
    for entry in &entries {
        let line = format!("> **Hypothesis:** {}", entry.hypothesis);
        assert!(
            rendered.contains(&line),
            "probe {}'s hypothesis is absent from the assembled document — it reaches the \
             report writer and is rendered nowhere; expected line:\n{line}",
            entry.finding.probe
        );
        assert!(
            rendered.contains(&format!("### {}", entry.finding.probe)),
            "probe {}'s detail block carries no heading naming it, so its summary cannot be \
             attributed to it",
            entry.finding.probe
        );
        checked += 1;
    }
    assert_eq!(
        checked, 22,
        "expected all 22 findings to be examined, examined {checked}"
    );
}

/// A `Clear` finding's own prose reaches the document, not just its ledger row.
///
/// `Clear` was the one verdict with no detail group: a clean result survived as a ledger row
/// carrying the verdict and the counts, while the summary explaining what was measured to reach
/// it was dropped. That is the reasoning distinguishing "checked and found nothing" from "could
/// not check" — the distinction D-07's BLUF ordering exists to protect.
#[tokio::test]
async fn clear_findings_render_their_summary_not_only_a_ledger_row() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: clear_findings_render_their_summary_not_only_a_ledger_row (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_evidence_report_clear");
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

    let clear: Vec<&report::ReportEntry> = entries
        .iter()
        .filter(|e| matches!(e.finding.verdict, Verdict::Clear(_)))
        .collect();
    assert!(
        !clear.is_empty(),
        "this fixture produced no Clear finding, so the test proves nothing — reseed it before \
         trusting a pass"
    );

    let section = extract_section(&rendered, "## Verified Negative Findings");
    for entry in &clear {
        assert!(
            section.contains(&entry.finding.summary),
            "Clear finding {}'s summary is absent from the Verified Negative Findings section — \
             its reasoning survives only as a ledger row; section:\n{section}",
            entry.finding.probe
        );
    }
}
