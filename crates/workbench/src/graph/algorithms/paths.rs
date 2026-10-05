//! Plan 44-04: all-simple-paths (GRA-01) and shortest/k-shortest paths (GRA-02) over
//! `UndirectedCsrView`.
//!
//! This layer is DTO-free (per `csr_view.rs`'s own precedent, D-01/D-04) — every function here
//! operates on raw CSR `u32` node indices and a local [`StopReason`], never the wire `Truncation`
//! DTO from `crates/workbench-dto` (plan 44-01), which the wiring plan (44-09) builds from this
//! module's return values.
//!
//! `all_simple_paths` reuses `petgraph::algo::all_simple_paths` (GRA-01), never a hand-rolled
//! DFS — petgraph#680 documents this algorithm's exponential-blowup footguns, and this project's
//! own hop/result/wall-clock caps are enforced by wrapping the iterator between `.next()` calls
//! (phase 44 research Pattern 2), never by collecting the full iterator first (T-44-04-01).
//!
//! `shortest_path`/`shortest_and_k_shortest` use `petgraph::algo::astar` with a zero heuristic
//! and unit edge cost (GRA-02) — never petgraph's other two cost-only algorithms, neither
//! of which reconstructs a path (phase 44 research Pitfall 3). k-shortest is derived by sorting
//! `all_simple_paths`'s own bounded enumeration by hop count and taking the first `k`, never a
//! separate Yen's-algorithm implementation.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use petgraph::algo;

use super::super::cancel::AlgoCancel;
use super::super::csr_view::{ScopedAdjacency, UndirectedCsrView};

/// This algorithm layer's own truncation/stop signal — NOT the wire `Truncation` DTO (plan
/// 44-01), which plan 44-09's wiring layer builds from this. Kept local so this module stays
/// DTO-free, mirroring `csr_view.rs`'s precedent of operating on raw `u32` indices only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The traversal completed naturally: either the full result set was found, or (when zero
    /// paths were found) no path exists between `from` and `to` at all, within the population
    /// scope, regardless of hop cap.
    None,
    /// Zero paths were found within `hop_cap`, but a longer path DOES exist beyond it — a cheap
    /// unbounded reachability check (`petgraph::algo::has_path_connecting`, linear, not the
    /// exponential-blowup enumeration this cap exists to guard against) confirmed `to` is
    /// reachable from `from`. Distinguishes "hop cap too small" from "genuinely no path".
    HopCap,
    /// Stopped because `results.len()` reached `result_cap` before the enumeration finished.
    ResultCap,
    /// Stopped because the wall-clock `budget` elapsed before the enumeration finished.
    WallClock,
    /// Stopped because `cancel` was observed cancelled, either before the traversal started or
    /// between `.next()` calls.
    Cancelled,
}

/// All simple (no repeated node) paths from `from` to `to` within `scope`, bounded by `hop_cap`
/// (native `max_intermediate_nodes` parameter to `petgraph::algo::all_simple_paths`), `result_cap`
/// and wall-clock `budget` — enforced BETWEEN iterator `.next()` calls (phase 44 research Pattern 2)
/// so no path beyond any cap is ever fully materialised (T-44-04-01). Every `.next()` also checks
/// `cancel` so a cancelled job stops promptly (GRA-06).
///
/// A `cancel` that is already cancelled before this call causes an immediate return of
/// `(Vec::new(), StopReason::Cancelled)` — the iterator is never even constructed, let alone
/// enumerated.
// This exact 8-parameter shape is mandated by plan 44-04's <action> block (D-07/D-08's four
// caps plus the four traversal endpoints/adjacency handles) — a params struct would obscure the
// call sites this plan and 44-09's wiring layer both read directly against the plan text.
#[allow(clippy::too_many_arguments)]
pub fn all_simple_paths(
    adjacency: &ScopedAdjacency,
    scope: &HashSet<u32>,
    from: u32,
    to: u32,
    hop_cap: u32,
    result_cap: u32,
    budget: Duration,
    cancel: &AlgoCancel,
) -> (Vec<Vec<u32>>, StopReason) {
    if cancel.is_cancelled() {
        return (Vec::new(), StopReason::Cancelled);
    }

    let view = UndirectedCsrView::new(adjacency, scope);
    let deadline = Instant::now() + budget;
    let mut results: Vec<Vec<u32>> = Vec::new();
    let mut stop = StopReason::None;

    let iter = algo::all_simple_paths::<Vec<u32>, _>(view, from, to, 0, Some(hop_cap as usize));
    for path in iter {
        if cancel.is_cancelled() {
            stop = StopReason::Cancelled;
            break;
        }
        if results.len() >= result_cap as usize {
            stop = StopReason::ResultCap;
            break;
        }
        if Instant::now() >= deadline {
            stop = StopReason::WallClock;
            break;
        }
        results.push(path);
    }

    // Zero results with a "natural" stop (no cap/cancellation fired): distinguish "hop cap too
    // small" from "genuinely no path exists" via a cheap unbounded reachability check. Any path
    // `has_path_connecting` finds here must be longer than `hop_cap`, since a path within the cap
    // would already have been found by the enumeration above.
    if stop == StopReason::None
        && results.is_empty()
        && from != to
        && algo::has_path_connecting(view, from, to, None)
    {
        stop = StopReason::HopCap;
    }

    (results, stop)
}

