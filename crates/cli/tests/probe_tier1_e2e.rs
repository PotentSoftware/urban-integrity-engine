//! DB-gated end-to-end test proving `urban-integrity-cli probe`'s 7-probe battery is reachable
//! and returns real findings (32-06 Task 2, ROADMAP success criterion 1).
//!
//! # Why this test builds the probe battery directly instead of calling `probe::run::execute()`
//!
//! `execute()` hardcodes `AppConfig::load()`, which reads `config.toml` plus `URBAN__*` env
//! vars — there is no parameter to redirect it at a `TestNs` namespace without mutating global
//! process env vars, which is unsafe under `cargo test`'s default parallel test execution (one
//! test's env mutation would race every other DB-gated test running concurrently in this binary).
//!
//! `crates/cli/tests/analysis_lifecycle.rs` already established the correct alternative for this
//! exact problem: build an `AppConfig` value directly (`test_config`, below, copied from that
//! file) and pass it straight to the real production functions that take a `&AppConfig`
//! parameter — `db::connect_db`, `registry::connect_catalog`, `db::resolve_analysis_db`. Every
//! one of those is the SAME function `execute()` calls; only the top-level `AppConfig::load()`
//! wrapper and the `execute()`/`run()` functions themselves are bypassed. This is the "fall back
//! to calling `run_probes` directly ... bypassing `execute()`'s `AppConfig::load()` call" scope
//! plan 32-06's Task 2 action explicitly sanctions, done via a real typed `AppConfig` rather
//! than raw namespace string-passing.

