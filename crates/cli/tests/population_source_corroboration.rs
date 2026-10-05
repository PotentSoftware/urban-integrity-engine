//! VRD-13's live corroboration proof (Plan 39-09): a real, oracle-checkable probe's
//! `PopulationSource` must come back `OracleCorroborated` with a count matching what was
//! actually seeded — proving the LIVE oracle query path end-to-end, not a mocked value.
//!
//! Modelled on `probe_oracle_agreement.rs` (same `db_tests_enabled`/`test_config` per-file
//! duplication convention, same `NsGuard` namespace hygiene, same build-battery/run-probes
//! shape as `run.rs::execute()`'s own wiring). Canary: `precomputed-alerts`, the simplest
//! `CHECKED_PROBES` entry (`OracleCheckKind::Events { kind: "MotifDetected", markers: &[] }`),
//! avoiding the entity-dedup fixture complexity other checked probes need.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_cli::db;
use urban_integrity_core::config::AppConfig;
use urban_integrity_probe::population_source;
use urban_integrity_probe::report::ReportEntry;
use urban_integrity_probe::run as probe_run;
use urban_integrity_probe::{run_probes, Probe, Strategy};
use urban_integrity_schema::types::verdict::PopulationSource;

fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Copied verbatim from `probe_oracle_agreement.rs` (itself from `probe_tier1_e2e.rs`) — the
/// established pattern for pointing a real `AppConfig` at an isolated namespace without mutating
/// global process env vars.
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

/// Copied verbatim from `probe_oracle_agreement.rs`'s `seed_motif_event`.
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

/// Runs the real battery and computes `population_source` per entry via the SAME
/// `for_probe(name, has_coverage, db)` call `run.rs::execute()` makes (Plan 39-08) — so this
/// test exercises the exact production dispatch path against a live database.
async fn run_battery_with_provenance(
    probes: &[Box<dyn Probe>],
    db: &Surreal<Any>,
) -> Vec<ReportEntry> {
    let mut st = Strategy::default();
    let findings = run_probes(probes, db, &mut st).await;
    let mut entries = Vec::with_capacity(probes.len());
    for (p, finding) in probes.iter().zip(findings) {
        let population_source =
            population_source::for_probe(p.name(), finding.verdict.coverage().is_some(), db)
                .await
                .expect("for_probe failed");
        entries.push(ReportEntry {
            tier: p.tier(),
            hypothesis: p.hypothesis(),
            motif_kind: p.motif_kind(),
            finding,
            population_source,
        });
    }
    entries
}

#[tokio::test]
async fn checked_probe_population_source_is_oracle_corroborated_matching_the_oracle() {
    if !db_tests_enabled() {
        eprintln!(
            "SKIP: checked_probe_population_source_is_oracle_corroborated_matching_the_oracle \
             (set URBAN_DB_TESTS=1)"
        );
        return;
    }
    // NsGuard bound for the whole test — never a hand-rolled namespace (docs/TESTING.md hygiene rule).
    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_population_source");
    let config = test_config(ns.name());
    let db = db::connect_db(&config).await.expect("connect");

    // Exactly 4 MotifDetected events, plus the detector-run marker precomputed-alerts reads.
    // A legacy-shaped `{ last_run_at, motif_count }` row is sufficient here — this fixture only
    // needs `detector_has_ever_run` to read `true`, never `record_detector_run` (removed by
    // Phase 46, D-05).
    db.query(
        "UPSERT detector_runs:current SET last_run_at = $last_run_at, motif_count = $motif_count",
    )
    .bind(("last_run_at", chrono::Utc::now().timestamp_millis()))
    .bind(("motif_count", 4i64))
    .await
    .expect("seed detector_runs marker")
    .check()
    .expect("seed detector_runs marker (check)");
    for i in 0..4 {
        seed_motif_event(&db, &format!("corrob-{i}")).await;
    }

    let probes = probe_run::build_battery(
        Surreal::init(),
        "main".to_string(),
        probe_run::OcProbeGates::default(),
    );
    let entries = run_battery_with_provenance(&probes, &db).await;

    let entry = entries
        .iter()
        .find(|e| e.finding.probe == "precomputed-alerts")
        .expect("precomputed-alerts entry present");

    // Destructure and assert the exact count — a defect that queried the wrong marker or
    // miscounted must fail here, never pass on the variant alone (T-39-11).
    match &entry.population_source {
        PopulationSource::OracleCorroborated { oracle_count } => {
            assert_eq!(
                *oracle_count, 4,
                "the oracle's independently recomputed MotifDetected count must equal the 4 \
                 events actually seeded"
            );
        }
        other => panic!("expected OracleCorroborated for a CHECKED_PROBES entry, got {other:?}"),
    }

    // And the probe's own declared population agrees with the oracle on this clean fixture —
    // the corroboration is a real cross-check of two independent paths, not one number
    // photocopied twice.
    assert_eq!(
        entry.finding.verdict.coverage().map(|c| c.population()),
        Some(4),
        "the probe's own declared population must also be 4 on this fixture"
    );
}
