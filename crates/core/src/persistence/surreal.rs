use crossbeam_channel::Receiver;
/// SurrealDB batch writer task.
///
/// Drains the WAL channel in batches and writes events to SurrealDB.
/// On write failure, logs the event to the dead letter log and continues.
///
/// Note: SurrealDB connection is managed by the caller (passed in via AppState).
/// This module defines the task function and configuration — wiring happens in main.rs.
use std::path::PathBuf;
use std::time::Instant;
use tracing::{error, info, warn};

use super::dead_letter::log_dead_letter;
use super::wal::drain_batch;
use crate::graph::types::{GraphEvent, GraphEventKind};

/// Configuration for the SurrealDB writer background task.
#[derive(Debug, Clone)]
pub struct SurrealWriterConfig {
    pub min_batch_size: usize,
    pub max_batch_size: usize,
    pub target_latency_ms: u64,
    pub dead_letter_path: PathBuf,
}

impl Default for SurrealWriterConfig {
    fn default() -> Self {
        Self {
            min_batch_size: 10,
            max_batch_size: 5000,
            target_latency_ms: 50,
            dead_letter_path: PathBuf::from("data/dead_letters.jsonl"),
        }
    }
}

/// Dynamic batch size controller using AIMD-style logic.
pub struct BatchController {
    current_size: usize,
    min_size: usize,
    max_size: usize,
    target_ms: u64,
}

impl BatchController {
    pub fn new(min: usize, max: usize, target: u64) -> Self {
        Self {
            current_size: 100, // Start with default
            min_size: min,
            max_size: max,
            target_ms: target,
        }
    }

    pub fn current_size(&self) -> usize {
        self.current_size
    }

    pub fn adjust(&mut self, last_latency_ms: u64) {
        if last_latency_ms < self.target_ms {
            // Latency is low: decrease batch size to be more aggressive (more frequent writes)
            self.current_size = (self.current_size as f64 * 0.9) as usize;
        } else {
            // Latency is high: increase batch size to reduce IOPS pressure
            self.current_size = (self.current_size as f64 * 1.25) as usize;
        }
        self.current_size = self.current_size.clamp(self.min_size, self.max_size);
    }
}

/// Serialisable event record for SurrealDB persistence.
/// Avoids tight coupling to surrealdb client generics at the task level.
#[derive(Debug, serde::Serialize)]
pub struct EventRecord {
    pub event_id: String,
    pub timestamp: u64,
    pub kind: String,
    pub payload: serde_json::Value,
}

impl From<&GraphEvent> for EventRecord {
    fn from(e: &GraphEvent) -> Self {
        let (kind, payload) = match &e.mutation {
            GraphEventKind::NodeAdded {
                entity_id,
                weight_json,
            } => (
                "NodeAdded",
                serde_json::json!({
                    "entity_id": entity_id,
                    "weight_json": weight_json,
                }),
            ),
            GraphEventKind::EdgeAdded {
                src_id,
                dst_id,
                weight_json,
            } => (
                "EdgeAdded",
                serde_json::json!({
                    "src_id": src_id,
                    "dst_id": dst_id,
                    "weight_json": weight_json,
                }),
            ),
            GraphEventKind::NodeRemoved { entity_id } => {
                ("NodeRemoved", serde_json::json!({ "entity_id": entity_id }))
            }
            GraphEventKind::MotifDetected { result_json } => (
                "MotifDetected",
                serde_json::json!({ "result_json": result_json }),
            ),
            GraphEventKind::NodeUpdated {
                entity_id,
                weight_json,
            } => (
                "NodeUpdated",
                serde_json::json!({
                    "entity_id": entity_id,
                    "weight_json": weight_json,
                }),
            ),
        };
        Self {
            event_id: e.id.to_string(),
            timestamp: e.timestamp,
            kind: kind.to_string(),
            payload,
        }
    }
}

