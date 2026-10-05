//! EnricherEngine integration tests — covers all code paths in engine.rs.
//!
//! All tests in this file require a live SurrealDB instance reachable at
//! http://localhost:8000. They are gated behind the URBAN_DB_TESTS env var —
//! when unset, every test returns early (skip-by-return pattern) with no panic.
//!
//! Run with: URBAN_DB_TESTS=1 cargo test -p urban-integrity-enricher --test engine_coverage

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc;
use urban_integrity_core::config::{EnricherConfig, SurrealConfig};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_core::persistence::detector_runs::list_detector_runs;
use urban_integrity_enricher::adapters::{EnricherAdapter, EnrichmentStats};
use urban_integrity_enricher::EnricherEngine;
use urban_integrity_schema::nation::{Jurisdiction, Nation};

/// Whether the DB-gated tests in this file should run.
///
/// This was `const URBAN_DB_TESTS: bool = option_env!("URBAN_DB_TESTS").is_some()`, which is
/// wrong in both directions. `option_env!` is expanded at COMPILE time, so the value is baked
/// into the test binary by whichever build produced it: a `target/` built without the variable
/// keeps it `false` even when CI exports `URBAN_DB_TESTS=1`, and every DB test here skips while
/// the summary still reads `N passed`. It also enabled on ANY value, so `URBAN_DB_TESTS=0` ran
/// them while every sibling file skipped. CI exists so the gated suite cannot silently rot.
fn db_tests_enabled() -> bool {
    std::env::var("URBAN_DB_TESTS").as_deref() == Ok("1")
}

/// SurrealConfig pointing at the local test instance.
fn test_surreal_config() -> SurrealConfig {
    SurrealConfig {
        ws_url: "ws://localhost:8000".to_string(),
        namespace: "urban".to_string(),
        database: "integrity".to_string(),
        username: Some("root".to_string()),
        password: Some("root".to_string()),
    }
}

/// Connect to the local SurrealDB instance. Panics with a clear message on failure.
async fn connect_db() -> urban_integrity_test_support::TestNs {
    urban_integrity_test_support::TestNs::connect("integrity").await
}

// ---------------------------------------------------------------------------
// StubAdapter — a test double that returns a fixed result or an error path.
// ---------------------------------------------------------------------------

/// Test double adapter that returns a fixed EnrichmentStats (or an error if result is None).
struct StubAdapter {
    name: &'static str,
    enabled: bool,
    /// None triggers the error path; Some(stats) returns Ok(stats).
    result: Option<EnrichmentStats>,
}

#[async_trait]
impl EnricherAdapter for StubAdapter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn run(
        &self,
        _db: &Surreal<Any>,
        _mutation_tx: &mpsc::Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        self.result
            .clone()
            .map(Ok)
            .unwrap_or_else(|| Err(anyhow::anyhow!("stub error")))
    }
}

// ---------------------------------------------------------------------------
// FlagAdapter — sets an AtomicBool when run() is called (for start_background).
// ---------------------------------------------------------------------------

/// Test double that records whether run() was called via a shared AtomicBool.
struct FlagAdapter {
    flag: Arc<AtomicBool>,
}

#[async_trait]
impl EnricherAdapter for FlagAdapter {
    fn name(&self) -> &'static str {
        "flag_adapter"
    }

    fn is_enabled(&self) -> bool {
        true
    }

    async fn run(
        &self,
        _db: &Surreal<Any>,
        _mutation_tx: &mpsc::Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        self.flag.store(true, Ordering::SeqCst);
        Ok(EnrichmentStats::default())
    }
}

/// Test double declaring England-and-Wales coverage, recording whether it was run.
struct EnglandAndWalesAdapter {
    ran: Arc<AtomicBool>,
}

#[async_trait]
impl EnricherAdapter for EnglandAndWalesAdapter {
    fn name(&self) -> &'static str {
        "stub_england_and_wales"
    }

    fn is_enabled(&self) -> bool {
        true
    }

    fn coverage(&self) -> Jurisdiction {
        Jurisdiction::ENGLAND_AND_WALES
    }

    async fn run(
        &self,
        _db: &Surreal<Any>,
        _tx: &tokio::sync::mpsc::Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        self.ran.store(true, Ordering::SeqCst);
        Ok(EnrichmentStats {
            candidates_found: 99,
            ..Default::default()
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// An England-and-Wales source must not run against a Scottish analysis — and the skip
/// must be visible as its own count, not as "ran, found nothing". CCOD is why: its row
/// filter has no geography test, so running it here yields real English property
/// presented as this analysis's own holdings.
#[tokio::test]
async fn test_engine_skips_adapter_out_of_jurisdiction() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);
    let ran = Arc::new(AtomicBool::new(false));
    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        vec![Box::new(EnglandAndWalesAdapter {
            ran: Arc::clone(&ran),
        })],
        Some(Nation::Scotland),
    );

    let stats = engine.run_all_enabled().await.expect("run_all_enabled");
    assert!(!ran.load(Ordering::SeqCst), "adapter must not have run");
    assert_eq!(
        stats.candidates_found, 0,
        "no results from a skipped adapter"
    );
    assert_eq!(stats.skipped_out_of_jurisdiction, 1);
}

