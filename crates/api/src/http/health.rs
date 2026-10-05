use axum::{extract::State, response::Json};
use serde::Serialize;
use std::sync::Arc;
use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;

/// Response body for `GET /health`.
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub node_count: u64,
    pub edge_count: u64,
}

/// Response body for `GET /metrics`.
#[derive(Serialize)]
pub struct MetricsResponse {
    pub node_count: u64,
    pub edge_count: u64,
    pub last_snapshot_event: u64,
}

/// Health check handler. Returns engine status with live graph counts.
/// Responds in < 5ms — reads only from the pre-baked Arc<CsrSnapshot>.
pub async fn health_handler(
    State(handle): State<Arc<GraphCoordinatorHandle>>,
) -> Json<HealthResponse> {
    let snapshot = handle.snapshot.load();
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        node_count: snapshot.graph.node_count() as u64,
        edge_count: snapshot.graph.edge_count() as u64,
    })
}

/// Metrics handler. Returns graph statistics for monitoring.
pub async fn metrics_handler(
    State(handle): State<Arc<GraphCoordinatorHandle>>,
) -> Json<MetricsResponse> {
    let snapshot = handle.snapshot.load();
    Json(MetricsResponse {
        node_count: snapshot.graph.node_count() as u64,
        edge_count: snapshot.graph.edge_count() as u64,
        last_snapshot_event: snapshot.baked_at_event,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::{
        graph::coordinator::{CoordinatorConfig, GraphCoordinator},
        persistence::wal::wal_channel,
    };

    async fn make_handle() -> Arc<GraphCoordinatorHandle> {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        Arc::new(handle)
    }

    #[tokio::test]
    async fn test_health_handler_returns_ok_status() {
        let handle = make_handle().await;
        let Json(resp) = health_handler(State(handle)).await;
        assert_eq!(resp.status, "ok");
        assert!(!resp.version.is_empty(), "version must be non-empty");
        assert_eq!(resp.node_count, 0, "empty graph must have 0 nodes");
        assert_eq!(resp.edge_count, 0, "empty graph must have 0 edges");
    }

    #[tokio::test]
    async fn test_metrics_handler_returns_zero_on_empty_graph() {
        let handle = make_handle().await;
        let Json(resp) = metrics_handler(State(handle)).await;
        assert_eq!(resp.node_count, 0);
        assert_eq!(resp.edge_count, 0);
        assert_eq!(resp.last_snapshot_event, 0);
    }
}
