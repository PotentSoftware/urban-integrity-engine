use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use surrealdb::{engine::any::Any, Surreal};
use tracing::{debug, warn};

use urban_integrity_schema::types::{edge::EdgeWeight, node::NodeWeight};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ProjectionStats {
    pub nodes_upserted: usize,
    pub edges_related: usize,
    pub nodes_skipped: usize,
    pub edges_skipped: usize,
    /// Of `edges_skipped`, how many were dropped because the src endpoint had no node in
    /// the projected graph. Tracked separately so a large loss is diagnosable from the
    /// summary rather than buried in per-edge logs.
    pub edges_skipped_unknown_src: usize,
    /// Of `edges_skipped`, how many were dropped because the dst endpoint had no node.
    pub edges_skipped_unknown_dst: usize,
    /// Of `edges_skipped`, how many had no usable `src_id`/`dst_id`/`weight_json`, or a
    /// `weight_json` that would not deserialise.
    pub edges_skipped_malformed: usize,
    /// Of `edges_skipped`, how many reached SurrealDB and were rejected by it — a bad RELATE.
    /// Isolated to the individual statement, never inferred from a failed batch.
    pub edges_skipped_rejected: usize,
    /// Of `edges_skipped`, how many could not be delivered at all (transport failure). These are
    /// not statements SurrealDB disliked; nothing is known about them beyond "never sent".
    pub edges_skipped_undelivered: usize,
}

impl ProjectionStats {
    /// The five sub-counters must partition `edges_skipped` exactly.
    ///
    /// They did not before: a batch that came back non-2xx was written off wholesale into
    /// `edges_skipped` with no sub-counter touched, so the summary reported a cause that
    /// explained none of the loss. Live 2026-07-28: 9,000 edges dropped with
    /// `skipped_unknown_src=0 skipped_unknown_dst=0` — 45 whole batches of 200. Checking the
    /// partition is what makes that class of misattribution impossible to reintroduce quietly.
    pub fn skipped_attributed(&self) -> usize {
        self.edges_skipped_unknown_src
            + self.edges_skipped_unknown_dst
            + self.edges_skipped_malformed
            + self.edges_skipped_rejected
            + self.edges_skipped_undelivered
    }

    /// Fold one flushed batch in, keeping `edges_skipped` and its sub-counters in step.
    ///
    /// The two used to be incremented at separate call sites, which is how they drifted apart.
    fn absorb(&mut self, outcome: BatchOutcome) {
        self.edges_related += outcome.related;
        self.edges_skipped += outcome.rejected + outcome.undelivered;
        self.edges_skipped_rejected += outcome.rejected;
        self.edges_skipped_undelivered += outcome.undelivered;
    }
}

/// What happened to one POSTed chunk of RELATE statements.
enum ChunkResult {
    /// The server answered with a decodable per-statement result array.
    Statuses(Vec<serde_json::Value>),
    /// The server was reached but rejected the request as a whole, or answered something that
    /// could not be mapped back onto the statements. Splitting the chunk can isolate the cause.
    Rejected(String),
    /// The request never completed. Splitting cannot help — the fault is not in the statements.
    Undelivered(String),
}

/// Outcome of flushing one batch, after any splitting.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct BatchOutcome {
    related: usize,
    rejected: usize,
    undelivered: usize,
}

/// How many rejected statements are logged in full before the rest drop to debug.
///
/// A rejection is now per-statement, so a systematically broken edge type would otherwise emit
/// one WARN per edge and bury every other line — the failure mode the aggregate summary was
/// introduced to fix. The first few carry the diagnosis; the total is in the summary regardless.
const REJECTED_LOG_LIMIT: usize = 10;

