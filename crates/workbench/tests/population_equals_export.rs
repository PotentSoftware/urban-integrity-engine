//! DB-gated proof of GRA-06 success criterion 5: the SET of entity ids
//! `crates/workbench`'s own [`population_scope`](urban_integrity_workbench::graph::population_scope)
//! walk produces equals the SET `export-graphml --reachable-from-wards` produces, for the same
//! seeded fixture analysis.
//!
//! Two INDEPENDENT implementations of the same ward-reachability walk (mirroring this project's
//! own ORC-06 precedent — two independent implementations of the same population count, checked
//! against each other): (a) this crate's `population_scope::find_ward_seeds`/`build` against a
//! freshly replayed `GraphStorage`/`CsrSnapshot`; (b) `urban-integrity-cli export-graphml
//! --reachable-from-wards`, run as a real SUBPROCESS (`cargo run -p urban-integrity-cli`) against
//! the SAME namespace/database — never a Cargo dependency edge in either direction (this crate's
//! `Cargo.toml` carries no `urban-integrity-cli` entry, dev or otherwise; T-44-02-03).

use std::collections::HashSet;

use urban_integrity_core::graph::snapshot::bake_csr;
use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_core::persistence::replay_from_surreal;
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_test_support::{db_tests_enabled, TestNs};
use urban_integrity_workbench::graph::population_scope;

/// The fixture's ward code — arbitrary, not one of `config.toml`'s configured wards (this test
/// passes `--reachable-from-wards` explicit codes, which override config entirely per
/// `effective_ward_codes`'s own precedence rule, so nothing here depends on what `config.toml`
/// happens to name).
const FIXTURE_WARD_CODE: &str = "X05009901";

/// Resolves the workspace root two levels up from this crate's manifest dir
/// (`<repo>/crates/workbench` -> `<repo>`), canonicalised and asserted to actually contain the
/// root `Cargo.toml` — so a future crate-move breaks this test loudly rather than silently
/// invoking `cargo run` from the wrong directory.
fn workspace_root() -> std::path::PathBuf {
    let candidate = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let canonical = candidate
        .canonicalize()
        .expect("crates/workbench -> workspace root must resolve to a real directory");
    assert!(
        canonical.join("Cargo.toml").is_file(),
        "workspace_root() resolved to {canonical:?}, which has no root Cargo.toml — \
         crates/workbench must have moved; fix this test's directory arithmetic"
    );
    canonical
}

/// Seeds a small `graph_events` fixture directly into `db`: one `Ward` node (the seed), a node
/// reachable via a single hop from the ward, a node reachable only two hops away (via the
/// one-hop node — mirrors `export_graphml.rs`'s own multi-hop fixture shape), and a node with
/// genuinely no path to the ward at all.
async fn seed_fixture(db: &surrealdb::Surreal<surrealdb::engine::any::Any>) {
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight, WardData};

    let ward_weight = serde_json::to_string(&NodeWeight::Ward(WardData {
        name: "Population-Equals-Export Test Ward".to_string(),
    }))
    .expect("WardData serialises");

    let entity_weight = |name: &str| {
        serde_json::to_string(&NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        }))
        .expect("EntityData serialises")
    };

    let ward_entity_id = format!("ward:{FIXTURE_WARD_CODE}");
    let one_hop_id = "GB-COH:pop-eq-export-one-hop";
    let two_hop_id = "GB-COH:pop-eq-export-two-hop";
    let isolated_id = "GB-COH:pop-eq-export-isolated";

    let one_hop_weight = entity_weight("One Hop Co");
    let two_hop_weight = entity_weight("Two Hop Co");
    let isolated_weight = entity_weight("Isolated Co");

    for (event_id, entity_id, weight_json) in [
        (
            "pop-eq-export-ward",
            ward_entity_id.as_str(),
            ward_weight.as_str(),
        ),
        ("pop-eq-export-one-hop", one_hop_id, one_hop_weight.as_str()),
        ("pop-eq-export-two-hop", two_hop_id, two_hop_weight.as_str()),
        (
            "pop-eq-export-isolated",
            isolated_id,
            isolated_weight.as_str(),
        ),
    ] {
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeAdded",
                    "payload": {
                        "entity_id": entity_id,
                        "weight_json": weight_json,
                    },
                }),
            ))
            .await
            .expect("failed to seed NodeAdded row");
    }

    let make_edge = || {
        serde_json::to_string(&EdgeWeight::Directs {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: DirectsData {
                details: "test".to_string(),
                officer_role: None,
            },
        })
        .expect("Directs edge serialises")
    };

    for (event_id, src, dst) in [
        (
            "pop-eq-export-edge-ward-onehop",
            ward_entity_id.as_str(),
            one_hop_id,
        ),
        ("pop-eq-export-edge-onehop-twohop", one_hop_id, two_hop_id),
    ] {
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_001_000u64,
                    "kind": "EdgeAdded",
                    "payload": {
                        "src_id": src,
                        "dst_id": dst,
                        "weight_json": make_edge(),
                    },
                }),
            ))
            .await
            .expect("failed to seed EdgeAdded row");
    }

    // `isolated_id` deliberately receives no edge at all — genuinely unreachable from the ward.
}

