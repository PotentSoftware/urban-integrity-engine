//! ORC-07's DB-gated population-oracle agreement test — modelled on `probe_tier1_e2e.rs`'s
//! established pattern for bypassing `AppConfig::load()` under `cargo test`'s default parallel
//! execution (see that file's header for why this is necessary rather than calling
//! `probe::run::execute()` directly).
//!
//! Two tests:
//!
//! 1. `probe_oracle_agrees_on_every_gated_probe_within_delta` — seeds a fixture with NO
//!    duplicate/re-ingested events for any entity/edge, so every `CHECKED_PROBES` probe's
//!    declared population and the oracle's independent count naturally agree exactly. The
//!    by-name skip for `geographic-concentration`/`osint-new-node-types` was removed in Phase 40
//!    once both findings (ADJ-01, ADJ-03) resolved — every `CHECKED_PROBES` row is now asserted
//!    (phase 40 design decision D-09).
//! 2. `probe_oracle_flags_a_real_counting_shape_disagreement` — the non-vacuity proof: seeds a
//!    REAL, non-artificial counting-shape disagreement (`address-cluster-exact`'s documented
//!    src-parse asymmetry) and proves `compare()` actually detects and flags it, rather than
//!    vacuously always agreeing. It previously used `geographic-concentration`'s un-deduped
//!    scan; that was ADJ-01, which this phase's oracle surfaced and which was fixed on
//!    2026-09-01 — see that test's own doc comment for why the replacement source is a
//!    structural asymmetry rather than another defect.
//!
//! ## Why this file's battery is ungated (GAT-06/D-20)
//!
//! Both tests below call [`run_battery`], which calls `run_probes(probes, db, &mut st)` directly —
//! there is no `ProbeArgs`, no `probe::run::execute()`, no CLI parsing anywhere in this file's call
//! path. `urban_integrity_probe::run_probes` is the deliberately ungated primitive and is never
//! modified by Phase 41 (see its own doc comment); the phase's gated dispatch path lives entirely
//! in a separate function (`urban_integrity_probe::graph::run_gated`, added by a different plan of
//! this phase) that only `probe::run::execute()`'s default path invokes. So this test's battery is unaffected
//! by gating BY CONSTRUCTION — not because it passes a `--full-battery` flag. D-20's phrasing "runs
//! ungated (`--full-battery`)" describes the EFFECT this test needs (every `CHECKED_PROBES` probe
//! produces a real, run verdict, never `Verdict::Skipped`), not a literal CLI flag this test passes;
//! `run_battery()` has no `ProbeArgs` in its call path at all and never will. Both
//! `#[tokio::test]` functions below assert this directly: no entry's `finding.verdict` may match
//! `Verdict::Skipped { .. }`.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_cli::commands::probe_oracle;
use urban_integrity_cli::{db, registry};
use urban_integrity_core::config::AppConfig;
use urban_integrity_probe::report::ReportEntry;
use urban_integrity_probe::run as probe_run;
use urban_integrity_probe::{run_probes, Probe, Strategy};
use urban_integrity_schema::types::edge::{
    AwardedToData, EdgeMeta, EdgeWeight, LocatedAtData, RepresentsData,
};
use urban_integrity_schema::types::node::{
    ApplicantProvenance, CharityData, ContractData, EntityData, LocationData, NodeWeight,
    OffshoreEntityData, PoliticalActorData, PropertyData, PscData,
};
use urban_integrity_schema::types::verdict::{PopulationSource, Verdict};

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Copied verbatim from `probe_tier1_e2e.rs`'s `test_config` — the established pattern for
/// pointing a real `AppConfig` at an isolated namespace without mutating global process env vars.
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

/// Copied verbatim from `probe_tier1_e2e.rs`'s `seed_node`.
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

/// Copied verbatim from `probe_tier1_e2e.rs`'s `seed_edge`.
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

/// A raw `MotifDetected` event — feeds `precomputed-alerts`. Not sourced through `seed_node`
/// (that helper always emits `NodeAdded`), so this is its own raw `graph_events` CREATE, mirroring
/// `precomputed_alerts.rs`'s own test fixture shape.
async fn seed_motif_event(db: &Surreal<Any>, suffix: &str) {
    let result_json = serde_json::json!({
        "motif_type": "phoenix_loop",
        "risk_score": 0.9,
    })
    .to_string();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("MotifDetected-fixture-{suffix}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "MotifDetected",
                "payload": { "result_json": result_json },
            }),
        ))
        .await
        .expect("failed to seed MotifDetected event")
        .check()
        .expect("seed MotifDetected event returned a statement error");
}

