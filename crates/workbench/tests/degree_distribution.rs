//! D-15's committed, re-derivable degree-distribution measurement (roadmap open question 2,
//! answered by `workbench-dto::caps::DEGREE_CAP_PERCENTILE`, Phase 50 plan 03).
//!
//! Prints (never asserts a fixed number — the research notes' own recommendation, since the whole
//! point is re-derivability as the fixture or the live corpus changes) the mean/max/p50/p75/p90/
//! p95/p99/p99.5/p99.9 degree of the WHOLE node population, under two edge-type filters: every
//! known `EdgeWeight` variant, and
//! [`urban_integrity_workbench::graph::csr_view::DEFAULT_EDGE_TYPES_FOR_DEGREE_CAP`]'s eight —
//! the same filter the degree cap itself is measured under (D-14).
//!
//! Two halves:
//! 1. [`planted_fixture_degree_distribution`] — no DB, always runs, over the planted manifest.
//! 2. [`live_corpus_degree_distribution`] — gated behind `URBAN_DB_TESTS=1`, over whichever
//!    analysis `config.toml`'s `[surreal]` section names (the REAL analysis — deliberately not
//!    a `TestNs` throwaway namespace, since the measurement's whole purpose is the actual
//!    corpus). Skips with a message when that analysis is empty, as it is in CI.
//!
//! Re-run this file (`URBAN_DB_TESTS=1 cargo test -p urban-integrity-workbench --test
//! degree_distribution -- --nocapture`) and update the phase 50 assumptions notes if the corpus has grown
//! materially since the recorded date — never hand-edit that file to a new guess.

use std::collections::HashSet;

use urban_integrity_core::config::AppConfig;
use urban_integrity_core::graph::snapshot::{bake_csr, CsrSnapshot};
use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_core::persistence::replay_from_surreal;
use urban_integrity_test_support::db_tests_enabled;
use urban_integrity_test_support::planted::planted_graph;
use urban_integrity_workbench::graph::csr_view::{self, DEFAULT_EDGE_TYPES_FOR_DEGREE_CAP};
use urban_integrity_workbench::graph::fixture::workbench_from_graph;
use urban_integrity_workbench_dto::caps::DEGREE_CAP_PERCENTILE;

/// Every known `EdgeWeight` display name (`csr_view::edge_type_name`'s exhaustive 12) — the
/// "all edge types" filter this tool measures alongside the degree-cap's own 8-type filter.
const ALL_EDGE_TYPES: [&str; 12] = [
    "Directs",
    "LocatedAt",
    "AwardedTo",
    "IssuedBy",
    "Represents",
    "Controls",
    "TrusteeOf",
    "OwnsProperty",
    "OffshoreLink",
    "DonatesTo",
    "DisqualifiedFrom",
    "DeclaresInterest",
];

/// Mean/max/percentile summary of one degree distribution, over one edge-type filter.
#[derive(Debug)]
struct DegreeTable {
    n: usize,
    mean: f64,
    max: u32,
    p50: u32,
    p75: u32,
    p90: u32,
    p95: u32,
    p99: u32,
    p99_5: u32,
    p99_9: u32,
}

/// Builds the scoped adjacency once, then derives every summary statistic from the same sorted
/// degree list — never rebuilding the adjacency per percentile (`degree_percentile_cutoff`,
/// which this tool otherwise reuses, is one-percentile-at-a-time by design; this measurement
/// needs nine numbers over the same distribution).
fn degree_table(
    snapshot: &CsrSnapshot,
    scope: &HashSet<u32>,
    included: &HashSet<String>,
) -> DegreeTable {
    let adjacency = csr_view::build_scoped_adjacency(snapshot, scope, included);
    let mut degrees: Vec<u32> = scope
        .iter()
        .map(|&idx| adjacency.neighbors(idx).len() as u32)
        .collect();
    degrees.sort_unstable();

    let n = degrees.len();
    let percentile = |p: f64| -> u32 {
        if degrees.is_empty() {
            return 0;
        }
        let rank = ((p / 100.0) * (n - 1) as f64).round() as usize;
        degrees[rank.min(n - 1)]
    };
    let mean = if degrees.is_empty() {
        0.0
    } else {
        degrees.iter().map(|&d| d as f64).sum::<f64>() / n as f64
    };

    DegreeTable {
        n,
        mean,
        max: degrees.last().copied().unwrap_or(0),
        p50: percentile(50.0),
        p75: percentile(75.0),
        p90: percentile(90.0),
        p95: percentile(95.0),
        p99: percentile(99.0),
        p99_5: percentile(99.5),
        p99_9: percentile(99.9),
    }
}

fn print_table(label: &str, filter_name: &str, t: &DegreeTable) {
    eprintln!(
        "[degree_distribution] {label} / {filter_name}: n={} mean={:.2} p50={} p75={} p90={} \
         p95={} p99={} p99.5={} p99.9={} max={} (DEGREE_CAP_PERCENTILE={DEGREE_CAP_PERCENTILE})",
        t.n, t.mean, t.p50, t.p75, t.p90, t.p95, t.p99, t.p99_5, t.p99_9, t.max
    );
}

