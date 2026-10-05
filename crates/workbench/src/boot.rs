//! `crates/workbench::boot` — the replay-only engine boot sequence (WBX-01, WBX-02, D-06).
//!
//! `boot()` deliberately omits every `serve`-only ingestor/detector task (the CH streaming
//! client, the Contracts Finder poller, the planning portal poller / scraping sidecar, the
//! enricher engine, the motif detector engine, and the WAL → SurrealDB writer loop) — see
//! the phase 42 research notes' "Boot Sequence" table for the exact
//! `tracing::info!` string each one logs on start, used as the structural non-start proof this
//! plan's DB-gated tests (plan 42-09) assert against. The ONE ingestor-adjacent task the
//! workbench keeps is the periodic SurrealDB re-auth — a workbench left open for hours needs it
//! just as much as `serve` does (the research notes' phase-specific finding, not in the milestone
//! research).
//!
//! # Why this crate cannot call `crates/cli::db::{connect_db, resolve_analysis_db, reauth_db}`
//!
//! `crates/cli` depends on `crates/workbench` (to call `boot()` from its own `Commands::Workbench`
//! arm, plan 42-07) — a `workbench -> cli` dependency edge the other way would be circular.
//! `boot()` therefore takes an already-connected `db: Arc<Surreal<Any>>` and the resolved
//! `db_name: String` as [`BootParams`] fields; the CLI arm resolves them itself, exactly as
//! `Commands::Serve` already does. The periodic re-auth logic below is a local reproduction of
//! `crates/cli::db::{reauth_db, connect_to_db}` for the same reason — copied, not called.
//!
//! # Note on this file's WorkbenchState struct
//!
//! [`WorkbenchState`] is defined here (rather than being introduced only once the router
//! assembly that consumes it lands) because `crates/workbench/src/status.rs`'s handlers need
//! this type to shape their `Extension` extractors, and `status.rs` is authored before the
//! router-assembly half of `boot()` that actually wires those handlers in — see this plan's
//! SUMMARY ("Deviations from Plan") for the full ordering rationale.

use std::sync::Arc;
use std::time::Duration;

use axum::routing::{get, post};
use axum::{Extension, Router};
use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_api::graphql::schema::build_schema;
use urban_integrity_api::server::core_router;
use urban_integrity_core::config::AppConfig;
use urban_integrity_core::graph::coordinator::{
    CoordinatorConfig, GraphCoordinator, GraphCoordinatorHandle,
};
use urban_integrity_core::persistence::{count_graph_events, replay_from_surreal, wal_channel};

use crate::{analysis, assets, home, population, status};

/// One entry in the workbench's fixed, boot-time analysis catalog snapshot.
///
/// NOT `urban_integrity_cli::registry::AnalysisRow` — see this module's doc comment for why
/// `crates/workbench` cannot depend on `crates/cli`. Plan 42-07's CLI arm constructs this from
/// its own `AnalysisRow` values before calling [`boot`].
#[derive(Debug, Clone)]
pub struct AnalysisCatalogEntry {
    pub db_name: String,
    pub label: String,
}

/// Everything [`boot`] needs, already resolved by the caller (plan 42-07's CLI arm) — this crate
/// never calls `connect_db`/`resolve_analysis_db`/`list_analyses` itself. The HTTP listener is
/// NOT here: the caller binds it via [`bind_listener`] BEFORE any of these are resolved (so a
/// taken port fails in milliseconds, not after a full corpus replay) and hands it to [`boot`]
/// separately.
pub struct BootParams {
    pub config: AppConfig,
    pub db: Arc<Surreal<Any>>,
    pub db_name: String,
    pub population: PopulationSummary,
    pub replayed_at: chrono::DateTime<chrono::Utc>,
    pub analyses: Vec<AnalysisCatalogEntry>,
    /// The `_catalog` connection — needed only by `IdentityIntegrity` inside the in-process probe
    /// battery (Phase 43 plan 05). `Surreal<Any>` is `Clone` (confirmed via
    /// `crates/cli/src/main.rs`'s existing `db.clone()` on a bare, non-`Arc` `Surreal<Any>`), so
    /// the caller passes one already-connected value and this crate clones it onward as needed.
    pub catalog: Surreal<Any>,
    /// `OpencorporatesCrossJurisdiction`'s three boolean config gates, lifted out of `AppConfig`
    /// exactly as `crates/cli/src/commands/probe.rs::execute` already does via
    /// `OcProbeGates::from_config`.
    pub oc_gates: urban_integrity_probe::run::OcProbeGates,
    /// The target analysis's own resolved ward-seed codes (its `_catalog` row's `ward_codes`,
    /// already computed by the CLI caller via `effective_ward_codes`/`analysis_ward_codes`
    /// before calling this — see `crates/cli/src/commands/workbench.rs::run`). An empty `Vec`
    /// means "no ward-seed codes were resolved for this analysis" (the `main`/uncatalogued path
    /// falling back from `None`), never "fall back to config.toml" — `crates/workbench` never
    /// independently re-resolves `config.toml` wards (plan 44-02, Pitfall 4).
    pub analysis_ward_codes: Vec<String>,
}

/// The canonical population (D-19/WBX-06) as computed by the caller — either a count, or a
/// typed reason it could not be computed (e.g. none of the target analysis's ward codes
/// resolve to a `Ward` node in the loaded graph). Both carry the generation timestamp: the
/// status page renders "unavailable — <reason>" rather than a fabricated `0`, and the
/// workbench still boots. NOT `crates/cli`'s `CanonicalPopulation` — same circular-dependency
/// reason as [`AnalysisCatalogEntry`]; the CLI arm maps one onto the other.
#[derive(Debug, Clone, PartialEq)]
pub enum PopulationSummary {
    Available {
        count: u64,
        generated_at: chrono::DateTime<chrono::Utc>,
        /// Number of ward seeds the reachable-from-wards walk started from (Phase 43 plan 05,
        /// D-20's `PopulationMeta.ward_seed_count`) — travels the same path `count` does from
        /// `crates/cli`'s `CanonicalPopulation`, never re-derived independently in this crate.
        ward_seed_count: u64,
    },
    Unavailable {
        reason: String,
        generated_at: chrono::DateTime<chrono::Utc>,
        ward_seed_count: u64,
    },
}

