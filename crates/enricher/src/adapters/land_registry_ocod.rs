//! Land Registry OCOD (Overseas Companies that Own (UK) property Data) adapter
//! (Phase 22, PROP-05, PROP-06, PROP-04).
//!
//! A near-exact clone of [`crate::adapters::land_registry_ccod`] with five deltas:
//! 1. OCOD endpoint / resource filter / cache filename.
//! 2. `OcodRecord` carries `proprietor_name` + canonical `country_incorporated`,
//!    parsed per slot by HEADER NAME (never by index — Pitfall 6) and
//!    canonicalised BEFORE any jurisdiction filter (PROP-05 ordering invariant).
//! 3. Company-number handling accepts ROE `OE######` proprietors via the
//!    `GB-ROE:` node-key form (UK numbers keep `GB-COH:`).
//! 4. Emitted `PropertyData.country_incorporated` is set to the canonical token.
//! 5. A bounded OCOD↔ICIJ name-only matcher (PROP-06) that is hard-capped at
//!    0.65 and FORCED to `pending_review` — it must NEVER auto-link.
//!
//! # Security
//! - T-22-03-04: `api_key` is never logged — only `api_key_present = true/false`.
//! - T-22-03-01: ZIP-bomb DoS guard. Content-Length > 2 GiB → WARN + skip, but that header is
//!   advisory (absent under chunked encoding, and server-supplied); the binding guard is the
//!   in-flight byte counter that aborts the download once 2 GiB have actually been received.
//! - T-22-03-05: OCOD↔ICIJ links can never auto-link (cap 0.65 + PendingReview).
//! - T-22-03-04 (ENR-06): `evidence_json` is always a flat `String`.

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, OwnsPropertyData};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{NodeWeight, OffshoreEntityData, PropertyData};

use crate::adapters::ch_psc::write_identity_link;
use crate::adapters::land_registry_ccod::{get_field_by_name, parse_ccod_date};
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{IdentityLinkRecord, LinkStatus};
use crate::country_canonical::canonicalise_country;

/// HMLR "Use Land and Property Data" OCOD dataset endpoint.
const OCOD_API_URL: &str = "https://use-land-property-data.service.gov.uk/api/v1/datasets/ocod";

/// Maximum ZIP file size (2 GiB) — T-22-03-01 DoS guard.
const MAX_ZIP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// OCOD proprietor slot indices — 4 slots per row (shares the CCOD layout).
const PROPRIETOR_SLOTS: &[usize] = &[1, 2, 3, 4];

/// PROP-06 hard cap for any OCOD↔ICIJ name-only match.
const OCOD_ICIJ_NAME_ONLY_CAP: f64 = 0.65;

/// A parsed OCOD row entry — one per non-empty proprietor slot.
#[derive(Debug, Clone)]
pub struct OcodRecord {
    pub title_number: String,
    pub tenure: String,
    pub price_paid: Option<f64>,
    /// Proprietor identifier: a UK CH number (`12345678`/`SC123456`) or an ROE
    /// number (`OE######`). Used to form the owner node key.
    pub company_number: String,
    /// Proprietor (overseas company) name — the only field the ICIJ matcher can use.
    pub proprietor_name: String,
    /// Canonical country of incorporation (already passed through
    /// `canonicalise_country`; unmapped values are retained verbatim, never dropped).
    pub country_incorporated: String,
    pub proprietor_slot: usize,
    pub date_proprietor_added: Option<chrono::DateTime<Utc>>,
}

/// True when `s` is an ROE (Register of Overseas Entities) number: `OE` + 6 digits.
///
/// ROE proprietors are the post-2022 overseas-entity identifiers OCOD uses; the
/// node-key form is `GB-ROE:OE######` so the owner edge `in` side is unambiguous.
pub fn is_roe_number(s: &str) -> bool {
    let s = s.trim();
    s.len() == 8
        && s.as_bytes()[..2].eq_ignore_ascii_case(b"OE")
        && s.as_bytes()[2..].iter().all(|b| b.is_ascii_digit())
}

/// How an OCOD proprietor's owner node is keyed.
///
/// OCOD is an **overseas-only** register, so a proprietor is NEVER a UK Companies
/// House entity. We therefore never use the `GB-COH:` namespace here — keying an
/// overseas company as `GB-COH:<n>` would falsely attribute its property to an
/// unrelated UK company sharing that number (a defamation-class mis-attribution,
/// the cardinal sin). The original CCOD-clone wrongly imported CCOD's UK-company
/// owner model; this is the corrected, OCOD-appropriate model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    /// Registered on the UK Register of Overseas Entities (`OE######`).
    Roe,
    /// Any other overseas proprietor (foreign company number, or none).
    Overseas,
}

/// Slugify a raw token for use inside a deterministic node id: lowercase, runs of
/// non-alphanumeric collapsed to a single `-`, trimmed. Empty input → `"unknown"`.
pub fn slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_dash = false;
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "unknown".to_string()
    } else {
        s
    }
}

