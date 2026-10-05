//! Plan 44-05: seeded Louvain via `graphrs`, GRA-03.
//!
//! `graphrs` 0.12.0's `leiden()` has no `seed` parameter anywhere in its public signature and
//! reseeds its internal RNG from OS entropy on every call (`utility::get_rng(None)` — verified by
//! reading `graphrs-0.12.0/src/algorithms/community/leiden/mod.rs` directly, per
//! the phase 44 research notes' "CRITICAL/BLOCKING finding" section). Only `louvain_communities`/
//! `louvain_partitions` accept and thread through `seed: Option<u64>`. GRA-03's requirement text
//! permits "Louvain or Leiden", and the phase 44 design notes' 2026-09-07 amendment (D-01/D-10 amended)
//! locks in Louvain as this phase's choice — this file therefore calls
//! `graphrs::algorithms::community::louvain::louvain_communities`, never `leiden`.
//!
//! ## A second, empirically-discovered determinism gap (beyond what the phase 44 research notes' excerpt shows)
//!
//! Reading the ACTUAL installed `graphrs-0.12.0` source (not just the excerpt quoted in
//! the phase 44 research notes) shows `louvain.rs` is a "rewritten from scratch" implementation whose own
//! top-of-file comment says: "Conditional determinism: fast HashMap iteration when seed=None,
//! sorted when seed=Some(_)". Grepping the whole file for `rand::`/`StdRng` returns zero hits —
//! unlike `leiden()`, this Louvain implementation contains **no RNG at all**. `seed: Option<u64>`
//! is stored on `Modularity` and threaded to `find_best_community`, but is used ONLY as an
//! `is_some()` toggle between "iterate this node's candidate-community `HashMap` in whatever
//! order `std`'s SipHash gives it this process" (fast, but non-deterministic run-to-run even for
//! an unchanged graph) and "iterate the same `HashMap`'s keys sorted ascending by
//! `CommunityId`" (deterministic). **The NUMERIC VALUE of the seed is never read anywhere** —
//! `louvain_communities(&g, false, None, None, Some(42))` and `...Some(7))` on the identical
//! `Graph` object are byte-identical, because both merely select the sorted branch.
//!
//! This satisfies "same seed -> same result" but not GRA-03's other, equally load-bearing half:
//! "different seed -> a genuinely different partition is possible" (the non-vacuity check
//! phase 44 research Pitfall 1 warns is easy to skip). `Modularity::run_louvain`'s local-moving
//! loop visits nodes in strict sequential `node_index` order (`0..num_communities`, wrapping —
//! confirmed by reading `run_louvain` directly), and `node_index` is assigned by INSERTION order
//! into the `graphrs::Graph`, never randomized internally. So this file introduces the seed's
//! actual effect upstream of `graphrs`, by shuffling the order nodes and edges are inserted into
//! the `Graph` with a seed-keyed deterministic shuffle before ever calling `louvain_communities` —
//! `louvain_communities` remains the ONE thing that performs the community-detection
//! optimisation; this file only controls the order its input is presented in, which is enough to
//! change which local optimum the identical greedy algorithm converges to (a well-documented
//! property of Louvain's node-order sensitivity in the wider literature, not a defect specific to
//! this crate).
//!
//! ## No `rand` crate dependency
//!
//! This plan's own parallel-execution boundary restricts this wave's executors to their assigned
//! files only (`community.rs`/`betweenness.rs`/`k_core.rs`) and explicitly forbids touching
//! `Cargo.toml`. `rand` is not a direct dependency of `crates/workbench` (only reachable
//! transitively through `graphrs`, and Rust does not expose transitive dependencies to `use`
//! without an explicit `Cargo.toml` entry), so `rand::rngs::StdRng` — the type
//! the phase 44 research notes' Supporting table names — is not actually reachable from this file without a
//! `Cargo.toml` edit this wave's boundary disallows. [`SeededRng`] below is a small,
//! self-contained SplitMix64 generator (a standard, widely-used deterministic PRNG; see
//! <https://prng.di.unimi.it/splitmix64.c>) implemented directly against `std`, used only to
//! produce a seed-keyed deterministic shuffle order — not for any cryptographic or
//! security-sensitive purpose. If `rand` is later added to `Cargo.toml` by a future plan, this
//! can be swapped for `rand::rngs::StdRng` for consistency with `betweenness.rs`'s equivalent
//! substitution (see that file's own doc comment for the same reasoning).

use std::collections::HashSet;

use graphrs::algorithms::community::louvain::louvain_communities;
use graphrs::{Graph, GraphSpecs, Node};

use super::super::csr_view::ScopedAdjacency;

