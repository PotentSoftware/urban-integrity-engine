//! Land Registry PPD (Price Paid Data) adapter (FLIP-01).
//!
//! Downloads yearly Price Paid Data files from the HM Land Registry open-data
//! endpoint (OGL v3 — no API key required), filters rows to in-scope LPA postcode
//! districts at load time, and writes rows to `ppd_transactions` via idempotent
//! UPSERT keyed on the PPD transaction UUID.
//!
//! Raw PPD rows are NEVER emitted as `GraphMutation`s — this is a staging-only
//! adapter (FLIP-01; mirrors the ICIJ staging pattern).
//!
//! # Security
//! - T-23-DoS:    500 MiB cap. Content-Length > `MAX_TXT_BYTES` → WARN + skip, but that header
//!   is advisory (absent under chunked encoding, and server-supplied); the binding guard is the
//!   in-flight counter that stops the download at the cap.
//! - T-23-Tamper: all PPD field values are written via parameterised `.bind()` —
//!   never string-interpolated into SurrealQL (V5 injection guard).
//! - T-23-OOM:    `PPD_CHUNK = 25_000` + `tokio::task::yield_now()` per chunk
//!   (RocksDB flush; mirrors the CCOD OOM guard in project memory).

use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, NaiveDateTime, TimeZone, Utc};
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_ingestor::address::{extract_house_number, normalise_postcode};

use crate::adapters::{EnricherAdapter, EnrichmentStats};
use urban_integrity_core::config::PpdConfig;

/// Per-yearly-file size guard (T-23-DoS): 500 MiB.
/// The national `pp-complete.txt` (5.1 GB) is never targeted; yearly files top out at ~800 MB.
const MAX_TXT_BYTES: u64 = 500 * 1024 * 1024;

/// Number of UPSERT writes per chunk boundary (T-23-OOM guard).
/// After every `PPD_CHUNK` writes the loop yields back to the Tokio runtime so
/// RocksDB can flush memtables between chunks instead of accumulating the whole
/// run's working set in resident memory (CCOD OOM history — project memory).
const PPD_CHUNK: usize = 25_000;

/// A parsed PPD row (one per data row in the yearly `.txt` file).
#[derive(Debug, Clone)]
pub struct PpdRecord {
    pub transaction_id: String,
    pub price_gbp: i64,
    pub date_of_transfer: chrono::DateTime<Utc>,
    pub postcode_raw: String,
    pub postcode_norm: String,
    pub property_type: String,
    pub old_new: String,
    pub tenure: String,
    pub paon: String,
    pub paon_norm: String,
    pub saon: Option<String>,
    pub street: String,
    pub street_norm: String,
    pub locality: Option<String>,
    pub town: String,
    pub district: String,
    pub county: String,
    pub ppd_category: String,
    pub district_code: String,
}

/// Extract the postcode district (outward code) from a full postcode.
///
/// E.g. `"LS1 1AB"` → `"LS1"`, `"SW1A 2AA"` → `"SW1A"`.
/// The outward code is everything before the first whitespace.
/// Returns uppercase; empty string if no whitespace is found.
pub fn postcode_district(pc: &str) -> String {
    pc.split_whitespace().next().unwrap_or("").to_uppercase()
}

/// Normalise a street name for gate-1 structural matching:
/// lowercase, keep only alphanumerics and spaces, collapse runs of whitespace.
pub fn normalise_street(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a PPD date string (`YYYY-MM-DD HH:MM`) into a UTC `DateTime`.
///
/// Falls back to date-only parse (`%Y-%m-%d`) when the time component is absent.
pub fn parse_ppd_date(s: &str) -> Option<chrono::DateTime<Utc>> {
    // Primary: "YYYY-MM-DD HH:MM"
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M") {
        return Some(Utc.from_utc_datetime(&ndt));
    }
    // Fallback: date-only
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| Utc.from_utc_datetime(&ndt))
}

