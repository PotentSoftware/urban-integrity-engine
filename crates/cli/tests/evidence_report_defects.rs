//! Wave 5, plan 35-16 — e2e proof that the five presentation defects catalogued by the withdrawn
//! Phase 27 do not reproduce in the Rust port. Covers ROADMAP criterion 2's (a), (b), (c) and (e)
//! sub-checks; defect (d) (mechanical pluralisation) is closed by plan 35-14's static scan
//! (`probe_report_statics.rs`), per the validation notes' row split, and is deliberately not re-tested
//! here.
//!
//! # Shared scaffolding block
//!
//! `test_config`/`full_battery`/`build_entries` below are this file's own copy of the block
//! documented at the top of `evidence_report_structure.rs` (plan 35-15) — the deliberate
//! per-test-binary duplication convention this phase adopted, because integration test binaries
//! under `tests/` compile independently and cannot share private items without a `tests/common/`
//! module. It is copied from what that file ACTUALLY does, including its recorded deviation:
//! namespaces come from `NsGuard::with_prefix` (one reserved name threaded through `test_config`),
//! never two `TestNs::connect_with_prefix` calls, which would put the `_catalog` connection in a
//! different namespace from the analysis DB and silently break `IdentityIntegrity`'s
//! same-namespace invariant (T-32-01).
//!
//! # Two scoping decisions, both recorded in the plan 35-16 summary
//!
//! **Defect (a).** The plan's Task 1 specifies seeding a 90+ character entity name and asserting
//! the full name appears in the rendered document. Seeding it is necessary but not sufficient: no
//! probe in this fixture surfaces a raw entity name into the report, so that assertion would fail
//! for ABSENCE rather than truncation, testing nothing about the defect. `defect_a` therefore
//! asserts on the strings the document is guaranteed to contain in full — every rendered
//! `Finding::summary` — and additionally asserts at least one of them exceeds 90 characters, so it
//! cannot pass vacuously against defect (a)'s original `summary[:90]` slicing mechanism. The long
//! entity name is still seeded and still asserted, but only where it demonstrably reaches the
//! document.
//!
//! **Defect (c).** The plan permits narrowing this check to table-cell context if any probe's own
//! prose legitimately contains the English word "None". It does:
//! `disqualified_active_in_procurement.rs` renders "None cross-referenced to active contract
//! awards yet." as a sentence. A whole-document check would fail on correct output, so `defect_c`
//! is scoped to the Full Probe Ledger's own cells — which is exactly where defect (c) lived
//! (`Option<u64>` population/capable rendering as the literal `None`).

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
use urban_integrity_schema::types::verdict::Finding;
use urban_integrity_schema::types::verdict::PopulationSource;
use urban_integrity_schema::types::verdict::Verdict;

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
        "GB-COH:E2E16001",
        &NodeWeight::Entity(EntityData {
            name: LONG_ENTITY_NAME.to_string(),
            company_number: Some("E2E16001".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    )
    .await;
    seed_node(
        db,
        "director:e2e-35-16-director",
        &NodeWeight::Officer(OfficerData {
            name: "Plan 35-16 Test Director".to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: Some("E2E16001".to_string()),
        }),
    )
    .await;
    seed_node(
        db,
        "location:e2e-35-16-loc",
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
        "planning:E2E16-C1",
        &NodeWeight::Contract(ContractData {
            name: "Plan 35-16 Test Planning Application".to_string(),
            ocid: None,
            value_gbp: None,
            status: Some("approved".to_string()),
            date_published: Some("2026-01-01".to_string()),
            ward_code: Some("E05001001".to_string()),
            tender_ref: None,
            application_ref: Some("E2E/2026/0016".to_string()),
            application_received_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            applicant_provenance: Some(ApplicantProvenance::Published),
            portal_detail_handle: None,
        }),
    )
    .await;

    seed_edge(
        db,
        "directs",
        "director:e2e-35-16-director",
        "GB-COH:E2E16001",
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
        "GB-COH:E2E16001",
        "planning:E2E16-C1",
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
        "planning:E2E16-C1",
        "location:e2e-35-16-loc",
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
        "planning:E2E16-C1",
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
        let sni = format!("entity:e2e-35-16-src-{i}");
        let tni = format!("entity:e2e-35-16-dst-{i}");
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

// ── Defect (a): no mid-word truncation ───────────────────────────────────────────────────────

/// The entity name seeded by `seed_small_fixture`, deliberately over 90 characters so that
/// defect (a)'s original `summary[:90]` slicing mechanism would have visibly cut it.
const LONG_ENTITY_NAME: &str =
    "Plan 35-16 Deliberately Very Long Test Entity Name For Truncation Detection Holdings Limited";

/// Returns the slice of `document` from `heading`'s line (inclusive) to the next `"\n## "`
/// heading, or to the end. Panics if the heading is absent.
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

/// Renders the full battery against an isolated, freshly seeded namespace and returns the
/// rendered document alongside the entries that produced it.
async fn render_seeded_report(prefix: &str) -> (String, Vec<report::ReportEntry>, Strategy) {
    let ns = urban_integrity_test_support::NsGuard::with_prefix(prefix);
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
    (rendered, entries, st)
}

/// Every summary the document renders appears in FULL — no cell or line truncates mid-word.
///
/// Only the four verdict groups `render_evidence_report` actually emits summaries for are checked
/// (`Hit`, `Weak`, `DataGap`/`Blocked`, `NotApplicable`). `Clear` findings are deliberately
/// excluded: the assembler has no `Clear` detail group, so a `Clear` summary is not rendered
/// anywhere and asserting on it would test absence, not truncation. That omission is reported as
/// an out-of-scope observation in the plan 35-16 summary, not silently absorbed here.
#[tokio::test]
async fn defect_a_no_mid_word_truncation() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: defect_a_no_mid_word_truncation (set URBAN_DB_TESTS=1)");
        return;
    }

    let (rendered, entries, _st) = render_seeded_report("test_evidence_report_defect_a").await;

    let mut checked_lengths: Vec<usize> = Vec::new();
    for entry in &entries {
        let renders_summary = matches!(
            entry.finding.verdict,
            Verdict::Hit { .. }
                | Verdict::Weak { .. }
                | Verdict::DataGap { .. }
                | Verdict::Blocked { .. }
                | Verdict::NotApplicable { .. }
        );
        if !renders_summary {
            continue;
        }
        assert!(
            rendered.contains(&entry.finding.summary),
            "probe {}'s summary does not appear in full in the rendered document — it was \
             truncated or altered.\nexpected verbatim: {:?}\ndocument:\n{rendered}",
            entry.finding.probe,
            entry.finding.summary
        );
        checked_lengths.push(entry.finding.summary.chars().count());
    }

    assert!(
        !checked_lengths.is_empty(),
        "no summary-rendering finding was produced — this test would pass vacuously"
    );
    let longest = checked_lengths.iter().copied().max().unwrap_or(0);
    assert!(
        longest > 90,
        "the longest summary asserted was {longest} characters; defect (a)'s original mechanism \
         was a summary[:90] slice, so a corpus whose summaries are all under 90 characters cannot \
         detect its return"
    );
}

/// The seeded 90+ character entity name survives into the document intact wherever it appears at
/// all. Scoped deliberately: if no probe surfaces it, there is nothing to truncate, and the test
/// says so rather than failing on absence.
#[tokio::test]
async fn defect_a_long_entity_name_is_never_partially_rendered() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!(
            "SKIP: defect_a_long_entity_name_is_never_partially_rendered (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let (rendered, _entries, _st) =
        render_seeded_report("test_evidence_report_defect_a_name").await;

    assert!(LONG_ENTITY_NAME.chars().count() > 90);
    // A partial render is detectable without the full name being present: any prefix of the name
    // longer than 40 characters that is NOT followed by the rest of the name is a truncation.
    let probe_prefix: String = LONG_ENTITY_NAME.chars().take(40).collect();
    if let Some(pos) = rendered.find(&probe_prefix) {
        assert!(
            rendered[pos..].starts_with(LONG_ENTITY_NAME),
            "the seeded entity name appears in the document but is cut short at byte {pos} — \
             mid-word truncation (defect a) has returned.\nfound: {:?}",
            &rendered[pos..(pos + 200).min(rendered.len())]
        );
    }
}

// ── Defect (b): no step-numbering gap ────────────────────────────────────────────────────────

/// Extracts the leading integers of every `^\d+\.` line inside "How the Search Adapted".
fn adapted_step_numbers(document: &str) -> Vec<u32> {
    extract_section(document, "## How the Search Adapted")
        .lines()
        .filter_map(|l| {
            let digits: String = l.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            if !l[digits.len()..].starts_with('.') {
                return None;
            }
            digits.parse::<u32>().ok()
        })
        .collect()
}

fn assert_contiguous(numbers: &[u32], label: &str, document: &str) {
    assert!(
        !numbers.is_empty(),
        "{label}: 'How the Search Adapted' rendered no numbered steps at all:\n{}",
        extract_section(document, "## How the Search Adapted")
    );
    let expected: Vec<u32> = (1..=numbers.len() as u32).collect();
    assert_eq!(
        numbers,
        expected.as_slice(),
        "{label}: step numbering is not a contiguous 1..n sequence — defect (b) has returned. \
         section:\n{}",
        extract_section(document, "## How the Search Adapted")
    );
}

/// The numbering is contiguous under two fixture shapes that fire DIFFERENT subsets of the
/// conditional steps — an empty namespace (no OSINT run, no identity linkage) and a seeded one
/// (OSINT active, confirmed links present). Defect (b) was a gap appearing only when a
/// conditional step was skipped, so a single fixture cannot detect it.
#[tokio::test]
async fn defect_b_no_step_numbering_gap() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: defect_b_no_step_numbering_gap (set URBAN_DB_TESTS=1)");
        return;
    }

    // Shape 1 — empty namespace: no identity_links, so `osint_active` stays false and the
    // confirmed/pending step does not fire.
    let ns_empty =
        urban_integrity_test_support::NsGuard::with_prefix("test_evidence_report_defect_b_empty");
    let cfg_empty = test_config(ns_empty.name());
    let db_empty = db::connect_db(&cfg_empty).await.expect("connect_db failed");
    let catalog_empty = registry::connect_catalog(&cfg_empty)
        .await
        .expect("connect_catalog failed");
    let db_name_empty = db::resolve_analysis_db(&cfg_empty)
        .await
        .expect("resolve_analysis_db failed");
    let probes_empty = full_battery(catalog_empty, db_name_empty, &cfg_empty);
    let mut st_empty = Strategy::default();
    let findings_empty = run_probes(&probes_empty, &db_empty, &mut st_empty).await;
    let entries_empty = build_entries(&probes_empty, findings_empty);
    let rendered_empty = report::render_evidence_report(&entries_empty, &st_empty);

    // Shape 2 — seeded namespace: confirmed identity_links, so `osint_active` becomes true and
    // the confirmed/pending step DOES fire.
    let (rendered_seeded, _entries, st_seeded) =
        render_seeded_report("test_evidence_report_defect_b_seeded").await;

    let empty_numbers = adapted_step_numbers(&rendered_empty);
    let seeded_numbers = adapted_step_numbers(&rendered_seeded);

    assert_contiguous(&empty_numbers, "empty fixture", &rendered_empty);
    assert_contiguous(&seeded_numbers, "seeded fixture", &rendered_seeded);

    // Prove the two shapes genuinely differ, so this is not two runs of the same path.
    assert!(
        st_seeded.osint_active,
        "the seeded fixture was expected to set Strategy.osint_active = true; if it does not, \
         both fixtures exercise the same conditional-step subset and defect (b) is untested"
    );
    assert!(
        !st_empty.osint_active,
        "the empty fixture was expected to leave Strategy.osint_active = false"
    );
}