/// A raw `NodeUpdated` event whose `weight_json` contains the literal substring `InLiquidation` —
/// feeds `gazette-insolvency-signals`. Deliberately NOT a real `NodeWeight::Entity` serialisation:
/// only the substring match matters to both the probe and the oracle (see
/// `probe_oracle.rs::CHECKED_PROBES`'s `gazette-insolvency-signals` entry, a plain substring
/// marker, not the JSON-shaped marker `osint-new-node-types` uses for the same term).
async fn seed_inliquidation_event(db: &Surreal<Any>, entity_id: &str) {
    let weight_json = serde_json::json!({
        "type": "InLiquidation",
        "content": "2024-01-01",
    })
    .to_string();
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": format!("NodeUpdated-{entity_id}"),
                "timestamp": 1_700_000_000_000u64,
                "kind": "NodeUpdated",
                "payload": { "entity_id": entity_id, "weight_json": weight_json },
            }),
        ))
        .await
        .expect("failed to seed InLiquidation event")
        .check()
        .expect("seed InLiquidation event returned a statement error");
}

/// The exact `enrichment_runs:council_registers` UPSERT shape `engine.rs::build_run_marker_upsert`
/// produces (read only, not imported — that function is not `pub`), reproduced here with a raw
/// `db.query(...)` per this plan's own interfaces note.
async fn seed_council_registers_marker(db: &Surreal<Any>) {
    let last_run_at = chrono::Utc::now().timestamp_millis();
    db.query(
        "UPSERT enrichment_runs:council_registers SET source = 'council_registers', \
         last_run_at = $last_run_at, candidates_found = 1, auto_linked = 0, \
         pending_review = 0, discarded = 0",
    )
    .bind(("last_run_at", last_run_at))
    .await
    .unwrap()
    .check()
    .unwrap();
}