/// State the workbench's own routes read via `Extension<Arc<WorkbenchState>>` — layered on
/// `workbench_routes` specifically inside `build_router` (added in this plan's router-assembly
/// task), never on `core_router`'s output. See this module's top-level doc comment for why the
/// struct is defined here rather than alongside `build_router`.
pub struct WorkbenchState {
    pub population: PopulationSummary,
    pub replayed_at: chrono::DateTime<chrono::Utc>,
    pub analyses: Vec<AnalysisCatalogEntry>,
    pub db_name: String,
    pub actor: String,
    /// A separate, cheaply-cloned connection to the analysis database for on-demand probe-battery
    /// and last-event queries (Phase 43 plan 05) — distinct from the coordinator's own handle
    /// (`params.db`, already `Arc`-wrapped for exactly this kind of reuse).
    pub db: Arc<Surreal<Any>>,
    /// The `_catalog` connection `IdentityIntegrity` needs inside the in-process battery.
    ///
    /// `Arc`, for the same reason as [`WorkbenchState::db`]: a session, once cloned, is never
    /// reached by [`spawn_periodic_reauth`] again. Callers that need an owned `Surreal<Any>`
    /// (the probe battery's `execute` takes one by value) clone it **per run**, which is safe
    /// precisely because it inherits this session's current, freshly re-authenticated token and
    /// is discarded ~90 seconds later. The fatal clone is the one made once at boot and reused
    /// for hours.
    pub catalog: Arc<Surreal<Any>>,
    pub oc_gates: urban_integrity_probe::run::OcProbeGates,
    /// Per-source ingestor last-event timestamps, computed once at boot (`last_event::compute`)
    /// — mirrors the existing "computed once at boot, cached on `WorkbenchState`" pattern this
    /// struct already uses for `population`.
    pub last_events: crate::last_event::LastEventSummary,
    /// The most recent completed probe battery run, if any — seeded at boot from
    /// `workbench_report:latest` when a prior run persisted successfully (checkpoint fix, Phase
    /// 43 plan 09, amending D-16), else `None` until the first successful `POST
    /// /api/analysis/run` in this process. Replaced, never merged, on every successful run; left
    /// untouched on a failed run (D-19).
    pub report_cache: std::sync::RwLock<Option<CachedReport>>,
    /// The raw `ReportEntry` list from the most recent **in-process** probe run, if any — the
    /// findings half of the entity profile's "Findings & alerts" section (PIV-02, plan 45-05).
    ///
    /// Separate from [`WorkbenchState::report_cache`] and deliberately **not persisted**.
    /// `CachedReport` carries no raw `Finding` on purpose (see its doc comment: `Finding`/`Verdict`
    /// have no `Deserialize`, so a persisted report could never be read back with them), which is
    /// exactly why the entity profile cannot get its findings from there. `None` — the state after
    /// a restart, however many runs preceded it — is not a gap to paper over: it is
    /// `FindingsOutcome::NotSearched`, which 45-05 built the vocabulary to say out loud.
    ///
    /// `Vec<ReportEntry>` rather than `Vec<Finding>` because `FindingRowView` needs a full
    /// `VerdictView`, and `to_verdict_view` needs the `PopulationSource` that lives on the entry
    /// rather than on the finding. `ReportEntry` is not `Clone`, so the battery's entries are
    /// *moved* in here after the Markdown is rendered.
    pub last_findings: std::sync::RwLock<Option<CachedFindings>>,
    /// Single-flight guard for `POST /api/analysis/run` (T-43-05-01): a `compare_exchange` on
    /// this flag rejects any concurrent second run with HTTP 409 before `execute()` is ever
    /// called a second time. Always reset on both the success and failure paths.
    pub report_running: std::sync::atomic::AtomicBool,
    /// The number of `graph_events` replayed into the coordinator at boot (`build_engine`'s own
    /// `replayed` count) — kept here so [`snapshot_pending`] can distinguish "nothing was ever
    /// replayed, an empty graph is correct" from "something was replayed and the CSR bake has
    /// not landed yet" (checkpoint fix item 4). Never re-derived from `handle.snapshot` alone:
    /// an unbaked snapshot and a genuinely-empty-but-baked snapshot both read `baked_at_event ==
    /// 0`, and only this count tells them apart.
    pub replayed_events: usize,
    /// The ward-reachable canonical population (GRA-06, plan 44-02), computed once at boot from
    /// the same already-baked `CsrSnapshot` `handle.snapshot.load()` returns — never re-derived
    /// per request. Every algorithm, the Arrow IPC export and the label index scope to this,
    /// never to the coordinator's full unfiltered snapshot.
    pub population_scope: crate::graph::population_scope::PopulationScope,
    /// The deferred population-scope cell (checkpoint fix, plan 44-14): when the first CSR bake
    /// had NOT landed by the time [`boot`] ran (`snapshot_pending`), `population_scope` above is
    /// an empty placeholder and the real scope is built here, once, by the first request that
    /// arrives after the bake — see [`resolve_population_scope`]. Every consumer goes through
    /// that resolver, never reads `population_scope` directly.
    pub late_scope: crate::graph::population_scope::LateScope,
    /// The boot-time entity typeahead index (NAV-06, plan 44-06), built once from the FULL
    /// replayed graph (never scoped to `population_scope` — see `label_index.rs`'s own doc
    /// comment for why). `GET /api/entities/search` (plan 44-08) queries this, never rebuilding
    /// per request.
    pub label_index: crate::graph::label_index::LabelIndex,
    /// The `POST /api/graph/algorithm` result cache (D-05, GRA-04, plan 44-07), keyed on the full
    /// request parameters plus the population's generation timestamp. Constructed fresh
    /// (`AlgorithmCache::new()`) at boot — no state to seed, unlike `population_scope`/
    /// `label_index`.
    pub graph_cache: crate::graph::cache::AlgorithmCache,
    /// The one-job-per-browser-session cancellation registry (D-05/GRA-06, plan 44-07) every
    /// `POST /api/graph/algorithm`/`POST /api/graph/algorithm/cancel` request reads/writes
    /// (plan 44-09's dispatch layer). Constructed fresh (`SessionRegistry::new()`) at boot.
    pub graph_sessions: crate::graph::session::SessionRegistry,
    /// The degree-cap hub-node-id set (D-14/D-15, Phase 50), resolved lazily and cached for the
    /// rest of this process's life — mirrors [`crate::graph::population_scope::LateScope`]'s
    /// "computed once, cached" contract, via [`resolve_hub_ids`]. Not computed eagerly at boot:
    /// unlike `population_scope`, nothing before the first finding-detail request needs this, and
    /// deferring it means a boot that races the first CSR bake never blocks on a second walk.
    ///
    /// # Staleness contract (WR-03, phase 50 code review)
    ///
    /// The cache never invalidates, and that is only correct because the graph it is measured
    /// over never changes either: this workbench is **replay-only** (see [`build_engine`] — no
    /// mutation source is ever spawned), so the CSR snapshot, `population_scope` and
    /// `label_index` are all fixed at the corpus as replayed at boot, and this set is fixed with
    /// them. The neighbourhood walk it gates, by contrast, reads SurrealDB live. If the analysis
    /// database gains `graph_events` while the workbench runs (an `enrich` or ingest against the
    /// same analysis), the walk sees the new edges but the hub set — like every other boot-time
    /// structure here — does not. **Restart the workbench after changing the corpus it serves**;
    /// nothing in this process will notice the change on its own. A `project` re-run alone does
    /// not change what the workbench replays (it reads `graph_events`, never writes them).
    ///
    /// The cell is only ever filled from the RESOLVED population scope, never from the boot-time
    /// placeholder: [`resolve_hub_ids`] refuses to compute (and so to cache) anything while the
    /// first CSR bake is still pending.
    pub hub_node_ids: std::sync::OnceLock<std::collections::HashSet<String>>,
}

/// The ONE way to obtain the degree-cap hub-node-id set (D-14/D-15, Phase 50) — the CSR-index
/// nodes whose degree, under [`crate::graph::csr_view::DEFAULT_EDGE_TYPES_FOR_DEGREE_CAP`], is
/// strictly above the [`urban_integrity_workbench_dto::caps::DEGREE_CAP_PERCENTILE`] cutoff of
/// the whole population's distribution — resolved lazily against the population scope and cached
/// on `state.hub_node_ids` for the rest of this process's life, mirroring
/// [`resolve_population_scope`]'s existing lazy-`OnceLock` pattern.
///
/// Returns `None` — "not yet", the same answer [`resolve_population_scope`] gives — while the
/// first CSR bake is still pending, and caches nothing in that case. Before WR-03 this read
/// `state.population_scope` directly; on a corpus large enough that the bake lands after boot,
/// that field is an empty placeholder (the real scope lives in `late_scope`), so the first
/// neighbourhood request froze an EMPTY hub set for the process lifetime and T-50-02's hub
/// exclusion silently never applied. See [`WorkbenchState::hub_node_ids`] for the staleness
/// contract this cache relies on.
///
/// First real call site: `crate::entity::neighbourhood::neighbourhood_handler` (plan 50-04) — the
/// interface-first `#[allow(dead_code)]` plan 50-03 left here is removed now that it has one.
pub(crate) fn resolve_hub_ids<'a>(
    handle: &GraphCoordinatorHandle,
    state: &'a WorkbenchState,
) -> Option<&'a std::collections::HashSet<String>> {
    if let Some(hub_ids) = state.hub_node_ids.get() {
        return Some(hub_ids);
    }
    let population = resolve_population_scope(state, handle)?;
    Some(state.hub_node_ids.get_or_init(|| {
        let snapshot = handle.snapshot.load();
        let scope: std::collections::HashSet<u32> = population.node_ids.clone();
        let included: std::collections::HashSet<String> =
            crate::graph::csr_view::DEFAULT_EDGE_TYPES_FOR_DEGREE_CAP
                .iter()
                .map(|s| s.to_string())
                .collect();
        let cutoff = crate::graph::csr_view::degree_percentile_cutoff(
            &snapshot,
            &scope,
            &included,
            urban_integrity_workbench_dto::caps::DEGREE_CAP_PERCENTILE,
        );
        let adjacency =
            crate::graph::csr_view::build_scoped_adjacency(&snapshot, &scope, &included);
        scope
            .iter()
            .filter(|&&idx| adjacency.neighbors(idx).len() as u32 > cutoff)
            .filter_map(|&idx| state.label_index.entity_ref_at(idx).map(|r| r.id))
            .collect()
    }))
}

