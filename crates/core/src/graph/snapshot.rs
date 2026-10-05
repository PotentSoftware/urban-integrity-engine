use arc_swap::ArcSwap;
// In petgraph's Csr, NodeIndex<Ix=DefaultIx> is actually an alias for Ix (u32).
// We use petgraph::csr::NodeIndex (u32) for Csr, distinct from petgraph::graph::NodeIndex.
use petgraph::csr::Csr;
use petgraph::graph::NodeIndex as GraphNodeIndex;
use std::sync::Arc;
use tracing::info;

use urban_integrity_schema::types::edge::EdgeWeight;
use urban_integrity_schema::types::node::NodeWeight;

use super::storage::GraphStorage;

/// petgraph Csr uses DefaultIx = u32 as the NodeIndex type alias.
type CsrNodeIndex = u32;

/// An immutable CSR snapshot of the graph, baked from DashMap state.
pub struct CsrSnapshot {
    pub graph: Csr<NodeWeight, EdgeWeight>,
    /// Event count at time of bake — used for diff tracking
    pub baked_at_event: u64,
    /// Event ID when each node was last modified (indexed by CsrNodeIndex)
    pub node_event_ids: Vec<u64>,
    /// Incoming adjacency, indexed by CsrNodeIndex: `reverse_adj[n]` lists every node with
    /// an edge pointing AT `n`.
    ///
    /// `petgraph::Csr` is directed and `neighbors_slice` yields outgoing edges only, so a
    /// multi-hop motif whose hops run in opposite directions cannot be expressed against the
    /// forward graph alone. Phoenix is exactly that shape — `seed -> officer` then
    /// `officer -> other_company` — and it had never been able to fire because of it
    /// (planning open item 20).
    ///
    /// It is a genuine index rather than a convention fix because the two ingest paths
    /// disagree on `Directs` direction: `ch_backfill.py` writes company -> officer (22,807
    /// live) and `ch/mapper.rs` writes officer -> company (8 live). Making a traversal
    /// direction-agnostic is robust to that; picking a side is not.
    pub reverse_adj: Vec<Vec<u32>>,
}

impl CsrSnapshot {
    pub fn delta_since(&self, previous_event: u64) -> Vec<u32> {
        self.node_event_ids
            .iter()
            .enumerate()
            .filter(|(_, &event_id)| event_id > previous_event)
            .map(|(idx, _)| idx as u32)
            .collect()
    }

    /// Nodes with an edge pointing AT `idx`. Empty slice if `idx` is out of range.
    pub fn neighbors_in(&self, idx: u32) -> &[u32] {
        self.reverse_adj
            .get(idx as usize)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Every node adjacent to `idx` in EITHER direction, deduplicated.
    ///
    /// Use this for motif hops whose edge direction is not guaranteed by ingest. It is
    /// deliberately not the default: `AddressCluster` and `Nepotism` rely on outgoing-only
    /// semantics and must keep them.
    pub fn neighbors_undirected(&self, idx: u32) -> Vec<u32> {
        let mut out: Vec<u32> = self.graph.neighbors_slice(idx).to_vec();
        for &n in self.neighbors_in(idx) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }
}

/// Thread-safe, lock-free handle to the current CSR snapshot.
/// Readers call `.load()` for a zero-copy Arc clone. Writers call `.store()` to rotate.
pub struct SnapshotHandle {
    inner: ArcSwap<CsrSnapshot>,
}

impl SnapshotHandle {
    /// Create a new handle with an empty initial snapshot.
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            inner: ArcSwap::new(Arc::new(CsrSnapshot {
                graph: Csr::new(),
                baked_at_event: 0,
                node_event_ids: Vec::new(),
                reverse_adj: Vec::new(),
            })),
        })
    }

    /// Load the current snapshot. Lock-free on the read path.
    pub fn load(&self) -> arc_swap::Guard<Arc<CsrSnapshot>> {
        self.inner.load()
    }

    /// Replace the snapshot with a freshly baked one.
    pub fn store(&self, snapshot: CsrSnapshot) {
        self.inner.store(Arc::new(snapshot));
    }
}