fn log_rejected(already_logged: &std::sync::atomic::AtomicUsize, stmt: &str, reason: &str) {
    let nth = already_logged.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if nth < REJECTED_LOG_LIMIT {
        warn!(
            statement = %truncate(stmt, 300),
            %reason,
            "project: SurrealDB rejected a RELATE — edge dropped"
        );
    } else {
        debug!(statement = %truncate(stmt, 300), %reason, "project: RELATE rejected");
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// Fraction of edges that may be skipped before projection treats the run as broken rather
/// than merely lossy. Known historical debris (dangling edges whose node events were lost to
/// an old node cap) sits well under this; a ratio at or above it means the node map failed to
/// load — e.g. the replay/empty-graph class of bug — and the projection is not trustworthy.
const MAX_EDGE_SKIP_RATIO: f64 = 0.90;

/// Fraction of seen edges that were skipped (0.0 when no edges were seen).
fn edge_skip_ratio(edges_related: usize, edges_skipped: usize) -> f64 {
    let seen = edges_related + edges_skipped;
    if seen == 0 {
        0.0
    } else {
        edges_skipped as f64 / seen as f64
    }
}

/// Err when the edge-skip ratio reaches [`MAX_EDGE_SKIP_RATIO`] — a signal the node map
/// failed to load and the projection is not trustworthy. Ok (lossy but usable) otherwise.
fn edge_skip_ratio_ok(edges_related: usize, edges_skipped: usize) -> anyhow::Result<()> {
    let seen = edges_related + edges_skipped;
    let ratio = edge_skip_ratio(edges_related, edges_skipped);
    if seen > 0 && ratio >= MAX_EDGE_SKIP_RATIO {
        anyhow::bail!(
            "projection dropped {:.1}% of edges ({} of {}) — exceeds the {:.0}% sanity \
             threshold; the node map almost certainly failed to load. Refusing to report a \
             broken projection as success.",
            ratio * 100.0,
            edges_skipped,
            seen,
            MAX_EDGE_SKIP_RATIO * 100.0
        );
    }
    Ok(())
}

/// Err when a paginated load returned fewer rows than `graph_events` holds for that kind.
///
/// The pagination loop stops on the first short page, which is only sound if a short page
/// really is the end of the stream. Nothing guarantees that, and the failure mode is silent:
/// the projector rebuilds a prefix of the log, reports success, and every downstream reader
/// sees a smaller graph with no error. On 2026-08-11 a run that stopped 62,777 rows into a
/// 498,986-row edge stream left five relation tables completely empty and was read as five
/// separate per-table bugs — see the projection-truncation investigation notes.
///
/// Loading MORE than expected is fine: events can be appended between the count and the last
/// page.
fn loaded_row_count_ok(kind: &str, loaded: usize, expected: usize) -> anyhow::Result<()> {
    if loaded < expected {
        anyhow::bail!(
            "{kind} load returned {loaded} of {expected} rows in graph_events — the paginated \
             SELECT truncated. Refusing to project a prefix of the log as if it were the whole \
             graph; every relation table would be silently partial."
        );
    }
    Ok(())
}

/// Configuration for the raw HTTP `/sql` path used by `flush_relate_batch` and
/// `load_edge_rows_via_http`.
///
/// SurrealDB 3.0.x server + 3.1.x SDK have a flatbuffers codec mismatch:
/// relation records returned by `INSERT INTO <rel_table>` encode typed `in`/`out`
/// RecordId fields that the SDK cannot decode.  We bypass the SDK's flatbuffers
/// path by POSTing plain JSON to the `/sql` endpoint directly.
#[derive(Clone, Debug)]
pub struct RawSqlConfig {
    /// HTTP base URL (e.g. `http://localhost:8000`)
    pub base_url: String,
    pub namespace: String,
    pub database: String,
    /// Optional `(username, password)` for Basic Auth.
    pub auth: Option<(String, String)>,
}

pub struct Projector {
    db: Arc<Surreal<Any>>,
    /// Raw HTTP client used for edge insertion (flatbuffers codec workaround).
    http: reqwest::Client,
    raw: RawSqlConfig,
    /// Rejections logged at WARN so far, across the whole run.
    ///
    /// Counted here rather than per batch: rejections are typically one per batch (a single
    /// poisoned statement), so a per-batch cap never binds and a systematic fault would emit
    /// one WARN per affected edge.
    rejected_logged: std::sync::atomic::AtomicUsize,
}

impl Projector {
    pub fn new(db: Arc<Surreal<Any>>, raw: RawSqlConfig) -> Self {
        Self {
            db,
            http: reqwest::Client::new(),
            raw,
            rejected_logged: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub async fn run(&self) -> Result<ProjectionStats> {
        let mut stats = ProjectionStats::default();

        // ── 1. Open the run marker ────────────────────────────────────────────
        // Before anything is destroyed, so a reader can always tell a partial projection
        // from a finished one. See `begin_run`.
        self.warn_if_previous_run_unfinished().await;
        self.begin_run().await?;

        // ── 2. Load + deduplicate NodeAdded events ────────────────────────────
        // The load happens BEFORE the wipe. A failed or short load must not cost the
        // existing projection: on 2026-08-11 the tables were wiped first and the run then
        // died, leaving nothing to fall back to.
        tracing::info!("project: loading node rows via HTTP JSON (flatbuffers bypass)");
        let node_rows = self.load_node_rows_via_http().await?;
        self.wipe_node_tables().await?;

        // One fold, shared with `replay_from_surreal` and the CLI/GraphQL read path. The
        // hand-rolled `ts > entry.0` here kept the stale BIRTH on an equal-timestamp tie while
        // `fold_latest_node_rows` kept the correction, so the projected `entity` table
        // disagreed with `entity --ch-number` for exactly the batch-backfill case (one clock
        // read stamped across a whole batch). The fold also drops rows with no readable
        // entity_id, which is what `nodes_skipped` counted here.
        // Count unreadable-id rows BEFORE folding: `before - after` would also count every
        // correction the fold legitimately collapsed onto its birth, inflating `nodes_skipped`
        // by the whole NodeUpdated population.
        stats.nodes_skipped += node_rows
            .iter()
            .filter(|row| self.extract_id(&row["payload"]["entity_id"]).is_none())
            .count();
        let node_rows =
            urban_integrity_core::persistence::queries::fold_latest_node_rows(node_rows);

        // ── 3. Upsert nodes + build entity_id → table map ────────────────────
        let mut entity_table: HashMap<String, String> = HashMap::new();

        for row in &node_rows {
            let Some(entity_id) = self.extract_id(&row["payload"]["entity_id"]) else {
                continue;
            };
            let entity_id = &entity_id;
            let wj = match row["payload"]["weight_json"].as_str() {
                Some(s) => s,
                None => {
                    stats.nodes_skipped += 1;
                    continue;
                }
            };
            let weight: NodeWeight = match serde_json::from_str(wj) {
                Ok(w) => w,
                Err(e) => {
                    warn!(entity_id = %entity_id, "project: bad node weight: {e}");
                    stats.nodes_skipped += 1;
                    continue;
                }
            };
            let table = table_for_weight(&weight);
            let data = node_data(entity_id, &weight);
            match self.upsert_node(table, entity_id, data).await {
                Ok(()) => {
                    entity_table.insert(entity_id.clone(), table.to_owned());
                    stats.nodes_upserted += 1;
                }
                Err(e) => {
                    warn!(entity_id = %entity_id, "project: upsert failed: {e}");
                    stats.nodes_skipped += 1;
                }
            }
        }

        tracing::info!(
            upserted = stats.nodes_upserted,
            skipped = stats.nodes_skipped,
            "project: nodes complete"
        );

        // ── 4. Load EdgeAdded events ──────────────────────────────────────────
        // Same ordering rule as the nodes: the relation tables survive until the whole
        // edge stream is in hand and verified against `SELECT count()`.
        tracing::info!("project: loading edge rows via HTTP JSON (flatbuffers bypass)");
        let edge_rows = self.load_edge_rows_via_http().await?;
        self.wipe_relation_tables().await?;

        // ── 5. Relate edges in batches ────────────────────────────────────────
        // Build RELATE statements locally, then POST them in batches of BATCH_SIZE
        // to the /sql endpoint.  One HTTP round-trip per batch vs one per edge.
        const BATCH_SIZE: usize = 200;

        let mut batch: Vec<String> = Vec::with_capacity(BATCH_SIZE);

        for row in &edge_rows {
            let src_id = match self.extract_id(&row["payload"]["src_id"]) {
                Some(s) => s,
                None => {
                    stats.edges_skipped += 1;
                    stats.edges_skipped_malformed += 1;
                    continue;
                }
            };
            let dst_id = match self.extract_id(&row["payload"]["dst_id"]) {
                Some(s) => s,
                None => {
                    stats.edges_skipped += 1;
                    stats.edges_skipped_malformed += 1;
                    continue;
                }
            };
            let src_table = match entity_table.get(&src_id) {
                Some(t) => t.clone(),
                None => {
                    // Per-edge detail at debug — at scale (hundreds of thousands of dangling
                    // edges) a warn! per edge drowns out every other log line and still hides
                    // the total. The aggregate is surfaced once in the summary below.
                    debug!(src = %src_id, "project: unknown src entity — skipping edge");
                    stats.edges_skipped += 1;
                    stats.edges_skipped_unknown_src += 1;
                    continue;
                }
            };
            let dst_table = match entity_table.get(&dst_id) {
                Some(t) => t.clone(),
                None => {
                    debug!(dst = %dst_id, "project: unknown dst entity — skipping edge");
                    stats.edges_skipped += 1;
                    stats.edges_skipped_unknown_dst += 1;
                    continue;
                }
            };
            let wj = match row["payload"]["weight_json"].as_str() {
                Some(s) => s,
                None => {
                    stats.edges_skipped += 1;
                    stats.edges_skipped_malformed += 1;
                    continue;
                }
            };
            let weight: EdgeWeight = match serde_json::from_str(wj) {
                Ok(w) => w,
                Err(e) => {
                    warn!(src = %src_id, "project: bad edge weight: {e}");
                    stats.edges_skipped += 1;
                    stats.edges_skipped_malformed += 1;
                    continue;
                }
            };
            let rel = edge_rel_table(&weight);
            let data = edge_data(&weight);
            let content = edge_data_to_surql_literal(&data);
            let in_id = quote_record_id(&src_table, &src_id);
            let out_id = quote_record_id(&dst_table, &dst_id);
            let stmt = format!("RELATE {in_id}->{rel}->{out_id} CONTENT {content} RETURN NONE");
            batch.push(stmt);

            if batch.len() >= BATCH_SIZE {
                let outcome = self.flush_relate_batch(&mut batch).await;
                stats.absorb(outcome);
            }
        }
        // Flush remaining
        let outcome = self.flush_relate_batch(&mut batch).await;
        stats.absorb(outcome);

        let edges_seen = stats.edges_related + stats.edges_skipped;
        let skip_ratio = edge_skip_ratio(stats.edges_related, stats.edges_skipped);

        if stats.edges_skipped > 0 {
            // One aggregate line, at WARN, so a large loss is impossible to miss and is
            // attributable — instead of being spread across per-edge debug lines.
            //
            // This used to end "(endpoints with no projected node)", stated as fact. It was not:
            // batch-level drops reached this counter without touching either endpoint counter, so
            // the line confidently gave a cause that accounted for none of the loss. Every skip
            // now carries its own reason, and the unattributed residual is printed rather than
            // absorbed into whichever explanation happened to be in the message.
            let unattributed = stats
                .edges_skipped
                .saturating_sub(stats.skipped_attributed());
            warn!(
                related = stats.edges_related,
                skipped = stats.edges_skipped,
                skipped_unknown_src = stats.edges_skipped_unknown_src,
                skipped_unknown_dst = stats.edges_skipped_unknown_dst,
                skipped_malformed = stats.edges_skipped_malformed,
                skipped_rejected = stats.edges_skipped_rejected,
                skipped_undelivered = stats.edges_skipped_undelivered,
                skipped_unattributed = unattributed,
                skip_ratio = format!("{:.1}%", skip_ratio * 100.0),
                "project: edges complete WITH LOSS — {} of {} edges dropped. unknown_src/dst are \
                 dangling edges whose endpoint node never projected (usually historical debris); \
                 rejected are statements SurrealDB refused, each logged with its statement; \
                 undelivered never reached the server at all. A ratio near 100% means the node \
                 map failed to load.",
                stats.edges_skipped,
                edges_seen
            );
            if unattributed > 0 {
                // Cannot happen while every skip site increments a sub-counter, and that is the
                // point: if it ever does, the summary says so instead of quietly mis-explaining
                // the difference.
                warn!(
                    unattributed,
                    "project: {unattributed} dropped edge(s) carry NO recorded cause — the skip \
                     counters no longer partition the loss; treat the breakdown above as incomplete"
                );
            }
        } else {
            tracing::info!(
                related = stats.edges_related,
                "project: edges complete — no edges dropped"
            );
        }

        // Fail loud on catastrophic loss. A near-total skip ratio means the node map did not
        // load (e.g. an empty/failed node pass), so the projected graph is not trustworthy —
        // returning Ok here is how this class of bug used to hide. Known historical debris
        // stays well under the threshold and passes.
        edge_skip_ratio_ok(stats.edges_related, stats.edges_skipped)?;

        // Only now — after the final flush and the skip-ratio gate — is the projection
        // complete. Anything that terminated earlier leaves finished_at_ms null.
        self.finish_run(&stats).await?;

        Ok(stats)
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Record that a projection run has started, with no `finished_at_ms`.
    ///
    /// Goes over the stateless raw HTTP path, NOT the SDK handle. The SDK's websocket
    /// session expires on a long run: the 2026-08-15 rebuild related 498,888 of 498,986
    /// edges cleanly over 2h20m, then died stamping this marker with
    /// `401 Unauthorized for url (http://localhost:8000/rpc)`. A completion marker that
    /// cannot be written at the end of a long run is worse than none — it reports every
    /// slow success as a failure. `raw_sql` authenticates per request.
    ///
    /// The pair with [`Projector::finish_run`] is the only thing that distinguishes a
    /// partial projection from a complete one. `project` is destructive-then-rebuild, so
    /// an interrupted run leaves tables that look exactly like a finished small graph —
    /// no error, no log line. `finished_at_ms` is stamped only after the final flush, so
    /// it stays null forever for a run that died, SIGKILL included.
    async fn begin_run(&self) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        self.raw_sql(
            "begin_run",
            format!(
                "UPSERT projection_runs:current CONTENT {{
                     started_at_ms: {now}, finished_at_ms: NONE,
                     nodes_upserted: 0, edges_related: 0, edges_skipped: 0
                 }}"
            ),
        )
        .await?;
        Ok(())
    }

    /// Stamp the run complete. Called only on the success path.
    async fn finish_run(&self, stats: &ProjectionStats) -> Result<()> {
        let now = Utc::now().timestamp_millis();
        let (nodes, related, skipped) = (
            stats.nodes_upserted,
            stats.edges_related,
            stats.edges_skipped,
        );
        self.raw_sql(
            "finish_run",
            format!(
                "UPSERT projection_runs:current MERGE {{
                     finished_at_ms: {now}, nodes_upserted: {nodes},
                     edges_related: {related}, edges_skipped: {skipped}
                 }}"
            ),
        )
        .await?;
        Ok(())
    }

    /// WARN if the previous run never stamped itself complete.
    ///
    /// Best-effort: a missing marker is normal on a database that predates this, and a
    /// failure to read it must not stop a projection.
    async fn warn_if_previous_run_unfinished(&self) {
        let Ok(rows) = self
            .raw_sql(
                "marker",
                "SELECT * FROM projection_runs:current".to_string(),
            )
            .await
        else {
            return;
        };
        let Some(prev) = rows.first() else { return };
        if prev.get("finished_at_ms").is_none_or(|v| v.is_null()) {
            warn!(
                started_at_ms = prev["started_at_ms"].as_i64(),
                "project: the previous projection run never finished — the tables it left are \
                 partial. This run replaces them."
            );
        }
    }

    /// Node tables, deleted only once the node stream has loaded.
    ///
    /// Split from the relation wipe deliberately. A single up-front `wipe_native_tables`
    /// destroyed both halves before a row was rebuilt, so any failure after it — a short
    /// load, a killed process, a dropped connection — left the database strictly worse
    /// than not running `project` at all.
    async fn wipe_node_tables(&self) -> Result<()> {
        self.wipe(&[
            "entity",
            "officer",
            "location",
            "contract",
            "ward",
            "political_actor",
            // OSINT node tables
            "psc",
            "charity",
            "property",
            "offshore_entity",
            "disqualification",
        ])
        .await?;
        tracing::info!("project: node tables wiped");
        Ok(())
    }

    /// Relation tables, deleted only once the edge stream has loaded and been verified.
    async fn wipe_relation_tables(&self) -> Result<()> {
        self.wipe(&[
            "directs",
            "located_at",
            "awarded_to",
            "issued_by",
            "represents",
            // OSINT relation tables
            "controls",
            "trustee_of",
            "owns_property",
            "offshore_link",
            "donates_to",
            "disqualified_from",
            "declares_interest",
        ])
        .await?;
        tracing::info!("project: relation tables wiped");
        Ok(())
    }

    async fn wipe(&self, tables: &[&str]) -> Result<()> {
        for table in tables {
            let q = format!("DELETE {table}");
            self.db.query(&q).await?;
        }
        Ok(())
    }

    async fn upsert_node(
        &self,
        table: &str,
        entity_id: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        // SurrealDB 3.x typed schema rejects JSON `null` for optional string fields —
        // it expects the field to be absent (NONE) rather than explicitly NULL.
        // Strip all null-valued keys before sending.
        let record_key = strip_own_table_prefix(table, entity_id);
        let data = strip_nulls(data);
        let _: Vec<serde_json::Value> = self
            .db
            .query("UPSERT type::record($table, $id) CONTENT $data")
            .bind(("table", table))
            .bind(("id", record_key))
            .bind(("data", data))
            .await?
            .take(0)?;
        Ok(())
    }

    /// Page through every `graph_events` row of a given `kind` via raw HTTP JSON,
    /// selecting `fields`.
    ///
    /// The SDK's flatbuffers path hits a codec mismatch on this server/SDK version
    /// combination ("Failed to deserialize flatbuffers payload") and can fail on
    /// large result sets, so the SELECT is bypassed with plain JSON over the raw
    /// `/sql` endpoint. (The `upsert_node` / `relate_edge` writes still go through
    /// the SDK.)
    ///
    /// Uses `ORDER BY id` + `START`/`LIMIT` pagination so the projection loads
    /// *every* matching row regardless of graph size. There is deliberately no
    /// fixed cap: a single `LIMIT` would silently truncate once the event count
    /// outgrew it and cascade into "unknown entity" edge drops — exactly the bug
    /// this replaced (node count crossed the old 100k cap → ~34k nodes + their
    /// edges silently lost).
    ///
    /// The loop stops on the first short page, so it also verifies the total against
    /// `SELECT count()` before returning — a short page is not proof of end-of-stream, and
    /// a truncated load is otherwise indistinguishable from a complete one.
    async fn load_rows_via_http(&self, kind: &str, fields: &str) -> Result<Vec<serde_json::Value>> {
        const PAGE: usize = 50_000;
        // Counted BEFORE the pages are read, so an append mid-load can only make the check
        // pass, never fail it.
        let expected = self.count_rows_via_http(kind).await?;
        let mut all: Vec<serde_json::Value> = Vec::new();
        let mut start = 0usize;
        loop {
            let sql = format!(
                "SELECT id, {fields} FROM graph_events WHERE kind = '{kind}' ORDER BY id LIMIT {PAGE} START {start}"
            );
            let rows = self.raw_sql(kind, sql).await?;
            let n = rows.len();
            all.extend(rows);
            if n < PAGE {
                break;
            }
            start += PAGE;
        }
        loaded_row_count_ok(kind, all.len(), expected)?;
        Ok(all)
    }

    /// `SELECT count()` for one event kind — the expected row count for
    /// [`loaded_row_count_ok`].
    async fn count_rows_via_http(&self, kind: &str) -> Result<usize> {
        let sql = format!("SELECT count() FROM graph_events WHERE kind = '{kind}' GROUP ALL");
        let rows = self.raw_sql(kind, sql).await?;
        // No rows at all → the GROUP ALL result is empty, not a zero.
        Ok(rows.first().and_then(|r| r["count"].as_u64()).unwrap_or(0) as usize)
    }

    /// POST one statement to the raw `/sql` endpoint and return its result array.
    async fn raw_sql(&self, kind: &str, sql: String) -> Result<Vec<serde_json::Value>> {
        let url = format!("{}/sql", self.raw.base_url.trim_end_matches('/'));
        let mut req = self
            .http
            .post(&url)
            .header("surreal-ns", &self.raw.namespace)
            .header("surreal-db", &self.raw.database)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(sql);
        if let Some((user, pass)) = &self.raw.auth {
            req = req.basic_auth(user, Some(pass));
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("{kind} SELECT HTTP {status}: {body}");
        }
        // Response: [{status: "OK", result: [ ...rows ]}]
        let mut outer: Vec<serde_json::Value> = resp.json().await?;
        let first = outer
            .pop()
            .ok_or_else(|| anyhow::anyhow!("empty {kind} SELECT response"))?;
        if first["status"].as_str() != Some("OK") {
            let detail = first["result"].as_str().unwrap_or("unknown error");
            anyhow::bail!("{kind} SELECT error: {detail}");
        }
        match first["result"].clone() {
            serde_json::Value::Array(a) => Ok(a),
            _ => anyhow::bail!("unexpected {kind} SELECT result shape"),
        }
    }

    /// Load all EdgeAdded payloads from `graph_events` (paginated, uncapped).
    async fn load_edge_rows_via_http(&self) -> Result<Vec<serde_json::Value>> {
        self.load_rows_via_http("EdgeAdded", "payload").await
    }

    /// Load all node-bearing payloads (with `timestamp`) from `graph_events`
    /// (paginated, uncapped) — both births and corrections.
    ///
    /// `NodeUpdated` carries the FULL merged weight at update time with a later timestamp, so
    /// the highest-timestamp-per-entity dedup in `project` reconstructs current state with no
    /// merge needed here. This mirrors `core::persistence::surreal::replay_from_surreal`,
    /// which has always folded both kinds.
    ///
    /// Loading only `NodeAdded` is what this used to do, and it meant **no status correction
    /// had ever reached the native tables**: on 2026-08-11 a backfill of 512 real
    /// `date_of_cessation` values left `graph_events` holding 2,477 dissolved entities while
    /// the projected `entity` table still reported 1,898 — with 67 earlier transitions
    /// already lost the same way. The hot graph and the projected tables disagreed, silently,
    /// and the projected side is what reports and analyst probes read.
    ///
    /// Queried as two separate equality predicates rather than `kind IN [..]`, matching the
    /// replay path: the SurrealDB 3.x planner mishandles some compound `kind` predicates
    /// elsewhere in this codebase.
    async fn load_node_rows_via_http(&self) -> Result<Vec<serde_json::Value>> {
        let mut rows = self
            .load_rows_via_http("NodeAdded", "kind, payload, timestamp")
            .await?;
        let updates = self
            .load_rows_via_http("NodeUpdated", "kind, payload, timestamp")
            .await?;
        tracing::info!(
            births = rows.len(),
            corrections = updates.len(),
            "project: node rows loaded"
        );
        rows.extend(updates);
        Ok(rows)
    }

    /// POST a batch of pre-built `RELATE … RETURN NONE` statements as a single
    /// semicolon-joined SQL body to the raw `/sql` endpoint.
    /// Returns `(edges_related, edges_skipped)`.
    /// POST one batch of RELATE statements, isolating any that SurrealDB rejects.
    ///
    /// The previous version wrote off the **entire batch** — 200 edges — whenever the response
    /// was not 2xx, counted them into `edges_skipped`, and touched none of the sub-counters. The
    /// live cost of that on 2026-07-28 was 9,000 edges (exactly 45 × `BATCH_SIZE`) vanishing from
    /// the projection under a summary line whose stated cause, "endpoints with no projected node",
    /// explained none of them. One bad statement discarded 199 good ones, and the log said nothing
    /// about which.
    ///
    /// So a rejected chunk is now bisected down to the individual statement. Every statement that
    /// SurrealDB will accept is written; only the ones it genuinely refuses are counted as
    /// rejected, each logged with the statement that caused it. Cost is bounded: a chunk of `n`
    /// costs at most `2n` extra requests, and only on the error path — a single bad statement in
    /// 200 converges in ~8 round-trips, not 200.
    ///
    /// A transport failure is treated differently on purpose: nothing is known about those
    /// statements except that they were never sent, and splitting a chunk the server cannot be
    /// reached for would just multiply the failure.
    async fn flush_relate_batch(&self, batch: &mut Vec<String>) -> BatchOutcome {
        let stmts = std::mem::take(batch);
        let mut outcome = BatchOutcome::default();
        if stmts.is_empty() {
            return outcome;
        }

        // An explicit stack rather than recursion: an async fn cannot call itself without
        // boxing, and this is a plain divide-and-conquer over the statement list.
        let mut pending: Vec<Vec<String>> = vec![stmts];
        while let Some(chunk) = pending.pop() {
            match self.post_relate_chunk(&chunk).await {
                ChunkResult::Statuses(results) => {
                    for (stmt, r) in chunk.iter().zip(results.iter()) {
                        if r["status"].as_str() == Some("OK") {
                            outcome.related += 1;
                        } else {
                            let detail = r["result"].as_str().unwrap_or("unknown");
                            log_rejected(&self.rejected_logged, stmt, detail);
                            outcome.rejected += 1;
                        }
                    }
                }
                ChunkResult::Rejected(reason) => {
                    if chunk.len() == 1 {
                        log_rejected(&self.rejected_logged, &chunk[0], &reason);
                        outcome.rejected += 1;
                    } else {
                        let mid = chunk.len() / 2;
                        debug!(
                            chunk = chunk.len(),
                            %reason,
                            "project: batch rejected — bisecting to isolate the offending statement"
                        );
                        pending.push(chunk[mid..].to_vec());
                        pending.push(chunk[..mid].to_vec());
                    }
                }
                ChunkResult::Undelivered(reason) => {
                    warn!(
                        edges = chunk.len(),
                        %reason,
                        "project: {} edge(s) never reached SurrealDB — transport failure, not a \
                         bad statement",
                        chunk.len()
                    );
                    outcome.undelivered += chunk.len();
                }
            }
        }
        outcome
    }

    /// POST one chunk and classify the response. Never partially attributes: either the caller
    /// gets a result per statement, or it gets a reason it can act on.
    async fn post_relate_chunk(&self, stmts: &[String]) -> ChunkResult {
        let url = format!("{}/sql", self.raw.base_url.trim_end_matches('/'));
        let mut req = self
            .http
            .post(&url)
            .header("surreal-ns", &self.raw.namespace)
            .header("surreal-db", &self.raw.database)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(stmts.join(";\n"));
        if let Some((user, pass)) = &self.raw.auth {
            req = req.basic_auth(user, Some(pass));
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return ChunkResult::Undelivered(e.to_string()),
        };
        let status = resp.status();
        if !status.is_success() {
            // The body carries SurrealDB's reason. Reading it is what turns "batch HTTP 400" —
            // the log line that told us nothing for 9,000 edges — into something diagnosable.
            let body = resp.text().await.unwrap_or_default();
            return ChunkResult::Rejected(format!("HTTP {status}: {}", truncate(&body, 300)));
        }
        let results = match resp.json::<Vec<serde_json::Value>>().await {
            Ok(v) => v,
            Err(e) => return ChunkResult::Rejected(format!("undecodable response: {e}")),
        };
        if results.len() != stmts.len() {
            // Without a 1:1 mapping there is no honest way to say which statement failed, so
            // fall through to the split rather than guess by position.
            return ChunkResult::Rejected(format!(
                "response had {} result(s) for {} statement(s)",
                results.len(),
                stmts.len()
            ));
        }
        ChunkResult::Statuses(results)
    }

    fn extract_id(&self, v: &serde_json::Value) -> Option<String> {
        if let Some(s) = v.as_str() {
            return Some(s.to_owned());
        }
        if let (Some(tb), Some(id_val)) = (v["tb"].as_str(), Some(&v["id"])) {
            let id_str = if let Some(s) = id_val.as_str() {
                s.to_owned()
            } else if let Some(s) = id_val["String"].as_str() {
                s.to_owned()
            } else {
                id_val.to_string().trim_matches('"').to_owned()
            };
            return Some(format!("{tb}:{id_str}"));
        }
        None
    }
}

