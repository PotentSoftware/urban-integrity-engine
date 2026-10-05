use crate::graphql::loaders::AlertLoader;
use async_graphql::dataloader::DataLoader;
use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use std::sync::Arc;
use urban_integrity_core::{
    bfs_traversal, graph::coordinator::GraphCoordinatorHandle, query_alerts,
    query_enrichment_status, query_entity_by_ch_number, query_entity_by_name, resolve_entity,
    AlertFilter, EnrichmentStatusResult, EntityProfile, PathStep, MAX_HOPS,
};

pub type AppSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

/// A single flagged motif alert from the detection engine.
#[derive(SimpleObject, Clone)]
pub struct AlertObject {
    pub id: String,
    pub motif_type: String,
    pub score: f64,
    pub result_json: String,
    pub detected_at: String,
    pub ward_code: Option<String>,
}

/// Live graph engine statistics.
#[derive(SimpleObject)]
pub struct GraphStatsObject {
    pub node_count: u64,
    pub edge_count: u64,
    pub last_snapshot_event: u64,
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// List alerts filtered by minimum score, ward, and motif type.
    async fn alerts(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 0.5)] min_score: f64,
        ward_code: Option<String>,
        motif_type: Option<String>,
        #[graphql(default = 50)] limit: i32,
    ) -> async_graphql::Result<Vec<AlertObject>> {
        let handle = ctx.data_unchecked::<Arc<GraphCoordinatorHandle>>();
        let Some(db) = handle.db.as_ref() else {
            // No DB connection (e.g. in tests) — return empty list
            return Ok(vec![]);
        };
        let filter = AlertFilter {
            min_score,
            ward_code,
            motif_type,
            from_date: None,
            to_date: None,
            limit: limit.max(1) as usize,
            // The API is a publishing surface. A retracted alert must never leave it.
            include_withdrawn: false,
            ..Default::default()
        };
        let records = query_alerts(db, &filter)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        Ok(records
            .alerts
            .into_iter()
            .map(|r| AlertObject {
                id: r.id,
                motif_type: r.motif_type,
                score: r.score,
                result_json: r.result_json,
                detected_at: r.detected_at,
                ward_code: r.ward_code,
            })
            .collect())
    }

    /// Get a single alert by ID using DataLoader (prevents N+1 queries).
    async fn alert(
        &self,
        ctx: &Context<'_>,
        id: String,
    ) -> async_graphql::Result<Option<AlertObject>> {
        // DataLoader is only registered when a live DB is present (see build_schema).
        // In test contexts (db = None), return null gracefully.
        let Some(loader) = ctx.data_opt::<DataLoader<AlertLoader>>() else {
            return Ok(None);
        };
        loader
            .load_one(id)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Live graph engine statistics from the current CSR snapshot.
    async fn graph_stats(&self, ctx: &Context<'_>) -> async_graphql::Result<GraphStatsObject> {
        let handle = ctx.data_unchecked::<Arc<GraphCoordinatorHandle>>();
        let snapshot = handle.snapshot.load();
        Ok(GraphStatsObject {
            node_count: snapshot.graph.node_count() as u64,
            edge_count: snapshot.graph.edge_count() as u64,
            last_snapshot_event: snapshot.baked_at_event,
        })
    }

    /// Look up an entity by name or Companies House number.
    /// Returns null (not an error) when no entity is found or when DB is absent (test contexts).
    /// D-09: calls query functions directly — no DataLoader for top-level lookups.
    async fn entity(
        &self,
        ctx: &Context<'_>,
        name: Option<String>,
        #[graphql(name = "chNumber")] ch_number: Option<String>,
    ) -> async_graphql::Result<Option<EntityProfile>> {
        let handle = ctx.data_unchecked::<Arc<GraphCoordinatorHandle>>();
        let Some(db) = handle.db.as_ref() else {
            // No DB connection (e.g. in tests) — return null gracefully
            return Ok(None);
        };
        match (name, ch_number) {
            (Some(n), _) => {
                let results = query_entity_by_name(db, &n)
                    .await
                    .map_err(|e| async_graphql::Error::new(e.to_string()))?;
                Ok(results.into_iter().next())
            }
            (None, Some(ch)) => query_entity_by_ch_number(db, &ch)
                .await
                .map_err(|e| async_graphql::Error::new(e.to_string())),
            (None, None) => Ok(None),
        }
    }

    /// Return the shortest path between two entities (by name or slug).
    /// Returns empty list (not an error) when DB is absent (test contexts) or no path found.
    /// D-05/D-06: calls shared bfs_traversal() from crates/core/src/persistence/traversal.rs.
    /// D-14: disambiguation errors surface as GraphQL errors listing candidate slugs.
    async fn graph_path(
        &self,
        ctx: &Context<'_>,
        from: String,
        to: String,
    ) -> async_graphql::Result<Vec<PathStep>> {
        let handle = ctx.data_unchecked::<Arc<GraphCoordinatorHandle>>();
        let Some(db) = handle.db.as_ref() else {
            // No DB connection (e.g. in tests) — return empty list gracefully
            return Ok(vec![]);
        };

        // Resolve source entity
        let src_ids = resolve_entity(db, &from)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        if src_ids.is_empty() {
            return Err(async_graphql::Error::new(format!(
                "Entity not found: '{from}'"
            )));
        }
        if src_ids.len() > 1 {
            return Err(async_graphql::Error::new(format!(
                "Ambiguous: {:?} — use a slug",
                src_ids
            )));
        }

        // Resolve destination entity
        let dst_ids = resolve_entity(db, &to)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;
        if dst_ids.is_empty() {
            return Err(async_graphql::Error::new(format!(
                "Entity not found: '{to}'"
            )));
        }
        if dst_ids.len() > 1 {
            return Err(async_graphql::Error::new(format!(
                "Ambiguous: {:?} — use a slug",
                dst_ids
            )));
        }

        // Same entity short-circuit
        if src_ids[0] == dst_ids[0] {
            return Ok(vec![]);
        }

        bfs_traversal(db, &src_ids[0], &dst_ids[0], MAX_HOPS)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }

    /// Returns identity-link enrichment statistics for a company number (SURF-05).
    ///
    /// Returns None when the DB handle is not available (test context) or when
    /// no identity_links exist for the given company number.
    /// Input is validated via is_valid_ch_number — invalid inputs return None (never error).
    async fn enrichment_status(
        &self,
        ctx: &Context<'_>,
        #[graphql(name = "companyNumber")] company_number: String,
    ) -> async_graphql::Result<Option<EnrichmentStatusResult>> {
        let handle = ctx.data_unchecked::<Arc<GraphCoordinatorHandle>>();
        let Some(db) = handle.db.as_ref() else {
            // No DB connection (e.g. in tests) — return null gracefully
            return Ok(None);
        };
        query_enrichment_status(&company_number, db)
            .await
            .map_err(|e| async_graphql::Error::new(e.to_string()))
    }
}

/// Build the async-graphql schema with DataLoader and GraphCoordinatorHandle injected.
/// When the handle has no DB (e.g. in tests), alert resolvers return empty lists
/// rather than panicking.
pub fn build_schema(handle: Arc<GraphCoordinatorHandle>) -> AppSchema {
    let mut builder = Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
        .data(handle.clone())
        // Security: prevent deeply nested query attacks (ASVS L1 — API protection)
        .limit_depth(10)
        // Security: prevent expensive query attacks
        .limit_complexity(1000);

    // Only inject the DataLoaders when a DB connection is available.
    // In test contexts (db = None) the resolvers return [] / null gracefully.
    if let Some(db) = handle.db.as_ref() {
        // Only `alert(id:)` loads through a DataLoader. There is deliberately no entity
        // loader: `EntityProfile` is a SimpleObject materialised by a single query, and the
        // schema has no per-parent entity resolver, so there is no N+1 for batching to
        // remove. A loader here would add indirection that never batches anything. If a
        // lazy sub-resolver is ever added that fetches entities per parent, reintroduce one
        // then — and wire it to a call site in the same change.
        builder = builder.data(DataLoader::new(AlertLoader::new(db.clone()), tokio::spawn));
    }

    builder.finish()
}