/// True when `cn`, after stripping any leading ASCII-alphabetic prefix, is
/// either empty or an all-same-character digit sequence — a placeholder/
/// sentinel company number (D-11) that must NOT be trusted to uniquely
/// identify a company.
///
/// This is a **structural** predicate, not a hardcoded list. The live corpus's
/// discovered sentinel is `FC000000` (shared by five unrelated companies across
/// five jurisdictions), but the next sentinel-shaped number — `FC111111`,
/// `00000000`, and so on — is caught the same way. A hardcoded
/// `FC000000`-only check would miss the next one silently, repeating this
/// project's `director:unknown-applicant` failure mode.
///
/// A remainder containing any non-digit character (e.g. the `"C2/GBL"` left
/// after no alpha prefix strips off `"079671C2/GBL"`) is NOT sentinel — it is
/// a genuine, usable identifier and takes the bare-number branch in
/// [`owner_key`].
pub fn is_sentinel_number(cn: &str) -> bool {
    let remainder = cn.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    if remainder.is_empty() {
        return true;
    }
    let mut chars = remainder.chars();
    let first = chars.next().expect("remainder checked non-empty above");
    if !first.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c == first)
}

/// Company numbers that a live measurement proved are carried by more than one unrelated
/// company, so the number does NOT uniquely identify an owner and D-09's bare-number key must
/// not be used for it (D-12).
///
/// This is a narrow, audited exception to D-09, not a general retreat from it. Each entry is a
/// number that:
///   1. is structurally genuine, so [`is_sentinel_number`] does not catch it, and
///   2. was observed live carrying two or more apparently unrelated company names.
///
/// `FC008194` is the only such number in the live corpus as of the 2026-08-22 re-measurement
/// (`28-03-MERGE-SET-REVIEW.md`, re-confirmed at 28-09's D-12 checkpoint): it carries
/// `ROBIN SWINDON LTD` (British Virgin Islands, 1 property) and `CREDIT AGRICOLE CORPORATE AND
/// INVESTMENT BANK` (France, 2 properties). Merging on it would make one node the owner of all
/// three, i.e. a false accumulation signal reaching the property-accumulation and
/// address-cluster detectors — the corruption-shaped artifact those detectors exist to find.
///
/// Adding to this list requires the same evidence: a live measurement showing multiple distinct,
/// non-punctuation-variant names on one number. Benign spelling variants of ONE company (the
/// other five groups in the merge-set review) merge correctly and must stay out of here.
const NON_UNIQUE_COMPANY_NUMBERS: &[&str] = &["fc008194"];

/// True when `cn` is a known non-unique company number (see [`NON_UNIQUE_COMPANY_NUMBERS`]).
///
/// Compares case-insensitively against the trimmed input: source rows are not normalised, so
/// the same number arrives as `FC008194`, `fc008194`, or padded with whitespace. A
/// case-sensitive check would let the excluded number through under a different spelling —
/// exactly the merge it exists to prevent.
pub fn is_non_unique_number(cn: &str) -> bool {
    let needle = cn.trim().to_ascii_lowercase();
    NON_UNIQUE_COMPANY_NUMBERS.contains(&needle.as_str())
}

/// Resolve the owner node key + kind for an OCOD proprietor.
///
/// - ROE `OE######` → `GB-ROE:OE######` (UK-issued overseas-entity id, RESEARCH Q4).
/// - A genuine (non-sentinel) company number → a bare `GB-OCOD:<number>` key
///   (D-09). The number alone uniquely identifies the owner once present, so
///   the jurisdiction term is dropped — this collapses fragmentation caused by
///   the source data spelling the same company's country differently across
///   rows (e.g. EAUBRINK FARM GMBH: `"Germany"` in one row, `"Hamburg"`, a
///   city not a country, in another — same `FC025391`).
/// - No usable number (empty, a sentinel/placeholder like `FC000000` — D-11, or a
///   live-proven non-unique number like `FC008194` — D-12) → the collision-free
///   `GB-OCOD:<country>-<id>` form, unchanged (D-10).
///   Dropping the country here would merge unrelated companies by name alone
///   across jurisdictions, which the exact-match-only policy forbids.
///
/// Never `GB-COH:` (see [`OwnerKind`]). Returns `None` ONLY when the slot has
/// neither a company number nor a name — every other proprietor is keyed and
/// retained (PROP-05 Pitfall 2: overseas ownership is never silently dropped).
pub fn owner_key(
    company_number: &str,
    country: &str,
    proprietor_name: &str,
) -> Option<(String, OwnerKind)> {
    let cn = company_number.trim();
    if is_roe_number(cn) {
        return Some((format!("GB-ROE:{}", cn.to_uppercase()), OwnerKind::Roe));
    }
    if !cn.is_empty() && !is_sentinel_number(cn) && !is_non_unique_number(cn) {
        // D-09: a genuine company number alone identifies the owner.
        return Some((format!("GB-OCOD:{}", slug(cn)), OwnerKind::Overseas));
    }
    // D-10/D-11/D-12: no usable company number — fall back to the country-keyed
    // form, exactly as before. For a sentinel or non-unique `cn`, `id_part` still uses
    // `slug(cn)` as the fallback id_part inside the country-keyed form (only
    // the bare-number path is denied to it).
    let id_part = if !cn.is_empty() {
        slug(cn)
    } else if !proprietor_name.trim().is_empty() {
        slug(proprietor_name)
    } else {
        return None;
    };
    let country_part = if country.trim().is_empty() {
        "unknown".to_string()
    } else {
        slug(country)
    };
    Some((
        format!("GB-OCOD:{country_part}-{id_part}"),
        OwnerKind::Overseas,
    ))
}

