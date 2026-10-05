//! Land Registry CCOD (Commercial and Corporate Ownership Data) adapter (PROP-01, PROP-04).
//!
//! Downloads the monthly CCOD bulk ZIP from the HMLR "Use Land and Property Data" service,
//! streams the CSV with bounded memory using `spawn_blocking` + `csv::Reader`, filters rows
//! by matching `company_number` values, and emits `Property` nodes + `OwnsProperty` edges
//! via `mutation_tx`.
//!
//! Idempotent UPSERT of `identity_link`s via the subquery UPDATE guard (ENR-02).
//!
//! # Security
//! - T-18-02-01: `api_key` is never logged — only `api_key_present = true/false`.
//! - T-18-02-02: ZIP-bomb DoS guard. Content-Length > 2 GiB → WARN + skip, but that header is
//!   advisory (absent under chunked encoding, and server-supplied); the binding guard is the
//!   in-flight byte counter that aborts the download once 2 GiB have actually been received.
//! - T-18-02-03: `company_number` validated via `is_valid_ch_number` before any DB write.
//! - T-18-02-04: `evidence_json` is always a flat `String` — never a nested `Value` (ENR-06).

use std::collections::HashSet;
use std::io::BufReader;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, OwnsPropertyData};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{NodeWeight, PropertyData};

use crate::adapters::ch_psc::{is_valid_ch_number, write_identity_link};
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{IdentityLinkRecord, LinkStatus};

/// HMLR "Use Land and Property Data" API endpoint.
const CCOD_API_URL: &str = "https://use-land-property-data.service.gov.uk/api/v1/datasets/ccod";

/// Maximum ZIP file size (2 GiB) — T-18-02-02 DoS guard.
const MAX_ZIP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// CCOD proprietor slot indices — 4 slots per row (CCOD tech-spec, Q1, Pitfall 2).
const PROPRIETOR_SLOTS: &[usize] = &[1, 2, 3, 4];

/// A parsed CCOD row entry — one per non-empty proprietor slot.
#[derive(Debug, Clone)]
pub struct CcodRecord {
    pub title_number: String,
    pub tenure: String,
    pub price_paid: Option<f64>,
    pub company_number: String,
    pub proprietor_slot: usize,
    pub date_proprietor_added: Option<chrono::DateTime<Utc>>,
}

/// Look up a field by header name within a `csv::StringRecord`.
///
/// `csv::StringRecord` is positional; the header record provides the name → index map.
pub fn get_field_by_name<'a>(
    record: &'a csv::StringRecord,
    headers: &csv::StringRecord,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .position(|h| h == name)
        .and_then(|i| record.get(i))
}

/// Parse a CCOD "DD-MM-YYYY" date string into a UTC midnight `DateTime<Utc>`.
pub fn parse_ccod_date(s: &str) -> Option<chrono::DateTime<Utc>> {
    NaiveDate::parse_from_str(s.trim(), "%d-%m-%Y")
        .ok()
        .and_then(|d| {
            d.and_hms_opt(0, 0, 0)
                .map(|ndt| Utc.from_utc_datetime(&ndt))
        })
}

/// Extract the most recent bulk file name (e.g. `CCOD_FULL_2026_06.zip`) from an
/// HMLR "Use Land and Property Data" dataset-listing response.
///
/// The real response shape is `{result:{resources:[{file_name, …}]}}` — each
/// resource carries `file_name` but **no URL**. `marker` identifies the bulk
/// file (`"CCOD_FULL"` / `"OCOD_FULL"`). Returns the lexicographically greatest
/// match; file names embed `YYYY_MM`, so lexical max == most recent.
pub fn extract_full_file_name(listing: &serde_json::Value, marker: &str) -> Option<String> {
    listing
        .get("result")
        .and_then(|r| r.get("resources"))
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
        .filter_map(|res| res.get("file_name").and_then(|v| v.as_str()))
        .filter(|name| name.contains(marker))
        .max()
        .map(|s| s.to_string())
}

