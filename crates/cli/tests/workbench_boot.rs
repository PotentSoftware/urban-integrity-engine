//! Non-DB-content, process-level tests for `urban-integrity-cli workbench` (WBX-01).
//!
//! These spawn the compiled CLI binary as a real subprocess — proving, at the process level
//! rather than by code review of `crates/workbench::boot`, that `workbench` never logs any of
//! `serve`'s ingestor-start lines, and that a taken `--bind` address exits non-zero naming the
//! address (D-05).
//!
//! Both tests still need a reachable, schema-initialised SurrealDB for `boot()`'s replay step to
//! get past connection/replay and into the code path actually being tested — if `connect_db`
//! fails first, the log-absence assertion would pass for the wrong reason (never got far enough
//! to have a chance of logging those lines). Guarded by the same `URBAN_DB_TESTS=1` + live-TCP
//! pre-flight `crates/cli/tests/cli.rs` uses, via `urban_integrity_test_support::db_tests_enabled`.
//! Each test runs its spawned process against its own throwaway `NsGuard` namespace — never the
//! live `an_anglesey_aebaa785` or any other real analysis — passed via
//! `URBAN__SURREAL__NAMESPACE`/`URBAN__SURREAL__DATABASE` env vars, matching `cli.rs`'s
//! `db_cli_cmd_no_stdin_in_ns` convention.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use urban_integrity_test_support::{db_tests_enabled, NsGuard};

/// `serve`-only log-line substrings (the phase 42 research notes' Boot Sequence table / this crate's
/// `crates/cli/src/main.rs`, the exact `tracing::info!` strings each ingestor/detector task logs
/// on start) — `workbench` must never emit any of these, structurally, since `boot()` never
/// constructs the types that would log them.
const SERVE_ONLY_LOG_LINES: &[&str] = &[
    "CH streaming started",
    "Contracts Finder poller started",
    "Planning portal poller started",
    "EnricherEngine background task started",
];

fn cli_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_urban-integrity-cli") {
        return std::path::PathBuf::from(p);
    }
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("urban-integrity-cli")
}

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/cli -> crates
        .unwrap()
        .parent() // crates -> workspace root
        .unwrap()
        .to_path_buf()
}

/// A free `127.0.0.1:<port>` address, reserved-then-released. Small race against whatever binds
/// next (the spawned `workbench` process), acceptable for a test-only port pick — the same
/// pattern `crates/workbench/src/boot.rs`'s own `bind_listener` tests use for ephemeral ports.
fn free_addr() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);
    format!("127.0.0.1:{}", addr.port())
}