// ── Pure helper functions (unit-testable, no DB) ──────────────────────────────

pub fn table_for_weight(weight: &NodeWeight) -> &'static str {
    match weight {
        NodeWeight::Entity(_) => "entity",
        NodeWeight::Officer(_) => "officer",
        NodeWeight::Location(_) => "location",
        NodeWeight::Contract(_) => "contract",
        NodeWeight::Ward(_) => "ward",
        NodeWeight::PoliticalActor(_) => "political_actor",
        // OSINT variants injected via enricher crate — not handled by ingestor projector
        NodeWeight::Psc(_) => "psc",
        NodeWeight::Charity(_) => "charity",
        NodeWeight::Property(_) => "property",
        NodeWeight::OffshoreEntity(_) => "offshore_entity",
        NodeWeight::Disqualification(_) => "disqualification",
    }
}

pub fn node_data(entity_id: &str, weight: &NodeWeight) -> serde_json::Value {
    match weight {
        NodeWeight::Entity(e) => serde_json::json!({
            "entity_id": entity_id,
            "name": e.name,
            "company_number": e.company_number,
            "entity_status": e.legacy_status(),
            // `legacy_status()` flattens EntityStatus to its discriminant and drops the date
            // the Dissolved/InLiquidation variants carry. RFC3339 string, matching the
            // Disqualification arm below: SurrealDB 3.x will not coerce a string into a
            // `datetime` field on the `CONTENT $data` path, it fails the row silently.
            "entity_status_date": e.status_date().map(|t| t.to_rfc3339()),
            "incorporation_date": e.incorporation_date.map(|d| d.to_string()),
        }),
        NodeWeight::Officer(o) => serde_json::json!({
            "entity_id": entity_id,
            "name": o.name,
            "officer_role": o.officer_role,
            "appointed_on": o.appointed_on,
            "resigned_on": o.resigned_on,
            // Must stay in step with SCHEMA_DEF's officer table: a field defined there but
            // absent from this literal is dropped silently, with no error and no warning.
            "company_number": o.company_number,
        }),
        NodeWeight::Location(l) => serde_json::json!({
            "entity_id": entity_id,
            "name": l.name,
            "postcode": l.postcode,
            "address_line_1": l.address_line_1,
            "locality": l.locality,
        }),
        NodeWeight::Contract(c) => serde_json::json!({
            "entity_id": entity_id,
            "name": c.name,
            "ocid": c.ocid,
            "value_gbp": c.value_gbp,
            "contract_status": c.status,
            "date_published": c.date_published,
            "ward_code": c.ward_code,
            "tender_ref": c.tender_ref,
            "application_ref": c.application_ref,
            // SCHEMA_DEF types all three as `option<string>`. `application_received_date`
            // is a NaiveDate in the weight, so stringify it the same way the Entity arm
            // handles `incorporation_date` — SurrealDB will not coerce a mismatched shape,
            // it fails the row silently. `applicant_provenance` serialises via its
            // `rename_all = "snake_case"` derive.
            "application_received_date": c.application_received_date.map(|d| d.to_string()),
            "applicant_provenance": c.applicant_provenance,
            "portal_detail_handle": c.portal_detail_handle,
        }),
        NodeWeight::Ward(w) => serde_json::json!({
            "entity_id": entity_id,
            "name": w.name,
        }),
        NodeWeight::PoliticalActor(p) => serde_json::json!({
            "entity_id": entity_id,
            "name": p.name,
            // Phase 20 POL-02 extended PoliticalActorData and SCHEMA_DEF with both of these,
            // but this literal was never updated, so `political_actor.party` and
            // `.council_name` read NONE for every projected row regardless of what
            // `graph_events` held. Same silent-drop shape as `contract`'s
            // `application_received_date`. Guarded now by
            // `tests/projector_schema_parity.rs`.
            "party": p.party,
            "council_name": p.council_name,
        }),
        NodeWeight::Psc(p) => serde_json::json!({
            "entity_id": entity_id,
            "name": p.name,
            "company_number": p.company_number,
            "natures_of_control": p.natures_of_control,
        }),
        NodeWeight::Charity(c) => serde_json::json!({
            "entity_id": entity_id,
            "name": c.name,
            "charity_number": c.charity_number,
            "registered_name": c.registered_name,
        }),
        NodeWeight::Property(p) => serde_json::json!({
            "entity_id": entity_id,
            "title_number": p.title_number,
            "tenure": p.tenure,
            "price_paid": p.price_paid,
            "company_number": p.company_number,
            // Set by the OCOD adapter, NULL for CCOD titles -- the presence of this field is
            // what separates the two HMLR datasets on a projected row. Until now it existed
            // only in `graph_events`, so `producer_of_property` worked solely because the
            // Tier-2 probes read the event log rather than this table.
            "country_incorporated": p.country_incorporated,
        }),
        NodeWeight::OffshoreEntity(o) => serde_json::json!({
            "entity_id": entity_id,
            "name": o.name,
            "jurisdiction": o.jurisdiction,
            "icij_node_id": o.icij_node_id,
        }),
        // Dates as RFC3339 strings: node rows go through `upsert_node`'s `CONTENT $data`,
        // and SurrealDB 3.x will not coerce a string into a `datetime` field. Matches
        // `entity.incorporation_date`. The typed values are on the edge, whose raw-SQL
        // path applies the `<datetime>` cast.
        NodeWeight::Disqualification(d) => serde_json::json!({
            "entity_id": entity_id,
            "case_number": d.case_number,
            "disqualified_from": d.disqualified_from.map(|t| t.to_rfc3339()),
            "disqualified_until": d.disqualified_until.map(|t| t.to_rfc3339()),
        }),
    }
}

