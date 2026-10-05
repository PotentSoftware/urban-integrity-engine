use crate::graphql::schema::AlertObject;
use async_graphql::dataloader::Loader;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::get_alerts_by_ids;

/// DataLoader for batching AlertObject fetches — prevents N+1 queries
/// when resolving the `alert(id)` field across multiple parent objects.
pub struct AlertLoader {
    db: Arc<Surreal<Any>>,
}

impl AlertLoader {
    pub fn new(db: Arc<Surreal<Any>>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Loader<String> for AlertLoader {
    type Value = AlertObject;
    type Error = Arc<anyhow::Error>;

    /// Batch-fetch alerts by ID list from SurrealDB.
    async fn load(&self, ids: &[String]) -> Result<HashMap<String, Self::Value>, Self::Error> {
        let records = get_alerts_by_ids(&self.db, ids)
            .await
            .map_err(|e| Arc::new(anyhow::anyhow!("AlertLoader DB error: {}", e)))?;

        // The API is a publishing surface. A retracted alert must never leave it — the same
        // rule the `alerts` list resolver enforces with `include_withdrawn: false`.
        //
        // Until 2026-08-20 this path had no such filter, and `AlertObject` carries no
        // withdrawal field, so `alert(id:)` served a retracted accusation looking exactly
        // like a live finding (T-23-FalseAccusation). Withdrawn alerts stay readable on the
        // operator's own surface — `alerts show`, `alerts list --include-withdrawn` — because
        // a retracted false positive is evidence about detector quality.
        //
        // Filtered here rather than in `get_alerts_by_ids`: that is a neutral core fetch, and
        // the CLI's by-id reader must still be able to show a withdrawn alert.
        Ok(records
            .into_iter()
            .filter(|(_, r)| r.withdrawn_at.is_none())
            .map(|(id, r)| {
                (
                    id,
                    AlertObject {
                        id: r.id,
                        motif_type: r.motif_type,
                        score: r.score,
                        result_json: r.result_json,
                        detected_at: r.detected_at,
                        ward_code: r.ward_code,
                    },
                )
            })
            .collect())
    }
}
