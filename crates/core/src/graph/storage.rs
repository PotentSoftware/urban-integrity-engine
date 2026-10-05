use dashmap::DashMap;
use fxhash::FxHashMap;
use petgraph::graph::NodeIndex;
use smallvec::SmallVec;
use std::sync::atomic::{AtomicU64, Ordering};

use urban_integrity_schema::types::edge::EdgeWeight;
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::NodeWeight;

type EdgeEntry = (NodeIndex, EdgeWeight);
type Shard = parking_lot::RwLock<FxHashMap<NodeIndex, SmallVec<[EdgeEntry; 4]>>>;

/// Outcome of [`GraphStorage::upsert_node`], used by the coordinator to decide
/// which WAL event to emit.
#[derive(Debug, Clone, PartialEq)]
pub enum UpsertOutcome {
    /// Node did not exist and was inserted.
    Inserted,
    /// Node existed and the merged weight differs; carries the full merged weight.
    ///
    /// Boxed (clippy::large_enum_variant, Phase 24): `NodeWeight` grew past the
    /// threshold once `ContractData` gained `applicant_provenance`/`portal_detail_handle`
    /// (D-02) — boxing keeps `Inserted`/`Unchanged` from paying for the largest variant.
    Updated(Box<NodeWeight>),
    /// Node existed and the merge produced an identical weight (no-op).
    Unchanged,
}

/// The operational in-memory graph index.
/// Sharded Adjacency Model with 128 shards for lock-contention-free ingestion.
pub struct GraphStorage {
    /// Maps domain EntityId → (petgraph NodeIndex, NodeWeight, last_event_id)
    pub node_map: DashMap<EntityId, (NodeIndex, NodeWeight, u64)>,
    /// Maps (src NodeIndex as u32, dst NodeIndex as u32) → EdgeWeight
    /// Used for O(1) duplicate checks and attribute updates.
    pub edge_map: DashMap<(u32, u32), EdgeWeight>,
    /// Sharded adjacency lists for CSR baking and traversal.
    /// Shard index = fxhash(src_idx) % 128
    pub shards: [Shard; 128],
    /// Monotonically increasing index counter
    next_index: AtomicU64,
    /// Total mutation events processed
    pub event_count: AtomicU64,
}

impl GraphStorage {
    pub fn new() -> Self {
        Self {
            node_map: DashMap::with_shard_amount(64),
            edge_map: DashMap::with_shard_amount(64),
            shards: std::array::from_fn(|_| parking_lot::RwLock::new(FxHashMap::default())),
            next_index: AtomicU64::new(0),
            event_count: AtomicU64::new(0),
        }
    }

    /// Insert a node. Returns (NodeIndex, is_new).
    pub fn insert_node(&self, id: EntityId, weight: NodeWeight) -> (NodeIndex, bool) {
        use dashmap::mapref::entry::Entry;
        match self.node_map.entry(id) {
            Entry::Occupied(e) => (e.get().0, false),
            Entry::Vacant(e) => {
                let idx_u64 = self.next_index.fetch_add(1, Ordering::Relaxed);
                let event_id = self.event_count.fetch_add(1, Ordering::Relaxed);
                let idx = NodeIndex::new(idx_u64 as usize);
                e.insert((idx, weight, event_id));
                (idx, true)
            }
        }
    }

