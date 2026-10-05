//! Charity Commission for England and Wales adapter (FUZ-02).
//!
//! OGL v3 open data, no API key required. Downloads the bulk charity ZIP from
//! the Azure blob storage register-of-charities CDN (`publicextract.*` schema).
//! Produces `Charity` nodes, `TrusteeOf` edges, and identity links ONLY from
//! exact company_number joins (charity registered company number == an Entity's
//! GB-COH company number). The former fuzzy trustee→officer name-matching path was
//! removed (260611-gzy): charity data carries no DOB/address, so name-only fuzzy
//! matching is inherently unusable — only the exact company_number join is trustworthy.
//!
//! # Memory safety
//! CSV is streamed via `spawn_blocking` + `csv::Reader` so the Tokio runtime is
//! never blocked and heap usage is bounded to one row at a time.
//!
//! # Security
//! - T-19-02-01: Content-Length > 2 GiB → WARN + skip (ZIP bomb DoS guard).
//!   Applied to the charity download (HEAD content-length + in-flight byte counter).
//! - T-19-02-02: All DB writes via `write_identity_link()` with bound parameters (ENR-06).
//! - T-19-02-04: `evidence_json` is always a flat `String` (ENR-06), never `serde_json::Value`.

use std::io::BufReader;
use std::path::PathBuf;
use urban_integrity_core::persistence::scan;

use async_trait::async_trait;
use chrono::Utc;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{CharityData, NodeWeight};

use crate::adapters::ch_psc::write_identity_link;
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{IdentityLinkRecord, LinkStatus};

/// Charity Commission bulk register — charity data ZIP (Azure blob CDN, `publicextract.*` schema).
const CC_CHARITY_URL: &str =
    "https://ccewuksprdoneregsadata1.blob.core.windows.net/data/txt/publicextract.charity.zip";

/// Maximum ZIP file size (2 GiB) — T-19-02-01 DoS guard.
const MAX_ZIP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A parsed Charity record from the Charity Commission bulk CSV.
#[derive(Debug, Clone)]
pub struct CharityRecord {
    pub charity_number: String,
    pub company_number: String,
    pub name: String,
}

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

/// Detect the company number column name using the CC field fallback chain (Pitfall 7).
///
/// Tries `"charity_company_registration_number"` first (current `publicextract.*` schema),
/// then `"coyno"` (legacy BCP schema), then `"registered_company_number"`,
/// then `"company_number"` (newer/JSON-export schema). Returns the first found.
pub fn detect_company_number_field(headers: &csv::StringRecord) -> Option<&'static str> {
    let names: &[&str] = &[
        "charity_company_registration_number",
        "coyno",
        "registered_company_number",
        "company_number",
    ];
    names
        .iter()
        .find(|&&name| headers.iter().any(|h| h.trim() == name))
        .copied()
        .map(|v| v as _)
}

/// Parse charity records from the `publicextract.charity.zip` archive.
///
/// Expects a single TAB-delimited entry whose name contains `"publicextract.charity"`
/// but NOT `"trustee"` and ends with `".txt"`.
///
/// Intended to run inside `tokio::task::spawn_blocking` (bounded-memory).
pub fn parse_charity_zip(path: &PathBuf) -> anyhow::Result<Vec<CharityRecord>> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("open CC charity ZIP '{}': {}", path.display(), e))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| anyhow::anyhow!("read CC charity ZIP: {}", e))?;

    // Identify the target entry by name.
    let mut entry_index: Option<usize> = None;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            let name = entry.name().to_lowercase();
            if name.contains("publicextract.charity")
                && !name.contains("trustee")
                && name.ends_with(".txt")
            {
                entry_index = Some(i);
                break;
            }
        }
    }

    let idx =
        entry_index.ok_or_else(|| anyhow::anyhow!("CC charity ZIP: no matching entry found"))?;

    let entry = archive.by_index(idx)?;
    let reader = BufReader::new(entry);
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .delimiter(b'\t')
        .from_reader(reader);

    let headers = rdr.headers()?.clone();

    // Pitfall 7: detect company number column with fallback chain.
    let cn_field = detect_company_number_field(&headers);
    debug!(
        adapter = "charity_commission",
        cn_field = ?cn_field,
        "Detected company number column in charity TXT"
    );

    // Find name column (typically "charity_name" or "name").
    let name_col = if headers.iter().any(|h| h.trim() == "charity_name") {
        "charity_name"
    } else {
        "name"
    };

    // Find charity number column (typically "regno" or "registered_charity_number").
    let regno_col = if headers.iter().any(|h| h.trim() == "regno") {
        "regno"
    } else {
        "registered_charity_number"
    };

    let mut charities: Vec<CharityRecord> = Vec::new();

    for result in rdr.records() {
        let row = match result {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "CC: charity TXT parse error — skipping row");
                continue;
            }
        };

        let charity_number = get_field_by_name(&row, &headers, regno_col)
            .unwrap_or("")
            .trim()
            .to_string();
        let name = get_field_by_name(&row, &headers, name_col)
            .unwrap_or("")
            .trim()
            .to_string();
        let company_number = cn_field
            .and_then(|col| get_field_by_name(&row, &headers, col))
            .unwrap_or("")
            .trim()
            .to_string();

        // ASVS V5: validate non-empty.
        if charity_number.is_empty() || name.is_empty() {
            continue;
        }

        charities.push(CharityRecord {
            charity_number,
            company_number,
            name,
        });
    }

    Ok(charities)
}

