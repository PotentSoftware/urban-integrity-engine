//! `GET /api/analysis/report`, `POST /api/analysis/run`, `GET /api/analysis/report.md` — the
//! in-process probe battery endpoints (D-15/D-16/D-19/D-21), layered onto `workbench_routes` in
//! `crates/workbench::boot::build_router` (Phase 43 plan 05).
//!
//! Reads exclusively from `Extension<Arc<WorkbenchState>>` — no client-supplied query/path
//! parameter selects the analysis or database (T-43-05-04). `POST /api/analysis/run` is guarded
//! by a single-flight `AtomicBool` compare-exchange (T-43-05-01): a second concurrent POST is
//! rejected with HTTP 409 before `execute()` is ever invoked a second time.
//!
//! `run_handler` also best-effort persists every successful run to `workbench_report:latest`
//! (checkpoint fix, Phase 43 plan 09, amending D-16's original "no persistence" — see
//! `crate::report_persistence` and the phase 43 design notes' dated amendment under D-16). A persistence failure is WARN-only and never
//! fails the run.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::Extension;
use serde_json::json;

use urban_integrity_probe::report::render_evidence_report;
use urban_integrity_probe::run::{execute, RunOptions};
use urban_integrity_workbench_dto::ReportView;

use crate::boot::{CachedFindings, CachedReport, WorkbenchState};
use crate::report_view::from_battery;

/// `GET /api/analysis/report` — the latest cached `ReportView`, or a typed 404 body when no run
/// has completed yet (D-16).
pub async fn report_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> Result<Json<ReportView>, (StatusCode, Json<serde_json::Value>)> {
    let cache = state
        .report_cache
        .read()
        .expect("report_cache lock poisoned");
    match cache.as_ref() {
        Some(cached) => Ok(Json(cached.view.clone())),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no run yet" })),
        )),
    }
}

/// `POST /api/analysis/run` — runs the in-process probe battery through
/// `urban_integrity_probe::run::execute` (the same path the CLI `probe` subcommand uses, D-15),
/// guarded so only one run may be in flight at a time (T-43-05-01).
///
/// On success, replaces `report_cache` with the new run and returns its `ReportView`. On
/// failure, `report_cache` is left UNTOUCHED — the previous report, if any, stays visible
/// (D-19) — and a 500 is returned. `report_running` is always reset to `false` on both paths.
pub async fn run_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> Result<Json<ReportView>, (StatusCode, Json<serde_json::Value>)> {
    if state
        .report_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "a probe run is already in progress" })),
        ));
    }

    let result = execute(
        &state.db,
        // Per-run clone of a live, periodically re-authenticated session — `execute` takes an
        // owned handle. Safe because it inherits the current token and is dropped when the run
        // ends; the fatal clone is one taken at boot and reused for hours (see
        // `boot::probe_db_handle`).
        state.catalog.as_ref().clone(),
        state.db_name.clone(),
        state.oc_gates,
        RunOptions {
            full_battery: false,
        },
    )
    .await;

    // Always reset the single-flight guard, on both the success and failure paths — a stuck
    // `true` here would permanently lock out every future run (T-43-05-01's acceptance
    // criterion).
    state.report_running.store(false, Ordering::SeqCst);

    match result {
        Ok(battery) => {
            let run_at = chrono::Utc::now();
            let view = from_battery(&battery, run_at, &state.label_index);
            let markdown = render_evidence_report(&battery.entries, &battery.strategy);
            // The raw entries feed the entity profile's findings section (PIV-02, plan 45-05) and
            // are MOVED here — `ReportEntry` is not `Clone`, and the Markdown above has already
            // been rendered from them. Replaced wholesale, never merged, for the same reason
            // `report_cache` is: two runs' findings interleaved would be a report of neither.
            *state
                .last_findings
                .write()
                .expect("last_findings lock poisoned") = Some(CachedFindings {
                entries: battery.entries,
                run_at,
            });
            let cached = CachedReport {
                view: view.clone(),
                markdown,
                run_at,
            };
            *state
                .report_cache
                .write()
                .expect("report_cache lock poisoned") = Some(cached.clone());

            // Persist best-effort (checkpoint fix, Phase 43 plan 09, amending D-16) — a
            // persistence failure must never fail the run that produced the report; the
            // in-memory cache set just above is already correct regardless of this outcome.
            if let Err(e) = crate::report_persistence::persist_report(&state.db, &cached).await {
                tracing::warn!(
                    error = %e,
                    "workbench: failed to persist workbench_report:latest — keeping the \
                     in-memory report_cache only, a restart will lose this run"
                );
            }

            Ok(Json(view))
        }
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )),
    }
}

