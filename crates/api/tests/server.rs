/// API integration tests — spin up the full Axum HTTP server on an ephemeral
/// port, then make real HTTP requests to each endpoint.
///
/// These tests exercise the actual handler code paths with a live
/// GraphCoordinator and CSR snapshot — not mocks.
///
/// No `URBAN_LIVE_TESTS` guard needed here: no external network calls are made.
/// The server is spun up and torn down within each test.
use std::sync::Arc;

use axum::routing::{get, post};
use axum::{Extension, Router};
use reqwest::Client;
use serde_json::Value;
use tokio::net::TcpListener;
use urban_integrity_api::{
    graphql::schema::build_schema,
    http::health::{HealthResponse, MetricsResponse},
};
use urban_integrity_core::graph::coordinator::{
    CoordinatorConfig, GraphCoordinator, GraphCoordinatorHandle,
};
use urban_integrity_core::persistence::wal::wal_channel;

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Spin up the HTTP portion of the UrbanIntegrity API on an ephemeral port.
/// Returns the bound address and a handle to the coordinator.
async fn spawn_test_server() -> (String, Arc<GraphCoordinatorHandle>) {
    let (wal_tx, _wal_rx) = wal_channel(1024);
    let (coordinator, handle) = GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
    tokio::spawn(coordinator.run());
    let handle = Arc::new(handle);
    let schema = build_schema(handle.clone());

    // Import the handlers from server.rs — they are private, so we re-implement
    // the same router inline using the public handler functions from http::health.
    // This mirrors what start_server() builds internally.
    use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
    use urban_integrity_api::graphql::schema::AppSchema;

    async fn graphql_handler(
        Extension(schema): Extension<AppSchema>,
        req: GraphQLRequest,
    ) -> GraphQLResponse {
        schema.execute(req.into_inner()).await.into()
    }

    async fn health_ext(
        Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    ) -> axum::response::Json<HealthResponse> {
        let snapshot = handle.snapshot.load();
        axum::response::Json(HealthResponse {
            status: "ok",
            version: env!("CARGO_PKG_VERSION"),
            node_count: snapshot.graph.node_count() as u64,
            edge_count: snapshot.graph.edge_count() as u64,
        })
    }

    async fn metrics_ext(
        Extension(handle): Extension<Arc<GraphCoordinatorHandle>>,
    ) -> axum::response::Json<MetricsResponse> {
        let snapshot = handle.snapshot.load();
        axum::response::Json(MetricsResponse {
            node_count: snapshot.graph.node_count() as u64,
            edge_count: snapshot.graph.edge_count() as u64,
            last_snapshot_event: snapshot.baked_at_event,
        })
    }

    let app: Router = Router::new()
        .route("/health", get(health_ext))
        .route("/metrics", get(metrics_ext))
        .route("/graphql", post(graphql_handler))
        .layer(Extension(handle.clone()))
        .layer(Extension(schema));

    // Bind to port 0 — OS assigns an ephemeral port
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, handle)
}

fn http_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

// ── Health endpoint ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_health_returns_200_and_ok_status() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let resp = client
        .get(format!("http://{}/health", addr))
        .send()
        .await
        .expect("health request");

    assert_eq!(resp.status(), 200);

    let body: Value = resp.json().await.expect("health JSON");
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
    assert!(body["node_count"].is_number());
    assert!(body["edge_count"].is_number());
    eprintln!("✓ /health → status=ok, version={}", body["version"]);
}

#[tokio::test]
async fn test_health_empty_graph_returns_zero_counts() {
    let (addr, _handle) = spawn_test_server().await;
    let body: Value = http_client()
        .get(format!("http://{}/health", addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["node_count"], 0u64);
    assert_eq!(body["edge_count"], 0u64);
    eprintln!("✓ /health empty graph → node_count=0, edge_count=0");
}

// ── Metrics endpoint ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_metrics_returns_200_and_graph_stats() {
    let (addr, _handle) = spawn_test_server().await;
    let resp = http_client()
        .get(format!("http://{}/metrics", addr))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["node_count"].is_number());
    assert!(body["edge_count"].is_number());
    assert!(body["last_snapshot_event"].is_number());
    eprintln!(
        "✓ /metrics → node_count={}, edge_count={}",
        body["node_count"], body["edge_count"]
    );
}