/// PROP-06: score + status for an OCOD↔ICIJ match.
///
/// An OCOD↔ICIJ match is ALWAYS name-only — ICIJ records carry no DOB,
/// nationality, or UK company number to corroborate against. So the raw score is
/// hard-capped at [`OCOD_ICIJ_NAME_ONLY_CAP`] (0.65), mirroring the OpenSanctions
/// name-only cap.
///
/// **CRITICAL deviation from the threshold ladder:** 0.65 falls in the
/// `SubCandidate` band, but a false offshore-ownership attribution is
/// defamation-class harm, so the status is FORCED to `PendingReview` (analyst-
/// gated) and never routed by the ladder. It can never auto-link.
pub fn ocod_icij_link_score(raw_name_score: f64) -> (f64, LinkStatus) {
    let translated = raw_name_score.min(OCOD_ICIJ_NAME_ONLY_CAP);
    (translated, LinkStatus::PendingReview)
}

/// Parse all OCOD rows from a ZIP synchronously (bounded-memory, PROP-04).
///
/// Intended to run inside `tokio::task::spawn_blocking`. `pub` so unit tests can
/// drive it without HTTP.
///
/// PROP-05 ordering invariant: the country string is canonicalised BEFORE any
/// jurisdiction handling — a non-standard variant of an in-scope jurisdiction is
/// retained (never silently dropped). Unmapped countries pass through with a WARN.
pub fn stream_from_path(cache_path: &std::path::Path) -> anyhow::Result<Vec<OcodRecord>> {
    let file = std::fs::File::open(cache_path)
        .map_err(|e| anyhow::anyhow!("open OCOD ZIP '{}': {}", cache_path.display(), e))?;

    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| anyhow::anyhow!("read OCOD ZIP: {}", e))?;

    // Pitfall 6: find the .csv entry by extension, not by_index(0).
    let csv_index = (0..archive.len())
        .find(|&i| {
            archive
                .by_index(i)
                .map(|f| f.name().to_lowercase().ends_with(".csv"))
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("No .csv file found in OCOD ZIP"))?;

    let zip_entry = archive.by_index(csv_index)?;
    let reader = std::io::BufReader::new(zip_entry);
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_reader(reader);

    let headers = rdr.headers()?.clone();
    let mut records: Vec<OcodRecord> = Vec::new();

    for result in rdr.records() {
        let row = match result {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "OCOD: CSV parse error — skipping row");
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

        for &slot in PROPRIETOR_SLOTS {
            let company_number = get_field_by_name(
                &row,
                &headers,
                &format!("Company Registration No. ({slot})"),
            )
            .unwrap_or("")
            .trim()
            .to_string();
            let proprietor_name =
                get_field_by_name(&row, &headers, &format!("Proprietor Name ({slot})"))
                    .unwrap_or("")
                    .trim()
                    .to_string();

            // PROP-05 Pitfall 2: NEVER drop overseas ownership. Skip a slot only
            // when it is entirely empty (no number AND no name) — otherwise the
            // proprietor is retained and keyed by `owner_key` (foreign number,
            // ROE, or name-only). OCOD proprietors are overseas; foreign company
            // numbers (slashes, spaces, country suffixes) are the norm, not an error.
            if company_number.is_empty() && proprietor_name.is_empty() {
                continue;
            }

            // Delta 2 / PROP-05 ordering invariant: canonicalise BEFORE any filter.
            let raw_country =
                get_field_by_name(&row, &headers, &format!("Country Incorporated ({slot})"))
                    .unwrap_or("")
                    .trim()
                    .to_string();
            let country_incorporated = canonicalise_country(&raw_country);
            if !raw_country.is_empty() && country_incorporated == raw_country {
                // Canonicaliser returned the input unchanged ⇒ not in the table.
                // Retained (never drop, Pitfall 2); surface the gap for table upkeep.
                debug!(
                    country = %raw_country,
                    "OCOD: unmapped country-of-incorporation — retained verbatim (extend country_incorporation_canonical.json)"
                );
            }

            records.push(OcodRecord {
                title_number: title_number.clone(),
                tenure: tenure.clone(),
                price_paid,
                company_number,
                proprietor_name,
                country_incorporated,
                proprietor_slot: slot,
                date_proprietor_added: parse_ccod_date(date_str),
            });
        }
    }

    Ok(records)
}

/// Land Registry OCOD bulk-CSV adapter.
///
/// Disabled (`is_enabled() == false`) when no HMLR key is available. Per Plan 02
/// the effective key is `ocod.api_key`, falling back to the shared
/// `land_registry` HMLR key when blank (one HMLR account licenses both datasets).
/// Page size for the ICIJ name-index load. Well under the ~55k-row ceiling where the
/// SurrealDB server/SDK pairing fails to decode a response (the live failure was a single
/// 814,315-row page).
const ICIJ_INDEX_PAGE_SIZE: usize = 20_000;

/// Safety cap on the ICIJ index load, mirroring `scan::SCAN_MAX_ROWS`. The loop ends on an
/// empty page; this bounds it if the table ever grows unexpectedly or a page never empties.
/// The live table holds 814,315 rows, so this leaves ample headroom.
const ICIJ_INDEX_MAX_ROWS: usize = 2_000_000;

pub struct LandRegistryOcodAdapter {
    /// Effective HMLR API key (already resolved: ocod key, else land_registry key).
    pub api_key: Option<String>,
    pub cache_dir: PathBuf,
    pub refresh_interval_days: u32,
}

impl LandRegistryOcodAdapter {
    pub fn new(api_key: Option<String>, cache_dir: PathBuf, refresh_interval_days: u32) -> Self {
        Self {
            api_key,
            cache_dir,
            refresh_interval_days,
        }
    }

