//! `/api/workbench-status` and `/api/analyses` — the workbench's own JSON endpoints (D-09/D-10),
//! layered onto `workbench_routes` in `crates/workbench::boot::build_router`.
//!
//! # Discretion note — why `/api/analyses` is a fixed, boot-time snapshot
//!
//! Per the phase 42 research notes' Architectural Responsibility Map ("this phase does not need to
//! implement multi-analysis query re-targeting, only the read-only list") and D-10 as amended in
//! the phase 42 design notes ("in this phase the workbench boots one replay-only engine bound to a single
//! analysis database... live re-targeting is a Phase 43+ concern"), the analysis list below is a
//! fixed snapshot taken once at CLI boot time (plan 42-07 fetches it via `list_analyses` before
//! calling `boot()`). There is deliberately no server-side code path here that lets a client pick
//! a DIFFERENT analysis and have this process query it live — genuine live re-targeting requires
//! a process restart with a different `--analysis` flag; the frontend surfaces this honestly
//! (plan 42-08) rather than silently reducing D-10's scope.
//!
//! Neither handler below reads any client-supplied query parameter that selects a database: both
//! read exclusively from `Extension<Arc<WorkbenchState>>`, which is fixed at boot time from
//! `BootParams` — the analysis-picker security control (threat T-42-07).

use std::sync::Arc;

use axum::{response::Json, Extension};

use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_workbench_dto::{AnalysisSummaryView, PopulationView, StatusView};

use crate::boot::{PopulationSummary, WorkbenchState};

/// Maps the boot-time [`PopulationSummary`] onto the wire shape: an available population is
/// `count: Some(n)` with no reason; an unavailable one is `count: None` with the reason — never
/// a fabricated `0` that looks like a real (empty) population. `generated_at` travels in both.
pub(crate) fn population_view(population: &PopulationSummary) -> PopulationView {
    match population {
        PopulationSummary::Available {
            count,
            generated_at,
            ..
        } => PopulationView {
            count: Some(*count),
            generated_at: generated_at.to_rfc3339(),
            unavailable_reason: None,
        },
        PopulationSummary::Unavailable {
            reason,
            generated_at,
            ..
        } => PopulationView {
            count: None,
            generated_at: generated_at.to_rfc3339(),
            unavailable_reason: Some(reason.clone()),
        },
    }
}

