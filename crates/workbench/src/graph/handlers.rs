//! Read-only Graph-page data endpoints (plan 44-08): `GET /api/graph/population` (Arrow IPC,
//! CNV-04), `GET /api/entities/search` (NAV-06), `GET /api/graph/node` (the side card's
//! degree/earliest/latest-edge lookup, GRA-06's node-summary requirement).
//!
//! Every handler here reads exclusively from `Extension<Arc<WorkbenchState>>`/`Extension<Arc<
//! GraphCoordinatorHandle>>` — no client-supplied query/path parameter ever selects the analysis
//! or database (T-43-05-04's established discipline, restated here per T-44-08-02). The two
//! genuinely client-supplied parameters in this file are `entities_search_handler`'s `q` (shapes
//! WHICH rows of the already-fixed label index are returned) and `node_summary_handler`'s `id`
//! (selects WHICH node within the already-fixed population) — neither ever redirects a request to
//! a different `WorkbenchState`/database.

use std::sync::Arc;

use axum::extract::Query;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::Extension;
use petgraph::visit::EdgeRef;
use serde_json::json;

use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight};
use urban_integrity_workbench_dto::{AlgorithmRequest, AlgorithmResult, EntityRef, NodeSummary};

use crate::boot::WorkbenchState;
use crate::graph::arrow_export::{node_display_name, node_kind_name};
use crate::graph::dispatch::{self, DispatchError, ResolveError};

/// `GET /api/graph/population` — the population-scoped Arrow IPC export (CNV-04), built via
/// [`crate::graph::arrow_export::build_arrow_ipc`] off the coordinator's own already-baked
/// `CsrSnapshot` and `WorkbenchState::population_scope`. Runs inside `tokio::task::spawn_blocking`
/// — Arrow serialisation is CPU-bound, synchronous, native-only work, never awaited inline on the
/// async runtime's own worker threads.
pub async fn population_handler(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    Extension(state): Extension<Arc<WorkbenchState>>,
) -> impl IntoResponse {
    let storage = handle.storage.clone();
    let snapshot = Arc::clone(&handle.snapshot.load());
    // Checkpoint fix (plan 44-14 Task 3): the scope may be deferred past boot on a large corpus;
    // answer 503 honestly while the bake is pending rather than streaming an empty population.
    let Some(scope) = crate::boot::resolve_population_scope(&state, &handle) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "population not yet available: the graph snapshot is still baking — retry shortly"
            })),
        )
            .into_response();
    };
    let scope = scope.clone();

    let result = tokio::task::spawn_blocking(move || {
        crate::graph::arrow_export::build_arrow_ipc(&storage, &snapshot, &scope)
    })
    .await;

    match result {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apache.arrow.stream")],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(join_err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": join_err.to_string() })),
        )
            .into_response(),
    }
}

/// Query parameters for [`entities_search_handler`]. `q` is genuinely client-supplied free text
/// (T-44-08-02).
#[derive(Debug, serde::Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
}

/// `GET /api/entities/search?q=` — typeahead search over the boot-time label index (NAV-06). An
/// empty/absent `q` or no match both return `200` with an empty JSON array, never an error — "No
/// entity matches" is an empty list, not a failure state (NAV-06's own requirement text).
pub async fn entities_search_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<SearchQuery>,
) -> Json<Vec<EntityRef>> {
    Json(state.label_index.search(params.q.as_deref().unwrap_or("")))
}

/// Query parameters for [`node_summary_handler`]. `id` selects a node WITHIN the already-fixed
/// population — never a database/analysis selector (T-44-08-02).
#[derive(Debug, serde::Deserialize)]
pub struct NodeQuery {
    id: String,
}

/// Exhaustive `EdgeWeight` -> `&EdgeMeta` extraction, mirroring `csr_view::edge_type_name`'s own
/// exhaustiveness discipline (no `_` wildcard arm — a 13th `EdgeWeight` variant fails this match
/// at compile time rather than silently being excluded from the earliest/latest-edge fold below).
pub(crate) fn edge_meta(w: &EdgeWeight) -> &EdgeMeta {
    match w {
        EdgeWeight::Directs { meta, .. } => meta,
        EdgeWeight::LocatedAt { meta, .. } => meta,
        EdgeWeight::AwardedTo { meta, .. } => meta,
        EdgeWeight::IssuedBy { meta, .. } => meta,
        EdgeWeight::Represents { meta, .. } => meta,
        EdgeWeight::Controls { meta } => meta,
        EdgeWeight::TrusteeOf { meta } => meta,
        EdgeWeight::OwnsProperty { meta, .. } => meta,
        EdgeWeight::OffshoreLink { meta } => meta,
        EdgeWeight::DonatesTo { meta } => meta,
        EdgeWeight::DisqualifiedFrom { meta, .. } => meta,
        EdgeWeight::DeclaresInterest { meta, .. } => meta,
    }
}

