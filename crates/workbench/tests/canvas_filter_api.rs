//! Plan 45-14 (CNV-03) through the router: **what a save writes is exactly what was visible.**
//!
//! # Why this is a router test and not a handler test
//!
//! 44-18's precedent, restated by 45-08: a test that calls a handler proves the function works and
//! says nothing about whether the route is wired, whether the path is spelled the way the browser
//! spells it, or whether the body deserialises. This plan adds two routes that three later plans
//! will build on, and every one of those failures reaches them as a blank screen.
//!
//! # The fixture is a real graph AND a real event log
//!
//! Both, because the two halves of this chain read different stores. The neighbourhood expansion
//! reads `graph_events` through `query_entity_neighbourhood` (45-08's decision — the CSR snapshot
//! omits identity links); the filter and the save read the baked `CsrSnapshot`. Seeding only one
//! would leave half the chain answering about an empty world, which is exactly the shape of failure
//! this project has shipped before.
//!
//! Namespaces come from `urban-integrity-test-support`, per the mandatory rule in docs/TESTING.md, and the
//! guard is bound for the whole test.

use std::collections::BTreeSet;
use std::sync::Arc;

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_schema::types::edge::{AwardedToData, EdgeMeta, EdgeWeight, RepresentsData};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{
    ContractData, EntityData, LocationData, NodeWeight, WardData,
};
use urban_integrity_test_support::{db_tests_enabled, TestNs};
use urban_integrity_workbench::boot::WorkbenchState;
use urban_integrity_workbench::graph::fixture::workbench_from_graph_with_db;
use urban_integrity_workbench_dto::canvas_filter::{
    CanvasFilterRequest, CanvasFilterView, MissingEvidence,
};
use urban_integrity_workbench_dto::investigation::{
    InvestigationDetailView, PinOriginView, PinTargetView, SaveSubgraphRequest,
    SaveSubgraphResultView, SavedInvestigationView,
};
use urban_integrity_workbench_dto::neighbourhood::NeighbourhoodView;

// ── Synthetic ids only: `scripts/check-no-analysis-literals.sh` forbids an analysis-specific
// literal anywhere in this crate, and an `X05…` ward code can never name a real ONS ward. ──

const SEED: &str = "GB-COH:X06999001";
const WARD_A: &str = "ward:X05999001";
const WARD_B: &str = "ward:X05999002";
/// Above `CLUSTER_COLLAPSE_THRESHOLD`, so a fixture derived from this one can collapse.
const CONTRACTS: usize = 26;
/// Two of the contracts also sit at an address, so the node-type checklist has something to remove
/// that is neither the seed nor a ward.
const LOCATIONS: usize = 2;
/// A second, DISCONNECTED component: an entity with one award, reachable from nothing the seed
/// touches.
///
/// It exists for one reason and the reason is a finding about this file's own instrument. Without
/// it the population and the two-hop expansion were the same set, so the mutation this test's own
/// doc comment names — a save that re-runs over the whole population instead of the canvas's focus
/// set — produced an identical answer and the test stayed **green**. A test that cannot tell the
/// difference between the scope and the population cannot prove a save honoured the scope.
const OUTSIDER: &str = "GB-COH:X06999002";
const OUTSIDE_CONTRACT: &str = "CF:x9999-cf-09001";

fn contract_id(i: usize) -> String {
    format!("CF:x9999-cf-{i:05}")
}

fn location_id(i: usize) -> String {
    format!("location:{i}-x9999-street")
}

fn meta() -> EdgeMeta {
    EdgeMeta {
        created_at: chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        effective_from: None,
        effective_to: None,
    }
}