/// The same adapter in a Welsh analysis runs normally — the gate narrows nothing that
/// was previously correct.
#[tokio::test]
async fn test_engine_runs_adapter_in_jurisdiction() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);
    let ran = Arc::new(AtomicBool::new(false));
    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        vec![Box::new(EnglandAndWalesAdapter {
            ran: Arc::clone(&ran),
        })],
        Some(Nation::Wales),
    );

    let stats = engine.run_all_enabled().await.expect("run_all_enabled");
    assert!(ran.load(Ordering::SeqCst));
    assert_eq!(stats.candidates_found, 99);
    assert_eq!(stats.skipped_out_of_jurisdiction, 0);
}

/// `enrich --source land_registry_ccod` is the same hazard by a different entry point,
/// so `run_source` gates too.
#[tokio::test]
async fn test_engine_run_source_refuses_out_of_jurisdiction() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);
    let ran = Arc::new(AtomicBool::new(false));
    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        vec![Box::new(EnglandAndWalesAdapter {
            ran: Arc::clone(&ran),
        })],
        Some(Nation::NorthernIreland),
    );

    let stats = engine
        .run_source("stub_england_and_wales")
        .await
        .expect("run_source");
    assert!(!ran.load(Ordering::SeqCst));
    assert_eq!(stats.skipped_out_of_jurisdiction, 1);
}

/// Covers: EnricherEngine::new() and run_all_enabled() empty-loop path.
#[tokio::test]
async fn test_engine_new_and_run_all_no_adapters() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);
    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        vec![],
        None,
    );

    let stats = engine
        .run_all_enabled()
        .await
        .expect("run_all_enabled should succeed");
    assert_eq!(stats.candidates_found, 0);
    assert_eq!(stats.auto_linked, 0);
    assert_eq!(stats.pending_review, 0);
    assert_eq!(stats.discarded, 0);
}

/// 46-05 Task 2: `run_all_enabled` mints one run_id per invocation and records exactly one
/// `detector_runs` history row (`pipeline == "enricher"`) tagging all 5 post-adapter motif
/// types — regardless of how many of the 5 detectors actually wrote an alert this run (the
/// history row records what COULD have fired, not what did; TestNs::connect() gives this
/// test its own fresh namespace, so the count below is exact, not "at least one").
#[tokio::test]
async fn run_all_enabled_records_one_history_row() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);
    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        vec![],
        None,
    );

    engine
        .run_all_enabled()
        .await
        .expect("run_all_enabled should succeed");

    let runs = list_detector_runs(&db)
        .await
        .expect("list_detector_runs must not error");
    let enricher_runs: Vec<_> = runs.iter().filter(|r| r.pipeline == "enricher").collect();
    assert_eq!(
        enricher_runs.len(),
        1,
        "run_all_enabled must record exactly one enricher-pipeline history row per invocation, \
         got: {enricher_runs:?}"
    );
    let run = enricher_runs[0];
    for wire_str in [
        "property_accumulation_anomaly",
        "disqualified_director_evasion",
        "offshore_shadow",
        "charity_commercial_crossover",
        "donation_contract_correlation",
    ] {
        assert!(
            run.motif_types.iter().any(|m| m == wire_str),
            "history row must tag {wire_str}, got: {:?}",
            run.motif_types
        );
    }
    assert_eq!(run.motif_types.len(), 5);

    // Two invocations must mint two DIFFERENT run ids, not a fixed/cached value.
    engine
        .run_all_enabled()
        .await
        .expect("second run_all_enabled should succeed");
    let runs_after_second = list_detector_runs(&db)
        .await
        .expect("list_detector_runs must not error");
    let enricher_runs_after_second: Vec<_> = runs_after_second
        .iter()
        .filter(|r| r.pipeline == "enricher")
        .collect();
    assert_eq!(
        enricher_runs_after_second.len(),
        2,
        "a second invocation must append a second history row, not overwrite the first"
    );
    assert_ne!(
        enricher_runs_after_second[0].run_id, enricher_runs_after_second[1].run_id,
        "each invocation must mint a fresh run_id"
    );
}

