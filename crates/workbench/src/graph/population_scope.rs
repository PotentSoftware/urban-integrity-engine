//! The canonical, ward-reachable population — re-implemented natively against
//! `crates/workbench`'s own already-baked [`CsrSnapshot`], never by depending on
//! `crates/cli` (the phase 44 research notes' "Architecture: the canonical-population cross-crate gap").
//!
//! This is what every algorithm (GRA-01..04), the Arrow IPC export (CNV-04), and the label
//! index (NAV-06) must be scoped to — the SAME population `export-graphml
//! --reachable-from-wards` computes, proven equal by a DB-gated test
//! (`crates/workbench/tests/population_equals_export.rs`, GRA-06 success criterion 5).
//!
//! Do NOT confuse this population-scoping walk with the Graph page's Tool Rail
//! "ward-reachable subgraph" *algorithm* (phase 44 design decision D-01): this module runs once at boot,
//! walks every edge type unconditionally from every configured ward seed, and determines what
//! is in the canonical graph at all; the rail's algorithm walks only `Represents` edges from a
//! single analyst-picked ward, within the already-loaded population, on demand.

use std::collections::HashSet;

use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::NodeWeight;

/// The ward-reachable node-index subset of a [`CsrSnapshot`], plus how many ward seeds the walk
/// started from. Built once at boot ([`build`]) from ward-seed codes the CLI caller already
/// resolved (its `_catalog` row's `ward_codes`) — this module never re-reads `config.toml` or
/// `_catalog` independently (plan 44-02's Pitfall 4 guard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopulationScope {
    /// CSR `u32` node indices reachable (undirected) from the ward seeds.
    pub node_ids: HashSet<u32>,
    /// The number of ward-seed codes that actually resolved to a `Ward` node in the graph.
    pub ward_seed_count: usize,
}

/// Resolve `codes` (bare ward/LPA codes, e.g. `"X05001499"`) to the CSR indices of every
/// matching `Ward` node in `snapshot` — exact equality on `format!("ward:{code}")`, never a
/// substring match, mirroring `crates/cli/src/commands/export_graphml.rs::find_ward_seeds`'s own
/// convention (the shape re-targeted here, never called — `crates/workbench` cannot depend on
/// `crates/cli`).
///
/// `entity_ids` is a `Vec<EntityId>` indexed by CSR `u32` index — the caller ([`boot`]) builds it
/// ONCE from `GraphStorage::snapshot_nodes()` sorted by `idx.index()`, which `bake_csr` proves is
/// numerically identical to the CSR index (every `petgraph::graph::NodeIndex` is assigned by a
/// monotonically-increasing counter with no reuse, and `bake_csr` sorts by that index before
/// adding nodes to the `Csr` in that exact order). Never re-derived per call.
///
/// A ward code with no matching node contributes nothing to the returned set — never a panic,
/// never an error. Zero resolvable codes correctly returns an empty set.
pub fn find_ward_seeds(
    snapshot: &CsrSnapshot,
    entity_ids: &[EntityId],
    codes: &[String],
) -> HashSet<u32> {
    let wanted: HashSet<String> = codes.iter().map(|c| format!("ward:{c}")).collect();
    let node_count = snapshot.graph.node_count();
    let mut seeds = HashSet::new();
    for idx in 0..node_count {
        let idx_u32 = idx as u32;
        let Some(entity_id) = entity_ids.get(idx) else {
            // entity_ids is shorter than the CSR — a caller-side bug (should never happen once
            // built from the same snapshot_nodes() the CSR was baked from), but this function
            // never panics: an unmatched index simply is not a seed.
            continue;
        };
        let is_ward = matches!(snapshot.graph[idx_u32], NodeWeight::Ward(_));
        if is_ward && wanted.contains(&entity_id.0) {
            seeds.insert(idx_u32);
        }
    }
    seeds
}