    /// Insert-or-merge a node. Unlike [`insert_node`](Self::insert_node) (insert-only)
    /// this folds `incoming` into any existing weight via
    /// [`merge_node_weight`](crate::graph::merge::merge_node_weight), so re-ingested
    /// status transitions (e.g. Companies House `active -> dissolved`) are applied
    /// instead of silently dropped.
    ///
    /// The merge and comparison happen under the DashMap entry lock, so the
    /// read-merge-write is atomic per node. Returns an [`UpsertOutcome`] telling the
    /// caller which WAL event (if any) to emit:
    /// - `Inserted` — node was new (emit `NodeAdded`).
    /// - `Updated(merged)` — existing weight changed (emit `NodeUpdated` with the
    ///   returned merged weight, which is the full post-merge state).
    /// - `Unchanged` — merge produced an identical weight (emit nothing; idempotent).
    pub fn upsert_node(&self, id: EntityId, incoming: NodeWeight) -> UpsertOutcome {
        use dashmap::mapref::entry::Entry;
        match self.node_map.entry(id) {
            Entry::Vacant(e) => {
                let idx_u64 = self.next_index.fetch_add(1, Ordering::Relaxed);
                let event_id = self.event_count.fetch_add(1, Ordering::Relaxed);
                let idx = NodeIndex::new(idx_u64 as usize);
                e.insert((idx, incoming, event_id));
                UpsertOutcome::Inserted
            }
            Entry::Occupied(mut e) => {
                let merged = crate::graph::merge::merge_node_weight(&e.get().1, incoming);
                if merged == e.get().1 {
                    UpsertOutcome::Unchanged
                } else {
                    let event_id = self.event_count.fetch_add(1, Ordering::Relaxed);
                    {
                        let slot = e.get_mut();
                        slot.1 = merged.clone();
                        slot.2 = event_id;
                    }
                    UpsertOutcome::Updated(Box::new(merged))
                }
            }
        }
    }

    /// Update the weight of an existing node in-place without changing the NodeIndex.
    ///
    /// Used by the Gazette adapter (Phase 18, PROP-02) to update EntityStatus
    /// to InLiquidation. The NodeIndex (graph topology) is unchanged — only the
    /// NodeWeight tuple element is replaced.
    ///
    /// If the node does not exist, emits a WARN and returns without modification.
    /// The event counter is incremented to track the update.
    pub fn update_node_weight(&self, id: EntityId, weight: NodeWeight) {
        if let Some(mut entry) = self.node_map.get_mut(&id) {
            entry.value_mut().1 = weight;
            entry.value_mut().2 = self.event_count.fetch_add(1, Ordering::Relaxed);
        } else {
            tracing::warn!(
                entity_id = %id.0,
                "update_node_weight: node not found — no-op"
            );
        }
    }