/// The last completed probe battery run's `ReportView` and the rendered Markdown dossier text
/// (`crates/probe::report::render_evidence_report`'s output, rendered once at run time — never
/// re-rendered from raw `Finding`s on every `GET /api/analysis/report.md`). Held in
/// `WorkbenchState::report_cache` as a single unit so the three analysis endpoints (`report`,
/// `run`, `report.md`) always observe one atomically-replaced snapshot rather than fields that
/// could individually go stale relative to each other.
///
/// Deliberately holds no raw `ReportEntry`/`Strategy` (the pre-checkpoint-fix shape did, so
/// `report_markdown_handler` could re-render on demand) — `Finding`/`Verdict` have no
/// `Deserialize` (`scripts/check-verdict-deserialize.sh` guards this), so a `CachedReport`
/// carrying them could never be persisted to `workbench_report:latest` and reloaded at boot.
/// Rendering the Markdown once at run time and storing the string sidesteps that entirely.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedReport {
    pub view: urban_integrity_workbench_dto::ReportView,
    pub markdown: String,
    pub run_at: chrono::DateTime<chrono::Utc>,
}

/// The last in-process probe run's raw entries and when it ran — see
/// [`WorkbenchState::last_findings`] for why this is separate from [`CachedReport`] and why it is
/// never persisted.
#[derive(Debug)]
pub struct CachedFindings {
    pub entries: Vec<urban_integrity_probe::report::ReportEntry>,
    pub run_at: chrono::DateTime<chrono::Utc>,
}

/// True exactly when a historical replay happened (`state.replayed_events > 0`) but the CSR
/// snapshot bake has not yet landed (`handle.snapshot.load().baked_at_event == 0`) — checkpoint
/// fix item 4. `force_initial_bake`'s 2s poll gives up long before a large corpus's bake actually
/// completes (measured ~4m46s against 865,646 replayed `graph_events` on a large live analysis
/// store), so `/api/workbench-status`/`/api/home` must be able to report this honestly instead of
/// fabricating a `0` node/edge count while the bake is still running in the background.
pub(crate) fn snapshot_pending(state: &WorkbenchState, handle: &GraphCoordinatorHandle) -> bool {
    state.replayed_events > 0 && handle.snapshot.load().baked_at_event == 0
}

/// Walks the coordinator's CURRENT `CsrSnapshot` for the ward-reachable canonical population
/// (GRA-06, plan 44-02). The entity-id-by-CSR-index vector is sorted by `idx.index()`, matching
/// `bake_csr`'s own sort order exactly (petgraph.rs's numerically-identical-index proof). Only
/// meaningful once the bake has landed — callers gate on [`snapshot_pending`] first.
pub(crate) fn build_population_scope(
    handle: &GraphCoordinatorHandle,
    ward_codes: &[String],
) -> crate::graph::population_scope::PopulationScope {
    let mut entries = handle.storage.snapshot_nodes();
    entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
    let entity_ids: Vec<urban_integrity_schema::types::id::EntityId> =
        entries.into_iter().map(|(id, _, _, _)| id).collect();
    let snapshot = handle.snapshot.load();
    let seeds = crate::graph::population_scope::find_ward_seeds(&snapshot, &entity_ids, ward_codes);
    crate::graph::population_scope::build(&snapshot, &seeds)
}

/// The ONE way to obtain the population scope (checkpoint fix, plan 44-14 Task 3).
///
/// Returns the boot-time scope when [`boot`] was able to build it (bake already landed, or
/// nothing was replayed so an empty graph is correct). Otherwise resolves `late_scope` exactly
/// once, on the first call after the bake lands, and returns `None` — "not yet" — while the bake
/// is still pending, so callers answer honestly (`503`/`PopulationUnavailable`) instead of
/// exporting or traversing an empty population.
pub(crate) fn resolve_population_scope<'a>(
    state: &'a WorkbenchState,
    handle: &GraphCoordinatorHandle,
) -> Option<&'a crate::graph::population_scope::PopulationScope> {
    if let Some(scope) = state.late_scope.cell.get() {
        return Some(scope);
    }
    if state.late_scope.ward_codes.is_empty() && !snapshot_pending(state, handle) {
        // Boot built the real scope (or an empty graph is the truth) — nothing deferred.
        if state.replayed_events == 0 || !state.population_scope.node_ids.is_empty() {
            return Some(&state.population_scope);
        }
    }
    if snapshot_pending(state, handle) {
        return None;
    }
    let scope = state
        .late_scope
        .cell
        .get_or_init(|| build_population_scope(handle, &state.late_scope.ward_codes));
    tracing::info!(
        population_nodes = scope.node_ids.len(),
        ward_seed_count = scope.ward_seed_count,
        "workbench: ward-reachable population scope resolved after the CSR bake"
    );
    Some(scope)
}