/// `GET /api/graph/node?id=` — the side card's node-click payload: degree, and the earliest/
/// latest `effective_from`/`effective_to` across every edge touching the node (both outgoing, via
/// `snapshot.graph.edges(idx)`, and incoming, via `snapshot.neighbors_in(idx)` resolved back
/// against each source's own outgoing edges — the same shape `csr_view.rs` already established).
/// `None` bounds (`effective_from`/`effective_to` absent on an edge) never widen the range; a
/// genuinely isolated node (degree 0) returns `earliest_edge`/`latest_edge` both `None`. 404 with
/// a typed JSON error body for an unknown `id`, never a 500 or a fabricated empty-looking success.
pub async fn node_summary_handler(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    Extension(_state): Extension<Arc<WorkbenchState>>,
    Query(params): Query<NodeQuery>,
) -> Result<Json<NodeSummary>, (StatusCode, Json<serde_json::Value>)> {
    let entity_id = urban_integrity_schema::types::id::EntityId(params.id.clone());
    let Some(entry) = handle.storage.node_map.get(&entity_id) else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no entity with this id" })),
        ));
    };
    let (node_idx, weight, _event_id) = entry.value().clone();
    drop(entry);
    let idx = node_idx.index() as u32;

    let snapshot = handle.snapshot.load();
    let degree = snapshot.neighbors_undirected(idx).len() as u64;

    let mut earliest: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut fold = |meta: &EdgeMeta| {
        if let Some(from) = meta.effective_from {
            earliest = Some(earliest.map_or(from, |cur| cur.min(from)));
        }
        if let Some(to) = meta.effective_to {
            latest = Some(latest.map_or(to, |cur| cur.max(to)));
        }
    };

    // Outgoing edges: idx's own forward adjacency in the CSR.
    for edge_ref in snapshot.graph.edges(idx) {
        fold(edge_meta(edge_ref.weight()));
    }
    // Incoming edges: for every source with an edge pointing AT idx, find that specific edge
    // among the source's own outgoing edges (the CSR has no direct reverse edge-weight lookup).
    for &src in snapshot.neighbors_in(idx) {
        for edge_ref in snapshot.graph.edges(src) {
            if edge_ref.target() == idx {
                fold(edge_meta(edge_ref.weight()));
            }
        }
    }

    let entity = EntityRef {
        id: params.id,
        name: node_display_name(&weight),
        kind: node_kind_name(&weight).to_string(),
    };

    Ok(Json(NodeSummary {
        entity,
        degree,
        earliest_edge: earliest.map(|dt| dt.to_rfc3339()),
        latest_edge: latest.map(|dt| dt.to_rfc3339()),
    }))
}