/// The one graph both halves of the chain read, as Rust values.
///
/// Shape: the seed is awarded every contract (hop 1); each ward represents half the contracts and
/// two contracts sit at an address (hop 2). So one hop and two hops are genuinely different sets,
/// which is what lets the save test assert that an expansion added nodes.
#[allow(clippy::type_complexity)]
fn fixture_graph() -> (
    Vec<(EntityId, NodeWeight)>,
    Vec<(EntityId, EntityId, EdgeWeight)>,
) {
    let mut nodes: Vec<(EntityId, NodeWeight)> = vec![(
        EntityId(SEED.to_string()),
        NodeWeight::Entity(EntityData {
            name: "X9999 Example Holdings Ltd".to_string(),
            company_number: Some("X06999001".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    )];
    let mut edges: Vec<(EntityId, EntityId, EdgeWeight)> = Vec::new();

    for i in 1..=CONTRACTS {
        nodes.push((
            EntityId(contract_id(i)),
            NodeWeight::Contract(ContractData {
                name: format!("X9999 CF Award {i:05}"),
                ocid: None,
                value_gbp: Some(125_000.0),
                status: Some("awarded".to_string()),
                date_published: Some("2024-03-01".to_string()),
                ward_code: None,
                tender_ref: Some(format!("X9999-TENDER-{i:05}")),
                application_ref: None,
                application_received_date: None,
                applicant_provenance: None,
                portal_detail_handle: None,
            }),
        ));
        edges.push((
            EntityId(contract_id(i)),
            EntityId(SEED.to_string()),
            EdgeWeight::AwardedTo {
                meta: meta(),
                data: AwardedToData {
                    details: "seeded".to_string(),
                },
            },
        ));
    }

    for (ward, range) in [
        (WARD_A, 1..=CONTRACTS / 2),
        (WARD_B, (CONTRACTS / 2 + 1)..=CONTRACTS),
    ] {
        nodes.push((
            EntityId(ward.to_string()),
            NodeWeight::Ward(WardData {
                name: ward.to_string(),
            }),
        ));
        for i in range {
            edges.push((
                EntityId(ward.to_string()),
                EntityId(contract_id(i)),
                EdgeWeight::Represents {
                    meta: meta(),
                    data: RepresentsData {
                        details: "seeded".to_string(),
                    },
                },
            ));
        }
    }

    nodes.push((
        EntityId(OUTSIDER.to_string()),
        NodeWeight::Entity(EntityData {
            name: "X9999 Unrelated Ltd".to_string(),
            company_number: Some("X06999002".to_string()),
            status: Default::default(),
            incorporation_date: None,
        }),
    ));
    nodes.push((
        EntityId(OUTSIDE_CONTRACT.to_string()),
        NodeWeight::Contract(ContractData {
            name: "X9999 Unrelated Award".to_string(),
            ocid: None,
            value_gbp: Some(9_000.0),
            status: Some("awarded".to_string()),
            date_published: Some("2024-03-01".to_string()),
            ward_code: None,
            tender_ref: Some("X9999-TENDER-09001".to_string()),
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        }),
    ));
    edges.push((
        EntityId(OUTSIDE_CONTRACT.to_string()),
        EntityId(OUTSIDER.to_string()),
        EdgeWeight::AwardedTo {
            meta: meta(),
            data: AwardedToData {
                details: "seeded".to_string(),
            },
        },
    ));

    for i in 1..=LOCATIONS {
        nodes.push((
            EntityId(location_id(i)),
            NodeWeight::Location(LocationData {
                name: format!("{i} X9999 Street"),
                postcode: Some("X0 1AA".to_string()),
                address_line_1: None,
                locality: None,
            }),
        ));
        edges.push((
            EntityId(contract_id(i)),
            EntityId(location_id(i)),
            EdgeWeight::LocatedAt {
                meta: meta(),
                data: urban_integrity_schema::types::edge::LocatedAtData {
                    details: "seeded".to_string(),
                },
            },
        ));
    }

    (nodes, edges)
}

fn node_row(event_id: &str, entity_id: &str, weight: &NodeWeight) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": 1_700_000_000_000u64,
        "kind": "NodeAdded",
        "payload": {
            "entity_id": entity_id,
            // Serialised from the real type, never hand-written: the read path parses this with
            // the same serde, and a hand-written shape that drifted would make every assertion
            // below a statement about a fiction.
            "weight_json": serde_json::to_string(weight).expect("node weight must serialise"),
        },
    })
}