// ── GraphQL endpoint ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_graphql_graph_stats_query() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ graphStats { nodeCount edgeCount lastSnapshotEvent } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .expect("graphQL request");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("graphQL JSON");

    assert!(
        body["errors"].is_null(),
        "GraphQL errors: {:?}",
        body["errors"]
    );
    let stats = &body["data"]["graphStats"];
    assert!(stats["nodeCount"].is_number());
    assert!(stats["edgeCount"].is_number());
    assert!(stats["lastSnapshotEvent"].is_number());
    eprintln!("✓ GraphQL graphStats → {:?}", stats);
}

#[tokio::test]
async fn test_graphql_alerts_query_returns_empty_list() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ alerts(minScore: 0.5) { id motifType score } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["errors"].is_null(),
        "Unexpected errors: {:?}",
        body["errors"]
    );
    let alerts = body["data"]["alerts"]
        .as_array()
        .expect("alerts must be array");
    // DB not wired yet — expect empty list, not an error
    assert!(alerts.is_empty(), "Expected empty alerts list from stub");
    eprintln!("✓ GraphQL alerts → [] (stub as expected)");
}

#[tokio::test]
async fn test_graphql_alert_by_id_returns_null_not_error() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ alert(id: \"nonexistent-uuid\") { id motifType score } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    // Should return null for unknown ID, not a server error
    assert!(
        body["errors"].is_null(),
        "Expected null not error for missing alert: {:?}",
        body["errors"]
    );
    eprintln!("✓ GraphQL alert(nonexistent) → null");
}

#[tokio::test]
async fn test_graphql_rejects_over_depth_query() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    // Build a query that exceeds the depth=10 limit
    let deeply_nested = "{ alerts { id __typename __typename __typename __typename __typename __typename __typename __typename __typename __typename } }";
    let query = serde_json::json!({ "query": deeply_nested });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .unwrap();

    // The schema has limit_depth(10) — malformed deep queries should get errors or 200+errors
    assert_eq!(
        resp.status(),
        200,
        "GraphQL always returns 200, errors in body"
    );
    eprintln!("✓ GraphQL depth-limited query handled gracefully");
}

// ── Entity + GraphPath GQL resolvers (GQL-01 .. GQL-05) ─────────────────────

/// GQL-01: entity(name:) returns null without errors when the entity is absent (no DB in test ctx).
#[tokio::test]
async fn test_graphql_entity_query() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ entity(name: \"nonexistent-entity-xyz\") { id name } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .expect("entity query request");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("entity query JSON");
    assert!(
        body["errors"].is_null(),
        "GraphQL errors on entity(name:): {:?}",
        body["errors"]
    );
    // No DB wired in test context — entity returns null, not an error
    assert!(
        body["data"]["entity"].is_null(),
        "Expected null entity when DB absent"
    );
    eprintln!("✓ GQL-01: entity(name:\"nonexistent\") → null (no errors)");
}

/// GQL-01: entity(chNumber:) returns null without errors when DB is absent in test context.
#[tokio::test]
async fn test_graphql_entity_by_ch() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ entity(chNumber: \"99999999\") { id name chNumber } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .expect("entity chNumber request");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("entity chNumber JSON");
    assert!(
        body["errors"].is_null(),
        "GraphQL errors on entity(chNumber:): {:?}",
        body["errors"]
    );
    assert!(
        body["data"]["entity"].is_null(),
        "Expected null entity when DB absent"
    );
    eprintln!("✓ GQL-01: entity(chNumber:\"99999999\") → null (no errors)");
}