/// Maps a [`DispatchError`] to its HTTP status + typed JSON error body (plan 44-09's `<action>`
/// text): `BAD_REQUEST` for a structurally invalid request (unknown algorithm/edge type, a
/// missing required id field), `UNPROCESSABLE_ENTITY` for a resolve failure (distinguishing
/// not-found from out-of-scope in the `"error"` text so the frontend can render 44-UI-SPEC's
/// exact out-of-scope copy), `SERVICE_UNAVAILABLE` when the population itself is unavailable,
/// `GATEWAY_TIMEOUT` when the outer wall-clock safety net fired, `INTERNAL_SERVER_ERROR` for a
/// panicked algorithm task.
fn dispatch_error_response(err: DispatchError) -> (StatusCode, Json<serde_json::Value>) {
    let status = match &err {
        DispatchError::UnknownAlgorithm(_)
        | DispatchError::InvalidEdgeType(_)
        | DispatchError::MissingParameter(_) => StatusCode::BAD_REQUEST,
        DispatchError::Resolve(ResolveError::NotFound(_))
        | DispatchError::Resolve(ResolveError::OutOfScope(_)) => StatusCode::UNPROCESSABLE_ENTITY,
        DispatchError::PopulationUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        DispatchError::BudgetExceeded => StatusCode::GATEWAY_TIMEOUT,
        DispatchError::JoinError(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": err.to_string() })))
}

/// `POST /api/graph/algorithm` — the single dispatch point for all six Graph-page algorithms
/// (GRA-01–GRA-04, GRA-06). See `crate::graph::dispatch::run_algorithm`'s own doc comment for the
/// full clamp/resolve/cache/session/blocking-pool contract this delegates to.
pub async fn algorithm_handler(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    Extension(state): Extension<Arc<WorkbenchState>>,
    Json(req): Json<AlgorithmRequest>,
) -> Result<Json<AlgorithmResult>, (StatusCode, Json<serde_json::Value>)> {
    dispatch::run_algorithm(handle, state, req)
        .await
        .map(Json)
        .map_err(dispatch_error_response)
}

/// Body for [`algorithm_cancel_handler`].
#[derive(Debug, serde::Deserialize)]
pub struct CancelRequest {
    session_id: String,
}

/// `POST /api/graph/algorithm/cancel` — cancels the currently-registered job for `session_id`,
/// if any (D-05/GRA-06). Always `200`, idempotent, a safe no-op for an unknown or
/// already-finished session id (`SessionRegistry::cancel`'s own contract, plan 44-07) — never a
/// 404/500.
pub async fn algorithm_cancel_handler(
    Extension(state): Extension<Arc<WorkbenchState>>,
    Json(req): Json<CancelRequest>,
) -> Json<serde_json::Value> {
    state.graph_sessions.cancel(&req.session_id);
    Json(json!({ "cancelled": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::persistence::wal_channel;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    use crate::boot::{build_router, AnalysisCatalogEntry, PopulationSummary};
    use crate::graph::label_index::LabelIndex;
    use crate::graph::population_scope::PopulationScope;

    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }

    fn meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

    /// Builds storage with `nodes`/`edges`, bakes a CSR snapshot, seeds a coordinator handle with
    /// both, scopes the population to every node (so the fixture's own nodes are exactly what
    /// `population_handler` exports), builds the label index over the same storage, and assembles
    /// the full router — mirroring `boot.rs`'s own router-level test fixture shape.
    async fn router_with_fixture(
        nodes: &[(&str, NodeWeight)],
        edges: &[(&str, &str)],
    ) -> axum::Router {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        let handle = Arc::new(handle);

        for (id, weight) in nodes {
            handle
                .storage
                .insert_node(EntityId(id.to_string()), weight.clone());
        }
        for (src, dst) in edges {
            handle
                .storage
                .insert_edge(
                    &EntityId(src.to_string()),
                    &EntityId(dst.to_string()),
                    EdgeWeight::Directs {
                        meta: meta(),
                        data: DirectsData {
                            details: "test".to_string(),
                            officer_role: None,
                        },
                    },
                )
                .expect("fixture edge insert");
        }

        let event_count = nodes.len() as u64 + edges.len() as u64;
        let baked = bake_csr(&handle.storage, event_count);
        handle.snapshot.store(baked);

        let mut entries = handle.storage.snapshot_nodes();
        entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
        let node_ids: std::collections::HashSet<u32> = entries
            .iter()
            .map(|(_, idx, _, _)| idx.index() as u32)
            .collect();
        let population_scope = PopulationScope {
            node_ids,
            ward_seed_count: 1,
        };

        let label_index = LabelIndex::build(&handle.storage);

        let state = Arc::new(WorkbenchState {
            population: PopulationSummary::Available {
                count: nodes.len() as u64,
                generated_at: chrono::Utc::now(),
                ward_seed_count: 1,
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
            oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
            last_events: crate::last_event::LastEventSummary::default(),
            report_cache: std::sync::RwLock::new(None),
            last_findings: std::sync::RwLock::new(None),
            report_running: std::sync::atomic::AtomicBool::new(false),
            replayed_events: event_count as usize,
            late_scope: crate::graph::population_scope::LateScope::none(),
            population_scope,
            label_index,
            graph_cache: crate::graph::cache::AlgorithmCache::new(),
            graph_sessions: crate::graph::session::SessionRegistry::new(),
            hub_node_ids: std::sync::OnceLock::new(),
        });

        build_router(handle, state)
    }

    async fn spawn_router(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr
    }

    /// A router-level test — `GET /api/graph/population` through the fully ASSEMBLED `Router`
    /// returns a 200 whose body round-trips through `arrow::ipc::reader::StreamReader` back to
    /// the expected node count (a small 2-node fixture, both in scope).
    #[tokio::test]
    async fn population_endpoint_round_trips_through_arrow_stream_reader_to_expected_node_count() {
        let router = router_with_fixture(
            &[("a", entity("Alpha Ltd")), ("b", entity("Beta Ltd"))],
            &[("a", "b")],
        )
        .await;
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/graph/population"))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .expect("content-type header present"),
            "application/vnd.apache.arrow.stream"
        );

        let bytes = resp.bytes().await.expect("response body bytes");
        assert!(
            bytes.len() >= 4,
            "buffer must hold at least a length prefix"
        );
        let nodes_len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let nodes_bytes = &bytes[4..4 + nodes_len];
        let mut reader =
            arrow::ipc::reader::StreamReader::try_new(nodes_bytes, None).expect("nodes reader");
        let mut total_rows = 0usize;
        for batch in &mut reader {
            total_rows += batch.expect("nodes RecordBatch").num_rows();
        }
        assert_eq!(total_rows, 2, "both fixture nodes are in scope");
    }

    /// `GET /api/entities/search?q=jane` returns 200 with a JSON array containing the expected
    /// fixture entity.
    #[tokio::test]
    async fn entities_search_returns_the_matching_fixture_entity() {
        let router = router_with_fixture(
            &[("a", entity("Jane Smith")), ("b", entity("Other Co"))],
            &[],
        )
        .await;
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/entities/search?q=jane"))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
        let body: Vec<EntityRef> = resp.json().await.expect("JSON array body");
        assert_eq!(body.len(), 1);
        assert_eq!(body[0].name, "Jane Smith");
    }

    /// `GET /api/entities/search?q=nomatch` returns 200 with an empty JSON array, never a
    /// 404/500 — "No entity matches" is an empty list, not an error.
    #[tokio::test]
    async fn entities_search_with_no_match_returns_200_empty_array_never_an_error() {
        let router = router_with_fixture(&[("a", entity("Alpha Ltd"))], &[]).await;
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/entities/search?q=nomatch"))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
        let body: Vec<EntityRef> = resp.json().await.expect("JSON array body");
        assert!(body.is_empty());
    }

    /// `GET /api/entities/search` with no `q` parameter at all does not panic — treated as an
    /// empty query, empty result, still 200.
    #[tokio::test]
    async fn entities_search_with_no_q_parameter_does_not_panic() {
        let router = router_with_fixture(&[("a", entity("Alpha Ltd"))], &[]).await;
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/entities/search"))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
        let body: Vec<EntityRef> = resp.json().await.expect("JSON array body");
        assert!(body.is_empty());
    }

    /// `GET /api/graph/node?id=<known fixture id>` returns 200 with the correct `degree` and both
    /// `earliest_edge`/`latest_edge` as `null` for an isolated fixture node (no `effective_from`/
    /// `effective_to` set on its fixture edges, so nothing bounds the range).
    #[tokio::test]
    async fn node_summary_returns_degree_and_null_edges_for_isolated_fixture_node() {
        let router = router_with_fixture(
            &[("a", entity("Alpha Ltd")), ("b", entity("Beta Ltd"))],
            &[("a", "b")],
        )
        .await;
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/graph/node?id=a"))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
        let body: NodeSummary = resp.json().await.expect("NodeSummary body");
        assert_eq!(body.entity.id, "a");
        assert_eq!(body.entity.name, "Alpha Ltd");
        assert_eq!(body.degree, 1, "one edge to b");
        assert_eq!(
            body.earliest_edge, None,
            "fixture edge carries no effective_from — must not fabricate a bound"
        );
        assert_eq!(
            body.latest_edge, None,
            "fixture edge carries no effective_to — must not fabricate a bound"
        );
    }

    /// `GET /api/graph/node?id=<unknown id>` returns 404 with a typed JSON error body, never a
    /// 500 or a fabricated empty-looking success.
    #[tokio::test]
    async fn node_summary_for_unknown_id_returns_404_with_typed_error_body() {
        let router = router_with_fixture(&[("a", entity("Alpha Ltd"))], &[]).await;
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .get(format!("http://{addr}/api/graph/node?id=nonexistent"))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 404);
        let body: serde_json::Value = resp.json().await.expect("JSON error body");
        assert!(body.get("error").is_some(), "must carry a typed error key");
    }
}

/// Task 2: `POST /api/graph/algorithm` router-level tests. A separate top-level module (not
/// nested inside `mod tests` above) so `cargo test dispatch::run_algorithm handlers::algorithm`'s
/// substring filter matches this module's own test paths (`handlers::algorithm_tests::...`).
#[cfg(test)]
mod algorithm_tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::persistence::wal_channel;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    use crate::boot::{build_router, AnalysisCatalogEntry, PopulationSummary, WorkbenchState};
    use crate::graph::label_index::LabelIndex;
    use crate::graph::population_scope::PopulationScope;
    use urban_integrity_workbench_dto::{AlgorithmOutcome, AlgorithmRequest, AlgorithmResult};

    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }

    fn directs() -> EdgeWeight {
        EdgeWeight::Directs {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: DirectsData {
                details: "d".into(),
                officer_role: None,
            },
        }
    }

    /// A 3-node `Directs` chain (`n0 -> n1 -> n2`), all in scope — mirrors `router_with_fixture`
    /// above, extended with the `graph_cache`/`graph_sessions` fields plan 44-09 adds.
    async fn router_with_chain_fixture() -> axum::Router {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        let handle = Arc::new(handle);

        for i in 0..3 {
            handle
                .storage
                .insert_node(EntityId(format!("n{i}")), entity(&format!("n{i}")));
        }
        for i in 0..2 {
            handle
                .storage
                .insert_edge(
                    &EntityId(format!("n{i}")),
                    &EntityId(format!("n{}", i + 1)),
                    directs(),
                )
                .expect("fixture edge insert");
        }

        let baked = bake_csr(&handle.storage, 5);
        handle.snapshot.store(baked);

        let mut entries = handle.storage.snapshot_nodes();
        entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
        let node_ids: HashSet<u32> = entries
            .iter()
            .map(|(_, idx, _, _)| idx.index() as u32)
            .collect();
        let population_scope = PopulationScope {
            node_ids,
            ward_seed_count: 1,
        };
        let label_index = LabelIndex::build(&handle.storage);

        let state = Arc::new(WorkbenchState {
            population: PopulationSummary::Available {
                count: 3,
                generated_at: chrono::Utc::now(),
                ward_seed_count: 1,
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
            oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
            last_events: crate::last_event::LastEventSummary::default(),
            report_cache: std::sync::RwLock::new(None),
            last_findings: std::sync::RwLock::new(None),
            report_running: std::sync::atomic::AtomicBool::new(false),
            replayed_events: 5,
            late_scope: crate::graph::population_scope::LateScope::none(),
            population_scope,
            label_index,
            graph_cache: crate::graph::cache::AlgorithmCache::new(),
            graph_sessions: crate::graph::session::SessionRegistry::new(),
            hub_node_ids: std::sync::OnceLock::new(),
        });

        build_router(handle, state)
    }

    async fn spawn_router(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr
    }

    fn paths_request() -> AlgorithmRequest {
        AlgorithmRequest {
            algorithm: "all_simple_paths".to_string(),
            session_id: "session-1".to_string(),
            edge_types_included: vec!["Directs".to_string()],
            seed: 42,
            from_id: Some("n0".to_string()),
            to_id: Some("n2".to_string()),
            ward_id: None,
            ids: vec![],
            scope_ids: vec![],
            hop_cap: 4,
            result_cap: 100,
            budget_secs: 5,
            k: 3,
            top_n: 50,
            min_community_size: 5,
            resolution: 1.0,
        }
    }

    /// `POST /api/graph/algorithm` through the fully assembled router returns 200 with the
    /// expected `AlgorithmOutcome::Paths` shape.
    #[tokio::test]
    async fn algorithm_endpoint_dispatches_all_simple_paths_through_the_assembled_router() {
        let router = router_with_chain_fixture().await;
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/graph/algorithm"))
            .json(&paths_request())
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
        let body: AlgorithmResult = resp.json().await.expect("AlgorithmResult body");
        match body.outcome {
            AlgorithmOutcome::Paths { paths } => assert_eq!(paths.len(), 1),
            other => panic!("expected AlgorithmOutcome::Paths, got {other:?}"),
        }
    }

    /// A request whose `from_id` is out of scope returns 422 with a typed JSON error body
    /// through the assembled router.
    #[tokio::test]
    async fn algorithm_endpoint_returns_422_for_an_unresolvable_from_id() {
        let router = router_with_chain_fixture().await;
        let addr = spawn_router(router).await;

        let mut req = paths_request();
        req.from_id = Some("does-not-exist".to_string());

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/graph/algorithm"))
            .json(&req)
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 422);
        let body: serde_json::Value = resp.json().await.expect("JSON error body");
        assert!(body.get("error").is_some());
    }

    /// An unknown algorithm identifier returns 400 through the assembled router.
    #[tokio::test]
    async fn algorithm_endpoint_returns_400_for_an_unknown_algorithm() {
        let router = router_with_chain_fixture().await;
        let addr = spawn_router(router).await;

        let mut req = paths_request();
        req.algorithm = "not_a_real_algorithm".to_string();

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/graph/algorithm"))
            .json(&req)
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 400);
    }
}
/// Task 3: `POST /api/graph/algorithm/cancel` router-level tests. A separate top-level module (not
/// nested inside `mod tests`) so `cargo test handlers::cancel`'s substring filter matches this
/// module's own test paths (`handlers::cancel_tests::...`).
#[cfg(test)]
mod cancel_tests {
    use std::sync::Arc;

