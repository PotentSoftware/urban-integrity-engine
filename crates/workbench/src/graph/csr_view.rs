//! The CSR traversal adapter every Graph-page algorithm consumes (plan 44-15 Task 1).
//!
//! [`edge_type_name`] is the single, exhaustive `EdgeWeight` → display-name mapping (no `_`
//! wildcard arm, so a 13th variant fails to compile here rather than silently falling through —
//! T-44-15-02). [`build_scoped_adjacency`] is the ONE place edge-type filtering happens: every
//! algorithm in this phase consumes its [`ScopedAdjacency`] output, never re-filters edge types
//! independently (D-01/D-04, T-44-15-01). [`UndirectedCsrView`] is the `petgraph::visit` adapter
//! over that scoped adjacency, used by `all_simple_paths` (GRA-01) and `astar` (GRA-02).

use std::collections::HashSet;

use petgraph::visit::{
    Data, EdgeRef, GraphBase, GraphRef, IntoEdgeReferences, IntoEdges, IntoNeighbors,
    IntoNeighborsDirected, NodeCount, Visitable,
};
use petgraph::Direction;

use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_schema::types::edge::EdgeWeight;

/// Exhaustive `EdgeWeight` → stable display name. Every one of the 12 `EdgeWeight` variants has
/// an arm; there is deliberately no `_` wildcard, so adding a 13th variant to
/// `crates/schema/src/types/edge.rs` fails this match at compile time instead of silently
/// mapping the new variant to nothing (T-44-15-02).
///
/// Only the first 11 are ever offered in the UI's edge-type checklist (plan 44-01's DTO plan);
/// `DeclaresInterest` still needs a name here so this function stays exhaustive and never panics
/// on real data reaching `build_scoped_adjacency`.
pub fn edge_type_name(w: &EdgeWeight) -> &'static str {
    match w {
        EdgeWeight::Directs { .. } => "Directs",
        EdgeWeight::LocatedAt { .. } => "LocatedAt",
        EdgeWeight::AwardedTo { .. } => "AwardedTo",
        EdgeWeight::IssuedBy { .. } => "IssuedBy",
        EdgeWeight::Represents { .. } => "Represents",
        EdgeWeight::Controls { .. } => "Controls",
        EdgeWeight::TrusteeOf { .. } => "TrusteeOf",
        EdgeWeight::OwnsProperty { .. } => "OwnsProperty",
        EdgeWeight::OffshoreLink { .. } => "OffshoreLink",
        EdgeWeight::DonatesTo { .. } => "DonatesTo",
        EdgeWeight::DisqualifiedFrom { .. } => "DisqualifiedFrom",
        EdgeWeight::DeclaresInterest { .. } => "DeclaresInterest",
    }
}

/// An undirected adjacency list, scoped to a population and an edge-type selection, indexed
/// directly by CSR `u32` node index. Built exactly once per algorithm run by
/// [`build_scoped_adjacency`] — every algorithm consumes this, never `CsrSnapshot` directly, so
/// edge-type filtering happens in exactly one place (D-01/D-04).
#[derive(Debug, Clone, Default)]
pub struct ScopedAdjacency {
    adjacency: Vec<Vec<u32>>,
}