/// Parse all valid PPD rows from a plain-text reader synchronously (bounded-memory, FLIP-01).
///
/// Intended to run inside `tokio::task::spawn_blocking`.
///
/// PPD format (no header row, positional columns):
///  0: Transaction unique identifier (wrapped in `{…}` — strip braces)
///  1: Price paid (GBP integer)
///  2: Date of transfer ("YYYY-MM-DD HH:MM")
///  3: Postcode
///  4: Property type (D/S/T/F/O)
///  5: Old/new (Y/N)
///  6: Duration (F = Freehold, L = Leasehold, U = Unknown)
///  7: PAON (primary addressable object name — usually house number)
///  8: SAON (secondary addressable object name — optional flat/unit)
///  9: Street
/// 10: Locality (optional)
/// 11: Town/city
/// 12: District
/// 13: County
/// 14: PPD category (A/B)
///
/// Rows whose outward postcode district is not in `allowed_districts` are skipped
/// at parse time (LPA filter, D-04) — the 5.1 GB national file is never buffered.
///
/// Exposed as `pub` so unit tests can call it without HTTP.
pub fn stream_ppd_from_reader<R: Read>(
    reader: R,
    allowed_districts: &HashSet<String>,
) -> anyhow::Result<Vec<PpdRecord>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false) // CRITICAL — PPD has NO header row (unlike CCOD)
        .flexible(true)
        .from_reader(reader);

    let mut records: Vec<PpdRecord> = Vec::new();

    for result in rdr.records() {
        let row = match result {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "PPD: CSV parse error — skipping row");
                continue;
            }
        };

        // PPD yearly file has 15 columns; pp-complete.txt adds a 16th Record Status.
        // Guard against malformed rows without the minimum 15 columns.
        if row.len() < 15 {
            warn!(
                cols = row.len(),
                "PPD: row has fewer than 15 columns — skipping"
            );
            continue;
        }

        // Col 0: transaction_id — strip surrounding `{` `}` braces (PPD format artifact).
        let transaction_id = row
            .get(0)
            .unwrap_or("")
            .trim_matches(['{', '}'])
            .to_string();
        if transaction_id.is_empty() {
            continue;
        }

        // Col 3: postcode — derive district for LPA filter BEFORE parsing the full row.
        let postcode_raw = row.get(3).unwrap_or("").trim().to_string();
        let district_code = postcode_district(&postcode_raw);

        // LPA filter: skip rows outside the in-scope allowlist (D-04).
        if !allowed_districts.contains(&district_code) {
            continue;
        }

        // Col 1: price paid
        let price_gbp: i64 = row.get(1).unwrap_or("").trim().parse().unwrap_or(0);

        // Col 2: date of transfer
        let date_str = row.get(2).unwrap_or("");
        let Some(date_of_transfer) = parse_ppd_date(date_str) else {
            warn!(date = date_str, transaction_id = %transaction_id, "PPD: unparseable date — skipping row");
            continue;
        };

        // Postcode normalisation for gate-1
        let postcode_norm = match normalise_postcode(&postcode_raw) {
            Some(p) => p,
            None => {
                warn!(postcode = %postcode_raw, "PPD: invalid postcode — skipping row");
                continue;
            }
        };

        // Cols 4–6: property_type, old_new, tenure
        let property_type = row.get(4).unwrap_or("").trim().to_string();
        let old_new = row.get(5).unwrap_or("").trim().to_string();
        let tenure = row.get(6).unwrap_or("").trim().to_string();

        // Col 7: PAON — normalise as house number for gate-1
        let paon = row.get(7).unwrap_or("").trim().to_string();
        let paon_norm = extract_house_number(&paon);

        // Col 8: SAON — optional secondary addressable object
        let saon_raw = row.get(8).unwrap_or("").trim().to_string();
        let saon = if saon_raw.is_empty() {
            None
        } else {
            Some(saon_raw)
        };

        // Col 9: Street — normalise for gate-1
        let street = row.get(9).unwrap_or("").trim().to_string();
        let street_norm = normalise_street(&street);

        // Col 10: Locality — optional
        let locality_raw = row.get(10).unwrap_or("").trim().to_string();
        let locality = if locality_raw.is_empty() {
            None
        } else {
            Some(locality_raw)
        };

        // Cols 11–14: town, district, county, ppd_category
        let town = row.get(11).unwrap_or("").trim().to_string();
        let district = row.get(12).unwrap_or("").trim().to_string();
        let county = row.get(13).unwrap_or("").trim().to_string();
        let ppd_category = row.get(14).unwrap_or("").trim().to_string();

        records.push(PpdRecord {
            transaction_id,
            price_gbp,
            date_of_transfer,
            postcode_raw,
            postcode_norm,
            property_type,
            old_new,
            tenure,
            paon,
            paon_norm,
            saon,
            street,
            street_norm,
            locality,
            town,
            district,
            county,
            ppd_category,
            district_code,
        });
    }

    Ok(records)
}

/// Land Registry PPD bulk-text adapter (FLIP-01).
///
/// Enabled (`is_enabled() == true`) only when `ppd.enabled = true` in config.
/// PPD is OGL v3 open data — no API key required.
pub struct LandRegistryPpdAdapter {
    pub config: PpdConfig,
    pub cache_dir: PathBuf,
}

impl LandRegistryPpdAdapter {
    pub fn new(config: PpdConfig, cache_dir: PathBuf) -> Self {
        Self { config, cache_dir }
    }
}

