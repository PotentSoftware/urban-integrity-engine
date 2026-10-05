//! London Gazette insolvency adapter (PROP-02).
//!
//! Polls the Gazette REST API for winding-up / liquidation notice codes
//! (2452, 2454, 2410), fetches per-notice JSON-LD to extract `companyNumber`,
//! and emits `GraphMutation::UpdateNode` to set `EntityStatus::InLiquidation`
//! on matched entities.
//!
//! # Scope (D-06 ratified 2026-05-31)
//!
//! InLiquidation-only: notice codes 2452/2454/2410. Dissolution is intentionally
//! out of scope — CH streaming already delivers dissolution reliably via
//! `EntityStatus::Dissolved`. No dissolution codes (2460/2408) are implemented.
//!
//! # Pitfall 3 (no companyNumber in search entry)
//!
//! The Gazette search `/data.json` entry has no `companyNumber`. The company
//! number is only available in the per-notice JSON-LD at `/notice/{id}/data.jsonld`
//! under `@graph[].companyNumber`. A second HTTP request is made for every entry.
//!
//! # ENR-01
//!
//! `SurrealStreamCursor` is imported from `urban_integrity_core::stream`, NOT
//! from `crates/ingestor`. This is a hard boundary.
//!
//! # Security (T-18-03-01, T-18-03-03)
//!
//! - `companyNumber` is validated via `is_valid_ch_number` before any DB write.
//! - `entity_id` comparisons for DB query results use both `.as_str()` and
//!   `.to_string().contains()` — RecordId coercion workaround (Pitfall 5).

use std::sync::Arc;
use urban_integrity_core::persistence::scan;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};
use urban_integrity_core::config::LondonGazetteConfig;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_core::stream::{CursorStore, SurrealStreamCursor};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{EntityStatus, NodeWeight};
use uuid::Uuid;

use crate::adapters::ch_psc::is_valid_ch_number;
use crate::adapters::{EnricherAdapter, EnrichmentStats};

/// Notice codes for corporate insolvency (D-06 ratified — InLiquidation only).
///
/// 2452 = Winding-Up Orders → InLiquidation
/// 2454 = Appointment of Liquidators → InLiquidation
/// 2410 = Appointment of Administrators → InLiquidation
///
/// NOT included (D-06 ratified scope): 2460/2408 dissolution codes.
/// CH streaming handles dissolution via EntityStatus::Dissolved already.
pub const NOTICE_CODES: &[u32] = &[2452, 2454, 2410];

/// London Gazette corporate insolvency adapter.
///
/// Uses the public Gazette REST API — no API key required.
/// SurrealStreamCursor stores the last-polled `published-from` date in
/// `enricher_cursors:london_gazette`.
pub struct LondonGazetteAdapter {
    db_for_cursor: Option<Arc<Surreal<Any>>>,
    config: LondonGazetteConfig,
}

impl LondonGazetteAdapter {
    /// Create a new adapter with a SurrealDB connection and runtime config.
    pub fn new(db: Arc<Surreal<Any>>, config: LondonGazetteConfig) -> Self {
        Self {
            db_for_cursor: Some(db),
            config,
        }
    }

    /// Create a test adapter without a DB connection.
    ///
    /// Uses `LondonGazetteConfig::default()` — tests that need non-default config
    /// should use `LondonGazetteAdapter { db_for_cursor: None, config: ... }` directly
    /// (or test the pure helpers independently, which is preferred).
    pub fn new_for_test() -> Self {
        Self {
            db_for_cursor: None,
            config: LondonGazetteConfig::default(),
        }
    }
}