/// Prints both filters' tables for `label` over `snapshot`'s WHOLE node population (every CSR
/// index `0..node_count`, never a ward-scoped subset — D-15 measures the population the degree
/// cap is applied to, and the cap itself is population-wide, not per-neighbourhood).
fn print_both_filters(label: &str, snapshot: &CsrSnapshot, node_count: usize) {
    let scope: HashSet<u32> = (0..node_count as u32).collect();
    let all_types: HashSet<String> = ALL_EDGE_TYPES.iter().map(|s| s.to_string()).collect();
    let degree_cap_types: HashSet<String> = DEFAULT_EDGE_TYPES_FOR_DEGREE_CAP
        .iter()
        .map(|s| s.to_string())
        .collect();

    let all = degree_table(snapshot, &scope, &all_types);
    print_table(label, "all 12 edge types", &all);
    assert!(all.p50 <= all.p99, "{label}: p50 must not exceed p99");
    assert!(all.p99 <= all.max, "{label}: p99 must not exceed max");
    assert!(all.n >= 1, "{label}: scope must contain at least one node");

    let capped = degree_table(snapshot, &scope, &degree_cap_types);
    print_table(label, "DEFAULT_EDGE_TYPES_FOR_DEGREE_CAP (8)", &capped);
    assert!(
        capped.p50 <= capped.p99,
        "{label}: p50 must not exceed p99 (8-type filter)"
    );
    assert!(
        capped.p99 <= capped.max,
        "{label}: p99 must not exceed max (8-type filter)"
    );
    assert!(
        capped.n >= 1,
        "{label}: scope must contain at least one node (8-type filter)"
    );
}

/// No DB, always runs. Over the planted manifest's FULL node population — `workbench_from_graph`
/// with an empty ward-codes list scopes the resulting `population_scope` to every node, which is
/// what this measurement needs (never the ward-reachable subset the golden tests use).
#[tokio::test]
async fn planted_fixture_degree_distribution() {
    let g = planted_graph();
    let edges: Vec<_> = g
        .edges
        .iter()
        .map(|e| (e.src.clone(), e.dst.clone(), e.weight.clone()))
        .collect();
    let (handle, _state) = workbench_from_graph(&g.nodes, &edges, &[]).await;

    let snapshot = handle.snapshot.load();
    let node_count = snapshot.graph.node_count();
    print_both_filters("planted fixture", &snapshot, node_count);
}

/// Cargo test binaries run with cwd = this crate's package directory
/// (`crates/workbench`), but `AppConfig::load()` reads `config.toml` relative to the workspace
/// root — same fix `crates/core/src/config.rs`'s `test_load_default_config` and
/// `crates/probe/src/donation_contract_correlation.rs`'s `ensure_cwd_is_workspace_root` both use.
/// Resolved from the compile-time `CARGO_MANIFEST_DIR`, not a relative climb off the process's
/// current cwd, so it stays correct regardless of call order or thread-shared cwd mutation.
fn ensure_cwd_is_workspace_root() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::set_current_dir(&workspace_root).ok();
}

/// Gated behind `URBAN_DB_TESTS=1`. Connects to whichever analysis `config.toml`'s `[surreal]`
/// section names — deliberately NOT a `TestNs` throwaway namespace, since D-15's whole point is
/// measuring the actual corpus. Before trusting this run's numbers, confirm (docs/TESTING.md's
/// discriminating checks) that `systemctl --user show urban-surreal -p WorkingDirectory` is the
/// repo root and `INFO FOR ROOT` lists `urban_integrity` — a gated run finishing in seconds is
/// the failure signal, not a fast success.
#[tokio::test]
async fn live_corpus_degree_distribution() {
    if !db_tests_enabled() {
        eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
        return;
    }

    ensure_cwd_is_workspace_root();
    let config = AppConfig::load().expect("AppConfig::load() must succeed against config.toml");

    let http_url = config
        .surreal
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");
    let db: surrealdb::Surreal<surrealdb::engine::any::Any> =
        surrealdb::engine::any::connect(http_url.as_str())
            .await
            .expect("SurrealDB connect failed");
    if let (Some(user), Some(pass)) = (
        config.surreal.username.as_deref(),
        config.surreal.password.as_deref(),
    ) {
        db.signin(surrealdb::opt::auth::Root {
            username: user.to_string(),
            password: pass.to_string(),
        })
        .await
        .expect("SurrealDB sign-in failed");
    }
    db.use_ns(&config.surreal.namespace)
        .use_db(&config.surreal.database)
        .await
        .expect("SurrealDB namespace/database selection failed");

    eprintln!(
        "[degree_distribution] live corpus: ns={} db={}",
        config.surreal.namespace, config.surreal.database
    );

    let storage = GraphStorage::new();
    let replayed = replay_from_surreal(&db, &storage)
        .await
        .expect("replay_from_surreal must succeed against the configured analysis");
    let snapshot = bake_csr(&storage, replayed as u64);
    let node_count = snapshot.graph.node_count();

    // CI's SurrealDB is a fresh instance with no corpus loaded, so the configured analysis
    // replays to an empty graph. There is nothing to measure there; say so instead of failing.
    if node_count == 0 {
        eprintln!(
            "[skip] live corpus: ns={} db={} replayed 0 nodes — no corpus to measure",
            config.surreal.namespace, config.surreal.database
        );
        return;
    }

    print_both_filters("live corpus", &snapshot, node_count);
}