    /// Load the ICIJ offshore-entity name index ONCE: `lowercase(name) -> node_id`.
    ///
    /// Read-only against the isolated `icij_offshore_entity` table (ODbL/OGL
    /// isolation). In-memory matching avoids a per-proprietor DB round-trip across
    /// the whole OCOD register (~90k rows) and is a near-no-op when ICIJ is not
    /// loaded (the common case — the table is empty unless the ICIJ adapter has run).
    async fn load_icij_name_index(
        db: &Surreal<Any>,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        Self::load_icij_name_index_paged(db, ICIJ_INDEX_PAGE_SIZE).await
    }

    /// Load the ICIJ name index in bounded `LIMIT`/`START` pages.
    ///
    /// This used to be one unpaged `SELECT ... LIMIT 5000000`. That worked only because the
    /// ICIJ adapter wrote 0 rows for its entire life; once that defect was fixed the table
    /// filled to 814,315 rows and the single response exceeded what the SurrealDB 3.0.2 server
    /// / 3.1.0-beta.2 SDK pairing can decode — `Failed to deserialize flatbuffers payload:
    /// Failed to decode fb value` — which aborted the entire OCOD re-emit before one mutation
    /// was sent (2026-08-22, mid-D-14 cycle, with the superseded events already purged).
    ///
    /// `ORDER BY id` keeps paging stable so no row is skipped or repeated, and `id` is in the
    /// projection because SurrealDB 3.x rejects an `ORDER BY` on a field that is not selected.
    ///
    /// The loop terminates only on an EMPTY page, never on a short one — the rule
    /// `core::persistence::scan` states and enforces (`scan.rs:19`). A short page is not
    /// evidence that the table is exhausted; it is one of the shapes a truncated read takes,
    /// and treating it as end-of-data is how a scan silently under-reads. An under-read here
    /// would drop ICIJ names from the index and silently reduce OCOD↔ICIJ matches, which looks
    /// exactly like "no match found". A safety cap bounds the loop instead.
    async fn load_icij_name_index_paged(
        db: &Surreal<Any>,
        page_size: usize,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut idx = std::collections::HashMap::new();
        let mut start = 0usize;
        loop {
            // page_size and start are trusted integer constants, not user input.
            let sql = format!(
                "SELECT id, node_id, name FROM icij_offshore_entity \
                 ORDER BY id LIMIT {page_size} START {start}"
            );
            let rows: Vec<serde_json::Value> = db.query(sql).await?.take(0)?;
            let returned = rows.len();
            if returned == 0 {
                break;
            }
            for row in rows {
                if let (Some(node_id), Some(name)) = (row["node_id"].as_str(), row["name"].as_str())
                {
                    idx.insert(name.trim().to_lowercase(), node_id.to_string());
                }
            }
            start += returned;
            if start >= ICIJ_INDEX_MAX_ROWS {
                tracing::warn!(
                    scanned = start,
                    cap = ICIJ_INDEX_MAX_ROWS,
                    "ICIJ name-index load hit the safety cap — index is truncated"
                );
                break;
            }
        }
        Ok(idx)
    }

    /// PROP-06 OCOD↔ICIJ matcher (in-memory, exact-name-only). Exact-match-only
    /// policy: case-insensitive equality against the preloaded ICIJ index (no
    /// fuzzy/phonetic). On a match the link is written at 0.65 / `pending_review` —
    /// never auto-linked, and no speculative OffshoreLink edge is emitted (RESEARCH Q2).
    /// Does NOT re-enable the disabled ICIJ auto-linker. Returns links written.
    async fn match_proprietor_to_icij(
        &self,
        db: &Surreal<Any>,
        icij_index: &std::collections::HashMap<String, String>,
        proprietor_name: &str,
        ocod_property_node_id: &str,
    ) -> anyhow::Result<usize> {
        let key = proprietor_name.trim().to_lowercase();
        if key.is_empty() {
            return Ok(0);
        }
        let Some(icij_node_id) = icij_index.get(&key) else {
            return Ok(0);
        };
        // Exact name equality ⇒ raw 1.0, but always name-only ⇒ capped to 0.65.
        let (confidence, status) = ocod_icij_link_score(1.0);

        let evidence_json = serde_json::json!({
            "proprietor_name": proprietor_name,
            "icij_node_id": icij_node_id,
            "match": "exact_name_only",
            "cap_reason": "icij_no_corroborating_identifier",
            "score": confidence,
        })
        .to_string();

        let link_record = IdentityLinkRecord {
            id: format!("identity_links:{}", uuid::Uuid::new_v4()),
            source_node_id: ocod_property_node_id.to_string(),
            target_node_id: icij_node_id.to_string(),
            resolution_type: "name_only_offshore_review".to_string(),
            confidence,
            source: "land_registry_ocod".to_string(),
            resolved_at: Utc::now(),
            evidence_json,
            status,
        };
        write_identity_link(db, &link_record).await?;
        Ok(1)
    }
}

