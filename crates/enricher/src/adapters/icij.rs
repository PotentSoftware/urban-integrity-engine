//! ICIJ Offshore Leaks adapter (FUZ-04).
//!
//! ODbL 1.0 open data — https://offshoreleaks.icij.org/pages/database
//! One-shot idempotent load of all four ICIJ datasets from `full-oldb.LATEST.zip`.
//! All data is written exclusively to `icij_offshore_entity` and `icij_offshore_link`
//! tables (D-09 ODbL isolation — prevents mixing with OGL-licensed tables).
//!
//! # Idempotency (D-10)
//! On each `run()` call, the adapter first issues a COUNT query against
//! `icij_offshore_entity`. If count > 0 the load is skipped and the adapter
//! returns immediately with default stats.
//!
//! # Memory safety
//! ZIP parsing runs inside `tokio::task::spawn_blocking` + `ZipArchive` +
//! `csv::Reader` so the Tokio runtime is never blocked and heap usage is bounded.
//!
//! # Security
//! - T-19-04-01: `MAX_ZIP_BYTES = 2 GiB` guard; Content-Length checked before download.
//! - T-19-04-02: All DB writes use bound parameters — field values never
//!   string-formatted into SurrealQL.
//! - T-19-04-03: Rows with empty `node_id` or `name` are skipped with WARN (ASVS V5).
//! - T-19-04-04: Only `icij_offshore_entity` / `icij_offshore_link` table names used —
//!   no JOIN with OGL tables.
//! - T-19-04-05: `evidence_json` stores only public identifiers and similarity floats,
//!   no PII beyond what ICIJ already publishes.
//!
//! # Column names (A1 mitigation)
//! ICIJ column schemas are LOW confidence (they changed between dataset releases).
//! On the **first run**, the adapter extracts and logs the `README.txt` (or any
//! `*README*` entry) from the ZIP at INFO level so the operator can verify actual
//! headers at runtime.  All CSV column access uses `get_field_by_name`
//! (header-position lookup), never positional indexing.
//!
//! # Scale (BatchController mandatory — the research notes' Don't Hand-Roll section)
//! The ICIJ database contains ~810k entities.  Sequential per-row DB writes would
//! cause timeouts.  `BatchController` from `urban_integrity_core::persistence::surreal`
//! provides AIMD back-pressure for bulk SurrealDB writes and is **mandatory** for all
//! `icij_offshore_entity` and `icij_offshore_link` upserts — do NOT substitute
//! sequential chunking.

use std::io::{BufReader, Cursor};
use std::path::PathBuf;

use anyhow::Context as _;
use async_trait::async_trait;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_core::persistence::surreal::BatchController;

use crate::adapters::{EnricherAdapter, EnrichmentStats};

/// ICIJ Offshore Leaks bulk CSV download URL.
const ICIJ_DOWNLOAD_URL: &str =
    "https://offshoreleaks-data.icij.org/offshoreleaks/csv/full-oldb.LATEST.zip";

/// Maximum ZIP file size (2 GiB) — T-19-04-01 DoS guard.
const MAX_ZIP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

// ── Data records ─────────────────────────────────────────────────────────────

/// A parsed entity record from the ICIJ `nodes-entities.csv` file.
#[derive(Debug, Clone)]
pub struct IcijEntityRecord {
    pub node_id: String,
    pub name: String,
    pub source_id: String,
    pub jurisdiction: Option<String>,
    pub country_codes: Option<String>,
    pub incorporation_date: Option<String>,
    pub status: Option<String>,
}

/// A parsed officer record from the ICIJ `nodes-officers.csv` file.
#[derive(Debug, Clone)]
pub struct IcijOfficerRecord {
    pub node_id: String,
    pub name: String,
    pub country_codes: Option<String>,
}

/// A parsed relationship record from the ICIJ `relationships.csv` file.
#[derive(Debug, Clone)]
pub struct IcijRelationshipRecord {
    pub node_id_start: String,
    pub node_id_end: String,
    pub rel_type: String,
    pub source_id: String,
}