pub fn edge_rel_table(weight: &EdgeWeight) -> &'static str {
    match weight {
        EdgeWeight::Directs { .. } => "directs",
        EdgeWeight::LocatedAt { .. } => "located_at",
        EdgeWeight::AwardedTo { .. } => "awarded_to",
        EdgeWeight::IssuedBy { .. } => "issued_by",
        EdgeWeight::Represents { .. } => "represents",
        // OSINT edge variants injected via enricher crate — not projected by ingestor
        EdgeWeight::Controls { .. } => "controls",
        EdgeWeight::TrusteeOf { .. } => "trustee_of",
        EdgeWeight::OwnsProperty { .. } => "owns_property",
        EdgeWeight::OffshoreLink { .. } => "offshore_link",
        EdgeWeight::DonatesTo { .. } => "donates_to",
        EdgeWeight::DisqualifiedFrom { .. } => "disqualified_from",
        EdgeWeight::DeclaresInterest { .. } => "declares_interest",
    }
}

pub fn edge_data(weight: &EdgeWeight) -> serde_json::Value {
    match weight {
        EdgeWeight::Directs { meta, data } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
            "data": {
                "details": data.details,
                "officer_role": data.officer_role,
            },
        }),
        EdgeWeight::LocatedAt { meta, data } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
            "data": { "details": data.details },
        }),
        EdgeWeight::AwardedTo { meta, data } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
            "data": { "details": data.details },
        }),
        EdgeWeight::IssuedBy { meta, data } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
            "data": { "details": data.details },
        }),
        EdgeWeight::Represents { meta, data } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
            "data": { "details": data.details },
        }),
        EdgeWeight::Controls { meta } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
        }),
        EdgeWeight::TrusteeOf { meta } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
        }),
        EdgeWeight::OffshoreLink { meta } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
        }),
        EdgeWeight::DonatesTo { meta } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
        }),
        EdgeWeight::DeclaresInterest { meta, data } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
            // Must stay in step with SCHEMA_DEF's declares_interest table: a field defined
            // there but absent from this literal is dropped silently, with no error.
            "data": {
                "council": data.council,
                "category": data.category,
            },
        }),
        EdgeWeight::OwnsProperty { meta, data } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
            "data": {
                "price_paid": data.price_paid,
                "tenure": data.tenure,
            },
        }),
        EdgeWeight::DisqualifiedFrom { meta, data } => serde_json::json!({
            "meta": {
                "created_at": meta.created_at,
                "effective_from": meta.effective_from,
                "effective_to": meta.effective_to,
            },
            "data": {
                "disqualified_from": data.disqualified_from.map(|d| d.to_rfc3339()),
                "disqualified_until": data.disqualified_until.map(|d| d.to_rfc3339()),
                "case_number": data.case_number,
            },
        }),
    }
}

