use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::response::{Html, IntoResponse};
use axum::{routing::get, routing::post, Extension, Router};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

use crate::graphql::schema::{build_schema, AppSchema};
use crate::grpc::build_grpc_routes;
use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;

/// Configuration for the UrbanIntegrity API server.
pub struct ServerConfig {
    /// Address to bind the server to (default: 127.0.0.1:8080)
    pub bind_addr: String,
    /// Ollama base URL for Gemma4 LLM integration (default: http://localhost:11434/v1)
    pub ollama_url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".to_string(),
            ollama_url: "http://localhost:11434/v1".to_string(),
        }
    }
}

/// GraphQL POST handler — executes the request against the schema.
pub(crate) async fn graphql_handler(
    Extension(schema): Extension<AppSchema>,
    req: GraphQLRequest,
) -> GraphQLResponse {
    schema.execute(req.into_inner()).await.into()
}

/// GraphiQL playground — browser-friendly GraphQL IDE at /graphiql.
pub(crate) async fn graphiql_handler() -> impl IntoResponse {
    Html(
        async_graphql::http::GraphiQLSource::build()
            .endpoint("/graphql")
            .finish(),
    )
}

/// Health check — uses Extension rather than State so it can be on the merged router.
pub(crate) async fn health_ext(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
) -> axum::response::Json<crate::http::health::HealthResponse> {
    let snapshot = handle.snapshot.load();
    axum::response::Json(crate::http::health::HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        node_count: snapshot.graph.node_count() as u64,
        edge_count: snapshot.graph.edge_count() as u64,
    })
}

/// Metrics — uses Extension rather than State so it can be on the merged router.
pub(crate) async fn metrics_ext(
    Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
) -> axum::response::Json<crate::http::health::MetricsResponse> {
    let snapshot = handle.snapshot.load();
    axum::response::Json(crate::http::health::MetricsResponse {
        node_count: snapshot.graph.node_count() as u64,
        edge_count: snapshot.graph.edge_count() as u64,
        last_snapshot_event: snapshot.baked_at_event,
    })
}

/// Builds the core `/health` `/metrics` `/graphql` `/graphiql` Router with the
/// `Extension(handle)`, `Extension(schema)`, `TraceLayer` chain applied — the single place
/// this Router is constructed, reused by both `start_server` and (future) `crates/workbench`'s
/// boot sequence.
///
/// Layers applied here (`Extension(handle)`, `Extension(schema)`, `TraceLayer`) only wrap the
/// routes registered before this function returns. Callers merging in additional routes that
/// need these extensions (e.g. `crates/workbench::boot`) must build those routes as their own
/// `Router` with their own `.layer(...)` calls, then `.merge()` the two routers — never call
/// `.route()` directly on the `Router` this function returns and expect the extensions to apply.
/// Axum 0.8's `Router::layer` does not retroactively apply to routes a caller adds afterwards.
pub fn core_router(handle: Arc<GraphCoordinatorHandle>, schema: AppSchema) -> Router {
    Router::new()
        .route("/health", get(health_ext))
        .route("/metrics", get(metrics_ext))
        .route("/graphql", post(graphql_handler))
        .route("/graphiql", get(graphiql_handler))
        .layer(Extension(handle))
        .layer(Extension(schema))
        .layer(TraceLayer::new_for_http())
}

/// Start the UrbanIntegrity API server.
///
/// Serves on a single port:
/// - `GET /health`   — JSON health check
/// - `GET /metrics`  — JSON graph statistics
/// - `POST /graphql` — async-graphql endpoint
/// - `GET /graphiql` — GraphiQL playground
/// - gRPC            — tonic IntegrityService (merged via into_axum_router)
///
/// Security: Bound to 127.0.0.1 by default — localhost only.
pub async fn start_server(
    handle: Arc<GraphCoordinatorHandle>,
    config: ServerConfig,
) -> anyhow::Result<()> {
    // Build GraphQL schema with DataLoader and handle injected
    let schema = build_schema(handle.clone());

    // NOTE: tonic 0.12 internally uses axum 0.7.x, while we target axum 0.8.x.
    // Direct .merge() fails due to type mismatch. Instead, route gRPC requests via
    // axum's fallback using the tonic Routes tower service directly.
    //
    // Alternative strategy: use Routes as a Tower Service on a separate port,
    // or upgrade tonic when axum 0.8 support is complete in tonic 0.13+.
    // For v1 (local deployment), we serve gRPC on a separate port: 8081.

    let grpc_service = build_grpc_routes(handle.clone(), config.ollama_url);

    // Spawn gRPC server on port 8081 (separate from HTTP on 8080)
    let grpc_addr = {
        let base = config
            .bind_addr
            .parse::<std::net::SocketAddr>()
            .unwrap_or_else(|_| "127.0.0.1:8080".parse().unwrap());
        std::net::SocketAddr::new(base.ip(), base.port() + 1)
    };

    tokio::spawn(async move {
        tracing::info!(addr = %grpc_addr, "gRPC server listening");
        if let Err(e) = tonic::transport::Server::builder()
            .add_routes(grpc_service)
            .serve(grpc_addr)
            .await
        {
            tracing::error!(error = %e, "gRPC server error");
        }
    });

    // Build HTTP/GraphQL routes using Extension-based state
    let app = core_router(handle.clone(), schema);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "UrbanIntegrity HTTP/GraphQL server listening");
    tracing::info!("GraphQL:  http://{}/graphql", config.bind_addr);
    tracing::info!("GraphiQL: http://{}/graphiql", config.bind_addr);
    tracing::info!("Health:   http://{}/health", config.bind_addr);
    tracing::info!("gRPC:     {}", grpc_addr);

    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
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

    #[test]
    fn test_server_config_default_values() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.bind_addr, "127.0.0.1:8080");
        assert!(cfg.ollama_url.contains("11434"));
    }

    #[tokio::test]
    async fn test_graphiql_handler_returns_html() {
        let resp = graphiql_handler().await;
        let body = axum::response::IntoResponse::into_response(resp);
        assert_eq!(body.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn test_health_ext_returns_ok_status() {
        let handle = make_handle().await;
        let resp = health_ext(Extension(handle)).await;
        assert_eq!(resp.0.status, "ok");
        assert_eq!(resp.0.node_count, 0);
        assert_eq!(resp.0.edge_count, 0);
    }

    #[tokio::test]
    async fn test_metrics_ext_returns_zero_on_empty_graph() {
        let handle = make_handle().await;
        let resp = metrics_ext(Extension(handle)).await;
        assert_eq!(resp.0.node_count, 0);
        assert_eq!(resp.0.edge_count, 0);
        assert_eq!(resp.0.last_snapshot_event, 0);
    }

    #[tokio::test]
    async fn test_graphiql_handler_returns_html_body() {
        use axum::response::IntoResponse;
        let resp = graphiql_handler().await.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
    }
}