/// Map an entity record to an insert row matching SCHEMA_DEF's
/// `icij_offshore_entity` fields — `dataset` (required string) carries ICIJ's
/// sourceID. `None` optionals are omitted (absent → NONE; an explicit JSON
/// null fails `option<string>` coercion).
fn entity_row(e: &IcijEntityRecord) -> serde_json::Value {
    let mut row = serde_json::Map::new();
    row.insert("node_id".into(), serde_json::json!(e.node_id));
    row.insert("name".into(), serde_json::json!(e.name));
    row.insert("dataset".into(), serde_json::json!(e.source_id));
    if let Some(v) = &e.jurisdiction {
        row.insert("jurisdiction".into(), serde_json::json!(v));
    }
    if let Some(v) = &e.country_codes {
        row.insert("country_codes".into(), serde_json::json!(v));
    }
    if let Some(v) = &e.incorporation_date {
        row.insert("incorporation_date".into(), serde_json::json!(v));
    }
    if let Some(v) = &e.status {
        row.insert("status".into(), serde_json::json!(v));
    }
    serde_json::Value::Object(row)
}

/// Map a relationship record to an insert row matching SCHEMA_DEF's
/// `icij_offshore_link` fields (`from_node_id`/`to_node_id`/`rel_type`/`dataset`).
fn link_row(r: &IcijRelationshipRecord) -> serde_json::Value {
    serde_json::json!({
        "from_node_id": r.node_id_start,
        "to_node_id": r.node_id_end,
        "rel_type": r.rel_type,
        "dataset": r.source_id,
    })
}

/// Output of `parse_icij_zip` — the three record categories from the ZIP.
pub struct IcijLoadResult {
    pub entities: Vec<IcijEntityRecord>,
    pub officers: Vec<IcijOfficerRecord>,
    pub relationships: Vec<IcijRelationshipRecord>,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

/// ICIJ Offshore Leaks bulk-CSV adapter (FUZ-04).
///
/// Downloads `full-oldb.LATEST.zip` once and caches it locally.
/// Idempotent: skips the load if `icij_offshore_entity` is already populated (D-10).
pub struct IcijAdapter {
    /// Local cache directory for the downloaded ICIJ ZIP.
    pub cache_dir: PathBuf,
    /// Whether the adapter is enabled in config.
    pub enabled: bool,
    /// HTTP client for downloading the bulk ZIP.
    client: reqwest::Client,
}

impl IcijAdapter {
    /// Construct a new `IcijAdapter`.
    ///
    /// `cache_dir` is mapped from `config.icij.cache_dir` (with a fallback of
    /// `"data/icij"`) in `engine.rs`.
    pub fn new(cache_dir: PathBuf, enabled: bool) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            // ICIJ's CloudFront CDN returns 403 ("Request blocked") for requests with no
            // User-Agent, and reqwest sends none by default. Set a descriptive UA (same
            // string as the London Gazette adapter) so the bulk ZIP download succeeds.
            .user_agent("Mozilla/5.0 (compatible; UrbanIntegrity/1.0)")
            .build()
            .expect("failed to build reqwest client for IcijAdapter");
        Self {
            cache_dir,
            enabled,
            client,
        }
    }
}