/// Drain-and-write loop. Runs in a dedicated blocking thread or Tokio task.
///
/// This function is intentionally free of a concrete surrealdb::Surreal<T> parameter
/// so it can be tested without a live SurrealDB instance. The `write_fn` callback
/// receives a serialised `EventRecord` and should perform the actual DB write.
///
/// In production wire this via:
/// ```ignore
/// let db = db.clone();
/// tokio::spawn(surreal_writer_loop(rx, config, move |record| {
///     let db = db.clone();
///     async move { db.create("graph_events").content(record).await.map(|_: Vec<serde_json::Value>| ()).map_err(|e| e.to_string()) }
/// }));
/// ```
pub async fn surreal_writer_loop<F, Fut>(
    rx: Receiver<GraphEvent>,
    config: SurrealWriterConfig,
    write_fn: F,
) where
    F: Fn(EventRecord) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send,
{
    info!("SurrealDB writer task started (elastic batching enabled)");
    let mut controller = BatchController::new(
        config.min_batch_size,
        config.max_batch_size,
        config.target_latency_ms,
    );

    loop {
        let batch_size = controller.current_size();
        let batch = {
            let rx = rx.clone();
            // Window is fixed at 100ms for now, batch size is dynamic
            tokio::task::spawn_blocking(move || drain_batch(&rx, batch_size, 100))
                .await
                .unwrap_or_default()
        };

        if batch.is_empty() {
            match rx.try_recv() {
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    info!("WAL channel closed, SurrealDB writer shutting down");
                    break;
                }
                Ok(event) => {
                    let record = EventRecord::from(&event);
                    let start = Instant::now();
                    if let Err(e) = write_fn(record).await {
                        error!(event_id = %event.id, "SurrealDB write failed (probe): {}", e);
                        log_dead_letter(&config.dead_letter_path, &event, &e);
                    } else {
                        controller.adjust(start.elapsed().as_millis() as u64);
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
            continue;
        }

        info!(
            count = batch.len(),
            current_batch_size = batch_size,
            "Writing WAL batch to SurrealDB"
        );
        let start = Instant::now();
        let mut success = true;
        for event in &batch {
            let record = EventRecord::from(event);
            if let Err(e) = write_fn(record).await {
                error!(
                    event_id = %event.id,
                    error = %e,
                    "SurrealDB write failed — logging to dead letter"
                );
                log_dead_letter(&config.dead_letter_path, event, &e);
                success = false;
            }
        }

        if success {
            controller.adjust(start.elapsed().as_millis() as u64);
        }
    }
    warn!("SurrealDB writer task exited");
}

/// Replay all `NodeAdded` and `EdgeAdded` events from SurrealDB into `GraphStorage`.
///
/// Called once at `serve` startup so the in-memory coordinator graph includes all
/// data written by `ingest-planning`, `ch_backfill`, and prior `serve` sessions.
/// Inserts are direct (no WAL emission) so they don't round-trip back to SurrealDB.
///
/// Page size for replay reads. Small enough that any single response stays well within
/// the SDK's transport limits, large enough that even a multi-hundred-thousand-row graph
/// loads in a handful of round-trips.
const REPLAY_PAGE_SIZE: usize = 20_000;

/// Fetch every `graph_events` row of one `kind` in `page_size` chunks, using `ORDER BY id`
/// with `LIMIT`/`START` pagination. Returns all rows; never truncates. `fields` must
/// include `id` (the ordering key) — SurrealDB 3.x rejects `ORDER BY` on unselected fields.
pub async fn fetch_events_paged(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    kind: &str,
    fields: &str,
    page_size: usize,
) -> anyhow::Result<Vec<serde_json::Value>> {
    use anyhow::Context as _;
    let mut all: Vec<serde_json::Value> = Vec::new();
    let mut start = 0usize;
    loop {
        let sql = format!(
            "SELECT {fields} FROM graph_events WHERE kind = '{kind}' ORDER BY id LIMIT {page_size} START {start}"
        );
        let page: Vec<serde_json::Value> = db
            .query(sql)
            .await
            .with_context(|| format!("paged replay query failed (kind={kind}, start={start})"))?
            .take(0)?;
        let n = page.len();
        all.extend(page);
        if n < page_size {
            break;
        }
        start += page_size;
    }
    Ok(all)
}

/// Count all `graph_events` rows. Used by the startup path to tell an empty database
/// (an empty graph is legitimate) from a replay failure against a populated one (fatal —
/// must not silently degrade to an empty graph).
pub async fn count_graph_events(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
) -> anyhow::Result<usize> {
    use anyhow::Context as _;
    // Take into a JSON value and read the number out — `GROUP ALL` yields `{ "count": N }`
    // and the SDK won't coerce that object straight into an integer.
    let rows: Vec<serde_json::Value> = db
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await
        .context("count_graph_events query failed")?
        .take(0)?;
    // Same fail-loud posture as oracle::extract_count (WR-01): no row at all is a genuinely
    // empty table; a row WITHOUT an integer `count` is a response-shape change and must error —
    // this function's entire purpose is telling "empty" from "failed", so returning 0 on a
    // malformed response would defeat it at the one moment it matters.
    let n = match rows.first() {
        None => 0i64,
        Some(r) => r["count"].as_i64().ok_or_else(|| {
            anyhow::anyhow!(
                "count_graph_events: response row carried no integer `count` field \
                 (SurrealDB response shape changed?): {r}"
            )
        })?,
    };
    Ok(n.max(0) as usize)
}

/// Returns the number of events replayed.
pub async fn replay_from_surreal(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    storage: &crate::graph::storage::GraphStorage,
) -> anyhow::Result<usize> {
    use anyhow::Context as _;
    use tracing::warn;
    use urban_integrity_schema::types::id::EntityId;

    // Load each event kind in bounded pages. A single unbounded `SELECT ... FROM
    // graph_events WHERE kind = ...` returns the entire kind in one response; on a
    // populated graph (hundreds of thousands of NodeAdded rows, tens of MB) the SDK's
    // WebSocket transport fails on that response, `?` propagates, and the caller used to
    // degrade to an empty graph — silently disabling all historical detection. Paginating
    // keeps every response small. `id` is selected because pagination needs a stable
    // `ORDER BY`, and SurrealDB 3.x rejects `ORDER BY` on a field absent from `SELECT`.
    let mut node_rows = fetch_events_paged(
        db,
        "NodeAdded",
        "id, kind, payload, timestamp",
        REPLAY_PAGE_SIZE,
    )
    .await
    .context("replay_from_surreal node query failed")?;

    // Fold NodeUpdated events (status transitions such as `active -> dissolved`,
    // written by the coordinator's UpsertNode path) over the NodeAdded births. Each
    // NodeUpdated payload carries the FULL merged weight at update time and a later
    // timestamp, so the highest-timestamp-per-entity dedup below reconstructs current
    // state with no merge needed here. Queried separately (not `kind IN [..]`) to keep
    // each predicate a simple equality — the SurrealDB 3.x planner mishandles some
    // compound `kind` predicates elsewhere in this codebase.
    let update_rows = fetch_events_paged(
        db,
        "NodeUpdated",
        "id, kind, payload, timestamp",
        REPLAY_PAGE_SIZE,
    )
    .await
    .context("replay_from_surreal node-update query failed")?;
    node_rows.extend(update_rows);

    let edge_rows = fetch_events_paged(db, "EdgeAdded", "id, payload", REPLAY_PAGE_SIZE)
        .await
        .context("replay_from_surreal edge query failed")?;

    // One fold, shared with the projector and the CLI/GraphQL read path, so all three agree on
    // the equal-timestamp case. The hand-rolled dedup here used strict `>` and loaded NodeAdded
    // first, so a tie kept the stale BIRTH while `fold_latest_node_rows` kept the correction —
    // the hot graph and the projected tables then disagreed with `entity --ch-number`.
    let node_rows = crate::persistence::queries::fold_latest_node_rows(node_rows);

    let total = node_rows.len() + edge_rows.len();
    let mut replayed = 0usize;

    for row in &node_rows {
        let id = match row["payload"]["entity_id"].as_str() {
            Some(s) => EntityId(s.to_string()),
            None => continue,
        };
        let weight_json = match row["payload"]["weight_json"].as_str() {
            Some(s) => s,
            None => continue,
        };
        match serde_json::from_str(weight_json) {
            Ok(weight) => {
                storage.insert_node(id, weight);
                replayed += 1;
            }
            Err(e) => {
                warn!(entity_id = %row["payload"]["entity_id"], "replay: bad node weight: {e}")
            }
        }
    }

    for row in &edge_rows {
        let src = match row["payload"]["src_id"].as_str() {
            Some(s) => EntityId(s.to_string()),
            None => continue,
        };
        let dst = match row["payload"]["dst_id"].as_str() {
            Some(s) => EntityId(s.to_string()),
            None => continue,
        };
        let weight_json = match row["payload"]["weight_json"].as_str() {
            Some(s) => s,
            None => continue,
        };
        match serde_json::from_str(weight_json) {
            Ok(weight) => {
                if let Err(e) = storage.insert_edge(&src, &dst, weight) {
                    warn!(src = %src.0, dst = %dst.0, "replay: edge insert failed: {e}");
                } else {
                    replayed += 1;
                }
            }
            Err(e) => warn!(src = %src.0, dst = %dst.0, "replay: bad edge weight: {e}"),
        }
    }

    info!(
        total,
        replayed, "Graph replayed from SurrealDB graph_events"
    );
    Ok(replayed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{GraphEvent, GraphEventKind};

    // ── BatchController (AIMD) ────────────────────────────────────────────────

    #[test]
    fn test_batch_controller_initial_size() {
        let bc = BatchController::new(10, 5000, 50);
        assert_eq!(bc.current_size(), 100);
    }

    #[test]
    fn test_batch_controller_increases_on_high_latency() {
        let mut bc = BatchController::new(10, 5000, 50);
        let before = bc.current_size();
        bc.adjust(200); // 200ms > 50ms target → should increase
        assert!(
            bc.current_size() > before,
            "High latency should increase batch size"
        );
    }

    #[test]
    fn test_batch_controller_decreases_on_low_latency() {
        let mut bc = BatchController::new(10, 5000, 50);
        let before = bc.current_size();
        bc.adjust(10); // 10ms < 50ms target → should decrease
        assert!(
            bc.current_size() < before,
            "Low latency should decrease batch size"
        );
    }

    #[test]
    fn test_batch_controller_clamps_to_min() {
        // Start small and repeatedly decrease — should never go below min_size
        let mut bc = BatchController::new(10, 5000, 50);
        for _ in 0..100 {
            bc.adjust(1); // very low latency → keeps decreasing
        }
        assert_eq!(
            bc.current_size(),
            10,
            "Batch size should clamp at min_size=10"
        );
    }

    #[test]
    fn test_batch_controller_clamps_to_max() {
        // Start large and repeatedly increase — should never exceed max_size
        let mut bc = BatchController::new(10, 5000, 50);
        for _ in 0..100 {
            bc.adjust(9999); // very high latency → keeps increasing
        }
        assert_eq!(
            bc.current_size(),
            5000,
            "Batch size should clamp at max_size=5000"
        );
    }

    #[test]
    fn test_batch_controller_at_target_latency_increases() {
        // Exactly at target_ms (50) is not < target, so it follows the high-latency branch
        let mut bc = BatchController::new(10, 5000, 50);
        let before = bc.current_size();
        bc.adjust(50);
        assert!(bc.current_size() >= before);
    }

    #[test]
    fn test_surreal_writer_config_defaults() {
        let cfg = SurrealWriterConfig::default();
        assert_eq!(cfg.min_batch_size, 10);
        assert_eq!(cfg.max_batch_size, 5000);
        assert_eq!(cfg.target_latency_ms, 50);
        assert_eq!(
            cfg.dead_letter_path.to_str().unwrap(),
            "data/dead_letters.jsonl"
        );
    }

    // ── EventRecord serialisation ─────────────────────────────────────────────

    fn make_event(kind: GraphEventKind) -> GraphEvent {
        GraphEvent {
            id: uuid::Uuid::new_v4(),
            timestamp: 1_700_000_000,
            mutation: kind,
        }
    }

    #[test]
    fn test_event_record_from_node_added() {
        let event = make_event(GraphEventKind::NodeAdded {
            entity_id: "GB-COH:123".into(),
            weight_json: r#"{"type":"Entity","name":"Test Co"}"#.into(),
        });
        let record = EventRecord::from(&event);
        assert_eq!(record.kind, "NodeAdded");
        assert_eq!(record.timestamp, 1_700_000_000);
        assert_eq!(record.payload["entity_id"], "GB-COH:123");
        assert!(!record.event_id.is_empty());
    }

    #[test]
    fn test_event_record_from_edge_added() {
        let event = make_event(GraphEventKind::EdgeAdded {
            src_id: "GB-COH:001".into(),
            dst_id: "GB-COH:002".into(),
            weight_json: r#"{"type":"Directs"}"#.into(),
        });
        let record = EventRecord::from(&event);
        assert_eq!(record.kind, "EdgeAdded");
        assert_eq!(record.payload["src_id"], "GB-COH:001");
        assert_eq!(record.payload["dst_id"], "GB-COH:002");
    }

    #[test]
    fn test_event_record_from_node_removed() {
        let event = make_event(GraphEventKind::NodeRemoved {
            entity_id: "GB-COH:999".into(),
        });
        let record = EventRecord::from(&event);
        assert_eq!(record.kind, "NodeRemoved");
        assert_eq!(record.payload["entity_id"], "GB-COH:999");
    }

    #[test]
    fn test_event_record_from_motif_detected() {
        let event = make_event(GraphEventKind::MotifDetected {
            result_json: r#"{"motif_type":"PhoenixLoop","risk_score":0.85}"#.into(),
        });
        let record = EventRecord::from(&event);
        assert_eq!(record.kind, "MotifDetected");
        assert!(record.payload["result_json"]
            .as_str()
            .unwrap()
            .contains("PhoenixLoop"));
    }

    #[test]
    fn test_event_record_is_serializable() {
        let event = make_event(GraphEventKind::NodeAdded {
            entity_id: "test".into(),
            weight_json: "{}".into(),
        });
        let record = EventRecord::from(&event);
        let json = serde_json::to_string(&record);
        assert!(
            json.is_ok(),
            "EventRecord must serialize to JSON without errors"
        );
        assert!(json.unwrap().contains("NodeAdded"));
    }
}