/// Extract the signed S3 `download_url` from an HMLR per-file response
/// (`{result:{download_url, …}}`). The URL is short-lived (~10–60s) so callers
/// must download immediately after this returns.
pub fn extract_download_url(file_resp: &serde_json::Value) -> Option<String> {
    file_resp
        .get("result")
        .and_then(|r| r.get("download_url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Resolve a short-lived signed S3 download URL from the HMLR "Use Land and
/// Property Data" API.
///
/// Two HMLR contract facts this encodes (both verified live, 2026-06-15):
/// 1. **Auth** — the API expects the RAW key in the `Authorization` header, NOT
///    a Bearer token. `reqwest::bearer_auth()` prepends `"Bearer "`, which HMLR
///    treats as part of the key and rejects.
/// 2. **Two-step URL** — listing a dataset returns `result.resources[]` with
///    `file_name` only; the signed URL comes from a second per-file call
///    `GET {dataset_url}/{file_name}` → `result.download_url`.
///
/// Returns `None` (with a structured WARN) on any auth/licence/shape failure.
/// The HMLR response body is **never** logged — it echoes the key on auth error.
pub async fn resolve_hmlr_download_url(
    client: &reqwest::Client,
    api_key: &str,
    dataset_url: &str,
    marker: &str,
    adapter: &'static str,
) -> Option<String> {
    // --- Step A: list the dataset's resources (raw-key auth) ----------------
    let listing: serde_json::Value = match client
        .get(dataset_url)
        .header(reqwest::header::AUTHORIZATION, api_key)
        .send()
        .await
    {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(adapter, error = %e, "HMLR dataset listing returned invalid JSON");
                return None;
            }
        },
        Err(e) => {
            warn!(adapter, error = %e, "HMLR dataset listing request failed");
            return None;
        }
    };

    if listing.get("success").and_then(|v| v.as_bool()) != Some(true) {
        let api_err = listing
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        warn!(adapter, error = %api_err, "HMLR dataset listing returned failure");
        return None;
    }

    let Some(file_name) = extract_full_file_name(&listing, marker) else {
        warn!(adapter, marker, "HMLR API listed no matching bulk resource");
        return None;
    };

    // --- Step B: per-file call → short-lived signed S3 URL ------------------
    // Fetch immediately before the download; the URL expires in ~10–60s.
    let file_url = format!("{dataset_url}/{file_name}");
    let file_resp: serde_json::Value = match client
        .get(&file_url)
        .header(reqwest::header::AUTHORIZATION, api_key)
        .send()
        .await
    {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(adapter, error = %e, "HMLR file-URL request returned invalid JSON");
                return None;
            }
        },
        Err(e) => {
            warn!(adapter, error = %e, "HMLR file-URL request failed");
            return None;
        }
    };

    if file_resp.get("success").and_then(|v| v.as_bool()) != Some(true) {
        let api_err = file_resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        warn!(adapter, error = %api_err, "HMLR file-URL request returned failure (licence not signed?)");
        return None;
    }

    let url = extract_download_url(&file_resp);
    if url.is_none() {
        warn!(adapter, "HMLR file response had no download_url");
    }
    url
}

