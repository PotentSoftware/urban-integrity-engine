//! Host-side test proving `bake_snapshot` mints ONE run_id per bake and stamps it onto every
//! `MotifDetected` event that bake produces (Phase 46, D-05/D-06). Pure in-memory — no
//! SurrealDB required, matching `coordinator_update_node.rs`'s convention.

use std::collections::HashMap;
use std::sync::Arc;

use urban_integrity_core::graph::coordinator::{
    CoordinatorConfig, GraphCoordinator, ImdRunState, TierBDetector,
};
use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_core::graph::types::GraphEventKind;

/// A `TierBDetector` stub returning two distinct `MotifDetected` events — enough to prove "one
/// run_id per bake, not one per event" (a single event could not distinguish the two).
struct TwoEventDetector;

impl TierBDetector for TwoEventDetector {
    fn run_tier_b(
        &self,
        _snapshot: &CsrSnapshot,
        _delta_nodes: &[u32],
        _id_map: &HashMap<u32, String>,
    ) -> Vec<GraphEventKind> {
        vec![
            GraphEventKind::MotifDetected {
                result_json: serde_json::json!({ "motif_type": "PhoenixLoop" }).to_string(),
            },
            GraphEventKind::MotifDetected {
                result_json: serde_json::json!({ "motif_type": "AddressCluster" }).to_string(),
            },
        ]
    }

    fn imd_state(&self) -> ImdRunState {
        ImdRunState {
            loaded: true,
            csv_path: Some("data/wimd_2019_wards.csv".to_string()),
            denominator_declared: true,
        }
    }
}

#[tokio::test]
async fn bake_snapshot_stamps_the_same_run_id_on_every_event_in_one_bake() {
    let (wal_tx, wal_rx) = crossbeam_channel::unbounded();
    let config = CoordinatorConfig {
        snapshot_every_n_events: 100_000,
        snapshot_interval_secs: 10_000,
    };
    let (coordinator, handle) =
        GraphCoordinator::new(config, wal_tx, Some(Arc::new(TwoEventDetector)));

    let task = tokio::spawn(async move { coordinator.run().await });
    handle.checkpoint_tx.send(()).await.unwrap();

    // Give the coordinator time to process the checkpoint and bake — matches
    // `coordinator_update_node.rs`'s "give the coordinator time to process" idiom.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    task.abort();

    let mut run_ids: Vec<String> = Vec::new();
    while let Ok(ev) = wal_rx.try_recv() {
        if let GraphEventKind::MotifDetected { result_json } = ev.mutation {
            let value: serde_json::Value =
                serde_json::from_str(&result_json).expect("result_json must be valid JSON");
            let run_id = value["run_id"]
                .as_str()
                .expect("result_json must carry an injected run_id field")
                .to_string();
            run_ids.push(run_id);
        }
    }

    assert_eq!(
        run_ids.len(),
        2,
        "both MotifDetected events from the stub detector must have been sent on the WAL \
         channel, got: {run_ids:?}"
    );
    assert!(!run_ids[0].is_empty(), "run_id must be non-empty");
    assert_eq!(
        run_ids[0], run_ids[1],
        "every MotifDetected event produced by ONE bake must carry the SAME run_id, not a \
         distinct one per event"
    );
}