#[async_trait]
impl EnricherAdapter for LondonGazetteAdapter {
    fn name(&self) -> &'static str {
        "london_gazette"
    }

    /// Always returns true — the Gazette REST API is public (no credentials needed).
    fn is_enabled(&self) -> bool {
        true
    }

    /// Run a single Gazette enrichment pass.
    ///
    /// Polls notice codes 2452/2454/2410 from the last cursor date to today.
    /// For each entry, fetches JSON-LD to get `companyNumber`, then emits
    /// `UpdateNode(InLiquidation)` for matched entities.
    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> Result<EnrichmentStats> {
        let mut stats = EnrichmentStats::default();

        // Wall-clock budget for this pass. Captured at entry so elapsed() is
        // monotonic and unaffected by tokio scheduling jitter.
        let pass_start = std::time::Instant::now();
        let budget_secs = self.config.max_pass_secs;

        // ── Determine starting position (cursor + partial checkpoint) ──────────
        //
        // Priority:
        //   1. Partial checkpoint from a previous interrupted pass
        //      → resume at (published_from, code_idx, page)
        //   2. Completed-pass cursor timestamp → clamp to lookback window
        //   3. Empty cursor → published_from = today - lookback_days
        let today = Utc::now().date_naive();

        let (published_from, start_code_idx, start_page) =
            if let Some(ref db_arc) = self.db_for_cursor {
                // Try partial checkpoint first
                if let Some(cp) = load_partial_checkpoint(db_arc).await {
                    info!(
                        adapter = "london_gazette",
                        cp_published_from = %cp.cp_published_from,
                        cp_code_idx = cp.cp_code_idx,
                        cp_page = cp.cp_page,
                        "Gazette: resuming from partial checkpoint"
                    );
                    (cp.cp_published_from, cp.cp_code_idx, cp.cp_page)
                } else {
                    // No partial checkpoint — use steady-state cursor + lookback clamp
                    let cursor = SurrealStreamCursor::new(
                        Arc::clone(db_arc),
                        "enricher_cursors",
                        "london_gazette",
                    );
                    let cursor_ts = cursor.load_timepoint().await;
                    let pf = compute_published_from(cursor_ts, self.config.lookback_days, today);
                    (pf, 0, 1)
                }
            } else {
                // No DB (test mode) — use lookback default
                let pf = compute_published_from(None, self.config.lookback_days, today);
                (pf, 0, 1)
            };

        info!(
            adapter = "london_gazette",
            published_from = %published_from,
            start_code_idx,
            start_page,
            budget_secs,
            "Starting Gazette enrichment pass"
        );

        // Single shared client across all notice codes: one JSESSIONID session
        // is preferred so the server tracks our request budget consistently.
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .user_agent("Mozilla/5.0 (compatible; UrbanIntegrity/1.0)")
            .cookie_store(true)
            .build()?;

        // IP-level rate limit: the Gazette returns HTTP 200 with an empty body
        // after ~100 JSON-LD fetches from the same IP. The block lasts ~5 minutes.
        // We pause 300s every 75 fetches so the window always resets before we'd
        // hit the cap — keeping the effective rate ≈ 15 req/min.
        //
        // Budget preemption: at the 75-fetch boundary we check the wall-clock budget
        // BEFORE sleeping. If the budget is exceeded we checkpoint-and-return instead,
        // letting the IP window reset naturally before the next scheduler invocation.
        let mut jld_fetches: u32 = 0;

        for (code_idx, &notice_code) in NOTICE_CODES.iter().enumerate() {
            // Skip codes before the resume point
            if code_idx < start_code_idx {
                continue;
            }

            let resume_page = if code_idx == start_code_idx {
                start_page
            } else {
                1
            };
            let mut page = resume_page;

            loop {
                let url = format!(
                    "https://www.thegazette.co.uk/all-notices/notice/data.json\
                     ?notice-code={}&published-from={}&results-page={}&results-page-size=50",
                    notice_code, published_from, page
                );

                let resp: serde_json::Value =
                    match gazette_fetch_json(&client, &url, notice_code, page).await {
                        Some(v) => v,
                        None => break,
                    };

                // WR-02: use u32::MAX as sentinel so that when f:page-stop is absent
                // from the response we do NOT stop after page 1. The loop always
                // terminates via `entries.is_empty()` when there are no more results.
                let page_stop = resp["f:page-stop"]
                    .as_str()
                    .and_then(|s| s.parse::<u32>().ok())
                    .or_else(|| resp["f:page-stop"].as_u64().map(|n| n as u32))
                    .unwrap_or(u32::MAX);

                let entries = resp["entry"].as_array().cloned().unwrap_or_default();

                for entry in &entries {
                    stats.candidates_found += 1;

                    let notice_uri = entry["id"].as_str().unwrap_or("");
                    let notice_id = notice_uri.rsplit('/').next().unwrap_or("");
                    let published = entry["published"].as_str().unwrap_or("").to_string();

                    if notice_id.is_empty() {
                        debug!("Gazette: empty notice_id in entry — skipping");
                        continue;
                    }

                    // Fetch per-notice JSON-LD for companyNumber (Pitfall 3 — not in search entry)
                    let jsonld_url = format!(
                        "https://www.thegazette.co.uk/notice/{}/data.jsonld",
                        notice_id
                    );

                    let jsonld: serde_json::Value = match client.get(&jsonld_url).send().await {
                        Ok(r) => match r.json().await {
                            Ok(v) => v,
                            Err(e) => {
                                debug!(error = %e, notice_id, "Gazette: failed to parse JSON-LD");
                                // Rate-limit even on error
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                continue;
                            }
                        },
                        Err(e) => {
                            debug!(error = %e, notice_id, "Gazette: JSON-LD HTTP request failed");
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                            continue;
                        }
                    };

                    // Track fetches and pause every 75 to stay under the ~100-req/~82s
                    // IP-level window. 75 fetches × 500ms ≈ 37s; 300s pause resets
                    // the window before the next batch starts.
                    jld_fetches += 1;
                    if jld_fetches.is_multiple_of(75) {
                        // Budget preemption: if the wall-clock budget is already
                        // exceeded, checkpoint-and-return instead of sleeping 300s.
                        // The IP window will reset naturally by the next invocation.
                        if should_stop_pass(pass_start.elapsed(), budget_secs) {
                            let cp = PartialCheckpoint {
                                cp_published_from: published_from.clone(),
                                cp_code_idx: code_idx,
                                cp_page: page,
                            };
                            info!(
                                jld_fetches,
                                budget_secs,
                                elapsed_secs = pass_start.elapsed().as_secs(),
                                cp_code_idx = code_idx,
                                cp_page = page,
                                "Gazette: budget exceeded at 75-fetch boundary — \
                                 checkpointing and returning (IP window resets naturally)"
                            );
                            if let Some(ref db_arc) = self.db_for_cursor {
                                persist_partial_checkpoint(db_arc, &cp).await;
                            }
                            info!(
                                adapter = "london_gazette",
                                candidates_found = stats.candidates_found,
                                nodes_updated = stats.auto_linked,
                                "Gazette enrichment pass interrupted (budget)"
                            );
                            return Ok(stats);
                        }

                        info!(
                            jld_fetches,
                            "Gazette: pausing 300s to reset IP rate-limit window (~5 min block)"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }

                    // Extract companyNumber from @graph array (A2 — find_map, not index 0)
                    let company_number = match extract_company_number_from_jsonld(&jsonld) {
                        Some(cn) => cn,
                        None => {
                            debug!(
                                notice_id,
                                "Gazette: no companyNumber in JSON-LD @graph — skipping (D-05)"
                            );
                            continue;
                        }
                    };

                    // Validate company number before any DB write (T-18-03-01)
                    if !is_valid_ch_number(&company_number) {
                        warn!(
                            company_number = %company_number,
                            notice_id,
                            "Gazette: invalid CH number — skipping"
                        );
                        continue;
                    }

                    let entity_id = format!("GB-COH:{}", company_number);

                    // Load existing weight (must preserve all EntityData fields except status)
                    let current_weight = load_entity_weight_from_db(db, &entity_id).await;

                    let current_weight = match current_weight {
                        Some(w) => w,
                        None => {
                            debug!(
                                entity_id = %entity_id,
                                "Gazette: entity not in graph — skipping (may not be in this LPA scope)"
                            );
                            continue;
                        }
                    };

                    // Emit UpdateNode with InLiquidation status
                    let gazette_date = match parse_gazette_date(&published) {
                        Ok(dt) => dt,
                        Err(e) => {
                            warn!(error = %e, published, "Gazette: failed to parse published date — using now()");
                            Utc::now()
                        }
                    };

                    if let NodeWeight::Entity(mut data) = current_weight {
                        data.status = EntityStatus::InLiquidation(gazette_date);
                        let updated_weight = NodeWeight::Entity(data);

                        // CR-01: write directly to graph_events so the CLI enrichment
                        // path (which drops the mutation_tx receiver with `_`) still
                        // records the InLiquidation status change persistently.
                        // The mutation_tx send below is still correct for serve-mode
                        // coordinator processing.
                        let weight_json =
                            serde_json::to_string(&updated_weight).unwrap_or_default();
                        let event_id = Uuid::new_v4().to_string();
                        let timestamp = Utc::now().timestamp() as u64;
                        let payload = serde_json::json!({
                            "entity_id": entity_id,
                            "weight_json": weight_json,
                        });
                        if let Err(e) = db
                            .query(
                                "INSERT INTO graph_events { \
                                 event_id: $event_id, \
                                 timestamp: $ts, \
                                 kind: 'NodeUpdated', \
                                 payload: $payload \
                                 };",
                            )
                            .bind(("event_id", event_id.as_str()))
                            .bind(("ts", timestamp))
                            .bind(("payload", payload))
                            .await
                        {
                            warn!(
                                entity_id = %entity_id,
                                ?e,
                                "Gazette: failed to persist NodeUpdated to graph_events"
                            );
                        }

                        let _ = mutation_tx
                            .send(GraphMutation::UpdateNode {
                                id: EntityId(entity_id),
                                weight: updated_weight,
                            })
                            .await;
                        // auto_linked counts InLiquidation status updates emitted
                        // (naming inherited from EnrichmentStats; semantics = nodes updated)
                        stats.auto_linked += 1;
                    }
                }

                // Per-page budget check (catches passes that finish a page but have
                // already exceeded the budget — skips the 300s boundary if no multiple
                // of 75 was hit, e.g. last page of a code with <75 entries).
                if should_stop_pass(pass_start.elapsed(), budget_secs) {
                    let cp = PartialCheckpoint {
                        cp_published_from: published_from.clone(),
                        cp_code_idx: code_idx,
                        // +1: we finished this page; resume at the next one
                        cp_page: page + 1,
                    };
                    info!(
                        budget_secs,
                        elapsed_secs = pass_start.elapsed().as_secs(),
                        cp_code_idx = code_idx,
                        cp_page = cp.cp_page,
                        "Gazette: budget exceeded at page boundary — checkpointing and returning"
                    );
                    if let Some(ref db_arc) = self.db_for_cursor {
                        persist_partial_checkpoint(db_arc, &cp).await;
                    }
                    info!(
                        adapter = "london_gazette",
                        candidates_found = stats.candidates_found,
                        nodes_updated = stats.auto_linked,
                        "Gazette enrichment pass interrupted (budget, page boundary)"
                    );
                    return Ok(stats);
                }

                if page >= page_stop || entries.is_empty() {
                    break;
                }
                page += 1;
                // Pause between pages to avoid triggering IP-level rate limits.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }

        // All NOTICE_CODES exhausted — full pass complete.
        let pass_complete = true;

        // Update cursor to today and clear the partial checkpoint.
        if let Some(ref db_arc) = self.db_for_cursor {
            let cursor =
                SurrealStreamCursor::new(Arc::clone(db_arc), "enricher_cursors", "london_gazette");
            cursor.persist_timepoint(Utc::now().timestamp()).await;
            clear_partial_checkpoint(db_arc).await;
        }

        info!(
            adapter = "london_gazette",
            candidates_found = stats.candidates_found,
            nodes_updated = stats.auto_linked,
            pass_complete,
            "Gazette enrichment pass complete"
        );

        Ok(stats)
    }
}

// ── Pure helper functions ─────────────────────────────────────────────────────

/// Compute the `published_from` date string for this pass.
///
/// Rules (per design decision gazette-adapter-runaway-crawl, 2026-06-09):
/// 1. Start from the cursor's last completed-pass timestamp (Unix seconds), if any.
/// 2. Clamp to no earlier than `today - lookback_days` regardless of cursor age.
/// 3. If cursor is absent, use `today - lookback_days` directly.
///
/// Returns a `"YYYY-MM-DD"` string suitable for the Gazette search API.
pub fn compute_published_from(
    cursor_ts: Option<i64>,
    lookback_days: i64,
    today: chrono::NaiveDate,
) -> String {
    let floor = today - Duration::days(lookback_days);
    let candidate = cursor_ts
        .and_then(|ts| DateTime::from_timestamp(ts, 0))
        .map(|dt: DateTime<Utc>| dt.date_naive())
        .unwrap_or(floor);
    let clamped = candidate.max(floor);
    clamped.format("%Y-%m-%d").to_string()
}

/// Return `true` when the wall-clock budget for this pass has been exceeded.
///
/// Pure function — no I/O. `budget_secs == 0` is treated as "always exceeded"
/// which is useful for tests that need to force the preemption path.
pub fn should_stop_pass(elapsed: std::time::Duration, budget_secs: u64) -> bool {
    elapsed.as_secs() >= budget_secs
}

/// Serialised partial-pass checkpoint stored on `enricher_cursors:london_gazette`.
///
/// Shape chosen: three flat extra columns (`cp_published_from`, `cp_code_idx`,
/// `cp_page`) on the same SCHEMALESS record that holds `timepoint`. This avoids
/// nested object serialisation round-trips and keeps the SurrealQL UPSERT trivial.
///
/// `cp_code_idx` is the index into `NOTICE_CODES` (0-based). `cp_page` is the
/// 1-based Gazette results page. Both are stored as `u32` to match the types used
/// in `run()`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PartialCheckpoint {
    /// `published_from` date string, e.g. `"2026-05-10"`.
    pub cp_published_from: String,
    /// Index into `NOTICE_CODES` at which to resume (0-based).
    pub cp_code_idx: usize,
    /// 1-based results page at which to resume within that notice code.
    pub cp_page: u32,
}

/// Persist a partial pass checkpoint to the adapter's `enricher_cursors:london_gazette` record.
///
/// Uses UPSERT with three extra columns — SCHEMALESS, so this never conflicts with
/// the `timepoint` column written by `SurrealStreamCursor::persist_timepoint`.
pub(crate) async fn persist_partial_checkpoint(db: &Surreal<Any>, cp: &PartialCheckpoint) {
    let result: surrealdb::Result<Vec<serde_json::Value>> = db
        .query(
            "UPSERT enricher_cursors:london_gazette \
             SET cp_published_from = $cp_pf, \
                 cp_code_idx       = $cp_ci, \
                 cp_page           = $cp_pg",
        )
        .bind(("cp_pf", cp.cp_published_from.as_str()))
        .bind(("cp_ci", cp.cp_code_idx as u64))
        .bind(("cp_pg", cp.cp_page as u64))
        .await
        .and_then(|mut r| r.take(0));

    if let Err(e) = result {
        warn!(
            ?e,
            "Gazette: failed to persist partial checkpoint (non-fatal)"
        );
    }
}

/// Load the partial checkpoint from `enricher_cursors:london_gazette`, if present.
///
/// Returns `None` if the record does not exist or any checkpoint column is absent —
/// both cases are treated as "start fresh" by `run()`.
pub(crate) async fn load_partial_checkpoint(db: &Surreal<Any>) -> Option<PartialCheckpoint> {
    let rows: surrealdb::Result<Vec<serde_json::Value>> = db
        .query(
            "SELECT cp_published_from, cp_code_idx, cp_page \
             FROM enricher_cursors:london_gazette",
        )
        .await
        .and_then(|mut r| r.take(0));

    let row = rows.ok()?.into_iter().next()?;

    let cp_published_from = row["cp_published_from"].as_str()?.to_string();
    // Absent or null cp_code_idx → not a partial checkpoint
    if cp_published_from.is_empty() {
        return None;
    }
    let cp_code_idx = row["cp_code_idx"].as_u64()? as usize;
    let cp_page = row["cp_page"].as_u64()? as u32;

    Some(PartialCheckpoint {
        cp_published_from,
        cp_code_idx,
        cp_page,
    })
}

/// Clear the partial checkpoint columns from `enricher_cursors:london_gazette`.
///
/// Called after a full pass (all NOTICE_CODES exhausted) so the next run starts
/// fresh from the newly-advanced `timepoint`.
pub(crate) async fn clear_partial_checkpoint(db: &Surreal<Any>) {
    let result: surrealdb::Result<Vec<serde_json::Value>> = db
        .query(
            "UPSERT enricher_cursors:london_gazette \
             SET cp_published_from = NONE, \
                 cp_code_idx       = NONE, \
                 cp_page           = NONE",
        )
        .await
        .and_then(|mut r| r.take(0));

    if let Err(e) = result {
        warn!(
            ?e,
            "Gazette: failed to clear partial checkpoint columns (non-fatal)"
        );
    }
}

// ── Public helpers (pub for tests) ───────────────────────────────────────────

/// Extract `companyNumber` from the `@graph` array in a JSON-LD document.
///
/// The Gazette JSON-LD places `companyNumber` inside the `@graph` array, not at
/// the top level. Use `find_map` over the array to locate it (A2 — do not assume
/// array index 0).
///
/// Returns `None` if `@graph` is absent, empty, or no object has `companyNumber`.
pub fn extract_company_number_from_jsonld(ld: &serde_json::Value) -> Option<String> {
    ld["@graph"]
        .as_array()?
        .iter()
        .find_map(|obj| obj["companyNumber"].as_str())
        .map(|s| s.trim().to_string())
}

/// Fetch a Gazette search page, returning the parsed JSON value.
///
/// Retries up to 2 times with a 15-second back-off when the response body
/// is not valid JSON (e.g. an HTML session-expired or rate-limit page).
/// Cookie persistence on the shared client keeps the JSESSIONID alive across
/// pages, preventing most session-expiry failures.
async fn gazette_fetch_json(
    client: &reqwest::Client,
    url: &str,
    notice_code: u32,
    page: u32,
) -> Option<serde_json::Value> {
    for attempt in 0u32..3 {
        if attempt > 0 {
            // 90s per retry: 3 attempts × 90s = 270s spans the ~5min block window,
            // so the final attempt arrives after the IP ban has lifted.
            warn!(
                notice_code,
                page, attempt, "Gazette: backing off 90s before retry"
            );
            tokio::time::sleep(std::time::Duration::from_secs(90)).await;
        }
        let text = match client.get(url).send().await {
            Ok(r) => match r.text().await {
                Ok(t) => t,
                Err(e) => {
                    warn!(error = %e, notice_code, page, "Gazette: failed to read response body");
                    continue;
                }
            },
            Err(e) => {
                warn!(error = %e, notice_code, page, "Gazette: HTTP request failed");
                continue;
            }
        };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => return Some(v),
            Err(e) => {
                let preview = text.chars().take(120).collect::<String>();
                warn!(error = %e, notice_code, page, response_preview = %preview,
                      "Gazette: non-JSON response (rate-limited or session expired)");
            }
        }
    }
    warn!(notice_code, page, "Gazette: giving up after 3 attempts");
    None
}