/// Covers: is_enabled() == false branch in run_all_enabled.
#[tokio::test]
async fn test_engine_run_all_enabled_skips_disabled() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);

    let adapters: Vec<Box<dyn EnricherAdapter>> = vec![
        Box::new(StubAdapter {
            name: "disabled_adapter",
            enabled: false,
            result: Some(EnrichmentStats {
                candidates_found: 99,
                auto_linked: 99,
                pending_review: 99,
                sub_candidate: 0,
                discarded: 99,
                skipped_out_of_jurisdiction: 0,
            }),
        }),
        Box::new(StubAdapter {
            name: "enabled_adapter",
            enabled: true,
            result: Some(EnrichmentStats {
                candidates_found: 3,
                auto_linked: 1,
                pending_review: 1,
                sub_candidate: 0,
                discarded: 1,
                skipped_out_of_jurisdiction: 0,
            }),
        }),
    ];

    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        adapters,
        None,
    );
    let stats = engine
        .run_all_enabled()
        .await
        .expect("run_all_enabled should succeed");

    // Only the enabled adapter contributes.
    assert_eq!(stats.candidates_found, 3);
    assert_eq!(stats.auto_linked, 1);
    assert_eq!(stats.pending_review, 1);
    assert_eq!(stats.discarded, 1);
}

/// Covers: Err(e) => warn! branch in run_all_enabled — error is swallowed and loop continues.
#[tokio::test]
async fn test_engine_run_all_continues_on_adapter_error() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);

    let adapters: Vec<Box<dyn EnricherAdapter>> = vec![
        Box::new(StubAdapter {
            name: "failing_adapter",
            enabled: true,
            result: None, // None triggers the error path
        }),
        Box::new(StubAdapter {
            name: "succeeding_adapter",
            enabled: true,
            result: Some(EnrichmentStats {
                candidates_found: 2,
                auto_linked: 1,
                pending_review: 0,
                sub_candidate: 0,
                discarded: 1,
                skipped_out_of_jurisdiction: 0,
            }),
        }),
    ];

    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        adapters,
        None,
    );
    let stats = engine
        .run_all_enabled()
        .await
        .expect("run_all_enabled should succeed despite adapter error");

    // First adapter error is swallowed; only second adapter contributes.
    assert_eq!(stats.candidates_found, 2);
    assert_eq!(stats.auto_linked, 1);
    assert_eq!(stats.pending_review, 0);
    assert_eq!(stats.discarded, 1);
}

/// Covers: run_source() happy path — adapter found and enabled.
#[tokio::test]
async fn test_engine_run_source_found_and_enabled() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);

    let adapters: Vec<Box<dyn EnricherAdapter>> = vec![Box::new(StubAdapter {
        name: "test_source",
        enabled: true,
        result: Some(EnrichmentStats {
            candidates_found: 5,
            auto_linked: 2,
            pending_review: 2,
            sub_candidate: 0,
            discarded: 1,
            skipped_out_of_jurisdiction: 0,
        }),
    })];

    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        adapters,
        None,
    );
    let stats = engine
        .run_source("test_source")
        .await
        .expect("run_source should succeed");

    assert_eq!(stats.candidates_found, 5);
    assert_eq!(stats.auto_linked, 2);
    assert_eq!(stats.pending_review, 2);
    assert_eq!(stats.discarded, 1);
}

/// Covers: run_source() with adapter found but NOT enabled — returns empty stats.
#[tokio::test]
async fn test_engine_run_source_found_but_disabled() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);

    let adapters: Vec<Box<dyn EnricherAdapter>> = vec![Box::new(StubAdapter {
        name: "test_source",
        enabled: false,
        result: Some(EnrichmentStats {
            candidates_found: 99,
            auto_linked: 99,
            pending_review: 99,
            sub_candidate: 0,
            discarded: 99,
            skipped_out_of_jurisdiction: 0,
        }),
    })];

    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        adapters,
        None,
    );
    let stats = engine
        .run_source("test_source")
        .await
        .expect("run_source should succeed");

    // Disabled path returns empty stats.
    assert_eq!(stats.candidates_found, 0);
    assert_eq!(stats.auto_linked, 0);
    assert_eq!(stats.pending_review, 0);
    assert_eq!(stats.discarded, 0);
}

/// Covers: run_source() warn! not-found path — adapter not registered.
#[tokio::test]
async fn test_engine_run_source_not_found() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);

    let adapters: Vec<Box<dyn EnricherAdapter>> = vec![Box::new(StubAdapter {
        name: "other",
        enabled: true,
        result: Some(EnrichmentStats::default()),
    })];

    let engine = EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        adapters,
        None,
    );
    let stats = engine
        .run_source("unknown_source")
        .await
        .expect("run_source should succeed for unknown source");

    // Not-found path returns empty stats.
    assert_eq!(stats.candidates_found, 0);
    assert_eq!(stats.auto_linked, 0);
    assert_eq!(stats.pending_review, 0);
    assert_eq!(stats.discarded, 0);
}