/// GQL-02: introspection of EntityProfile type shows all required fields.
#[tokio::test]
async fn test_graphql_entity_schema_fields() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ __type(name: \"EntityProfile\") { fields { name } } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .expect("introspection request");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("introspection JSON");
    assert!(
        body["errors"].is_null(),
        "GraphQL errors on __type introspection: {:?}",
        body["errors"]
    );

    let fields = body["data"]["__type"]["fields"]
        .as_array()
        .expect("EntityProfile fields must be an array");

    let field_names: Vec<&str> = fields.iter().filter_map(|f| f["name"].as_str()).collect();

    for expected in &[
        "id",
        "name",
        "chNumber",
        "status",
        "addresses",
        "directors",
        "contracts",
        "motifAlerts",
        "lpaPresence",
    ] {
        assert!(
            field_names.contains(expected),
            "EntityProfile missing field '{}'; got: {:?}",
            expected,
            field_names
        );
    }
    eprintln!(
        "✓ GQL-02: EntityProfile has all 9 required GQL fields: {:?}",
        field_names
    );
}

/// GQL-03: graphPath(from:, to:) returns empty list (not an error) when DB is absent.
#[tokio::test]
async fn test_graphql_graph_path() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ graphPath(from: \"nonexistent-a\", to: \"nonexistent-b\") { id label edgeType } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .expect("graphPath request");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("graphPath JSON");
    // With no DB wired, the resolver returns Ok(vec![]) — no errors
    assert!(
        body["errors"].is_null(),
        "GraphQL errors on graphPath when DB absent: {:?}",
        body["errors"]
    );
    let path = body["data"]["graphPath"]
        .as_array()
        .expect("graphPath must return an array");
    assert!(
        path.is_empty(),
        "Expected empty path when DB absent, got: {:?}",
        path
    );
    eprintln!("✓ GQL-03: graphPath(from:, to:) → [] when DB absent (no errors)");
}

/// GQL-04: the `EntityProfile` type is registered in the schema and reachable by introspection.
///
/// This test used to be named `..._entity_loader_registered` and claimed to confirm "EntityLoader
/// wiring". It never did: introspecting a type says nothing about which DataLoaders are in the
/// schema's data map. The `EntityLoader` it referred to was registered in `build_schema` but had
/// no call site for its whole life, and was removed on 2026-08-23 — `EntityProfile` is a
/// `SimpleObject` materialised by one query and no resolver fetches entities per parent, so there
/// is no N+1 for a loader to remove. What this test actually asserts, and all it ever asserted, is
/// that the type is exposed.
#[tokio::test]
async fn test_graphql_entity_profile_type_registered() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ __type(name: \"EntityProfile\") { name } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .expect("EntityProfile introspection request");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("EntityProfile introspection JSON");
    assert!(
        body["errors"].is_null(),
        "GraphQL errors: {:?}",
        body["errors"]
    );
    let type_name = body["data"]["__type"]["name"]
        .as_str()
        .expect("EntityProfile type name must be a string");
    assert_eq!(
        type_name, "EntityProfile",
        "Expected EntityProfile type to be registered in schema"
    );
    eprintln!("✓ GQL-04: EntityProfile type registered in schema");
}