fn edge_row(event_id: &str, src: &str, dst: &str, weight: &EdgeWeight) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "timestamp": 1_700_000_000_000u64,
        "kind": "EdgeAdded",
        "payload": {
            "src_id": src,
            "dst_id": dst,
            "weight_json": serde_json::to_string(weight).expect("edge weight must serialise"),
        },
    })
}

/// Write the same graph into `graph_events`, fail-loud.
async fn seed_events(db: &Surreal<Any>) {
    let (nodes, edges) = fixture_graph();
    let mut rows: Vec<serde_json::Value> = nodes
        .iter()
        .enumerate()
        .map(|(i, (id, weight))| node_row(&format!("x9999-n{i:05}"), &id.0, weight))
        .collect();
    rows.extend(edges.iter().enumerate().map(|(i, (src, dst, weight))| {
        edge_row(&format!("x9999-e{i:05}"), &src.0, &dst.0, weight)
    }));

    db.query("INSERT INTO graph_events $rows")
        .bind(("rows", serde_json::Value::Array(rows)))
        .await
        .expect("seed: query")
        .check()
        .expect("seed: statement");

    let counted: Option<usize> = db
        .query("SELECT count() FROM graph_events WHERE kind = 'EdgeAdded' GROUP ALL")
        .await
        .expect("count: query")
        .check()
        .expect("count: statement")
        .take((0, "count"))
        .expect("count: take");
    assert_eq!(
        counted,
        Some(edges.len()),
        "the batched INSERT did not land in full — every assertion below would be comparing a \
         read path against a fiction"
    );
}