/// `GET /api/workbench-status` — analysis identity, live node/edge counts (read from
/// `handle.snapshot.load()`, never hardcoded), the D-06 "graph as of `<timestamp>`" honesty
/// line, the canonical population and its generation time (D-19/WBX-06), and the D-09
/// "ingestors: not running" line — always `false` in this phase, typed as a real `bool`.
pub async fn workbench_status_handler(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> Json<StatusView> {
    let snapshot = handle.snapshot.load();
    let snapshot_pending = crate::boot::snapshot_pending(&state, &handle);

    let analysis_name = state
        .analyses
        .iter()
        .find(|entry| entry.db_name == state.db_name)
        .map(|entry| entry.label.clone())
        .unwrap_or_else(|| state.db_name.clone());

    Json(StatusView {
        analysis_name,
        database: state.db_name.clone(),
        node_count: snapshot.graph.node_count() as u64,
        edge_count: snapshot.graph.edge_count() as u64,
        graph_as_of: state.replayed_at.to_rfc3339(),
        population: population_view(&state.population),
        ingestors_running: false,
        actor: state.actor.clone(),
        snapshot_pending,
    })
}

/// `GET /api/analyses` — the read-only `_catalog` listing (D-10), one `AnalysisSummaryView` per
/// `BootParams.analyses` entry, with `is_current` true on exactly the one whose `db_name` equals
/// the database this process is actually bound to.
///
/// The bound database is ALWAYS present in the list, exactly once, as the current entry. When
/// it has no `_catalog` row (the default `main` path, or a fresh namespace), a synthetic entry
/// labelled by its own database name is prepended — the picker must never read "No analysis"
/// beside a status page that names a real database (Phase 42 code review, WR-10).
pub async fn analyses_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> Json<Vec<AnalysisSummaryView>> {
    Json(analyses_view(&state))
}

/// The pure core of [`analyses_handler`], split out for testing: the catalog entries mapped to
/// wire views, plus the synthetic current entry when the bound database is uncatalogued.
pub(crate) fn analyses_view(state: &WorkbenchState) -> Vec<AnalysisSummaryView> {
    let mut views: Vec<AnalysisSummaryView> = state
        .analyses
        .iter()
        .map(|entry| AnalysisSummaryView {
            db_name: entry.db_name.clone(),
            label: entry.label.clone(),
            is_current: entry.db_name == state.db_name,
        })
        .collect();
    if !views.iter().any(|view| view.is_current) {
        views.insert(
            0,
            AnalysisSummaryView {
                db_name: state.db_name.clone(),
                label: state.db_name.clone(),
                is_current: true,
            },
        );
    }
    views
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
    use urban_integrity_core::persistence::wal_channel;

    use crate::boot::AnalysisCatalogEntry;

    async fn make_handle() -> Arc<GraphCoordinatorHandle> {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        Arc::new(handle)
    }

    fn sample_state() -> Arc<WorkbenchState> {
        Arc::new(WorkbenchState {
            population: PopulationSummary::Available {
                count: 1_234,
                generated_at: Utc::now(),
                ward_seed_count: 0,
            },
            replayed_at: Utc::now(),
            analyses: vec![
                AnalysisCatalogEntry {
                    db_name: "an_a".to_string(),
                    label: "Analysis A".to_string(),
                },
                AnalysisCatalogEntry {
                    db_name: "an_b".to_string(),
                    label: "Analysis B".to_string(),
                },
            ],
            db_name: "an_a".to_string(),
            actor: "operator".to_string(),
            db: std::sync::Arc::new(surrealdb::Surreal::init()),
            catalog: std::sync::Arc::new(surrealdb::Surreal::init()),
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
        })
    }

    /// `workbench_status_handler`'s output reflects `BootParams`/`WorkbenchState` — never
    /// hardcoded values — for every field the `<behavior>` block names.
    #[tokio::test]
    async fn workbench_status_reflects_state_not_hardcoded_values() {
        let handle = make_handle().await;
        let state = sample_state();
        let replayed_at = state.replayed_at;
        let PopulationSummary::Available { generated_at, .. } = state.population else {
            unreachable!("sample_state builds an Available population");
        };

        let resp = workbench_status_handler(Extension(handle), Extension(state.clone())).await;
        let status = resp.0;

        assert!(!status.ingestors_running);
        assert_eq!(status.actor, "operator");
        assert_eq!(status.node_count, 0);
        assert_eq!(status.edge_count, 0);
        assert_eq!(status.graph_as_of, replayed_at.to_rfc3339());
        assert_eq!(status.population.count, Some(1_234));
        assert_eq!(status.population.unavailable_reason, None);
        assert_eq!(status.population.generated_at, generated_at.to_rfc3339());
        assert_eq!(status.database, "an_a");
        assert_eq!(status.analysis_name, "Analysis A");
        assert!(
            !status.snapshot_pending,
            "sample_state's replayed_events is 0 — never pending regardless of bake state"
        );
    }

    /// An unavailable population is reported as `count: None` plus the reason — never as a
    /// fabricated `0` that a reader would take for a real, empty population — and its
    /// generation timestamp still travels with it.
    ///
    /// PROVE THIS CAN FAIL: mapping `Unavailable` to `count: Some(0)` (the pre-fix behaviour
    /// was a hard boot failure; a naive "fix" would be to report 0) fails the first assertion.
    #[test]
    fn unavailable_population_is_none_with_a_reason_not_a_fabricated_zero() {
        let generated_at = Utc::now();
        let view = population_view(&PopulationSummary::Unavailable {
            reason: "no ward code resolved".to_string(),
            generated_at,
            ward_seed_count: 0,
        });
        assert_eq!(view.count, None);
        assert_eq!(
            view.unavailable_reason.as_deref(),
            Some("no ward code resolved")
        );
        assert_eq!(view.generated_at, generated_at.to_rfc3339());
    }

    /// PROVE THIS CAN FAIL (per `docs/DEVELOPMENT.md`): this test was first run against a
    /// version of `analyses_handler` that compared `entry.db_name == entry.label` instead of
    /// `entry.db_name == state.db_name` — every entry came back `is_current: false` (a real,
    /// observed failure: `current.len()` was `0`, not `1`) — before the comparison was corrected
    /// to the version committed here.
    #[tokio::test]
    async fn analyses_handler_marks_exactly_one_entry_as_current() {
        let state = sample_state();
        let resp = analyses_handler(Extension(state)).await;
        let views = resp.0;

        assert_eq!(views.len(), 2);
        let current: Vec<_> = views.iter().filter(|v| v.is_current).collect();
        assert_eq!(current.len(), 1, "exactly one entry must be current");
        assert_eq!(current[0].db_name, "an_a");
    }

    /// The bound database has no `_catalog` row (the default `main` path, or a fresh test
    /// namespace): it must still appear, exactly once, as the current entry — labelled by its
    /// own database name — so the picker never shows "No analysis" beside a real database.
    ///
    /// PROVE THIS CAN FAIL: against the pre-fix handler (a plain map over `state.analyses`),
    /// `current.len()` is `0` and the first assertion fails.
    #[test]
    fn uncatalogued_bound_database_is_still_the_single_current_entry() {
        let state = WorkbenchState {
            population: PopulationSummary::Available {
                count: 0,
                generated_at: Utc::now(),
                ward_seed_count: 0,
            },
            replayed_at: Utc::now(),
            analyses: vec![AnalysisCatalogEntry {
                db_name: "an_a".to_string(),
                label: "Analysis A".to_string(),
            }],
            db_name: "main".to_string(),
            actor: "operator".to_string(),
            db: std::sync::Arc::new(surrealdb::Surreal::init()),
            catalog: std::sync::Arc::new(surrealdb::Surreal::init()),
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
        };
        let views = analyses_view(&state);

        let current: Vec<_> = views.iter().filter(|v| v.is_current).collect();
        assert_eq!(current.len(), 1, "exactly one entry must be current");
        assert_eq!(current[0].db_name, "main");
        assert_eq!(current[0].label, "main");
        assert_eq!(views.len(), 2, "the catalogued entry is still listed");
        assert!(
            views.iter().filter(|v| v.db_name == "main").count() == 1,
            "the bound database must appear exactly once"
        );
    }
}