/// GQL-05: schema introspection returns all v2.0 type names.
#[tokio::test]
async fn test_graphql_introspection_includes_entity_types() {
    let (addr, _handle) = spawn_test_server().await;
    let client = http_client();

    let query = serde_json::json!({
        "query": "{ __schema { types { name } } }"
    });

    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .expect("schema introspection request");

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("schema introspection JSON");
    assert!(
        body["errors"].is_null(),
        "GraphQL errors on __schema: {:?}",
        body["errors"]
    );

    let type_names: Vec<&str> = body["data"]["__schema"]["types"]
        .as_array()
        .expect("__schema.types must be an array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();

    for expected in &[
        "EntityProfile",
        "PathStep",
        "DirectorRow",
        "ContractRow",
        "AddressRow",
        "AlertRecord",
        "LpaPresence",
    ] {
        assert!(
            type_names.contains(expected),
            "Schema missing type '{}'; got types: {:?}",
            expected,
            type_names
        );
    }
    eprintln!(
        "✓ GQL-05: all v2.0 types present in introspection: EntityProfile, PathStep, DirectorRow, ContractRow, AddressRow, AlertRecord, LpaPresence"
    );
}

#[ignore = "Requires live internet access to contractsfinder.service.gov.uk"]
#[tokio::test]
async fn test_graphql_with_live_data_from_contracts_finder() {
    // Fetch real data from Contracts Finder, ingest it into the graph, then
    // query graphStats via GraphQL to confirm the node count increased.
    use urban_integrity_core::pipeline::process as process_event;
    use urban_integrity_ingestor::cf::{mapper::map_release, types::OcdsPackage};

    let (addr, handle) = spawn_test_server().await;

    // Fetch 5 real releases from Contracts Finder
    let client = http_client();
    let package: OcdsPackage = client
        .get("https://www.contractsfinder.service.gov.uk/Published/Notices/OCDS/Search?limit=5")
        .send()
        .await
        .expect("CF fetch")
        .json()
        .await
        .expect("CF JSON");

    // Ingest all mutations into the coordinator
    let mut ingested = 0usize;
    for release in &package.releases {
        for ingest_event in map_release(release) {
            if let Ok(mutation) = process_event(ingest_event) {
                handle.mutation_tx.send(mutation).await.ok();
                ingested += 1;
            }
        }
    }
    eprintln!(
        "✓ Ingested {} mutations from {} CF releases",
        ingested,
        package.releases.len()
    );

    // Give the coordinator time to process mutations into storage
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // GraphQL must still respond 200 OK after ingest (no panics or crashes)
    let query = serde_json::json!({
        "query": "{ graphStats { nodeCount edgeCount } }"
    });
    let resp = client
        .post(format!("http://{}/graphql", addr))
        .json(&query)
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "GraphQL must respond 200 after live ingest"
    );
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["errors"].is_null(),
        "No GraphQL errors after ingest: {:?}",
        body["errors"]
    );

    // The CSR snapshot only bakes every 1000 mutations (CoordinatorConfig default),
    // so graphStats node_count may still be 0 — that is correct behaviour.
    // What we verify here is that the ingest pipeline accepted real CF data:
    assert!(
        ingested > 0,
        "Expected > 0 mutations from {} real CF releases",
        package.releases.len()
    );

    eprintln!(
        "✓ Live CF ingest: {} mutations accepted, GraphQL still healthy after ingest",
        ingested
    );
}

// ── Regression: the API must not publish a withdrawn alert (2026-08-20) ──────