/// Undirected BFS from `seeds` via [`CsrSnapshot::neighbors_undirected`] — mirrors
/// `export_graphml.rs::filter_reachable`'s own queue/visited shape exactly, including a node
/// reachable only via an inbound edge from a seed being included (deliberately undirected, not
/// a bug — see `filter_reachable`'s own doc comment).
///
/// With zero seeds, returns an empty [`PopulationScope`] — never silently falling back to "the
/// whole graph". This is a legitimate "no ward codes resolved for this analysis" state, not an
/// error at this layer (mirrors `PopulationSummary::Unavailable`'s existing honesty rule).
pub fn build(snapshot: &CsrSnapshot, seeds: &HashSet<u32>) -> PopulationScope {
    let mut visited: HashSet<u32> = seeds.clone();
    let mut queue: std::collections::VecDeque<u32> = seeds.iter().copied().collect();
    while let Some(idx) = queue.pop_front() {
        for next in snapshot.neighbors_undirected(idx) {
            if visited.insert(next) {
                queue.push_back(next);
            }
        }
    }
    PopulationScope {
        node_ids: visited,
        ward_seed_count: seeds.len(),
    }
}

/// The deferred-resolution cell for [`PopulationScope`] (checkpoint fix, plan 44-14 Task 3).
///
/// `boot` can only walk the ward-reachable population once the coordinator's first CSR bake has
/// landed, and on a large corpus that bake takes minutes while `force_initial_bake` waits two
/// seconds. Building the scope eagerly at boot therefore froze an EMPTY scope for the whole
/// process lifetime (observed 2026-09-08: `/api/graph/population/meta` reported 113,384 nodes
/// while `/api/graph/population` streamed 0). This cell carries the analysis's ward codes and
/// resolves the scope exactly once, on the first request that arrives after the bake — see
/// `boot::resolve_population_scope`.
#[derive(Debug, Default)]
pub struct LateScope {
    /// The analysis's own ward codes (`_catalog.ward_codes`), retained so the deferred build
    /// can resolve seeds without re-reading config.
    pub ward_codes: Vec<String>,
    /// Set at most once, by the first request that finds the bake landed.
    pub cell: std::sync::OnceLock<PopulationScope>,
}

impl LateScope {
    /// A cell that will resolve against `ward_codes` once the bake lands.
    pub fn new(ward_codes: Vec<String>) -> Self {
        Self {
            ward_codes,
            cell: std::sync::OnceLock::new(),
        }
    }