#[async_trait]
impl EnricherAdapter for LandRegistryPpdAdapter {
    fn name(&self) -> &'static str {
        "ppd"
    }

    fn is_enabled(&self) -> bool {
        self.config.is_enabled()
    }

    /// HMLR Price Paid Data is England and Wales only. In a Scottish analysis the derived
    /// postcode-district allowlist matches nothing, and land-flip gate-1 reports
    /// `gate1_no_ppd_match` — which reads as "no transactions here", not "this registry
    /// does not cover this country".
    fn coverage(&self) -> urban_integrity_schema::nation::Jurisdiction {
        urban_integrity_schema::nation::Jurisdiction::ENGLAND_AND_WALES
    }

    async fn run(
        &self,
        db: &Surreal<Any>,
        _mutation_tx: &Sender<GraphMutation>,
        // mutation_tx is intentionally UNUSED — PPD emits NO GraphMutation (staging-only, FLIP-01).
    ) -> anyhow::Result<EnrichmentStats> {
        if !self.is_enabled() {
            warn!(
                adapter = "land_registry_ppd",
                "Adapter not enabled — set enricher.ppd.enabled = true to activate"
            );
            return Ok(EnrichmentStats::default());
        }

        // --- Step 1: derive allowed-districts allowlist (D-04) -------------------
        // Query existing Location-node postcodes from the graph and extract districts.
        // Fallback: config.postcode_districts override (D-04).
        let allowed_districts: HashSet<String> =
            if let Some(ref districts) = self.config.postcode_districts {
                if !districts.is_empty() {
                    districts.iter().map(|s| s.to_uppercase()).collect()
                } else {
                    derive_districts_from_db(db).await
                }
            } else {
                derive_districts_from_db(db).await
            };

        if allowed_districts.is_empty() {
            warn!(
                adapter = "land_registry_ppd",
                "No postcode districts in allowlist — no rows will be loaded (D-04)"
            );
            return Ok(EnrichmentStats::default());
        }

        info!(
            adapter = "land_registry_ppd",
            district_count = allowed_districts.len(),
            "PPD district allowlist derived"
        );

        // --- Step 2: download and stage yearly files ----------------------------
        let current_year = chrono::Utc::now().year();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()?;

        std::fs::create_dir_all(&self.cache_dir)?;

        let mut total_written = 0usize;

        for offset in 0..self.config.years_back {
            let year = current_year - offset as i32;
            let url =
                format!("https://price-paid-data.publicdata.landregistry.gov.uk/pp-{year}.txt");

            info!(
                adapter = "land_registry_ppd",
                year, "Downloading PPD yearly file"
            );
            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!(adapter = "land_registry_ppd", year, error = %e, "PPD download failed — skipping year");
                    continue;
                }
            };

            if !resp.status().is_success() {
                warn!(
                    adapter = "land_registry_ppd",
                    year,
                    status = %resp.status(),
                    "PPD download returned non-200 status — skipping year"
                );
                continue;
            }

            // T-23-DoS: content-length guard before buffering (500 MiB limit).
            // Advisory only — `content_length()` is `None` under chunked transfer-encoding and
            // is server-supplied either way, so it can be absent or simply wrong. The in-flight
            // counter below is the guard that actually holds.
            if let Some(len) = resp.content_length() {
                if len > MAX_TXT_BYTES {
                    warn!(
                        adapter = "land_registry_ppd",
                        year,
                        content_length = len,
                        max_bytes = MAX_TXT_BYTES,
                        "PPD file exceeds 500 MiB limit — skipping (T-23-DoS)"
                    );
                    continue;
                }
            }

            // T-23-DoS: the binding guard — enforced on bytes ACTUALLY received. The
            // downstream `spawn_blocking` parse was already bounded (FLIP-01); the DOWNLOAD
            // was not, which is what this closes. See `adapters::download`.
            let Some(buf) = crate::adapters::download::read_to_vec_capped(
                resp,
                MAX_TXT_BYTES,
                "land_registry_ppd",
            )
            .await?
            else {
                continue;
            };
            let reader = std::io::Cursor::new(buf);

            // --- Step 3: parse CSV in spawn_blocking (bounded memory, FLIP-01) ---
            let allowed_clone = allowed_districts.clone();
            let records: Vec<PpdRecord> =
                tokio::task::spawn_blocking(move || stream_ppd_from_reader(reader, &allowed_clone))
                    .await??;

            info!(
                adapter = "land_registry_ppd",
                year,
                record_count = records.len(),
                "PPD rows parsed after LPA filter"
            );

            // --- Step 4: idempotent UPSERT into ppd_transactions (D-06) ----------
            // Transaction UUID (col 0, braces stripped) is the natural key.
            // NEVER uuid::Uuid::new_v4() — breaks idempotent re-run.
            for rec in &records {
                // CRITICAL (date coercion): `date_of_transfer` is bound as a chrono
                // `DateTime<Utc>` so SurrealDB serialises it as a real `datetime`. Routing it
                // through `serde_json::json!` (a CONTENT blob) serialises chrono to an RFC3339
                // *string*, which SurrealDB 3.x rejects ("Expected `datetime` but found '...'").
                // The previous CONTENT write ALSO never called `.take()`/`.check()`, so every
                // row failed coercion silently and ZERO rows were staged. All-SET with bound
                // primitives (mirrors `write_identity_link`) + `.take(0)?` to surface errors.
                // V5 / T-23-Tamper: parameterised binds only — never string-interpolate.
                db.query(
                    "UPSERT type::record('ppd_transactions', $key) SET \
                       transaction_id = $transaction_id, price_gbp = $price_gbp, \
                       date_of_transfer = $date_of_transfer, postcode_raw = $postcode_raw, \
                       postcode_norm = $postcode_norm, property_type = $property_type, \
                       old_new = $old_new, tenure = $tenure, paon = $paon, \
                       paon_norm = $paon_norm, saon = $saon, street = $street, \
                       street_norm = $street_norm, locality = $locality, town = $town, \
                       district = $district, county = $county, \
                       ppd_category = $ppd_category, district_code = $district_code",
                )
                .bind(("key", rec.transaction_id.clone()))
                .bind(("transaction_id", rec.transaction_id.clone()))
                .bind(("price_gbp", rec.price_gbp))
                .bind(("date_of_transfer", rec.date_of_transfer))
                .bind(("postcode_raw", rec.postcode_raw.clone()))
                .bind(("postcode_norm", rec.postcode_norm.clone()))
                .bind(("property_type", rec.property_type.clone()))
                .bind(("old_new", rec.old_new.clone()))
                .bind(("tenure", rec.tenure.clone()))
                .bind(("paon", rec.paon.clone()))
                .bind(("paon_norm", rec.paon_norm.clone()))
                .bind(("saon", rec.saon.clone()))
                .bind(("street", rec.street.clone()))
                .bind(("street_norm", rec.street_norm.clone()))
                .bind(("locality", rec.locality.clone()))
                .bind(("town", rec.town.clone()))
                .bind(("district", rec.district.clone()))
                .bind(("county", rec.county.clone()))
                .bind(("ppd_category", rec.ppd_category.clone()))
                .bind(("district_code", rec.district_code.clone()))
                .await?
                .take::<Vec<serde_json::Value>>(0)?;

                total_written += 1;

                // T-23-OOM: chunk-yield guard (RocksDB flush between chunks).
                if total_written.is_multiple_of(PPD_CHUNK) {
                    info!(
                        adapter = "land_registry_ppd",
                        written = total_written,
                        "PPD chunk boundary — yielding for RocksDB flush"
                    );
                    tokio::task::yield_now().await;
                }
            }

            info!(
                adapter = "land_registry_ppd",
                year,
                written_this_year = records.len(),
                total_written,
                "PPD year complete"
            );
        }

        info!(
            adapter = "land_registry_ppd",
            total_written, "PPD staging complete"
        );

        Ok(EnrichmentStats {
            candidates_found: total_written,
            auto_linked: 0,
            pending_review: 0,
            sub_candidate: 0,
            discarded: 0,
            skipped_out_of_jurisdiction: 0,
        })
    }
}