/// `GET /api/analysis/report.md` — the same Markdown dossier the CLI `probe` subcommand writes to
/// disk. Served straight from `cached.markdown`, rendered once at run time (`run_handler`), never
/// re-rendered per request and never re-running the battery — a checkpoint fix (Phase 43 plan
/// 09, amending D-16) so a persisted, reloaded-at-boot `CachedReport` (which carries no raw
/// `Finding`/`Verdict`, see `boot::CachedReport`'s doc comment) can still serve this endpoint.
/// 404 with the same typed "no run yet" body as `report_handler` when no run has completed yet.
pub async fn report_markdown_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> impl IntoResponse {
    let cache = state
        .report_cache
        .read()
        .expect("report_cache lock poisoned");
    match cache.as_ref() {
        Some(cached) => (StatusCode::OK, cached.markdown.clone()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no run yet" })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicBool;
    use std::sync::RwLock;

    use urban_integrity_core::graph::coordinator::{
        CoordinatorConfig, GraphCoordinator, GraphCoordinatorHandle,
    };
    use urban_integrity_core::persistence::wal_channel;
    use urban_integrity_probe::run::OcProbeGates;

    use crate::boot::{build_router, AnalysisCatalogEntry, PopulationSummary};

    async fn make_handle() -> Arc<GraphCoordinatorHandle> {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        Arc::new(handle)
    }

    fn base_state(report_running: bool) -> Arc<WorkbenchState> {
        Arc::new(WorkbenchState {
            population: PopulationSummary::Unavailable {
                reason: "test fixture".to_string(),
                generated_at: chrono::Utc::now(),
                ward_seed_count: 0,
            },
            replayed_at: chrono::Utc::now(),
            analyses: vec![AnalysisCatalogEntry {
                db_name: "an_a".to_string(),
                label: "Analysis A".to_string(),
            }],
            db_name: "an_a".to_string(),
            actor: "operator".to_string(),
            db: std::sync::Arc::new(surrealdb::Surreal::init()),
            catalog: std::sync::Arc::new(surrealdb::Surreal::init()),
            oc_gates: OcProbeGates::default(),
            last_events: crate::last_event::LastEventSummary::default(),
            report_cache: RwLock::new(None),
            last_findings: RwLock::new(None),
            report_running: AtomicBool::new(report_running),
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
        })
    }

    /// `GET /api/analysis/report` returns 404 with a JSON body before any run.
    #[tokio::test]
    async fn report_handler_returns_404_with_json_body_before_any_run() {
        let state = base_state(false);
        let resp = report_handler(Extension(state)).await;
        let err = resp.expect_err("no run yet must be an error response");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    /// A second concurrent `POST /api/analysis/run` — simulated by pre-setting `report_running`
    /// to `true` — returns 409 without invoking `execute()` a second time. Proven at the
    /// router level: a real HTTP request through the fully assembled `Router`.
    #[tokio::test]
    async fn concurrent_run_request_returns_409_while_a_run_is_in_flight() {
        let handle = make_handle().await;
        let state = base_state(true);

        let router = build_router(handle, state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/analysis/run"))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), StatusCode::CONFLICT.as_u16());
    }

    /// `GET /api/analysis/report.md` returns 404 with a JSON body before any run — never a bare
    /// empty 200, which would be indistinguishable from "the battery genuinely produced an
    /// empty report".
    #[tokio::test]
    async fn report_markdown_handler_returns_404_before_any_run() {
        let state = base_state(false);
        let resp = report_markdown_handler(Extension(state))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