    /// Insert a directed edge between two domain IDs.
    pub fn insert_edge(
        &self,
        src: &EntityId,
        dst: &EntityId,
        weight: EdgeWeight,
    ) -> Result<bool, String> {
        let src_idx = self
            .node_map
            .get(src)
            .map(|r| r.0)
            .ok_or_else(|| format!("Source node not found: {:?}", src))?;
        let dst_idx = self
            .node_map
            .get(dst)
            .map(|r| r.0)
            .ok_or_else(|| format!("Destination node not found: {:?}", dst))?;

        let key = (src_idx.index() as u32, dst_idx.index() as u32);
        if self.edge_map.contains_key(&key) {
            return Ok(false);
        }

        // 1. Update edge_map for O(1) checks
        self.edge_map.insert(key, weight.clone());

        // 2. Routing: hash(src_idx) % 128
        let shard_idx = (fxhash::hash64(&src_idx.index()) % 128) as usize;
        let mut shard = self.shards[shard_idx].write();
        shard.entry(src_idx).or_default().push((dst_idx, weight));

        self.event_count.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Collect all nodes for CSR baking (shard locks acquired one at a time).
    pub fn snapshot_nodes(&self) -> Vec<(EntityId, NodeIndex, NodeWeight, u64)> {
        self.node_map
            .iter()
            .map(|r| {
                (
                    r.key().clone(),
                    r.value().0,
                    r.value().1.clone(),
                    r.value().2,
                )
            })
            .collect()
    }

    /// Collect all edges for CSR baking using incremental shard-locking.
    /// This avoids a global "Stop-the-World" pause.
    pub fn snapshot_edges(&self) -> Vec<(NodeIndex, NodeIndex, EdgeWeight)> {
        let mut all_edges = Vec::with_capacity(self.edge_map.len());
        for shard_lock in &self.shards {
            let shard = shard_lock.read();
            for (src_idx, targets) in shard.iter() {
                for (dst_idx, weight) in targets {
                    all_edges.push((*src_idx, *dst_idx, weight.clone()));
                }
            }
        }
        all_edges
    }

    /// Total number of nodes currently in the index.
    pub fn node_count(&self) -> usize {
        self.node_map.len()
    }

    /// Total number of edges currently in the index.
    pub fn edge_count(&self) -> usize {
        self.edge_map.len()
    }
}

impl Default for GraphStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    fn make_entity(id_str: &str, name: &str) -> (EntityId, NodeWeight) {
        (
            EntityId(id_str.to_string()),
            NodeWeight::Entity(EntityData {
                name: name.to_string(),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        )
    }

    #[test]
    fn test_insert_node_dedup() {
        let storage = GraphStorage::new();
        let (id, weight) = make_entity("company-001", "Test Corp");
        let (idx1, is_new1) = storage.insert_node(id.clone(), weight.clone());
        let (idx2, is_new2) = storage.insert_node(id.clone(), weight);
        assert!(is_new1, "First insert should be new");
        assert!(!is_new2, "Second insert should be duplicate");
        assert_eq!(idx1, idx2, "Both inserts return same NodeIndex");
        assert_eq!(storage.node_count(), 1, "Only one node stored");
    }

    #[test]
    fn test_upsert_node_insert_then_status_transition() {
        use chrono::{TimeZone, Utc};
        let storage = GraphStorage::new();
        let id = EntityId("GB-COH:12345678".to_string());

        // First sighting: Active.
        let active = NodeWeight::Entity(EntityData {
            name: "ACME LTD".to_string(),
            company_number: Some("12345678".to_string()),
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        assert_eq!(
            storage.upsert_node(id.clone(), active.clone()),
            UpsertOutcome::Inserted
        );

        // Re-ingesting the identical weight must be a no-op (idempotent).
        assert_eq!(
            storage.upsert_node(id.clone(), active),
            UpsertOutcome::Unchanged
        );

        // Later: dissolution arrives (company_number omitted on this event).
        let dt = Utc.with_ymd_and_hms(2025, 4, 15, 0, 0, 0).unwrap();
        let dissolved = NodeWeight::Entity(EntityData {
            name: "ACME LTD".to_string(),
            company_number: None,
            status: EntityStatus::Dissolved(dt),
            incorporation_date: None,
        });
        match storage.upsert_node(id.clone(), dissolved) {
            UpsertOutcome::Updated(merged) => match *merged {
                NodeWeight::Entity(e) => {
                    assert!(matches!(e.status, EntityStatus::Dissolved(_)));
                    // Merge must preserve the CH number an earlier event supplied.
                    assert_eq!(e.company_number.as_deref(), Some("12345678"));
                }
                _ => panic!("expected Entity"),
            },
            other => panic!("expected Updated, got {other:?}"),
        }

        // A stale Active event must NOT downgrade the terminal state.
        let stale_active = NodeWeight::Entity(EntityData {
            name: "ACME LTD".to_string(),
            company_number: Some("12345678".to_string()),
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        assert_eq!(
            storage.upsert_node(id.clone(), stale_active),
            UpsertOutcome::Unchanged,
            "terminal Dissolved must not be downgraded to Active"
        );
        assert_eq!(storage.node_count(), 1, "upsert never creates a duplicate");
    }

    #[test]
    fn test_insert_edge_requires_existing_nodes() {
        let storage = GraphStorage::new();
        let (id_a, w_a) = make_entity("a", "Alpha");
        let (id_b, w_b) = make_entity("b", "Beta");
        let id_missing = EntityId("missing".to_string());

        let edge = EdgeWeight::Directs {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: DirectsData {
                details: "director".to_string(),
                officer_role: None,
            },
        };

        // Insert nodes first
        storage.insert_node(id_a.clone(), w_a);
        storage.insert_node(id_b.clone(), w_b);

        // Valid edge
        let result = storage.insert_edge(&id_a, &id_b, edge.clone());
        assert!(result.is_ok(), "Valid edge insert should succeed");
        assert!(result.unwrap(), "Should be a new edge");

        // Duplicate edge
        let dup = storage.insert_edge(&id_a, &id_b, edge.clone());
        assert!(dup.is_ok());
        assert!(!dup.unwrap(), "Duplicate edge should return false");

        // Missing node
        let bad = storage.insert_edge(&id_missing, &id_b, edge);
        assert!(bad.is_err(), "Edge to missing node should fail");
    }
}
