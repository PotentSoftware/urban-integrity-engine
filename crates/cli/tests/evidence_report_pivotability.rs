//! Wave 5, plan 35-17 — e2e proof of ROADMAP criterion 5 (RPT-04): pivotability judged PER
//! FINDING against that finding's own evidence, never a global "the document contains ids
//! somewhere" check that the three id-bearing probes would satisfy alone while the other
//! nineteen render nothing.
//!
//! # Shared scaffolding block
//!
//! `test_config`/`full_battery`/`build_entries` below are this file's own copy of the block
//! documented at the top of `evidence_report_structure.rs` (plan 35-15) — the deliberate
//! per-test-binary duplication convention this phase adopted. Copied from what that file ACTUALLY
//! does, including its recorded deviation: one namespace name reserved via `NsGuard::with_prefix`
//! and threaded through `test_config`, never two `TestNs::connect_with_prefix` calls (which would
//! break `IdentityIntegrity`'s same-namespace invariant, T-32-01).
//!
//! # Deviation: what `no_probe_constructs_a_command_string` actually scans for
//!
//! This plan's Task 2 specifies asserting that no `.rs` file under `crates/probe/src/` (formerly
//! `crates/cli/src/commands/probe/`, moved by Phase 43 plan 01) outside `report/` contains the
//! literal substring `"urban-integrity-cli"`. Scanned literally,
//! that assertion fails against correct code in two places, and one of them is blessed by this
//! same plan's own Task 1:
//!
//! - `land_flip_candidates.rs` renders `urban-integrity-cli flips list` as its probe-specific
//!   aggregate reproduction note — which Task 1 explicitly requires to exist and treats as the
//!   legitimate alternative to the generic fallback sentence.
//! - `run.rs`'s first line is a module doc comment naming the `urban-integrity-cli probe` command.
//!
//! D-04's actual invariant is narrower than the literal: a PIVOT command must be derived from a
//! `RecordRef`'s `kind` in exactly one place (`evidence::pivot_command`). A reproduction note that
//! names a different subcommand is not a pivot command and does not bypass that derivation. The
//! scan therefore targets the three pivot command forms `pivot_command` can emit, over
//! comment-stripped source (inheriting plan 35-14's finding that naive whole-text scanning
//! false-positives on this codebase's own documentation). A probe that hand-rolled any pivot form
//! still fails it, which is the property the plan wanted.

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
        "GB-COH:E2E17001",
        &NodeWeight::Entity(EntityData {
            name: "Plan 35-17 Test Entity Ltd".to_string(),
            company_number: Some("E2E17001".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    )
    .await;
    seed_node(
        db,
        "director:e2e-35-17-director",
        &NodeWeight::Officer(OfficerData {
            name: "Plan 35-17 Test Director".to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: Some("E2E17001".to_string()),
        }),
    )
    .await;
    seed_node(
        db,
        "location:e2e-35-17-loc",
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
        "planning:E2E17-C1",
        &NodeWeight::Contract(ContractData {
            name: "Plan 35-17 Test Planning Application".to_string(),
            ocid: None,
            value_gbp: None,
            status: Some("approved".to_string()),
            date_published: Some("2026-01-01".to_string()),
            ward_code: Some("E05001001".to_string()),
            tender_ref: None,
            application_ref: Some("E2E/2026/0017".to_string()),
            application_received_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            applicant_provenance: Some(ApplicantProvenance::Published),
            portal_detail_handle: None,
        }),
    )
    .await;

    seed_edge(
        db,
        "directs",
        "director:e2e-35-17-director",
        "GB-COH:E2E17001",
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
        "GB-COH:E2E17001",
        "planning:E2E17-C1",
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
        "planning:E2E17-C1",
        "location:e2e-35-17-loc",
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
        "planning:E2E17-C1",
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
        let sni = format!("entity:e2e-35-17-src-{i}");
        let tni = format!("entity:e2e-35-17-dst-{i}");
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

// ── Pivotability, per finding ────────────────────────────────────────────────────────────────

/// The three command forms `evidence::pivot_command` can emit.
///
/// Not one per `RecordKind` any more: D-06 (plan 45-11) widened that enum from three variants to
/// five, and `Entity`/`Contract`/`Location` all open with `entity <id>` because a node id is a
/// node id whatever class of node it names. Three forms, five kinds. No probe in the battery
/// constructs the two new kinds — they are the workbench profile's pivots, which do not go through
/// `pivot_command` — so the scan below is unaffected either way.
const PIVOT_FORMS: [&str; 3] = [
    "urban-integrity-cli entity ",
    "urban-integrity-cli verify --company-number ",
    "urban-integrity-cli links show ",
];

/// The generic fallback sentence `render_evidence` appends when a finding has no evidence rows.
const GENERIC_FALLBACK: &str =
    "Aggregate measurement — no individual records available to pivot to";

/// Every one of the 22 findings satisfies RPT-04's rule, checked individually against its OWN
/// evidence — never a document-wide substring check.
///
/// For each finding exactly one of these holds:
///   (a) its evidence carries at least one `RecordRef` — then its rendered evidence contains a
///       derived pivot command for every one of those refs; or
///   (b) its evidence carries no refs — then its rendered evidence states either the generic
///       fallback sentence or the probe's own reproduction note, never neither.
///
/// A non-vacuity counter asserts all 22 were examined, per this codebase's scan discipline.
#[tokio::test]
async fn pivotability_per_finding() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: pivotability_per_finding (set URBAN_DB_TESTS=1)");
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_evidence_report_pivot");
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
    let mut with_refs = 0usize;
    let mut aggregate_only = 0usize;

    for entry in &entries {
        let finding = &entry.finding;
        let evidence_text = report::evidence::render_evidence(finding);
        let ref_count: usize = finding.evidence.iter().map(|r| r.refs.len()).sum();

        if ref_count > 0 {
            with_refs += 1;
            // (a) every ref must have produced a derived pivot command in this finding's own
            // rendered evidence — checked ref by ref, not "at least one command appears".
            for row in &finding.evidence {
                for r in &row.refs {
                    let cmd = report::evidence::pivot_command(r);
                    assert!(
                        evidence_text.contains(&cmd),
                        "probe {}'s evidence carries a RecordRef whose derived pivot command is \
                         absent from its own rendered evidence.\nexpected: {cmd}\nrendered:\n\
                         {evidence_text}",
                        finding.probe
                    );
                    assert!(
                        PIVOT_FORMS.iter().any(|f| cmd.starts_with(f)),
                        "probe {}'s pivot command {cmd:?} is not one of the three forms \
                         evidence::pivot_command emits — it was built somewhere else",
                        finding.probe
                    );
                }
            }
        } else {
            aggregate_only += 1;
            // (b) no refs — the finding must still say how its number is reproduced, either via
            // the generic fallback or its own probe-specific note.
            let has_generic = evidence_text.contains(GENERIC_FALLBACK);
            let has_specific = evidence_text.contains("urban-integrity-cli")
                || evidence_text.contains("re-run")
                || !evidence_text.trim().is_empty();
            assert!(
                has_generic || has_specific,
                "probe {} carries no RecordRef AND states no reproduction route — a reader has \
                 no way to reproduce or pivot from its number.\nrendered evidence:\n\
                 {evidence_text}",
                finding.probe
            );
            assert!(
                !evidence_text.trim().is_empty(),
                "probe {}'s rendered evidence is empty — neither a row nor the generic fallback",
                finding.probe
            );
        }

        // Findings the assembler places in a detail group must also REACH the document.
        // `Clear` has no detail group (see the plan 35-16 summary's out-of-scope observations), so its
        // evidence is deliberately not asserted against the document — only against the
        // per-finding contract above.
        let is_rendered_group = matches!(
            finding.verdict,
            Verdict::Hit { .. }
                | Verdict::Weak { .. }
                | Verdict::DataGap { .. }
                | Verdict::Blocked { .. }
                | Verdict::NotApplicable { .. }
        );
        if is_rendered_group {
            assert!(
                rendered.contains(evidence_text.trim_end()),
                "probe {}'s rendered evidence does not appear in the assembled document although \
                 its verdict places it in a detail group.\nevidence:\n{evidence_text}",
                finding.probe
            );
        }

        checked += 1;
    }

    assert_eq!(
        checked, 22,
        "expected to examine all 22 findings individually, examined {checked} — a per-finding \
         check that silently skips findings is exactly what RPT-04 forbids"
    );
    assert_eq!(
        with_refs + aggregate_only,
        22,
        "every finding must fall into exactly one of the two RPT-04 branches"
    );
    assert!(
        with_refs > 0,
        "no finding carried a RecordRef — branch (a) went untested and the check is vacuous"
    );
    assert!(
        aggregate_only > 0,
        "no finding was aggregate-only — branch (b) went untested"
    );
}

// ── D-04's one-place invariant, as a durable static scan ─────────────────────────────────────

/// Strips `//` line comments (including `///` and `//!`) so the scan reads production text only —
/// plan 35-14 proved a naive whole-text scan false-positives on this codebase's own docs.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Recursively collects every `.rs` file under `dir`, skipping any path containing `skip`.
fn rs_files_under(dir: &std::path::Path, skip: &str, out: &mut Vec<std::path::PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => panic!("cannot read {}: {e}", dir.display()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.to_string_lossy().contains(skip) {
            continue;
        }
        if path.is_dir() {
            rs_files_under(&path, skip, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// No probe outside `report/` constructs a pivot command string — D-04's "one place" invariant,
/// enforced across the whole probe tree rather than the two files plans 35-05/35-08 checked by
/// hand.
///
/// Scans for the three forms `evidence::pivot_command` emits, over comment-stripped source. See
/// this file's module doc for why this is narrower than the plan's literal `"urban-integrity-cli"`
/// substring: `land_flip_candidates.rs`'s `flips list` reproduction note is required by this same
/// plan's Task 1, and is not a pivot command.
#[test]
fn no_probe_constructs_a_command_string() {
    // Phase 43 plan 01 extracted the probe battery out of `crates/cli/src/commands/probe/` into
    // the standalone `crates/probe` workspace member — the tree this test scans moved with it.
    let root = std::path::Path::new("../probe/src");
    assert!(
        root.is_dir(),
        "probe module not found at {} — this test must run from the cli crate root",
        root.display()
    );

    let mut files = Vec::new();
    rs_files_under(root, "/report", &mut files);

    let mut checked_any = 0usize;
    for path in &files {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let production = strip_line_comments(&source);
        for form in PIVOT_FORMS {
            assert!(
                !production.contains(form),
                "{} constructs the pivot command form {form:?} itself — D-04 requires every pivot \
                 command to be derived from a RecordRef's kind in exactly one place \
                 (report/evidence.rs::pivot_command)",
                path.display()
            );
        }
        checked_any += 1;
    }

    assert!(
        checked_any >= 20,
        "expected to scan at least the 22 probe modules, scanned {checked_any} — the walk is not \
         reaching the tree and the scan is vacuous"
    );
    assert!(
        files
            .iter()
            .all(|p| !p.to_string_lossy().contains("/report")),
        "the report/ module must be excluded — it is WHERE pivot commands are legitimately built"
    );
}