/// Whether the projected tables reflect a `project` run that ran to completion.
///
/// `None` — no marker at all — means the database predates the marker (or has never been
/// projected). That is reported as complete so existing deployments keep working, but the
/// caller warns: it is "unknown", not "verified".
fn projection_is_complete(marker: Option<&serde_json::Value>) -> bool {
    match marker {
        None => true,
        Some(row) => row
            .get("finished_at_ms")
            .is_some_and(|v| !v.is_null() && v.as_i64().is_some_and(|ms| ms > 0)),
    }
}

/// Derive the set of in-scope postcode districts from existing Location nodes (D-04).
///
/// Queries `SELECT postcode FROM location WHERE postcode != NONE`, then extracts
/// the outward code (district) from each postcode. Returns an empty set on failure
/// (graceful degradation — the adapter will WARN and skip loading).
async fn derive_districts_from_db(db: &Surreal<Any>) -> HashSet<String> {
    // The allowlist decides what gets INGESTED, so a partial `location` table does not
    // degrade the result — it discards source data, permanently, with no counter. That is
    // exactly what happened: the 2026-08-11 `project` run was killed 12.6% through, leaving
    // 2,051 districts against the log's 2,803, and every PPD transaction in the missing 752
    // was dropped at ingest. A later `project` run does not bring them back; only a
    // re-ingest does.
    //
    // So refuse to build an allowlist from a projection that is known to be partial. An
    // empty set makes the caller WARN and skip loading, which is recoverable; a silently
    // short set is not.
    let marker: surrealdb::Result<Vec<serde_json::Value>> = db
        .query("SELECT * FROM projection_runs:current")
        .await
        .and_then(|mut r| r.take(0));

    // Three states, not two. `.ok()` collapsed a FAILED read into "no marker", which
    // `projection_is_complete(None)` reports as complete — so a wedged instance or an auth
    // failure walked straight past the refusal this function exists to perform.
    let marker_row = match marker {
        Ok(rows) => rows.into_iter().next(),
        Err(e) => {
            warn!(
                adapter = "land_registry_ppd",
                error = %e,
                "Could not read the projection marker, so the `location` table's completeness is \
                 unverified. Refusing to derive an ingest allowlist from it: a silently short \
                 allowlist discards source data permanently, whereas refusing is recoverable."
            );
            return HashSet::new();
        }
    };

    if marker_row.is_none() {
        // The warn `projection_is_complete`'s doc has always promised for this case, and which
        // did not exist: the only warn was in the `!complete` branch below.
        warn!(
            adapter = "land_registry_ppd",
            "No projection marker — allowlist completeness is UNKNOWN, not verified. This \
             database predates the marker or has never been projected."
        );
    }

    if !projection_is_complete(marker_row.as_ref()) {
        warn!(
            adapter = "land_registry_ppd",
            "The last `project` run never finished — the `location` table is a partial rebuild. \
             Refusing to derive an ingest allowlist from it: every transaction in a missing \
             postcode district would be discarded silently and a later projection would not \
             recover it. Re-run `project`, then re-run this ingest."
        );
        return HashSet::new();
    }

    let rows: surrealdb::Result<Vec<serde_json::Value>> = db
        .query("SELECT postcode FROM location WHERE postcode != NONE")
        .await
        .and_then(|mut r| r.take(0));

    match rows {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                row.get("postcode")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(postcode_district)
            })
            .filter(|d| !d.is_empty())
            .collect(),
        Err(e) => {
            warn!(
                adapter = "land_registry_ppd",
                error = %e,
                "Failed to derive postcode districts from Location nodes (D-04) — empty allowlist"
            );
            HashSet::new()
        }
    }
}

