//! Golden tests (spec §3.4): every algorithm reproduces the planted manifest through
//! `dispatch::run_algorithm` (the real HTTP entry point's core), never by calling the algorithm
//! modules directly. No DB.

use std::collections::BTreeSet;
use std::sync::Arc;

use urban_integrity_core::graph::coordinator::GraphCoordinatorHandle;
use urban_integrity_test_support::planted::{
    planted_graph, ExpectedCommunitySet, PlantedGraph, DEFAULT_EDGES, WARD_CODE,
};
use urban_integrity_workbench::boot::WorkbenchState;
use urban_integrity_workbench::graph::dispatch::{run_algorithm, KNOWN_EDGE_TYPES};
use urban_integrity_workbench::graph::fixture::workbench_from_graph;
use urban_integrity_workbench_dto::{
    AlgorithmOutcome, AlgorithmRequest, AlgorithmResult, NodeSummary,
};

async fn fixture() -> (
    Arc<GraphCoordinatorHandle>,
    Arc<WorkbenchState>,
    PlantedGraph,
) {
    let g = planted_graph();
    let edges: Vec<_> = g
        .edges
        .iter()
        .map(|e| (e.src.clone(), e.dst.clone(), e.weight.clone()))
        .collect();
    let (handle, state) = workbench_from_graph(&g.nodes, &edges, &[WARD_CODE.to_string()]).await;
    (handle, state, g)
}

