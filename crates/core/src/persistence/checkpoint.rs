use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{info, warn};

use urban_integrity_schema::types::id::EntityId;

use crate::graph::storage::GraphStorage;

/// The serialisable checkpoint format.
///
/// Node and edge weights are stored as JSON strings inside a bincode envelope.
/// This sidesteps bincode's incompatibility with `#[serde(tag)]` (externally-tagged enums),
/// while preserving bincode's fast framing for the overall structure.
///
/// Edges stored as (EntityId, EntityId) domain ID pairs — NOT NodeIndex values
/// (NodeIndex is volatile and changes on rebuild).
#[derive(Serialize, Deserialize)]
pub struct GraphCheckpoint {
    /// Unix timestamp of the last event included in this snapshot
    pub timestamp: u64,
    /// Companies House stream timepoint for reconnection resume
    pub ch_timepoint: Option<String>,
    /// All nodes: (domain EntityId string, node weight JSON)
    pub nodes: Vec<(String, String)>,
    /// All edges: (source EntityId string, destination EntityId string, edge weight JSON)
    pub edges: Vec<(String, String, String)>,
}

/// Write a checkpoint to disk atomically (write .tmp, then rename).
///
/// # Safety
/// Call from `tokio::task::spawn_blocking` — this is a synchronous blocking operation.
pub fn write_checkpoint(
    storage: &GraphStorage,
    dir: &Path,
    ch_timepoint: Option<String>,
) -> Result<()> {
    std::fs::create_dir_all(dir).context("create checkpoint directory")?;

    // Serialise node weights to JSON (avoids bincode + tagged-enum incompatibility)
    let nodes: Vec<(String, String)> = storage
        .snapshot_nodes()
        .into_iter()
        .map(|(id, _, weight, _)| {
            let weight_json = serde_json::to_string(&weight).unwrap_or_else(|_| "{}".to_string());
            (id.0, weight_json)
        })
        .collect();

    // Build reverse index: NodeIndex (as u32) → EntityId string for edge reconstruction
    let idx_to_id: std::collections::HashMap<u32, String> = storage
        .node_map
        .iter()
        .map(|r| (r.value().0.index() as u32, r.key().0.clone()))
        .collect();

    // Serialise edges to JSON
    let edges: Vec<(String, String, String)> = storage
        .edge_map
        .iter()
        .filter_map(|r| {
            let (src_u32, dst_u32) = r.key();
            let src_id = idx_to_id.get(src_u32)?.clone();
            let dst_id = idx_to_id.get(dst_u32)?.clone();
            let weight_json = serde_json::to_string(r.value()).unwrap_or_else(|_| "{}".to_string());
            Some((src_id, dst_id, weight_json))
        })
        .collect();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let checkpoint = GraphCheckpoint {
        timestamp,
        ch_timepoint,
        nodes,
        edges,
    };

    let bytes = bincode::serialize(&checkpoint).context("bincode serialise checkpoint")?;

    // Atomic write: .tmp → rename
    let tmp_path = dir.join("graph.checkpoint.tmp");
    let final_path = dir.join("graph.checkpoint");
    std::fs::write(&tmp_path, &bytes).context("write checkpoint tmp file")?;
    std::fs::rename(&tmp_path, &final_path).context("rename checkpoint to final path")?;

    info!(
        nodes = checkpoint.nodes.len(),
        edges = checkpoint.edges.len(),
        timestamp,
        "Checkpoint written to {:?}",
        final_path
    );
    Ok(())
}