async fn spawn_router(
    handle: &Arc<GraphCoordinatorHandle>,
    state: &Arc<WorkbenchState>,
) -> std::net::SocketAddr {
    let router =
        urban_integrity_workbench::boot::build_router(Arc::clone(handle), Arc::clone(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

async fn seeded_router(db_name: &str) -> (TestNs, std::net::SocketAddr) {
    let ns = TestNs::connect(db_name).await;
    seed_events(&ns).await;
    let (nodes, edges) = fixture_graph();
    let (handle, state) = workbench_from_graph_with_db(&nodes, &edges, &[], ns.arc()).await;
    let addr = spawn_router(&handle, &state).await;
    (ns, addr)
}

/// Every filter box ticked and nothing else set — the inactive baseline.
fn all_ticked(
    facets: &urban_integrity_workbench_dto::canvas_filter::FilterFacets,
) -> CanvasFilterRequest {
    CanvasFilterRequest {
        node_kinds: facets.node_kinds.iter().map(|k| k.value.clone()).collect(),
        edge_types: facets.edge_types.iter().map(|e| e.value.clone()).collect(),
        from_date: None,
        to_date: None,
        undated_edges: MissingEvidence::Keep,
        ward_id: None,
        min_p_c: None,
        unscored_nodes: MissingEvidence::Exclude,
        scope_ids: vec![],
    }
}

async fn expand(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    hops: &str,
) -> NeighbourhoodView {
    client
        .get(format!("http://{addr}/api/entity/neighbourhood"))
        .query(&[("id", SEED), ("hops", hops), ("node_cap", "250")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// The canvas's own focus set for an expansion: the seed plus everything the walk returned.
/// Mirrors `workbench-web`'s `canvas_focus_ids` — `workbench-web` is a binary crate with no
/// library target, so this crate cannot call it, and the duplication is named rather than hidden.
fn focus_ids(view: &NeighbourhoodView) -> Vec<String> {
    let mut ids: Vec<String> = view.seeds.iter().map(|s| s.id.clone()).collect();
    ids.extend(view.nodes.iter().map(|n| n.entity.id.clone()));
    ids
}

async fn filter(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    request: &CanvasFilterRequest,
) -> CanvasFilterView {
    client
        .post(format!("http://{addr}/api/graph/canvas-filter"))
        .json(request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn set(ids: &[String]) -> BTreeSet<&str> {
    ids.iter().map(String::as_str).collect()
}

/// **The whole chain: expand, filter, save, reopen — and what came back is exactly what was on
/// screen.**
///
/// Built so it cannot pass vacuously. Every one of these would be true of a save that quietly
/// re-ran the unfiltered query, so none of them is the assertion: the set is non-empty, the set is
/// a subset of the population, the set has the right kind of ids. What is asserted is set equality
/// against the filtered visible set, with three things established first:
///
/// 1. the two-hop expansion genuinely **adds** nodes to the one-hop one (counts compared),
/// 2. the filter genuinely **removes** nodes from the two-hop set (counts compared),
/// 3. the filtered and unfiltered populations genuinely **differ** (sets compared).
///
/// PROVE THIS CAN FAIL: in `investigation::save_subgraph_handler`, pass `&[]` instead of
/// `&filter.scope_ids` to `gather` — the save that re-runs over the whole population. The
/// recomputation then disagrees with the browser and the save 409s, which fails this test at the
/// status assertion; removing the expected-ids comparison as well makes it save the population and
/// fail at the equality. Both mutations were applied and reverted.
#[tokio::test]
async fn db_a_saved_subgraph_is_exactly_what_was_visible() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr) = seeded_router("canvas_filter_saved_equals_visible").await;
    let client = reqwest::Client::new();

    // ── 1. The expansion adds ───────────────────────────────────────────────────────────────
    let near = expand(&client, addr, "1").await;
    let far = expand(&client, addr, "2").await;
    let near_ids = focus_ids(&near);
    let far_ids = focus_ids(&far);
    assert_eq!(
        near_ids.len(),
        CONTRACTS + 1,
        "one hop from the seed is the seed and every contract"
    );
    assert!(
        far_ids.len() > near_ids.len() && set(&far_ids).is_superset(&set(&near_ids)),
        "non-vacuity: two hops must actually add nodes ({} then {})",
        near_ids.len(),
        far_ids.len()
    );

    // ── 2. The filter removes ───────────────────────────────────────────────────────────────
    let facets_probe = filter(
        &client,
        addr,
        &CanvasFilterRequest {
            scope_ids: far_ids.clone(),
            ..all_ticked(
                &urban_integrity_workbench_dto::canvas_filter::FilterFacets {
                    node_kinds: vec![],
                    edge_types: vec![],
                    wards: vec![],
                    nodes_with_a_score: 0,
                    highest_score: None,
                    edges_with_a_date: 0,
                    edges_without_a_date: 0,
                },
            )
        },
    )
    .await;
    let mut request = all_ticked(&facets_probe.facets);
    request.scope_ids = far_ids.clone();
    let unfiltered = filter(&client, addr, &request).await;
    assert!(
        !unfiltered.filter_active,
        "every box ticked is not a filter: {:?}",
        unfiltered.removed
    );
    assert_eq!(unfiltered.candidate_nodes, far_ids.len() as u64);

    // Untick Entity and Location: the seed and the two addresses go, the contracts and wards stay.
    request.node_kinds = vec!["Contract".to_string(), "Ward".to_string()];
    let filtered = filter(&client, addr, &request).await;
    assert!(filtered.filter_active);
    assert_eq!(
        filtered.node_ids.len(),
        CONTRACTS + 2,
        "every contract and both wards"
    );
    assert_eq!(
        filtered.removed.by_node_type,
        1 + LOCATIONS as u64,
        "the seed and the addresses"
    );
    assert_ne!(
        set(&filtered.node_ids),
        set(&far_ids),
        "non-vacuity: the filtered and unfiltered populations must differ, or an equality against \
         either proves nothing about which one was saved"
    );

    // Non-vacuity for the OTHER mutation this test claims to catch: the whole population must be
    // strictly bigger than the canvas's focus set, or "the save re-ran over the whole population"
    // and "the save honoured the scope" produce the same answer and the test proves neither. This
    // assertion exists because the first version of this file did not have it and that mutation
    // stayed green.
    let whole_population = filter(
        &client,
        addr,
        &CanvasFilterRequest {
            scope_ids: vec![],
            ..request.clone()
        },
    )
    .await;
    assert!(
        whole_population.candidate_nodes > far_ids.len() as u64,
        "the population ({}) must exceed the focus set ({}), or the scope cannot be seen to \
         matter",
        whole_population.candidate_nodes,
        far_ids.len()
    );
    assert!(
        whole_population.node_ids.len() > filtered.node_ids.len(),
        "and filtering the whole population must pass more than filtering the focus set"
    );

    // The visible edge set is NOT derivable from the visible node set: the AwardedTo edges all
    // touched the seed, which is gone, so what survives is the Represents edges alone.
    assert_eq!(filtered.edges.len(), CONTRACTS);
    assert!(filtered.edges.iter().all(|e| e.relation == "Represents"));

    // ── 3. Save, and reopen ─────────────────────────────────────────────────────────────────
    let investigation: SavedInvestigationView = client
        .post(format!("http://{addr}/api/investigations"))
        .json(&serde_json::json!({ "name": "Filtered canvas", "notes": "" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let saved = client
        .post(format!("http://{addr}/api/investigation/subgraph"))
        .query(&[("id", investigation.id.as_str())])
        .json(&SaveSubgraphRequest {
            filter: request.clone(),
            expected_node_ids: filtered.node_ids.clone(),
            expansion_cut: Some(
                urban_integrity_workbench_dto::investigation::ExpansionCutView {
                    seed_id: far.seeds.first().map(|s| s.id.clone()).unwrap_or_default(),
                    hops: far.hops,
                    node_cap: far.node_cap,
                    total_discovered: far.total_discovered,
                    shown: far.nodes.len() as u64,
                    truncated: far.truncation.truncated,
                },
            ),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(
        saved.status(),
        reqwest::StatusCode::OK,
        "the save must land"
    );
    let result: SaveSubgraphResultView = saved.json().await.unwrap();
    assert_eq!(result.entity_count, CONTRACTS as u64 + 2);
    assert_eq!(result.edge_count, CONTRACTS as u64);

    let reopened: InvestigationDetailView = client
        .get(format!("http://{addr}/api/investigation"))
        .query(&[("id", investigation.id.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let pinned_entities: BTreeSet<&str> = reopened
        .pins
        .iter()
        .filter_map(|p| match &p.target {
            PinTargetView::Entity { entity_id } => Some(entity_id.as_str()),
            PinTargetView::Edge { .. } => None,
        })
        .collect();
    assert_eq!(
        pinned_entities,
        set(&filtered.node_ids),
        "SAVED == VISIBLE: the reopened set must be exactly what the filtered canvas was showing"
    );
    assert_ne!(
        pinned_entities,
        set(&far_ids),
        "a save that re-ran the unfiltered query would land here"
    );
    assert_ne!(
        pinned_entities,
        set(&near_ids),
        "and a save that ignored the expansion would land here"
    );
    assert!(
        reopened
            .pins
            .iter()
            .all(|p| p.origin == PinOriginView::Subgraph),
        "every member arrived inside a bulk subgraph save, and the set must be able to say so"
    );

    // The edge pins carry direction, which is what makes the visible edge set recoverable at all.
    let pinned_edges: BTreeSet<(&str, &str, &str)> = reopened
        .pins
        .iter()
        .filter_map(|p| match &p.target {
            PinTargetView::Edge { from, to, relation } => {
                Some((from.as_str(), to.as_str(), relation.as_str()))
            }
            PinTargetView::Entity { .. } => None,
        })
        .collect();
    let visible_edges: BTreeSet<(&str, &str, &str)> = filtered
        .edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str(), e.relation.as_str()))
        .collect();
    assert_eq!(pinned_edges, visible_edges);

    // ── 4. The cut the canvas was already under is recorded ─────────────────────────────────
    assert!(
        reopened.investigation.notes.contains("nothing was cut"),
        "this expansion fitted, and the note must say so: {}",
        reopened.investigation.notes
    );
    assert_eq!(result.provenance_note, reopened.investigation.notes);
}

/// **A save whose ids disagree with the server's own recomputation writes nothing.**
///
/// This is the other half of "saved == visible": the first test proves the browser sends the
/// visible set, and this one proves the server would refuse anything else. A 409 rather than a
/// quiet reconciliation, because an investigation that is *nearly* what was on screen is the
/// failure this requirement is written against.
///
/// PROVE THIS CAN FAIL: delete the `recomputed != expected` comparison from
/// `save_subgraph_handler` and this test's status assertion fails. The mutation was applied and
/// reverted.
#[tokio::test]
async fn db_a_save_that_disagrees_with_the_server_writes_nothing() {
    if !db_tests_enabled() {
        return;
    }
    let (_ns, addr) = seeded_router("canvas_filter_save_mismatch").await;
    let client = reqwest::Client::new();

    let far = expand(&client, addr, "2").await;
    let far_ids = focus_ids(&far);
    let probe = filter(
        &client,
        addr,
        &CanvasFilterRequest {
            node_kinds: vec![],
            edge_types: vec![],
            from_date: None,
            to_date: None,
            undated_edges: MissingEvidence::Keep,
            ward_id: None,
            min_p_c: None,
            unscored_nodes: MissingEvidence::Exclude,
            scope_ids: far_ids.clone(),
        },
    )
    .await;
    let mut request = all_ticked(&probe.facets);
    request.scope_ids = far_ids.clone();
    request.node_kinds = vec!["Contract".to_string()];
    let filtered = filter(&client, addr, &request).await;
    // Non-vacuity: the filter must have removed something, or "disagreeing" would be trivial.
    assert!(filtered.node_ids.len() < far_ids.len());

    let investigation: SavedInvestigationView = client
        .post(format!("http://{addr}/api/investigations"))
        .json(&serde_json::json!({ "name": "Mismatched", "notes": "" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // The browser claims the WHOLE expansion was visible while sending the filter that narrowed it.
    let response = client
        .post(format!("http://{addr}/api/investigation/subgraph"))
        .query(&[("id", investigation.id.as_str())])
        .json(&SaveSubgraphRequest {
            filter: request,
            expected_node_ids: far_ids.clone(),
            expansion_cut: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    let message = body["error"].as_str().unwrap_or_default();
    assert!(message.starts_with("Nothing was saved."), "{message}");
    assert!(message.contains(&far_ids.len().to_string()));
    assert!(message.contains(&filtered.node_ids.len().to_string()));

    // And nothing was written: a refusal that had already pinned half the set would be worse than
    // a save.
    let reopened: InvestigationDetailView = client
        .get(format!("http://{addr}/api/investigation"))
        .query(&[("id", investigation.id.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reopened.pins.len(), 0);
    assert!(reopened.investigation.notes.is_empty());
}

/// The two routes this plan adds are registered, at the path and method the browser uses.
///
/// Ungated: each request is shaped to fail inside its handler, so a handler that ran is
/// distinguishable from a route that is not there (404) or a method that is not registered (405).
#[tokio::test]
async fn the_two_new_routes_reach_their_handlers() {
    let (handle, state) =
        urban_integrity_workbench::graph::fixture::workbench_from_graph(&[], &[], &[]).await;
    let addr = spawn_router(&handle, &state).await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap();

    let empty = CanvasFilterRequest {
        node_kinds: vec![],
        edge_types: vec![],
        from_date: None,
        to_date: None,
        undated_edges: MissingEvidence::Keep,
        ward_id: None,
        min_p_c: None,
        unscored_nodes: MissingEvidence::Exclude,
        scope_ids: vec![],
    };
    let status = client
        .post(format!("http://{addr}/api/graph/canvas-filter"))
        .json(&empty)
        .send()
        .await
        .unwrap()
        .status();
    assert_ne!(status, reqwest::StatusCode::NOT_FOUND);
    assert_ne!(status, reqwest::StatusCode::METHOD_NOT_ALLOWED);

    // No `id` — the handler's own parameter check answers 400, which proves it ran.
    let save_status = client
        .post(format!("http://{addr}/api/investigation/subgraph"))
        .json(&SaveSubgraphRequest {
            filter: empty,
            expected_node_ids: vec![],
            expansion_cut: None,
        })
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(save_status, reqwest::StatusCode::BAD_REQUEST);
}