impl ScopedAdjacency {
    /// Every node adjacent to `idx` within the scope/edge-type selection this adjacency was
    /// built with. Empty slice if `idx` is out of range or has no scoped neighbours.
    pub fn neighbors(&self, idx: u32) -> &[u32] {
        self.adjacency
            .get(idx as usize)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// Build a [`ScopedAdjacency`] over `snapshot`, keeping only edges whose type name
/// ([`edge_type_name`]) is in `included_types` AND whose both endpoints are in `scope`. This is
/// the ONE place edge-type filtering happens in this phase's backend (D-01/D-04) — every
/// algorithm (`algorithms/*.rs`) consumes the returned adjacency, never re-filters independently.
///
/// Undirected (D-06): a kept edge is pushed into BOTH endpoints' adjacency lists, deduplicated.
/// `snapshot.graph.edges(idx)` (petgraph `Csr::edges`, outgoing-only since `Csr`'s default `Ty`
/// is `Directed`) is iterated for every node in `scope` — an edge is only ever considered once,
/// from its source's outgoing list, and pushed both ways here.
pub fn build_scoped_adjacency(
    snapshot: &CsrSnapshot,
    scope: &HashSet<u32>,
    included_types: &HashSet<String>,
) -> ScopedAdjacency {
    let node_count = snapshot.graph.node_count();
    let mut adjacency: Vec<Vec<u32>> = vec![Vec::new(); node_count];

    for &node in scope {
        if node as usize >= node_count {
            continue;
        }
        for edge_ref in snapshot.graph.edges(node) {
            let target = edge_ref.target();
            if !scope.contains(&target) {
                continue;
            }
            if !included_types.contains(edge_type_name(edge_ref.weight())) {
                continue;
            }
            if !adjacency[node as usize].contains(&target) {
                adjacency[node as usize].push(target);
            }
            if node != target && !adjacency[target as usize].contains(&node) {
                adjacency[target as usize].push(node);
            }
        }
    }

    ScopedAdjacency { adjacency }
}

/// The eight edge types the degree-cap measurement and the boot-time hub-set resolver filter by
/// (Phase 50, D-14/D-15) — a deliberate, independently-maintained native-side copy of
/// `workbench-web::tool_rail::default_edge_types_included()`'s eight names, mirroring that file's
/// own "cannot import across the wasm boundary" precedent in the opposite direction: this side
/// needs the list natively for a boot-time computation the wasm crate never runs.
pub const DEFAULT_EDGE_TYPES_FOR_DEGREE_CAP: [&str; 8] = [
    "Directs",
    "AwardedTo",
    "Controls",
    "TrusteeOf",
    "OwnsProperty",
    "OffshoreLink",
    "DonatesTo",
    "DisqualifiedFrom",
];

/// The smallest degree `d` such that at least `percentile`% of `scope`'s nodes have degree `<=
/// d`, under `included_types` (D-14/D-15) — nearest-rank method, matching how the phase 50 research notes'
/// measurement was computed. Reuses [`build_scoped_adjacency`], the one place edge-type filtering
/// happens, rather than a second traversal.
///
/// Returns `0` on an empty scope, never panicking or dividing by zero.
pub fn degree_percentile_cutoff(
    snapshot: &CsrSnapshot,
    scope: &HashSet<u32>,
    included_types: &HashSet<String>,
    percentile: f64,
) -> u32 {
    if scope.is_empty() {
        return 0;
    }
    let adjacency = build_scoped_adjacency(snapshot, scope, included_types);
    let mut degrees: Vec<u32> = scope
        .iter()
        .map(|&idx| adjacency.neighbors(idx).len() as u32)
        .collect();
    degrees.sort_unstable();
    let rank = ((percentile / 100.0) * (degrees.len() - 1) as f64).round() as usize;
    degrees[rank.min(degrees.len() - 1)]
}

/// A zero-weight synthetic edge reference. This phase is unweighted (D-03): every edge counts 1,
/// so `astar`'s edge-cost closure only ever needs the endpoints from this, never a real weight.
#[derive(Debug, Clone, Copy)]
pub struct UnitEdgeRef {
    source: u32,
    target: u32,
}

impl EdgeRef for UnitEdgeRef {
    type NodeId = u32;
    type EdgeId = ();
    type Weight = ();

    fn source(&self) -> u32 {
        self.source
    }

    fn target(&self) -> u32 {
        self.target
    }

    fn weight(&self) -> &() {
        &()
    }

    fn id(&self) -> Self::EdgeId {}
}

/// The `petgraph::visit` adapter over a [`ScopedAdjacency`] + `scope`, letting `petgraph`'s
/// built-in traversal algorithms (`all_simple_paths` for GRA-01, `astar` for GRA-02) run directly
/// against the scoped population without a bespoke DFS/BFS implementation
/// (phase 44 research Pattern 1).
///
/// `neighbors_directed` always returns the same undirected neighbour set regardless of the
/// `Direction` argument `all_simple_paths` passes — D-06 treats all traversal as undirected.
#[derive(Debug, Clone, Copy)]
pub struct UndirectedCsrView<'a> {
    adjacency: &'a ScopedAdjacency,
    scope: &'a HashSet<u32>,
}

impl<'a> UndirectedCsrView<'a> {
    pub fn new(adjacency: &'a ScopedAdjacency, scope: &'a HashSet<u32>) -> Self {
        Self { adjacency, scope }
    }
}

impl<'a> GraphBase for UndirectedCsrView<'a> {
    type NodeId = u32;
    type EdgeId = ();
}

// `GraphRef` (`Copy + GraphBase`) has a blanket impl only for `&'a G` in petgraph 0.6.5 — an
// owned `Copy` type implementing `GraphBase` (this view is passed BY VALUE to `all_simple_paths`/
// `astar`, per their `graph: G` signatures, not `&G`) must impl this zero-method marker itself.
impl<'a> GraphRef for UndirectedCsrView<'a> {}

impl<'a> NodeCount for UndirectedCsrView<'a> {
    fn node_count(&self) -> usize {
        self.scope.len()
    }
}

impl<'a> IntoNeighbors for UndirectedCsrView<'a> {
    type Neighbors = std::vec::IntoIter<u32>;

