use std::sync::Arc;
use surrealdb::{engine::any::Any, Surreal};

/// Cursor store abstraction — pluggable so tests can use in-memory storage.
///
/// Both the ingestor's `SurrealCursorStore` and the enricher's per-stream
/// cursors implement this trait, allowing `run_ch_stream` to be generic over
/// the backing store.
#[async_trait::async_trait]
pub trait CursorStore: Send + Sync {
    async fn load_timepoint(&self) -> Option<i64>;
    async fn persist_timepoint(&self, tp: i64);
    async fn clear_timepoint(&self);
}

/// In-memory cursor store for testing.
pub struct InMemoryCursorStore {
    tp: tokio::sync::Mutex<Option<i64>>,
}

impl Default for InMemoryCursorStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryCursorStore {
    pub fn new() -> Self {
        Self {
            tp: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl CursorStore for InMemoryCursorStore {
    async fn load_timepoint(&self) -> Option<i64> {
        *self.tp.lock().await
    }
    async fn persist_timepoint(&self, tp: i64) {
        *self.tp.lock().await = Some(tp);
    }
    async fn clear_timepoint(&self) {
        *self.tp.lock().await = None;
    }
}

/// SurrealDB-backed timepoint cursor parameterised by stream name and table.
///
/// The record ID is constructed as `{table}:{stream_name}`, e.g.:
/// - `ch_cursor:ch_stream` (ingestor — backward-compatible)
/// - `enricher_cursors:ch_psc` (enricher PSC adapter)
/// - `enricher_cursors:ch_disqualified` (enricher Disqualified Directors adapter)
pub struct SurrealStreamCursor {
    db: Arc<Surreal<Any>>,
    record_id: String,
}

impl SurrealStreamCursor {
    /// Create a new cursor for the given table + stream name.
    ///
    /// `table` — SurrealDB table name (e.g. `"ch_cursor"`, `"enricher_cursors"`)
    /// `stream_name` — record identifier within the table (e.g. `"ch_stream"`, `"ch_psc"`)
    pub fn new(db: Arc<Surreal<Any>>, table: &str, stream_name: &str) -> Self {
        Self {
            db,
            record_id: format!("{table}:{stream_name}"),
        }
    }
}

#[async_trait::async_trait]
impl CursorStore for SurrealStreamCursor {
    /// Load the persisted CH stream timepoint, or `None` if not yet set.
    async fn load_timepoint(&self) -> Option<i64> {
        let id = &self.record_id;
        let result: surrealdb::Result<Vec<serde_json::Value>> = self
            .db
            .query(format!("SELECT timepoint FROM {id}"))
            .await
            .and_then(|mut r| r.take(0));

        result
            .ok()
            .and_then(|rows| rows.into_iter().next())
            .and_then(|v| v["timepoint"].as_i64())
    }

    /// Persist the current timepoint to SurrealDB using UPSERT SurrealQL.
    /// Failures are logged but not propagated — streaming must continue
    /// even if the cursor persist call fails.
    async fn persist_timepoint(&self, tp: i64) {
        let id = &self.record_id;
        let result: surrealdb::Result<Vec<serde_json::Value>> = self
            .db
            .query(format!("UPSERT {id} SET timepoint = $tp"))
            .bind(("tp", tp))
            .await
            .and_then(|mut r| r.take(0));

        if let Err(e) = result {
            tracing::warn!(
                error = %e,
                timepoint = tp,
                record = %id,
                "SurrealStreamCursor: failed to persist timepoint (non-fatal)"
            );
        }
    }

    /// Clear the cursor (called when CH returns 416 Range Not Satisfiable —
    /// timepoint too old). Next reconnect will replay from the stream head.
    async fn clear_timepoint(&self) {
        let id = &self.record_id;
        let result: surrealdb::Result<Vec<serde_json::Value>> = self
            .db
            .query(format!("DELETE {id}"))
            .await
            .and_then(|mut r| r.take(0));

        if let Err(e) = result {
            tracing::warn!(
                error = %e,
                record = %id,
                "SurrealStreamCursor: failed to clear timepoint (non-fatal)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_inmemory_cursor_default_is_none() {
        let store = InMemoryCursorStore::default();
        assert!(store.load_timepoint().await.is_none());
    }

    #[tokio::test]
    async fn test_inmemory_cursor_persist_and_load() {
        let store = InMemoryCursorStore::new();
        store.persist_timepoint(42).await;
        assert_eq!(store.load_timepoint().await, Some(42));
    }

    #[tokio::test]
    async fn test_inmemory_cursor_clear_resets_to_none() {
        let store = InMemoryCursorStore::new();
        store.persist_timepoint(100).await;
        store.clear_timepoint().await;
        assert!(store.load_timepoint().await.is_none());
    }

    #[tokio::test]
    async fn test_inmemory_cursor_overwrite_persists_latest() {
        let store = InMemoryCursorStore::new();
        store.persist_timepoint(1).await;
        store.persist_timepoint(2).await;
        assert_eq!(store.load_timepoint().await, Some(2));
    }
}