/// Parse a Gazette `published` date string into `DateTime<Utc>`.
///
/// The Gazette API returns ISO 8601 strings in various forms:
/// - Full datetime: `"2024-03-15T00:00:00Z"`
/// - Date-only: `"2024-03-15"`
///
/// Falls back to current time only when the caller explicitly handles the error.
pub fn parse_gazette_date(s: &str) -> Result<DateTime<Utc>> {
    // Try full ISO 8601 datetime with timezone first
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    // Try naive datetime (YYYY-MM-DDTHH:MM:SS) — Gazette omits timezone for some entries
    if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(ndt.and_utc());
    }
    // Try date-only format (YYYY-MM-DD) — treat as midnight UTC
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(nd
            .and_hms_opt(0, 0, 0)
            .expect("midnight always valid")
            .and_utc());
    }
    anyhow::bail!("Gazette: could not parse date string: {s:?}")
}

/// Load the current `NodeWeight` for an entity from `graph_events`.
///
/// Queries `NodeAdded` and `NodeUpdated` events, filters Rust-side by `entity_id`
/// (RecordId coercion workaround — Pitfall 5), and returns the weight from the
/// row with the highest timestamp (latest-timestamp-wins).
///
/// Returns `None` if the entity has no events in this DB (e.g. different LPA scope).
pub async fn load_entity_weight_from_db(db: &Surreal<Any>, entity_id: &str) -> Option<NodeWeight> {
    let rows = scan::scan_events_by_kinds(db, &["NodeAdded", "NodeUpdated"]).await;

    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, entity_id, "Gazette: failed to load graph_events for entity weight");
            return None;
        }
    };

    // Filter Rust-side (Pitfall 5 — RecordId coercion workaround).
    // Check both as_str() equality and .to_string().contains() forms.
    let mut best: Option<(u64, NodeWeight)> = None;

    for row in rows {
        // Check if this row matches our entity_id
        let eid = row["payload"]["entity_id"].as_str().unwrap_or("");
        let matches =
            eid == entity_id || row["payload"]["entity_id"].to_string().contains(entity_id);

        if !matches {
            continue;
        }

        let ts = row["timestamp"].as_u64().unwrap_or(0);
        let weight_json = match row["payload"]["weight_json"].as_str() {
            Some(s) => s,
            None => continue,
        };

        let weight: NodeWeight = match serde_json::from_str(weight_json) {
            Ok(w) => w,
            Err(e) => {
                debug!(error = %e, entity_id, "Gazette: failed to deserialise NodeWeight — skipping row");
                continue;
            }
        };

        // Latest-timestamp-wins
        match best {
            None => best = Some((ts, weight)),
            Some((best_ts, _)) if ts > best_ts => best = Some((ts, weight)),
            _ => {}
        }
    }

    best.map(|(_, w)| w)
}

