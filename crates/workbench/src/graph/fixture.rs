//! A public, `#[doc(hidden)]` assembler that turns a node/edge list into a coordinator handle
//! and `WorkbenchState` with the population scoped to the ward-reachable set — the same shape
//! `handlers.rs`/`dispatch.rs`'s private test fixtures build, exposed so integration tests in
//! `tests/` (which only see the public API) can drive `dispatch::run_algorithm` and
//! `boot::build_router` against a known graph. Never called by shipped code paths.

use std::collections::HashSet;
use std::sync::Arc;

use urban_integrity_core::graph::coordinator::{
    CoordinatorConfig, GraphCoordinator, GraphCoordinatorHandle,
};
use urban_integrity_core::graph::snapshot::bake_csr;
use urban_integrity_core::persistence::wal_channel;
use urban_integrity_schema::types::edge::EdgeWeight;
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::NodeWeight;

use crate::boot::{AnalysisCatalogEntry, PopulationSummary, WorkbenchState};
use crate::graph::label_index::LabelIndex;
use crate::graph::population_scope::{self, LateScope};

/// Build handle + state. `ward_codes` seed the reachability walk exactly as boot does; an empty
/// list scopes the population to every node (the fixtures' previous behaviour).
#[doc(hidden)]
pub async fn workbench_from_graph(
    nodes: &[(EntityId, NodeWeight)],
    edges: &[(EntityId, EntityId, EdgeWeight)],
    ward_codes: &[String],
) -> (Arc<GraphCoordinatorHandle>, Arc<WorkbenchState>) {
    workbench_from_graph_with_db(
        nodes,
        edges,
        ward_codes,
        Arc::new(surrealdb::Surreal::init()),
    )
    .await
}

/// [`workbench_from_graph`] with a REAL database handle on `WorkbenchState::db`.
///
/// The no-db form leaves an uninitialised `Surreal::init()` there, which is fine for the phase-44
/// canvas routes (they read the in-process snapshot and never touch SurrealDB) and useless for
/// plan 45-08's entity and investigation routes, which read `crates/core::persistence` on every
/// request. A DB-gated test passes a `TestNs` handle here and drives the same
/// `boot::build_router` the operator's browser talks to.
///
/// The caller keeps the `TestNs`/`NsGuard` alive for the whole test — per the
/// test-namespace-hygiene rule (docs/TESTING.md), dropping it removes the namespace.
#[doc(hidden)]
pub async fn workbench_from_graph_with_db(
    nodes: &[(EntityId, NodeWeight)],
    edges: &[(EntityId, EntityId, EdgeWeight)],
    ward_codes: &[String],
    db: Arc<surrealdb::Surreal<surrealdb::engine::any::Any>>,
) -> (Arc<GraphCoordinatorHandle>, Arc<WorkbenchState>) {
    let (wal_tx, _wal_rx) = wal_channel(1024);
    let (coordinator, handle) = GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
    tokio::spawn(coordinator.run());
    let handle = Arc::new(handle);

    for (id, weight) in nodes {
        handle.storage.insert_node(id.clone(), weight.clone());
    }
    for (src, dst, weight) in edges {
        handle
            .storage
            .insert_edge(src, dst, weight.clone())
            .expect("fixture edge insert");
    }

    let event_count = (nodes.len() + edges.len()) as u64;
    let baked = bake_csr(&handle.storage, event_count);
    handle.snapshot.store(baked);

    let scope = if ward_codes.is_empty() {
        population_scope::PopulationScope {
            node_ids: (0..handle.storage.snapshot_nodes().len() as u32).collect::<HashSet<u32>>(),
            ward_seed_count: 0,
        }
    } else {
        let scope = crate::boot::build_population_scope(&handle, ward_codes);
        assert!(
            scope.ward_seed_count > 0,
            "ward_codes {ward_codes:?} matched no Ward node in the fixture graph"
        );
        scope
    };

    let count = scope.node_ids.len() as u64;
    let ward_seed_count = scope.ward_seed_count as u64;
    let label_index = LabelIndex::build(&handle.storage);

    let state = Arc::new(WorkbenchState {
        // Unlike boot, where population comes from the catalog and the scope from the CSR walk (and the two can disagree), the fixture ties them together by construction.
        population: PopulationSummary::Available {
            count,
            generated_at: chrono::Utc::now(),
            ward_seed_count,
        },
        replayed_at: chrono::Utc::now(),
        analyses: vec![AnalysisCatalogEntry {
            db_name: "an_planted".to_string(),
            label: "Planted fixture".to_string(),
        }],
        db_name: "an_planted".to_string(),
        actor: "operator".to_string(),
        db,
        catalog: std::sync::Arc::new(surrealdb::Surreal::init()),
        oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
        last_events: crate::last_event::LastEventSummary::default(),
        report_cache: std::sync::RwLock::new(None),
        last_findings: std::sync::RwLock::new(None),
        report_running: std::sync::atomic::AtomicBool::new(false),
        replayed_events: event_count as usize,
        late_scope: LateScope::none(),
        population_scope: scope,
        label_index,
        graph_cache: crate::graph::cache::AlgorithmCache::new(),
        graph_sessions: crate::graph::session::SessionRegistry::new(),
        hub_node_ids: std::sync::OnceLock::new(),
    });

    (handle, state)
}
