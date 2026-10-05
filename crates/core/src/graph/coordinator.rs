use std::sync::Arc;
use std::time::Duration;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc;
use tracing::{error, info};

use super::snapshot::{bake_csr, CsrSnapshot, SnapshotHandle};
use super::storage::{GraphStorage, UpsertOutcome};
use super::types::{GraphEvent, GraphEventKind, GraphMutation};
use std::collections::HashMap;

/// A detector's deprivation-index state for one bake (Phase 46, D-07). Core owns this type
/// because core owns the `TierBDetector` trait's return type — `crates/detector` implements
/// against it, not the reverse (mirrors the trait's own "defined here to avoid circular
/// dependencies" rationale below).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImdRunState {
    pub loaded: bool,
    pub csv_path: Option<String>,
    pub denominator_declared: bool,
}

/// Trait for motif detectors to run after CSR snapshot generation.
/// Defined here to avoid circular dependencies with the detector crate.
pub trait TierBDetector: Send + Sync {
    fn run_tier_b(
        &self,
        snapshot: &CsrSnapshot,
        delta_nodes: &[u32],
        id_map: &HashMap<u32, String>,
    ) -> Vec<GraphEventKind>;

    /// This detector's deprivation-index state, as of construction (Phase 46, D-07) — read by
    /// `bake_snapshot` before every bake so the `detector_runs` history row it writes states
    /// whether an IMD/WIMD index was loaded, and which file, rather than defaulting silently.
    fn imd_state(&self) -> ImdRunState;
}

/// Configuration for the GraphCoordinator task.
pub struct CoordinatorConfig {
    /// Bake a new CSR snapshot every N mutations
    pub snapshot_every_n_events: u64,
    /// Bake a new CSR snapshot at least every T seconds
    pub snapshot_interval_secs: u64,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            snapshot_every_n_events: 1_000,
            snapshot_interval_secs: 30,
        }
    }
}

/// The GraphCoordinator owns all mutable graph state and runs as a long-lived Tokio task.
///
/// # Architecture
/// - Receives `GraphMutation` from ingestion layer via `mpsc::Receiver`
/// - Updates DashMap operational index (immediate, in-memory)
/// - Forwards `GraphEvent` to WAL channel (crossbeam-channel → SurrealDB writer)
/// - Periodically bakes immutable `Arc<CsrSnapshot>` for motif detection
/// - Handles checkpoint triggers (adaptive N-events, T-seconds, SIGUSR1, API)
pub struct GraphCoordinator {
    storage: Arc<GraphStorage>,
    snapshot: Arc<SnapshotHandle>,
    mutation_rx: mpsc::Receiver<GraphMutation>,
    checkpoint_trigger_rx: mpsc::Receiver<()>,
    wal_tx: crossbeam_channel::Sender<GraphEvent>,
    config: CoordinatorConfig,
    last_snapshot_event: u64,
    detector: Option<Arc<dyn TierBDetector>>,
    /// Optional SurrealDB connection, used only to persist the `detector_runs` marker
    /// (Phase 32, D1a) after a bake where a detector actually ran. `Clone` of the same
    /// `Arc` handed to `GraphCoordinatorHandle.db` — cheap, since it's an `Arc` clone.
    db: Option<Arc<Surreal<Any>>>,
}

impl GraphCoordinator {
    /// Create a new GraphCoordinator and return the public handle.
    pub fn new(
        config: CoordinatorConfig,
        wal_tx: crossbeam_channel::Sender<GraphEvent>,
        detector: Option<Arc<dyn TierBDetector>>,
    ) -> (Self, GraphCoordinatorHandle) {
        Self::new_with_db(config, wal_tx, detector, None)
    }

