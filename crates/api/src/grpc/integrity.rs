use chrono::DateTime;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use urban_integrity_core::{
    get_alert_by_id, graph::coordinator::GraphCoordinatorHandle, query_alerts,
    AlertFilter as CoreAlertFilter,
};

// Include generated protobuf code (produced by tonic-build in build.rs)
pub mod proto {
    tonic::include_proto!("urban_integrity");
}

use proto::integrity_service_server::IntegrityService;
use proto::*;

/// gRPC service implementation for the IntegrityService.
pub struct IntegrityServiceImpl {
    handle: Arc<GraphCoordinatorHandle>,
    ollama_url: String,
}

impl IntegrityServiceImpl {
    pub fn new(handle: Arc<GraphCoordinatorHandle>, ollama_url: String) -> Self {
        Self { handle, ollama_url }
    }
}

#[tonic::async_trait]
impl IntegrityService for IntegrityServiceImpl {
    async fn query_alerts(
        &self,
        request: Request<AlertFilter>,
    ) -> Result<Response<AlertList>, Status> {
        let filter = request.into_inner();
        tracing::info!(
            min_score = filter.min_score.unwrap_or(0.0),
            ward = filter.ward_code.as_deref().unwrap_or("all"),
            motif = filter.motif_type.as_deref().unwrap_or("all"),
            limit = filter.limit,
            "gRPC QueryAlerts"
        );
        if filter.limit > 1000 {
            return Err(Status::invalid_argument("limit must be <= 1000"));
        }

        let db_filter = CoreAlertFilter {
            // proto optional float → f64; default to 0.0 (no score filter) if omitted
            min_score: filter.min_score.unwrap_or(0.0) as f64,
            ward_code: filter.ward_code,
            motif_type: filter.motif_type,
            from_date: None,
            to_date: None,
            limit: filter.limit.max(1) as usize,
            // The API is a publishing surface. A retracted alert must never leave it.
            include_withdrawn: false,
            ..Default::default()
        };

        let records = query_alerts(self.handle.db(), &db_filter)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "SurrealDB query_alerts failed");
                Status::internal(format!("Database error: {}", e))
            })?;

        let alerts: Vec<Alert> = records
            .alerts
            .into_iter()
            .map(|r| {
                // Proto Alert: score is float (f32), detected_at is uint64 unix ms
                let score_f32 = r.score as f32;
                let detected_at_u64 = DateTime::parse_from_rfc3339(&r.detected_at)
                    .map(|dt| dt.timestamp_millis() as u64)
                    .unwrap_or(0);
                Alert {
                    id: r.id,
                    motif_type: r.motif_type,
                    score: score_f32,
                    result_json: r.result_json,
                    detected_at: detected_at_u64,
                    ward_code: r.ward_code,
                }
            })
            .collect();

        Ok(Response::new(AlertList { alerts }))
    }

    async fn get_alert(&self, request: Request<AlertId>) -> Result<Response<Alert>, Status> {
        let id = request.into_inner().id;
        tracing::info!(alert_id = %id, "gRPC GetAlert");

        match get_alert_by_id(self.handle.db(), &id).await {
            // The API is a publishing surface. A retracted alert must never leave it — the
            // same rule `list_alerts` enforces above with `include_withdrawn: false`.
            //
            // Reported as NOT_FOUND rather than a distinct code on purpose: a withdrawn alert
            // must be indistinguishable from an absent one here. The proto `Alert` message
            // carries no withdrawal field, so returning it published a disowned accusation
            // as a live finding, with the retraction visible only to a client that happened
            // to parse `result_json` (T-23-FalseAccusation).
            //
            // Withdrawn alerts stay readable on the operator's own surface: `alerts show`,
            // and `alerts list --include-withdrawn`.
            Ok(Some(r)) if r.withdrawn_at.is_some() => {
                tracing::info!(alert_id = %id, "gRPC GetAlert: withdrawn alert not published");
                Err(Status::not_found(format!("Alert {} not found", id)))
            }
            Ok(Some(r)) => {
                let score_f32 = r.score as f32;
                let detected_at_u64 = DateTime::parse_from_rfc3339(&r.detected_at)
                    .map(|dt| dt.timestamp_millis() as u64)
                    .unwrap_or(0);
                Ok(Response::new(Alert {
                    id: r.id,
                    motif_type: r.motif_type,
                    score: score_f32,
                    result_json: r.result_json,
                    detected_at: detected_at_u64,
                    ward_code: r.ward_code,
                }))
            }
            Ok(None) => Err(Status::not_found(format!("Alert {} not found", id))),
            Err(e) => {
                tracing::error!(error = %e, alert_id = %id, "SurrealDB get_alert failed");
                Err(Status::internal(format!("Database error: {}", e)))
            }
        }
    }

    async fn summarize_alert(
        &self,
        request: Request<AlertId>,
    ) -> Result<Response<AlertSummary>, Status> {
        let id = request.into_inner().id;
        tracing::info!(alert_id = %id, "gRPC SummarizeAlert — calling Ollama");

        let prompt = format!(
            "Summarise this procurement integrity alert (ID: {}) in 2-3 sentences for a public auditor. \
             Focus on the risk, affected entities, and recommended follow-up action.",
            id
        );

        let body = serde_json::json!({
            "model": "gemma3",
            "messages": [
                {
                    "role": "system",
                    "content": "You are a procurement fraud analyst. Be concise and factual."
                },
                { "role": "user", "content": prompt }
            ],
            "stream": false
        });

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/chat/completions", self.ollama_url))
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "Ollama unavailable for SummarizeAlert");
                Status::unavailable(format!("LLM unavailable: {}", e))
            })?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Status::internal(format!("LLM response parse error: {}", e)))?;

        let text = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("No summary available")
            .to_string();

        Ok(Response::new(AlertSummary {
            text,
            model: "gemma3".to_string(),
        }))
    }

    async fn get_graph_stats(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<GraphStats>, Status> {
        let snapshot = self.handle.snapshot.load();
        let node_count = snapshot.graph.node_count() as u64;
        let edge_count = snapshot.graph.edge_count() as u64;
        let last_snapshot_event = snapshot.baked_at_event;

        tracing::debug!(
            node_count,
            edge_count,
            last_snapshot_event,
            "gRPC GetGraphStats"
        );

        Ok(Response::new(GraphStats {
            node_count,
            edge_count,
            last_snapshot_event,
        }))
    }
}

