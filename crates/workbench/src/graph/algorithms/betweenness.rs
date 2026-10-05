//! Plan 44-05: hand-rolled pivot-sampled Brandes betweenness centrality, GRA-04, D-09.
//!
//! `graphrs::betweenness_centrality` is confirmed **exact** (all `0..node_count()` sources, no
//! pivot/sample parameter — the phase 44 research notes' "graphrs 0.12.0" section, verified by reading
//! `graphrs-0.12.0/src/algorithms/centrality/betweenness.rs` directly), and its single-source
//! BFS/accumulate helpers are `fn`, not `pub fn` — not callable from outside the crate. No other
//! maintained Rust crate ships a pivot-sampled/approximate betweenness primitive as a public API
//! (the phase 44 research notes' "Don't Hand-Roll" table, "Explicitly NOT a don't-hand-roll violation"
//! row). This file is therefore a legitimate, scoped hand-roll: the textbook unweighted Brandes
//! algorithm (single-source BFS + reverse-order dependency accumulation), looped over a
//! seed-selected pivot subset instead of every node — `graphrs`'s own `betweenness.rs` (MIT) is a
//! safe SHAPE reference read while writing this, never called into.
//!
//! ## No `rand` crate dependency
//!
//! See `community.rs`'s equivalent doc-comment section for the full reasoning: this wave's
//! parallel-execution boundary forbids editing `Cargo.toml`, and `rand` is not a direct
//! dependency of `crates/workbench` (only reachable transitively through `graphrs`, not `use`-able
//! without an explicit `Cargo.toml` entry). [`SeededRng`] here is the same small,
//! dependency-free SplitMix64 generator `community.rs` defines (duplicated rather than shared,
//! since this wave's file-ownership boundary also forbids adding a new shared module to
//! `algorithms/mod.rs`) — used only to select a deterministic, seed-keyed pivot subset, not for
//! any cryptographic purpose.

use std::collections::{HashMap, HashSet, VecDeque};

use super::super::cancel::AlgoCancel;
use super::super::csr_view::ScopedAdjacency;

/// D-09's operator-facing pivot-count default. The ONE place this number is defined in Rust —
/// plan 44-09's dispatch call site imports and passes this constant, never a bare literal `500`
/// and never a later result-row truncation's `top_n`, which is a separate, later concern.
/// The phase 44 UI spec's static rail Help text quotes the same figure by hand (rail copy cannot
/// import a Rust const); a future change to one should update the other.
pub const DEFAULT_BETWEENNESS_PIVOT_COUNT: u32 =
    urban_integrity_workbench_dto::caps::BETWEENNESS_PIVOTS;

/// A small, dependency-free SplitMix64 deterministic PRNG — see this module's top doc comment
/// ("No `rand` crate dependency") for why this exists instead of `rand::rngs::StdRng`.
struct SeededRng(u64);