#[cfg(test)]
mod tests {
    /// `None` (never projected) is proceed-with-warning; anything unfinished must refuse.
    ///
    /// A query ERROR is a fourth case handled by the caller, not here: it is indistinguishable
    /// from an unfinished projection, so `derive_districts_from_db` refuses on it rather than
    /// letting `.ok()` turn it into `None` and hence "complete".
    #[test]
    fn an_absent_marker_is_distinct_from_an_unfinished_one() {
        use super::projection_is_complete;
        assert!(
            projection_is_complete(None),
            "no marker: proceed (with a warn)"
        );
        assert!(
            !projection_is_complete(Some(&serde_json::json!({}))),
            "marker with no finished_at_ms: the run never completed"
        );
        assert!(
            !projection_is_complete(Some(&serde_json::json!({ "finished_at_ms": null }))),
            "explicit null finished_at_ms: the run never completed"
        );
        assert!(
            !projection_is_complete(Some(&serde_json::json!({ "finished_at_ms": 0 }))),
            "zero finished_at_ms is not a completion stamp"
        );
        assert!(projection_is_complete(Some(
            &serde_json::json!({ "finished_at_ms": 1_723_000_000_000i64 })
        )));
    }
    use super::projection_is_complete;

    // ─── projection_is_complete (Open Item 30) ──────────────────────────────

    #[test]
    fn a_finished_run_is_complete() {
        let m = serde_json::json!({ "started_at_ms": 1, "finished_at_ms": 2 });
        assert!(projection_is_complete(Some(&m)));
    }

    #[test]
    fn an_unfinished_run_is_not_complete() {
        // What a killed run leaves: started, never stamped.
        let m = serde_json::json!({ "started_at_ms": 1, "finished_at_ms": null });
        assert!(!projection_is_complete(Some(&m)));
        let missing = serde_json::json!({ "started_at_ms": 1 });
        assert!(
            !projection_is_complete(Some(&missing)),
            "an absent finished_at_ms on a present marker means the run died"
        );
        let zero = serde_json::json!({ "started_at_ms": 1, "finished_at_ms": 0 });
        assert!(!projection_is_complete(Some(&zero)));
    }

    #[test]
    fn no_marker_at_all_is_treated_as_complete() {
        // Databases predating the marker must keep working; the caller warns instead.
        assert!(projection_is_complete(None));
    }

    use super::*;
    use std::io::Cursor;