/// Parse all valid CCOD rows from a ZIP file synchronously (bounded-memory, PROP-04).
///
/// Intended to run inside `tokio::task::spawn_blocking`.
///
/// Exposed as `pub` so unit tests can call it without HTTP (fixture ZIP → records).
///
/// # Pitfalls mitigated
/// - Pitfall 2: all 4 proprietor slots per row are iterated.
/// - Pitfall 6: CSV entry located by `.csv` extension, not by_index(0).
/// - V5: `company_number` validated via `is_valid_ch_number` before emission.
pub fn stream_from_path(cache_path: &std::path::Path) -> anyhow::Result<Vec<CcodRecord>> {
    let file = std::fs::File::open(cache_path)
        .map_err(|e| anyhow::anyhow!("open CCOD ZIP '{}': {}", cache_path.display(), e))?;

    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| anyhow::anyhow!("read CCOD ZIP: {}", e))?;

    // Pitfall 6: find .csv entry by extension, not by_index(0).
    let csv_index = (0..archive.len())
        .find(|&i| {
            archive
                .by_index(i)
                .map(|f| f.name().to_lowercase().ends_with(".csv"))
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("No .csv file found in CCOD ZIP"))?;

    let zip_entry = archive.by_index(csv_index)?;
    let reader = BufReader::new(zip_entry);
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(reader);

    // Read header row once (O(1) clone; the header record is tiny).
    let headers = rdr.headers()?.clone();

    let mut records: Vec<CcodRecord> = Vec::new();

    for result in rdr.records() {
        let row = match result {
            Ok(r) => r,
            Err(e) => {
                // Per-row error: degrade gracefully.
                tracing::warn!(error = %e, "CCOD: CSV parse error — skipping row");
                continue;
            }
        };

        let title_number = get_field_by_name(&row, &headers, "Title Number")
            .unwrap_or("")
            .to_string();
        let tenure = get_field_by_name(&row, &headers, "Tenure")
            .unwrap_or("")
            .to_string();
        let price_paid: Option<f64> = get_field_by_name(&row, &headers, "Price Paid")
            .unwrap_or("")
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|&v| v > 0.0);
        let date_str = get_field_by_name(&row, &headers, "Date Proprietor Added").unwrap_or("");

        // Pitfall 2: iterate all 4 proprietor slots.
        for &slot in PROPRIETOR_SLOTS {
            let cn_col = format!("Company Registration No. ({})", slot);
            let company_number = get_field_by_name(&row, &headers, &cn_col)
                .unwrap_or("")
                .trim()
                .to_string();

            if company_number.is_empty() {
                continue;
            }

            // V5: validate before any write.
            if !is_valid_ch_number(&company_number) {
                tracing::warn!(
                    company_number = %company_number,
                    slot = slot,
                    "CCOD: invalid CH number in CSV — skipping slot"
                );
                continue;
            }

            records.push(CcodRecord {
                title_number: title_number.clone(),
                tenure: tenure.clone(),
                price_paid,
                company_number,
                proprietor_slot: slot,
                date_proprietor_added: parse_ccod_date(date_str),
            });
        }
    }

    Ok(records)
}

/// Number of identity-link writes per commit chunk (PROP-04, OOM safety net).
///
/// CCOD emits one direct `write_identity_link` round-trip per surviving row. After every
/// `IDENTITY_LINK_CHUNK` writes the loop yields back to the Tokio runtime, which lets
/// surrealkv flush its memtables between chunks instead of accumulating the whole run's
/// working set in resident memory (the 21.5 GB OOM observed on the first full ingest).
const IDENTITY_LINK_CHUNK: usize = 25_000;

/// Load the set of company owner-node ids already tracked in the graph.
///
/// The hot graph persists every node as a `graph_events` row with `kind='NodeAdded'`
/// and the node id at `payload.entity_id` (see core `replay_from_surreal`). Company
/// owners are keyed `GB-COH:{company_number}`. We materialise just those ids once,
/// before the emit loop, so the loop can skip CCOD rows whose owner the graph never
/// tracked — avoiding orphan Property nodes / orphan identity_links (correctness) and
/// cutting write volume to the graph-tracked intersection (memory).
///
/// Returns an empty set on any query/parse failure (graceful degradation): an empty
/// known-set causes the loop to emit nothing, which is the safe outcome — CCOD is an
/// intersection enrichment and is meaningless before companies are loaded.
pub async fn load_known_company_ids(db: &Surreal<Any>) -> HashSet<String> {
    let rows: surrealdb::Result<Vec<serde_json::Value>> = db
        .query("SELECT payload.entity_id AS entity_id FROM graph_events WHERE kind = 'NodeAdded' AND string::starts_with(payload.entity_id, 'GB-COH:')")
        .await
        .and_then(|mut r| r.take(0));

    match rows {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                row.get("entity_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| s.starts_with("GB-COH:"))
                    .map(|s| s.to_string())
            })
            .collect(),
        Err(e) => {
            warn!(
                adapter = "land_registry_ccod",
                error = %e,
                "Failed to load known-company set from graph_events — emitting nothing this run (graceful degradation)"
            );
            HashSet::new()
        }
    }
}