/// Poll `addr` with a plain TCP connect until it accepts, or `timeout` elapses.
///
/// This proves the workbench has BOUND its listener — nothing more. `workbench` binds before it
/// connects to SurrealDB, computes the canonical population, or replays the corpus (so a taken
/// port fails in milliseconds, and the printed URL exists for the whole replay), and the kernel
/// completes TCP handshakes into the backlog long before `axum::serve` starts. A test that
/// needs "the workbench is SERVING" must use `wait_for_http_ready` instead.
fn wait_for_listening(addr: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Spawn `urban-integrity-cli workbench --bind <addr>` pointed at an isolated, throwaway
/// namespace/database (never a real analysis), stdin closed (never inherits a real terminal —
/// the same Pitfall-2 fix `cli.rs`'s `purge` tests apply), stdout/stderr piped for later
/// inspection.
fn spawn_workbench(addr: &str, ns: &str, db_name: &str) -> Child {
    Command::new(cli_bin())
        .args(["workbench", "--bind", addr])
        .current_dir(workspace_root())
        .env("URBAN__SURREAL__WS_URL", "ws://localhost:8000")
        .env("URBAN__SURREAL__USERNAME", "root")
        .env("URBAN__SURREAL__PASSWORD", "root")
        .env("URBAN__SURREAL__NAMESPACE", ns)
        .env("URBAN__SURREAL__DATABASE", db_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn urban-integrity-cli workbench")
}

/// Kill `child`, then read whatever stdout/stderr it produced before dying.
fn kill_and_capture(mut child: Child) -> (String, String) {
    let _ = child.kill();
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let _ = child.wait();
    (stdout, stderr)
}

/// WBX-01, process-level: `workbench` must never log any of `serve`'s ingestor-start lines.
///
/// Not a code-review claim — a real spawned process, pointed at a reachable (if empty)
/// SurrealDB namespace so it actually gets past `connect_db`/replay and into the boot sequence
/// under test, is given a bounded window to start, killed, and its captured stdout+stderr is
/// asserted to contain none of `SERVE_ONLY_LOG_LINES`.
#[test]
fn workbench_never_logs_serve_only_ingestor_lines() {
    if !db_tests_enabled() {
        eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
        return;
    }

    let guard = NsGuard::with_prefix("test_wbx01");
    let addr = free_addr();
    let child = spawn_workbench(&addr, guard.name(), "main");

    // The log-absence assertion below is only meaningful if the process genuinely got through
    // connect_db / population / replay and into the serving state — a workbench that died early
    // (bad config, unreachable DB, `_catalog` failure) logs none of the forbidden lines either,
    // and would pass this test for exactly the wrong reason the file header warns about. So
    // wait for a real `/api/workbench-status` 200 and FAIL if it never arrives.
    let ready = wait_for_http_ready(&addr, Duration::from_secs(30));

    let (stdout, stderr) = kill_and_capture(child);
    let combined = format!("{stdout}\n{stderr}");

    if let Err(e) = ready {
        panic!(
            "workbench must reach the serving state before the log-absence check is \
             meaningful: {e}\ncaptured output:\n{combined}"
        );
    }

    for forbidden in SERVE_ONLY_LOG_LINES {
        assert!(
            !combined.contains(forbidden),
            "workbench must never log a serve-only ingestor line, found {forbidden:?} in \
             captured output:\n{combined}"
        );
    }
}

/// D-05, process-level: binding two `workbench` processes to the SAME `--bind` address — the
/// first binds successfully, the second exits non-zero naming both "already in use" and the
/// literal bind address.
#[test]
fn workbench_port_in_use_fails_loud_and_names_the_address() {
    if !db_tests_enabled() {
        eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
        return;
    }

    let guard = NsGuard::with_prefix("test_wbx01_port");
    let addr = free_addr();

    let first = spawn_workbench(&addr, guard.name(), "main");
    let first_up = wait_for_listening(&addr, Duration::from_secs(15));
    assert!(
        first_up,
        "the first workbench process must successfully bind and start listening on {addr}"
    );

    // A second, independent throwaway namespace — this test is proving the PORT conflict, not a
    // namespace conflict, so give the second process its own untouched namespace.
    let guard2 = NsGuard::with_prefix("test_wbx01_port2");
    let second = spawn_workbench(&addr, guard2.name(), "main");
    let second_output = second
        .wait_with_output()
        .expect("failed to wait on the second workbench process");

    assert!(
        !second_output.status.success(),
        "a second workbench process bound to an address already in use must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&second_output.stderr);
    assert!(
        stderr.contains("already in use"),
        "error must say 'already in use': {stderr}"
    );
    assert!(
        stderr.contains(&addr),
        "error must name the exact bind address {addr}: {stderr}"
    );

    let _ = kill_and_capture(first);
}

// ── WBX-02: real replay against a `TestNs`-seeded fixture (plan 42-09) ─────────────────────
//
// Scope note (see this plan's `<action>` text): this test proves boot()'s real
// `replay_from_surreal`/`count_graph_events` calls are wired to `decide_replay_outcome`
// correctly on the happy (replay-succeeds) path — the ONLY path constructible against a live
// SurrealDB without corrupting schema state shared by every other DB-gated test. The three
// failure-path branches (replay failed but genuinely empty / replay failed with existing rows
// / replay AND count both failed) are proven deterministically instead, by
// `crates/workbench/src/boot.rs`'s `decide_replay_outcome_*` unit tests (plain `#[test]`s, no
// live DB needed) — forcing a genuine `replay_from_surreal` query-level failure against a real
// SurrealDB instance is not reliably constructible, so this omission here is deliberate, not an
// oversight.

/// The fixture's ward code. Deliberately NOT one of the repo `config.toml`'s
/// `[[ingest.planning_portals]]` ward codes: the spawned `workbench` seeds its canonical
/// population from the fixture database's OWN `_catalog` row (`register_fixture_analysis`), so
/// these tests must pass unchanged whatever analysis `config.toml` is currently pointed at.
const FIXTURE_WARD_CODE: &str = "W05000001";

/// Seeds a small, self-contained replay fixture directly into `fixture`'s `graph_events` table,
/// mirroring `crates/core/tests/replay_status_transition.rs`'s exact seeding shape: 2 `Entity`
/// `NodeAdded` rows + 1 `Ward` `NodeAdded` row (`ward:{FIXTURE_WARD_CODE}`, isolated from the
/// two entities) + 1 `Directs` `EdgeAdded` row connecting the two entities. Returns the seeded
/// node/edge counts so the test asserts against the exact numbers seeded here, not a hardcoded
/// duplicate.
async fn seed_replay_fixture(fixture: &urban_integrity_test_support::TestNs) -> (u64, u64) {
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight, WardData};

    let ward_weight = serde_json::to_string(&NodeWeight::Ward(WardData {
        name: "WBX-02 Test Ward".to_string(),
    }))
    .expect("WardData serialises");
    let src_weight = serde_json::to_string(&NodeWeight::Entity(EntityData {
        name: "WBX-02 Source Co".to_string(),
        company_number: None,
        status: EntityStatus::Active,
        incorporation_date: None,
    }))
    .expect("EntityData serialises");
    let dst_weight = serde_json::to_string(&NodeWeight::Entity(EntityData {
        name: "WBX-02 Dest Co".to_string(),
        company_number: None,
        status: EntityStatus::Active,
        incorporation_date: None,
    }))
    .expect("EntityData serialises");

    let ward_entity_id = format!("ward:{FIXTURE_WARD_CODE}");
    let nodes: [(&str, &str, &str); 3] = [
        ("wbx02-ward", ward_entity_id.as_str(), ward_weight.as_str()),
        ("wbx02-src", "GB-COH:WBX02SRC", src_weight.as_str()),
        ("wbx02-dst", "GB-COH:WBX02DST", dst_weight.as_str()),
    ];
    for (event_id, entity_id, weight_json) in nodes {
        fixture
            .query("CREATE graph_events CONTENT $r")
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

    let edge = EdgeWeight::Directs {
        meta: EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        },
        data: DirectsData {
            details: String::new(),
            officer_role: None,
        },
    };
    let edge_weight_json = serde_json::to_string(&edge).expect("Directs edge serialises");
    fixture
        .query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": "wbx02-edge",
                "timestamp": 1_700_000_001_000u64,
                "kind": "EdgeAdded",
                "payload": {
                    "src_id": "GB-COH:WBX02SRC",
                    "dst_id": "GB-COH:WBX02DST",
                    "weight_json": edge_weight_json,
                },
            }),
        ))
        .await
        .expect("failed to seed EdgeAdded row");

    (nodes.len() as u64, 1)
}