    /// A minimal PPD fixture row (15 columns, no header row).
    /// Transaction ID is wrapped in `{…}` as the real PPD format uses.
    const FIXTURE_ROW: &str =
        "{A83E1B2C-4321-ABCD-1234-0A0B0C0D0E0F},175000,2024-03-15 00:00,LL65 2AB,D,Y,F,42,,HIGH STREET,,HOLYHEAD,ANGLESEY,ANGLESEY,A";

    /// A second fixture row in a DIFFERENT district (SW1A) — for filter test.
    const FIXTURE_ROW_OUT_OF_SCOPE: &str =
        "{BBBB2222-0000-AAAA-CCCC-111122223333},500000,2024-06-01 00:00,SW1A 2AA,F,N,L,10,,DOWNING STREET,,LONDON,WESTMINSTER,GREATER LONDON,A";

    /// `stream_ppd_from_reader` parses a no-header fixture row correctly.
    /// Verifies: `{}` braces stripped, price parsed, date parsed, postcode normalised,
    /// paon_norm from extract_house_number, district_code from outward code.
    #[test]
    fn land_registry_ppd_parses_fixture_row() {
        let csv_input = format!("{}\n", FIXTURE_ROW);
        let mut allowed = HashSet::new();
        allowed.insert("LL65".to_string());

        let records = stream_ppd_from_reader(Cursor::new(csv_input.as_bytes()), &allowed)
            .expect("stream_ppd_from_reader should not error");

        assert_eq!(
            records.len(),
            1,
            "Expected exactly 1 record from the fixture"
        );
        let r = &records[0];

        // Braces stripped from transaction_id.
        assert_eq!(r.transaction_id, "A83E1B2C-4321-ABCD-1234-0A0B0C0D0E0F");

        // Price and date parsed correctly.
        assert_eq!(r.price_gbp, 175_000);
        assert_eq!(r.date_of_transfer.year(), 2024);
        assert_eq!(r.date_of_transfer.month(), 3);
        assert_eq!(r.date_of_transfer.day(), 15);

        // Postcode normalised (spaces stripped, uppercase).
        assert_eq!(r.postcode_raw, "LL65 2AB");
        assert_eq!(r.postcode_norm, "LL652AB");

        // PAON norm: extract_house_number("42") → "42".
        assert_eq!(r.paon, "42");
        assert_eq!(r.paon_norm, "42");

        // Street norm: lowercase + alphanumeric collapse.
        assert_eq!(r.street, "HIGH STREET");
        assert_eq!(r.street_norm, "high street");

        // District code from outward postcode.
        assert_eq!(r.district_code, "LL65");

        // SAON is empty → None.
        assert!(r.saon.is_none());

        // Locality is empty → None.
        assert!(r.locality.is_none());

        // Other fields.
        assert_eq!(r.property_type, "D");
        assert_eq!(r.old_new, "Y");
        assert_eq!(r.tenure, "F");
        assert_eq!(r.town, "HOLYHEAD");
        assert_eq!(r.ppd_category, "A");
    }

    /// A row whose district is NOT in the allowlist must be absent from the returned Vec.
    #[test]
    fn ppd_filter_excludes_out_of_scope_district() {
        let csv_input = format!("{}\n{}\n", FIXTURE_ROW, FIXTURE_ROW_OUT_OF_SCOPE);
        let mut allowed = HashSet::new();
        allowed.insert("LL65".to_string());
        // SW1A is NOT in the allowlist.

        let records = stream_ppd_from_reader(Cursor::new(csv_input.as_bytes()), &allowed)
            .expect("stream_ppd_from_reader should not error");

        assert_eq!(
            records.len(),
            1,
            "Only the LL65 row should survive the filter"
        );
        assert_eq!(records[0].district_code, "LL65");
    }

    /// When `allowed_districts` is empty, every row is filtered out.
    #[test]
    fn ppd_filter_empty_allowlist_yields_no_records() {
        let csv_input = format!("{}\n", FIXTURE_ROW);
        let allowed: HashSet<String> = HashSet::new();

        let records = stream_ppd_from_reader(Cursor::new(csv_input.as_bytes()), &allowed)
            .expect("stream_ppd_from_reader should not error");

        assert!(
            records.is_empty(),
            "Empty allowlist must produce zero records"
        );
    }

    /// The adapter MUST NEVER construct a `GraphMutation::AddNode`.
    /// This is a source-level assertion: the adapter's run() signature accepts mutation_tx
    /// but the implementation never calls mutation_tx.send.
    /// We verify the adapter type implements the trait but has the correct name.
    #[test]
    fn ppd_adapter_name_is_ppd() {
        let adapter = LandRegistryPpdAdapter::new(PpdConfig::default(), PathBuf::from("data/ppd"));
        assert_eq!(adapter.name(), "ppd");
    }