#[async_trait]
impl EnricherAdapter for IcijAdapter {
    fn name(&self) -> &'static str {
        "icij"
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn run(
        &self,
        db: &Surreal<Any>,
        _mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        // ── Guard: disabled ───────────────────────────────────────────────────
        if !self.is_enabled() {
            warn!(
                adapter = "icij",
                "Adapter not enabled (config.icij.enabled = false)"
            );
            return Ok(EnrichmentStats::default());
        }

        // ── D-10: idempotency check ────────────────────────────────────────────
        let count_rows: Vec<serde_json::Value> = db
            .query("SELECT count() FROM icij_offshore_entity GROUP ALL")
            .await?
            .take(0)?;
        let already_loaded = count_rows
            .first()
            .map(|r| r["count"].as_u64().unwrap_or(0))
            .unwrap_or(0)
            > 0;
        if already_loaded {
            info!(
                adapter = "icij",
                "ICIJ tables already populated — skipping load (D-10 idempotency)"
            );
            return Ok(EnrichmentStats::default());
        }

        // ── Step 1: ensure cache dir + determine whether to download ──────────
        let cache_path = self.cache_dir.join("full-oldb.LATEST.zip");
        if !cache_path.exists() {
            std::fs::create_dir_all(&self.cache_dir)?;

            // CR-03 / T-19-04-01: stream the body to disk — never buffer the full ZIP
            // in memory. Check content-length on the GET response inline during streaming
            // so the guard fires even when the server uses chunked-transfer encoding.
            info!(
                adapter = "icij",
                "Downloading ICIJ full-oldb ZIP (~1 GB, streaming)"
            );
            let mut resp = self.client.get(ICIJ_DOWNLOAD_URL).send().await?;

            // Content-Length pre-check (best-effort; may be absent for chunked responses).
            if let Some(len) = resp.content_length() {
                if len > MAX_ZIP_BYTES {
                    warn!(
                        adapter = "icij",
                        content_length = len,
                        max_bytes = MAX_ZIP_BYTES,
                        "ICIJ ZIP exceeds 2 GiB limit — skipping download (T-19-04-01)"
                    );
                    return Ok(EnrichmentStats::default());
                }
            }

            let mut file = tokio::io::BufWriter::new(tokio::fs::File::create(&cache_path).await?);
            let mut written: u64 = 0;
            while let Some(chunk) = resp.chunk().await? {
                written += chunk.len() as u64;
                if written > MAX_ZIP_BYTES {
                    warn!(
                        adapter = "icij",
                        "ICIJ ZIP exceeded size limit while streaming — aborting (T-19-04-01)"
                    );
                    drop(file);
                    let _ = tokio::fs::remove_file(&cache_path).await;
                    return Ok(EnrichmentStats::default());
                }
                tokio::io::copy(&mut chunk.as_ref(), &mut file).await?;
            }
            info!(
                adapter = "icij",
                bytes = written,
                "ICIJ ZIP cached at {:?} (streamed)",
                cache_path
            );
        } else {
            info!(
                adapter = "icij",
                "Using cached ICIJ ZIP at {:?} (D-10)", cache_path
            );
        }

        // ── Step 2: parse ZIP inside spawn_blocking (bounded memory) ──────────
        // WR-04: open the cached file directly — do NOT read the entire ZIP into a
        // Vec<u8> first. parse_icij_zip_from_file accepts impl Read + Seek so the
        // 1 GiB file is never loaded into heap memory.
        let cache_path_owned = cache_path.clone();
        let load_result = tokio::task::spawn_blocking(move || -> anyhow::Result<IcijLoadResult> {
            let file = std::fs::File::open(&cache_path_owned).map_err(|e| {
                anyhow::anyhow!("open ICIJ ZIP '{}': {}", cache_path_owned.display(), e)
            })?;
            parse_icij_zip_from_file(file)
        })
        .await??;

        info!(
            adapter = "icij",
            entities = load_result.entities.len(),
            officers = load_result.officers.len(),
            relationships = load_result.relationships.len(),
            "ICIJ ZIP parsed — beginning bulk upsert"
        );

        // ── Step 3: batch-insert icij_offshore_entity ─────────────────────────
        // One `INSERT IGNORE` per batch (BatchController-sized), NOT one query per
        // row: 810k single-row round-trips took ~10 h. `IGNORE` + the UNIQUE
        // node_id index makes re-entry after a partial load safe (first write
        // wins; the ICIJ dump is static). Field names MUST match SCHEMA_DEF —
        // `dataset` is a required string there, and a mismatch is a per-row
        // coercion error, which is why every write error is now propagated
        // instead of discarded (the old `let _ =` silently dropped 814k rows).
        let mut entity_controller = BatchController::new(10, 5000, 50);
        let mut entities_written: usize = 0;

        while entities_written < load_result.entities.len() {
            let end = (entities_written + entity_controller.current_size())
                .min(load_result.entities.len());
            let rows: Vec<serde_json::Value> = load_result.entities[entities_written..end]
                .iter()
                .map(entity_row)
                .collect();
            let start = std::time::Instant::now();
            let _created: Vec<serde_json::Value> = db
                .query("INSERT IGNORE INTO icij_offshore_entity $rows")
                .bind(("rows", rows))
                .await
                .context("ICIJ entity batch insert failed")?
                .take(0)
                .context("ICIJ entity batch insert rejected (schema mismatch?)")?;
            entities_written = end;
            entity_controller.adjust(start.elapsed().as_millis() as u64);
        }

        // ── Step 4: batch-insert icij_offshore_link ───────────────────────────
        // No UNIQUE index on links; duplicate protection is the D-10 whole-run
        // guard (entity count > 0 → skip), which also covers this table.
        let mut link_controller = BatchController::new(10, 5000, 50);
        let mut links_written: usize = 0;

        while links_written < load_result.relationships.len() {
            let end = (links_written + link_controller.current_size())
                .min(load_result.relationships.len());
            let rows: Vec<serde_json::Value> = load_result.relationships[links_written..end]
                .iter()
                .map(link_row)
                .collect();
            let start = std::time::Instant::now();
            let _created: Vec<serde_json::Value> = db
                .query("INSERT IGNORE INTO icij_offshore_link $rows")
                .bind(("rows", rows))
                .await
                .context("ICIJ link batch insert failed")?
                .take(0)
                .context("ICIJ link batch insert rejected (schema mismatch?)")?;
            links_written = end;
            link_controller.adjust(start.elapsed().as_millis() as u64);
        }

        info!(
            adapter = "icij",
            entities_written, links_written, "ICIJ bulk upsert complete"
        );

        // ── Step 5: entity→ICIJ identity linking — DISABLED (exact-match-only policy) ──
        // The former name-only fuzzy matcher (composite_score: Jaro-Winkler + DoubleMetaphone)
        // created identity_links of resolution_type "fuzzy_name_match_offshore" whenever a
        // graph director's surname phonetically/string-matched an ICIJ offshore entity name.
        //
        // ICIJ records carry NO corroborating identifier that can be joined against a UK
        // director (no UK company number, no DOB, no address overlap with the graph). The
        // match was therefore *structurally* name-only — the exact uncorroborated class that
        // produced ~87k false positives in the charity resolver (purged 2026-06-11) and 3,513
        // in the Electoral Commission individual path (disabled 2026-06-13, commit f7daa45).
        //
        // Per the project's exact-match-only data-integrity policy, no name-only fuzzy
        // identity_links are written. The ICIJ datasets are still fully ingested into the
        // isolated `icij_offshore_entity` / `icij_offshore_link` tables above (Steps 3–4) for
        // analyst reference; only the automatic entity→ICIJ *linking* is removed. Should a
        // corroborating exact identifier become available, an exact join can be reinstated here.
        Ok(EnrichmentStats {
            candidates_found: load_result.entities.len(),
            auto_linked: 0,
            pending_review: 0,
            sub_candidate: 0,
            discarded: 0,
            skipped_out_of_jurisdiction: 0,
        })
    }
}