/// The single shortest path from `from` to `to` within `scope`, or `None` if unreachable.
/// `petgraph::algo::astar` with a zero heuristic and unit edge cost (D-03, unweighted) — the only
/// petgraph 0.6.5 built-in that returns an actual path, not just a cost (the phase 44 research notes
/// Pitfall 3: never petgraph's other two cost-only path algorithms, neither of which
/// reconstructs a path).
pub fn shortest_path(
    adjacency: &ScopedAdjacency,
    scope: &HashSet<u32>,
    from: u32,
    to: u32,
) -> Option<Vec<u32>> {
    let view = UndirectedCsrView::new(adjacency, scope);
    let result: Option<(u32, Vec<u32>)> =
        algo::astar(view, from, |n| n == to, |_edge| 1u32, |_n| 0u32);
    result.map(|(_cost, path)| path)
}

/// Up to `k` shortest paths from `from` to `to`, derived by sorting [`all_simple_paths`]'s own
/// bounded enumeration by hop count and taking the first `k` — never a separate Yen's-algorithm
/// implementation (GRA-02, the phase 44 research notes' "Don't Hand-Roll" table). Reuses the SAME
/// `hop_cap`/`result_cap`/`budget` the all-simple-paths tool uses internally, per the UI-SPEC's
/// parameter table.
///
/// If `k` exceeds the number of distinct paths found, all found paths are returned — this is
/// "fewer than k paths exist", a normal result (the underlying [`StopReason`] is passed through
/// unchanged; it is not overridden into an invented "k not met" truncation).
// Same plan 44-04-mandated parameter shape as `all_simple_paths` above, plus `k`.
#[allow(clippy::too_many_arguments)]
pub fn shortest_and_k_shortest(
    adjacency: &ScopedAdjacency,
    scope: &HashSet<u32>,
    from: u32,
    to: u32,
    k: u32,
    hop_cap: u32,
    result_cap: u32,
    budget: Duration,
    cancel: &AlgoCancel,
) -> (Vec<Vec<u32>>, StopReason) {
    let (mut paths, stop) = all_simple_paths(
        adjacency, scope, from, to, hop_cap, result_cap, budget, cancel,
    );
    paths.sort_by_key(|p| p.len());
    paths.truncate(k as usize);
    (paths, stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::graph::snapshot::{bake_csr, CsrSnapshot};
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    use crate::graph::csr_view::build_scoped_adjacency;

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

    /// Builds a storage + baked snapshot + `HashMap<name, csr_idx>` from a list of node names and
    /// `(src, dst)` `Directs` edges, mirroring `csr_view.rs`'s own fixture helper.
    fn fixture(
        node_names: &[&str],
        edges: &[(&str, &str)],
    ) -> (CsrSnapshot, std::collections::HashMap<String, u32>) {
        let storage = GraphStorage::new();
        for name in node_names {
            storage.insert_node(EntityId(name.to_string()), entity(name));
        }
        for (src, dst) in edges {
            storage
                .insert_edge(
                    &EntityId(src.to_string()),
                    &EntityId(dst.to_string()),
                    directs(),
                )
                .expect("fixture edge insert");
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

    fn linear_fixture() -> (
        ScopedAdjacency,
        HashSet<u32>,
        std::collections::HashMap<String, u32>,
    ) {
        // A - B - C - D - E, 4 hops apart, exactly one simple path.
        let (snapshot, idx) = fixture(
            &["a", "b", "c", "d", "e"],
            &[("a", "b"), ("b", "c"), ("c", "d"), ("d", "e")],
        );
        let scope: HashSet<u32> = idx.values().copied().collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &scope, &included);
        (adjacency, scope, idx)
    }

    #[test]
    fn all_simple_paths_returns_the_one_four_hop_path_untruncated() {
        let (adjacency, scope, idx) = linear_fixture();
        let (paths, stop) = all_simple_paths(
            &adjacency,
            &scope,
            idx["a"],
            idx["e"],
            4,
            100,
            Duration::from_secs(5),
            &AlgoCancel::new(),
        );

        assert_eq!(paths.len(), 1, "exactly one simple path exists");
        assert_eq!(
            paths[0],
            vec![idx["a"], idx["b"], idx["c"], idx["d"], idx["e"]]
        );
        assert_eq!(stop, StopReason::None, "not truncated by any cap");
    }

    #[test]
    fn all_simple_paths_with_too_small_a_hop_cap_reports_hop_cap_not_no_path() {
        let (adjacency, scope, idx) = linear_fixture();
        // The only path is 4 hops; a hop cap of 2 cannot reach it.
        let (paths, stop) = all_simple_paths(
            &adjacency,
            &scope,
            idx["a"],
            idx["e"],
            2,
            100,
            Duration::from_secs(5),
            &AlgoCancel::new(),
        );

        assert!(paths.is_empty(), "hop cap of 2 cannot reach a 4-hop path");
        assert_eq!(
            stop,
            StopReason::HopCap,
            "zero paths because of the hop cap, not because no path exists at all"
        );
    }

    #[test]
    fn all_simple_paths_reports_none_when_genuinely_unreachable() {
        let (snapshot, idx) = fixture(&["a", "isolated"], &[]);
        let scope: HashSet<u32> = idx.values().copied().collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &scope, &included);

        let (paths, stop) = all_simple_paths(
            &adjacency,
            &scope,
            idx["a"],
            idx["isolated"],
            4,
            100,
            Duration::from_secs(5),
            &AlgoCancel::new(),
        );

        assert!(paths.is_empty());
        assert_eq!(
            stop,
            StopReason::None,
            "genuinely no path must never be reported as HopCap"
        );
    }

    #[test]
    fn all_simple_paths_diamond_with_result_cap_1_returns_exactly_one_path_truncated() {
        // a -> b -> d and a -> c -> d: two simple paths from a to d.
        let (snapshot, idx) = fixture(
            &["a", "b", "c", "d"],
            &[("a", "b"), ("b", "d"), ("a", "c"), ("c", "d")],
        );
        let scope: HashSet<u32> = idx.values().copied().collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &scope, &included);

        let (paths, stop) = all_simple_paths(
            &adjacency,
            &scope,
            idx["a"],
            idx["d"],
            4,
            1,
            Duration::from_secs(5),
            &AlgoCancel::new(),
        );

        assert_eq!(
            paths.len(),
            1,
            "result cap of 1 truncates the diamond's two paths to one"
        );
        assert_eq!(stop, StopReason::ResultCap);
    }

    #[test]
    fn shortest_and_k_shortest_returns_the_one_path_and_requested_k_exceeds_it() {
        let (adjacency, scope, idx) = linear_fixture();
        let (paths, stop) = shortest_and_k_shortest(
            &adjacency,
            &scope,
            idx["a"],
            idx["e"],
            3,
            4,
            100,
            Duration::from_secs(5),
            &AlgoCancel::new(),
        );

        let requested_k = 3;
        let returned = paths.len();
        assert_eq!(returned, 1, "only one path exists");
        assert_ne!(
            requested_k, returned,
            "requested_k must be distinct from the actual returned count"
        );
        assert_eq!(
            stop,
            StopReason::None,
            "fewer than k paths is a normal result"
        );
    }

    #[test]
    fn shortest_path_finds_the_linear_fixtures_only_path() {
        let (adjacency, scope, idx) = linear_fixture();
        let path = shortest_path(&adjacency, &scope, idx["a"], idx["e"]).expect("path exists");
        assert_eq!(path, vec![idx["a"], idx["b"], idx["c"], idx["d"], idx["e"]]);
    }

    #[test]
    fn shortest_path_returns_none_when_unreachable() {
        let (snapshot, idx) = fixture(&["a", "isolated"], &[]);
        let scope: HashSet<u32> = idx.values().copied().collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &scope, &included);

        assert!(shortest_path(&adjacency, &scope, idx["a"], idx["isolated"]).is_none());
    }

    #[test]
    fn all_simple_paths_cancelled_before_start_returns_immediately_never_enumerates() {
        let (adjacency, scope, idx) = linear_fixture();
        let cancel = AlgoCancel::new();
        cancel.cancel();

        let (paths, stop) = all_simple_paths(
            &adjacency,
            &scope,
            idx["a"],
            idx["e"],
            4,
            100,
            Duration::from_secs(5),
            &cancel,
        );

        assert!(
            paths.is_empty(),
            "a pre-cancelled call must never enumerate any path"
        );
        assert_eq!(stop, StopReason::Cancelled);
    }
}