/// True when a CCOD row's owner company is already tracked in the graph.
///
/// Pure predicate (no I/O) so the pre-filter decision is unit-testable without a DB.
/// `known` is the set of owner-node ids (`GB-COH:{company_number}`) from
/// [`load_known_company_ids`]; `company_number` is the raw CCOD slot value.
pub fn owner_is_known(known: &HashSet<String>, company_number: &str) -> bool {
    known.contains(&format!("GB-COH:{}", company_number))
}

/// Land Registry CCOD bulk-CSV adapter.
///
/// Disabled (`is_enabled() == false`) when `api_key` is absent or empty (D-03, CHA-03).
pub struct LandRegistryCcodAdapter {
    /// HMLR "Use Land and Property Data" API key (Bearer token).
    /// `None` or empty → `is_enabled()` returns false (graceful degradation, D-03).
    pub api_key: Option<String>,
    /// Local cache directory for downloaded CCOD ZIPs (D-02).
    pub cache_dir: PathBuf,
    /// Days between re-downloads (default 30 — monthly CCOD cadence, D-01).
    pub refresh_interval_days: u32,
}

impl LandRegistryCcodAdapter {
    pub fn new(api_key: Option<String>, cache_dir: PathBuf, refresh_interval_days: u32) -> Self {
        Self {
            api_key,
            cache_dir,
            refresh_interval_days,
        }
    }
}