/// Path (a): replay the fixture into a fresh `GraphStorage`, bake a `CsrSnapshot`, then run this
/// crate's own `population_scope::find_ward_seeds`/`build` against the known ward code. Returns
/// the resulting node ids as `EntityId` strings (never raw CSR indices), so the comparison at the
/// call site is over the same domain both paths already speak.
async fn workbench_path_population(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
) -> HashSet<String> {
    let storage = GraphStorage::new();
    let replayed = replay_from_surreal(db, &storage)
        .await
        .expect("replay_from_surreal must succeed against the seeded fixture");

    let mut entries = storage.snapshot_nodes();
    entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
    let entity_ids: Vec<EntityId> = entries.into_iter().map(|(id, _, _, _)| id).collect();

    let snapshot = bake_csr(&storage, replayed as u64);
    let seeds =
        population_scope::find_ward_seeds(&snapshot, &entity_ids, &[FIXTURE_WARD_CODE.to_string()]);
    let scope = population_scope::build(&snapshot, &seeds);

    scope
        .node_ids
        .into_iter()
        .map(|idx| entity_ids[idx as usize].0.clone())
        .collect()
}

/// Path (b): invoke the real `urban-integrity-cli export-graphml --reachable-from-wards` binary
/// as a subprocess against the SAME namespace/database, parse the exported GraphML's `<node
/// id="...">` attributes into a `HashSet<String>`. `cargo run -p` needs no Cargo dependency
/// declaration on either crate — see this file's header doc comment.
fn export_path_population(ns: &str, db_name: &str) -> HashSet<String> {
    let tmp_dir = std::env::temp_dir();
    let output_path = tmp_dir.join(format!(
        "population-equals-export-{}.graphml",
        uuid::Uuid::new_v4()
    ));

    let output = std::process::Command::new("cargo")
        .args([
            "run",
            "-q",
            "-p",
            "urban-integrity-cli",
            "--",
            "export-graphml",
            "--source",
            "events",
            "--reachable-from-wards",
            FIXTURE_WARD_CODE,
            "--output",
            output_path.to_str().expect("output path must be UTF-8"),
        ])
        .current_dir(workspace_root())
        .env(
            "URBAN__SURREAL__WS_URL",
            urban_integrity_test_support::endpoint(),
        )
        .env(
            "URBAN__SURREAL__USERNAME",
            urban_integrity_test_support::credentials().0,
        )
        .env(
            "URBAN__SURREAL__PASSWORD",
            urban_integrity_test_support::credentials().1,
        )
        .env("URBAN__SURREAL__NAMESPACE", ns)
        .env("URBAN__SURREAL__DATABASE", db_name)
        .output()
        .expect("failed to spawn `cargo run -p urban-integrity-cli -- export-graphml`");

    assert!(
        output.status.success(),
        "export-graphml subprocess must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let xml = std::fs::read_to_string(&output_path)
        .unwrap_or_else(|e| panic!("failed to read exported GraphML at {output_path:?}: {e}"));
    let _ = std::fs::remove_file(&output_path);

    parse_node_ids(&xml)
}

/// Minimal `<node id="...">` extractor. This test's fixture entity ids contain no XML-special
/// characters (`&`, `<`, `>`, `"`), so a plain substring scan is sufficient — no XML parser
/// dependency needed for a test-only fixture with a known-safe id alphabet.
fn parse_node_ids(xml: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    for line in xml.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(r#"<node id=""#) {
            if let Some(end) = rest.find('"') {
                ids.insert(rest[..end].to_string());
            }
        }
    }
    ids
}

/// GRA-06 success criterion 5: the two independent ward-reachability implementations must agree
/// on the exact SET of entity ids for the same seeded fixture — the ward, its one-hop neighbour,
/// and the two-hop-away node (reachable only via an intermediate hop), but NOT the isolated node.
#[tokio::test]
async fn workbench_population_scope_matches_export_graphml_reachable_from_wards() {
    if !db_tests_enabled() {
        eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
        return;
    }

    let fixture = TestNs::connect_with_prefix("test_pop_eq_export", "main").await;
    seed_fixture(&fixture).await;

    let workbench_population = workbench_path_population(&fixture).await;
    let export_population = export_path_population(fixture.ns(), "main");

    assert_eq!(
        workbench_population, export_population,
        "population_scope's own walk must produce the exact same entity-id set as \
         export-graphml --reachable-from-wards for the same seeded fixture"
    );

    // Non-vacuity: both sets must actually contain the expected three reachable nodes (never an
    // accidental pass from two independently-empty sets).
    let expected: HashSet<String> = [
        format!("ward:{FIXTURE_WARD_CODE}"),
        "GB-COH:pop-eq-export-one-hop".to_string(),
        "GB-COH:pop-eq-export-two-hop".to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        workbench_population, expected,
        "expected exactly the ward seed, its one-hop neighbour and the two-hop node — never the \
         isolated node, never an empty set"
    );
}