/// Covers: start_background() spawn path and the loop body (sleep-first D-04 pattern).
///
/// Uses a zero-second interval so the loop fires immediately after the first sleep,
/// then asserts the FlagAdapter's AtomicBool was set within 200ms.
#[tokio::test]
async fn test_engine_start_background_fires_after_interval() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);

    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = Arc::clone(&flag);

    let adapters: Vec<Box<dyn EnricherAdapter>> = vec![Box::new(FlagAdapter { flag: flag_clone })];

    // Zero interval: sleep(Duration::from_secs(0)) resolves immediately.
    let config = EnricherConfig {
        enrich_interval_secs: 0,
        ch: Default::default(),
        land_registry: Default::default(),
        ocod: Default::default(),
        opensanctions: Default::default(),
        charity_commission: Default::default(),
        icij: Default::default(),
        electoral_commission: Default::default(),
        council_registers: Default::default(),
        opencorporates: Default::default(),
        london_gazette: Default::default(),
        ppd: Default::default(),
        mutuals: Default::default(),
    };

    let engine = Arc::new(EnricherEngine::new(
        (*db).clone(),
        tx,
        config,
        test_surreal_config(),
        adapters,
        None,
    ));
    let handle = engine.start_background().await;

    // Allow the spawned task time to wake up and call run().
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert!(
        flag.load(Ordering::SeqCst),
        "start_background did not call run() within 200ms — D-04 background loop not firing"
    );

    // Clean up: abort the background task so it does not outlive this test.
    handle.abort();
}

// ---------------------------------------------------------------------------
// Regression tests for the session-expiry bugs (2026-06-07)
// ---------------------------------------------------------------------------

/// Regression: engine.reauth() must refresh the engine's own DB session.
///
/// Previously, the periodic reauth task called signin() on a db.clone() (a
/// new independent session), leaving the engine's self.db with the expired
/// token. engine.reauth() now calls signin() on self.db directly, so this
/// test verifies that it succeeds without error.
#[tokio::test]
async fn test_engine_reauth_refreshes_own_session() {
    if !db_tests_enabled() {
        return;
    }
    let db = connect_db().await;
    let (tx, _rx) = mpsc::channel::<GraphMutation>(16);

    let engine = Arc::new(EnricherEngine::new(
        (*db).clone(),
        tx,
        EnricherConfig::default(),
        test_surreal_config(),
        vec![],
        None,
    ));

    engine
        .reauth()
        .await
        .expect("engine.reauth() must succeed with valid credentials");

    // Verify the session is still functional after reauth.
    let stats = engine
        .run_all_enabled()
        .await
        .expect("run_all_enabled must succeed after reauth");
    assert_eq!(stats.candidates_found, 0);
}

/// Characterises SurrealDB SDK behaviour: invalidating a *clone's* session does NOT
/// tear down the original handle's session — `db_a` keeps working after
/// `db_a.clone().invalidate()`. Verified deterministically in isolation.
///
/// This underpins why `engine.reauth()` must re-authenticate on `self.db` directly
/// rather than on a throwaway clone (see the reauth test above): a clone's
/// auth lifecycle is its own, so refreshing a clone would not refresh the handle the
/// engine actually queries with.
///
/// History: this test is DB-gated and never ran in CI; when first run live it failed,
/// but only inside the full concurrent test binary — the `any` HTTP client is shared
/// process-wide across sibling tests, so a concurrent `invalidate()` elsewhere could
/// drop this handle's auth. The connection setup below is self-contained and the
/// assertion holds deterministically; see VERIFICATION notes for the concurrency caveat.
#[tokio::test]
async fn test_surreal_clone_invalidate_leaves_original_usable() {
    if !db_tests_enabled() {
        return;
    }
    let db_a = urban_integrity_test_support::TestNs::connect("integrity").await;

    // Invalidate a throwaway clone's session. This must stay a `Surreal<Any>`
    // clone (not an `Arc` clone of the guard's handle) — cloning the handle is
    // exactly what the engine does, and it is what this test pins down.
    let db_b = (*db_a).clone();
    db_b.invalidate().await.expect("invalidate should succeed");

    // The original handle's own session is unaffected and still queries successfully.
    let after: surrealdb::Result<Vec<serde_json::Value>> = db_a
        .query("SELECT * FROM graph_events LIMIT 1")
        .await
        .and_then(|mut r| r.take(0));
    assert!(
        after.is_ok(),
        "db_a must remain usable after invalidating a clone — the engine relies on \
         this by reauthing on self.db, not on a clone"
    );
}