/// A small, dependency-free SplitMix64 deterministic PRNG — see this module's top doc comment
/// ("No `rand` crate dependency") for why this exists instead of `rand::rngs::StdRng`.
struct SeededRng(u64);

impl SeededRng {
    fn new(seed: u64) -> Self {
        // XOR with a fixed odd constant so an all-zero seed doesn't produce a degenerate
        // all-zero first state (SplitMix64's usual seeding convention).
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// In-place Fisher-Yates shuffle, deterministic given this generator's seed.
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next_u64() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

/// Builds a `graphrs::Graph<u32, ()>` over every node in `scope`, adding every undirected edge in
/// `adjacency` exactly once (`neighbor > node` dedup — `ScopedAdjacency` already stores both
/// directions of every kept edge, so this avoids adding each undirected pair twice, which would
/// trip `graphrs`'s duplicate-edge error under its default `EdgeDedupeStrategy::Error`). Node and
/// edge insertion order is shuffled by `seed` (see this module's top doc comment) — this is what
/// makes the eventual `louvain_communities` result actually depend on the seed's numeric value.
fn build_graphrs_graph(
    adjacency: &ScopedAdjacency,
    scope: &HashSet<u32>,
    seed: u64,
) -> Graph<u32, ()> {
    let mut rng = SeededRng::new(seed);

    let mut sorted_scope: Vec<u32> = scope.iter().copied().collect();
    sorted_scope.sort_unstable();

    let mut nodes = sorted_scope.clone();
    rng.shuffle(&mut nodes);

    let mut graph: Graph<u32, ()> = Graph::new(GraphSpecs::undirected());
    graph.add_nodes(nodes.iter().map(|&n| Node::from_name(n)).collect());

    let mut edges: Vec<(u32, u32)> = Vec::new();
    for &node in &sorted_scope {
        for &neighbor in adjacency.neighbors(node) {
            if neighbor > node {
                edges.push((node, neighbor));
            }
        }
    }
    rng.shuffle(&mut edges);

    if !edges.is_empty() {
        graph
            .add_edge_tuples(edges)
            .expect("ScopedAdjacency only references nodes already added to the graph above");
    }

    graph
}

/// Seeded Louvain community detection (GRA-03) over a population-scoped, edge-type-filtered
/// adjacency. `weighted: false` per D-03 (this phase is unweighted).
///
/// Returns `(communities_meeting_min_size, hidden_node_ids)`: every surviving community's node
/// list is sorted ascending (graphrs's own `HashSet<T>` community members have no guaranteed
/// stable iteration order even for an identical partition — sorting here is what makes the
/// determinism test below assert actual partition membership, not incidental hash-iteration
/// order), and the surviving communities themselves are sorted by their first (smallest) member
/// so the overall return value is fully order-independent. Any community whose size is below
/// `min_size` is excluded from the returned community list, but every one of its node ids is
/// still reported in `hidden_node_ids` — never silently dropped with no trace.
pub fn seeded_louvain(
    adjacency: &ScopedAdjacency,
    scope: &HashSet<u32>,
    seed: u64,
    min_size: u32,
    resolution: f64,
) -> (Vec<Vec<u32>>, Vec<u32>) {
    if scope.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let graph = build_graphrs_graph(adjacency, scope, seed);

    // `Some(seed)` is mandatory here, but for a different reason than making the RESULT vary by
    // the seed's numeric value (that comes from `build_graphrs_graph`'s insertion-order shuffle
    // above): `find_best_community` iterates a `HashMap` directly (std's randomized SipHash) when
    // `seed` is `None`, which is non-deterministic ACROSS RUNS even for the identical graph
    // object. `Some(_)` switches it to sorted-by-`CommunityId` iteration, which is what makes
    // "same seed -> same result" hold at all.
    let communities = louvain_communities(&graph, false, Some(resolution), None, Some(seed))
        .expect("louvain_communities on a non-empty graph always yields at least one partition");

    let mut kept: Vec<Vec<u32>> = Vec::new();
    let mut hidden: Vec<u32> = Vec::new();

    for community in communities {
        let mut members: Vec<u32> = community.into_iter().collect();
        members.sort_unstable();
        if members.len() as u32 >= min_size {
            kept.push(members);
        } else {
            hidden.extend(members);
        }
    }

    kept.sort();
    hidden.sort_unstable();

    (kept, hidden)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::csr_view::build_scoped_adjacency;
    use urban_integrity_core::graph::snapshot::bake_csr;
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

    /// Two 4-node cliques (A, B — dense internal connectivity) plus one bridge node `x` whose
    /// FOUR edges split exactly evenly (two into A, two into B) — per phase 44 research Pitfall 1's
    /// warning, a small fixture can converge to the same partition "by luck" regardless of seed;
    /// this fixture is deliberately designed so `x`'s community assignment is a genuine tie
    /// (equal pull from both cliques), which is exactly the kind of ambiguity the insertion-order
    /// shuffle can resolve differently depending on whether A's or B's nodes happen to be
    /// processed (and therefore already merged into one community) before `x` is.
    fn two_cliques_plus_bridge_node_fixture() -> (
        urban_integrity_core::graph::snapshot::CsrSnapshot,
        std::collections::HashMap<String, u32>,
    ) {
        let storage = GraphStorage::new();
        for cluster in ["a", "b"] {
            for i in 0..4 {
                let id = format!("{cluster}{i}");
                storage.insert_node(EntityId(id.clone()), entity(&id));
            }
        }
        storage.insert_node(EntityId("x".into()), entity("x"));

        // Fully connect each 4-node cluster (6 edges per clique).
        for cluster in ["a", "b"] {
            for i in 0..4 {
                for j in (i + 1)..4 {
                    storage
                        .insert_edge(
                            &EntityId(format!("{cluster}{i}")),
                            &EntityId(format!("{cluster}{j}")),
                            directs(),
                        )
                        .expect("clique edge insert");
                }
            }
        }

        // `x` splits its 4 edges exactly evenly between the two cliques.
        for id in ["a0", "a1", "b0", "b1"] {
            storage
                .insert_edge(&EntityId("x".into()), &EntityId(id.into()), directs())
                .expect("bridge-node edge insert");
        }

        let mut entries = storage.snapshot_nodes();
        entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
        let index_of: std::collections::HashMap<String, u32> = entries
            .iter()
            .enumerate()
            .map(|(i, (id, _, _, _))| (id.0.clone(), i as u32))
            .collect();
        let entity_ids: Vec<EntityId> = entries.into_iter().map(|(id, _, _, _)| id).collect();

        let snapshot = bake_csr(&storage, entity_ids.len() as u64);
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
    fn seeded_louvain_same_seed_produces_byte_identical_partitions() {
        let (snapshot, idx) = two_cliques_plus_bridge_node_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);

        let run1 = seeded_louvain(&adjacency, &scope, 42, 1, 1.0);
        let run2 = seeded_louvain(&adjacency, &scope, 42, 1, 1.0);

        assert_eq!(
            run1, run2,
            "two runs with the identical seed must produce byte-identical (communities, hidden) output"
        );
    }

    #[test]
    fn seeded_louvain_different_seeds_produce_a_differently_grouped_node() {
        let (snapshot, idx) = two_cliques_plus_bridge_node_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);

        let (communities_42, _) = seeded_louvain(&adjacency, &scope, 42, 1, 1.0);
        let (communities_7, _) = seeded_louvain(&adjacency, &scope, 7, 1, 1.0);

        assert_ne!(
            communities_42, communities_7,
            "the fixture must not be accidentally seed-insensitive: seeds 42 and 7 must produce \
             at least one differently-grouped node (phase 44 research Pitfall 1's non-vacuity check)"
        );
    }

    #[test]
    fn seeded_louvain_excludes_below_min_size_communities_but_reports_them_as_hidden() {
        let (snapshot, idx) = two_cliques_plus_bridge_node_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);

        // min_size above the standalone clique's size (4) but at or below the clique that
        // absorbs `x` (5), so SOME nodes are always excluded regardless of which clique the
        // seed's shuffle happens to have `x` join.
        let (kept, hidden) = seeded_louvain(&adjacency, &scope, 42, 5, 1.0);

        let kept_total: usize = kept.iter().map(|c| c.len()).sum();
        assert_eq!(
            kept_total + hidden.len(),
            9,
            "every one of the 9 fixture nodes must be accounted for in either a kept community or hidden — never silently dropped"
        );
        for community in &kept {
            assert!(
                community.len() as u32 >= 5,
                "every kept community must meet min_size: {community:?}"
            );
        }
        if !hidden.is_empty() {
            let mut hidden_sorted = hidden.clone();
            hidden_sorted.sort_unstable();
            assert_eq!(hidden, hidden_sorted, "hidden node ids must be sorted");
        }
    }

    #[test]
    fn seeded_louvain_on_empty_scope_returns_no_communities_and_no_hidden_nodes() {
        let (snapshot, _idx) = two_cliques_plus_bridge_node_fixture();
        let empty_scope: HashSet<u32> = HashSet::new();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &empty_scope, &included);

        let (kept, hidden) = seeded_louvain(&adjacency, &empty_scope, 42, 1, 1.0);

        assert!(kept.is_empty());
        assert!(hidden.is_empty());
    }
}