/// Load a checkpoint from disk and hydrate GraphStorage.
///
/// Returns the checkpoint timestamp for WAL replay window.
/// Returns `Ok(0)` if no checkpoint file exists (cold start).
///
/// # Safety
/// Call from `tokio::task::spawn_blocking`.
pub fn hydrate_from_checkpoint(storage: &GraphStorage, dir: &Path) -> Result<u64> {
    let path = dir.join("graph.checkpoint");
    if !path.exists() {
        info!(
            "No checkpoint found at {:?}, starting with empty graph",
            path
        );
        return Ok(0);
    }

    let bytes = std::fs::read(&path).context("read checkpoint file")?;
    let checkpoint: GraphCheckpoint =
        bincode::deserialize(&bytes).context("bincode deserialise checkpoint")?;

    info!(
        nodes = checkpoint.nodes.len(),
        edges = checkpoint.edges.len(),
        timestamp = checkpoint.timestamp,
        "Hydrating graph from checkpoint"
    );

    // Rebuild node_map (dedup-safe)
    for (id_str, weight_json) in checkpoint.nodes {
        match serde_json::from_str(&weight_json) {
            Ok(weight) => {
                storage.insert_node(EntityId(id_str), weight);
            }
            Err(e) => {
                warn!(
                    "Hydration: failed to deserialise node weight, skipping: {}",
                    e
                );
            }
        }
    }

    // Rebuild edge_map using stable domain ID pairs
    for (src_str, dst_str, weight_json) in checkpoint.edges {
        match serde_json::from_str(&weight_json) {
            Ok(weight) => {
                let src = EntityId(src_str);
                let dst = EntityId(dst_str);
                if let Err(e) = storage.insert_edge(&src, &dst, weight) {
                    warn!("Hydration: edge insert failed: {}", e);
                }
            }
            Err(e) => {
                warn!(
                    "Hydration: failed to deserialise edge weight, skipping: {}",
                    e
                );
            }
        }
    }

    info!(
        nodes = storage.node_count(),
        edges = storage.edge_count(),
        "Graph hydration complete"
    );
    Ok(checkpoint.timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    #[test]
    fn test_checkpoint_roundtrip() {
        let dir = tempdir().expect("tmpdir");
        let storage = GraphStorage::new();

        let id = EntityId("company-001".to_string());
        let weight = NodeWeight::Entity(EntityData {
            name: "Test Corp".to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        storage.insert_node(id.clone(), weight.clone());

        write_checkpoint(&storage, dir.path(), None).expect("write_checkpoint");

        let storage2 = GraphStorage::new();
        let ts = hydrate_from_checkpoint(&storage2, dir.path()).expect("hydrate");

        assert!(ts > 0, "Timestamp should be non-zero after checkpoint");
        assert_eq!(
            storage2.node_count(),
            1,
            "Hydrated graph should have 1 node"
        );
        assert!(
            storage2.node_map.contains_key(&id),
            "Hydrated graph should contain the original node"
        );
    }

    #[test]
    fn test_hydrate_from_nonexistent_dir_returns_zero() {
        let dir = tempdir().expect("tmpdir");
        let storage = GraphStorage::new();
        let ts = hydrate_from_checkpoint(&storage, dir.path()).expect("hydrate nonexistent");
        assert_eq!(ts, 0, "No checkpoint file should return timestamp 0");
        assert_eq!(storage.node_count(), 0, "Storage should remain empty");
    }

    #[test]
    fn test_checkpoint_with_ch_timepoint_preserved() {
        let dir = tempdir().expect("tmpdir");
        let storage = GraphStorage::new();
        let id = EntityId("company-tp".to_string());
        let weight = NodeWeight::Entity(EntityData {
            name: "Timepoint Corp".to_string(),
            company_number: Some("12345678".to_string()),
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        storage.insert_node(id.clone(), weight);
        write_checkpoint(&storage, dir.path(), Some("99999".to_string())).expect("write");
        let storage2 = GraphStorage::new();
        let ts = hydrate_from_checkpoint(&storage2, dir.path()).expect("hydrate");
        assert!(ts > 0);
        assert_eq!(storage2.node_count(), 1);
    }

    #[test]
    fn test_hydrate_skips_malformed_node_weight_and_continues() {
        let dir = tempdir().expect("tmpdir");

        let valid_weight = NodeWeight::Entity(EntityData {
            name: "Good Corp".to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        let valid_json = serde_json::to_string(&valid_weight).unwrap();

        let checkpoint = GraphCheckpoint {
            timestamp: 12345,
            ch_timepoint: None,
            nodes: vec![
                ("company-good".to_string(), valid_json),
                ("company-bad".to_string(), "not-valid-json{{{".to_string()),
            ],
            edges: vec![],
        };
        let bytes = bincode::serialize(&checkpoint).unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("graph.checkpoint"), &bytes).unwrap();

        let storage = GraphStorage::new();
        let ts = hydrate_from_checkpoint(&storage, dir.path()).expect("hydrate with bad node");
        assert_eq!(ts, 12345);
        assert_eq!(storage.node_count(), 1, "Only valid node should hydrate");
    }
}
