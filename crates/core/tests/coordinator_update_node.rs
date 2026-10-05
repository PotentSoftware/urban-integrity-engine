/// Unit tests for GraphMutation::UpdateNode round-trip including CSR snapshot propagation.
///
/// All tests are pure in-memory — no SurrealDB required.
/// These tests verify D-07 and D-08 from the phase 18 design notes.
use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
use urban_integrity_core::graph::snapshot::bake_csr;
use urban_integrity_core::graph::types::{GraphEventKind, GraphMutation};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

fn make_entity_weight(name: &str, status: EntityStatus) -> NodeWeight {
    NodeWeight::Entity(EntityData {
        name: name.to_string(),
        company_number: None,
        status,
        incorporation_date: None,
    })
}

#[tokio::test]
async fn coordinator_update_node_changes_weight() {
    let (wal_tx, _wal_rx) = crossbeam_channel::unbounded();
    let config = CoordinatorConfig {
        snapshot_every_n_events: 100_000,
        snapshot_interval_secs: 10_000,
    };
    let (coordinator, handle) = GraphCoordinator::new(config, wal_tx, None);
    let storage = handle.storage.clone();
    let entity_id = EntityId("GB-COH:12345678".to_string());

    // First: insert a node via AddNode
    handle
        .mutation_tx
        .send(GraphMutation::AddNode {
            id: entity_id.clone(),
            weight: make_entity_weight("Original Corp", EntityStatus::Active),
        })
        .await
        .unwrap();

    // Then: send UpdateNode with a new weight (InLiquidation status)
    let gazette_date = chrono::Utc::now();
    handle
        .mutation_tx
        .send(GraphMutation::UpdateNode {
            id: entity_id.clone(),
            weight: make_entity_weight("Original Corp", EntityStatus::InLiquidation(gazette_date)),
        })
        .await
        .unwrap();

    // Spawn the coordinator to process both mutations
    let task = tokio::spawn(async move { coordinator.run().await });

    // Give the coordinator time to process both messages
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    task.abort();

    // Assert: node_map entry has the updated weight (InLiquidation)
    let entry = storage.node_map.get(&entity_id).expect("node must exist");
    match &entry.value().1 {
        NodeWeight::Entity(data) => {
            assert!(
                matches!(data.status, EntityStatus::InLiquidation(_)),
                "Expected InLiquidation status after UpdateNode, got: {:?}",
                data.status
            );
        }
        other => panic!("Expected Entity weight, got: {:?}", other),
    }
}