    /// Create a new GraphCoordinator with an optional SurrealDB connection.
    pub fn new_with_db(
        config: CoordinatorConfig,
        wal_tx: crossbeam_channel::Sender<GraphEvent>,
        detector: Option<Arc<dyn TierBDetector>>,
        db: Option<Arc<Surreal<Any>>>,
    ) -> (Self, GraphCoordinatorHandle) {
        let (mutation_tx, mutation_rx) = mpsc::channel(10_000);
        let (checkpoint_tx, checkpoint_trigger_rx) = mpsc::channel(8);
        let storage = Arc::new(GraphStorage::new());
        let snapshot = SnapshotHandle::empty();

        let coordinator = Self {
            storage: storage.clone(),
            snapshot: snapshot.clone(),
            mutation_rx,
            checkpoint_trigger_rx,
            wal_tx,
            config,
            last_snapshot_event: 0,
            detector,
            db: db.clone(),
        };

        let handle = GraphCoordinatorHandle {
            mutation_tx,
            checkpoint_tx,
            storage,
            snapshot,
            db,
        };

        (coordinator, handle)
    }

    /// Run the coordinator event loop. Call via `tokio::spawn(coordinator.run())`.
    pub async fn run(mut self) {
        let snapshot_interval = Duration::from_secs(self.config.snapshot_interval_secs);
        let mut interval = tokio::time::interval(snapshot_interval);
        // Skip the first immediate tick
        interval.tick().await;

        loop {
            tokio::select! {
                Some(mutation) = self.mutation_rx.recv() => {
                    self.apply_mutation(mutation).await;
                    let event_count = self.storage.event_count.load(std::sync::atomic::Ordering::Relaxed);
                    if event_count.saturating_sub(self.last_snapshot_event) >= self.config.snapshot_every_n_events {
                        self.bake_snapshot(event_count).await;
                    }
                }
                _ = interval.tick() => {
                    let event_count = self.storage.event_count.load(std::sync::atomic::Ordering::Relaxed);
                    info!(event_count, "Interval snapshot trigger");
                    self.bake_snapshot(event_count).await;
                }
                Some(()) = self.checkpoint_trigger_rx.recv() => {
                    let event_count = self.storage.event_count.load(std::sync::atomic::Ordering::Relaxed);
                    info!(event_count, "Manual checkpoint trigger");
                    self.bake_snapshot(event_count).await;
                }
                else => break,
            }
        }
        info!("GraphCoordinator shutting down");
    }

    async fn apply_mutation(&self, mutation: GraphMutation) {
        match mutation {
            GraphMutation::AddNode { id, weight } => {
                let (_, is_new) = self.storage.insert_node(id.clone(), weight.clone());
                if is_new {
                    self.emit_wal(GraphEventKind::NodeAdded {
                        entity_id: id.0.clone(),
                        weight_json: serde_json::to_string(&weight).unwrap_or_default(),
                    });
                }
            }
            GraphMutation::AddEdge { src, dst, weight } => {
                match self.storage.insert_edge(&src, &dst, weight.clone()) {
                    Ok(true) => {
                        self.emit_wal(GraphEventKind::EdgeAdded {
                            src_id: src.0.clone(),
                            dst_id: dst.0.clone(),
                            weight_json: serde_json::to_string(&weight).unwrap_or_default(),
                        });
                    }
                    Ok(false) => {} // duplicate — silently skip
                    Err(e) => error!("Edge insert failed: {}", e),
                }
            }
            GraphMutation::RemoveNode { id } => {
                self.storage.node_map.remove(&id);
                self.emit_wal(GraphEventKind::NodeRemoved { entity_id: id.0 });
            }
            GraphMutation::UpdateNode { id, weight } => {
                // Update the node weight in-place. Unlike AddNode, there is no is_new check —
                // UpdateNode always emits the WAL event when the node exists.
                // If the node is missing, update_node_weight logs a WARN and skips.
                if self.storage.node_map.contains_key(&id) {
                    self.storage.update_node_weight(id.clone(), weight.clone());
                    self.emit_wal(GraphEventKind::NodeUpdated {
                        entity_id: id.0.clone(),
                        weight_json: serde_json::to_string(&weight).unwrap_or_default(),
                    });
                } else {
                    // Delegate to update_node_weight for the WARN log (no-op)
                    self.storage.update_node_weight(id, weight);
                }
            }
            GraphMutation::UpsertNode { id, weight } => {
                // Insert-or-merge. This is the ingestion path: a re-ingested node
                // (e.g. a Companies House `active -> dissolved` transition) is folded
                // into the existing weight instead of being dropped by the insert-only
                // AddNode path. The merge/compare is atomic under the node's entry lock.
                match self.storage.upsert_node(id.clone(), weight.clone()) {
                    UpsertOutcome::Inserted => {
                        self.emit_wal(GraphEventKind::NodeAdded {
                            entity_id: id.0.clone(),
                            weight_json: serde_json::to_string(&weight).unwrap_or_default(),
                        });
                    }
                    UpsertOutcome::Updated(merged) => {
                        // Persist the FULL merged weight so replay (which folds
                        // NodeUpdated over NodeAdded by highest timestamp) reconstructs
                        // current state without needing to re-run the merge.
                        self.emit_wal(GraphEventKind::NodeUpdated {
                            entity_id: id.0.clone(),
                            weight_json: serde_json::to_string(&merged).unwrap_or_default(),
                        });
                    }
                    // Identical weight — no state change, no WAL write (idempotent).
                    UpsertOutcome::Unchanged => {}
                }
            }
        }
    }