/// Remove all keys whose value is `null` from a JSON object (top-level only).
/// SurrealDB 3.x typed schema rejects explicit JSON `null` for `option<string>`
/// fields — the field must be absent for the NONE sentinel to apply.
pub fn strip_nulls(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let cleaned = map.into_iter().filter(|(_, val)| !val.is_null()).collect();
            serde_json::Value::Object(cleaned)
        }
        other => other,
    }
}

/// Strip a leading `"{table}:"` from `id`, if present — table-equality only.
///
/// Some `NodeWeight` variants (namely `Property`, D-15) carry an `entity_id` that
/// the emitting adapter has already prefixed with its own table name (e.g.
/// `"property:ln1234"`). Both record-id-construction sites in this file
/// (`upsert_node`'s `$id` bind, `quote_record_id`) independently prepend
/// `"{table}:"` again, so an unstripped id doubles up:
/// `property:`property:ln1234`` — measured live 2026-08-21, 105,809/105,809
/// `property` rows (D-15/D-16).
///
/// This is deliberately a table-equality strip, never a blanket "strip
/// everything before the first colon" — that would break the load-bearing
/// `director:{slug}` keying inside the `entity` table (see
/// `an_ordinary_slug_is_untouched` below, which must keep passing unmodified).
fn strip_own_table_prefix<'a>(table: &str, id: &'a str) -> &'a str {
    id.strip_prefix(&format!("{table}:")).unwrap_or(id)
}

/// Render `table:id` as a SurrealQL record id, escaping the id so it cannot break the
/// statement it is embedded in.
///
/// Backtick-quoted identifiers are *not* literal — SurrealQL processes escape sequences
/// inside them. So a slug containing a backslash produces `Parse error: Invalid escape
/// sequence` and the whole statement is refused. Measured live 2026-07-29 against
/// `an_anglesey_aebaa785`: address slugs such as
/// `location:maes-parcio-\-car-park,mynydd-parys-\-parys-mountain` come from planning
/// records whose address field uses a backslash as a Welsh/English separator. Every one
/// of those poisoned its whole 200-statement batch, which is where the run's 9,000
/// "skipped" edges came from.
///
/// The `CONTENT` literal has always escaped its strings (see
/// `edge_data_to_surql_literal`); the record id was the one interpolation that did not.
fn quote_record_id(table: &str, id: &str) -> String {
    let id = strip_own_table_prefix(table, id);
    let escaped = id.replace('\\', "\\\\").replace('`', "\\`");
    format!("{table}:`{escaped}`")
}

/// Serialise edge `data` (a JSON object from `edge_data()`) into a SurrealQL
/// object literal suitable for embedding directly in a `RELATE … CONTENT …`
/// statement sent to the `/sql` HTTP endpoint.
///
/// The TYPE RELATION SCHEMAFULL tables declare `meta.created_at`,
/// `meta.effective_from`, and `meta.effective_to` as `datetime` / `option<datetime>`.
/// When the data arrives as a plain JSON string (e.g. `"2024-01-01T00:00:00Z"`)
/// the server rejects it with "Expected `datetime` but found `'…'`".
/// We wrap those specific string values with the SurrealQL `<datetime>"…"` cast.
///
/// All other fields are serialised as standard JSON literals (numbers, strings,
/// booleans, null, arrays, and nested objects).
pub fn edge_data_to_surql_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .map(|(k, val)| {
                    let surql_val = if k == "meta" {
                        // Recurse into meta object, casting datetime fields.
                        edge_meta_to_surql_literal(val)
                    } else if k == "data" {
                        // Recurse into data sub-object, casting known datetime fields
                        // (e.g. disqualified_from/disqualified_until in DisqualifiedFrom edges).
                        edge_data_subobj_to_surql_literal(val)
                    } else {
                        json_to_surql_literal(val)
                    };
                    format!("{k}: {surql_val}")
                })
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        other => json_to_surql_literal(other),
    }
}

