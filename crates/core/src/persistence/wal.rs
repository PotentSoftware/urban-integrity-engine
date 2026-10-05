use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};
use tracing::debug;

use crate::graph::types::GraphEvent;

/// Create a bounded WAL channel pair with the given capacity.
pub fn wal_channel(capacity: usize) -> (Sender<GraphEvent>, Receiver<GraphEvent>) {
    bounded(capacity)
}

/// Drain a batch from the WAL receiver.
///
/// Blocks up to `window_ms` for the first event to arrive, then non-blocking
/// drains up to `max_batch` events within the remaining window.
///
/// Returns an empty Vec if no events arrive within the window.
pub fn drain_batch(rx: &Receiver<GraphEvent>, max_batch: usize, window_ms: u64) -> Vec<GraphEvent> {
    let mut batch = Vec::with_capacity(max_batch);
    let timeout = Duration::from_millis(window_ms);

    // Block for the first event (avoids busy-loop on idle channel)
    match rx.recv_timeout(timeout) {
        Ok(event) => batch.push(event),
        Err(_) => return batch, // timeout — no events
    }

    // Non-blocking drain up to max_batch within remaining window
    let deadline = Instant::now() + Duration::from_millis(window_ms / 4);
    while batch.len() < max_batch && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(e) => batch.push(e),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }

    debug!(count = batch.len(), "WAL batch drained");
    batch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{GraphEvent, GraphEventKind};

    fn make_event(entity_id: &str) -> GraphEvent {
        GraphEvent {
            id: uuid::Uuid::new_v4(),
            timestamp: 0,
            mutation: GraphEventKind::NodeAdded {
                entity_id: entity_id.to_string(),
                weight_json: "{}".to_string(),
            },
        }
    }

    #[test]
    fn test_drain_batch_disconnected_returns_empty() {
        let (tx, rx) = wal_channel(10);
        drop(tx);
        let batch = drain_batch(&rx, 10, 50);
        assert!(
            batch.is_empty(),
            "Disconnected channel should yield empty batch"
        );
    }

    #[test]
    fn test_drain_batch_idle_channel_timeout() {
        let (_tx, rx) = wal_channel(10);
        // tx kept alive but nothing sent — should timeout after window_ms
        let batch = drain_batch(&rx, 10, 30);
        assert!(
            batch.is_empty(),
            "Idle channel should yield empty batch after timeout"
        );
    }

    #[test]
    fn test_drain_batch_single_event() {
        let (tx, rx) = wal_channel(10);
        tx.send(make_event("company-001")).expect("send");
        let batch = drain_batch(&rx, 10, 200);
        assert_eq!(batch.len(), 1, "Should drain exactly 1 event");
        if let GraphEventKind::NodeAdded { entity_id, .. } = &batch[0].mutation {
            assert_eq!(entity_id, "company-001");
        } else {
            panic!("Unexpected event kind");
        }
    }

    #[test]
    fn test_drain_batch_respects_max_batch() {
        let (tx, rx) = wal_channel(20);
        for i in 0..10 {
            tx.send(make_event(&format!("co-{:03}", i))).expect("send");
        }
        // max_batch=4 — should never return more than 4
        let batch = drain_batch(&rx, 4, 200);
        assert!(
            batch.len() <= 4,
            "Batch must not exceed max_batch (got {})",
            batch.len()
        );
        assert!(
            !batch.is_empty(),
            "Should drain at least 1 event when items available"
        );
    }
}