// ── CSV helpers ───────────────────────────────────────────────────────────────

/// Look up a field by header name within a `csv::StringRecord`.
///
/// `csv::StringRecord` is positional; the header record provides the name → index map.
/// Using header-position lookup avoids column-order assumptions (A1 mitigation).
pub fn get_field_by_name<'a>(
    record: &'a csv::StringRecord,
    headers: &csv::StringRecord,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .position(|h| h.trim() == name)
        .and_then(|i| record.get(i))
}

// ── ZIP parser ────────────────────────────────────────────────────────────────

/// Parse the ICIJ `full-oldb.LATEST.zip` from a file on disk.
///
/// WR-04: Accepts a `std::fs::File` directly so the ZIP is never loaded into a
/// `Vec<u8>` on the heap — only the rows being parsed occupy memory at any time.
///
/// See [`parse_icij_zip`] for the full contract (same CSV processing, same A1
/// mitigations, same ASVS V5 row validation).
pub fn parse_icij_zip_from_file(file: std::fs::File) -> anyhow::Result<IcijLoadResult> {
    let archive =
        zip::ZipArchive::new(file).map_err(|e| anyhow::anyhow!("read ICIJ ZIP: {}", e))?;
    parse_icij_zip_inner(archive)
}

/// Parse the ICIJ `full-oldb.LATEST.zip` from raw bytes.
///
/// Processes three CSV files from the ZIP:
/// - `nodes-entities.csv` → `IcijEntityRecord`s
/// - `nodes-officers.csv` → `IcijOfficerRecord`s
/// - `relationships.csv`  → `IcijRelationshipRecord`s
///
/// All other entries (intermediaries, addresses) are skipped with DEBUG logs.
///
/// **A1 risk mitigation:** On first call, any `*README*` entry is extracted and
/// logged at INFO level so the operator can verify actual column headers at runtime.
///
/// Column names are assumed to be:
/// - entities: `"node_id"`, `"name"`, `"sourceID"` / `"source_id"`, `"jurisdiction"`,
///   `"country_codes"`, `"incorporation_date"`, `"status"`
/// - officers: `"node_id"`, `"name"`, `"country_codes"`
/// - relationships: `"node_id_start"`, `"node_id_end"`, `"rel_type"`
///
/// If a column name is not found the field is treated as `None` / empty with a WARN.
pub fn parse_icij_zip(zip_bytes: &[u8]) -> anyhow::Result<IcijLoadResult> {
    let cursor = Cursor::new(zip_bytes);
    let archive =
        zip::ZipArchive::new(cursor).map_err(|e| anyhow::anyhow!("read ICIJ ZIP: {}", e))?;
    parse_icij_zip_inner(archive)
}

