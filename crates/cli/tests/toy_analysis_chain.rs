//! Spec §4.4: the toy analysis end to end — provision, seed, probe, boot the workbench, and
//! assert population meta and algorithm results equal the manifest over HTTP. Gated on
//! `URBAN_DB_TESTS=1`; namespace from `test-support`.
//!
//! Per the project's gate discipline (docs/TESTING.md), `URBAN_DB_TESTS=1` skips **by return**, so a green run with
//! the gate closed reports identically to a green run with it open. The evidence that this test
//! actually ran against a live SurrealDB is the run's **duration**, never the pass count alone.

use std::sync::Arc;

use urban_integrity_cli::commands::analysis::{create_analysis, CreateArgs};
use urban_integrity_cli::commands::export_graphml::{
    canonical_population_summary, CanonicalPopulation,
};
use urban_integrity_cli::{db, registry};
use urban_integrity_core::config::AppConfig;
use urban_integrity_test_support::planted::{planted_graph, OD, WARD_CODE};
use urban_integrity_workbench::boot::{
    bind_listener, boot, AnalysisCatalogEntry, BootParams, PopulationSummary,
};
use urban_integrity_workbench_dto::algorithm::{
    AlgorithmOutcome, AlgorithmRequest, AlgorithmResult,
};

fn test_config(namespace: &str) -> AppConfig {
    // Copied from crates/cli/tests/evidence_report_structure.rs::test_config — keep identical.
    let json = serde_json::json!({
        "analysis": null,
        "surreal": { "ws_url": "ws://localhost:8000", "namespace": namespace, "database": "main", "username": "root", "password": "root" },
        "graph": { "checkpoint_events_n": 1000, "checkpoint_interval_secs": 60, "checkpoint_path": "data/checkpoints", "wal_channel_capacity": 10000, "wal_batch_size": 100, "wal_batch_window_ms": 100, "shard_count": 64, "snapshot_every_n_events": 1000, "snapshot_interval_secs": 30 },
        "ingest": { "cf_poll_interval_secs": 900, "scrapling_url": "http://localhost", "scrapling_port": 8765 },
        "detector": { "phoenix_window_days": 180, "phoenix_min_value": 10000.0 }
    });
    serde_json::from_value(json).expect("test config")
}