/// Registers `db_name` in the test namespace's `_catalog` (the same registry `workbench` reads
/// via `list_analyses`) with `ward_codes` as its OWN ward set, so the spawned process seeds its
/// canonical population from that row — never from whatever `config.toml` happens to name.
/// Lives in the same throwaway namespace as the fixture, so `TestNs`'s `Drop` removes it too.
async fn register_fixture_analysis(ns: &str, db_name: &str, ward_codes: &[&str]) {
    use surrealdb::opt::auth::Root;
    use urban_integrity_cli::registry::{insert_analysis, AnalysisRow};

    let (username, password) = urban_integrity_test_support::credentials();
    let catalog = surrealdb::engine::any::connect(urban_integrity_test_support::endpoint())
        .await
        .expect("connect to SurrealDB for the _catalog fixture");
    catalog
        .signin(Root { username, password })
        .await
        .expect("sign in for the _catalog fixture");
    catalog
        .use_ns(ns)
        .use_db("_catalog")
        .await
        .expect("select the test namespace's _catalog");
    catalog
        .query(urban_integrity_schema::db::CATALOG_SCHEMA)
        .await
        .expect("apply CATALOG_SCHEMA");

    let row = AnalysisRow {
        db_name: db_name.to_string(),
        label: "WBX-02 replay fixture".to_string(),
        ward_codes: ward_codes.iter().map(|c| c.to_string()).collect(),
        wardset_hash: "wbx02-fixture".to_string(),
        baseline_source: None,
        baseline_vintage: None,
        boundary_vintage: None,
        schema_version: 1,
        checkpoint_path: String::new(),
        status: "active".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        retired_at: None,
        notes: None,
        coverage_baseline_json: None,
        fixture: false,
    };
    insert_analysis(&catalog, &row)
        .await
        .expect("insert the fixture analysis into _catalog");
}