#[async_trait]
impl EnricherAdapter for LandRegistryOcodAdapter {
    fn name(&self) -> &'static str {
        "land_registry_ocod"
    }

    /// Disabled (OPS-01) until a non-empty HMLR key is present. The effective key
    /// is resolved at construction (ocod key, else shared land_registry key).
    fn is_enabled(&self) -> bool {
        self.api_key
            .as_deref()
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }

    /// HMLR OCOD is the overseas-ownership half of the same England-and-Wales register.
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
                adapter = "land_registry_ocod",
                api_key_present = false,
                "Adapter not enabled — HMLR api_key absent or empty (OPS-01)"
            );
            return Ok(EnrichmentStats::default());
        }

        let api_key = self.api_key.as_deref().unwrap_or_default().to_string();
        let cache_path = self.cache_dir.join("OCOD_FULL_current.zip");

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

            // Raw-key auth + two-step listing→per-file contract — shared with the
            // CCOD adapter (verified live; see resolve_hmlr_download_url).
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?;

            let Some(download_url) =
                crate::adapters::land_registry_ccod::resolve_hmlr_download_url(
                    &client,
                    &api_key,
                    OCOD_API_URL,
                    "OCOD_FULL",
                    "land_registry_ocod",
                )
                .await
            else {
                return Ok(EnrichmentStats::default());
            };

            // The signed URL expires in ~10–60s — download immediately.
            let dl_client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()?;

            info!(
                adapter = "land_registry_ocod",
                "Downloading OCOD ZIP (streaming)"
            );
            let resp = dl_client.get(&download_url).send().await?;

            // T-22-03-01: content-length guard before buffering the body (ZIP-bomb DoS).
            // Advisory only — `content_length()` is `None` under chunked transfer-encoding and
            // is server-supplied either way, so it can be absent or simply wrong. The in-flight
            // counter below is the guard that actually holds.
            if let Some(len) = resp.content_length() {
                if len > MAX_ZIP_BYTES {
                    warn!(
                        adapter = "land_registry_ocod",
                        content_length = len,
                        max_bytes = MAX_ZIP_BYTES,
                        "OCOD ZIP exceeds 2 GiB limit — skipping download (T-22-03-01)"
                    );
                    return Ok(EnrichmentStats::default());
                }
            }

            // T-22-03-01: the binding guard — enforced on bytes ACTUALLY received.
            // See `adapters::download` for why `content_length()` alone is not a cap.
            let Some(written) = crate::adapters::download::stream_to_file_capped(
                resp,
                &cache_path,
                MAX_ZIP_BYTES,
                "land_registry_ocod",
            )
            .await?
            else {
                return Ok(EnrichmentStats::default());
            };
            info!(
                adapter = "land_registry_ocod",
                bytes = written,
                "OCOD ZIP cached"
            );
        } else {
            info!(
                adapter = "land_registry_ocod",
                "OCOD ZIP is fresh — using cached file"
            );
        }

        let cache_path_owned = cache_path.clone();
        let records: Vec<OcodRecord> =
            tokio::task::spawn_blocking(move || stream_from_path(&cache_path_owned)).await??;

        info!(
            adapter = "land_registry_ocod",
            record_count = records.len(),
            "OCOD CSV parsed — emitting mutations"
        );

        // PROP-06: load the ICIJ name index ONCE (in-memory exact match; no-op
        // when ICIJ isn't loaded) — avoids a per-proprietor DB query across the register.
        let icij_index = Self::load_icij_name_index(db).await?;

        let mut candidates_found = 0usize;
        let mut properties_emitted = 0usize;
        let mut pending_review = 0usize;
        // Dedup owner-node emission: an overseas company owning N properties must be
        // emitted ONCE, not N times (throughput — the OCOD register is owner-heavy).
        let mut emitted_owners: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for rec in &records {
            // OCOD is overseas-only: key the owner as GB-ROE: (ROE) or GB-OCOD:
            // (any other overseas proprietor) — NEVER GB-COH:. owner_key returns
            // None only for a slot with no number AND no name (already filtered).
            let Some((owner_id, _kind)) = owner_key(
                &rec.company_number,
                &rec.country_incorporated,
                &rec.proprietor_name,
            ) else {
                continue;
            };
            candidates_found += 1;

            // Deterministic property node id (PROP-04), slugged so foreign company
            // numbers with slashes/spaces produce a valid RecordId.
            let prop_id_part = if !rec.company_number.trim().is_empty() {
                slug(&rec.company_number)
            } else {
                slug(&rec.proprietor_name)
            };
            let node_id = format!("property:{}_{}", prop_id_part, slug(&rec.title_number));

            // The overseas owner has no UK Companies House node, so we create it as
            // an OffshoreEntity node (name + canonical jurisdiction). This makes it
            // first-class for the offshore/ICIJ machinery and keeps it OUT of the
            // GB-COH: namespace (no false UK-company attribution). Emit once per owner.
            if emitted_owners.insert(owner_id.clone()) {
                let jurisdiction = if rec.country_incorporated.trim().is_empty() {
                    None
                } else {
                    Some(rec.country_incorporated.clone())
                };
                let _ = mutation_tx
                    .send(GraphMutation::AddNode {
                        id: EntityId(owner_id.clone()),
                        weight: NodeWeight::OffshoreEntity(OffshoreEntityData {
                            name: if rec.proprietor_name.is_empty() {
                                owner_id.clone()
                            } else {
                                rec.proprietor_name.clone()
                            },
                            jurisdiction,
                            icij_node_id: None,
                        }),
                    })
                    .await;
            }

            let _ = mutation_tx
                .send(GraphMutation::AddNode {
                    id: EntityId(node_id.clone()),
                    weight: NodeWeight::Property(PropertyData {
                        title_number: Some(rec.title_number.clone()),
                        tenure: Some(rec.tenure.clone()),
                        price_paid: rec.price_paid,
                        company_number: Some(rec.company_number.clone()),
                        // Delta 4: overseas country-of-incorporation (canonical).
                        country_incorporated: Some(rec.country_incorporated.clone()),
                    }),
                })
                .await;

            // Ownership is recorded by the OwnsProperty edge itself (property → owner
            // via in/out) — OCOD's statutory fact. We deliberately do NOT also write an
            // `identity_link` for it: identity_links are for cross-registry entity
            // resolution (analyst-reviewable), and a per-property guarded UPSERT here
            // is both redundant and the dominant throughput cost on a 97k-row register.
            let _ = mutation_tx
                .send(GraphMutation::AddEdge {
                    src: EntityId(owner_id.clone()),
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
            properties_emitted += 1;

            // PROP-06: bounded OCOD↔ICIJ name-only matcher (pending_review only).
            pending_review += self
                .match_proprietor_to_icij(db, &icij_index, &rec.proprietor_name, &node_id)
                .await?;
        }
        let auto_linked = properties_emitted;

        Ok(EnrichmentStats {
            candidates_found,
            auto_linked,
            pending_review,
            sub_candidate: 0,
            discarded: 0,
            skipped_out_of_jurisdiction: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture_zip(rows_csv: &str) -> tempfile::TempPath {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let mut zw = zip::ZipWriter::new(tmp.reopen().expect("reopen"));
        zw.start_file("OCOD_FULL.csv", SimpleFileOptions::default())
            .expect("start_file");
        zw.write_all(rows_csv.as_bytes()).expect("write csv");
        zw.finish().expect("finish zip");
        tmp.into_temp_path()
    }

    const HEADERS: &str = "Title Number,Tenure,Price Paid,Date Proprietor Added,\
Proprietor Name (1),Company Registration No. (1),Country Incorporated (1)";

    #[test]
    fn test_ocod_parse_country() {
        // Raw "BVI" must canonicalise to the single canonical token on the record.
        let csv = format!(
            "{HEADERS}\nABC123,Freehold,450000,01-01-2020,Acme Holdings Ltd,OE123456,BVI\n"
        );
        let path = write_fixture_zip(&csv);
        let recs = stream_from_path(&path).expect("parse");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].country_incorporated, "British Virgin Islands");
        assert_eq!(recs[0].proprietor_name, "Acme Holdings Ltd");
    }

    #[test]
    fn test_canonicalise_before_filter() {
        // A non-canonical variant of an in-scope jurisdiction must be RETAINED
        // (canonicalised first), proving Pitfall 2 (filter-before-canonicalise) is avoided.
        let csv = format!(
            "{HEADERS}\nDEF456,Leasehold,,02-02-2021,Shell Co,OE654321,BRITISH VIRGIN ISLANDS\n"
        );
        let path = write_fixture_zip(&csv);
        let recs = stream_from_path(&path).expect("parse");
        assert_eq!(recs.len(), 1, "variant row must be retained, not dropped");
        assert_eq!(recs[0].country_incorporated, "British Virgin Islands");
    }

    #[test]
    fn test_ocod_retains_foreign_numbered_proprietor() {
        // REGRESSION (live-run finding): a foreign company number with slashes /
        // country suffix is the OCOD norm and must be RETAINED (not dropped) and
        // keyed overseas — NEVER GB-COH:. Previously ~95% of OCOD was dropped here.
        let csv = format!(
            "{HEADERS}\nJKL012,Freehold,750000,04-04-2023,Foreign Holdings BV,079671C2/GBL,British Virgin Islands\n"
        );
        let path = write_fixture_zip(&csv);
        let recs = stream_from_path(&path).expect("parse");
        assert_eq!(
            recs.len(),
            1,
            "foreign-numbered proprietor must be retained"
        );
        let (key, kind) = owner_key(
            &recs[0].company_number,
            &recs[0].country_incorporated,
            &recs[0].proprietor_name,
        )
        .expect("foreign proprietor must be keyed");
        assert_eq!(kind, OwnerKind::Overseas);
        assert!(
            key.starts_with("GB-OCOD:") && !key.starts_with("GB-COH:"),
            "foreign proprietor must NOT be keyed GB-COH:; got {key}"
        );
    }

    #[test]
    fn test_ocod_accepts_roe_number() {
        // An ROE OE###### proprietor must produce a record with a GB-ROE owner key.
        let csv =
            format!("{HEADERS}\nGHI789,Freehold,1000000,03-03-2022,Overseas Ltd,OE999888,Jersey\n");
        let path = write_fixture_zip(&csv);
        let recs = stream_from_path(&path).expect("parse");
        assert_eq!(recs.len(), 1, "ROE-numbered proprietor must be accepted");
        assert_eq!(recs[0].company_number, "OE999888");
        assert_eq!(
            owner_key(
                &recs[0].company_number,
                &recs[0].country_incorporated,
                &recs[0].proprietor_name
            ),
            Some(("GB-ROE:OE999888".to_string(), OwnerKind::Roe))
        );
    }

    #[test]
    fn test_owner_key_forms() {
        assert!(is_roe_number("OE123456"));
        assert!(is_roe_number("oe123456"));
        assert!(!is_roe_number("OE12345")); // 5 digits
        assert!(!is_roe_number("12345678")); // CH number, not ROE

        // ROE → GB-ROE:
        assert_eq!(
            owner_key("OE123456", "Jersey", "Acme Ltd"),
            Some(("GB-ROE:OE123456".to_string(), OwnerKind::Roe))
        );

        // CARDINAL RULE: a foreign company number is NEVER keyed GB-COH: — it
        // gets a GB-OCOD: overseas key. Under D-09, a genuine (non-sentinel)
        // company number alone identifies the owner, so this now collapses to
        // a bare number key rather than the old country+number form — this is
        // the intended D-09 collapse, not a collision to avoid.
        assert_eq!(
            owner_key("079671C2/GBL", "British Virgin Islands", "Shell Co"),
            Some(("GB-OCOD:079671c2-gbl".to_string(), OwnerKind::Overseas))
        );
        // A clean-alphanumeric foreign number must STILL be overseas (never GB-COH:).
        let (k, kind) = owner_key("12345678", "Jersey", "X").unwrap();
        assert_eq!(kind, OwnerKind::Overseas);
        assert!(k.starts_with("GB-OCOD:"), "must not be GB-COH:; got {k}");

        // Name-only (blank number) is keyed by name, not dropped.
        let (k2, _) = owner_key("", "Panama", "Mossack Holdings").unwrap();
        assert_eq!(k2, "GB-OCOD:panama-mossack-holdings");

        // A genuine (non-sentinel) company number is keyed on its own under
        // D-09 regardless of country — the "unknown" country segment no
        // longer applies once a real number is present.
        let (k3, _) = owner_key("FC012665", "", "Y").unwrap();
        assert_eq!(k3, "GB-OCOD:fc012665");

        // Entirely empty slot → None.
        assert_eq!(owner_key("", "", ""), None);
    }

    #[test]
    fn eaubrink_merges_on_shared_company_number() {
        // D-09: EAUBRINK FARM GMBH is live as two GB-OCOD: nodes solely
        // because the source data spells its country of incorporation
        // differently across rows — "Germany" in one, "Hamburg" (a city, not
        // a country) in another. Same company, same FC025391 — once a
        // company number is present it alone identifies the owner; the
        // jurisdiction term must be dropped.
        let (k1, kind1) = owner_key("FC025391", "Germany", "EAUBRINK FARM GMBH").unwrap();
        let (k2, kind2) = owner_key("FC025391", "Hamburg", "EAUBRINK FARM GMBH").unwrap();
        assert_eq!(
            k1, k2,
            "same company number must collapse to one key regardless of country string"
        );
        assert_eq!(k1, "GB-OCOD:fc025391");
        assert_eq!(kind1, OwnerKind::Overseas);
        assert_eq!(kind2, OwnerKind::Overseas);
    }

    #[test]
    fn black_stone_stays_country_keyed_no_number() {
        // D-10: BLACK STONE INTERNATIONAL HOLDINGS LTD carries no company
        // number in any of its four live jurisdictions (ABU DHABI / DUBAI /
        // RAS AL KHAIMAH / United Arab Emirates). With no number, D-09 never
        // applies — merging by name alone across jurisdictions is exactly
        // what the exact-match-only policy forbids. It must stay four
        // distinct, country-keyed nodes.
        let (k1, _) = owner_key("", "ABU DHABI", "BLACK STONE INTERNATIONAL HOLDINGS LTD").unwrap();
        let (k2, _) = owner_key("", "DUBAI", "BLACK STONE INTERNATIONAL HOLDINGS LTD").unwrap();
        assert_ne!(
            k1, k2,
            "no-number owners must stay country-keyed, never merged by name alone"
        );
    }

    #[test]
    fn fc000000_sentinel_falls_back_to_country_key() {
        // D-11: FC000000 is shared LIVE by five unrelated companies across
        // five jurisdictions (ROCCA INTERNATIONAL LIMITED / BVI, LINK
        // HOLDINGS (GIBRALTAR) LIMITED, XL CATLIN SERVICES SE / Ireland,
        // GREENPARK ENTERPRISES LIMITED / Isle of Man, BLUE DIAMOND STUD FARM
        // LIMITED / Jersey). A naive D-09 (number present => bare-key) would
        // fuse all five into one synthetic node — the exact
        // director:unknown-applicant failure shape this milestone exists to
        // remove. The sentinel filter must reject it and fall back to the
        // country-keyed form.
        let (k, kind) = owner_key(
            "FC000000",
            "British Virgin Islands",
            "ROCCA INTERNATIONAL LIMITED",
        )
        .unwrap();
        assert!(
            k.starts_with("GB-OCOD:british-virgin-islands-"),
            "sentinel number must fall back to the country-keyed form; got {k}"
        );
        assert_eq!(kind, OwnerKind::Overseas);
    }

    #[test]
    fn owner_key_refuses_bare_number_for_a_non_unique_number() {
        // D-12 / FC008194: measured live against an_anglesey_aebaa785 (2026-08-22, and
        // re-confirmed unchanged at 28-09's checkpoint), this ONE number is carried by two
        // apparently unrelated companies — ROBIN SWINDON LTD (British Virgin Islands) and
        // CREDIT AGRICOLE CORPORATE AND INVESTMENT BANK (France). It is structurally genuine,
        // so `is_sentinel_number` does not and should not catch it. Merging on it would put one
        // node in charge of 3 properties belonging to two unrelated companies — a false
        // accumulation signal fed straight into the property-accumulation and address-cluster
        // detectors. Operator decision at 28-09 Task 1: exclude (option-b).
        let (robin, _) = owner_key("FC008194", "British Virgin Islands", "ROBIN SWINDON LTD")
            .expect("a proprietor with a number and a name is always keyed");
        let (agricole, _) = owner_key(
            "FC008194",
            "France",
            "CREDIT AGRICOLE CORPORATE AND INVESTMENT BANK",
        )
        .expect("a proprietor with a number and a name is always keyed");
        assert_eq!(robin, "GB-OCOD:british-virgin-islands-fc008194");
        assert_eq!(agricole, "GB-OCOD:france-fc008194");
        assert_ne!(
            robin, agricole,
            "an excluded number must stay country-keyed so the two companies stay distinct"
        );
    }

    #[test]
    fn owner_key_still_merges_every_other_genuine_number() {
        // The exclusion is a named, audited set — not a general retreat from D-09. EAUBRINK
        // (the phase's demonstrated merge) and the five benign punctuation-variant groups must
        // all still collapse to a bare number key.
        for cn in [
            "FC025391", "FC006968", "FC008955", "FC015955", "FC017716", "FC026260",
        ] {
            let (germany, _) = owner_key(cn, "Germany", "ACME GMBH").expect("keyed");
            let (elsewhere, _) = owner_key(cn, "United States", "ACME GMBH").expect("keyed");
            assert_eq!(
                germany, elsewhere,
                "{cn} carries no known collision and must still merge across country spellings"
            );
            assert!(
                germany.starts_with("GB-OCOD:fc"),
                "{cn} must key on the bare number; got {germany}"
            );
        }
    }

    #[test]
    fn is_non_unique_number_is_case_and_whitespace_insensitive() {
        // Source rows are not normalised — the same number arrives as `FC008194`, `fc008194`
        // and with stray whitespace. A case-sensitive check would let the excluded number
        // through under a different spelling, which is the whole failure it exists to prevent.
        assert!(is_non_unique_number("FC008194"));
        assert!(is_non_unique_number("fc008194"));
        assert!(is_non_unique_number("  Fc008194 "));
        assert!(!is_non_unique_number("FC008195"));
        assert!(!is_non_unique_number("FC025391"));
    }

    #[test]
    fn is_sentinel_number_structural_cases() {
        // Structural predicate (not a hardcoded FC000000-only list): strip
        // any leading ASCII-alpha prefix, then reject an empty or
        // all-same-digit remainder.
        assert!(
            is_sentinel_number("FC000000"),
            "FC000000: all-zero after stripping FC"
        );
        assert!(
            is_sentinel_number("FC111111"),
            "FC111111: all-same-digit after stripping FC"
        );
        assert!(
            !is_sentinel_number("FC025391"),
            "FC025391: mixed digits, genuine number"
        );
        assert!(
            is_sentinel_number("00000000"),
            "00000000: all-zero, no alpha prefix to strip"
        );
        assert!(
            !is_sentinel_number("12345678"),
            "12345678: mixed digits, no alpha prefix, genuine number"
        );
    }

    #[test]
    fn test_ocod_icij_cap_pending_review() {
        // PROP-06: a name-only match (raw 1.0) is capped at 0.65 and FORCED to
        // PendingReview — NOT SubCandidate (despite 0.65 being in that band), NOT AutoLinked.
        let (conf, status) = ocod_icij_link_score(1.0);
        assert!((conf - 0.65).abs() < 1e-9, "must cap at 0.65, got {conf}");
        assert_eq!(status, LinkStatus::PendingReview);

        // Even a high raw score is capped + held at PendingReview.
        let (conf2, status2) = ocod_icij_link_score(0.97);
        assert!((conf2 - 0.65).abs() < 1e-9);
        assert_eq!(status2, LinkStatus::PendingReview);

        // A low raw score is preserved (still never auto-linked).
        let (conf3, status3) = ocod_icij_link_score(0.40);
        assert!((conf3 - 0.40).abs() < 1e-9);
        assert_eq!(status3, LinkStatus::PendingReview);
    }

    /// DB-gated: the ICIJ name index must be complete even when the table holds more rows than
    /// one response can carry.
    ///
    /// The unpaged `SELECT ... LIMIT 5000000` this replaces failed outright against the live
    /// corpus on 2026-08-22 — `icij_offshore_entity` holds 814,315 rows and the
    /// SurrealDB 3.0.2 server / 3.1.0-beta.2 SDK pairing cannot decode a response that large
    /// ("Failed to deserialize flatbuffers payload: Failed to decode fb value"), aborting the
    /// whole OCOD re-emit before a single mutation was sent. It had never bitten because the
    /// ICIJ adapter wrote 0 rows for its entire life until that defect was fixed, so this
    /// loader had only ever read an EMPTY table.
    ///
    /// Uses a deliberately tiny page size so the multi-page path is exercised on 5 rows rather
    /// than by seeding hundreds of thousands.
    #[tokio::test]
    async fn icij_name_index_pages_and_returns_every_row() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_ocodicij", "main")
            .await;

        for i in 0..5 {
            db.query("CREATE type::record('icij_offshore_entity', $key) CONTENT { node_id: $nid, name: $name, dataset: 'test' }")
                .bind(("key", format!("n{i}")))
                .bind(("nid", format!("icij-node-{i}")))
                .bind(("name", format!("TEST OFFSHORE {i} LIMITED")))
                .await
                .expect("seed icij row failed")
                .check()
                .expect("seed icij row returned a statement error");
        }

        let idx = LandRegistryOcodAdapter::load_icij_name_index_paged(&db, 2)
            .await
            .expect("paged ICIJ index load failed");

        assert_eq!(
            idx.len(),
            5,
            "a page size of 2 across 5 rows must still yield every row — a short page must not \
             be read as end-of-data, and paging must not stop at the first page"
        );
        assert_eq!(
            idx.get("test offshore 3 limited").map(String::as_str),
            Some("icij-node-3"),
            "rows from later pages must be present and keyed case-insensitively"
        );
    }
}