/// Bake a new CSR snapshot from the current storage state.
///
/// Must be called from a `tokio::task::spawn_blocking` context.
/// The DashMap iteration is synchronous and acquires shard locks briefly.
pub fn bake_csr(storage: &GraphStorage, event_count: u64) -> CsrSnapshot {
    // Collect node weights and event IDs sorted by GraphNodeIndex
    let mut node_entries: Vec<(GraphNodeIndex, NodeWeight, u64)> = storage
        .snapshot_nodes()
        .into_iter()
        .map(|(_, idx, w, ev)| (idx, w, ev))
        .collect();
    node_entries.sort_unstable_by_key(|(idx, _, _)| idx.index());

    let node_count = node_entries.len();

    // Collect edges sorted by source (as u32 for Csr compatibility)
    let raw_edges = storage.snapshot_edges();
    let mut sorted_edges: Vec<(CsrNodeIndex, CsrNodeIndex, EdgeWeight)> = raw_edges
        .into_iter()
        .map(|(src, dst, w)| (src.index() as u32, dst.index() as u32, w))
        .collect();
    sorted_edges.sort_unstable_by_key(|e| e.0);

    // Build Csr by adding nodes in order then edges.
    let mut csr: Csr<NodeWeight, EdgeWeight> = Csr::new();
    let mut node_event_ids = Vec::with_capacity(node_count);

    for (_, weight, event_id) in node_entries {
        csr.add_node(weight);
        node_event_ids.push(event_id);
    }

    // Build the incoming-adjacency index alongside the forward graph. One pass over the
    // same edge list, so bake cost is O(E) extra and memory is one u32 per edge.
    let mut reverse_adj: Vec<Vec<u32>> = vec![Vec::new(); node_count];
    for (src, dst, weight) in sorted_edges {
        if let Some(slot) = reverse_adj.get_mut(dst as usize) {
            slot.push(src);
        }
        csr.add_edge(src, dst, weight);
    }

    info!(nodes = node_count, event_count, "CSR snapshot baked");

    CsrSnapshot {
        graph: csr,
        baked_at_event: event_count,
        node_event_ids,
        reverse_adj,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    fn make_storage_with_nodes(count: usize) -> GraphStorage {
        let storage = GraphStorage::new();
        for i in 0..count {
            let id = EntityId(format!("company-{:04}", i));
            let weight = NodeWeight::Entity(EntityData {
                name: format!("Company {}", i),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            });
            storage.insert_node(id, weight);
        }
        storage
    }

    fn make_edge_weight() -> EdgeWeight {
        EdgeWeight::Directs {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: DirectsData {
                details: "director".to_string(),
                officer_role: None,
            },
        }
    }

    #[test]
    fn test_bake_csr_empty_storage() {
        let storage = GraphStorage::new();
        let snapshot = bake_csr(&storage, 0);
        assert_eq!(
            snapshot.graph.node_count(),
            0,
            "Empty storage yields empty CSR"
        );
        assert_eq!(snapshot.baked_at_event, 0);
    }

    #[test]
    fn test_bake_csr_node_count_matches_storage() {
        let storage = make_storage_with_nodes(50);
        let snapshot = bake_csr(&storage, 50);
        assert_eq!(
            snapshot.graph.node_count(),
            50,
            "CSR node count must match storage node count"
        );
        assert_eq!(snapshot.baked_at_event, 50);
    }

    #[test]
    fn test_bake_csr_edge_count_matches_storage() {
        let storage = make_storage_with_nodes(3);
        let ids: Vec<EntityId> = (0..3)
            .map(|i| EntityId(format!("company-{:04}", i)))
            .collect();

        storage
            .insert_edge(&ids[0], &ids[1], make_edge_weight())
            .expect("edge 0→1");
        storage
            .insert_edge(&ids[1], &ids[2], make_edge_weight())
            .expect("edge 1→2");

        let snapshot = bake_csr(&storage, 5);
        assert_eq!(snapshot.graph.node_count(), 3);
        assert_eq!(snapshot.graph.edge_count(), 2, "CSR edge count must match");
    }

    #[test]
    fn test_bake_csr_baked_at_event_recorded() {
        let storage = make_storage_with_nodes(10);
        let snapshot = bake_csr(&storage, 42);
        assert_eq!(
            snapshot.baked_at_event, 42,
            "baked_at_event must record the event count"
        );
    }

    #[test]
    fn test_snapshot_handle_empty_initialises() {
        let handle = SnapshotHandle::empty();
        let guard = handle.load();
        assert_eq!(
            guard.graph.node_count(),
            0,
            "Initial snapshot should be empty"
        );
        assert_eq!(guard.baked_at_event, 0);
    }

    #[test]
    fn test_snapshot_handle_store_rotates_snapshot() {
        let handle = SnapshotHandle::empty();
        let storage = make_storage_with_nodes(5);
        let new_snapshot = bake_csr(&storage, 99);
        handle.store(new_snapshot);

        let guard = handle.load();
        assert_eq!(
            guard.graph.node_count(),
            5,
            "Stored snapshot should be visible after rotation"
        );
        assert_eq!(guard.baked_at_event, 99);
    }

    #[test]
    fn test_snapshot_handle_concurrent_read() {
        use std::sync::Arc;

        let handle = Arc::new(SnapshotHandle::empty());
        let storage = make_storage_with_nodes(20);
        let snapshot = bake_csr(&storage, 20);
        handle.store(snapshot);

        // Two threads reading simultaneously — no locks, no panics
        let h1 = handle.clone();
        let h2 = handle.clone();
        let t1 = std::thread::spawn(move || {
            let guard = h1.load();
            guard.graph.node_count()
        });
        let t2 = std::thread::spawn(move || {
            let guard = h2.load();
            guard.graph.node_count()
        });

        let c1 = t1.join().expect("thread 1");
        let c2 = t2.join().expect("thread 2");
        assert_eq!(c1, 20, "Thread 1 should see 20 nodes");
        assert_eq!(c2, 20, "Thread 2 should see 20 nodes");
    }
}