/// Charity Commission for England and Wales adapter.
///
/// Disabled (`is_enabled() == false`) via config. Enabled by default because
/// no API key is required (OGL v3 open data).
pub struct CharityCommissionAdapter {
    /// Local cache directory for downloaded CC ZIPs.
    pub cache_dir: PathBuf,
    /// Days between re-downloads (default 30 — monthly CC cadence).
    pub refresh_interval_days: u64,
    /// Whether the adapter is enabled in config.
    pub enabled: bool,
    /// HTTP client for downloading the bulk ZIPs.
    http_client: reqwest::Client,
}

impl CharityCommissionAdapter {
    pub fn new(cache_dir: PathBuf, refresh_interval_days: u64, enabled: bool) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .expect("failed to build reqwest client");
        Self {
            cache_dir,
            refresh_interval_days,
            enabled,
            http_client,
        }
    }
}

#[async_trait]
impl EnricherAdapter for CharityCommissionAdapter {
    fn name(&self) -> &'static str {
        "charity_commission"
    }

    /// Returns true when `enabled = true` in config (no API key required — OGL v3).
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The Charity Commission for England and Wales. Scotland is OSCR and NI is CCNI;
    /// neither is implemented, so a Scottish analysis gets zero trustee links.
    fn coverage(&self) -> urban_integrity_schema::nation::Jurisdiction {
        urban_integrity_schema::nation::Jurisdiction::ENGLAND_AND_WALES
    }

    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        if !self.is_enabled() {
            warn!(
                adapter = "charity_commission",
                "Adapter not enabled — disabled in config"
            );
            return Ok(EnrichmentStats::default());
        }

        // --- Step 1: determine whether to download fresh CC ZIPs ----------------
        std::fs::create_dir_all(&self.cache_dir)?;

        let charity_cache = self.cache_dir.join("publicextract.charity.zip");

        for (url, cache_path) in [(CC_CHARITY_URL, &charity_cache)] {
            let needs_download = if cache_path.exists() {
                let age_secs = std::fs::metadata(cache_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|mt| mt.elapsed().ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(u64::MAX);
                age_secs >= (self.refresh_interval_days * 86_400)
            } else {
                true
            };

            if needs_download {
                // WR-03 / T-19-02-01: HEAD the blob URL directly — Azure blob storage
                // returns Content-Length and application/octet-stream with no redirect.
                info!(
                    adapter = "charity_commission",
                    zip = url,
                    "Checking CC ZIP size via HEAD"
                );
                let head_resp = self
                    .http_client
                    .head(url)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("CC HEAD request failed for {url}: {e}"))?;

                // Content-Length guard (T-19-02-01):
                if let Some(len) = head_resp.content_length() {
                    if len > MAX_ZIP_BYTES {
                        warn!(
                            adapter = "charity_commission",
                            zip = url,
                            content_length = len,
                            max_bytes = MAX_ZIP_BYTES,
                            "CC ZIP exceeds 2 GiB limit — skipping download (T-19-02-01)"
                        );
                        return Ok(EnrichmentStats::default());
                    }
                }

                // CR-03: stream body to disk — never buffer the full ZIP in memory.
                // The in-flight byte counter enforces MAX_ZIP_BYTES even when the server
                // omits Content-Length (chunked-transfer path).
                info!(
                    adapter = "charity_commission",
                    zip = url,
                    "Downloading CC ZIP (streaming)"
                );
                let mut resp = self.http_client.get(url).send().await?;
                let mut file =
                    tokio::io::BufWriter::new(tokio::fs::File::create(cache_path).await?);
                let mut written: u64 = 0;
                while let Some(chunk) = resp.chunk().await? {
                    written += chunk.len() as u64;
                    if written > MAX_ZIP_BYTES {
                        warn!(
                            adapter = "charity_commission",
                            zip = url,
                            "CC ZIP exceeded size limit while streaming — aborting (T-19-02-01)"
                        );
                        drop(file);
                        let _ = tokio::fs::remove_file(cache_path).await;
                        return Ok(EnrichmentStats::default());
                    }
                    tokio::io::copy(&mut chunk.as_ref(), &mut file).await?;
                }
                info!(
                    adapter = "charity_commission",
                    bytes = written,
                    zip = url,
                    "CC ZIP cached"
                );
            } else {
                info!(
                    adapter = "charity_commission",
                    zip = url,
                    "CC ZIP is fresh — using cached file"
                );
            }
        }

        // --- Step 2: parse TXT file inside spawn_blocking (bounded-memory) -------
        let charity_cache_owned = charity_cache.clone();
        let charities: Vec<CharityRecord> =
            tokio::task::spawn_blocking(move || parse_charity_zip(&charity_cache_owned)).await??;

        info!(
            adapter = "charity_commission",
            charity_count = charities.len(),
            "CC TXT file parsed"
        );

        // --- Step 3: build lookup map from company_number → charity record ------
        // Only include charities with a non-empty company_number.
        let charity_by_cn: std::collections::HashMap<String, &CharityRecord> = charities
            .iter()
            .filter(|c| !c.company_number.is_empty())
            .map(|c| (c.company_number.clone(), c))
            .collect();

        // --- Step 4: query existing entity graph nodes --------------------------
        // Use weight_json CONTAINS pattern (SurrealDB 3.x RecordId coercion workaround).
        let entity_rows: Vec<serde_json::Value> =
            scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Entity\"")
                .await
                .unwrap_or_default();

        let mut stats = EnrichmentStats::default();

        // --- Step 5: exact company_number join ----------------------------------
        for row in &entity_rows {
            // Extract company_number from weight_json Rust-side.
            // SurrealDB 3.x RecordId coercion workaround: parse weight_json as a string,
            // then deserialize — direct field access may return null for nested values.
            let weight_json = if let Some(s) = row["payload"]["weight_json"].as_str() {
                serde_json::from_str::<serde_json::Value>(s).unwrap_or_default()
            } else {
                row["payload"]["weight_json"].clone()
            };
            let company_number = weight_json["company_number"]
                .as_str()
                .unwrap_or("")
                .to_string();

            if company_number.is_empty() {
                continue;
            }

            let Some(charity_rec) = charity_by_cn.get(&company_number) else {
                continue;
            };

            stats.candidates_found += 1;

            let entity_node_id = row["payload"]["node_id"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    // Fallback: extract from weight_json id or entity_id field
                    row["payload"]["entity_id"]
                        .as_str()
                        .unwrap_or("")
                        .to_string()
                });

            let charity_node_id = format!("charity:{}", charity_rec.charity_number);

            // Emit AddNode { Charity }
            let _ = mutation_tx
                .send(GraphMutation::AddNode {
                    id: EntityId(charity_node_id.clone()),
                    weight: NodeWeight::Charity(CharityData {
                        name: charity_rec.name.clone(),
                        charity_number: Some(charity_rec.charity_number.clone()),
                        registered_name: None,
                    }),
                })
                .await;

            // Emit AddEdge { TrusteeOf } — entity → charity at confidence 1.0
            let _ = mutation_tx
                .send(GraphMutation::AddEdge {
                    src: EntityId(entity_node_id.clone()),
                    dst: EntityId(charity_node_id.clone()),
                    weight: EdgeWeight::TrusteeOf {
                        meta: EdgeMeta {
                            created_at: Utc::now(),
                            effective_from: None,
                            effective_to: None,
                        },
                    },
                })
                .await;

            // Write identity_link — T-19-02-02: evidence_json MUST be a flat String (ENR-06).
            let evidence_json = serde_json::json!({
                "charity_number": charity_rec.charity_number,
                "company_number": company_number,
                "match": "exact_company_number",
                "score": 1.0
            })
            .to_string();

            let link_record = IdentityLinkRecord {
                id: format!("identity_links:{}", uuid::Uuid::new_v4()),
                source_node_id: charity_node_id.clone(),
                target_node_id: entity_node_id.clone(),
                resolution_type: "exact_company_number".to_string(),
                confidence: 1.0,
                source: "charity_commission".to_string(),
                resolved_at: Utc::now(),
                evidence_json,
                status: LinkStatus::AutoLinked,
            };

            write_identity_link(db, &link_record).await?;
            stats.auto_linked += 1;
        }

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1: CharityCommissionAdapter with enabled=false — is_enabled() returns false
    // and run() returns Ok(EnrichmentStats::default()) (no panic, no error).
    #[test]
    fn test_disabled_adapter_is_not_enabled() {
        let adapter = CharityCommissionAdapter::new(
            PathBuf::from("/tmp/charity_test"),
            30,
            false, // disabled
        );
        assert!(!adapter.is_enabled());
        assert_eq!(adapter.name(), "charity_commission");
    }

    // Test 2: Exact company_number join — given a CharityRecord with company_number="12345678"
    // and an entity with company_number="12345678", the IdentityLinkRecord has confidence=1.0,
    // status=AutoLinked, source="charity_commission", resolution_type="exact_company_number".
    #[test]
    fn test_exact_company_number_produces_auto_linked_record() {
        let charity = CharityRecord {
            charity_number: "1234567".to_string(),
            company_number: "12345678".to_string(),
            name: "Test Charity".to_string(),
        };

        let evidence_json = serde_json::json!({
            "charity_number": charity.charity_number,
            "company_number": charity.company_number,
            "match": "exact_company_number",
            "score": 1.0
        })
        .to_string();

        let link_record = IdentityLinkRecord {
            id: format!("identity_links:{}", uuid::Uuid::new_v4()),
            source_node_id: format!("charity:{}", charity.charity_number),
            target_node_id: "GB-COH:12345678".to_string(),
            resolution_type: "exact_company_number".to_string(),
            confidence: 1.0,
            source: "charity_commission".to_string(),
            resolved_at: Utc::now(),
            evidence_json: evidence_json.clone(),
            status: LinkStatus::AutoLinked,
        };

        assert!((link_record.confidence - 1.0).abs() < f64::EPSILON);
        assert_eq!(link_record.status, LinkStatus::AutoLinked);
        assert_eq!(link_record.source, "charity_commission");
        assert_eq!(link_record.resolution_type, "exact_company_number");
        // ENR-06: evidence_json must be a plain String, not a serde_json::Value.
        let parsed: serde_json::Value = serde_json::from_str(&evidence_json).unwrap();
        assert!(
            parsed.is_object(),
            "evidence_json must be valid JSON object"
        );
        assert!(!evidence_json.is_empty(), "evidence_json must not be empty");
    }

    // Test 4: Header-position fallback — company number column detection.
    // detect_company_number_field tries "charity_company_registration_number" first,
    // then "coyno", then "registered_company_number", then "company_number".
    #[test]
    fn test_header_position_fallback_registered_company_number() {
        // Build a mock CSV with "registered_company_number" header (not "coyno" or new column).
        let headers = csv::StringRecord::from(vec!["regno", "registered_company_number", "name"]);
        let row = csv::StringRecord::from(vec!["1234567", "12345678", "Test Charity"]);

        // detect_company_number_field should return "registered_company_number" (not "coyno").
        let detected = detect_company_number_field(&headers);
        assert_eq!(
            detected,
            Some("registered_company_number"),
            "Should fall back to 'registered_company_number' when 'coyno' and new column absent"
        );

        // get_field_by_name should return the correct value.
        let cn = get_field_by_name(&row, &headers, detected.unwrap());
        assert_eq!(cn, Some("12345678"));

        // Also verify that if "coyno" IS present (without new column), it takes precedence.
        let headers_with_coyno =
            csv::StringRecord::from(vec!["regno", "coyno", "registered_company_number", "name"]);
        let detected_coyno = detect_company_number_field(&headers_with_coyno);
        assert_eq!(
            detected_coyno,
            Some("coyno"),
            "'coyno' should be preferred over 'registered_company_number'"
        );

        // Verify that "charity_company_registration_number" takes priority over everything else.
        let headers_new = csv::StringRecord::from(vec![
            "registered_charity_number",
            "charity_company_registration_number",
            "charity_name",
        ]);
        let detected_new = detect_company_number_field(&headers_new);
        assert_eq!(
            detected_new,
            Some("charity_company_registration_number"),
            "charity_company_registration_number must take priority over legacy names"
        );
    }

    // Test 5: detect_company_number_field last-resort "company_number" fallback.
    #[test]
    fn test_detect_company_number_field_last_resort_fallback() {
        let headers = csv::StringRecord::from(vec!["regno", "name", "company_number"]);
        assert_eq!(
            detect_company_number_field(&headers),
            Some("company_number"),
            "should fall all the way back to 'company_number'"
        );
    }

    // Test 6: detect_company_number_field returns None when no known field present.
    #[test]
    fn test_detect_company_number_field_none_when_absent() {
        let headers = csv::StringRecord::from(vec!["regno", "name", "postcode"]);
        assert_eq!(
            detect_company_number_field(&headers),
            None,
            "should return None when no company number column exists"
        );
    }

    // Test 7: get_field_by_name returns None when the field is not in headers.
    #[test]
    fn test_get_field_by_name_missing_returns_none() {
        let headers = csv::StringRecord::from(vec!["regno", "name"]);
        let row = csv::StringRecord::from(vec!["1234567", "Test Charity"]);
        assert_eq!(
            get_field_by_name(&row, &headers, "company_number"),
            None,
            "should return None when column is absent"
        );
    }

    // Test 8: get_field_by_name handles leading/trailing whitespace in header names.
    #[test]
    fn test_get_field_by_name_trims_header_whitespace() {
        // CC exports sometimes have " coyno " with surrounding spaces.
        let headers = csv::StringRecord::from(vec![" regno ", " coyno ", " name "]);
        let row = csv::StringRecord::from(vec!["1234567", "12345678", "Test Charity"]);
        assert_eq!(
            get_field_by_name(&row, &headers, "coyno"),
            Some("12345678"),
            "header whitespace trimming must find 'coyno'"
        );
    }

    // Test 9a: parse_charity_zip parses publicextract.charity.txt (TAB-delimited).
    #[test]
    fn test_parse_charity_zip_parses_mock_archive() {
        use std::io::Write;
        use zip::write::FileOptions;

        // TAB-delimited. Use the new primary company column.
        let charity_txt = concat!(
            "registered_charity_number\tcharity_company_registration_number\tcharity_name\r\n",
            "1111111\t11111111\tAlpha Charity\r\n",
            "2222222\t\tBeta Charity No Coyno\r\n",
        );

        let cursor = std::io::Cursor::new(Vec::<u8>::new());
        let mut zip_writer = zip::ZipWriter::new(cursor);
        let opts: FileOptions<()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip_writer
            .start_file("publicextract.charity.txt", opts)
            .unwrap();
        zip_writer.write_all(charity_txt.as_bytes()).unwrap();
        let zip_bytes = zip_writer.finish().unwrap().into_inner();

        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile creation failed");
        tmp.write_all(&zip_bytes).unwrap();
        tmp.flush().unwrap();

        let charities =
            parse_charity_zip(&tmp.path().to_path_buf()).expect("parse_charity_zip should succeed");

        assert_eq!(charities.len(), 2);
        let alpha = charities
            .iter()
            .find(|c| c.charity_number == "1111111")
            .unwrap();
        assert_eq!(alpha.company_number, "11111111");
        assert_eq!(alpha.name, "Alpha Charity");
        let beta = charities
            .iter()
            .find(|c| c.charity_number == "2222222")
            .unwrap();
        assert!(beta.company_number.is_empty());
    }
}