/// The checkpoint-bake-race fix (the phase 42 research notes' "Checkpoint-trigger finding", not covered by
/// the milestone-level research): `replay_from_surreal` writes straight into the shared
/// `GraphStorage` the coordinator and handle both hold an `Arc` to — it does NOT go through
/// `handle.mutation_tx`, so `coordinator.run()`'s `select!` loop never sees a mutation arrive and
/// will not bake a new CSR snapshot until the `snapshot_interval_secs` timer (default 30s) next
/// fires. Left unhandled, the workbench's first several page loads would show `node_count: 0`
/// racing that timer — directly undermining D-09's "graph as of `<timestamp>`" honesty
/// requirement. DO NOT remove the `checkpoint_tx.send(())` below or reduce it to a fire-and-forget
/// with no poll — both are load-bearing for the first response after boot being accurate.
async fn force_initial_bake(handle: &GraphCoordinatorHandle, replayed: usize) {
    // A rejected send is the ONLY signal that `coordinator.run()` has already exited (the
    // receiver is dropped with it) — nothing else in this function can observe that. Left as
    // `let _ =`, that case fell through to the 2s poll below and was reported as "bake did not
    // land within 2s", misattributing a dead coordinator to a slow bake.
    if let Err(e) = handle.checkpoint_tx.send(()).await {
        tracing::error!(
            error = %e,
            replayed,
            "workbench: checkpoint trigger rejected — the graph coordinator task has already \
             exited, so no snapshot will ever be baked; the status page will keep serving the \
             empty initial snapshot"
        );
        return;
    }
    if replayed == 0 {
        // Nothing was replayed — an empty initial snapshot IS the correct state, no bake to
        // wait for.
        return;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if handle.snapshot.load().baked_at_event > 0 {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                "workbench: checkpoint bake did not land within 2s of replay — on a large \
                 corpus this is the normal shape, not a defect (a full replay's first bake can \
                 take several minutes; 865,646 replayed graph_events measured ~4m46s on a large \
                 live analysis store); the server is starting anyway and \
                 `/api/workbench-status`/`/api/home` will honestly report snapshot_pending \
                 until the bake lands"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The synchronous, DB-free core of the replay-guard's three-way branch (WBX-02).
///
/// Takes the ALREADY-COMPUTED replay result and a lazily-invoked count closure — `count_fn` is
/// never called on the `Ok` fast path, matching `build_engine`'s own short-circuit (a
/// successful replay never pays for a count query). This function is pure and synchronous
/// (plain `#[test]`, no `tokio::test` needed) specifically so its three failure branches — and
/// the fast-path `Ok` short-circuit — are deterministically testable without a live SurrealDB
/// connection, which cannot reliably be made to fail at the query level without corrupting
/// schema state shared by every other DB-gated test.
///
/// - `replay_result` is `Ok(n)`: replay succeeded — start with `n` replayed events, `count_fn`
///   is never invoked.
/// - `replay_result` is `Err` and `count_fn()` proves `graph_events` is genuinely empty (`0`):
///   start empty, logging a warn — an empty graph on an empty table is not a defect.
/// - `replay_result` is `Err` and `count_fn()` proves rows exist: refuse to start — silently
///   starting empty here would disable all historical detection while looking healthy.
/// - `replay_result` is `Err` AND `count_fn()` also fails: cannot prove the table is empty
///   either, so refuse to start rather than vouching for emptiness on exactly the evidence that
///   could not support it.
pub(crate) fn decide_replay_outcome(
    replay_result: Result<usize, String>,
    count_fn: impl FnOnce() -> Result<usize, String>,
) -> Result<usize, String> {
    let replay_err = match replay_result {
        Ok(n) => return Ok(n),
        Err(e) => e,
    };

    match count_fn() {
        Ok(existing) if existing > 0 => Err(format!(
            "workbench: graph replay failed with {existing} graph_events present — refusing \
             to start with an empty graph (historical detection would be silently disabled). \
             Error: {replay_err}"
        )),
        Ok(_) => {
            tracing::warn!(
                "workbench: graph replay failed on an empty graph_events table — starting \
                 empty: {replay_err}"
            );
            Ok(0)
        }
        Err(count_err) => Err(format!(
            "workbench: graph replay failed AND the graph_events count failed — cannot prove \
             the table is empty, refusing to start with an empty graph. Replay error: \
             {replay_err}. Count error: {count_err}"
        )),
    }
}

/// Builds the replay-only `GraphCoordinator`: no detector, no mutation source, and the exact
/// three-way replay guard `Commands::Serve` uses (`crates/cli/src/main.rs:209-239`), copied
/// verbatim per D-06 but returning `Err` instead of `panic!` — `boot()` is a library function
/// whose caller decides how to fail, not a top-level process entry point.
///
/// The plan's own CI grep guard (naming every `serve`-only ingestor/detector construction site
/// this file must never match — see this module's top-level doc comment) must find nothing
/// here: `GraphCoordinator::new_with_db` is called with `None` in the detector slot, and no
/// consumer is spawned for the WAL channel receiver — there is no mutation source in a
/// replay-only boot, so nothing would ever flow through it.
pub(crate) async fn build_engine(
    params: &BootParams,
) -> anyhow::Result<(Arc<GraphCoordinatorHandle>, usize)> {
    let coord_cfg = CoordinatorConfig {
        snapshot_every_n_events: params.config.graph.snapshot_every_n_events,
        snapshot_interval_secs: params.config.graph.snapshot_interval_secs,
    };
    let (wal_tx, _wal_rx) = wal_channel(params.config.graph.wal_channel_capacity);

    let (coordinator, handle) =
        GraphCoordinator::new_with_db(coord_cfg, wal_tx, None, Some(params.db.clone()));
    let handle = Arc::new(handle);

    // A replay failure must never silently masquerade as an empty graph — that disables ALL
    // historical detection while the workbench looks healthy. The three-way decision (replay
    // ok / replay failed but genuinely empty / replay failed and the count also fails) is
    // proven deterministically by `decide_replay_outcome`'s own unit tests (WBX-02), not
    // exercised here. The count is only computed on this Err path — the Ok fast path never
    // pays for it, matching the pre-extraction short-circuit.
    let replay_result = replay_from_surreal(&params.db, &handle.storage)
        .await
        .map_err(|e| e.to_string());
    let replayed = match replay_result {
        Ok(n) => n,
        Err(e) => {
            let count_result = count_graph_events(&params.db)
                .await
                .map_err(|count_err| count_err.to_string());
            decide_replay_outcome(Err(e), || count_result).map_err(|msg| anyhow::anyhow!(msg))?
        }
    };
    tracing::info!(
        replayed,
        "workbench: historical graph_events replayed into coordinator"
    );

    tokio::spawn(coordinator.run());
    force_initial_bake(&handle, replayed).await;

    Ok((handle, replayed))
}

/// Periodic re-auth: belt-and-suspenders against the ~1-hour SurrealDB HTTP session expiry —
/// copied from `Commands::Serve`'s 50-minute-interval block (`crates/cli/src/main.rs:438-486`),
/// the ONE ingestor-adjacent task the workbench keeps (the research notes' Boot Sequence table: a
/// workbench left open for hours needs it too, a phase-specific finding not covered by the
/// milestone research). Cannot call `urban_integrity_cli::db::reauth_db` directly — see this
/// module's top-level doc comment — so the logic is reproduced locally against
/// `params.db`/`params.config`/`params.db_name`.
fn spawn_periodic_reauth(
    db: Arc<Surreal<Any>>,
    catalog: Arc<Surreal<Any>>,
    config: AppConfig,
    db_name: String,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(50 * 60));
        interval.tick().await; // skip the immediate first tick
        loop {
            interval.tick().await;
            // Both handles, every tick. The catalog connection was previously never
            // re-authenticated at all: `IdentityIntegrity`'s `_catalog` coverage baseline read
            // therefore started failing one hour after boot, in the same way and for the same
            // reason as the analysis handle.
            for (handle, name, label) in [
                (&db, db_name.as_str(), "analysis"),
                (&catalog, CATALOG_DB, "catalog"),
            ] {
                match reauth_db(handle, &config, name).await {
                    Ok(()) => tracing::info!(
                        connection = label,
                        "workbench: SurrealDB session refreshed (periodic re-auth)"
                    ),
                    Err(e) => tracing::error!(
                        error = %e,
                        connection = label,
                        "workbench: SurrealDB periodic re-auth FAILED — every query through \
                         this connection will return 401 until a later tick succeeds"
                    ),
                }
            }
        }
    });
}

/// The handle [`WorkbenchState`] hands the probe battery and the report-persistence writer.
///
/// **This MUST share `boot_db`'s session, not copy it.** In `surrealdb 3.1.0-beta.2` every
/// `Surreal<C>` carries its own `session_id` and `Clone` mints a NEW one (`src/lib.rs:334`:
/// `clone_session(old, new)` copies the session, then the two are independent). Authentication
/// is per-session, so a `Surreal::clone` here produces a handle [`spawn_periodic_reauth`]
/// cannot reach — its token expires one hour after boot and every query through it 401s
/// thereafter, while the re-auth task keeps logging success against a session nothing reads.
/// That is exactly what happened live on 2026-09-13; see
/// `reauth_session_tests::periodic_reauth_restores_the_handle_the_probe_battery_actually_queries_through`,
/// which fails if this returns a cloned session.
///
/// `Arc::clone` shares the one `Surreal` value, hence the one session.
pub(crate) fn probe_db_handle(boot_db: &Arc<Surreal<Any>>) -> Arc<Surreal<Any>> {
    Arc::clone(boot_db)
}

/// The `_catalog` database name — the registry `IdentityIntegrity` reads its coverage baseline
/// from. Mirrors `crates/cli::registry::connect_catalog`'s own `use_db("_catalog")`.
const CATALOG_DB: &str = "_catalog";

/// Re-authenticate an existing SurrealDB connection **in place**, so that every holder of this
/// `Surreal` value — which means every holder of an `Arc` of it, and nobody else — gets the new
/// token.
///
/// **There is deliberately no reconnect-and-return fallback.** The previous shape returned
/// `Ok(Some(new_db))` on signin failure, having opened a fresh connection for the caller to swap
/// in. That could not work: the caller's `db` is now shared via `Arc`, so swapping its own
/// binding would leave every other holder — the probe battery, the report writer — pointing at
/// the old, dead session. It would silently re-create the very defect this module now guards
/// against (see [`probe_db_handle`]). A failure here is reported as a failure; the next tick
/// retries against the same connection, and `connect_to_db`'s own retry belongs at boot, not
/// here.
async fn reauth_db(db: &Surreal<Any>, config: &AppConfig, db_name: &str) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let (Some(user), Some(pass)) = (
        config.surreal.username.as_deref(),
        config.surreal.password.as_deref(),
    ) else {
        return Ok(());
    };

    db.signin(surrealdb::opt::auth::Root {
        username: user.to_string(),
        password: pass.to_string(),
    })
    .await
    .context("workbench: SurrealDB re-signin was rejected")?;

    // Signing in resets the connection's namespace/database selection, so both are restored
    // explicitly rather than trusting `config.surreal.database`.
    db.use_ns(&config.surreal.namespace)
        .use_db(db_name)
        .await
        .with_context(|| {
            format!(
                "workbench: SurrealDB namespace/db re-selection failed ({}/{db_name})",
                config.surreal.namespace
            )
        })?;
    Ok(())
}