/// Serialise the `data` sub-object of an edge, applying `<datetime>"…"` casts to
/// known datetime keys (`disqualified_from`, `disqualified_until`).  All other
/// fields pass through `json_to_surql_literal` unchanged.
fn edge_data_subobj_to_surql_literal(v: &serde_json::Value) -> String {
    let serde_json::Value::Object(map) = v else {
        return json_to_surql_literal(v);
    };
    let datetime_keys = ["disqualified_from", "disqualified_until"];
    let fields: Vec<String> = map
        .iter()
        .map(|(k, val)| {
            let surql_val = if datetime_keys.contains(&k.as_str()) {
                match val {
                    serde_json::Value::String(s) => {
                        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
                        format!("<datetime>\"{escaped}\"")
                    }
                    serde_json::Value::Null => "NONE".to_owned(),
                    other => json_to_surql_literal(other),
                }
            } else {
                json_to_surql_literal(val)
            };
            format!("{k}: {surql_val}")
        })
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// Serialise the `meta` sub-object, applying `<datetime>"…"` casts to the three
/// known datetime fields.
fn edge_meta_to_surql_literal(v: &serde_json::Value) -> String {
    let serde_json::Value::Object(map) = v else {
        return json_to_surql_literal(v);
    };
    let datetime_keys = ["created_at", "effective_from", "effective_to"];
    let fields: Vec<String> = map
        .iter()
        .map(|(k, val)| {
            let surql_val = if datetime_keys.contains(&k.as_str()) {
                match val {
                    serde_json::Value::String(s) => {
                        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
                        format!("<datetime>\"{escaped}\"")
                    }
                    serde_json::Value::Null => "NONE".to_owned(),
                    other => json_to_surql_literal(other),
                }
            } else {
                json_to_surql_literal(val)
            };
            format!("{k}: {surql_val}")
        })
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// Convert a `serde_json::Value` to a SurrealQL literal.  No datetime casting —
/// use `edge_data_to_surql_literal` for edge data containing datetime fields.
fn json_to_surql_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NONE".to_owned(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            // Escape backslashes then double-quotes.
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{escaped}\"")
        }
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_to_surql_literal).collect();
            format!("[{}]", items.join(", "))
        }
        serde_json::Value::Object(map) => {
            let fields: Vec<String> = map
                .iter()
                .map(|(k, val)| format!("{k}: {}", json_to_surql_literal(val)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use urban_integrity_schema::types::{
        edge::{AwardedToData, DirectsData, EdgeMeta, IssuedByData, LocatedAtData, RepresentsData},
        node::{
            ContractData, EntityData, EntityStatus, LocationData, OfficerData, PoliticalActorData,
            WardData,
        },
    };

    fn meta() -> EdgeMeta {
        EdgeMeta {
            created_at: Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

    // ── batch rejection isolation ─────────────────────────────────────────────
    //
    // The defect these pin: a non-2xx response discarded the WHOLE batch (200 edges) into
    // `edges_skipped`, touching no sub-counter. Live 2026-07-28 that lost 9,000 edges — exactly
    // 45 × BATCH_SIZE — under a summary line blaming unprojected endpoints, whose own counters
    // both read 0.

    /// A stand-in `/sql` endpoint. Returns HTTP 400 for any body containing `poison`, and an
    /// OK-per-statement JSON array otherwise — which is how SurrealDB behaves when one statement
    /// in a POSTed batch is unparseable: the request fails as a whole, taking its neighbours down.
    fn spawn_poison_sql_server(max_requests: usize) -> (String, std::sync::Arc<AtomicUsize>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let requests = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();

        std::thread::spawn(move || {
            for _ in 0..max_requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = vec![0u8; 65536];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();

                let response = if body.contains("poison") {
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 21\r\nConnection: close\r\n\r\nparse error near ...\n".to_string()
                } else {
                    let n_stmts = body.matches("RELATE").count();
                    let results: Vec<String> = (0..n_stmts)
                        .map(|_| r#"{"status":"OK","result":null,"time":"1µs"}"#.to_string())
                        .collect();
                    let json = format!("[{}]", results.join(","));
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    )
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });

        (format!("http://127.0.0.1:{port}"), requests)
    }

    /// A `/sql` endpoint that records the `kind = '…'` of every SELECT it is asked for and
    /// answers each with an empty result set, so a loader terminates after one page.
    fn spawn_kind_recording_server(
        max_requests: usize,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let kinds = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = kinds.clone();

        std::thread::spawn(move || {
            for _ in 0..max_requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 65536];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
                if let Some(rest) = body.split("kind = '").nth(1) {
                    if let Some(kind) = rest.split('\'').next() {
                        seen.lock().expect("lock").push(kind.to_string());
                    }
                }
                let json = r#"[{"status":"OK","result":[],"time":"1µs"}]"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    json.len(),
                    json
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        (format!("http://127.0.0.1:{port}"), kinds)
    }

    /// The projector must resolve an equal-timestamp birth/correction pair the same way the
    /// CLI read path does: to the correction.
    ///
    /// The projector loaded `NodeAdded` first and used strict `>`, so a tie kept the birth while
    /// `fold_latest_node_rows` kept the correction — the projected table and the read path
    /// disagreed for every same-millisecond backfill pair, which is the disagreement class
    /// Phase 30 existed to close.
    #[test]
    fn equal_timestamp_correction_wins_in_the_projector_fold() {
        let birth = serde_json::json!({
            "kind": "NodeAdded",
            "timestamp": 500,
            "payload": {
                "entity_id": "GB-COH:12345678",
                "weight_json": "{\"type\":\"Entity\",\"name\":\"A\",\"status\":{\"type\":\"Active\"}}"
            }
        });
        let correction = serde_json::json!({
            "kind": "NodeUpdated",
            "timestamp": 500,
            "payload": {
                "entity_id": "GB-COH:12345678",
                "weight_json": "{\"type\":\"Entity\",\"name\":\"B\",\"status\":{\"type\":\"Active\"}}"
            }
        });

        for rows in [
            vec![birth.clone(), correction.clone()],
            vec![correction.clone(), birth.clone()],
        ] {
            let folded = urban_integrity_core::persistence::queries::fold_latest_node_rows(rows);
            assert_eq!(folded.len(), 1);
            assert_eq!(
                folded[0]["kind"].as_str(),
                Some("NodeUpdated"),
                "the correction must win the tie in both arrival orders"
            );
        }
    }

    /// The projector must read `NodeUpdated`, not only `NodeAdded`.
    ///
    /// The defect this pins: `project` loaded `NodeAdded` and `EdgeAdded` only, so **no
    /// `NodeUpdated` event has ever reached the native tables**. Live on 2026-08-11, after a
    /// backfill wrote 512 real `date_of_cessation` values as `NodeUpdated`, `graph_events`
    /// held 2,477 dissolved entities and the projected `entity` table still reported 1,898 —
    /// and the same silence had already swallowed 67 earlier status transitions. The replay
    /// path (`core::persistence::surreal`) folds both kinds and reconstructs current state
    /// correctly, so the hot graph and the projected tables actively disagreed.
    #[tokio::test]
    async fn projector_reads_node_updated_events_too() {
        let (base, kinds) = spawn_kind_recording_server(8);
        let projector = test_projector(base);

        let _ = projector.load_node_rows_via_http().await;

        let seen = kinds.lock().expect("lock").clone();
        assert!(
            seen.iter().any(|k| k == "NodeAdded"),
            "NodeAdded must still be loaded; saw {seen:?}"
        );
        assert!(
            seen.iter().any(|k| k == "NodeUpdated"),
            "NodeUpdated must be loaded — without it every status correction (active -> \
             dissolved) is silently dropped from the projected tables; saw {seen:?}"
        );
    }

    fn test_projector(base_url: String) -> Projector {
        // `flush_relate_batch` never touches the SDK handle — it is the HTTP bypass path — so an
        // unconnected `Surreal` is sufficient and keeps this test DB-free.
        Projector::new(
            Arc::new(Surreal::init()),
            RawSqlConfig {
                base_url,
                namespace: "test_ns".to_string(),
                database: "test_db".to_string(),
                auth: None,
            },
        )
    }

    fn relate_stmt(tag: &str) -> String {
        format!("RELATE entity:`a`->awarded_to->contract:`{tag}` CONTENT {{}} RETURN NONE")
    }

    #[tokio::test]
    async fn one_rejected_statement_does_not_discard_its_batch() {
        // Enough accepts to cover the bisection: the split retries at most 2n chunks.
        let (base, requests) = spawn_poison_sql_server(64);
        let projector = test_projector(base);

        let mut batch: Vec<String> = (0..7).map(|i| relate_stmt(&format!("ok{i}"))).collect();
        batch.insert(3, relate_stmt("poison"));

        let outcome = projector.flush_relate_batch(&mut batch).await;

        assert_eq!(
            outcome.related, 7,
            "the 7 good statements must be written — the old code dropped all 8 because one \
             was bad"
        );
        assert_eq!(
            outcome.rejected, 1,
            "exactly the poison statement is rejected"
        );
        assert_eq!(outcome.undelivered, 0);
        assert!(
            requests.load(Ordering::SeqCst) > 1,
            "a rejected batch must be split and retried, not written off after one request"
        );
        assert!(batch.is_empty(), "the batch buffer must be drained");
    }

    #[tokio::test]
    async fn an_entirely_rejected_batch_is_attributed_statement_by_statement() {
        let (base, _) = spawn_poison_sql_server(64);
        let projector = test_projector(base);

        let mut batch: Vec<String> = (0..4).map(|i| relate_stmt(&format!("poison{i}"))).collect();
        let outcome = projector.flush_relate_batch(&mut batch).await;

        assert_eq!(outcome.related, 0);
        assert_eq!(
            outcome.rejected, 4,
            "every dropped edge must be counted as rejected individually, so the summary's \
             breakdown accounts for the whole loss"
        );
        assert_eq!(outcome.undelivered, 0);
    }

    #[test]
    fn a_backslash_in_a_slug_cannot_break_the_statement() {
        // The live offender: planning addresses use a backslash as a Welsh/English separator,
        // and backtick-quoted identifiers process escapes, so the raw form parsed as an
        // invalid escape sequence and SurrealDB refused the whole batch.
        assert_eq!(
            quote_record_id("location", "maes-parcio-\\-car-park"),
            "location:`maes-parcio-\\\\-car-park`"
        );
    }

    #[test]
    fn a_backtick_in_a_slug_cannot_terminate_the_identifier() {
        // Not seen live, but an unescaped backtick would close the identifier early and let the
        // rest of the slug be parsed as SurrealQL.
        assert_eq!(
            quote_record_id("entity", "acme`corp"),
            "entity:`acme\\`corp`"
        );
    }

    #[test]
    fn an_ordinary_slug_is_untouched() {
        assert_eq!(
            quote_record_id("entity", "director:jane-smith"),
            "entity:`director:jane-smith`"
        );
    }

    #[test]
    fn property_double_prefix_is_stripped() {
        // D-15/D-16: emitting adapters already prefix property entity_ids with
        // "property:" before handing them to the projector, and quote_record_id
        // used to append the table prefix again, doubling it.
        assert_eq!(
            quote_record_id("property", "property:ln1234"),
            "property:`ln1234`"
        );
    }

    #[test]
    fn strip_own_table_prefix_is_table_equality_only() {
        // The strip only fires when the leading segment equals the table name —
        // it must never become a blanket "strip everything before the first colon",
        // which would break the load-bearing director:{slug} keying inside `entity`.
        assert_eq!(strip_own_table_prefix("property", "property:xyz"), "xyz");
        assert_eq!(
            strip_own_table_prefix("entity", "director:xyz"),
            "director:xyz"
        );
    }

    #[tokio::test]
    async fn a_clean_batch_costs_exactly_one_request() {
        let (base, requests) = spawn_poison_sql_server(8);
        let projector = test_projector(base);

        let mut batch: Vec<String> = (0..16).map(|i| relate_stmt(&format!("ok{i}"))).collect();
        let outcome = projector.flush_relate_batch(&mut batch).await;

        assert_eq!(outcome.related, 16);
        assert_eq!(outcome.rejected, 0);
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "the happy path must not pay for the isolation machinery"
        );
    }

    #[tokio::test]
    async fn an_unreachable_server_is_undelivered_not_rejected() {
        // Nothing is listening. These statements were never judged by SurrealDB, so calling them
        // "rejected" would invent a verdict; and splitting would multiply a failure that has
        // nothing to do with the statements.
        let projector = test_projector("http://127.0.0.1:1".to_string());
        let mut batch: Vec<String> = (0..5).map(|i| relate_stmt(&format!("ok{i}"))).collect();

        let outcome = projector.flush_relate_batch(&mut batch).await;

        assert_eq!(outcome.related, 0);
        assert_eq!(outcome.rejected, 0);
        assert_eq!(outcome.undelivered, 5);
    }

    #[tokio::test]
    async fn an_empty_batch_is_a_no_op() {
        let projector = test_projector("http://127.0.0.1:1".to_string());
        let mut batch: Vec<String> = Vec::new();
        assert_eq!(
            projector.flush_relate_batch(&mut batch).await,
            BatchOutcome::default()
        );
    }

    // ── skip attribution ──────────────────────────────────────────────────────

    #[test]
    fn absorb_keeps_the_total_and_its_breakdown_in_step() {
        let mut stats = ProjectionStats::default();
        stats.absorb(BatchOutcome {
            related: 10,
            rejected: 3,
            undelivered: 2,
        });
        assert_eq!(stats.edges_related, 10);
        assert_eq!(stats.edges_skipped, 5);
        assert_eq!(
            stats.skipped_attributed(),
            stats.edges_skipped,
            "every skipped edge must carry a recorded cause — an unattributed residual is the \
             exact defect being fixed"
        );
    }

    #[test]
    fn skipped_attributed_covers_every_skip_reason() {
        let stats = ProjectionStats {
            edges_skipped: 15,
            edges_skipped_unknown_src: 4,
            edges_skipped_unknown_dst: 3,
            edges_skipped_malformed: 2,
            edges_skipped_rejected: 5,
            edges_skipped_undelivered: 1,
            ..Default::default()
        };
        assert_eq!(stats.skipped_attributed(), 15);
    }

    #[test]
    fn the_live_regression_shape_would_now_be_visible() {
        // The 2026-07-28 numbers, as the old code recorded them: a total with nothing to explain
        // it. `skipped_attributed` is what makes that state detectable instead of plausible.
        let old_shape = ProjectionStats {
            edges_related: 379_162,
            edges_skipped: 9_000,
            edges_skipped_unknown_src: 0,
            edges_skipped_unknown_dst: 0,
            ..Default::default()
        };
        assert_eq!(
            old_shape.edges_skipped - old_shape.skipped_attributed(),
            9_000,
            "the historical loss was entirely unattributed, which the summary now reports"
        );
    }

    // ── edge-skip ratio + fail-loud threshold ─────────────────────────────────

    #[test]
    fn test_edge_skip_ratio_basic() {
        assert_eq!(super::edge_skip_ratio(0, 0), 0.0);
        assert_eq!(super::edge_skip_ratio(100, 0), 0.0);
        assert_eq!(super::edge_skip_ratio(0, 100), 1.0);
        assert!((super::edge_skip_ratio(50, 50) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_skip_ratio_ok_for_no_edges_and_clean_runs() {
        // Empty graph is legitimate, not a failure.
        assert!(super::edge_skip_ratio_ok(0, 0).is_ok());
        // No skips.
        assert!(super::edge_skip_ratio_ok(1000, 0).is_ok());
    }

    #[test]
    fn test_skip_ratio_ok_passes_historical_debris() {
        // The observed real case: ~59% of edges dangle on nodes lost to an old cap. Lossy
        // but usable — must NOT fail the projection.
        assert!(super::edge_skip_ratio_ok(239_000, 342_000).is_ok());
    }

    #[test]
    fn test_skip_ratio_fails_on_catastrophic_loss() {
        // Node map failed to load → nearly everything dropped → must error.
        assert!(super::edge_skip_ratio_ok(0, 581_000).is_err());
        assert!(super::edge_skip_ratio_ok(100, 100_000).is_err());
        // Exactly at the threshold is a failure (>=).
        assert!(super::edge_skip_ratio_ok(10, 90).is_err());
        // Just under passes.
        assert!(super::edge_skip_ratio_ok(11, 89).is_ok());
    }

    // ── loaded_row_count_ok ───────────────────────────────────────────────────

    #[test]
    fn truncated_load_is_rejected() {
        // The 2026-08-11 shape: the edge pass got 62,777 of 498,986 rows. The skip-ratio
        // guard cannot see this — it divides by rows *iterated*, so 436,209 rows that were
        // never loaded look like a clean run.
        let err = super::loaded_row_count_ok("EdgeAdded", 62_777, 498_986).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("62777"), "must name what it got: {msg}");
        assert!(msg.contains("498986"), "must name what it expected: {msg}");
        // One short page is enough to matter: a single lost row is a lost edge.
        assert!(super::loaded_row_count_ok("NodeAdded", 463_721, 463_722).is_err());
    }

    #[test]
    fn complete_load_passes() {
        assert!(super::loaded_row_count_ok("EdgeAdded", 498_986, 498_986).is_ok());
        assert!(super::loaded_row_count_ok("EdgeAdded", 0, 0).is_ok());
        // Events appended between the count and the last page — not truncation.
        assert!(super::loaded_row_count_ok("EdgeAdded", 498_990, 498_986).is_ok());
    }

    // ── table_for_weight ──────────────────────────────────────────────────────

    #[test]
    fn test_table_entity() {
        let w = NodeWeight::Entity(EntityData {
            name: "ACME Ltd".into(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        assert_eq!(table_for_weight(&w), "entity");
    }

    #[test]
    fn test_table_officer() {
        let w = NodeWeight::Officer(OfficerData {
            name: "Jane Smith".into(),
            officer_role: None,
            appointed_on: None,
            resigned_on: None,
            company_number: None,
        });
        assert_eq!(table_for_weight(&w), "officer");
    }

    #[test]
    fn test_table_location() {
        let w = NodeWeight::Location(LocationData {
            name: "SW1A 1AA".into(),
            postcode: Some("SW1A1AA".into()),
            address_line_1: None,
            locality: None,
        });
        assert_eq!(table_for_weight(&w), "location");
    }

    #[test]
    fn test_table_contract() {
        let w = NodeWeight::Contract(ContractData {
            name: "Road Works 2024".into(),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        });
        assert_eq!(table_for_weight(&w), "contract");
    }

    #[test]
    fn test_table_ward() {
        let w = NodeWeight::Ward(WardData {
            name: "E05001234".into(),
        });
        assert_eq!(table_for_weight(&w), "ward");
    }

    #[test]
    fn test_table_political_actor() {
        let w = NodeWeight::PoliticalActor(PoliticalActorData {
            name: "Cllr Smith".into(),
            ..Default::default()
        });
        assert_eq!(table_for_weight(&w), "political_actor");
    }

    // ── node_data ─────────────────────────────────────────────────────────────

    #[test]
    fn test_node_data_entity_has_entity_id() {
        let w = NodeWeight::Entity(EntityData {
            name: "ACME Ltd".into(),
            company_number: Some("12345678".into()),
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        let d = node_data("director:acme-ltd", &w);
        assert_eq!(d["entity_id"].as_str(), Some("director:acme-ltd"));
        assert_eq!(d["name"].as_str(), Some("ACME Ltd"));
        assert_eq!(d["company_number"].as_str(), Some("12345678"));
        assert_eq!(d["entity_status"].as_str(), Some("active"));
    }

    #[test]
    fn test_node_data_officer_fields() {
        let w = NodeWeight::Officer(OfficerData {
            name: "Jane Smith".into(),
            officer_role: Some("director".into()),
            appointed_on: Some("2020-01-15".into()),
            resigned_on: None,
            company_number: None,
        });
        let d = node_data("OFF:12345678-1", &w);
        assert_eq!(d["entity_id"].as_str(), Some("OFF:12345678-1"));
        assert_eq!(d["officer_role"].as_str(), Some("director"));
        assert_eq!(d["appointed_on"].as_str(), Some("2020-01-15"));
        assert!(d["resigned_on"].is_null());
    }

    #[test]
    fn officer_company_number_survives_projection() {
        // A field can be defined in SCHEMA_DEF and still vanish here without any error: the
        // projected row is built from a `json!` literal, so an omitted key is simply dropped.
        // That is how 30,836 events carried an `application_received_date` while the projected
        // `contract` table held zero of them. `company_number` is the officer's only company
        // scope now that the key is `director:{slug}`, so losing it silently would un-scope the
        // PSC join without anything failing.
        let w = NodeWeight::Officer(OfficerData {
            name: "Jane Smith".into(),
            officer_role: Some("director".into()),
            appointed_on: None,
            resigned_on: None,
            company_number: Some("12345678".into()),
        });
        let d = node_data("director:jane-smith", &w);
        assert_eq!(
            d["company_number"].as_str(),
            Some("12345678"),
            "officer company_number must reach the projected row"
        );
    }

    #[test]
    fn test_node_data_contract_fields() {
        let w = NodeWeight::Contract(ContractData {
            name: "Road Works".into(),
            ocid: Some("ocds-b5fd17-abc".into()),
            value_gbp: Some(50000.0),
            status: Some("active".into()),
            date_published: Some("2024-01-01".into()),
            ward_code: Some("E05001234".into()),
            tender_ref: Some("CF-1234".into()),
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        });
        let d = node_data("CF:ocds-b5fd17-abc", &w);
        assert_eq!(d["ocid"].as_str(), Some("ocds-b5fd17-abc"));
        assert!((d["value_gbp"].as_f64().unwrap() - 50000.0).abs() < 1e-6);
        assert_eq!(d["ward_code"].as_str(), Some("E05001234"));
    }

    // ── edge_rel_table ────────────────────────────────────────────────────────

    #[test]
    fn test_edge_rel_table_directs() {
        let w = EdgeWeight::Directs {
            meta: meta(),
            data: DirectsData {
                details: "d".into(),
                officer_role: None,
            },
        };
        assert_eq!(edge_rel_table(&w), "directs");
    }

    #[test]
    fn test_edge_rel_table_located_at() {
        let w = EdgeWeight::LocatedAt {
            meta: meta(),
            data: LocatedAtData {
                details: "d".into(),
            },
        };
        assert_eq!(edge_rel_table(&w), "located_at");
    }

    #[test]
    fn test_edge_rel_table_awarded_to() {
        let w = EdgeWeight::AwardedTo {
            meta: meta(),
            data: AwardedToData {
                details: "d".into(),
            },
        };
        assert_eq!(edge_rel_table(&w), "awarded_to");
    }

    #[test]
    fn test_edge_rel_table_issued_by() {
        let w = EdgeWeight::IssuedBy {
            meta: meta(),
            data: IssuedByData {
                details: "d".into(),
            },
        };
        assert_eq!(edge_rel_table(&w), "issued_by");
    }

    #[test]
    fn test_edge_rel_table_represents() {
        let w = EdgeWeight::Represents {
            meta: meta(),
            data: RepresentsData {
                details: "d".into(),
            },
        };
        assert_eq!(edge_rel_table(&w), "represents");
    }

    // ── edge_data ─────────────────────────────────────────────────────────────

    #[test]
    fn test_edge_data_directs_has_officer_role() {
        let w = EdgeWeight::Directs {
            meta: meta(),
            data: DirectsData {
                details: "CH officer".into(),
                officer_role: Some("director".into()),
            },
        };
        let d = edge_data(&w);
        assert_eq!(d["data"]["details"].as_str(), Some("CH officer"));
        assert_eq!(d["data"]["officer_role"].as_str(), Some("director"));
        assert!(!d["meta"]["created_at"].is_null());
    }

    #[test]
    fn test_edge_data_awarded_to() {
        let w = EdgeWeight::AwardedTo {
            meta: meta(),
            data: AwardedToData {
                details: "award".into(),
            },
        };
        let d = edge_data(&w);
        assert_eq!(d["data"]["details"].as_str(), Some("award"));
    }

    // ── OSINT node_data tests (Task 1 RED) ────────────────────────────────────

    #[test]
    fn test_node_data_psc_fields() {
        use urban_integrity_schema::types::node::PscData;
        let w = NodeWeight::Psc(PscData {
            name: "Jane Doe".into(),
            company_number: Some("12345678".into()),
            natures_of_control: Some(vec!["ownership-of-shares-75-to-100-percent".into()]),
        });
        let d = node_data("psc:jane-doe", &w);
        assert_eq!(d["entity_id"].as_str(), Some("psc:jane-doe"));
        assert_eq!(d["name"].as_str(), Some("Jane Doe"));
        assert_eq!(d["company_number"].as_str(), Some("12345678"));
        assert!(
            d["natures_of_control"].is_array(),
            "natures_of_control must be an array"
        );
    }

    #[test]
    fn test_node_data_charity_fields() {
        use urban_integrity_schema::types::node::CharityData;
        let w = NodeWeight::Charity(CharityData {
            name: "Good Causes Foundation".into(),
            charity_number: Some("1234567".into()),
            registered_name: Some("Good Causes Foundation CIO".into()),
        });
        let d = node_data("charity:good-causes", &w);
        assert_eq!(d["entity_id"].as_str(), Some("charity:good-causes"));
        assert_eq!(d["name"].as_str(), Some("Good Causes Foundation"));
        assert_eq!(d["charity_number"].as_str(), Some("1234567"));
    }

    #[test]
    fn test_node_data_property_fields() {
        use urban_integrity_schema::types::node::PropertyData;
        let w = NodeWeight::Property(PropertyData {
            title_number: Some("LN1234".into()),
            tenure: Some("Freehold".into()),
            price_paid: Some(350000.0),
            company_number: Some("12345678".into()),
            country_incorporated: None,
        });
        let d = node_data("property:ln1234", &w);
        assert_eq!(d["entity_id"].as_str(), Some("property:ln1234"));
        assert_eq!(d["title_number"].as_str(), Some("LN1234"));
        assert_eq!(d["tenure"].as_str(), Some("Freehold"));
        assert!((d["price_paid"].as_f64().unwrap() - 350000.0).abs() < 1e-6);
        // PropertyData has no name field — must not appear
        assert!(d.get("name").is_none() || d["name"].is_null());
    }

    #[test]
    fn test_node_data_offshore_entity_fields() {
        use urban_integrity_schema::types::node::OffshoreEntityData;
        let w = NodeWeight::OffshoreEntity(OffshoreEntityData {
            name: "Acme Offshore Ltd".into(),
            jurisdiction: Some("British Virgin Islands".into()),
            icij_node_id: Some("abc123def456".into()),
        });
        let d = node_data("offshore_entity:abc123def456", &w);
        assert_eq!(
            d["entity_id"].as_str(),
            Some("offshore_entity:abc123def456")
        );
        assert_eq!(d["name"].as_str(), Some("Acme Offshore Ltd"));
        assert_eq!(d["jurisdiction"].as_str(), Some("British Virgin Islands"));
        assert_eq!(d["icij_node_id"].as_str(), Some("abc123def456"));
    }

    // ── OSINT edge_data tests (Task 1 RED) ────────────────────────────────────

    #[test]
    fn test_edge_data_controls_has_meta() {
        let w = EdgeWeight::Controls { meta: meta() };
        let d = edge_data(&w);
        assert!(
            !d["meta"]["created_at"].is_null(),
            "meta.created_at must be present"
        );
    }

    #[test]
    fn test_edge_data_trustee_of_has_meta() {
        let w = EdgeWeight::TrusteeOf { meta: meta() };
        let d = edge_data(&w);
        assert!(
            !d["meta"]["created_at"].is_null(),
            "meta.created_at must be present"
        );
    }

    #[test]
    fn test_edge_data_owns_property_fields() {
        use urban_integrity_schema::types::edge::OwnsPropertyData;
        let w = EdgeWeight::OwnsProperty {
            meta: meta(),
            data: OwnsPropertyData {
                price_paid: Some(450000.0),
                tenure: Some("Leasehold".into()),
            },
        };
        let d = edge_data(&w);
        assert!(
            !d["meta"]["created_at"].is_null(),
            "meta.created_at must be present"
        );
        assert!((d["data"]["price_paid"].as_f64().unwrap() - 450000.0).abs() < 1e-6);
        assert_eq!(d["data"]["tenure"].as_str(), Some("Leasehold"));
    }

    #[test]
    fn test_edge_data_offshore_link_has_meta() {
        let w = EdgeWeight::OffshoreLink { meta: meta() };
        let d = edge_data(&w);
        assert!(
            !d["meta"]["created_at"].is_null(),
            "meta.created_at must be present"
        );
    }

    #[test]
    fn test_edge_data_donates_to_has_meta() {
        let w = EdgeWeight::DonatesTo { meta: meta() };
        let d = edge_data(&w);
        assert!(
            !d["meta"]["created_at"].is_null(),
            "meta.created_at must be present"
        );
    }

    #[test]
    fn test_edge_data_disqualified_from_fields() {
        use urban_integrity_schema::types::edge::DisqualifiedFromData;
        let disq_from = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let w = EdgeWeight::DisqualifiedFrom {
            meta: meta(),
            data: DisqualifiedFromData {
                disqualified_from: Some(disq_from),
                disqualified_until: None,
                case_number: Some("CASE-001".into()),
            },
        };
        let d = edge_data(&w);
        assert!(
            !d["meta"]["created_at"].is_null(),
            "meta.created_at must be present"
        );
        // data.disqualified_from must be present as a string
        assert!(
            d["data"]["disqualified_from"].is_string(),
            "data.disqualified_from must be a string"
        );
        assert_eq!(d["data"]["case_number"].as_str(), Some("CASE-001"));
    }

    // ── Phase 30: contract projection field completeness ──────────────────────
    //
    // The defect these pin: `node_data`'s Contract arm emitted 9 of the 12 fields
    // `SCHEMA_DEF` defines on `contract`, silently dropping the other 3. There is no
    // error path for this — the field simply never reaches the table and reads as
    // NONE forever. It cost a mis-scoped follow-up phase: 30,836 events carried a real
    // `application_received_date` while the projected table held 0, and the gap was
    // attributed to the scraper rather than to projection.

    fn contract_weight_all_fields() -> NodeWeight {
        NodeWeight::Contract(ContractData {
            name: "Land at Newry Street".into(),
            ocid: Some("ocds-b5fd17-abc".into()),
            value_gbp: Some(1234.5),
            status: Some("active".into()),
            date_published: Some("2026-01-02".into()),
            ward_code: Some("W05001504".into()),
            tender_ref: Some("CF-0001-0002".into()),
            application_ref: Some("30C304".into()),
            application_received_date: Some(chrono::NaiveDate::from_ymd_opt(1991, 12, 18).unwrap()),
            applicant_provenance: Some(
                urban_integrity_schema::types::node::ApplicantProvenance::Published,
            ),
            portal_detail_handle: Some("a0X1234567ABCDE".into()),
        })
    }

    #[test]
    fn contract_projection_carries_received_date_provenance_and_handle() {
        let d = node_data("planning:30C304", &contract_weight_all_fields());

        // `application_received_date` is a chrono::NaiveDate in the weight but
        // `option<string>` in SCHEMA_DEF, so it must serialise as a plain YYYY-MM-DD
        // string — matching the Entity arm's `incorporation_date` idiom. Handing
        // SurrealDB a shape its schema won't coerce fails the row silently.
        assert_eq!(
            d["application_received_date"].as_str(),
            Some("1991-12-18"),
            "application_received_date must project as a YYYY-MM-DD string"
        );
        // ApplicantProvenance is `#[serde(rename_all = \"snake_case\")]`.
        assert_eq!(
            d["applicant_provenance"].as_str(),
            Some("published"),
            "applicant_provenance must project as its snake_case serde form"
        );
        assert_eq!(
            d["portal_detail_handle"].as_str(),
            Some("a0X1234567ABCDE"),
            "portal_detail_handle must project"
        );
    }

    /// `tests/projector_schema_parity.rs` proves the KEY is emitted; these prove the VALUE
    /// survives. A `status_date()` that always returned `None`, or a mapper that wrote a null
    /// literal, would satisfy parity and still lose every date.
    #[test]
    fn entity_status_date_projects_the_dissolution_instant_as_rfc3339() {
        let at = chrono::DateTime::parse_from_rfc3339("2021-07-14T09:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let weight = NodeWeight::Entity(EntityData {
            name: "Gone Ltd".into(),
            company_number: Some("12345678".into()),
            status: EntityStatus::Dissolved(at),
            incorporation_date: None,
        });

        let d = node_data("entity:12345678", &weight);
        assert_eq!(d["entity_status"].as_str(), Some("dissolved"));
        assert_eq!(
            d["entity_status_date"].as_str(),
            Some("2021-07-14T09:30:00+00:00"),
            "the date EntityStatus carries must reach the projected row, not just the discriminant"
        );
    }

    #[test]
    fn entity_status_date_is_null_for_an_active_company() {
        let weight = NodeWeight::Entity(EntityData {
            name: "Apex Build Ltd".into(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });

        let d = node_data("entity:1", &weight);
        assert_eq!(d["entity_status"].as_str(), Some("active"));
        assert!(
            d["entity_status_date"].is_null(),
            "an active company has no terminal date; got {:?}",
            d["entity_status_date"]
        );
    }

    /// The OCOD/CCOD split on a projected row. `producer_of_property` keys on exactly this
    /// field's presence, so a null here collapses both HMLR datasets into one.
    #[test]
    fn property_country_incorporated_projects_and_separates_ocod_from_ccod() {
        use urban_integrity_schema::types::node::PropertyData;

        let ocod = NodeWeight::Property(PropertyData {
            title_number: Some("LN9999".into()),
            tenure: Some("Freehold".into()),
            price_paid: None,
            company_number: None,
            country_incorporated: Some("Jersey".into()),
        });
        let ccod = NodeWeight::Property(PropertyData {
            title_number: Some("CY1234".into()),
            tenure: Some("Leasehold".into()),
            price_paid: None,
            company_number: Some("07654321".into()),
            country_incorporated: None,
        });

        assert_eq!(
            node_data("property:LN9999", &ocod)["country_incorporated"].as_str(),
            Some("Jersey")
        );
        assert!(
            node_data("property:CY1234", &ccod)["country_incorporated"].is_null(),
            "a CCOD title must project a null country, not an empty string"
        );
    }

    /// Structural guard against the whole defect class, not just the three fields
    /// that happened to be missing: every field `SCHEMA_DEF` declares on `contract`
    /// must appear in the projected object. Adding a schema field without projecting
    /// it now fails here instead of silently reading NONE in production.
    ///
    /// Now generalised to every node table by `tests/projector_schema_parity.rs`; kept
    /// because it asserts against a fully-populated `ContractData` rather than defaults.
    #[test]
    fn every_schema_defined_contract_field_is_projected() {
        let projected = node_data("planning:30C304", &contract_weight_all_fields());
        let obj = projected
            .as_object()
            .expect("node_data must produce a JSON object");

        let mut missing: Vec<&str> = Vec::new();
        for line in urban_integrity_schema::db::SCHEMA_DEF.lines() {
            let line = line.trim();
            // e.g. `DEFINE FIELD IF NOT EXISTS ward_code ON contract TYPE option<string>;`
            let Some(rest) = line.strip_prefix("DEFINE FIELD IF NOT EXISTS ") else {
                continue;
            };
            let mut parts = rest.split_whitespace();
            let (Some(field), Some("ON"), Some("contract")) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if !obj.contains_key(field) {
                missing.push(field);
            }
        }

        assert!(
            missing.is_empty(),
            "SCHEMA_DEF defines these `contract` fields that node_data never projects, \
             so they silently read as NONE forever: {missing:?}"
        );
    }
}