/// Build a `tonic::service::Routes` object containing the IntegrityService.
/// Call `.into_axum_router()` on the result to merge into an Axum router.
pub fn build_grpc_routes(
    handle: Arc<GraphCoordinatorHandle>,
    ollama_url: String,
) -> tonic::service::Routes {
    let svc = IntegrityServiceImpl::new(handle, ollama_url);
    let server = proto::integrity_service_server::IntegrityServiceServer::new(svc);
    let mut builder = tonic::service::Routes::builder();
    builder.add_service(server);
    builder.routes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::integrity_service_server::IntegrityService;
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
    async fn test_get_graph_stats_returns_zero_on_empty_graph() {
        let handle = make_handle().await;
        let svc = IntegrityServiceImpl::new(handle, "http://localhost:11434".to_string());
        let req = tonic::Request::new(proto::Empty {});
        let resp = svc.get_graph_stats(req).await.unwrap();
        assert_eq!(resp.get_ref().node_count, 0);
        assert_eq!(resp.get_ref().edge_count, 0);
        assert_eq!(resp.get_ref().last_snapshot_event, 0);
    }

    #[tokio::test]
    async fn test_query_alerts_returns_invalid_argument_when_limit_over_1000() {
        let handle = make_handle().await;
        let svc = IntegrityServiceImpl::new(handle, "http://localhost:11434".to_string());
        let filter = proto::AlertFilter {
            min_score: None,
            ward_code: None,
            motif_type: None,
            from_date: None,
            to_date: None,
            limit: 1001,
        };
        let req = tonic::Request::new(filter);
        let err = svc.query_alerts(req).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_build_grpc_routes_does_not_panic() {
        let handle = make_handle().await;
        let _ = build_grpc_routes(handle, "http://localhost:11434".to_string());
    }
}