#[tokio::test]
async fn toy_analysis_chain_matches_manifest_over_http() {
    if !urban_integrity_test_support::db_tests_enabled() {
        eprintln!("SKIP: toy_analysis_chain (set URBAN_DB_TESTS=1)");
        return;
    }
    let ns = urban_integrity_test_support::NsGuard::with_prefix("test_toy_chain");
    let mut cfg = test_config(ns.name());

    // 1. create --fixture
    let wards = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(wards.path(), format!("{WARD_CODE}\n")).unwrap();
    create_analysis(
        &cfg,
        CreateArgs {
            label: "Planted fixture".into(),
            wards_file: wards.path().to_path_buf(),
            from: None,
            boundary_vintage: "unknown".into(),
            fixture: true,
        },
    )
    .await
    .expect("create");
    let catalog = registry::connect_catalog(&cfg).await.unwrap();
    let mut rows = registry::list_analyses(&catalog).await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a fresh namespace's catalog must hold exactly the analysis this test just created"
    );
    let row = rows.remove(0);
    assert!(
        row.fixture,
        "create_analysis must have recorded the --fixture flag"
    );
    cfg.analysis = Some(row.db_name.clone());

    // 2. seed
    let db = db::connect_db(&cfg).await.unwrap();
    let g = planted_graph();
    let (nodes, edges) = urban_integrity_test_support::events::seed_planted(&db, &g)
        .await
        .unwrap();
    assert_eq!(
        (nodes, edges),
        (g.manifest.seeded_nodes, g.manifest.seeded_edges)
    );

    // 3. probe: the planted disqualified officer must surface, and the evidence row naming it
    //    must carry a pivotable ref to it — not merely mention it in prose.
    let result = urban_integrity_probe::run::execute(
        &db,
        catalog.clone(),
        row.db_name.clone(),
        urban_integrity_probe::run::OcProbeGates::default(),
        urban_integrity_probe::run::RunOptions { full_battery: true },
    )
    .await
    .unwrap();
    let risk = result
        .entries
        .iter()
        .find(|e| e.finding.probe == "osint-risk-edges")
        .expect("osint-risk-edges must be in the battery");
    assert!(
        matches!(
            risk.finding.verdict,
            urban_integrity_schema::types::verdict::Verdict::Hit { .. }
        ),
        "the planted disqualified-officer edge must make osint-risk-edges a Hit, got {:?}",
        risk.finding.verdict
    );
    assert!(
        risk.finding
            .evidence
            .iter()
            .any(|r| r.refs.iter().any(|x| x.id == OD)),
        "osint-risk-edges must carry a RecordRef to {OD}, got {:?}",
        risk.finding.evidence
    );

    // 4. canonical population equals manifest
    let population =
        match canonical_population_summary(&cfg, &row.db_name, Some(&[WARD_CODE.to_string()]))
            .await
            .unwrap()
        {
            CanonicalPopulation::Available {
                count,
                generated_at,
                ward_seed_count,
            } => {
                assert_eq!(count as usize, g.manifest.reachable_nodes);
                PopulationSummary::Available {
                    count,
                    generated_at,
                    ward_seed_count,
                }
            }
            CanonicalPopulation::Unavailable { reason, .. } => {
                panic!("population unavailable: {reason}")
            }
        };

    // 5. boot the workbench on an ephemeral port
    let listener = bind_listener("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let params = BootParams {
        config: cfg.clone(),
        db: Arc::new(db.clone()),
        db_name: row.db_name.clone(),
        population,
        replayed_at: chrono::Utc::now(),
        analyses: vec![AnalysisCatalogEntry {
            db_name: row.db_name.clone(),
            label: row.label.clone(),
        }],
        catalog,
        oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
        analysis_ward_codes: vec![WARD_CODE.to_string()],
    };
    tokio::spawn(async move { boot(params, listener).await.unwrap() });

    let client = reqwest::Client::new();
    let meta_url = format!("http://{addr}/api/graph/population/meta");
    let mut meta = None;
    for _ in 0..120 {
        if let Ok(resp) = client.get(&meta_url).send().await {
            if resp.status() == 200 {
                let v: serde_json::Value = resp.json().await.unwrap();
                if v["count"].as_u64().is_some() {
                    meta = Some(v);
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let meta = meta.expect("population meta never became ready");
    assert_eq!(
        meta["count"].as_u64().unwrap() as usize,
        g.manifest.reachable_nodes
    );

    // 6. one path over HTTP, hop for hop against the manifest
    let expected = &g.manifest.shortest_paths[0];
    let req = AlgorithmRequest {
        algorithm: "shortest_paths".into(),
        session_id: "chain".into(),
        edge_types_included: expected
            .edge_types_included
            .iter()
            .map(|s| s.to_string())
            .collect(),
        seed: 42,
        from_id: Some(expected.from.into()),
        to_id: Some(expected.to.into()),
        ward_id: None,
        ids: vec![],
        scope_ids: vec![],
        hop_cap: 4,
        result_cap: 100,
        budget_secs: 5,
        k: 1,
        top_n: 50,
        min_community_size: 5,
        resolution: 1.0,
    };
    let wire: AlgorithmResult = client
        .post(format!("http://{addr}/api/graph/algorithm"))
        .json(&req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let AlgorithmOutcome::ShortestPaths { paths, .. } = wire.outcome else {
        panic!("shortest_paths must produce a ShortestPaths outcome")
    };
    let ids: Vec<String> = paths[0].hops.iter().map(|h| h.entity.id.clone()).collect();
    assert_eq!(ids, expected.node_ids);

    let _keep = &ns;
}