#[async_trait]
impl EnricherAdapter for LandRegistryCcodAdapter {
    fn name(&self) -> &'static str {
        "land_registry_ccod"
    }

    /// Returns true only when a non-empty, non-whitespace API key is present (D-03, CHA-03).
    fn is_enabled(&self) -> bool {
        self.api_key
            .as_deref()
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }

    /// HM Land Registry covers England and Wales only; Registers of Scotland and Land &
    /// Property Services NI are separate registries this adapter does not read. The row
    /// filter here is `owner_is_known(company_number)` with NO geography test, so without
    /// this declaration a Scottish analysis would receive English property as its own.
    fn coverage(&self) -> urban_integrity_schema::nation::Jurisdiction {
        urban_integrity_schema::nation::Jurisdiction::ENGLAND_AND_WALES
    }

    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        if !self.is_enabled() {
            // T-18-02-01: never log the api_key value.
            warn!(
                adapter = "land_registry_ccod",
                api_key_present = false,
                "Adapter not enabled — api_key absent or empty (D-03)"
            );
            return Ok(EnrichmentStats::default());
        }

        let api_key = self.api_key.as_deref().unwrap_or_default().to_string();

        // --- Step 1: determine whether to download a fresh CCOD ZIP ------------
        let cache_path = self.cache_dir.join("CCOD_FULL_current.zip");

        let needs_download = if cache_path.exists() {
            let age_secs = std::fs::metadata(&cache_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|mt| mt.elapsed().ok())
                .map(|d| d.as_secs())
                .unwrap_or(u64::MAX);
            let refresh_secs = u64::from(self.refresh_interval_days) * 86_400;
            age_secs >= refresh_secs
        } else {
            true
        };

        if needs_download {
            std::fs::create_dir_all(&self.cache_dir)?;

            // --- Step 2: resolve the signed S3 URL via the HMLR API -----------
            // Raw-key auth + two-step listing→per-file contract (verified live).
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?;

            let Some(download_url) = resolve_hmlr_download_url(
                &client,
                &api_key,
                CCOD_API_URL,
                "CCOD_FULL",
                "land_registry_ccod",
            )
            .await
            else {
                return Ok(EnrichmentStats::default());
            };

            // Pitfall 1: the signed URL expires in ~10–60s — download immediately.
            let dl_client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()?;

            info!(
                adapter = "land_registry_ccod",
                "Downloading CCOD ZIP (streaming)"
            );
            let resp = dl_client.get(&download_url).send().await?;

            // T-18-02-02: content-length guard before buffering the body (ZIP-bomb DoS).
            // Advisory only — `content_length()` is `None` under chunked transfer-encoding and
            // is server-supplied either way, so it can be absent or simply wrong. The in-flight
            // counter below is the guard that actually holds.
            if let Some(len) = resp.content_length() {
                if len > MAX_ZIP_BYTES {
                    warn!(
                        adapter = "land_registry_ccod",
                        content_length = len,
                        max_bytes = MAX_ZIP_BYTES,
                        "CCOD ZIP exceeds 2 GiB limit — skipping download (T-18-02-02)"
                    );
                    return Ok(EnrichmentStats::default());
                }
            }

            // T-18-02-02: the binding guard — enforced on bytes ACTUALLY received.
            // See `adapters::download` for why `content_length()` alone is not a cap.
            let Some(written) = crate::adapters::download::stream_to_file_capped(
                resp,
                &cache_path,
                MAX_ZIP_BYTES,
                "land_registry_ccod",
            )
            .await?
            else {
                return Ok(EnrichmentStats::default());
            };
            info!(
                adapter = "land_registry_ccod",
                bytes = written,
                "CCOD ZIP cached"
            );
        } else {
            info!(
                adapter = "land_registry_ccod",
                "CCOD ZIP is fresh — using cached file (D-01)"
            );
        }

        // --- Step 3: parse CSV inside spawn_blocking (bounded-memory, PROP-04) -
        let cache_path_owned = cache_path.clone();
        let records: Vec<CcodRecord> =
            tokio::task::spawn_blocking(move || stream_from_path(&cache_path_owned)).await??;

        info!(
            adapter = "land_registry_ccod",
            record_count = records.len(),
            "CCOD CSV parsed — emitting mutations"
        );

        // --- Step 4a: pre-filter to graph-known companies (PROP-04, intersection) -
        // Build the known-owner set ONCE. Without this, AddNode{Property} and the
        // direct write_identity_link write for every UK property owner (3.49M rows),
        // while AddEdge{OwnsProperty} to an unknown owner is silently rejected by the
        // coordinator (storage::insert_edge returns Err, no WAL) — leaving orphan
        // Property nodes / orphan identity_links and OOM-killing surreal. We only emit
        // for owners the graph already tracks.
        let known = load_known_company_ids(db).await;
        info!(
            adapter = "land_registry_ccod",
            known_companies = known.len(),
            "CCOD pre-filter: loaded graph-known company set"
        );

        // --- Step 4b: emit mutations + write identity_links ----------------------
        let mut candidates_found = 0usize;
        let mut auto_linked = 0usize;
        let mut skipped_unknown = 0usize;

        for rec in &records {
            candidates_found += 1;

            // Pre-filter: skip rows whose owner company is not in the graph.
            // This is the correctness fix (no orphans) AND the primary memory fix.
            if !owner_is_known(&known, &rec.company_number) {
                skipped_unknown += 1;
                continue;
            }

            let node_id = format!("property:{}_{}", rec.company_number, rec.title_number);
            let src_id = format!("GB-COH:{}", rec.company_number);

            // AddNode { Property }
            let _ = mutation_tx
                .send(GraphMutation::AddNode {
                    id: EntityId(node_id.clone()),
                    weight: NodeWeight::Property(PropertyData {
                        title_number: Some(rec.title_number.clone()),
                        tenure: Some(rec.tenure.clone()),
                        price_paid: rec.price_paid,
                        company_number: Some(rec.company_number.clone()),
                        // CCOD is UK-domestic ownership; overseas country-of-
                        // incorporation is populated only by the OCOD adapter (PROP-05).
                        country_incorporated: None,
                    }),
                })
                .await;

            // AddEdge { OwnsProperty }
            let _ = mutation_tx
                .send(GraphMutation::AddEdge {
                    src: EntityId(src_id.clone()),
                    dst: EntityId(node_id.clone()),
                    weight: EdgeWeight::OwnsProperty {
                        meta: EdgeMeta {
                            created_at: Utc::now(),
                            effective_from: rec.date_proprietor_added,
                            effective_to: None,
                        },
                        data: OwnsPropertyData {
                            price_paid: rec.price_paid,
                            tenure: Some(rec.tenure.clone()),
                        },
                    },
                })
                .await;

            // Write identity_link — T-18-02-04: evidence_json MUST be a flat String (ENR-06).
            let evidence_json = serde_json::json!({
                "title_number": rec.title_number,
                "company_number": rec.company_number,
                "proprietor_slot": rec.proprietor_slot,
                "score": 1.0
            })
            .to_string();

            let link_record = IdentityLinkRecord {
                id: format!("identity_links:{}", uuid::Uuid::new_v4()),
                source_node_id: node_id.clone(),
                target_node_id: src_id.clone(),
                resolution_type: "exact_company_number".to_string(),
                confidence: 1.0,
                source: "land_registry".to_string(),
                resolved_at: Utc::now(),
                evidence_json,
                status: LinkStatus::AutoLinked,
            };

            write_identity_link(db, &link_record).await?;
            auto_linked += 1;

            // Chunked-commit boundary (OOM safety net): every IDENTITY_LINK_CHUNK
            // writes, yield to the runtime so surrealkv can flush memtables between
            // chunks instead of holding the whole run's working set resident.
            if auto_linked.is_multiple_of(IDENTITY_LINK_CHUNK) {
                info!(
                    adapter = "land_registry_ccod",
                    written = auto_linked,
                    "CCOD chunk boundary — yielding to let surrealkv flush"
                );
                tokio::task::yield_now().await;
            }
        }

        info!(
            adapter = "land_registry_ccod",
            candidates_found,
            auto_linked,
            skipped_unknown,
            "CCOD emit complete — wrote only graph-known owners (intersection)"
        );

        Ok(EnrichmentStats {
            candidates_found,
            auto_linked,
            pending_review: 0,
            sub_candidate: 0,
            discarded: skipped_unknown,
            skipped_out_of_jurisdiction: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-filter predicate keys on the `GB-COH:{company_number}` owner-node form,
    /// matching exactly what the emit loop builds as `src_id` and what the coordinator
    /// requires for AddEdge to succeed.
    #[test]
    fn owner_is_known_matches_gb_coh_key() {
        let mut known = HashSet::new();
        known.insert("GB-COH:12345678".to_string());
        known.insert("GB-COH:SC123456".to_string());

        // Tracked owners pass.
        assert!(owner_is_known(&known, "12345678"));
        assert!(owner_is_known(&known, "SC123456"));

        // Untracked owner is skipped — this is the orphan-prevention / OOM fix.
        assert!(!owner_is_known(&known, "99999999"));
    }

    /// An empty known-set (cold start or a failed graph_events query) skips every row,
    /// which is the safe degradation: CCOD is an intersection enrichment and must not
    /// write the entire UK property register when no companies are loaded.
    #[test]
    fn owner_is_known_empty_set_skips_all() {
        let known: HashSet<String> = HashSet::new();
        assert!(!owner_is_known(&known, "12345678"));
        assert!(!owner_is_known(&known, "SC123456"));
    }

    /// The key is exact — a bare company number without the `GB-COH:` prefix in the
    /// known-set must not accidentally match (guards against prefix drift).
    #[test]
    fn owner_is_known_requires_prefix() {
        let mut known = HashSet::new();
        known.insert("12345678".to_string()); // wrong form (no prefix)
        assert!(!owner_is_known(&known, "12345678"));
    }
}
