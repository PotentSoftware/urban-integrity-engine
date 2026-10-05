//! Plan 44-05: hand-rolled degeneracy peeling, GRA-04.
//!
//! Confirmed absent from both `petgraph` 0.6.5 and `graphrs` 0.12.0's public API
//! (the phase 44 research notes' "Explicitly NOT a don't-hand-roll violation" table: `grep`'d both source
//! trees for `k_core`/`kcore`/`k-core`, zero hits in either) — a legitimate, scoped hand-roll of
//! the standard, well-documented degeneracy-peeling algorithm.

use std::collections::{HashSet, VecDeque};

use super::super::csr_view::ScopedAdjacency;

/// Returns the maximal subgraph of `scope` in which every surviving node has at least `k`
/// neighbours WITHIN that same surviving subgraph (`(surviving_node_ids, edge_count_within_core)`).
///
/// Standard degeneracy peeling: repeatedly remove any node whose CURRENT degree (within the
/// shrinking working set, not its original degree in `scope`) is below `k`, updating neighbours'
/// current degrees as nodes are removed, until no more removals are possible. `k = 0`/`k = 1`
/// (every node has at least 0/1 neighbours, i.e. every connected node) never removes anything;
/// `k` higher than any node's degree in `scope` empties the core entirely rather than panicking.
pub fn k_core(adjacency: &ScopedAdjacency, scope: &HashSet<u32>, k: u32) -> (HashSet<u32>, u64) {
    let mut current_degree: std::collections::HashMap<u32, u32> = scope
        .iter()
        .map(|&node| {
            let degree = adjacency
                .neighbors(node)
                .iter()
                .filter(|n| scope.contains(n))
                .count() as u32;
            (node, degree)
        })
        .collect();

    let mut removed: HashSet<u32> = HashSet::new();
    let mut queue: VecDeque<u32> = current_degree
        .iter()
        .filter(|(_, &degree)| degree < k)
        .map(|(&node, _)| node)
        .collect();
    let mut queued: HashSet<u32> = queue.iter().copied().collect();

    while let Some(node) = queue.pop_front() {
        queued.remove(&node);
        if removed.contains(&node) {
            continue;
        }
        // Re-check: an earlier removal in this same pass may have already dropped this node's
        // degree further, or (impossible for degree to rise) confirm it's still below k.
        if *current_degree.get(&node).unwrap_or(&0) >= k {
            continue;
        }
        removed.insert(node);

        for &neighbor in adjacency.neighbors(node) {
            if !scope.contains(&neighbor) || removed.contains(&neighbor) {
                continue;
            }
            if let Some(degree) = current_degree.get_mut(&neighbor) {
                if *degree > 0 {
                    *degree -= 1;
                }
                if *degree < k && !queued.contains(&neighbor) {
                    queue.push_back(neighbor);
                    queued.insert(neighbor);
                }
            }
        }
    }

    let surviving: HashSet<u32> = scope.difference(&removed).copied().collect();

    let mut edge_count: u64 = 0;
    for &node in &surviving {
        for &neighbor in adjacency.neighbors(node) {
            if surviving.contains(&neighbor) {
                edge_count += 1;
            }
        }
    }
    // Every surviving undirected edge was counted once from each endpoint.
    edge_count /= 2;

    (surviving, edge_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::csr_view::build_scoped_adjacency;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    fn meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

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
            meta: meta(),
            data: DirectsData {
                details: "d".into(),
                officer_role: None,
            },
        }
    }

    /// A 4-node fully-connected clique (`c0..c3`, each with degree 3 within the clique) plus 3
    /// pendant nodes (`p0..p2`), each attached by a single edge to `c0`.
    fn clique_plus_pendants_fixture() -> (
        urban_integrity_core::graph::snapshot::CsrSnapshot,
        std::collections::HashMap<String, u32>,
    ) {
        let storage = GraphStorage::new();
        for i in 0..4 {
            storage.insert_node(EntityId(format!("c{i}")), entity(&format!("c{i}")));
        }
        for i in 0..3 {
            storage.insert_node(EntityId(format!("p{i}")), entity(&format!("p{i}")));
        }

        for i in 0..4u32 {
            for j in (i + 1)..4u32 {
                storage
                    .insert_edge(
                        &EntityId(format!("c{i}")),
                        &EntityId(format!("c{j}")),
                        directs(),
                    )
                    .expect("clique edge insert");
            }
        }
        for i in 0..3 {
            storage
                .insert_edge(
                    &EntityId("c0".into()),
                    &EntityId(format!("p{i}")),
                    directs(),
                )
                .expect("pendant edge insert");
        }

        let mut entries = storage.snapshot_nodes();
        entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
        let index_of: std::collections::HashMap<String, u32> = entries
            .iter()
            .enumerate()
            .map(|(i, (id, _, _, _))| (id.0.clone(), i as u32))
            .collect();
        let entity_ids: Vec<EntityId> = entries.into_iter().map(|(id, _, _, _)| id).collect();

        let snapshot =
            urban_integrity_core::graph::snapshot::bake_csr(&storage, entity_ids.len() as u64);
        (snapshot, index_of)
    }

    fn scope_and_adjacency(
        snapshot: &urban_integrity_core::graph::snapshot::CsrSnapshot,
        idx: &std::collections::HashMap<String, u32>,
    ) -> (HashSet<u32>, ScopedAdjacency) {
        let scope: HashSet<u32> = idx.values().copied().collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(snapshot, &scope, &included);
        (scope, adjacency)
    }

    #[test]
    fn k_core_3_returns_exactly_the_clique_never_the_pendants() {
        let (snapshot, idx) = clique_plus_pendants_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);

        let (surviving, edge_count) = k_core(&adjacency, &scope, 3);

        let expected: HashSet<u32> = (0..4).map(|i| idx[&format!("c{i}")]).collect();
        assert_eq!(
            surviving, expected,
            "3-core must be exactly the 4-node clique, never any pendant"
        );
        assert_eq!(
            edge_count, 6,
            "the clique's 4 nodes have 6 internal edges (C(4,2))"
        );
    }

    #[test]
    fn k_core_1_returns_every_connected_node() {
        let (snapshot, idx) = clique_plus_pendants_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);

        let (surviving, _) = k_core(&adjacency, &scope, 1);

        assert_eq!(
            surviving, scope,
            "1-core must include every node that has at least one edge (all 7 fixture nodes)"
        );
    }

    #[test]
    fn k_core_99_returns_an_empty_core_not_a_panic() {
        let (snapshot, idx) = clique_plus_pendants_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);

        let (surviving, edge_count) = k_core(&adjacency, &scope, 99);

        assert!(
            surviving.is_empty(),
            "k=99 (higher than any node's degree) must empty the core, not panic"
        );
        assert_eq!(edge_count, 0);
    }

    #[test]
    fn k_core_on_empty_scope_returns_empty_core() {
        let (snapshot, _idx) = clique_plus_pendants_fixture();
        let empty_scope: HashSet<u32> = HashSet::new();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &empty_scope, &included);

        let (surviving, edge_count) = k_core(&adjacency, &empty_scope, 3);

        assert!(surviving.is_empty());
        assert_eq!(edge_count, 0);
    }
}