impl SeededRng {
    fn new(seed: u64) -> Self {
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

/// Selects `min(pivot_count, scope.len())` DISTINCT nodes from `scope`, deterministic given
/// `seed` (sort to a canonical starting order first, so the shuffle itself is the only source of
/// variation — a `HashSet`'s own iteration order is not stable across runs).
fn select_pivots(scope: &HashSet<u32>, seed: u64, pivot_count: u32) -> Vec<u32> {
    let mut nodes: Vec<u32> = scope.iter().copied().collect();
    nodes.sort_unstable();
    let mut rng = SeededRng::new(seed);
    rng.shuffle(&mut nodes);
    let take = (pivot_count as usize).min(nodes.len());
    nodes.truncate(take);
    nodes
}

/// Unweighted single-source Brandes BFS + dependency accumulation from `source`, adding every
/// node's contribution directly into `scores` (caller sums across all sampled pivots by reusing
/// the same `scores` map across calls).
fn accumulate_single_source(
    adjacency: &ScopedAdjacency,
    scope: &HashSet<u32>,
    source: u32,
    scores: &mut HashMap<u32, f64>,
) {
    let mut dist: HashMap<u32, u32> = HashMap::new();
    let mut sigma: HashMap<u32, f64> = HashMap::new();
    let mut preds: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut visit_order: Vec<u32> = Vec::new();

    dist.insert(source, 0);
    sigma.insert(source, 1.0);
    let mut queue: VecDeque<u32> = VecDeque::new();
    queue.push_back(source);

    while let Some(v) = queue.pop_front() {
        visit_order.push(v);
        let dv = dist[&v];
        for &w in adjacency.neighbors(v) {
            if !scope.contains(&w) {
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = dist.entry(w) {
                entry.insert(dv + 1);
                queue.push_back(w);
            }
            if dist[&w] == dv + 1 {
                let sigma_v = sigma[&v];
                *sigma.entry(w).or_insert(0.0) += sigma_v;
                preds.entry(w).or_default().push(v);
            }
        }
    }

    // Reverse-BFS-order dependency accumulation (standard Brandes): by the time `v` is
    // processed, every node strictly farther from `source` than `v` (i.e. every successor of
    // `v` on some shortest path) has already had its own delta fully accumulated.
    let mut delta: HashMap<u32, f64> = HashMap::new();
    for &v in visit_order.iter().rev() {
        let delta_v = *delta.get(&v).unwrap_or(&0.0);
        if let Some(ps) = preds.get(&v) {
            let sigma_v = sigma[&v];
            for &p in ps {
                let contribution = (sigma[&p] / sigma_v) * (1.0 + delta_v);
                *delta.entry(p).or_insert(0.0) += contribution;
            }
        }
        if v != source {
            *scores.entry(v).or_insert(0.0) += delta_v;
        }
    }
}

/// Pivot-sampled betweenness centrality (GRA-04, D-09) over a population-scoped,
/// edge-type-filtered adjacency, unweighted and undirected (D-03/D-06). Samples
/// `min(pivot_count, scope.len())` DISTINCT pivots deterministically from `seed`, runs a
/// single-source Brandes BFS from each, and sums the per-node contributions across all sampled
/// pivots. Checks `cancel.is_cancelled()` between pivots so a long run over the wall-clock budget
/// is interruptible. Returns every node the sampled runs touched, unsorted and untruncated — the
/// caller sorts descending and truncates to top-N.
pub fn sampled_betweenness(
    adjacency: &ScopedAdjacency,
    scope: &HashSet<u32>,
    seed: u64,
    pivot_count: u32,
    cancel: &AlgoCancel,
) -> Vec<(u32, f64)> {
    if scope.is_empty() {
        return Vec::new();
    }

    let pivots = select_pivots(scope, seed, pivot_count);
    let mut scores: HashMap<u32, f64> = scope.iter().map(|&n| (n, 0.0)).collect();

    for pivot in pivots {
        if cancel.is_cancelled() {
            break;
        }
        accumulate_single_source(adjacency, scope, pivot, &mut scores);
    }

    scores.into_iter().collect()
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

    /// One center node connected to 5 leaves (star topology). Every leaf-to-leaf shortest path
    /// passes through the center, so the center's betweenness score must strictly exceed every
    /// leaf's (leaves have none: no shortest path between two OTHER nodes ever routes through a
    /// leaf).
    fn star_fixture() -> (
        urban_integrity_core::graph::snapshot::CsrSnapshot,
        std::collections::HashMap<String, u32>,
    ) {
        let storage = GraphStorage::new();
        storage.insert_node(EntityId("center".into()), entity("center"));
        for i in 0..5 {
            storage.insert_node(EntityId(format!("leaf{i}")), entity(&format!("leaf{i}")));
        }
        for i in 0..5 {
            storage
                .insert_edge(
                    &EntityId("center".into()),
                    &EntityId(format!("leaf{i}")),
                    directs(),
                )
                .expect("star edge insert");
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
    fn center_of_a_star_scores_strictly_higher_than_every_leaf_with_all_nodes_sampled() {
        let (snapshot, idx) = star_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);
        let cancel = AlgoCancel::new();

        let scored = sampled_betweenness(
            &adjacency,
            &scope,
            42,
            DEFAULT_BETWEENNESS_PIVOT_COUNT,
            &cancel,
        );

        assert_eq!(
            scored.len(),
            6,
            "pivot_count (500) >= node_count (6) must degrade gracefully to sampling every node"
        );

        let scores: std::collections::HashMap<u32, f64> = scored.into_iter().collect();
        let center_score = scores[&idx["center"]];
        for i in 0..5 {
            let leaf_score = scores[&idx[&format!("leaf{i}")]];
            assert!(
                center_score > leaf_score,
                "center ({center_score}) must strictly exceed leaf{i} ({leaf_score})"
            );
        }
    }

    #[test]
    fn same_seed_and_pivot_count_produce_identical_scores() {
        let (snapshot, idx) = star_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);
        let cancel = AlgoCancel::new();

        let mut run1 = sampled_betweenness(&adjacency, &scope, 7, 3, &cancel);
        let mut run2 = sampled_betweenness(&adjacency, &scope, 7, 3, &cancel);
        run1.sort_by_key(|(id, _)| *id);
        run2.sort_by_key(|(id, _)| *id);

        assert_eq!(
            run1, run2,
            "two runs with the identical seed and pivot_count must produce identical scores \
             (the sampled pivot set itself is seed-reproducible)"
        );
    }

    #[test]
    fn select_pivots_never_duplicates_and_clamps_to_scope_len() {
        let scope: HashSet<u32> = (0..5).collect();

        let pivots = select_pivots(&scope, 42, 10);

        assert_eq!(
            pivots.len(),
            5,
            "pivot_count (10) clamped to scope.len() (5), never sampling more than scope has"
        );
        let unique: HashSet<u32> = pivots.iter().copied().collect();
        assert_eq!(
            unique.len(),
            5,
            "pivot_count clamped to at most scope.len() must never sample the same node twice: {pivots:?}"
        );
    }

    #[test]
    fn sampled_betweenness_on_empty_scope_returns_no_scores() {
        let (snapshot, _idx) = star_fixture();
        let empty_scope: HashSet<u32> = HashSet::new();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &empty_scope, &included);
        let cancel = AlgoCancel::new();

        let scored = sampled_betweenness(&adjacency, &empty_scope, 42, 500, &cancel);

        assert!(scored.is_empty());
    }

    #[test]
    fn cancelling_before_the_first_pivot_returns_a_zeroed_but_complete_score_map() {
        let (snapshot, idx) = star_fixture();
        let (scope, adjacency) = scope_and_adjacency(&snapshot, &idx);
        let cancel = AlgoCancel::new();
        cancel.cancel();

        let scored = sampled_betweenness(&adjacency, &scope, 42, 500, &cancel);

        assert_eq!(
            scored.len(),
            6,
            "every scope node must still be present (at score 0.0), even with zero pivots run"
        );
        assert!(scored.iter().all(|(_, score)| *score == 0.0));
    }
}