// ── Defect (c): no raw placeholder leakage ───────────────────────────────────────────────────

/// Splits a Markdown table row into its trimmed cells.
fn table_cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(|c| c.trim())
        .collect()
}

/// No Full Probe Ledger cell renders the literal `None`. Verdicts carrying no `Coverage`
/// (`DataGap`/`Blocked`) must render `-` in the Population/Capable columns.
///
/// Scoped to the ledger's own cells rather than the whole document — see this file's module doc:
/// `disqualified_active_in_procurement` legitimately renders the English word "None" at the start
/// of a sentence, so a document-wide check fails against correct output.
#[tokio::test]
async fn defect_c_no_raw_placeholder_leakage() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: defect_c_no_raw_placeholder_leakage (set URBAN_DB_TESTS=1)");
        return;
    }

    let (rendered, entries, _st) = render_seeded_report("test_evidence_report_defect_c").await;

    // The fixture must actually exercise both shapes, or the test passes vacuously.
    let none_carrying = entries
        .iter()
        .filter(|e| e.population().is_none() || e.capable().is_none())
        .count();
    let some_carrying = entries
        .iter()
        .filter(|e| e.population().is_some() && e.capable().is_some())
        .count();
    assert!(
        none_carrying > 0,
        "fixture produced no Coverage-less (DataGap/Blocked) finding — defect (c) would be \
         untested, since it only manifests when population/capable are None"
    );
    assert!(
        some_carrying > 0,
        "fixture produced no Coverage-carrying finding — the '-' placeholder would be \
         indistinguishable from a report with no numeric cells at all"
    );

    let ledger = extract_section(&rendered, "## Full Probe Ledger");
    for line in ledger.lines().filter(|l| l.starts_with('|')) {
        for cell in table_cells(line) {
            assert_ne!(
                cell, "None",
                "a Full Probe Ledger cell rendered the literal token `None` instead of `-` — \
                 defect (c) has returned. row: {line}"
            );
        }
    }

    // The `-` placeholder must actually be present, proving the None path was rendered.
    assert!(
        ledger.contains("| - |"),
        "no `-` placeholder cell found in the ledger although {none_carrying} Coverage-less \
         findings exist; ledger:\n{ledger}"
    );
}