    /// Disabled adapter returns is_enabled() = false when config.enabled = false.
    #[test]
    fn ppd_adapter_disabled_by_default() {
        let adapter = LandRegistryPpdAdapter::new(PpdConfig::default(), PathBuf::from("data/ppd"));
        assert!(
            !adapter.is_enabled(),
            "PPD adapter must be disabled by default"
        );
    }

    /// Enabled adapter returns is_enabled() = true.
    #[test]
    fn ppd_adapter_enabled_when_config_true() {
        let cfg = PpdConfig {
            enabled: true,
            ..Default::default()
        };
        let adapter = LandRegistryPpdAdapter::new(cfg, PathBuf::from("data/ppd"));
        assert!(adapter.is_enabled());
    }

    /// `postcode_district` extracts the outward code (before whitespace).
    #[test]
    fn ppd_postcode_district_extracts_outward_code() {
        assert_eq!(postcode_district("LS1 1AB"), "LS1");
        assert_eq!(postcode_district("SW1A 2AA"), "SW1A");
        assert_eq!(postcode_district("LL65 2AB"), "LL65");
        assert_eq!(postcode_district(""), "");
    }

    /// The `normalise_street` helper lowercases and collapses whitespace.
    #[test]
    fn ppd_normalise_street_lowercases_and_collapses() {
        assert_eq!(normalise_street("HIGH STREET"), "high street");
        assert_eq!(normalise_street("  St. Mary's Road  "), "st marys road");
        assert_eq!(normalise_street(""), "");
    }

    /// D-08 pin: `normalise_street`'s output for PPD-shaped inputs must never drift for the
    /// rest of this phase. This is deliberately a pin of CURRENT behavior, not a new
    /// capability — no production code in this file changes. In particular, the apostrophe
    /// in "O'Connell Street" is DELETED (not spaced) by the current implementation; this is
    /// the exact current behavior and must NOT be "fixed" here or anywhere in this phase.
    #[test]
    fn normalise_street_ppd_output_unchanged_by_planning_parser_work() {
        assert_eq!(normalise_street("Crigyll Road"), "crigyll road");
        assert_eq!(normalise_street("Parc Tyddyn Bach"), "parc tyddyn bach");
        assert_eq!(normalise_street("Market Hill"), "market hill");
        assert_eq!(normalise_street("HIGH STREET"), "high street");
        assert_eq!(normalise_street("O'Connell Street"), "oconnell street");
    }

    // ── Integration tests (URBAN_DB_TESTS=1 required) ─────────────────────────
    //
    // These tests require a live SurrealDB at ws://localhost:8000 and the schema
    // already applied. Skip by returning early when URBAN_DB_TESTS is unset so the
    // default `cargo test` unit run is unaffected (mirrors the CCOD adapter pattern).

    /// Connect to the test SurrealDB instance.
    async fn connect_test_db() -> urban_integrity_test_support::TestNs {
        urban_integrity_test_support::TestNs::connect("integrity").await
    }

    /// Idempotency test: loading the same PPD fixture twice yields identical
    /// `ppd_transactions` row counts (FLIP-01, D-06).
    ///
    /// Proves that the UNIQUE-keyed UPSERT on `transaction_id` overwrites in-place
    /// rather than appending a second row on re-ingestion.
    ///
    /// Requires `URBAN_DB_TESTS=1` and a live SurrealDB at `ws://localhost:8000`.
    #[tokio::test]
    async fn ppd_idempotent() {
        // Gate: skip when URBAN_DB_TESTS is not set — never fail the unit run.
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }

        let db = connect_test_db().await;

        // Two fixture rows in the LL65 district (both within the allowlist).
        // Transaction IDs are deterministic — re-running will hit the UPSERT path.
        let fixture = concat!(
            "{AA000001-0000-0000-0000-000000000001},175000,2024-03-15 00:00,LL65 2AB,D,Y,F,42,,HIGH STREET,,HOLYHEAD,ANGLESEY,ANGLESEY,A\n",
            "{AA000002-0000-0000-0000-000000000002},200000,2024-06-01 00:00,LL65 3CD,S,N,F,10,,MARKET STREET,,HOLYHEAD,ANGLESEY,ANGLESEY,A\n",
        );

        let mut allowed = HashSet::new();
        allowed.insert("LL65".to_string());

        let fixture_ids = vec![
            "AA000001-0000-0000-0000-000000000001",
            "AA000002-0000-0000-0000-000000000002",
        ];

        // Clean up any stale fixture rows from a previous run.
        for id in &fixture_ids {
            db.query("DELETE type::record('ppd_transactions', $key)")
                .bind(("key", *id))
                .await
                .ok();
        }