    /// A cell with no ward codes — for fixtures whose `population_scope` is already final.
    pub fn none() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_core::graph::storage::GraphStorage;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, WardData};

    /// Builds a storage + baked snapshot + parallel `entity_ids` vector (sorted by
    /// `idx.index()`, exactly as `boot()` builds it) from a list of `(entity_id, NodeWeight)`
    /// pairs, plus a list of `(src_entity_id, dst_entity_id)` directed edges.
    fn fixture(
        nodes: &[(&str, NodeWeight)],
        edges: &[(&str, &str)],
    ) -> (CsrSnapshot, Vec<EntityId>) {
        let storage = GraphStorage::new();
        for (id, weight) in nodes {
            storage.insert_node(EntityId(id.to_string()), weight.clone());
        }
        for (src, dst) in edges {
            let weight = EdgeWeight::Directs {
                meta: EdgeMeta {
                    created_at: chrono::Utc::now(),
                    effective_from: None,
                    effective_to: None,
                },
                data: DirectsData {
                    details: "test".to_string(),
                    officer_role: None,
                },
            };
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
        let entity_ids: Vec<EntityId> = entries.into_iter().map(|(id, _, _, _)| id).collect();

        let snapshot = bake_csr(&storage, entity_ids.len() as u64);
        (snapshot, entity_ids)
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

    #[test]
    fn find_ward_seeds_matches_the_one_ward_node() {
        let (snapshot, entity_ids) = fixture(
            &[
                ("ward:X05001499", ward("Sample Ward")),
                ("GB-COH:other", entity("Other Co")),
            ],
            &[],
        );

        let seeds = find_ward_seeds(&snapshot, &entity_ids, &["X05001499".to_string()]);

        assert_eq!(seeds.len(), 1, "exactly the one matching Ward node");
        let seed_idx = *seeds.iter().next().unwrap();
        assert_eq!(
            entity_ids[seed_idx as usize].0, "ward:X05001499",
            "the returned index must be the Ward node's own CSR index"
        );
    }

    #[test]
    fn find_ward_seeds_with_no_matching_code_returns_empty_never_panics() {
        let (snapshot, entity_ids) = fixture(&[("ward:X05001499", ward("Sample Ward"))], &[]);

        let seeds = find_ward_seeds(&snapshot, &entity_ids, &["W09999999".to_string()]);

        assert!(
            seeds.is_empty(),
            "a ward code with no matching node must return an empty seed set"
        );
    }

    #[test]
    fn build_includes_a_node_reachable_only_via_an_inbound_edge_from_a_seed() {
        // seed -> mid is a forward edge FROM the seed; leaf -> mid is an edge pointing AT mid
        // from a third node, but mid itself is only reached via the seed's own outgoing edge
        // here — so instead make the seed reachable ONLY via inbound: an edge FROM "leaf" TO
        // the seed's neighbour "mid", with no outgoing edge from the seed at all.
        let (snapshot, entity_ids) = fixture(
            &[
                ("ward:X05001499", ward("Sample Ward")),
                ("GB-COH:mid", entity("Mid Co")),
            ],
            &[("GB-COH:mid", "ward:X05001499")], // mid -> seed (inbound to the seed)
        );

        let seeds = find_ward_seeds(&snapshot, &entity_ids, &["X05001499".to_string()]);
        assert_eq!(seeds.len(), 1);

        let scope = build(&snapshot, &seeds);

        let mid_idx = entity_ids.iter().position(|e| e.0 == "GB-COH:mid").unwrap() as u32;
        assert!(
            scope.node_ids.contains(&mid_idx),
            "a node reachable only via an inbound edge from a seed must be included \
             (undirected BFS, mirrors filter_reachable_undirected_bfs_keeps_node_reachable_only_via_inbound_edge)"
        );
    }

    #[test]
    fn build_excludes_a_node_with_no_path_to_any_seed() {
        let (snapshot, entity_ids) = fixture(
            &[
                ("ward:X05001499", ward("Sample Ward")),
                ("GB-COH:reachable", entity("Reachable Co")),
                ("GB-COH:isolated", entity("Isolated Co")),
            ],
            &[("ward:X05001499", "GB-COH:reachable")],
        );

        let seeds = find_ward_seeds(&snapshot, &entity_ids, &["X05001499".to_string()]);
        let scope = build(&snapshot, &seeds);

        let isolated_idx = entity_ids
            .iter()
            .position(|e| e.0 == "GB-COH:isolated")
            .unwrap() as u32;
        assert!(
            !scope.node_ids.contains(&isolated_idx),
            "a node with no path to any seed must be excluded"
        );
        assert_eq!(
            scope.node_ids.len(),
            2,
            "exactly the ward seed plus its one reachable neighbour"
        );
    }

    #[test]
    fn build_with_zero_seeds_returns_empty_never_the_whole_graph() {
        let (snapshot, entity_ids) = fixture(
            &[
                ("ward:X05001499", ward("Sample Ward")),
                ("GB-COH:other", entity("Other Co")),
            ],
            &[("ward:X05001499", "GB-COH:other")],
        );

        // No codes resolve to any Ward node.
        let seeds = find_ward_seeds(&snapshot, &entity_ids, &["W09999999".to_string()]);
        assert!(seeds.is_empty());

        let scope = build(&snapshot, &seeds);

        assert!(
            scope.node_ids.is_empty(),
            "zero resolvable seeds must produce an empty PopulationScope, never silently \
             falling back to the whole graph"
        );
        assert_eq!(scope.ward_seed_count, 0);
    }
}