    fn neighbors(self, a: u32) -> Self::Neighbors {
        self.adjacency.neighbors(a).to_vec().into_iter()
    }
}

impl<'a> IntoNeighborsDirected for UndirectedCsrView<'a> {
    type NeighborsDirected = std::vec::IntoIter<u32>;

    fn neighbors_directed(self, n: u32, _d: Direction) -> Self::NeighborsDirected {
        self.neighbors(n)
    }
}

impl<'a> Data for UndirectedCsrView<'a> {
    type NodeWeight = ();
    type EdgeWeight = ();
}

impl<'a> IntoEdgeReferences for UndirectedCsrView<'a> {
    type EdgeRef = UnitEdgeRef;
    type EdgeReferences = std::vec::IntoIter<UnitEdgeRef>;

    fn edge_references(self) -> Self::EdgeReferences {
        let mut edges = Vec::new();
        for &node in self.scope {
            for &target in self.adjacency.neighbors(node) {
                edges.push(UnitEdgeRef {
                    source: node,
                    target,
                });
            }
        }
        edges.into_iter()
    }
}

impl<'a> IntoEdges for UndirectedCsrView<'a> {
    type Edges = std::vec::IntoIter<UnitEdgeRef>;

    fn edges(self, a: u32) -> Self::Edges {
        self.adjacency
            .neighbors(a)
            .iter()
            .map(|&target| UnitEdgeRef { source: a, target })
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl<'a> Visitable for UndirectedCsrView<'a> {
    type Map = HashSet<u32>;

    fn visit_map(&self) -> Self::Map {
        HashSet::new()
    }

    fn reset_map(&self, map: &mut Self::Map) {
        map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::edge::{
        AwardedToData, DeclaresInterestData, DirectsData, DisqualifiedFromData, EdgeMeta,
        IssuedByData, LocatedAtData, OwnsPropertyData, RepresentsData,
    };
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

    /// One fixture instance of every one of the 12 `EdgeWeight` variants, in enum declaration
    /// order — used both to prove `edge_type_name`'s exhaustiveness (every one of the 12 known
    /// names appears) and as `build_scoped_adjacency`'s edge-type fixture.
    fn all_variant_fixtures() -> Vec<EdgeWeight> {
        vec![
            EdgeWeight::Directs {
                meta: meta(),
                data: DirectsData {
                    details: "d".into(),
                    officer_role: None,
                },
            },
            EdgeWeight::LocatedAt {
                meta: meta(),
                data: LocatedAtData {
                    details: "d".into(),
                },
            },
            EdgeWeight::AwardedTo {
                meta: meta(),
                data: AwardedToData {
                    details: "d".into(),
                },
            },
            EdgeWeight::IssuedBy {
                meta: meta(),
                data: IssuedByData {
                    details: "d".into(),
                },
            },
            EdgeWeight::Represents {
                meta: meta(),
                data: RepresentsData {
                    details: "d".into(),
                },
            },
            EdgeWeight::Controls { meta: meta() },
            EdgeWeight::TrusteeOf { meta: meta() },
            EdgeWeight::OwnsProperty {
                meta: meta(),
                data: OwnsPropertyData {
                    price_paid: None,
                    tenure: None,
                },
            },
            EdgeWeight::OffshoreLink { meta: meta() },
            EdgeWeight::DonatesTo { meta: meta() },
            EdgeWeight::DisqualifiedFrom {
                meta: meta(),
                data: DisqualifiedFromData {
                    disqualified_from: None,
                    disqualified_until: None,
                    case_number: None,
                },
            },
            EdgeWeight::DeclaresInterest {
                meta: meta(),
                data: DeclaresInterestData {
                    council: None,
                    category: None,
                },
            },
        ]
    }

    #[test]
    fn edge_type_name_covers_all_12_variants_with_distinct_non_empty_names() {
        let fixtures = all_variant_fixtures();
        assert_eq!(
            fixtures.len(),
            12,
            "fixture must cover every EdgeWeight variant"
        );

        let names: Vec<&'static str> = fixtures.iter().map(edge_type_name).collect();
        for name in &names {
            assert!(
                !name.is_empty(),
                "edge_type_name must never return an empty name"
            );
        }
        let unique: HashSet<&'static str> = names.iter().copied().collect();
        assert_eq!(unique.len(), 12, "all 12 names must be distinct: {names:?}");

        let expected: HashSet<&'static str> = [
            "Directs",
            "LocatedAt",
            "AwardedTo",
            "IssuedBy",
            "Represents",
            "Controls",
            "TrusteeOf",
            "OwnsProperty",
            "OffshoreLink",
            "DonatesTo",
            "DisqualifiedFrom",
            "DeclaresInterest",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            unique, expected,
            "every known name must appear exactly once"
        );
    }

    /// Builds a storage + baked snapshot + parallel `entity_ids` vector (sorted by
    /// `idx.index()`, mirroring `population_scope.rs`'s fixture and `boot()`'s own construction)
    /// from a list of `(entity_id, NodeWeight)` pairs, plus a list of
    /// `(src_entity_id, dst_entity_id, EdgeWeight)` edges.
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
    fn build_scoped_adjacency_keeps_only_included_types_within_scope() {
        let directs = EdgeWeight::Directs {
            meta: meta(),
            data: DirectsData {
                details: "d".into(),
                officer_role: None,
            },
        };
        let issued_by = EdgeWeight::IssuedBy {
            meta: meta(),
            data: IssuedByData {
                details: "d".into(),
            },
        };

        // a -Directs-> b (both in scope, Directs included: KEPT)
        // b -IssuedBy-> c (both in scope, IssuedBy excluded: DROPPED)
        // a -Directs-> outside (outside not in scope: DROPPED)
        let (snapshot, idx) = fixture(
            &[
                ("a", entity("A")),
                ("b", entity("B")),
                ("c", entity("C")),
                ("outside", entity("Outside")),
            ],
            vec![
                ("a", "b", directs.clone()),
                ("b", "c", issued_by),
                ("a", "outside", directs),
            ],
        );

        let scope: HashSet<u32> = [idx["a"], idx["b"], idx["c"]].into_iter().collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();

        let adjacency = build_scoped_adjacency(&snapshot, &scope, &included);

        assert_eq!(
            adjacency.neighbors(idx["a"]),
            &[idx["b"]],
            "a-b kept (Directs, both in scope); a-outside dropped (outside not in scope)"
        );
        assert_eq!(
            adjacency.neighbors(idx["b"]),
            &[idx["a"]],
            "undirected: b sees a back; b-c dropped (IssuedBy not included)"
        );
        assert!(
            adjacency.neighbors(idx["c"]).is_empty(),
            "c has no kept edges (its only edge, b-c, was IssuedBy, not included)"
        );
        assert!(
            adjacency.neighbors(idx["outside"]).is_empty(),
            "outside is not in scope, so it must have no adjacency entries even though an edge referenced it"
        );
    }

    #[test]
    fn degree_percentile_cutoff_returns_zero_on_an_empty_scope() {
        let directs = EdgeWeight::Directs {
            meta: meta(),
            data: DirectsData {
                details: "d".into(),
                officer_role: None,
            },
        };
        let (snapshot, _idx) = fixture(
            &[("a", entity("A")), ("b", entity("B"))],
            vec![("a", "b", directs)],
        );
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let cutoff = degree_percentile_cutoff(&snapshot, &HashSet::new(), &included, 99.0);
        assert_eq!(
            cutoff, 0,
            "an empty scope must never panic or divide by zero"
        );
    }

    #[test]
    fn degree_percentile_cutoff_uses_nearest_rank_over_a_hand_built_star() {
        // hub -Directs-> leaf1..leaf4: undirected adjacency gives hub degree 4, each leaf degree
        // 1 — sorted degree list [1, 1, 1, 1, 4], len 5.
        let directs = |details: &str| EdgeWeight::Directs {
            meta: meta(),
            data: DirectsData {
                details: details.to_string(),
                officer_role: None,
            },
        };
        let (snapshot, idx) = fixture(
            &[
                ("hub", entity("Hub")),
                ("leaf1", entity("Leaf1")),
                ("leaf2", entity("Leaf2")),
                ("leaf3", entity("Leaf3")),
                ("leaf4", entity("Leaf4")),
            ],
            vec![
                ("hub", "leaf1", directs("d1")),
                ("hub", "leaf2", directs("d2")),
                ("hub", "leaf3", directs("d3")),
                ("hub", "leaf4", directs("d4")),
            ],
        );
        let scope: HashSet<u32> = idx.values().copied().collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();

        // rank = round(0.9 * (5 - 1)) = round(3.6) = 4 -> degrees[4] = 4 (the hub itself).
        assert_eq!(
            degree_percentile_cutoff(&snapshot, &scope, &included, 90.0),
            4,
            "p90 over [1,1,1,1,4] must land on the hub's own degree"
        );
        // rank = round(0.5 * 4) = round(2.0) = 2 -> degrees[2] = 1.
        assert_eq!(
            degree_percentile_cutoff(&snapshot, &scope, &included, 50.0),
            1,
            "p50 over [1,1,1,1,4] must land in the leaf run, not the hub"
        );
        // rank = round(1.0 * 4) = 4 -> degrees[4] = 4, the max.
        assert_eq!(
            degree_percentile_cutoff(&snapshot, &scope, &included, 100.0),
            4,
            "p100 must equal the max degree"
        );
    }

    #[test]
    fn undirected_csr_view_finds_a_simple_path_via_all_simple_paths() {
        let directs = EdgeWeight::Directs {
            meta: meta(),
            data: DirectsData {
                details: "d".into(),
                officer_role: None,
            },
        };
        let (snapshot, idx) = fixture(
            &[("a", entity("A")), ("b", entity("B")), ("c", entity("C"))],
            vec![("a", "b", directs.clone()), ("b", "c", directs)],
        );
        let scope: HashSet<u32> = [idx["a"], idx["b"], idx["c"]].into_iter().collect();
        let included: HashSet<String> = ["Directs".to_string()].into_iter().collect();
        let adjacency = build_scoped_adjacency(&snapshot, &scope, &included);
        let view = UndirectedCsrView::new(&adjacency, &scope);

        let paths: Vec<Vec<u32>> =
            petgraph::algo::all_simple_paths(view, idx["a"], idx["c"], 0, Some(4)).collect();

        assert_eq!(paths.len(), 1, "exactly one simple path a -> b -> c");
        assert_eq!(paths[0], vec![idx["a"], idx["b"], idx["c"]]);
    }
}