        // Helper closure: load fixture rows into ppd_transactions via the same
        // UPSERT path the adapter uses.
        let load_fixture = |records: Vec<PpdRecord>| {
            let db = db.clone();
            async move {
                for rec in &records {
                    // CRITICAL (date coercion): bind `date_of_transfer` as a chrono
                    // `DateTime<Utc>` via all-SET — NOT through `serde_json::json!` (a CONTENT
                    // blob), which serialises chrono to an RFC3339 *string* that SurrealDB 3.x
                    // refuses to coerce into a `datetime` field, silently dropping the row.
                    // `.take(0)` surfaces any coercion error instead of swallowing it. This
                    // mirrors the adapter's real staging path (see `run` above).
                    // V5 / T-23-Tamper: parameterised binds only — never string-interpolate.
                    db.query(
                        "UPSERT type::record('ppd_transactions', $key) SET \
                           transaction_id = $transaction_id, price_gbp = $price_gbp, \
                           date_of_transfer = $date_of_transfer, postcode_raw = $postcode_raw, \
                           postcode_norm = $postcode_norm, property_type = $property_type, \
                           old_new = $old_new, tenure = $tenure, paon = $paon, \
                           paon_norm = $paon_norm, saon = $saon, street = $street, \
                           street_norm = $street_norm, locality = $locality, town = $town, \
                           district = $district, county = $county, \
                           ppd_category = $ppd_category, district_code = $district_code",
                    )
                    .bind(("key", rec.transaction_id.clone()))
                    .bind(("transaction_id", rec.transaction_id.clone()))
                    .bind(("price_gbp", rec.price_gbp))
                    .bind(("date_of_transfer", rec.date_of_transfer))
                    .bind(("postcode_raw", rec.postcode_raw.clone()))
                    .bind(("postcode_norm", rec.postcode_norm.clone()))
                    .bind(("property_type", rec.property_type.clone()))
                    .bind(("old_new", rec.old_new.clone()))
                    .bind(("tenure", rec.tenure.clone()))
                    .bind(("paon", rec.paon.clone()))
                    .bind(("paon_norm", rec.paon_norm.clone()))
                    .bind(("saon", rec.saon.clone()))
                    .bind(("street", rec.street.clone()))
                    .bind(("street_norm", rec.street_norm.clone()))
                    .bind(("locality", rec.locality.clone()))
                    .bind(("town", rec.town.clone()))
                    .bind(("district", rec.district.clone()))
                    .bind(("county", rec.county.clone()))
                    .bind(("ppd_category", rec.ppd_category.clone()))
                    .bind(("district_code", rec.district_code.clone()))
                    .await
                    .expect("UPSERT ppd_transactions query")
                    .take::<Vec<serde_json::Value>>(0)
                    .expect("UPSERT ppd_transactions should not fail coercion");
                }
            }
        };

        // --- First load ----------------------------------------------------------
        let records_1 = stream_ppd_from_reader(std::io::Cursor::new(fixture.as_bytes()), &allowed)
            .expect("parse fixture (first load)");
        assert_eq!(records_1.len(), 2, "fixture must parse 2 rows");

        load_fixture(records_1).await;

        // Count after first load.
        let count_rows: Vec<serde_json::Value> = db
            .query(
                "SELECT count() FROM ppd_transactions \
                 WHERE transaction_id IN $ids GROUP ALL",
            )
            .bind(("ids", fixture_ids.clone()))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let count_1 = count_rows
            .first()
            .and_then(|v| v["count"].as_u64())
            .unwrap_or(0);

        // --- Second load (same fixture) -----------------------------------------
        let records_2 = stream_ppd_from_reader(std::io::Cursor::new(fixture.as_bytes()), &allowed)
            .expect("parse fixture (second load)");

        load_fixture(records_2).await;

        // Count after second load.
        let count_rows_2: Vec<serde_json::Value> = db
            .query(
                "SELECT count() FROM ppd_transactions \
                 WHERE transaction_id IN $ids GROUP ALL",
            )
            .bind(("ids", fixture_ids.clone()))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let count_2 = count_rows_2
            .first()
            .and_then(|v| v["count"].as_u64())
            .unwrap_or(0);

        // --- Assert idempotency -------------------------------------------------
        assert_eq!(
            count_1, count_2,
            "Re-ingesting the same fixture must produce identical ppd_transactions \
             row counts (UNIQUE-keyed UPSERT on transaction_id, D-06)"
        );
        assert_eq!(count_1, 2, "Both fixture rows must have been written");

        // --- Cleanup ------------------------------------------------------------
        for id in &fixture_ids {
            db.query("DELETE type::record('ppd_transactions', $key)")
                .bind(("key", *id))
                .await
                .ok();
        }
    }
}
