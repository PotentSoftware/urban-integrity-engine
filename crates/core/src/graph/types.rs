use urban_integrity_schema::types::edge::EdgeWeight;
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::NodeWeight;

/// A mutation that can be applied to the live graph.
#[derive(Debug, Clone)]
pub enum GraphMutation {
    AddNode {
        id: EntityId,
        weight: NodeWeight,
    },
    AddEdge {
        src: EntityId,
        dst: EntityId,
        weight: EdgeWeight,
    },
    RemoveNode {
        id: EntityId,
    },
    /// Update the weight of an existing node in-place without changing topology.
    ///
    /// Used by the Gazette adapter (Phase 18, PROP-02) to update EntityStatus
    /// to InLiquidation via mutation_tx (D-07, D-08).
    /// If the node does not exist, the mutation is a no-op and a WARN is logged.
    UpdateNode {
        id: EntityId,
        weight: NodeWeight,
    },
    /// Insert-or-merge a node. Emitted by the ingestion pipeline for every
    /// `NodeUpsert` event so that re-ingested data (notably Companies House status
    /// transitions such as `active -> dissolved`) is folded into the existing node
    /// via `merge_node_weight`, rather than dropped by the insert-only `AddNode`
    /// path. New node -> `NodeAdded`; changed weight -> `NodeUpdated`; identical -> no-op.
    UpsertNode {
        id: EntityId,
        weight: NodeWeight,
    },
}

/// An event recorded in the WAL for SurrealDB persistence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphEvent {
    pub id: uuid::Uuid,
    pub timestamp: u64,
    pub mutation: GraphEventKind,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum GraphEventKind {
    NodeAdded {
        entity_id: String,
        weight_json: String,
    },
    EdgeAdded {
        src_id: String,
        dst_id: String,
        weight_json: String,
    },
    NodeRemoved {
        entity_id: String,
    },
    MotifDetected {
        result_json: String,
    },
    /// Emitted when an existing node's weight is updated in-place (e.g. EntityStatus change).
    /// Payload structure mirrors NodeAdded for consistency in the SurrealDB event log.
    NodeUpdated {
        entity_id: String,
        weight_json: String,
    },
}