#[tokio::test]
async fn coordinator_update_node_emits_wal_event() {
    let (wal_tx, wal_rx) = crossbeam_channel::unbounded();
    let config = CoordinatorConfig {
        snapshot_every_n_events: 100_000,
        snapshot_interval_secs: 10_000,
    };
    let (coordinator, handle) = GraphCoordinator::new(config, wal_tx, None);
    let entity_id = EntityId("GB-COH:99887766".to_string());

    // Insert first, then update
    handle
        .mutation_tx
        .send(GraphMutation::AddNode {
            id: entity_id.clone(),
            weight: make_entity_weight("WAL Test Corp", EntityStatus::Active),
        })
        .await
        .unwrap();

    let gazette_date = chrono::Utc::now();
    handle
        .mutation_tx
        .send(GraphMutation::UpdateNode {
            id: entity_id.clone(),
            weight: make_entity_weight("WAL Test Corp", EntityStatus::InLiquidation(gazette_date)),
        })
        .await
        .unwrap();

    let task = tokio::spawn(async move { coordinator.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    task.abort();

    // Drain WAL events and find a NodeUpdated event
    let mut found_node_added = false;
    let mut found_node_updated = false;
    while let Ok(event) = wal_rx.try_recv() {
        match &event.mutation {
            GraphEventKind::NodeAdded { entity_id: eid, .. } if eid == "GB-COH:99887766" => {
                found_node_added = true;
            }
            GraphEventKind::NodeUpdated {
                entity_id: eid,
                weight_json,
            } if eid == "GB-COH:99887766" => {
                found_node_updated = true;
                assert!(
                    weight_json.contains("InLiquidation"),
                    "weight_json must contain InLiquidation status"
                );
            }
            _ => {}
        }
    }
    assert!(found_node_added, "NodeAdded WAL event must be emitted");
    assert!(
        found_node_updated,
        "NodeUpdated WAL event must be emitted after UpdateNode"
    );
}

#[tokio::test]
async fn coordinator_update_node_no_topology_change() {
    let (wal_tx, _wal_rx) = crossbeam_channel::unbounded();
    let config = CoordinatorConfig {
        snapshot_every_n_events: 100_000,
        snapshot_interval_secs: 10_000,
    };
    let (_coordinator, handle) = GraphCoordinator::new(config, wal_tx, None);
    let storage = handle.storage.clone();
    let entity_id = EntityId("GB-COH:11223344".to_string());

    // Insert node directly via storage
    storage.insert_node(
        entity_id.clone(),
        make_entity_weight("Topology Corp", EntityStatus::Active),
    );

    // Record the NodeIndex before UpdateNode
    let idx_before = storage
        .node_map
        .get(&entity_id)
        .expect("node must exist after insert_node")
        .value()
        .0;

    // Call update_node_weight directly on storage
    let gazette_date = chrono::Utc::now();
    storage.update_node_weight(
        entity_id.clone(),
        make_entity_weight("Topology Corp", EntityStatus::InLiquidation(gazette_date)),
    );

    // NodeIndex must be unchanged
    let idx_after = storage
        .node_map
        .get(&entity_id)
        .expect("node must still exist after update")
        .value()
        .0;

    assert_eq!(
        idx_before, idx_after,
        "NodeIndex must not change after UpdateNode (topology unchanged)"
    );
}

#[tokio::test]
async fn coordinator_update_node_noop_on_missing() {
    let (wal_tx, wal_rx) = crossbeam_channel::unbounded();
    let config = CoordinatorConfig {
        snapshot_every_n_events: 100_000,
        snapshot_interval_secs: 10_000,
    };
    let (coordinator, handle) = GraphCoordinator::new(config, wal_tx, None);
    let storage = handle.storage.clone();
    let missing_id = EntityId("GB-COH:00000000".to_string());

    handle
        .mutation_tx
        .send(GraphMutation::UpdateNode {
            id: missing_id.clone(),
            weight: make_entity_weight("Ghost Corp", EntityStatus::Active),
        })
        .await
        .unwrap();

    let task = tokio::spawn(async move { coordinator.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    task.abort();

    // node_map must remain empty (no node created for missing ID)
    assert!(
        storage.node_map.get(&missing_id).is_none(),
        "UpdateNode on missing id must not create a new node"
    );

    // WAL must NOT emit a NodeUpdated event for the missing entity
    let mut found_updated = false;
    while let Ok(event) = wal_rx.try_recv() {
        if matches!(&event.mutation, GraphEventKind::NodeUpdated { entity_id, .. } if entity_id == "GB-COH:00000000")
        {
            found_updated = true;
        }
    }
    assert!(
        !found_updated,
        "UpdateNode on missing node must NOT emit NodeUpdated WAL event"
    );
}

/// Tests that after apply_mutation(UpdateNode) + bake_snapshot(), the CSR snapshot
/// returns the updated NodeWeight with EntityStatus::InLiquidation.
///
/// This confirms the CSR snapshot propagates the UpdateNode result before the
/// next detector cycle (D-07, D-08 from the phase 18 design notes).
#[tokio::test]
async fn snapshot_entity_status() {
    use std::sync::Arc;
    use urban_integrity_core::graph::storage::GraphStorage;

    let storage = Arc::new(GraphStorage::new());
    let entity_id = EntityId("GB-COH:55443322".to_string());

    // Insert a node with Active status
    storage.insert_node(
        entity_id.clone(),
        make_entity_weight("Snapshot Corp", EntityStatus::Active),
    );

    // Directly call update_node_weight (simulating apply_mutation(UpdateNode))
    let gazette_date = chrono::Utc::now();
    storage.update_node_weight(
        entity_id.clone(),
        make_entity_weight("Snapshot Corp", EntityStatus::InLiquidation(gazette_date)),
    );

    // Bake the CSR snapshot synchronously
    let event_count = storage
        .event_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let snapshot = bake_csr(&storage, event_count);

    // Find the entity's NodeWeight in the CSR snapshot.
    // petgraph::csr::Csr uses index-based access: graph[node_index: u32]
    let node_count = snapshot.graph.node_count();
    let found_in_liquidation = (0..node_count).any(|i| {
        matches!(
            &snapshot.graph[i as u32],
            NodeWeight::Entity(EntityData {
                status: EntityStatus::InLiquidation(_),
                ..
            })
        )
    });

    assert!(
        found_in_liquidation,
        "CSR snapshot must reflect InLiquidation status after UpdateNode + bake_snapshot"
    );

    // Also assert the Active-status node is gone from the snapshot (it was updated in-place)
    let still_active = (0..node_count)
        .filter(|&i| {
            matches!(
                &snapshot.graph[i as u32],
                NodeWeight::Entity(EntityData {
                    status: EntityStatus::Active,
                    ..
                })
            )
        })
        .count();
    assert_eq!(
        still_active, 0,
        "No Active entity nodes should remain in snapshot after UpdateNode to InLiquidation"
    );
}