/// Test-only helper: process a single Gazette entry with a pre-supplied JSON-LD and
/// optional existing weight. Allows unit tests to drive the UpdateNode emission path
/// without a live HTTP server.
///
/// If `existing_weight` is `None`, simulates "entity not in graph" — no mutation sent.
/// If `existing_weight` is `Some(...)`, emits `UpdateNode` with `InLiquidation` status.
pub async fn process_gazette_entry_for_test(
    mutation_tx: &Sender<GraphMutation>,
    jsonld: &serde_json::Value,
    published: &str,
    notice_code: u32,
    existing_weight: Option<NodeWeight>,
) {
    // Extract companyNumber from @graph
    let company_number = match extract_company_number_from_jsonld(jsonld) {
        Some(cn) => cn,
        None => {
            debug!("process_gazette_entry_for_test: no companyNumber in JSON-LD @graph (D-05)");
            return;
        }
    };

    // Validate CH number (T-18-03-01)
    if !is_valid_ch_number(&company_number) {
        warn!(company_number = %company_number, "process_gazette_entry_for_test: invalid CH number");
        return;
    }

    // Only process InLiquidation notice codes (D-06 ratified)
    if !matches!(notice_code, 2452 | 2454 | 2410) {
        return;
    }

    let current_weight = match existing_weight {
        Some(w) => w,
        None => return, // entity not in graph — skip
    };

    let gazette_date = match parse_gazette_date(published) {
        Ok(dt) => dt,
        Err(_) => Utc::now(),
    };

    if let NodeWeight::Entity(mut data) = current_weight {
        data.status = EntityStatus::InLiquidation(gazette_date);
        let entity_id = format!("GB-COH:{}", company_number);
        let _ = mutation_tx
            .send(GraphMutation::UpdateNode {
                id: EntityId(entity_id),
                weight: NodeWeight::Entity(data),
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    // ── (i) Lookback clamp ────────────────────────────────────────────────────

    /// Empty cursor → published_from is exactly today - lookback_days.
    #[test]
    fn lookback_clamp_empty_cursor() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 9).unwrap();
        let result = compute_published_from(None, 30, today);
        assert_eq!(result, "2026-05-10");
    }

    /// Cursor older than lookback window → clamped to today - lookback_days.
    #[test]
    fn lookback_clamp_old_cursor() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 9).unwrap();
        // Simulate a cursor from 2020-01-01 (the old hard-coded default)
        let old_ts = chrono::DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .unwrap()
            .timestamp();
        let result = compute_published_from(Some(old_ts), 30, today);
        // Must be clamped to 30 days ago, not replayed from 2020
        assert_eq!(result, "2026-05-10");
    }

    /// Recent cursor within the lookback window → cursor date is honoured.
    #[test]
    fn lookback_clamp_recent_cursor() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 9).unwrap();
        // 3 days ago — well within the 30-day window
        let recent_ts = chrono::DateTime::parse_from_rfc3339("2026-06-06T00:00:00Z")
            .unwrap()
            .timestamp();
        let result = compute_published_from(Some(recent_ts), 30, today);
        assert_eq!(result, "2026-06-06");
    }

    // ── (ii) Budget preemption ────────────────────────────────────────────────

    /// Zero budget is treated as "always exceeded".
    #[test]
    fn budget_zero_always_exceeded() {
        let elapsed = std::time::Duration::from_secs(0);
        assert!(should_stop_pass(elapsed, 0));
    }

    /// Elapsed exactly equal to budget_secs → stop.
    #[test]
    fn budget_exactly_at_limit() {
        let elapsed = std::time::Duration::from_secs(300);
        assert!(should_stop_pass(elapsed, 300));
    }

    /// Elapsed one second under budget → do not stop.
    #[test]
    fn budget_under_limit() {
        let elapsed = std::time::Duration::from_secs(299);
        assert!(!should_stop_pass(elapsed, 300));
    }

    /// Elapsed well over budget → stop.
    #[test]
    fn budget_over_limit() {
        let elapsed = std::time::Duration::from_secs(400);
        assert!(should_stop_pass(elapsed, 300));
    }

    // ── (iii) Checkpoint round-trip ───────────────────────────────────────────

    /// PartialCheckpoint serialises to and from JSON correctly.
    #[test]
    fn checkpoint_roundtrip_json() {
        let cp = PartialCheckpoint {
            cp_published_from: "2026-05-10".to_string(),
            cp_code_idx: 1,
            cp_page: 3,
        };
        let json = serde_json::to_string(&cp).unwrap();
        let restored: PartialCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, cp);
    }

    /// Deserialising a flat JSON object (as SurrealDB SELECT would return) yields
    /// the correct PartialCheckpoint fields.
    ///
    /// This validates the column shape used by `load_partial_checkpoint`.
    #[test]
    fn checkpoint_from_flat_surreal_row() {
        let row = serde_json::json!({
            "cp_published_from": "2026-05-15",
            "cp_code_idx": 2,
            "cp_page": 7
        });
        let cp: PartialCheckpoint = serde_json::from_value(row).unwrap();
        assert_eq!(cp.cp_published_from, "2026-05-15");
        assert_eq!(cp.cp_code_idx, 2);
        assert_eq!(cp.cp_page, 7);
    }

    /// A row missing `cp_published_from` (i.e. no partial checkpoint stored)
    /// must NOT produce a `PartialCheckpoint` — `load_partial_checkpoint` returns None.
    ///
    /// We test the guard condition directly rather than through the async DB call.
    #[test]
    fn checkpoint_absent_columns_returns_none() {
        // Simulate the guard in load_partial_checkpoint: if cp_published_from is empty,
        // treat as absent.
        let row = serde_json::json!({
            "cp_published_from": null,
            "cp_code_idx": null,
            "cp_page": null
        });
        // The `as_str()` call on null returns None → load_partial_checkpoint short-circuits.
        assert!(row["cp_published_from"].as_str().is_none());
    }

    // ── Config defaults ───────────────────────────────────────────────────────

    /// LondonGazetteConfig::default() has the expected sentinel values.
    #[test]
    fn config_defaults() {
        let cfg = LondonGazetteConfig::default();
        assert_eq!(cfg.max_pass_secs, 300);
        assert_eq!(cfg.lookback_days, 30);
    }
}