use std::path::PathBuf;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_cli::{db, registry};
use urban_integrity_core::config::AppConfig;
use urban_integrity_probe::detector_readiness::DetectorReadiness;
use urban_integrity_probe::geographic_concentration::GeographicConcentration;
use urban_integrity_probe::identity_integrity::IdentityIntegrity;
use urban_integrity_probe::precomputed_alerts::PrecomputedAlerts;
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
use urban_integrity_schema::types::verdict::Verdict;

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Build an `AppConfig` pointed at an isolated namespace, no named analysis (`None` selects
/// `config.surreal.database` verbatim, i.e. `"main"` below) — copied verbatim from
/// `analysis_lifecycle.rs`'s `test_config`, the established pattern for this exact need.
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

/// The ordered 7-probe battery, matching `run.rs::execute`'s `Vec<Box<dyn Probe>>` exactly
/// (registration order is load-bearing: `IdentityIntegrity` at position 2 must precede
/// `SharedExactAddress` at position 4).
fn tier1_battery(catalog: Surreal<Any>, db_name: String) -> Vec<Box<dyn Probe>> {
    vec![
        Box::new(PrecomputedAlerts),
        Box::new(IdentityIntegrity { catalog, db_name }),
        Box::new(DetectorReadiness),
        Box::new(SharedExactAddress),
        Box::new(GeographicConcentration),
        Box::new(WardConcentration),
        Box::new(TemporalBurst),
    ]
}

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

/// Seeds a minimal fixture spanning every node/edge type the 7 Tier-1 probes touch: one
/// `Entity`, one `Officer`, one `Location`, one `Contract` (planning-prefixed, with a
/// `timestamp`), and one `Directs`/`AwardedTo`/`LocatedAt`/`Represents` edge each. Post-D-04:
/// `LocatedAt` is sourced from the Contract node, not the applicant Entity
/// (`shared_exact_address.rs`'s module doc comment).
async fn seed_reachability_fixture(db: &Surreal<Any>) {
    seed_node(
        db,
        "GB-COH:E2E001",
        &NodeWeight::Entity(EntityData {
            name: "E2E Test Entity Ltd".to_string(),
            company_number: Some("E2E00001".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    )
    .await;
    seed_node(
        db,
        "director:e2e-test-director",
        &NodeWeight::Officer(OfficerData {
            name: "E2E Test Director".to_string(),
            officer_role: Some("director".to_string()),
            appointed_on: None,
            resigned_on: None,
            company_number: Some("E2E00001".to_string()),
        }),
    )
    .await;
    seed_node(
        db,
        "location:e2e-loc-001",
        &NodeWeight::Location(LocationData {
            name: "1 Test Street".to_string(),
            postcode: Some("SW1A1AA".to_string()),
            address_line_1: Some("1 Test Street".to_string()),
            locality: Some("Testville".to_string()),
        }),
    )
    .await;
    seed_node(
        db,
        "planning:E2E-C1",
        &NodeWeight::Contract(ContractData {
            name: "E2E Test Planning Application".to_string(),
            ocid: None,
            value_gbp: None,
            status: Some("approved".to_string()),
            date_published: Some("2026-01-01".to_string()),
            ward_code: Some("E05001001".to_string()),
            tender_ref: None,
            application_ref: Some("E2E/2026/0001".to_string()),
            application_received_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            applicant_provenance: Some(ApplicantProvenance::Published),
            portal_detail_handle: None,
        }),
    )
    .await;

    seed_edge(
        db,
        "directs",
        "director:e2e-test-director",
        "GB-COH:E2E001",
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
        "GB-COH:E2E001",
        "planning:E2E-C1",
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
        "planning:E2E-C1",
        "location:e2e-loc-001",
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
        "planning:E2E-C1",
        &EdgeWeight::Represents {
            meta: edge_meta(),
            data: RepresentsData {
                details: String::new(),
            },
        },
    )
    .await;
}

/// ROADMAP success criterion 1: all 7 Tier-1 probes are reachable through the real ordered
/// battery and return non-placeholder findings.
#[tokio::test]
async fn probe_battery_returns_seven_findings_with_correct_names_and_nonempty_summaries() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: probe_battery_returns_seven_findings_with_correct_names_and_nonempty_summaries (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_probe_e2e");
    let cfg = test_config(ns.name());

    let db = db::connect_db(&cfg).await.expect("connect_db failed");
    seed_reachability_fixture(&db).await;

    let catalog = registry::connect_catalog(&cfg)
        .await
        .expect("connect_catalog failed");
    let db_name = db::resolve_analysis_db(&cfg)
        .await
        .expect("resolve_analysis_db failed");

    let probes = tier1_battery(catalog, db_name);
    let mut st = Strategy::default();
    let findings = run_probes(&probes, &db, &mut st).await;

    assert_eq!(
        findings.len(),
        7,
        "expected all 7 Tier-1 findings, got {findings:?}"
    );

    let expected_names: std::collections::HashSet<&str> = [
        "precomputed-alerts",
        "identity-integrity",
        "detector-readiness",
        "address-cluster-exact",
        "geographic-concentration",
        "ward-concentration",
        "temporal-burst",
    ]
    .into_iter()
    .collect();
    let actual_names: std::collections::HashSet<&str> =
        findings.iter().map(|f| f.probe.as_str()).collect();
    assert_eq!(
        actual_names, expected_names,
        "the set of Finding.probe display names must equal exactly the 7 expected strings"
    );

    for finding in &findings {
        assert!(
            !finding.summary.trim().is_empty(),
            "finding for probe {} has an empty summary — placeholder finding, not real evidence",
            finding.probe
        );
    }
}

/// ROADMAP success criterion 5, proven through the REAL ordered battery rather than a
/// preset-flag unit test (32-06 Task 3 Part B).
///
/// 32-03's `shared_exact_address_blocked_fires_before_clear_when_identity_blocked` proves the
/// READ side of the `Strategy.identity_blocked` cross-probe dependency in isolation, by manually
/// constructing a `Strategy { identity_blocked: true, .. }` before calling
/// `SharedExactAddress::run` directly — that proves the branch logic works, but says nothing
/// about whether the dependency survives real dispatch. This test drives `IdentityIntegrity`
/// (position 2 of the real ordered battery) to set the flag ITSELF, by seeding a fixture with
/// ZERO planning Contract nodes — the exact condition `identity_integrity.rs:98-109` checks
/// (`total == 0` on the planning-prefixed Contract population) — then runs the FULL ordered
/// 7-probe battery through `run_probes` with one shared `&mut Strategy`, exactly as the
/// reachability test above does. It never assigns `st.identity_blocked` itself anywhere in this
/// test's body; only `IdentityIntegrity::run` may write that field for this test to demonstrate
/// anything. What makes `SharedExactAddress` (`"address-cluster-exact"`) reach `Verdict::Blocked`
/// here is `IdentityIntegrity` actually running BEFORE it in `GRAPH_PROBES` dispatch order.
#[tokio::test]
async fn identity_blocked_propagates_through_the_real_battery_to_address_cluster_exact() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: identity_blocked_propagates_through_the_real_battery_to_address_cluster_exact (set URBAN_DB_TESTS=1)"
        );
        return;
    }

    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_probe_e2e_blocked");
    let cfg = test_config(ns.name());

    // Deliberately empty: zero planning Contract nodes (drives IdentityIntegrity's
    // total == 0 branch, which sets identity_blocked = true) and zero LocatedAt edges (so
    // SharedExactAddress's `shared` set is empty, satisfying its
    // `shared.is_empty() && st.identity_blocked` Blocked condition). No seeding call at all —
    // an unseeded TestNs database already satisfies both.
    let db = db::connect_db(&cfg).await.expect("connect_db failed");

    let catalog = registry::connect_catalog(&cfg)
        .await
        .expect("connect_catalog failed");
    let db_name = db::resolve_analysis_db(&cfg)
        .await
        .expect("resolve_analysis_db failed");

    let probes = tier1_battery(catalog, db_name);
    let mut st = Strategy::default();
    let findings = run_probes(&probes, &db, &mut st).await;

    assert_eq!(findings.len(), 7);

    let identity_finding = findings
        .iter()
        .find(|f| f.probe == "identity-integrity")
        .expect("identity-integrity finding present");
    assert!(
        matches!(identity_finding.verdict, Verdict::DataGap { .. }),
        "identity-integrity must report DataGap on zero planning Contracts, got {:?}",
        identity_finding.verdict
    );

    let address_finding = findings
        .iter()
        .find(|f| f.probe == "address-cluster-exact")
        .expect("address-cluster-exact finding present");
    assert!(
        matches!(address_finding.verdict, Verdict::Blocked { .. }),
        "address-cluster-exact must be Blocked when identity-integrity (dispatched earlier) set \
         identity_blocked=true — got {:?}. If this fails, the registration order in run.rs no \
         longer matches Python's GRAPH_PROBES order.",
        address_finding.verdict
    );
}

// ── Part C: GRD-04's routing claim, made falsifiable (32-06 Task 3) ────────────────────────────

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/cli; the workspace is two levels up.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

/// Files excluded from the raw-query scan: `run.rs` carries no probe body of its own (battery
/// registration/dispatch), `lib.rs` (formerly `mod.rs`) is pure scaffolding, and `registry.rs`
/// IS the `registry::` primitive Tier-1 probes are required to route through — Phase 43 plan 01
/// moved it into this same crate (it previously lived at `crates/cli/src/registry.rs`, outside
/// this directory, so this scan never saw it).
const EXCLUDED_FILES: &[&str] = &["run.rs", "lib.rs", "registry.rs"];

/// The production-code portion of a probe module's source: everything before its first
/// `#[cfg(test)]` marker. Every `.query(` call in every Tier-1 probe module lives inside a
/// `#[cfg(test)]` fixture block (verified by direct read of all 7 files, 2026-08-23) — this
/// scan must not trip on those, only on a raw query reachable from `Probe::run`.
fn production_portion(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(idx) => &src[..idx],
        None => src,
    }
}

/// GRD-04's evidence for this phase: every Tier-1 probe reads through
/// `persistence::scan`/`registry` primitives rather than issuing its own SurrealQL. Static
/// source scan — opens no DB connection, so it correctly shows no gated/ungated duration delta.
#[test]
fn tier1_probes_issue_no_raw_queries() {
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
            !production.contains(".query("),
            "{} issues a raw SurrealQL query outside #[cfg(test)] (excluded files: {:?}) — \
             Tier-1 probes must route through persistence::scan/registry primitives, not their \
             own SurrealQL (GRD-04)",
            path.display(),
            EXCLUDED_FILES
        );
    }
    assert!(
        checked_any,
        "the scan checked zero files under {} — the walk is broken",
        probe_dir.display()
    );
}
