//! Plan 44-04: ward-reachable-subgraph rail tool, GRA-06 — always walks `Represents` regardless
//! of the edge-type selection, D-01.
//!
//! This is a DISTINCT code path from `population_scope.rs`'s boot-time walk
//! (`population_scope::build`): that module runs once at boot, walks every edge type
//! unconditionally from every configured ward seed to determine the canonical population at all.
//! This tool runs on demand, from a single analyst-picked ward, `Represents`-only, WITHIN the
//! already-loaded population (`scope`) — never outside it.
//!
//! It also does NOT reuse the caller's [`super::super::csr_view::ScopedAdjacency`]: that
//! adjacency reflects the analyst's checkable edge-type selection (D-01/D-04), and this
//! algorithm's edge set is fixed and independent of that selection — it always walks
//! `Represents`, regardless of what the edge-type checklist would otherwise include or exclude.
//! A fresh, Represents-only adjacency is built on the fly for each call instead.

use std::collections::HashSet;

use petgraph::visit::EdgeRef;

use urban_integrity_core::graph::snapshot::CsrSnapshot;

use super::super::cancel::AlgoCancel;
use super::super::csr_view::edge_type_name;
use super::paths::StopReason;

/// Every node adjacent to `idx` via a `Represents` edge, in either direction, restricted to
/// `scope` — mirrors [`CsrSnapshot::neighbors_undirected`]'s outgoing-plus-incoming shape, but
/// filtered to `Represents` only rather than every edge type.
fn represents_neighbors(snapshot: &CsrSnapshot, scope: &HashSet<u32>, idx: u32) -> Vec<u32> {
    let mut out = Vec::new();

    // Outgoing: idx -> target.
    for edge_ref in snapshot.graph.edges(idx) {
        let target = edge_ref.target();
        if scope.contains(&target)
            && edge_type_name(edge_ref.weight()) == "Represents"
            && !out.contains(&target)
        {
            out.push(target);
        }
    }

    // Incoming: src -> idx. `reverse_adj` gives node ids only (no edge type), so each candidate
    // source's own outgoing edges are checked for one landing on `idx` with the Represents type.
    for &src in snapshot.neighbors_in(idx) {
        if !scope.contains(&src) || out.contains(&src) {
            continue;
        }
        let is_represents_to_idx = snapshot.graph.edges(src).any(|edge_ref| {
            edge_ref.target() == idx && edge_type_name(edge_ref.weight()) == "Represents"
        });
        if is_represents_to_idx {
            out.push(src);
        }
    }

    out
}