// ── Defect (e): every emoji has a legend entry ───────────────────────────────────────────────

/// The fixed six-variant verdict emoji set (D-11).
const VERDICT_EMOJI: [&str; 6] = ["🔴", "🟡", "🟢", "⚪", "❓", "🚫"];

/// Every verdict emoji used anywhere outside the legend has a legend entry, AND the legend
/// carries no entry for an emoji outside the fixed six — closing defect (e) in both directions.
#[tokio::test]
async fn defect_e_every_emoji_has_a_legend_entry() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: defect_e_every_emoji_has_a_legend_entry (set URBAN_DB_TESTS=1)");
        return;
    }

    let (rendered, _entries, _st) = render_seeded_report("test_evidence_report_defect_e").await;

    let legend = extract_section(&rendered, "## Verdict Legend");
    let outside_legend = rendered.replace(legend, "");

    // Direction 1: every emoji the document uses is keyed by the legend.
    for emoji in VERDICT_EMOJI {
        if outside_legend.contains(emoji) {
            assert!(
                legend.contains(emoji),
                "emoji {emoji} appears in the document but has no Verdict Legend entry — \
                 defect (e) has returned. legend:\n{legend}"
            );
        }
    }

    // Direction 2: the legend keys nothing outside the fixed six.
    let legend_emoji: Vec<&str> = VERDICT_EMOJI
        .iter()
        .copied()
        .filter(|e| legend.contains(e))
        .collect();
    assert_eq!(
        legend_emoji.len(),
        6,
        "the Verdict Legend does not carry all six verdict emoji; found {legend_emoji:?}"
    );
    for ch in legend.chars() {
        let is_pictographic = matches!(ch as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF);
        if is_pictographic {
            let s = ch.to_string();
            assert!(
                VERDICT_EMOJI.contains(&s.as_str()),
                "the Verdict Legend contains emoji {s:?}, which is not one of the six verdict \
                 variants — a legend entry for something nothing renders. legend:\n{legend}"
            );
        }
    }
}
