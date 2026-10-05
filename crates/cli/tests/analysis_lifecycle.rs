//! DB-gated integration tests for the analysis lifecycle foundation.
//!
//! Requires a live SurrealDB (server v3, port 8000, root/root) and
//! `URBAN_DB_TESTS=1`. Each test uses a unique namespace so runs never collide.

use urban_integrity_cli::{db, registry};
use urban_integrity_core::config::AppConfig;

fn db_tests_enabled() -> bool {
    std::env::var("URBAN_DB_TESTS").as_deref() == Ok("1")
        && std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Build an AppConfig pointed at an isolated namespace, optional analysis id.
fn test_config(namespace: &str, analysis: Option<&str>) -> AppConfig {
    let json = serde_json::json!({
        "analysis": analysis,
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

/// Reserve a namespace. The returned guard removes it on drop, so callers
/// must bind it (`let ns = unique_ns();`) rather than discard it.
fn unique_ns() -> urban_integrity_test_support::NsGuard {
    urban_integrity_test_support::NsGuard::new()
}

fn write_wards_file(contents: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ui-wards-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("wards.txt");
    std::fs::write(&path, contents).unwrap();
    path
}

fn sample_row(db_name: &str) -> registry::AnalysisRow {
    registry::AnalysisRow {
        db_name: db_name.to_string(),
        label: "Black Country 2026".into(),
        ward_codes: vec!["E05001001".into(), "E05001002".into()],
        wardset_hash: "deadbeefdeadbeef".into(),
        baseline_source: Some("uk_national".into()),
        baseline_vintage: Some("2026-07-24T00:00:00Z".into()),
        boundary_vintage: Some("2024".into()),
        schema_version: 1,
        checkpoint_path: "data/analyses/an_x/checkpoints".into(),
        status: "active".into(),
        created_at: "2026-07-24T12:00:00Z".into(),
        retired_at: None,
        notes: None,
        coverage_baseline_json: None,
        fixture: false,
    }
}

#[tokio::test]
async fn registry_insert_get_list_roundtrip() {
    if !db_tests_enabled() {
        return;
    }
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);
    let cat = registry::connect_catalog(&cfg).await.unwrap();

    let row = sample_row("an_black_country_deadbeef");
    registry::insert_analysis(&cat, &row).await.unwrap();

    let got = registry::get_analysis(&cat, "an_black_country_deadbeef")
        .await
        .unwrap();
    let got = got.expect("row present");
    assert_eq!(got.label, "Black Country 2026");
    assert_eq!(got.ward_codes.len(), 2);
    assert_eq!(got.status, "active");

    let all = registry::list_analyses(&cat).await.unwrap();
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn registry_insert_duplicate_db_name_fails() {
    if !db_tests_enabled() {
        return;
    }
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);
    let cat = registry::connect_catalog(&cfg).await.unwrap();
    let row = sample_row("an_dupe_cafebabe");
    registry::insert_analysis(&cat, &row).await.unwrap();
    let second = registry::insert_analysis(&cat, &row).await;
    assert!(second.is_err(), "duplicate db_name must be rejected");
}

#[tokio::test]
async fn resolve_main_ignores_registry() {
    if !db_tests_enabled() {
        return;
    }
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);
    let resolved = db::resolve_analysis_db(&cfg).await.unwrap();
    assert_eq!(resolved, "main");
}

#[tokio::test]
async fn resolve_unknown_analysis_errors() {
    if !db_tests_enabled() {
        return;
    }
    let ns = unique_ns();
    let cfg = test_config(ns.name(), Some("an_nonexistent_00000000"));
    let resolved = db::resolve_analysis_db(&cfg).await;
    assert!(resolved.is_err(), "unknown analysis id must error");
}

#[tokio::test]
async fn resolve_known_analysis_returns_db_name() {
    if !db_tests_enabled() {
        return;
    }
    let ns = unique_ns();
    let seed_cfg = test_config(ns.name(), None);
    let cat = registry::connect_catalog(&seed_cfg).await.unwrap();
    registry::insert_analysis(&cat, &sample_row("an_known_12345678"))
        .await
        .unwrap();

    let cfg = test_config(ns.name(), Some("an_known_12345678"));
    let resolved = db::resolve_analysis_db(&cfg).await.unwrap();
    assert_eq!(resolved, "an_known_12345678");
}

/// The `surreal` binary must be present for clone tests. CI installs it via
/// setup-surreal; locally it lives at ~/.surrealdb/surreal.
fn surreal_present() -> bool {
    if let Ok(p) = std::env::var("SURREAL_BIN") {
        return std::path::Path::new(&p).is_file();
    }
    if let Ok(home) = std::env::var("HOME") {
        if std::path::Path::new(&format!("{home}/.surrealdb/surreal")).is_file() {
            return true;
        }
    }
    std::process::Command::new("surreal")
        .arg("version")
        .output()
        .is_ok()
}

#[tokio::test]
async fn clone_baseline_copies_graph_events() {
    if !db_tests_enabled() || !surreal_present() {
        return;
    }
    use urban_integrity_cli::commands::analysis::clone_baseline;

    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);

    // Seed a source DB with two graph_events + one entity via connect_to_db.
    let src = db::connect_to_db(&cfg, "clone_src").await.unwrap();
    src.query(
        "CREATE graph_events CONTENT { kind: 'NodeAdded', payload: { entity_id: 'GB-COH:1' } }",
    )
    .await
    .unwrap()
    .check()
    .unwrap();
    src.query(
        "CREATE graph_events CONTENT { kind: 'NodeAdded', payload: { entity_id: 'GB-COH:2' } }",
    )
    .await
    .unwrap()
    .check()
    .unwrap();
    src.query("CREATE entity:seed SET name = 'Seed Co', entity_id = 'GB-COH:seed'")
        .await
        .unwrap()
        .check()
        .unwrap();

    clone_baseline(&cfg, "clone_src", "clone_dst")
        .await
        .unwrap();

    // Destination must have identical counts.
    let dst = db::connect_to_db(&cfg, "clone_dst").await.unwrap();
    let mut r = dst
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await
        .unwrap();
    let counts: Vec<serde_json::Value> = r.take(0).unwrap();
    let ge = counts
        .first()
        .and_then(|v| v.get("count"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert_eq!(ge, 2, "cloned graph_events count must match source");

    let mut r2 = dst
        .query("SELECT count() FROM entity GROUP ALL")
        .await
        .unwrap();
    let counts2: Vec<serde_json::Value> = r2.take(0).unwrap();
    let ec = counts2
        .first()
        .and_then(|v| v.get("count"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert_eq!(ec, 1, "cloned entity count must match source");
}

#[tokio::test]
async fn create_empty_analysis_provisions_and_lists() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{create_analysis, CreateArgs};

    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);

    let dir = std::env::temp_dir().join(format!("ui-wards-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let wards = dir.join("wards.txt");
    std::fs::write(&wards, "E05001001\nE05001002\n").unwrap();

    create_analysis(
        &cfg,
        CreateArgs {
            label: "Black Country 2026".into(),
            wards_file: wards,
            from: None,
            boundary_vintage: "2024".into(),
            fixture: false,
        },
    )
    .await
    .expect("create empty analysis");

    let cat = urban_integrity_cli::registry::connect_catalog(&cfg)
        .await
        .unwrap();
    let all = urban_integrity_cli::registry::list_analyses(&cat)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, "active");
    assert!(all[0].db_name.starts_with("an_black_country_2026_"));
    assert!(all[0].baseline_source.is_none());
    assert_eq!(all[0].ward_codes.len(), 2);
}

#[tokio::test]
async fn create_rejects_malformed_ward_code() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{create_analysis, CreateArgs};

    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);

    let dir = std::env::temp_dir().join(format!("ui-wards-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let wards = dir.join("bad.txt");
    std::fs::write(&wards, "NOTAWARD\n").unwrap();

    let result = create_analysis(
        &cfg,
        CreateArgs {
            label: "Bad".into(),
            wards_file: wards,
            from: None,
            boundary_vintage: "2024".into(),
            fixture: false,
        },
    )
    .await;
    assert!(
        result.is_err(),
        "malformed ward code must be rejected before DB work"
    );
}

#[tokio::test]
async fn create_from_missing_baseline_errors() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{create_analysis, CreateArgs};

    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);

    let dir = std::env::temp_dir().join(format!("ui-wards-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let wards = dir.join("wards.txt");
    std::fs::write(&wards, "E05001001\n").unwrap();

    // Baseline DB does not exist in this fresh namespace → must error.
    let result = create_analysis(
        &cfg,
        CreateArgs {
            label: "No Baseline".into(),
            wards_file: wards,
            from: Some("uk_national".into()),
            boundary_vintage: "2024".into(),
            fixture: false,
        },
    )
    .await;
    assert!(
        result.is_err(),
        "missing --from baseline must error before provisioning"
    );

    // And no active row should have been written.
    let cat = urban_integrity_cli::registry::connect_catalog(&cfg)
        .await
        .unwrap();
    let all = urban_integrity_cli::registry::list_analyses(&cat)
        .await
        .unwrap();
    assert!(
        all.iter().all(|r| r.status != "active"),
        "no active row after failed baseline check"
    );
}

#[tokio::test]
async fn create_from_baseline_clones_and_selection_isolates() {
    if !db_tests_enabled() || !surreal_present() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{create_analysis, CreateArgs};

    let ns = unique_ns();
    let base_cfg = test_config(ns.name(), None);

    // Stand up a baseline DB "uk_national" with one graph_event.
    let base = db::connect_to_db(&base_cfg, "uk_national").await.unwrap();
    base.query(
        "CREATE graph_events CONTENT { kind: 'NodeAdded', payload: { entity_id: 'GB-COH:BASE' } }",
    )
    .await
    .unwrap()
    .check()
    .unwrap();

    // Create an analysis cloning that baseline.
    let dir = std::env::temp_dir().join(format!("ui-wards-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let wards = dir.join("wards.txt");
    std::fs::write(&wards, "E05009001\n").unwrap();

    create_analysis(
        &base_cfg,
        CreateArgs {
            label: "Clone Target".into(),
            wards_file: wards,
            from: Some("uk_national".into()),
            boundary_vintage: "2024".into(),
            fixture: false,
        },
    )
    .await
    .expect("create --from clone");

    // Find the provisioned db_name.
    let cat = urban_integrity_cli::registry::connect_catalog(&base_cfg)
        .await
        .unwrap();
    let rows = urban_integrity_cli::registry::list_analyses(&cat)
        .await
        .unwrap();
    let clone = rows
        .iter()
        .find(|r| r.label == "Clone Target")
        .expect("clone row");
    assert_eq!(clone.baseline_source.as_deref(), Some("uk_national"));
    assert_eq!(clone.status, "active");

    // Selection: connect_db for that analysis must see the cloned baseline event.
    let sel_cfg = test_config(ns.name(), Some(&clone.db_name));
    let selected = db::connect_db(&sel_cfg).await.unwrap();
    let mut r = selected
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await
        .unwrap();
    let counts: Vec<serde_json::Value> = r.take(0).unwrap();
    let ge = counts
        .first()
        .and_then(|v| v.get("count"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert_eq!(ge, 1, "clone must carry the baseline graph_events");

    // Isolation: writing to the clone is invisible to the baseline.
    selected.query("CREATE graph_events CONTENT { kind: 'NodeAdded', payload: { entity_id: 'GB-COH:CLONE_ONLY' } }")
        .await.unwrap().check().unwrap();
    let base2 = db::connect_to_db(&base_cfg, "uk_national").await.unwrap();
    let mut rb = base2
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await
        .unwrap();
    let cb: Vec<serde_json::Value> = rb.take(0).unwrap();
    let base_ge = cb
        .first()
        .and_then(|v| v.get("count"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert_eq!(
        base_ge, 1,
        "baseline must NOT see the clone's later write (isolation)"
    );
}

#[tokio::test]
async fn registry_mark_retired_sets_status_and_timestamp() {
    if !db_tests_enabled() {
        return;
    }
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);
    let cat = registry::connect_catalog(&cfg).await.unwrap();

    registry::insert_analysis(&cat, &sample_row("an_retire_me_00000000"))
        .await
        .unwrap();
    registry::mark_retired(&cat, "an_retire_me_00000000", "2026-07-25T09:00:00Z")
        .await
        .unwrap();

    let row = registry::get_analysis(&cat, "an_retire_me_00000000")
        .await
        .unwrap()
        .expect("row");
    assert_eq!(row.status, "retired");
    assert_eq!(row.retired_at.as_deref(), Some("2026-07-25T09:00:00Z"));
}

#[tokio::test]
async fn remove_dry_run_does_not_drop() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{
        create_analysis, remove_analysis, CreateArgs, RemoveArgs,
    };
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);

    create_analysis(
        &cfg,
        CreateArgs {
            label: "Dry Run".into(),
            wards_file: write_wards_file("E05001001\n"),
            from: None,
            boundary_vintage: "2024".into(),
            fixture: false,
        },
    )
    .await
    .unwrap();
    let cat = registry::connect_catalog(&cfg).await.unwrap();
    let row = registry::list_analyses(&cat)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    remove_analysis(
        &cfg,
        RemoveArgs {
            db_name: row.db_name.clone(),
            apply: false,
            decommission: false,
        },
    )
    .await
    .expect("dry run ok");

    // DB still exists; registry still active.
    assert!(
        urban_integrity_cli::commands::analysis::database_exists(&cat, &row.db_name)
            .await
            .unwrap(),
        "dry run must not drop the database"
    );
    let after = registry::get_analysis(&cat, &row.db_name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, "active");
}

#[tokio::test]
async fn remove_apply_out_of_window_drops_and_retires() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{
        create_analysis, remove_analysis, CreateArgs, RemoveArgs,
    };
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);

    create_analysis(
        &cfg,
        CreateArgs {
            label: "Old".into(),
            wards_file: write_wards_file("E05002002\n"),
            from: None,
            boundary_vintage: "2024".into(),
            fixture: false,
        },
    )
    .await
    .unwrap();
    let cat = registry::connect_catalog(&cfg).await.unwrap();
    let row = registry::list_analyses(&cat)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    // Age the analysis past the 180-day window by rewriting created_at.
    let old = (chrono::Utc::now() - chrono::Duration::days(800)).to_rfc3339();
    cat.query("UPDATE type::record('analyses', $id) SET created_at = $ts")
        .bind(("id", row.db_name.clone()))
        .bind(("ts", old))
        .await
        .unwrap()
        .check()
        .unwrap();

    // Checkpoint dir was created by create_analysis.
    assert!(
        std::path::Path::new(&row.checkpoint_path).exists(),
        "checkpoint dir should exist pre-remove"
    );

    remove_analysis(
        &cfg,
        RemoveArgs {
            db_name: row.db_name.clone(),
            apply: true,
            decommission: false,
        },
    )
    .await
    .expect("apply out-of-window ok");

    // DB gone, checkpoint dir gone, registry retired + retired_at set.
    let cat2 = registry::connect_catalog(&cfg).await.unwrap();
    assert!(
        !urban_integrity_cli::commands::analysis::database_exists(&cat2, &row.db_name)
            .await
            .unwrap(),
        "db should be dropped"
    );
    assert!(
        !std::path::Path::new(&row.checkpoint_path).exists(),
        "checkpoint dir should be deleted"
    );
    let after = registry::get_analysis(&cat2, &row.db_name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, "retired");
    assert!(after.retired_at.is_some());
}

#[tokio::test]
async fn remove_reserved_names_hard_refused() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{remove_analysis, RemoveArgs};
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);
    for name in ["main", "uk_national"] {
        let r = remove_analysis(
            &cfg,
            RemoveArgs {
                db_name: name.into(),
                apply: true,
                decommission: true,
            },
        )
        .await;
        assert!(r.is_err(), "{name} must be hard-refused");
    }
}

#[tokio::test]
async fn remove_in_window_refused_without_decommission() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{
        create_analysis, remove_analysis, CreateArgs, RemoveArgs,
    };
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);
    create_analysis(
        &cfg,
        CreateArgs {
            label: "Fresh".into(),
            wards_file: write_wards_file("E05003003\n"),
            from: None,
            boundary_vintage: "2024".into(),
            fixture: false,
        },
    )
    .await
    .unwrap();
    let cat = registry::connect_catalog(&cfg).await.unwrap();
    let row = registry::list_analyses(&cat)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    let r = remove_analysis(
        &cfg,
        RemoveArgs {
            db_name: row.db_name.clone(),
            apply: true,
            decommission: false,
        },
    )
    .await;
    assert!(
        r.is_err(),
        "in-window remove must be refused without --decommission"
    );
    // Still active, DB intact.
    let after = registry::get_analysis(&cat, &row.db_name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, "active");
    assert!(
        urban_integrity_cli::commands::analysis::database_exists(&cat, &row.db_name)
            .await
            .unwrap(),
        "db must remain"
    );
}

#[tokio::test]
async fn remove_in_window_with_decommission_drops() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{
        create_analysis, remove_analysis, CreateArgs, RemoveArgs,
    };
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);
    create_analysis(
        &cfg,
        CreateArgs {
            label: "Decomm".into(),
            wards_file: write_wards_file("E05004004\n"),
            from: None,
            boundary_vintage: "2024".into(),
            fixture: false,
        },
    )
    .await
    .unwrap();
    let cat = registry::connect_catalog(&cfg).await.unwrap();
    let row = registry::list_analyses(&cat)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    remove_analysis(
        &cfg,
        RemoveArgs {
            db_name: row.db_name.clone(),
            apply: true,
            decommission: true,
        },
    )
    .await
    .expect("decommission drop ok");
    let after = registry::get_analysis(&cat, &row.db_name)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, "retired");
}

#[tokio::test]
async fn remove_unknown_analysis_errors() {
    if !db_tests_enabled() {
        return;
    }
    use urban_integrity_cli::commands::analysis::{remove_analysis, RemoveArgs};
    let ns = unique_ns();
    let cfg = test_config(ns.name(), None);
    let r = remove_analysis(
        &cfg,
        RemoveArgs {
            db_name: "an_ghost_00000000".into(),
            apply: true,
            decommission: false,
        },
    )
    .await;
    assert!(r.is_err(), "unknown analysis must error");
}