/// The set of node indices reachable from `ward_idx` by walking only `Represents` edges, up to
/// `hops` levels, restricted to `scope` (the canonical population — this tool operates WITHIN it,
/// never outside it) and capped at `result_cap` visited nodes. Checks `cancel` between BFS levels
/// so a cancelled job stops promptly (GRA-06).
///
/// `ward_idx` not in `scope` returns an empty set with [`StopReason::None`] — there is nothing to
/// walk from outside the population.
pub fn ward_reachable_subgraph(
    snapshot: &CsrSnapshot,
    scope: &HashSet<u32>,
    ward_idx: u32,
    hops: u32,
    result_cap: u32,
    cancel: &AlgoCancel,
) -> (HashSet<u32>, StopReason) {
    if cancel.is_cancelled() {
        return (HashSet::new(), StopReason::Cancelled);
    }
    if !scope.contains(&ward_idx) {
        return (HashSet::new(), StopReason::None);
    }

    let mut visited: HashSet<u32> = HashSet::new();
    visited.insert(ward_idx);
    let mut frontier: Vec<u32> = vec![ward_idx];
    let mut stop = StopReason::None;

    'levels: for _level in 0..hops {
        if cancel.is_cancelled() {
            stop = StopReason::Cancelled;
            break;
        }
        let mut next_frontier = Vec::new();
        for &node in &frontier {
            for neighbor in represents_neighbors(snapshot, scope, node) {
                if visited.contains(&neighbor) {
                    continue;
                }
                if visited.len() >= result_cap as usize {
                    stop = StopReason::ResultCap;
                    break 'levels;
                }
                visited.insert(neighbor);
                next_frontier.push(neighbor);
            }
        }
        if next_frontier.is_empty() {
            break;
        }
        frontier = next_frontier;
    }

    (visited, stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::edge::{
        AwardedToData, EdgeMeta, EdgeWeight, RepresentsData,
    };
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight, WardData};

    fn meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

    fn represents() -> EdgeWeight {
        EdgeWeight::Represents {
            meta: meta(),
            data: RepresentsData {
                details: "d".into(),
            },
        }
    }

    fn awarded_to() -> EdgeWeight {
        EdgeWeight::AwardedTo {
            meta: meta(),
            data: AwardedToData {
                details: "d".into(),
            },
        }
    }

    fn ward(name: &str) -> NodeWeight {
        NodeWeight::Ward(WardData {
            name: name.to_string(),
        })
    }

    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }

    /// Builds a storage + baked snapshot + `HashMap<name, csr_idx>` from a list of `(name,
    /// NodeWeight)` pairs and `(src, dst, EdgeWeight)` edges.
    fn fixture(
        nodes: &[(&str, NodeWeight)],
        edges: Vec<(&str, &str, EdgeWeight)>,
    ) -> (CsrSnapshot, std::collections::HashMap<String, u32>) {
        let storage = GraphStorage::new();
        for (id, weight) in nodes {
            storage.insert_node(EntityId(id.to_string()), weight.clone());
        }
        for (src, dst, weight) in edges {
            storage
                .insert_edge(
                    &EntityId(src.to_string()),
                    &EntityId(dst.to_string()),
                    weight,
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

    #[test]
    fn hops_1_follows_represents_only_never_awarded_to() {
        // ward -Represents-> contract (direct hop), ward -AwardedTo-> other (unrelated edge type,
        // must never be followed even though AwardedTo would be in-scope for the caller's own
        // edge-type checklist elsewhere).
        let (snapshot, idx) = fixture(
            &[
                ("ward:w", ward("W")),
                ("contract:c", entity("Contract C")),
                ("other:o", entity("Other O")),
            ],
            vec![
                ("ward:w", "contract:c", represents()),
                ("ward:w", "other:o", awarded_to()),
            ],
        );
        let scope: HashSet<u32> = idx.values().copied().collect();

        let (reachable, stop) =
            ward_reachable_subgraph(&snapshot, &scope, idx["ward:w"], 1, 100, &AlgoCancel::new());

        assert_eq!(
            reachable,
            [idx["ward:w"], idx["contract:c"]].into_iter().collect(),
            "only the ward and the directly-Represents-connected contract, never the AwardedTo target"
        );
        assert_eq!(stop, StopReason::None);
    }

    #[test]
    fn hops_2_reaches_the_wider_represents_only_set() {
        // ward -Represents-> contract -Represents-> deeper, 2 hops.
        let (snapshot, idx) = fixture(
            &[
                ("ward:w", ward("W")),
                ("contract:c", entity("Contract C")),
                ("deep:d", entity("Deep D")),
            ],
            vec![
                ("ward:w", "contract:c", represents()),
                ("contract:c", "deep:d", represents()),
            ],
        );
        let scope: HashSet<u32> = idx.values().copied().collect();

        let (reachable_1hop, _) =
            ward_reachable_subgraph(&snapshot, &scope, idx["ward:w"], 1, 100, &AlgoCancel::new());
        assert!(
            !reachable_1hop.contains(&idx["deep:d"]),
            "1 hop must not yet reach the 2-hop-away node"
        );

        let (reachable_2hop, stop) =
            ward_reachable_subgraph(&snapshot, &scope, idx["ward:w"], 2, 100, &AlgoCancel::new());
        assert_eq!(
            reachable_2hop,
            [idx["ward:w"], idx["contract:c"], idx["deep:d"]]
                .into_iter()
                .collect(),
            "2 hops reaches the wider set"
        );
        assert_eq!(stop, StopReason::None);
    }

    #[test]
    fn a_result_cap_smaller_than_the_true_reachable_set_truncates_and_reports_it() {
        let (snapshot, idx) = fixture(
            &[
                ("ward:w", ward("W")),
                ("a:a", entity("A")),
                ("b:b", entity("B")),
                ("c:c", entity("C")),
            ],
            vec![
                ("ward:w", "a:a", represents()),
                ("ward:w", "b:b", represents()),
                ("ward:w", "c:c", represents()),
            ],
        );
        let scope: HashSet<u32> = idx.values().copied().collect();

        let (reachable, stop) =
            ward_reachable_subgraph(&snapshot, &scope, idx["ward:w"], 1, 2, &AlgoCancel::new());

        assert_eq!(
            reachable.len(),
            2,
            "result cap of 2 truncates the true reachable set of 4 (ward + 3 neighbours)"
        );
        assert_eq!(stop, StopReason::ResultCap);
    }

    #[test]
    fn cancelled_before_start_returns_immediately() {
        let (snapshot, idx) = fixture(
            &[("ward:w", ward("W")), ("contract:c", entity("Contract C"))],
            vec![("ward:w", "contract:c", represents())],
        );
        let scope: HashSet<u32> = idx.values().copied().collect();
        let cancel = AlgoCancel::new();
        cancel.cancel();

        let (reachable, stop) =
            ward_reachable_subgraph(&snapshot, &scope, idx["ward:w"], 1, 100, &cancel);

        assert!(reachable.is_empty());
        assert_eq!(stop, StopReason::Cancelled);
    }
}
