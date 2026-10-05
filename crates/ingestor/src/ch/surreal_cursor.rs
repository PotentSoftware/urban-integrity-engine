use std::sync::Arc;
use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::stream::SurrealStreamCursor;

pub use urban_integrity_core::stream::CursorStore;

/// SurrealDB-backed timepoint cursor for the CH ingest stream.
///
/// Wraps `SurrealStreamCursor` from `crates/core` with the fixed record ID
/// `ch_cursor:ch_stream` so existing SurrealDB records are preserved without
/// any data migration.
pub struct SurrealCursorStore(SurrealStreamCursor);

impl SurrealCursorStore {
    pub fn new(db: Arc<Surreal<Any>>) -> Self {
        Self(SurrealStreamCursor::new(db, "ch_cursor", "ch_stream"))
    }

    /// Cursor for the officer-appointment stream.
    ///
    /// A **separate** record from the company stream's. CH issues timepoints per stream, so
    /// sharing one row would have each connection resume from the other's position — the two
    /// would leapfrog and both would skip events, silently.
    pub fn new_officers(db: Arc<Surreal<Any>>) -> Self {
        Self(SurrealStreamCursor::new(db, "ch_cursor", "ch_officers"))
    }
}

#[async_trait::async_trait]
impl CursorStore for SurrealCursorStore {
    async fn load_timepoint(&self) -> Option<i64> {
        self.0.load_timepoint().await
    }
    async fn persist_timepoint(&self, tp: i64) {
        self.0.persist_timepoint(tp).await
    }
    async fn clear_timepoint(&self) {
        self.0.clear_timepoint().await
    }
}