/// Test-only. Production connection is the CLI arm's job — `boot()` receives an
/// already-connected `params.db` — and the one former production caller, `reauth_db`'s
/// reconnect-and-return fallback, was removed as unsound (it handed back a connection the other
/// `Arc` holders could never see). This survives because the re-auth guard needs a genuinely
/// connected, signed-in handle to invalidate and then recover.
#[cfg(test)]
/// Connect + sign in + select `db_name`, then apply `SCHEMA_DEF` (idempotent). Local
/// reproduction of `crates/cli::db::connect_to_db` — see this module's top-level doc comment.
async fn connect_to_db(config: &AppConfig, db_name: &str) -> anyhow::Result<Surreal<Any>> {
    use anyhow::Context as _;

    let http_url = config
        .surreal
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");

    let db: Surreal<Any> = surrealdb::engine::any::connect(http_url.as_str())
        .await
        .with_context(|| format!("workbench: SurrealDB connect failed: {http_url}"))?;

    if let (Some(user), Some(pass)) = (
        config.surreal.username.as_deref(),
        config.surreal.password.as_deref(),
    ) {
        db.signin(surrealdb::opt::auth::Root {
            username: user.to_string(),
            password: pass.to_string(),
        })
        .await
        .context("workbench: SurrealDB sign-in failed")?;
    }

    db.use_ns(&config.surreal.namespace)
        .use_db(db_name)
        .await
        .with_context(|| {
            format!(
                "workbench: SurrealDB namespace/db selection failed: {}/{}",
                config.surreal.namespace, db_name
            )
        })?;

    db.query(urban_integrity_schema::db::SCHEMA_DEF)
        .await
        .context("workbench: SurrealDB schema initialisation failed")?;

    Ok(db)
}

/// Binds the workbench's HTTP listener, translating a taken port into a named-cause error
/// naming both the address and the remedy (D-05) rather than a bare OS error. Public and
/// separate from [`boot`] so the caller can bind FIRST — before connecting to SurrealDB,
/// computing the canonical population, or replaying the corpus (minutes on a live analysis) —
/// and only then print the URL / open a browser against a socket that already exists. Until
/// [`boot`] reaches `axum::serve`, connections queue in the kernel backlog rather than being
/// refused, so an early browser tab waits instead of erroring. Also testable without a live
/// SurrealDB connection.
pub async fn bind_listener(bind_addr: &str) -> anyhow::Result<tokio::net::TcpListener> {
    match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Err(anyhow::anyhow!(
            "workbench: address {bind_addr} already in use — is another workbench or serve \
             instance already running? (or set [workbench] bind_addr / \
             URBAN__WORKBENCH__BIND_ADDR / --bind to a free port)"
        )),
        Err(e) => Err(anyhow::Error::from(e).context("workbench: failed to bind HTTP listener")),
    }
}

/// Builds the merged `Router`: `core_router`'s `/health` `/metrics` `/graphql` `/graphiql` plus
/// the workbench's own `/api/workbench-status` `/api/analyses`, plus the embedded-asset SPA
/// fallback.
///
/// `workbench_routes` is built as its OWN `Router` with its OWN `.layer(Extension(...))` calls
/// and `.merge()`d onto `core_router`'s output — NEVER `.route()`d directly onto `core_router`'s
/// returned `Router`. `core_router`'s own doc comment states why: Axum 0.8's `Router::layer`
/// does not retroactively apply to routes a caller adds after the function returns, so a
/// `.route()` appended afterward would mean `workbench_routes`' `Extension(handle)`/
/// `Extension(state)` are never applied at all, and the handlers below would fail every request
/// with a missing-extension rejection — exactly the defect class this structure exists to avoid.
pub fn build_router(handle: Arc<GraphCoordinatorHandle>, state: Arc<WorkbenchState>) -> Router {
    let schema = build_schema(handle.clone());

    let workbench_routes = Router::new()
        .route(
            "/api/workbench-status",
            get(status::workbench_status_handler),
        )
        .route("/api/analyses", get(status::analyses_handler))
        .route("/api/home", get(home::home_handler))
        .route(
            "/api/graph/population/meta",
            get(population::population_meta_handler),
        )
        .route("/api/analysis/report", get(analysis::report_handler))
        .route("/api/analysis/run", post(analysis::run_handler))
        .route(
            "/api/analysis/report.md",
            get(analysis::report_markdown_handler),
        )
        .route(
            "/api/graph/population",
            get(crate::graph::handlers::population_handler),
        )
        .route(
            "/api/entities/search",
            get(crate::graph::handlers::entities_search_handler),
        )
        .route(
            "/api/graph/node",
            get(crate::graph::handlers::node_summary_handler),
        )
        .route(
            "/api/graph/algorithm",
            post(crate::graph::handlers::algorithm_handler),
        )
        .route(
            "/api/graph/canvas-filter",
            post(crate::graph::canvas_filter::canvas_filter_handler),
        )
        .route(
            "/api/graph/algorithm/cancel",
            post(crate::graph::handlers::algorithm_cancel_handler),
        )
        .route("/api/entity/profile", get(crate::entity::profile_handler))
        .route(
            "/api/entity/disambiguate",
            get(crate::entity::disambiguate_handler),
        )
        .route(
            "/api/entity/neighbourhood",
            get(crate::entity::neighbourhood::neighbourhood_handler),
        )
        .route(
            "/api/investigations",
            get(crate::investigation::list_handler).post(crate::investigation::create_handler),
        )
        .route(
            "/api/investigation",
            get(crate::investigation::get_handler)
                .patch(crate::investigation::update_handler)
                .delete(crate::investigation::delete_handler),
        )
        .route(
            "/api/investigation/pin",
            post(crate::investigation::pin_handler).patch(crate::investigation::pin_note_handler),
        )
        .route(
            "/api/investigation/subgraph",
            post(crate::investigation::save_subgraph_handler),
        )
        .route(
            "/api/investigation/unpin",
            post(crate::investigation::unpin_handler),
        )
        .route("/api/alerts", get(crate::alerts::list_handler))
        .route("/api/alert/{id}", get(crate::alerts::detail_handler))
        .route(
            "/api/alert/{id}/withdraw",
            post(crate::alerts::withdraw_handler),
        )
        .layer(Extension(handle.clone()))
        .layer(Extension(state));

    core_router(handle, schema)
        .merge(workbench_routes)
        .fallback(assets::static_handler)
}