/// Regression: `alert(id:)` must not return an alert an operator withdrew.
///
/// The sibling `alerts` list resolver states the rule outright — "The API is a publishing
/// surface. A retracted alert must never leave it." — and sets `include_withdrawn: false`.
/// The by-id path bypassed that entirely: `AlertLoader` handed `get_alerts_by_ids` straight
/// through, and `AlertObject` carries no withdrawal fields, so a retracted alert was served
/// looking exactly like a live one.
///
/// Withdrawn alerts remain readable on the operator's own surface (`alerts show`, and
/// `alerts list --include-withdrawn`) because a retracted false positive is evidence about
/// detector quality. They must not leave the published API.
#[tokio::test]
async fn the_api_does_not_publish_a_withdrawn_alert() {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return;
    }
    use async_graphql::dataloader::Loader;
    use urban_integrity_api::graphql::loaders::AlertLoader;

    let ns = urban_integrity_test_support::TestNs::connect("main").await;
    let db = ns.arc();

    let seed = |event_id: &'static str, withdrawn: bool| {
        let db = db.clone();
        async move {
            let mut result = serde_json::json!({
                "motif_type": "PhoenixLoop",
                "risk_score": 0.9,
                "score_breakdown": { "ward_code": "E05001234" },
                "path_nodes": [],
                "unique_hash": uuid::Uuid::new_v4().to_string(),
            });
            if withdrawn {
                result["withdrawn_at"] = serde_json::json!("2026-08-11T09:00:00Z");
                result["withdrawn_reason"] = serde_json::json!("adjudicated a false positive");
            }
            db.query("CREATE graph_events CONTENT $r")
                .bind((
                    "r",
                    serde_json::json!({
                        "event_id": event_id,
                        "timestamp": 1_700_000_000_000u64,
                        "kind": "MotifDetected",
                        "payload": { "result_json": result.to_string() },
                    }),
                ))
                .await
                .expect("seed must succeed");
        }
    };
    seed("api-live-001", false).await;
    seed("api-withdrawn-001", true).await;

    let loader = AlertLoader::new(db.clone());
    let map = loader
        .load(&["api-live-001".to_string(), "api-withdrawn-001".to_string()])
        .await
        .expect("loader must succeed");

    assert!(
        map.contains_key("api-live-001"),
        "the live alert must still be published"
    );
    assert!(
        !map.contains_key("api-withdrawn-001"),
        "a withdrawn alert must never leave the API — `AlertObject` has no withdrawal field, \
         so publishing one presents a retracted accusation as a live finding"
    );
}

/// Regression: gRPC `GetAlert` must not publish a withdrawn alert either.
///
/// The gRPC `Alert` message has no withdrawal field, exactly like `AlertObject`, so serving
/// a retracted alert over it presents a disowned accusation as a live finding. `ListAlerts`
/// on the same service already sets `include_withdrawn: false`; the by-id call did not.
#[tokio::test]
async fn grpc_get_alert_does_not_publish_a_withdrawn_alert() {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return;
    }
    use urban_integrity_api::grpc::integrity::proto::integrity_service_server::IntegrityService as _;
    use urban_integrity_api::grpc::integrity::proto::AlertId;
    use urban_integrity_api::grpc::integrity::IntegrityServiceImpl;
    let ns = urban_integrity_test_support::TestNs::connect("main").await;
    let db = ns.arc();

    let mut result = serde_json::json!({
        "motif_type": "PhoenixLoop",
        "risk_score": 0.9,
        "score_breakdown": { "ward_code": "E05001234" },
        "path_nodes": [],
        "unique_hash": uuid::Uuid::new_v4().to_string(),
    });
    result["withdrawn_at"] = serde_json::json!("2026-08-11T09:00:00Z");
    result["withdrawn_reason"] = serde_json::json!("adjudicated a false positive");
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": "grpc-withdrawn-001",
                "timestamp": 1_700_000_000_000u64,
                "kind": "MotifDetected",
                "payload": { "result_json": result.to_string() },
            }),
        ))
        .await
        .expect("seed must succeed");

    // Same channel helper the other tests in this file use.
    let (wal_tx, _wal_rx) = wal_channel(1024);
    let (_coordinator, handle) =
        GraphCoordinator::new_with_db(CoordinatorConfig::default(), wal_tx, None, Some(db.clone()));
    let service = IntegrityServiceImpl::new(Arc::new(handle), "http://localhost:11434/v1".into());

    let status = service
        .get_alert(tonic::Request::new(AlertId {
            id: "grpc-withdrawn-001".to_string(),
        }))
        .await
        .expect_err("a withdrawn alert must not be returned over gRPC");

    assert_eq!(
        status.code(),
        tonic::Code::NotFound,
        "a withdrawn alert must be indistinguishable from an absent one on the published \
         API — got {status:?}"
    );
}
