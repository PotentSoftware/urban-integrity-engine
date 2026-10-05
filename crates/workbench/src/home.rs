//! `GET /api/home` — the Home page's four-card wire shape (D-20's `HomeView`), layered onto
//! `workbench_routes` in `crates/workbench::boot::build_router` (Phase 43 plan 05).
//!
//! Reads exclusively from `Extension<Arc<WorkbenchState>>`/`Extension<Arc<
//! GraphCoordinatorHandle>>` — no client-supplied query/path parameter selects the analysis (the
//! same Extension-only discipline `status.rs` documents).
//!
//! The Help card carries no data on the wire: its glossary term count is computed client-side
//! from the glossary compiled into the wasm bundle (`crates/workbench-web::glossary`), which this
//! native crate cannot reach. An earlier shape sent a placeholder `0` for it — a fabricated value
//! — removed per phase 43 code review finding WR-01.

use std::sync::Arc;

use axum::{response::Json, Extension};

use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_workbench_dto::{AnalysisCardView, GraphCardView, HomeView, IngestorsCardView};

use crate::boot::WorkbenchState;
use crate::population::population_meta_view;

/// The pure core of [`home_handler`] — builds all four Home page cards from `state`/`handle`,
/// with no HTTP concerns of its own.
pub(crate) fn home_view(state: &WorkbenchState, handle: &GraphCoordinatorHandle) -> HomeView {
    let snapshot = handle.snapshot.load();

    let analysis_name = state
        .analyses
        .iter()
        .find(|entry| entry.db_name == state.db_name)
        .map(|entry| entry.label.clone())
        .unwrap_or_else(|| state.db_name.clone());

    let cache = state
        .report_cache
        .read()
        .expect("report_cache lock poisoned");
    let (findings_confirmed, probes_could_not_evaluate, probes_skipped, total_probes, last_run_at) =
        match cache.as_ref() {
            None => (0, 0, 0, 0, None),
            Some(cached) => (
                cached.view.bluf.hit_count,
                cached.view.bluf.could_not_evaluate,
                cached.view.bluf.skipped,
                cached.view.bluf.total_probes,
                Some(cached.view.run_at.clone()),
            ),
        };
    drop(cache);

    HomeView {
        analysis: AnalysisCardView {
            analysis_name,
            database: state.db_name.clone(),
            findings_confirmed,
            probes_could_not_evaluate,
            probes_skipped,
            total_probes,
            last_run_at,
        },
        graph: GraphCardView {
            node_count: snapshot.graph.node_count() as u64,
            edge_count: snapshot.graph.edge_count() as u64,
            population: population_meta_view(&state.population),
            graph_as_of: state.replayed_at.to_rfc3339(),
            snapshot_pending: crate::boot::snapshot_pending(state, handle),
        },
        ingestors: IngestorsCardView {
            companies_house_last_event: state.last_events.companies_house.map(|t| t.to_rfc3339()),
            contracts_finder_last_event: state.last_events.contracts_finder.map(|t| t.to_rfc3339()),
            planning_last_event: state.last_events.planning.map(|t| t.to_rfc3339()),
            overall_last_event: state.last_events.overall.map(|t| t.to_rfc3339()),
        },
    }
}

/// `GET /api/home` — the Home page's four cards.
pub async fn home_handler(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> Json<HomeView> {
    Json(home_view(&state, &handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
    use urban_integrity_core::persistence::wal_channel;
    use urban_integrity_probe::run::OcProbeGates;
    use urban_integrity_workbench_dto::{BlufView, ProbeGroupView, ReportView};

    use crate::boot::{AnalysisCatalogEntry, CachedReport, PopulationSummary};

    async fn make_handle() -> Arc<GraphCoordinatorHandle> {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        Arc::new(handle)
    }

    fn base_state() -> WorkbenchState {
        WorkbenchState {
            population: PopulationSummary::Unavailable {
                reason: "no ward code resolved".to_string(),
                generated_at: Utc::now(),
                ward_seed_count: 0,
            },
            replayed_at: Utc::now(),
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
        }
    }

    /// `report_cache: None` (no probe run yet) — `last_run_at: None` and every count `0`.
    #[tokio::test]
    async fn home_view_with_no_report_yet_has_none_last_run_at_and_zero_counts() {
        let handle = make_handle().await;
        let state = base_state();

        let view = home_view(&state, &handle);

        assert_eq!(view.analysis.last_run_at, None);
        assert_eq!(view.analysis.findings_confirmed, 0);
        assert_eq!(view.analysis.probes_could_not_evaluate, 0);
        assert_eq!(view.analysis.probes_skipped, 0);
        assert_eq!(view.analysis.total_probes, 0);
        assert_eq!(view.analysis.analysis_name, "Analysis A");
        assert_eq!(view.analysis.database, "an_a");
        assert!(
            !view.graph.snapshot_pending,
            "base_state's replayed_events is 0 — never pending regardless of bake state"
        );
    }

    /// `report_cache: Some(...)` — `findings_confirmed` matches the cached `ReportView`'s hit
    /// count, and `last_run_at` is populated from the cached run's timestamp.
    #[tokio::test]
    async fn home_view_with_a_populated_report_cache_reflects_its_hit_count() {
        let handle = make_handle().await;
        let state = base_state();

        let view = ReportView {
            bluf: BlufView {
                could_not_evaluate: 2,
                total_probes: 22,
                skipped: 1,
                population: 358_484,
                capable: 358_483,
                hit_count: 3,
            },
            contradiction_checks: vec![],
            groups: vec![ProbeGroupView {
                name: "Confirmed Findings".to_string(),
                probes: vec![],
            }],
            adapted_notes: vec![],
            remediation_items: vec![],
            run_at: "2026-01-01T10:33:00Z".to_string(),
        };
        *state.report_cache.write().unwrap() = Some(CachedReport {
            view,
            markdown: String::new(),
            run_at: Utc::now(),
        });

        let rendered = home_view(&state, &handle);

        assert_eq!(rendered.analysis.findings_confirmed, 3);
        assert_eq!(rendered.analysis.probes_could_not_evaluate, 2);
        assert_eq!(rendered.analysis.probes_skipped, 1);
        assert_eq!(rendered.analysis.total_probes, 22);
        assert_eq!(
            rendered.analysis.last_run_at,
            Some("2026-01-01T10:33:00Z".to_string())
        );
    }
}