/// Poll `GET http://{addr}/api/workbench-status` until it returns a `200` JSON body or
/// `timeout` elapses. This is the ONLY reliable "the workbench is serving" signal: the listener
/// is bound before connect/population/replay (see `wait_for_listening`), so a TCP connect
/// succeeds minutes before the router exists on a real analysis. A request sent to the bound
/// but not-yet-served socket simply waits in the backlog until `axum::serve` starts, then gets
/// its response — so a single request here can block for the whole replay; `reqwest` is given
/// a per-request timeout so the loop keeps re-checking `deadline` instead.
async fn poll_workbench_status(addr: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let deadline = Instant::now() + timeout;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build a reqwest client");
    let url = format!("http://{addr}/api/workbench-status");
    let mut last_err = String::new();
    while Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return resp
                    .json::<serde_json::Value>()
                    .await
                    .map_err(|e| format!("bad JSON body: {e}"));
            }
            Ok(resp) => last_err = format!("HTTP {}", resp.status()),
            Err(e) => last_err = e.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("timed out polling {url}: {last_err}"))
}

/// Synchronous wrapper over `poll_workbench_status` for the plain `#[test]`s in this file: the
/// first successful `/api/workbench-status` body, or the last error once `timeout` elapses.
fn wait_for_http_ready(addr: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let rt = tokio::runtime::Runtime::new().expect("build a tokio runtime for HTTP polling");
    rt.block_on(poll_workbench_status(addr, timeout))
}

/// WBX-02, DB-gated: `boot()`'s real `replay_from_surreal`/`count_graph_events` calls are
/// wired to `decide_replay_outcome` correctly on the happy path — a `TestNs`-seeded fixture is
/// genuinely replayed into the coordinator, and `/api/workbench-status`'s `node_count`/
/// `edge_count` reflect exactly what was seeded, not a hardcoded or stale value.
///
/// Re-running this test a second time (no manual cleanup between runs) must still pass — that
/// confirms `TestNs`'s `Drop` genuinely removed its namespace after the first run (the
/// namespace-hygiene rule in docs/TESTING.md, verified by non-recurrence rather than assumed).
#[test]
fn workbench_replay_guard_starts_with_real_replayed_data() {
    if !db_tests_enabled() {
        eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
        return;
    }

    let rt = tokio::runtime::Runtime::new().expect("build a tokio runtime for seeding/polling");

    let fixture = rt.block_on(urban_integrity_test_support::TestNs::connect(
        "workbench_replay_guard",
    ));
    let (expected_nodes, expected_edges) = rt.block_on(seed_replay_fixture(&fixture));
    rt.block_on(register_fixture_analysis(
        fixture.ns(),
        "workbench_replay_guard",
        &[FIXTURE_WARD_CODE],
    ));

    let addr = free_addr();
    let child = spawn_workbench(&addr, fixture.ns(), "workbench_replay_guard");

    // The listener is bound before replay, so a TCP connect proves nothing about the router —
    // wait for a real `/api/workbench-status` 200 (the only "serving" signal) instead.
    let status_result = wait_for_http_ready(&addr, Duration::from_secs(30));
    let (stdout, stderr) = kill_and_capture(child);

    let status = status_result.unwrap_or_else(|e| {
        panic!(
            "workbench must bind, replay and serve /api/workbench-status on {addr}: \
             {e}\nstdout: {stdout}\nstderr: {stderr}"
        )
    });

    assert_eq!(
        status["node_count"].as_u64(),
        Some(expected_nodes),
        "node_count must match the {expected_nodes} seeded NodeAdded rows: {status}"
    );
    assert_eq!(
        status["edge_count"].as_u64(),
        Some(expected_edges),
        "edge_count must match the {expected_edges} seeded EdgeAdded row(s): {status}"
    );

    // The canonical population is seeded from the fixture's `_catalog` ward set, not
    // `config.toml`: the seeded ward is isolated from the two entities, so the reachable
    // component is the ward alone — exactly 1, not the whole graph (3) and not "unavailable".
    assert_eq!(
        status["population"]["count"].as_u64(),
        Some(1),
        "population must be the component reachable from the registry-row ward (the isolated \
         ward alone), proving the seed came from _catalog rather than config.toml: {status}"
    );
    assert!(
        status["population"]["unavailable_reason"].is_null(),
        "population must not be unavailable when the registry ward resolves: {status}"
    );
    assert!(
        status["population"]["generated_at"].is_string(),
        "generated_at must always travel with the population: {status}"
    );

    drop(fixture);
}