    use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
    use urban_integrity_core::persistence::wal_channel;

    use crate::boot::{build_router, AnalysisCatalogEntry, PopulationSummary, WorkbenchState};
    use crate::graph::label_index::LabelIndex;
    use crate::graph::population_scope::PopulationScope;

    fn base_state() -> Arc<WorkbenchState> {
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
            oc_gates: urban_integrity_probe::run::OcProbeGates::default(),
            last_events: crate::last_event::LastEventSummary::default(),
            report_cache: std::sync::RwLock::new(None),
            last_findings: std::sync::RwLock::new(None),
            report_running: std::sync::atomic::AtomicBool::new(false),
            replayed_events: 0,
            late_scope: crate::graph::population_scope::LateScope::none(),
            population_scope: PopulationScope {
                node_ids: Default::default(),
                ward_seed_count: 0,
            },
            label_index: LabelIndex::default(),
            graph_cache: crate::graph::cache::AlgorithmCache::new(),
            graph_sessions: crate::graph::session::SessionRegistry::new(),
            hub_node_ids: std::sync::OnceLock::new(),
        })
    }

    async fn spawn_router(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        addr
    }

    /// The router-level cancel-then-observe-flipped-token round trip: registers a session
    /// directly against the fixture `WorkbenchState.graph_sessions` (bypassing HTTP, mirroring
    /// `analysis.rs`'s own `base_state(report_running: bool)` pre-set-state precedent), captures
    /// the returned `AlgoCancel`, `POST /api/graph/algorithm/cancel`s that session's id through
    /// the real assembled router, then asserts `is_cancelled()` on the CAPTURED token is now
    /// `true` — proving the HTTP round trip actually reaches `SessionRegistry::cancel` and flips
    /// the SAME token the algorithm loop would be checking.
    #[tokio::test]
    async fn cancel_endpoint_flips_the_correct_sessions_token_through_the_assembled_router() {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        let handle = Arc::new(handle);

        let state = base_state();
        let token = state.graph_sessions.start("session-a");
        assert!(!token.is_cancelled(), "sanity: freshly started token");

        let router = build_router(handle, state);
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/graph/algorithm/cancel"))
            .json(&serde_json::json!({ "session_id": "session-a" }))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.expect("JSON body");
        assert_eq!(body["cancelled"], serde_json::json!(true));

        assert!(
            token.is_cancelled(),
            "the HTTP round trip must have flipped the SAME token the algorithm loop holds"
        );
    }

    /// `POST /api/graph/algorithm/cancel` with an unknown session id returns 200, never a
    /// 404/500 (idempotent, safe no-op per `SessionRegistry::cancel`'s own contract).
    #[tokio::test]
    async fn cancel_endpoint_with_an_unknown_session_id_returns_200() {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        let handle = Arc::new(handle);

        let state = base_state();
        let router = build_router(handle, state);
        let addr = spawn_router(router).await;

        let resp = reqwest::Client::new()
            .post(format!("http://{addr}/api/graph/algorithm/cancel"))
            .json(&serde_json::json!({ "session_id": "never-started" }))
            .send()
            .await
            .expect("request through the assembled router");

        assert_eq!(resp.status(), 200);
    }
}