fn request(algorithm: &str, edge_types: &[&str]) -> AlgorithmRequest {
    AlgorithmRequest {
        algorithm: algorithm.to_string(),
        session_id: "golden".to_string(),
        edge_types_included: edge_types.iter().map(|s| s.to_string()).collect(),
        seed: 42,
        from_id: None,
        to_id: None,
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

async fn run(
    handle: &Arc<GraphCoordinatorHandle>,
    state: &Arc<WorkbenchState>,
    req: AlgorithmRequest,
) -> AlgorithmResult {
    run_algorithm(Arc::clone(handle), Arc::clone(state), req)
        .await
        .expect("dispatch must succeed on the planted graph")
}

fn path_ids(result: &AlgorithmResult) -> Vec<Vec<String>> {
    match &result.outcome {
        AlgorithmOutcome::Paths { paths } | AlgorithmOutcome::ShortestPaths { paths, .. } => paths
            .iter()
            .map(|p| p.hops.iter().map(|h| h.entity.id.clone()).collect())
            .collect(),
        other => panic!("expected a path outcome, got {other:?}"),
    }
}

/// Spawns `boot::build_router` on an ephemeral port and returns its address. Shared by every
/// test that must reach the real HTTP surface rather than `dispatch` directly.
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

#[test]
fn default_edges_are_every_known_type_except_the_three_hub_formers() {
    let hub = ["Represents", "IssuedBy", "LocatedAt"];
    assert!(
        DEFAULT_EDGES.iter().all(|e| !hub.contains(e)),
        "a hub-forming type leaked into DEFAULT_EDGES"
    );
    let mut all: Vec<&str> = DEFAULT_EDGES.iter().copied().chain(hub).collect();
    all.sort_unstable();
    let mut known = KNOWN_EDGE_TYPES.to_vec();
    known.sort_unstable();
    assert_eq!(all, known);
}

#[tokio::test]
async fn population_scope_equals_manifest_reachable_count() {
    let (_h, state, g) = fixture().await;
    assert_eq!(
        state.population_scope.node_ids.len(),
        g.manifest.reachable_nodes
    );
    assert_eq!(state.population_scope.ward_seed_count, 1);
}

#[tokio::test]
async fn shortest_paths_match_manifest() {
    let (h, state, g) = fixture().await;
    for expected in &g.manifest.shortest_paths {
        let mut req = request("shortest_paths", expected.edge_types_included);
        req.from_id = Some(expected.from.to_string());
        req.to_id = Some(expected.to.to_string());
        req.k = 1;
        let result = run(&h, &state, req).await;
        let paths = path_ids(&result);
        assert_eq!(paths.len(), 1, "{} -> {}", expected.from, expected.to);
        assert_eq!(paths[0], expected.node_ids);
        assert!(!result.truncation.truncated);
    }
}

#[tokio::test]
async fn no_path_cases_return_zero_paths() {
    let (h, state, g) = fixture().await;
    for np in &g.manifest.no_path {
        let mut req = request("shortest_paths", np.edge_types_included);
        req.from_id = Some(np.from.to_string());
        req.to_id = Some(np.to.to_string());
        let result = run(&h, &state, req).await;
        assert!(
            path_ids(&result).is_empty(),
            "{} -> {} must have no path",
            np.from,
            np.to
        );
    }
}

#[tokio::test]
async fn all_simple_paths_counts_match_manifest() {
    let (h, state, g) = fixture().await;
    for expected in &g.manifest.all_simple_paths {
        let mut req = request("all_simple_paths", expected.edge_types_included);
        req.from_id = Some(expected.from.to_string());
        req.to_id = Some(expected.to.to_string());
        req.hop_cap = expected.hop_cap;
        let result = run(&h, &state, req).await;
        assert_eq!(path_ids(&result).len(), expected.count);
        assert!(!result.truncation.truncated);
    }
}

fn community_sets(result: &AlgorithmResult) -> (Vec<BTreeSet<String>>, usize) {
    match &result.outcome {
        AlgorithmOutcome::Community {
            communities,
            hidden_node_ids,
        } => (
            communities
                .iter()
                .map(|c| c.member_node_ids.iter().cloned().collect())
                .collect(),
            hidden_node_ids.len(),
        ),
        other => panic!("expected community, got {other:?}"),
    }
}

fn assert_partition(expected: &ExpectedCommunitySet, actual: &[BTreeSet<String>]) {
    let flexible: BTreeSet<String> = expected.flexible.iter().map(|s| s.to_string()).collect();
    let actual_stripped: Vec<BTreeSet<String>> = actual
        .iter()
        .map(|s| s.difference(&flexible).cloned().collect())
        .collect();
    for want in &expected.sets {
        let want: BTreeSet<String> = want
            .iter()
            .map(|s| s.to_string())
            .filter(|s| !flexible.contains(s))
            .collect();
        assert!(
            actual_stripped.contains(&want),
            "expected community {want:?} not found in {actual_stripped:?}"
        );
    }
    assert_eq!(actual.len(), expected.sets.len(), "community count");
}

#[tokio::test]
async fn louvain_partitions_match_manifest() {
    let (h, state, g) = fixture().await;
    for expected in &g.manifest.communities {
        let mut req = request("community", expected.edge_types_included);
        req.seed = expected.seed;
        req.min_community_size = expected.min_size;
        let result = run(&h, &state, req).await;
        let (sets, hidden) = community_sets(&result);
        assert_partition(expected, &sets);
        assert_eq!(hidden, expected.hidden_count, "hidden count");
        assert_eq!(result.seed_used, Some(expected.seed));
        let (h2, state2, _) = fixture().await;
        let mut again = request("community", expected.edge_types_included);
        again.seed = expected.seed;
        again.min_community_size = expected.min_size;
        let result_again = run(&h2, &state2, again).await;
        assert_eq!(community_sets(&result_again).0, sets);
    }
}

#[tokio::test]
async fn k_core_membership_matches_manifest() {
    let (h, state, g) = fixture().await;
    let population: BTreeSet<String> = state
        .population_scope
        .node_ids
        .iter()
        .filter_map(|&i| state.label_index.entity_ref_at(i).map(|e| e.id))
        .collect();
    for expected in &g.manifest.k_cores {
        let mut req = request("k_core", expected.edge_types_included);
        req.k = expected.k;
        let result = run(&h, &state, req).await;
        let AlgorithmOutcome::KCore {
            hidden_node_ids,
            node_count,
            ..
        } = &result.outcome
        else {
            panic!("expected k_core");
        };
        let hidden: BTreeSet<String> = hidden_node_ids.iter().cloned().collect();
        let members: BTreeSet<String> = population.difference(&hidden).cloned().collect();
        let want: BTreeSet<String> = expected.members.iter().map(|s| s.to_string()).collect();
        assert_eq!(members, want, "k={}", expected.k);
        assert_eq!(*node_count as usize, want.len());
    }
}

#[tokio::test]
async fn betweenness_is_exact_and_ranks_the_bridge_first() {
    let (h, state, g) = fixture().await;
    for expected in &g.manifest.betweenness {
        let result = run(
            &h,
            &state,
            request("betweenness", expected.edge_types_included),
        )
        .await;
        let AlgorithmOutcome::Betweenness {
            sampled_sources,
            total_sources,
            top_nodes,
        } = &result.outcome
        else {
            panic!("expected betweenness");
        };
        assert_eq!(sampled_sources, total_sources, "small graph must be exact");
        assert!(!result.truncation.truncated);
        assert_eq!(top_nodes[0].entity.id, expected.top_1);
        assert!(top_nodes.len() >= 2, "need at least two ranked nodes");
        assert!(top_nodes[0].score > top_nodes[1].score);
        assert!(
            (top_nodes[0].score - expected.top_1_score).abs() < 1e-9,
            "got {}",
            top_nodes[0].score
        );
    }
}

#[tokio::test]
async fn ward_subgraph_matches_manifest() {
    let (h, state, g) = fixture().await;
    for expected in &g.manifest.ward_subgraphs {
        let mut req = request("ward_subgraph", &[]);
        req.ward_id = Some(expected.ward_id.to_string());
        let result = run(&h, &state, req).await;
        let AlgorithmOutcome::WardSubgraph { node_ids, .. } = &result.outcome else {
            panic!("expected ward_subgraph");
        };
        let got: BTreeSet<String> = node_ids.iter().cloned().collect();
        let want: BTreeSet<String> = expected.members.iter().map(|s| s.to_string()).collect();
        assert_eq!(got, want);
    }
}

/// The side card's manifest sample, asserted through the real `GET /api/graph/node` handler —
/// the endpoint the operator's click actually reaches — not by re-deriving the degree from the
/// snapshot the way the handler does.
#[tokio::test]
async fn side_card_sample_matches_node_summary_endpoint() {
    let (h, state, g) = fixture().await;
    let addr = spawn_router(&h, &state).await;
    let sample = &g.manifest.side_card_sample;

    let summary: NodeSummary = reqwest::Client::new()
        .get(format!("http://{addr}/api/graph/node"))
        .query(&[("id", sample.id)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(summary.degree, sample.degree as u64);
    let earliest = summary
        .earliest_edge
        .as_deref()
        .expect("the sample node has edges, so earliest_edge must be present");
    assert_eq!(&earliest[..10], sample.earliest_from);
    match (summary.latest_edge.as_deref(), sample.latest_to) {
        (None, None) => {}
        (Some(got), Some(want)) => assert_eq!(&got[..10], want),
        (got, want) => panic!("latest_edge {got:?} does not match manifest {want:?}"),
    }
}

#[tokio::test]
async fn router_round_trip_shortest_path_equals_dispatch() {
    let (h, state, g) = fixture().await;
    let addr = spawn_router(&h, &state).await;

    let expected = &g.manifest.shortest_paths[0];
    let mut req = request("shortest_paths", expected.edge_types_included);
    req.from_id = Some(expected.from.to_string());
    req.to_id = Some(expected.to.to_string());
    req.k = 1;

    let client = reqwest::Client::new();
    let wire: AlgorithmResult = client
        .post(format!("http://{addr}/api/graph/algorithm"))
        .json(&req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(path_ids(&wire)[0], expected.node_ids);

    let meta: serde_json::Value = client
        .get(format!("http://{addr}/api/graph/population/meta"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        meta["count"].as_u64().unwrap() as usize,
        g.manifest.reachable_nodes
    );
}