/// Boots the replay-only workbench engine and serves it on `listener` (WBX-01/WBX-02/D-05/
/// D-06/D-07): builds the engine ([`build_engine`]), assembles the merged router
/// ([`build_router`]), then serves until the process is killed. `listener` comes from
/// [`bind_listener`], which the caller runs BEFORE resolving any [`BootParams`] field — a taken
/// port is a fail-loud, named-cause error (never an auto-picked fallback) that surfaces before
/// a single SurrealDB round-trip, and the printed URL (D-07: URL only, no auto-opened browser
/// by default) points at a socket that already exists for the whole replay.
pub async fn boot(params: BootParams, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let (handle, replayed_events) = build_engine(&params).await?;

    // The ward-reachable canonical population (GRA-06, plan 44-02): built AFTER build_engine
    // returns, since it walks the coordinator's own already-baked CsrSnapshot — never a call
    // into crates/cli (see crate::graph::population_scope's doc comment for why). The
    // entity-id-by-CSR-index vector is built ONCE here, sorted by `idx.index()`, matching
    // `bake_csr`'s own sort order exactly (petgraph.rs's numerically-identical-index proof).
    //
    // Checkpoint fix (plan 44-14 Task 3, 2026-09-08): on a large corpus the first bake lands
    // MINUTES after `force_initial_bake`'s 2s poll gives up, so walking the snapshot here would
    // freeze an empty scope for the process lifetime. When the bake is still pending the scope
    // is deferred to `late_scope` and built by the first request after the bake instead.
    let bake_pending = replayed_events > 0 && handle.snapshot.load().baked_at_event == 0;
    let population_scope = if bake_pending {
        tracing::info!(
            "workbench: CSR bake still pending at boot — the ward-reachable population scope \
             will be resolved by the first Graph-page request after the bake lands"
        );
        crate::graph::population_scope::PopulationScope {
            node_ids: Default::default(),
            ward_seed_count: 0,
        }
    } else {
        build_population_scope(&handle, &params.analysis_ward_codes)
    };
    let late_scope = if bake_pending {
        crate::graph::population_scope::LateScope::new(params.analysis_ward_codes.clone())
    } else {
        crate::graph::population_scope::LateScope::none()
    };

    // The boot-time entity typeahead index (NAV-06, plan 44-06): built once here, over the same
    // already-replayed `handle.storage` — never re-derived per `GET /api/entities/search`
    // request.
    let label_index = crate::graph::label_index::LabelIndex::build(&handle.storage);

    // Computed once at boot, cached on WorkbenchState — mirrors the existing PopulationSummary
    // pattern. A per-request re-query would repeat 6 SurrealDB round-trips (3 prefixes x 2
    // kinds) on every Home page load.
    let last_events = crate::last_event::compute(&params.db).await;

    // Seed `report_cache` from `workbench_report:latest`, if a prior run persisted successfully
    // (checkpoint fix, Phase 43 plan 09, amending D-16) — a read/deserialize failure is WARN-only
    // and starts with `None`, exactly as the "no run yet" state always has; it must never be
    // confused with a genuine absence of any prior run.
    let report_cache = match crate::report_persistence::load_report(&params.db).await {
        Ok(cached) => cached,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "workbench: failed to load workbench_report:latest at boot — starting with no \
                 cached report"
            );
            None
        }
    };

    // `params` is consumed field-by-field building the state below, so the re-auth task's
    // config copy is taken first.
    let config_for_reauth = params.config.clone();
    let state = Arc::new(WorkbenchState {
        population: params.population,
        replayed_at: params.replayed_at,
        analyses: params.analyses,
        db_name: params.db_name,
        actor: "operator".to_string(),
        db: probe_db_handle(&params.db),
        catalog: Arc::new(params.catalog),
        oc_gates: params.oc_gates,
        last_events,
        report_cache: std::sync::RwLock::new(report_cache),
        last_findings: std::sync::RwLock::new(None),
        report_running: std::sync::atomic::AtomicBool::new(false),
        replayed_events,
        late_scope,
        population_scope,
        label_index,
        graph_cache: crate::graph::cache::AlgorithmCache::new(),
        graph_sessions: crate::graph::session::SessionRegistry::new(),
        hub_node_ids: std::sync::OnceLock::new(),
    });

    // Spawned HERE, not inside `build_engine`, so it re-authenticates the very handles the
    // request path uses — `state.db` and `state.catalog` — rather than a separate session no
    // query reads. That distinction is the whole defect: see [`probe_db_handle`].
    spawn_periodic_reauth(
        Arc::clone(&state.db),
        Arc::clone(&state.catalog),
        config_for_reauth,
        state.db_name.clone(),
    );

    let router = build_router(handle, state);
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::graph::types::GraphMutation;
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    // ── decide_replay_outcome (WBX-02): the pure, synchronous three-way branch ─────────────

    /// Replay succeeded: the count closure must never be invoked (the `Ok` fast path is a
    /// zero-cost short-circuit) — asserted here via a closure that panics if called at all.
    #[test]
    fn decide_replay_outcome_ok_never_invokes_the_count_closure() {
        let result = decide_replay_outcome(Ok(42), || unreachable!("count_fn must not be called"));
        assert_eq!(result, Ok(42));
    }

    /// Replay failed, but the count proves `graph_events` is genuinely empty: start empty
    /// rather than refusing — an empty graph on an empty table is not a defect.
    #[test]
    fn decide_replay_outcome_replay_failed_but_table_genuinely_empty_starts_empty() {
        let result = decide_replay_outcome(Err("boom".to_string()), || Ok(0));
        assert_eq!(result, Ok(0));
    }

    /// Replay failed AND existing rows are present: refuse to start — starting empty here
    /// would silently disable all historical detection while the workbench looks healthy. The
    /// error must name both the replay failure and the count that proved rows exist.
    #[test]
    fn decide_replay_outcome_replay_failed_with_existing_rows_refuses_to_start() {
        let result = decide_replay_outcome(Err("boom".to_string()), || Ok(7));
        let err = result.expect_err("must refuse to start when rows exist");
        assert!(err.contains('7'), "error must name the row count: {err}");
        assert!(
            err.contains("boom"),
            "error must name the replay failure: {err}"
        );
    }

    /// Replay failed AND the count ALSO failed: cannot prove the table is empty either, so
    /// refuse to start rather than vouching for emptiness on exactly the evidence that could
    /// not support it. The error must name both failures.
    #[test]
    fn decide_replay_outcome_correlated_failure_refuses_to_start() {
        let result = decide_replay_outcome(Err("boom".to_string()), || {
            Err("count also broken".to_string())
        });
        let err = result.expect_err("must refuse to start when the count also fails");
        assert!(
            err.contains("boom"),
            "error must name the replay failure: {err}"
        );
        assert!(
            err.contains("count also broken"),
            "error must name the count failure: {err}"
        );
    }

    /// Proves the checkpoint-bake-race fix's mechanism directly (Task 1's acceptance
    /// criteria explicitly allow a `TestNs`-free fixture using synthetic mutations instead of
    /// a real SurrealDB replay — the DB-gated replay-failure path is exercised in plan 42-09's
    /// tests, not here). `snapshot_every_n_events`/`snapshot_interval_secs` are set high enough
    /// that neither the count-triggered nor the timer-triggered bake path can fire inside this
    /// test's 2s window — the only thing that can make `baked_at_event` non-zero here is
    /// `force_initial_bake`'s own `checkpoint_tx.send(())`.
    ///
    /// PROVE THIS CAN FAIL (per `docs/DEVELOPMENT.md`): this test was first run against a
    /// version of `force_initial_bake` with the `handle.checkpoint_tx.send(()).await` line
    /// removed — the final assertion failed for real (`baked_at_event` stayed `0` for the
    /// entire 2s poll window, since neither the count nor timer thresholds could fire) — before
    /// the send was restored to the version committed here.
    #[tokio::test]
    async fn force_initial_bake_lands_a_snapshot_within_the_poll_window() {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let coord_cfg = CoordinatorConfig {
            snapshot_every_n_events: 1_000_000,
            snapshot_interval_secs: 30,
        };
        let (coordinator, handle) = GraphCoordinator::new(coord_cfg, wal_tx, None);
        tokio::spawn(coordinator.run());

        let id = EntityId("workbench-boot-test:1".to_string());
        let weight = NodeWeight::Entity(EntityData {
            name: "Test Corp".to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        handle
            .mutation_tx
            .send(GraphMutation::AddNode { id, weight })
            .await
            .expect("mutation channel open");

        // Wait for the mutation to actually land in storage before triggering the checkpoint —
        // `mutation_tx.send().await` only enqueues it; the coordinator's event loop applies it
        // asynchronously. This poll is a test-setup concern, not part of the fix under test.
        for _ in 0..40 {
            if handle
                .storage
                .event_count
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(
            handle.snapshot.load().baked_at_event,
            0,
            "sanity: nothing baked yet"
        );

        force_initial_bake(&handle, 1).await;

        assert!(
            handle.snapshot.load().baked_at_event > 0,
            "force_initial_bake must land a bake within its poll window"
        );
    }

    /// Minimal `WorkbenchState` fixture for [`snapshot_pending`]'s own tests — every field is a
    /// generic, non-analysis-specific placeholder except `replayed_events`, the one field under
    /// test.
    /// Checkpoint fix (plan 44-14 Task 3): a scope deferred at boot resolves to `None` while the
    /// bake is pending, then to a real scope exactly once after the bake lands — never to the
    /// empty boot placeholder. Reproduces the 2026-09-08 defect where `/api/graph/population`
    /// streamed 0 nodes against a 113,384-node population.
    #[test]
    fn deferred_population_scope_resolves_only_after_the_bake_lands() {
        use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
        use urban_integrity_core::graph::snapshot::bake_csr;
        use urban_integrity_core::persistence::wal_channel;
        use urban_integrity_schema::types::id::EntityId;
        use urban_integrity_schema::types::node::{NodeWeight, WardData};

        let (wal_tx, _wal_rx) = wal_channel(16);
        let (_coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        handle.storage.insert_node(
            EntityId("ward:X05000001".to_string()),
            NodeWeight::Ward(WardData {
                name: "Test Ward".to_string(),
            }),
        );

        let mut state = minimal_state(1);
        state.late_scope =
            crate::graph::population_scope::LateScope::new(vec!["X05000001".to_string()]);

        assert!(
            resolve_population_scope(&state, &handle).is_none(),
            "bake pending (replayed_events > 0, baked_at_event == 0) must resolve to None"
        );
        assert!(
            state.late_scope.cell.get().is_none(),
            "nothing cached while pending"
        );

        handle.snapshot.store(bake_csr(&handle.storage, 1));

        let scope = resolve_population_scope(&state, &handle).expect("resolves after bake");
        assert_eq!(
            scope.ward_seed_count, 1,
            "the ward code resolved to its Ward node"
        );
        assert_eq!(scope.node_ids.len(), 1);
        assert!(
            state.late_scope.cell.get().is_some(),
            "cached after first resolution"
        );
        let again = resolve_population_scope(&state, &handle).expect("still resolves");
        assert!(
            std::ptr::eq(scope, again),
            "resolved exactly once, same allocation"
        );
    }

    fn minimal_state(replayed_events: usize) -> WorkbenchState {
        WorkbenchState {
            population: PopulationSummary::Available {
                count: 0,
                generated_at: chrono::Utc::now(),
                ward_seed_count: 0,
            },
            replayed_at: chrono::Utc::now(),
            analyses: vec![],
            db_name: "test_db".to_string(),
            actor: "operator".to_string(),
            db: std::sync::Arc::new(Surreal::init()),
            catalog: std::sync::Arc::new(Surreal::init()),
            oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
            last_events: crate::last_event::LastEventSummary::default(),
            report_cache: std::sync::RwLock::new(None),
            last_findings: std::sync::RwLock::new(None),
            report_running: std::sync::atomic::AtomicBool::new(false),
            replayed_events,
            late_scope: crate::graph::population_scope::LateScope::none(),
            population_scope: crate::graph::population_scope::PopulationScope {
                node_ids: Default::default(),
                ward_seed_count: 0,
            },
            label_index: crate::graph::label_index::LabelIndex::default(),
            graph_cache: crate::graph::cache::AlgorithmCache::new(),
            graph_sessions: crate::graph::session::SessionRegistry::new(),
            hub_node_ids: std::sync::OnceLock::new(),
        }
    }

    /// [`snapshot_pending`] (checkpoint fix item 4): `false` when nothing was ever replayed (an
    /// empty graph at `baked_at_event == 0` is correct, not pending); `true` once a nonzero
    /// replay happened but the bake has not landed; `false` again once a real bake lands,
    /// mirroring `force_initial_bake_lands_a_snapshot_within_the_poll_window`'s own fixture for
    /// "landing a bake" (a real `AddNode` mutation, not a synthetic snapshot swap).
    ///
    /// PROVE THIS CAN FAIL: a version of `snapshot_pending` that reads only
    /// `handle.snapshot.load().baked_at_event == 0` (dropping the `replayed_events > 0` guard)
    /// reports `true` for the never-replayed case too, which the first assertion below would
    /// catch.
    #[tokio::test]
    async fn snapshot_pending_is_true_only_between_a_nonzero_replay_and_its_bake_landing() {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let coord_cfg = CoordinatorConfig {
            snapshot_every_n_events: 1_000_000,
            snapshot_interval_secs: 30,
        };
        let (coordinator, handle) = GraphCoordinator::new(coord_cfg, wal_tx, None);
        tokio::spawn(coordinator.run());

        assert!(
            !snapshot_pending(&minimal_state(0), &handle),
            "nothing replayed — an empty graph is the correct state, never pending"
        );
        assert!(
            snapshot_pending(&minimal_state(1), &handle),
            "a nonzero replay happened but the bake has not landed yet — must be pending"
        );

        let id = EntityId("snapshot-pending-test:1".to_string());
        let weight = NodeWeight::Entity(EntityData {
            name: "Test Corp".to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        handle
            .mutation_tx
            .send(GraphMutation::AddNode { id, weight })
            .await
            .expect("mutation channel open");
        for _ in 0..40 {
            if handle
                .storage
                .event_count
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        force_initial_bake(&handle, 1).await;

        assert!(
            !snapshot_pending(&minimal_state(1), &handle),
            "once the bake has landed, no longer pending"
        );
    }

    /// A taken port produces a named, non-zero-exit error whose message contains the literal
    /// bind address and the words "already in use" (D-05) — never a bare OS error, and never
    /// an auto-picked fallback port.
    #[tokio::test]
    async fn bind_listener_on_a_taken_port_names_the_address_and_the_remedy() {
        let held = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port to hold it open");
        let addr = held.local_addr().unwrap().to_string();

        let err = bind_listener(&addr)
            .await
            .expect_err("binding an already-held address must fail");
        let msg = err.to_string();
        assert!(
            msg.contains(&addr),
            "error must name the exact bind address: {msg}"
        );
        assert!(
            msg.contains("already in use"),
            "error must say 'already in use': {msg}"
        );

        drop(held);
    }

    /// A router-level test — sends `GET /api/workbench-status` through the fully ASSEMBLED
    /// `Router` (a real HTTP request against a real listener, not a direct call to the handler
    /// function) and asserts a `200` + JSON body, proving `workbench_status_handler`'s
    /// `Extension<Arc<GraphCoordinatorHandle>>`/`Extension<Arc<WorkbenchState>>` extractors
    /// actually resolve when routed through the merged router — the specific defect class
    /// `core_router`'s doc comment warns about (`.route()`d atop its output loses the layers).
    ///
    /// PROVE THIS CAN FAIL (per `docs/DEVELOPMENT.md`): this test was first run against a
    /// version of `build_router` with `.layer(Extension(state))` removed from
    /// `workbench_routes` — the request failed for real (HTTP 500, Axum's "Extension of type
    /// `Arc<WorkbenchState>` was not found" rejection body) — before the layer was restored to
    /// the version committed here.
    #[tokio::test]
    async fn workbench_status_is_reachable_through_the_assembled_router() {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        let handle = Arc::new(handle);

        let state = Arc::new(WorkbenchState {
            population: PopulationSummary::Available {
                count: 0,
                generated_at: chrono::Utc::now(),
                ward_seed_count: 0,
            },
            replayed_at: chrono::Utc::now(),
            analyses: vec![],
            db_name: "test_db".to_string(),
            actor: "operator".to_string(),
            db: std::sync::Arc::new(Surreal::init()),
            catalog: std::sync::Arc::new(Surreal::init()),
            oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
            last_events: crate::last_event::LastEventSummary::default(),
            report_cache: std::sync::RwLock::new(None),
            last_findings: std::sync::RwLock::new(None),
            report_running: std::sync::atomic::AtomicBool::new(false),
            replayed_events: 0,
            late_scope: crate::graph::population_scope::LateScope::none(),
            population_scope: crate::graph::population_scope::PopulationScope {
                node_ids: Default::default(),
                ward_seed_count: 0,
            },
            label_index: crate::graph::label_index::LabelIndex::default(),
            graph_cache: crate::graph::cache::AlgorithmCache::new(),
            graph_sessions: crate::graph::session::SessionRegistry::new(),
            hub_node_ids: std::sync::OnceLock::new(),
        });

        let router = build_router(handle, state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/workbench-status"))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.expect("JSON body");
        assert!(body.is_object(), "expected a JSON object body: {body:?}");
    }

    // ── resolve_hub_ids (D-14/D-15): computed once, cached for the process's life ───────────

    /// PROVE THIS CAN FAIL: a version of `resolve_hub_ids` calling `state.hub_node_ids.get()`
    /// then unconditionally recomputing on `None` (dropping `get_or_init`'s atomicity) would
    /// still pass a same-*value* assertion, since the fixture graph is static — only comparing
    /// the returned reference's address, not merely its contents, catches a resolver that
    /// recomputes on every call.
    #[tokio::test]
    async fn resolve_hub_ids_is_computed_once_and_cached() {
        let nodes = vec![(
            EntityId("a".to_string()),
            NodeWeight::Entity(EntityData {
                name: "A".to_string(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        )];
        let (handle, state) = crate::graph::fixture::workbench_from_graph(&nodes, &[], &[]).await;

        let first = resolve_hub_ids(&handle, &state).expect("fixture bake has landed");
        let first_ptr = first as *const std::collections::HashSet<String>;
        let first_value = first.clone();

        let second = resolve_hub_ids(&handle, &state).expect("fixture bake has landed");
        let second_ptr = second as *const std::collections::HashSet<String>;

        assert!(
            std::ptr::eq(first_ptr, second_ptr),
            "resolve_hub_ids must return the same cached allocation on every call"
        );
        assert_eq!(
            *second, first_value,
            "resolve_hub_ids must return the same value on every call"
        );
    }

    /// WR-03: while the first CSR bake is pending, the hub set is "not yet" — never an empty set
    /// measured over the boot-time placeholder scope and then frozen for the process lifetime.
    ///
    /// PROVE THIS CAN FAIL: revert `resolve_hub_ids` to reading `state.population_scope`
    /// directly and the first call returns `Some(&{})` and fills the cell, so both "pending"
    /// assertions below fail — that empty set is exactly what a large-corpus boot used to keep.
    #[test]
    fn resolve_hub_ids_caches_nothing_while_the_bake_is_pending() {
        use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
        use urban_integrity_core::graph::snapshot::bake_csr;
        use urban_integrity_core::persistence::wal_channel;
        use urban_integrity_schema::types::id::EntityId;
        use urban_integrity_schema::types::node::{NodeWeight, WardData};

        let (wal_tx, _wal_rx) = wal_channel(16);
        let (_coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        handle.storage.insert_node(
            EntityId("ward:X05000001".to_string()),
            NodeWeight::Ward(WardData {
                name: "Test Ward".to_string(),
            }),
        );

        let mut state = minimal_state(1);
        state.late_scope =
            crate::graph::population_scope::LateScope::new(vec!["X05000001".to_string()]);

        assert!(
            resolve_hub_ids(&handle, &state).is_none(),
            "bake pending: the hub set is not yet measurable"
        );
        assert!(
            state.hub_node_ids.get().is_none(),
            "nothing cached while pending"
        );

        handle.snapshot.store(bake_csr(&handle.storage, 1));

        let first = resolve_hub_ids(&handle, &state).expect("resolves after the bake lands");
        assert!(
            state.late_scope.cell.get().is_some(),
            "measured over the RESOLVED late scope, which the call resolved on the way"
        );
        let again = resolve_hub_ids(&handle, &state).expect("still resolves");
        assert!(std::ptr::eq(first, again), "cached after first resolution");
    }
}

#[cfg(test)]
mod reauth_session_tests {
    use super::*;

    /// Build an `AppConfig` pointed at an isolated namespace — the established pattern from
    /// `crates/cli/tests/probe_tier2_e2e.rs`'s own `test_config`, which exists to drive the real
    /// production connect/re-auth functions without racing `AppConfig::load()`'s global env-var
    /// reads under `cargo test`'s default parallel execution. `AppConfig` has no `Default` and is
    /// `deny_unknown_fields`, so every required section is spelled out.
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

    /// **The defect this guards** (found live 2026-09-13 on an operator analysis): every Tier-1
    /// probe returned `DataGap` with `401 Unauthorized for url (http://localhost:8000/rpc)` and
    /// all 15 Tier-2 probes cascaded to `Skipped`, while the periodic re-auth task logged
    /// `SurrealDB session refreshed (periodic re-auth)` on schedule. The report read
    /// `population: 0, capable: 0, hit_count: 0` — a total loss of analysis, five hours after
    /// boot, with a healthy-looking log.
    ///
    /// **Why it happened.** In `surrealdb 3.1.0-beta.2` a `Surreal<C>` carries its own
    /// `session_id`, and `Clone` mints a NEW one (`src/lib.rs:334`, `clone_session(old, new)`
    /// copies the session then leaves the two independent). Authentication is per-session. The
    /// old `boot()` stored `db: params.db.as_ref().clone()` — a `Surreal::clone`, hence a
    /// SEPARATE session from the `Arc` the re-auth task holds. Its token expired one hour after
    /// boot (measured: `exp - iat == 3600`) and nothing ever refreshed it. The task's own
    /// 50-minute re-signin was real, and refreshed a session no query used.
    ///
    /// **How this test fails without the fix.** `invalidate()` leaves a session
    /// unauthenticated, exactly as expiry does — it is the lever that makes an hour of wall
    /// clock testable in milliseconds. Invalidating the probe handle's session and then doing
    /// what the periodic task does (re-auth the boot handle) must leave the probe handle able
    /// to query. When the two are separate sessions the re-auth cannot reach it, the final
    /// query returns 401, and the test fails — which is what it did before
    /// [`probe_db_handle`] switched from `Surreal::clone` to `Arc::clone`.
    #[tokio::test]
    async fn periodic_reauth_restores_the_handle_the_probe_battery_actually_queries_through() {
        if !urban_integrity_test_support::db_tests_enabled() {
            return;
        }
        let ns = urban_integrity_test_support::NsGuard::with_prefix("test_reauth_session");
        let config = test_config(ns.name());
        let db_name = "reauth_probe";

        let boot_db = Arc::new(
            connect_to_db(&config, db_name)
                .await
                .expect("boot connect + signin"),
        );

        // Exactly what `boot()` hands `WorkbenchState.db`, via the same seam.
        let probe_db = probe_db_handle(&boot_db);

        // The probe handle's session loses its authentication, as it does when its token
        // expires an hour after boot.
        probe_db
            .invalidate()
            .await
            .expect("invalidate must reach the server");

        // What `spawn_periodic_reauth` does every 50 minutes, against the handle it holds.
        reauth_db(&boot_db, &config, db_name)
            .await
            .expect("periodic re-auth must succeed");

        // The probe battery's next query. Before the fix this is the live 401.
        let queried = probe_db.query("INFO FOR DB").await;
        assert!(
            queried.is_ok(),
            "the handle the probe battery queries through must be usable after the periodic \
             re-auth ran — it shares the boot handle's session. Got: {:?}",
            queried.err()
        );
    }

    /// Non-vacuity guard for the test above: proves `invalidate()` genuinely de-authenticates
    /// the session, so the assertion there cannot pass merely because nothing ever breaks.
    /// Without this, a change that made `invalidate` a no-op would leave the test above green
    /// while guarding nothing.
    #[tokio::test]
    async fn invalidate_really_does_break_a_session_so_the_guard_above_is_not_vacuous() {
        if !urban_integrity_test_support::db_tests_enabled() {
            return;
        }
        let ns = urban_integrity_test_support::NsGuard::with_prefix("test_reauth_vacuity");
        let config = test_config(ns.name());
        let db_name = "reauth_vacuity";

        let db = connect_to_db(&config, db_name)
            .await
            .expect("connect + signin");
        db.query("INFO FOR DB")
            .await
            .expect("an authenticated session must query fine");

        db.invalidate().await.expect("invalidate");

        assert!(
            db.query("INFO FOR DB").await.is_err(),
            "invalidate() must leave the session unable to query — if this passes, \
             invalidate is not a stand-in for token expiry and the re-auth guard is vacuous"
        );
    }
}