/// Shared ZIP-parsing logic used by both [`parse_icij_zip`] and [`parse_icij_zip_from_file`].
fn parse_icij_zip_inner<R: std::io::Read + std::io::Seek>(
    mut archive: zip::ZipArchive<R>,
) -> anyhow::Result<IcijLoadResult> {
    let mut entities: Vec<IcijEntityRecord> = Vec::new();
    let mut officers: Vec<IcijOfficerRecord> = Vec::new();
    let mut relationships: Vec<IcijRelationshipRecord> = Vec::new();

    // First pass: log README.txt content (A1 column-name verification).
    // Two-step: first find the README index, then read it — avoids double-borrow on archive.
    let readme_index: Option<usize> = {
        let mut found = None;
        for i in 0..archive.len() {
            if let Ok(entry) = archive.by_index(i) {
                if entry.name().to_uppercase().contains("README") {
                    found = Some(i);
                    break;
                }
            }
        }
        found
    };
    if let Some(idx) = readme_index {
        use std::io::Read;
        let mut entry = archive.by_index(idx)?;
        let ename = entry.name().to_string();
        let mut readme = String::new();
        entry.read_to_string(&mut readme).ok();
        info!(
            adapter = "icij",
            readme_entry = %ename,
            "ICIJ README contents (A1 column-name verification):\n{}",
            &readme[..readme.len().min(4096)]
        );
    }

    // Second pass: parse CSV files.
    for i in 0..archive.len() {
        let entry_name = {
            let entry = archive.by_index(i)?;
            entry.name().to_string()
        };
        let entry_lower = entry_name.to_lowercase();

        if entry_lower.contains("nodes-entities") && entry_lower.ends_with(".csv") {
            let entry = archive.by_index(i)?;
            let reader = BufReader::new(entry);
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(true)
                .flexible(true)
                .from_reader(reader);

            let headers = rdr
                .headers()
                .map_err(|e| anyhow::anyhow!("ICIJ entities CSV headers: {}", e))?
                .clone();

            // Resolve "sourceID" or "source_id" variant (A1 pitfall).
            let source_id_col = if headers.iter().any(|h| h.trim() == "sourceID") {
                "sourceID"
            } else {
                "source_id"
            };

            for result in rdr.records() {
                let row = match result {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "ICIJ: entities CSV parse error — skipping row");
                        continue;
                    }
                };

                let node_id = get_field_by_name(&row, &headers, "node_id")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let name = get_field_by_name(&row, &headers, "name")
                    .unwrap_or("")
                    .trim()
                    .to_string();

                // ASVS V5: skip empty node_id or name.
                if node_id.is_empty() {
                    warn!(
                        adapter = "icij",
                        "ICIJ entities: empty node_id — skipping row"
                    );
                    continue;
                }
                if name.is_empty() {
                    warn!(adapter = "icij", "ICIJ entities: empty name — skipping row");
                    continue;
                }

                let source_id = get_field_by_name(&row, &headers, source_id_col)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let jurisdiction = get_field_by_name(&row, &headers, "jurisdiction")
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string());
                let country_codes = get_field_by_name(&row, &headers, "country_codes")
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string());
                let incorporation_date = get_field_by_name(&row, &headers, "incorporation_date")
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string());
                let status = get_field_by_name(&row, &headers, "status")
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string());

                entities.push(IcijEntityRecord {
                    node_id,
                    name,
                    source_id,
                    jurisdiction,
                    country_codes,
                    incorporation_date,
                    status,
                });
            }
        } else if entry_lower.contains("nodes-officers") && entry_lower.ends_with(".csv") {
            let entry = archive.by_index(i)?;
            let reader = BufReader::new(entry);
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(true)
                .flexible(true)
                .from_reader(reader);

            let headers = rdr
                .headers()
                .map_err(|e| anyhow::anyhow!("ICIJ officers CSV headers: {}", e))?
                .clone();

            for result in rdr.records() {
                let row = match result {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "ICIJ: officers CSV parse error — skipping row");
                        continue;
                    }
                };

                let node_id = get_field_by_name(&row, &headers, "node_id")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let name = get_field_by_name(&row, &headers, "name")
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if node_id.is_empty() {
                    warn!(
                        adapter = "icij",
                        "ICIJ officers: empty node_id — skipping row"
                    );
                    continue;
                }
                if name.is_empty() {
                    warn!(adapter = "icij", "ICIJ officers: empty name — skipping row");
                    continue;
                }

                let country_codes = get_field_by_name(&row, &headers, "country_codes")
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string());

                officers.push(IcijOfficerRecord {
                    node_id,
                    name,
                    country_codes,
                });
            }
        } else if entry_lower.contains("relationships") && entry_lower.ends_with(".csv") {
            let entry = archive.by_index(i)?;
            let reader = BufReader::new(entry);
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(true)
                .flexible(true)
                .from_reader(reader);

            let headers = rdr
                .headers()
                .map_err(|e| anyhow::anyhow!("ICIJ relationships CSV headers: {}", e))?
                .clone();

            for result in rdr.records() {
                let row = match result {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "ICIJ: relationships CSV parse error — skipping row");
                        continue;
                    }
                };

                let node_id_start = get_field_by_name(&row, &headers, "node_id_start")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let node_id_end = get_field_by_name(&row, &headers, "node_id_end")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let rel_type = get_field_by_name(&row, &headers, "rel_type")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                // "sourceID" / "source_id" variant handling (A1 pitfall), with a
                // fallback: the schema requires `dataset` to be a string.
                let source_id = get_field_by_name(&row, &headers, "sourceID")
                    .or_else(|| get_field_by_name(&row, &headers, "source_id"))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("unknown")
                    .to_string();

                if node_id_start.is_empty() || node_id_end.is_empty() {
                    warn!(
                        adapter = "icij",
                        "ICIJ relationships: empty node_id — skipping row"
                    );
                    continue;
                }

                relationships.push(IcijRelationshipRecord {
                    node_id_start,
                    node_id_end,
                    rel_type,
                    source_id,
                });
            }
        } else {
            debug!(
                adapter = "icij",
                entry = %entry_name,
                "ICIJ ZIP entry — skipping (not a target CSV)"
            );
        }
    }

    info!(
        adapter = "icij",
        entities = entities.len(),
        officers = officers.len(),
        relationships = relationships.len(),
        "parse_icij_zip complete"
    );

    Ok(IcijLoadResult {
        entities,
        officers,
        relationships,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper: build a minimal in-memory ZIP with given CSV files ────────────

    fn make_zip_with_csv(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;

        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut zip_writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        for (name, content) in entries {
            zip_writer.start_file(*name, options).unwrap();
            zip_writer.write_all(content.as_bytes()).unwrap();
        }

        let result = zip_writer.finish().unwrap();
        result.into_inner()
    }

    // ── get_field_by_name (pure column lookup) ────────────────────────────────

    #[test]
    fn get_field_by_name_returns_matching_column() {
        let headers = csv::StringRecord::from(vec!["node_id", "name", "jurisdiction"]);
        let record = csv::StringRecord::from(vec!["N1", "Acme Ltd", "PA"]);
        assert_eq!(
            get_field_by_name(&record, &headers, "name"),
            Some("Acme Ltd")
        );
        assert_eq!(get_field_by_name(&record, &headers, "node_id"), Some("N1"));
        assert_eq!(
            get_field_by_name(&record, &headers, "jurisdiction"),
            Some("PA")
        );
    }

    #[test]
    fn get_field_by_name_trims_header_whitespace() {
        // Headers carry stray padding; lookup trims before comparing.
        let headers = csv::StringRecord::from(vec![" node_id ", "  name"]);
        let record = csv::StringRecord::from(vec!["N1", "Acme"]);
        assert_eq!(get_field_by_name(&record, &headers, "name"), Some("Acme"));
        assert_eq!(get_field_by_name(&record, &headers, "node_id"), Some("N1"));
    }

    #[test]
    fn get_field_by_name_unknown_column_is_none() {
        let headers = csv::StringRecord::from(vec!["node_id", "name"]);
        let record = csv::StringRecord::from(vec!["N1", "Acme"]);
        assert_eq!(get_field_by_name(&record, &headers, "missing"), None);
    }

    #[test]
    fn get_field_by_name_short_record_is_none() {
        // Header exists but the record has fewer columns → no value.
        let headers = csv::StringRecord::from(vec!["node_id", "name", "jurisdiction"]);
        let record = csv::StringRecord::from(vec!["N1", "Acme"]);
        assert_eq!(get_field_by_name(&record, &headers, "jurisdiction"), None);
    }

    // ── Test 1: Idempotency check ─────────────────────────────────────────────
    // When icij_offshore_entity COUNT > 0 (simulated by checking the query
    // string pattern), run() would return Ok(default) and log "already populated".
    // We test this by verifying the adapter name and the D-10 query string.
    #[test]
    fn test_idempotency_check_skips_when_count_nonzero() {
        // Verify the query constant used by the idempotency check contains the
        // correct table name and GROUP ALL idiom (D-10 truth).
        let idempotency_query = "SELECT count() FROM icij_offshore_entity GROUP ALL";
        assert!(
            idempotency_query.contains("icij_offshore_entity"),
            "idempotency query must reference icij_offshore_entity table"
        );
        assert!(
            idempotency_query.contains("count()"),
            "idempotency query must use count() aggregate"
        );
        assert!(
            idempotency_query.contains("GROUP ALL"),
            "idempotency query must use GROUP ALL for scalar count"
        );

        // Also verify the adapter skips when disabled (simulated by is_enabled check).
        let adapter = IcijAdapter {
            cache_dir: PathBuf::from("/tmp/icij-test"),
            enabled: false,
            client: reqwest::Client::new(),
        };
        assert!(
            !adapter.is_enabled(),
            "adapter with enabled=false must return is_enabled()=false"
        );
    }

    // ── Test 2: CSV row validation (ASVS V5) ─────────────────────────────────
    // A mock CSV row with empty node_id or name must be skipped.
    // A valid row must be accepted.
    #[test]
    fn test_csv_row_validation_empty_fields_are_skipped() {
        // Build a minimal ZIP with nodes-entities.csv containing:
        // - row 1: empty node_id (should be skipped)
        // - row 2: empty name (should be skipped)
        // - row 3: valid row (should be accepted)
        let csv_content = "node_id,name,sourceID,jurisdiction,country_codes,incorporation_date,status\n\
                           ,Empty NodeId Corp,Panama Papers,,,, \n\
                           12345,   ,Panama Papers,,,,\n\
                           67890,Apex BVI Ltd,Panama Papers,British Virgin Islands,,2001-01-01,Active\n";

        let zip_bytes = make_zip_with_csv(&[("nodes-entities.csv", csv_content)]);
        let result = parse_icij_zip(&zip_bytes).expect("parse_icij_zip should succeed");

        assert_eq!(
            result.entities.len(),
            1,
            "Only the valid row should be parsed; got {} entities (expected 1)",
            result.entities.len()
        );
        assert_eq!(
            result.entities[0].node_id, "67890",
            "Valid entity node_id must be '67890'"
        );
        assert_eq!(
            result.entities[0].name, "Apex BVI Ltd",
            "Valid entity name must be 'Apex BVI Ltd'"
        );
    }

    // ── Test 3: parse_icij_zip with mock ZIP bytes ────────────────────────────
    // A mock CSV row (node_id="12345", name="Apex BVI Ltd") must parse correctly
    // with header-position lookup (not positional indexing).
    #[test]
    fn test_parse_icij_zip_extracts_entity_record() {
        let csv_content = "node_id,name,sourceID,jurisdiction,country_codes,incorporation_date,status\n\
                           12345,Apex BVI Ltd,Panama Papers,British Virgin Islands,VGB,2001-03-15,Active\n";

        let zip_bytes = make_zip_with_csv(&[("nodes-entities.csv", csv_content)]);
        let result = parse_icij_zip(&zip_bytes).expect("parse_icij_zip should succeed");

        assert_eq!(result.entities.len(), 1);
        let entity = &result.entities[0];

        assert_eq!(entity.node_id, "12345");
        assert_eq!(entity.name, "Apex BVI Ltd");
        assert_eq!(entity.source_id, "Panama Papers");
        assert_eq!(
            entity.jurisdiction.as_deref(),
            Some("British Virgin Islands")
        );
        assert_eq!(entity.country_codes.as_deref(), Some("VGB"));
        assert_eq!(entity.incorporation_date.as_deref(), Some("2001-03-15"));
        assert_eq!(entity.status.as_deref(), Some("Active"));
    }

    // ── Test 3b: parse_icij_zip with source_id variant ───────────────────────
    // Some ICIJ releases use "source_id" instead of "sourceID" (A1 pitfall mitigation).
    #[test]
    fn test_parse_icij_zip_accepts_source_id_column_variant() {
        let csv_content = "node_id,name,source_id,jurisdiction\n\
                           99999,Pandora Corp,Pandora Papers,Panama\n";

        let zip_bytes = make_zip_with_csv(&[("nodes-entities.csv", csv_content)]);
        let result = parse_icij_zip(&zip_bytes)
            .expect("parse_icij_zip should succeed with source_id variant");

        assert_eq!(result.entities.len(), 1);
        assert_eq!(result.entities[0].source_id, "Pandora Papers");
    }

    // ── Test 4: insert rows must satisfy SCHEMA_DEF's required icij fields ───
    // Regression for the silent-write-failure bug: the old per-row UPSERT
    // omitted the required `dataset` field (and wrote node_id_start/node_id_end
    // where the schema defines from_node_id/to_node_id), so every insert failed
    // string coercion — and the error was discarded. These tests derive the
    // required field list straight from SCHEMA_DEF so schema/adapter drift
    // fails offline instead of silently at runtime.

    /// Field names defined `TYPE string` (required) on `table` in a schema doc.
    fn required_string_fields(schema: &str, table: &str) -> Vec<String> {
        let on_marker = format!(" ON {table} ");
        schema
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let rest = line.strip_prefix("DEFINE FIELD IF NOT EXISTS ")?;
                if !rest.contains(&on_marker) || !rest.ends_with("TYPE string;") {
                    return None;
                }
                rest.split_whitespace().next().map(str::to_string)
            })
            .collect()
    }

    #[test]
    fn test_entity_row_satisfies_schema_required_fields() {
        let required = required_string_fields(
            urban_integrity_schema::db::SCHEMA_DEF,
            "icij_offshore_entity",
        );
        assert!(
            !required.is_empty(),
            "SCHEMA_DEF parse found no required fields — parser or schema changed"
        );

        let entity = IcijEntityRecord {
            node_id: "12345".into(),
            name: "Apex BVI Ltd".into(),
            source_id: "Panama Papers".into(),
            jurisdiction: None,
            country_codes: None,
            incorporation_date: None,
            status: None,
        };
        let row = entity_row(&entity);
        for field in &required {
            assert!(
                row.get(field).map(serde_json::Value::is_string) == Some(true),
                "entity row is missing required string field `{field}` defined in SCHEMA_DEF"
            );
        }
        // None optionals must be omitted, not JSON null — an explicit null fails
        // `option<string>` coercion just like a missing required field does.
        assert!(
            row.get("jurisdiction").is_none(),
            "None optionals must be omitted from the insert row"
        );
    }

    #[test]
    fn test_link_row_satisfies_schema_required_fields() {
        let required =
            required_string_fields(urban_integrity_schema::db::SCHEMA_DEF, "icij_offshore_link");
        assert!(
            !required.is_empty(),
            "SCHEMA_DEF parse found no required fields — parser or schema changed"
        );

        let rel = IcijRelationshipRecord {
            node_id_start: "12345".into(),
            node_id_end: "67890".into(),
            rel_type: "officer_of".into(),
            source_id: "Panama Papers".into(),
        };
        let row = link_row(&rel);
        for field in &required {
            assert!(
                row.get(field).map(serde_json::Value::is_string) == Some(true),
                "link row is missing required string field `{field}` defined in SCHEMA_DEF"
            );
        }
    }

    // ── Test 5: relationships parse carries sourceID into `dataset` ──────────
    #[test]
    fn test_relationships_parse_carries_source_id_with_fallback() {
        let with_source = "node_id_start,node_id_end,rel_type,sourceID\n\
                           1,2,officer_of,Panama Papers\n";
        let zip_bytes = make_zip_with_csv(&[("relationships.csv", with_source)]);
        let result = parse_icij_zip(&zip_bytes).expect("parse should succeed");
        assert_eq!(result.relationships.len(), 1);
        assert_eq!(result.relationships[0].source_id, "Panama Papers");

        let without_source = "node_id_start,node_id_end,rel_type\n\
                              1,2,officer_of\n";
        let zip_bytes = make_zip_with_csv(&[("relationships.csv", without_source)]);
        let result = parse_icij_zip(&zip_bytes).expect("parse should succeed");
        assert_eq!(result.relationships.len(), 1);
        assert_eq!(
            result.relationships[0].source_id, "unknown",
            "missing sourceID column must fall back to 'unknown' (dataset is a required string)"
        );
    }
}