/// Seeds a fixture with NO duplicate/re-ingested events for any single entity/edge, so every
/// `CHECKED_PROBES` probe's declared population and the oracle's independent count naturally
/// agree exactly.
async fn seed_full_agreement_fixture(db: &Surreal<Any>) {
    // identity-integrity / ward-concentration / temporal-burst: one "planning:"-prefixed
    // Contract and one non-"planning:" Contract — proves identity-integrity's prefix filter
    // (population 1) diverges correctly from ward-concentration/temporal-burst's unfiltered
    // population (2), and both still independently agree with their own oracle check.
    seed_node(
        db,
        "planning:c1",
        &NodeWeight::Contract(ContractData {
            name: "Fixture Planning Contract".to_string(),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: Some(ApplicantProvenance::Published),
            portal_detail_handle: None,
        }),
    )
    .await;
    seed_node(
        db,
        "GB-CF:c2",
        &NodeWeight::Contract(ContractData {
            name: "Fixture CF Contract".to_string(),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: Some(ApplicantProvenance::Published),
            portal_detail_handle: None,
        }),
    )
    .await;

    // address-cluster-exact: one Entity, one Location, an AwardedTo edge and a LocatedAt edge —
    // population 1 (one distinct located destination).
    seed_node(
        db,
        "GB-COH:applicant-1",
        &NodeWeight::Entity(EntityData {
            name: "Fixture Applicant Ltd".to_string(),
            company_number: Some("FIX0001".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    )
    .await;
    seed_node(
        db,
        "GB-LOC:site-1",
        &NodeWeight::Location(LocationData {
            name: "1 Fixture Street".to_string(),
            postcode: Some("SW1A 1AA".to_string()),
            address_line_1: Some("1 Fixture Street".to_string()),
            locality: Some("Fixtureville".to_string()),
        }),
    )
    .await;
    seed_edge(
        db,
        "awarded-to",
        "GB-COH:applicant-1",
        "planning:c1",
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
        "planning:c1",
        "GB-LOC:site-1",
        &EdgeWeight::LocatedAt {
            meta: edge_meta(),
            data: LocatedAtData {
                details: String::new(),
            },
        },
    )
    .await;

    // ward-concentration: needs at least one Represents edge, or it short-circuits to DataGap
    // (`wards.is_empty()`) regardless of the Contract population — a Represents edge for just
    // one of the two Contracts is enough to make it report the full Contract population (2).
    seed_edge(
        db,
        "represents",
        "ward:E05001001",
        "planning:c1",
        &EdgeWeight::Represents {
            meta: edge_meta(),
            data: RepresentsData {
                details: String::new(),
            },
        },
    )
    .await;

    // precomputed-alerts: the detector-run marker plus 2 MotifDetected events — population 2.
    // A legacy-shaped `{ last_run_at, motif_count }` row is sufficient here — this fixture only
    // needs `detector_has_ever_run` to read `true`, never `record_detector_run` (removed by
    // Phase 46, D-05).
    db.query(
        "UPSERT detector_runs:current SET last_run_at = $last_run_at, motif_count = $motif_count",
    )
    .bind(("last_run_at", chrono::Utc::now().timestamp_millis()))
    .bind(("motif_count", 2i64))
    .await
    .expect("seed detector_runs marker")
    .check()
    .expect("seed detector_runs marker (check)");
    for i in 0..2 {
        seed_motif_event(db, &format!("fixture-{i}")).await;
    }

    // gazette-insolvency-signals: 1 InLiquidation NodeUpdated event — population 1.
    seed_inliquidation_event(db, "GB-COH:gazette-fixture-1").await;

    // property-nodes: 1 Property node (country_incorporated: None -> land_registry_ccod) —
    // population 1.
    seed_node(
        db,
        "property:fixture-1",
        &NodeWeight::Property(PropertyData {
            title_number: Some("TN-FIXTURE-1".to_string()),
            tenure: Some("Freehold".to_string()),
            price_paid: Some(100_000.0),
            company_number: None,
            country_incorporated: None,
        }),
    )
    .await;

    // phase20-council-registers: 1 council-shaped PoliticalActor node plus the
    // enrichment_runs:council_registers marker — without the marker this probe short-circuits
    // to DataGap regardless of the PoliticalActor node.
    seed_node(
        db,
        "political_actor:fixture-cllr-1",
        &NodeWeight::PoliticalActor(PoliticalActorData {
            name: "Cllr Fixture".to_string(),
            party: None,
            council_name: Some("Anglesey Council".to_string()),
        }),
    )
    .await;
    seed_council_registers_marker(db).await;

    // osint-new-node-types: seeds all 5 SumOfEvents terms (4 NodeAdded types + the InLiquidation
    // NodeUpdated below) so this probe agrees with the oracle exactly, same as every other row.
    seed_node(
        db,
        "psc:fixture-1",
        &NodeWeight::Psc(PscData {
            name: "Fixture PSC".to_string(),
            company_number: None,
            natures_of_control: None,
        }),
    )
    .await;
    seed_node(
        db,
        "charity:fixture-1",
        &NodeWeight::Charity(CharityData {
            name: "Fixture Charity".to_string(),
            charity_number: None,
            registered_name: None,
        }),
    )
    .await;
    seed_node(
        db,
        "offshore:fixture-1",
        &NodeWeight::OffshoreEntity(OffshoreEntityData {
            name: "Fixture Offshore Co".to_string(),
            jurisdiction: None,
            icij_node_id: None,
        }),
    )
    .await;
}

/// The REAL 22-probe battery, from its single registration site.
///
/// This used to hand-build the 10 `CHECKED_PROBES` probes as a subset vec. CR-02 (phase 38 code review)
/// is why it no longer does: a hand-copied subset drifts silently from the battery, and it meant
/// `compare()`'s coverage reconciliation never saw the 12 excluded names even in a debug run —
/// the one place the check could have fired. Calling `build_battery` makes this test exercise the
/// same list the shipped command does, so battery drift breaks it.
///
/// `OcProbeGates::default()` leaves `OpencorporatesCrossJurisdiction` disabled, which is the state
/// `config.toml` ships (`[enricher.opencorporates]` is absent). The probe still registers and
/// still reports a verdict, so coverage is unaffected.
fn full_battery(catalog: Surreal<Any>, db_name: String) -> Vec<Box<dyn Probe>> {
    probe_run::build_battery(catalog, db_name, probe_run::OcProbeGates::default())
}

/// Runs `probes` through `run_probes` and zips the results into `ReportEntry`s exactly the way
/// `probe::run::execute` does — reusing `ReportEntry` directly, never a parallel struct.
///
/// This direct call to `run_probes(probes, db, &mut st)` — not a `--full-battery` flag, which this
/// function has no path to pass — is the mechanism that keeps this test's battery ungated for
/// GAT-06/D-20 (see the module doc comment above). `run_probes` is untouched by Phase 41's gating
/// work; the gated dispatch path is a separate function only `execute()`'s default path calls.
async fn run_battery(probes: &[Box<dyn Probe>], db: &Surreal<Any>) -> Vec<ReportEntry> {
    let mut st = Strategy::default();
    let findings = run_probes(probes, db, &mut st).await;
    probes
        .iter()
        .zip(findings)
        .map(|(p, finding)| ReportEntry {
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

/// ORC-07's core agreement proof: every `CHECKED_PROBES` entry must agree with the oracle
/// exactly, on a fixture with zero duplicate/re-ingested events. No probe is excluded by name
/// (phase 40 design decision D-09).
#[tokio::test]
async fn probe_oracle_agrees_on_every_gated_probe_within_delta() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: probe_oracle_agrees_on_every_gated_probe_within_delta (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_probe_oracle_agree");
    let cfg = test_config(ns.name());

    let db = db::connect_db(&cfg).await.expect("connect_db failed");
    seed_full_agreement_fixture(&db).await;

    let catalog = registry::connect_catalog(&cfg)
        .await
        .expect("connect_catalog failed");
    let db_name = db::resolve_analysis_db(&cfg)
        .await
        .expect("resolve_analysis_db failed");

    let probes = full_battery(catalog, db_name);
    let entries = run_battery(&probes, &db).await;

    // GAT-06/D-20: this test's battery must never gate — assert no finding was skipped rather than
    // run. See the module doc comment for why `run_battery` -> `run_probes` guarantees this by
    // construction; this assertion is the checked, regression-proof form of that guarantee.
    for entry in &entries {
        assert!(
            !matches!(entry.finding.verdict, Verdict::Skipped { .. }),
            "GAT-06/D-20: ORC-07's battery must never gate — found a Skipped verdict for probe \
             '{}', which means this test's dispatch path changed",
            entry.finding.probe
        );
    }

    let report = probe_oracle::compare(&entries, &db)
        .await
        .expect("compare() must succeed against a fully-populated battery");

    for row in &report.checked {
        // ADJ-01 (geographic-concentration, resolved Phase 38) and ADJ-03 (osint-new-node-types,
        // resolved Phase 40) are both settled — every CHECKED_PROBES row is now asserted, with no
        // by-name exclusion (phase 40 design decision D-09).
        assert_eq!(
            row.agrees,
            Some(true),
            "probe {} disagreed: declared={:?} oracle={} delta={}",
            row.probe,
            row.declared,
            row.oracle,
            row.delta_allowed
        );
    }
}

/// The non-vacuity proof: a real, non-artificial counting-shape disagreement must make
/// `compare()` report `agrees: Some(false)`, not silently agree.
///
/// # Why this uses `address-cluster-exact` and not `geographic-concentration`
///
/// This test originally built its disagreement from `geographic-concentration`'s un-deduped
/// `scan_entities_by_type` path: re-ingesting one Location emitted a second `NodeAdded` row that
/// the probe counted as a second population unit while the oracle counted one entity. That WAS a
/// real disagreement — it was ADJ-01, and the oracle built in this phase is what proved it
/// (102,897 events over 61,756 distinct Locations on `an_anglesey_aebaa785`, 22,839 of them
/// duplicated). The probe was fixed on 2026-09-01 to use `scan_latest_entities_by_type`, so that
/// source of disagreement no longer exists and this test could no longer distinguish a working
/// `compare()` from a broken one.
///
/// The replacement is deliberately a **structural asymmetry, not a defect** — so it cannot be
/// "fixed" out from under this test the way ADJ-01 was. `shared_exact_address.rs:141-144` counts
/// a `LocatedAt` edge's destination only when BOTH `src_id` and `dst_id` resolve through
/// `record_id_str`, which returns `None` for an empty string. The oracle's
/// `count_distinct_edge_field(.., Dst)` groups by `payload.dst_id` alone and never inspects
/// `src_id`. One `LocatedAt` edge with an empty `src_id` and an otherwise-unseen `dst_id` is
/// therefore counted by the oracle and skipped by the probe: `oracle == declared + 1`, from two
/// counting shapes that are each individually correct. This is the asymmetry already documented
/// on the `address-cluster-exact` entry in `probe_oracle.rs`.
#[tokio::test]
async fn probe_oracle_flags_a_real_counting_shape_disagreement() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: probe_oracle_flags_a_real_counting_shape_disagreement (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_probe_oracle_disagree");
    let cfg = test_config(ns.name());

    let db = db::connect_db(&cfg).await.expect("connect_db failed");

    // Start from the fixture every probe agrees on, so the ONLY disagreement in the report is
    // the one this test deliberately introduces below. A test that seeded the divergent edge
    // alone could pass while `compare()` flagged everything indiscriminately.
    seed_full_agreement_fixture(&db).await;

    // The divergence: a LocatedAt edge whose `src_id` is empty (so `record_id_str` yields None
    // and `shared_exact_address` drops the row) pointing at a `dst_id` no other edge uses (so
    // the oracle's DISTINCT dst count gains exactly one).
    seed_node(
        &db,
        "GB-LOC:unreferenced-dst",
        &NodeWeight::Location(LocationData {
            name: "Location reachable only via an unparseable-src edge".to_string(),
            postcode: None,
            address_line_1: None,
            locality: None,
        }),
    )
    .await;
    seed_edge(
        &db,
        "locatedat-emptysrc",
        "",
        "GB-LOC:unreferenced-dst",
        &EdgeWeight::LocatedAt {
            meta: edge_meta(),
            data: LocatedAtData {
                details: String::new(),
            },
        },
    )
    .await;

    // `compare()` requires every CHECKED_PROBES entry to be present in `entries` (it errors out
    // if any is missing -- CHECKED_PROBES has 10 entries, not 1), so the full 10-probe battery
    // runs here too, even though only the address-cluster-exact row is asserted on below.
    let catalog: Surreal<Any> = Surreal::init();
    let db_name = db::resolve_analysis_db(&cfg)
        .await
        .expect("resolve_analysis_db failed");

    let probes = full_battery(catalog, db_name);
    let entries = run_battery(&probes, &db).await;

    // GAT-06/D-20: this test's battery must never gate — assert no finding was skipped rather than
    // run. See the module doc comment for why `run_battery` -> `run_probes` guarantees this by
    // construction; this assertion is the checked, regression-proof form of that guarantee.
    for entry in &entries {
        assert!(
            !matches!(entry.finding.verdict, Verdict::Skipped { .. }),
            "GAT-06/D-20: ORC-07's battery must never gate — found a Skipped verdict for probe \
             '{}', which means this test's dispatch path changed",
            entry.finding.probe
        );
    }

    let report = probe_oracle::compare(&entries, &db)
        .await
        .expect("compare() must succeed against a fully-populated battery");

    let row = report
        .checked
        .iter()
        .find(|r| r.probe == "address-cluster-exact")
        .expect("address-cluster-exact row must be present");

    let declared = row
        .declared
        .expect("address-cluster-exact must have measured a population in this fixture");
    assert_eq!(
        row.oracle,
        declared + 1,
        "the empty-src LocatedAt edge must be counted by the oracle and skipped by the probe: \
         declared={declared} oracle={}",
        row.oracle
    );
    assert_eq!(
        row.agrees,
        Some(false),
        "a real counting-shape gap must be flagged as a disagreement, got agrees={:?} \
         (declared={declared}, oracle={})",
        row.agrees,
        row.oracle
    );

    // Non-vacuity of the non-vacuity proof: every OTHER checked probe must still agree, so this
    // test fails if `compare()` degenerates into flagging everything.
    for other in report.checked.iter().filter(|r| r.probe != row.probe) {
        // `assert_ne!(agrees, Some(false))` was satisfied by `agrees == None` — a probe that
        // silently degraded to DataGap (bad catalog handle, missing marker) passed while proving
        // nothing (WR-07). Every other probe must be MEASURED and agree, not merely not-disagree.
        assert_eq!(
            other.agrees,
            Some(true),
            "every other checked probe must be MEASURED and AGREE in this fixture; {} was {:?} \
             (declared={:?}, oracle={})",
            other.probe,
            other.agrees,
            other.declared,
            other.oracle
        );
    }
}