    fn emit_wal(&self, kind: GraphEventKind) {
        let event = GraphEvent {
            id: uuid::Uuid::new_v4(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            mutation: kind,
        };
        if let Err(e) = self.wal_tx.send(event) {
            error!("WAL channel full or closed, dropping event: {}", e);
        }
    }

    async fn bake_snapshot(&mut self, event_count: u64) {
        let storage = self.storage.clone();
        let snapshot_handle = self.snapshot.clone();
        let detector = self.detector.clone();
        let previous_event = self.last_snapshot_event;
        let wal_tx = self.wal_tx.clone();

        // Minted ONCE per bake, outside spawn_blocking — every MotifDetected event this bake
        // produces carries the SAME run_id (Phase 46, D-05/D-06: one run id per bake, not one
        // per event). `started_at_ms` and `imd_state` are captured here too, before `detector`
        // moves into the closure below, so the post-bake history write can report this run's
        // actual IMD load state (D-07) rather than a default.
        let run_id = uuid::Uuid::new_v4().to_string();
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let imd_run_state = detector.as_deref().map(|d| d.imd_state());
        let run_id_for_closure = run_id.clone();

        // `Some(motif_count)` iff a detector was actually configured and ran this bake;
        // `None` when `detector` was `None`. This distinction (not merely "bake_snapshot was
        // entered") is what drives the detector_runs history write below — see D1's
        // "written on the detector's success path only" requirement (Phase 46 D-05 keeps this
        // posture, only the write target changed from a singleton to a history row).
        let motif_count: Option<usize> = tokio::task::spawn_blocking(move || {
            let snapshot = bake_csr(&storage, event_count);
            let mut motif_count = None;

            if let Some(det) = detector {
                let delta_nodes = snapshot.delta_since(previous_event);
                // We need an id_map: u32 -> String
                // The Csr NodeIndex is the same as the GraphNodeIndex from storage
                let mut id_map = HashMap::new();
                for (id, idx, _, _) in storage.snapshot_nodes() {
                    id_map.insert(idx.index() as u32, id.0.clone());
                }

                let mut events = det.run_tier_b(&snapshot, &delta_nodes, &id_map);
                motif_count = Some(events.len());

                // Stamp this bake's run_id onto every MotifDetected event's result_json BEFORE
                // it hits the WAL — the `serve` WAL writer's first-seen/last-seen merge (46-04
                // Task 3) reads this field back out. Parse-mutate-reserialize, the same
                // technique `withdraw_alert` uses; no DB round-trip needed here since this runs
                // before persistence, not after.
                for event in &mut events {
                    if let GraphEventKind::MotifDetected { result_json } = event {
                        if let Ok(mut value) =
                            serde_json::from_str::<serde_json::Value>(result_json)
                        {
                            value["run_id"] = serde_json::json!(run_id_for_closure);
                            if let Ok(reserialised) = serde_json::to_string(&value) {
                                *result_json = reserialised;
                            }
                        }
                    }
                }

                for event in events {
                    let ev = GraphEvent {
                        id: uuid::Uuid::new_v4(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                        mutation: event,
                    };
                    let _ = wal_tx.send(ev);
                }
            }

            snapshot_handle.store(snapshot);
            motif_count
        })
        .await
        .unwrap_or_else(|e| {
            error!("Snapshot bake panicked: {:?}", e);
            None
        });
        self.last_snapshot_event = event_count;

        // WARN-only, non-fatal posture (deliberate, not an oversight): bake_snapshot runs
        // inside the coordinator's always-on background event loop, and a transient DB write
        // failure here must not crash live motif detection — the history row is a best-effort
        // capability signal for a downstream probe (probe_precomputed_alerts, 32-05), not a
        // correctness-critical write on the hot ingestion path. Contrast with
        // `anomaly_sink.rs`'s fail-loud posture, which guards a correctness-critical write.
        //
        // A bake with no detector configured (`detector: None`, motif_count == None) must
        // leave `detector_runs` completely untouched — no row with `motif_count: 0` as a
        // substitute for "never ran"; those are two different facts D1 requires to stay
        // distinguishable.
        if let Some(count) = motif_count {
            if let Some(db) = self.db.as_deref() {
                let imd_state = imd_run_state.unwrap_or(ImdRunState {
                    loaded: false,
                    csv_path: None,
                    denominator_declared: false,
                });
                let record = crate::persistence::detector_runs::DetectorRunRecord {
                    run_id,
                    started_at_ms,
                    pipeline: "core".to_string(),
                    motif_types: vec![
                        "PhoenixLoop".to_string(),
                        "AddressCluster".to_string(),
                        "NepotismBridge".to_string(),
                    ],
                    motif_count: count as i64,
                    imd_loaded: Some(imd_state.loaded),
                    imd_csv_path: imd_state.csv_path,
                    imd_denominator_declared: Some(imd_state.denominator_declared),
                };
                if let Err(e) =
                    crate::persistence::detector_runs::record_detector_run_history(db, record).await
                {
                    tracing::warn!(
                        error = %e,
                        "GraphCoordinator: failed to persist detector_runs history row (non-fatal)"
                    );
                }
            }
        }
    }
}

/// Cloneable handle for other tasks to interact with the coordinator.
#[derive(Clone)]
pub struct GraphCoordinatorHandle {
    /// Send mutations to the coordinator
    pub mutation_tx: mpsc::Sender<GraphMutation>,
    /// Trigger a manual checkpoint / CSR bake
    pub checkpoint_tx: mpsc::Sender<()>,
    /// Direct read access to the operational DashMap index
    pub storage: Arc<GraphStorage>,
    /// Read access to the current immutable CSR snapshot
    pub snapshot: Arc<SnapshotHandle>,
    /// Optional SurrealDB connection — present when engine is started via `serve` command.
    /// None in unit-test contexts that don't spin up a live DB.
    pub db: Option<Arc<Surreal<Any>>>,
}

impl GraphCoordinatorHandle {
    /// Access the SurrealDB connection. Panics if the engine was started without a DB
    /// (e.g. in tests using the in-memory path). Call only from API / CLI query paths.
    pub fn db(&self) -> &Arc<Surreal<Any>> {
        self.db
            .as_ref()
            .expect("GraphCoordinatorHandle: db not initialised — start engine with new_with_db()")
    }
}
