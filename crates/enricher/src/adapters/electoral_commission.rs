//! Electoral Commission donations CSV adapter (Phase 20, POL-01).
//!
//! Downloads the Electoral Commission donations CSV (OGL v3 open data, no API key required)
//! and performs an exact company_number join for company donors (confidence 1.0, AutoLinked).
//! Individual-donor name matching is DISABLED (exact-match-only policy, 2026-06-13): an EC
//! individual donation carries no corroborating identifier, so its name-only match was the
//! false-positive class purged from the enricher — individual donors are now discarded, not linked.
//!
//! # Security
//! - T-20-02-01: `CompanyRegistrationNumber` normalised via `normalise_ch_number` (Phase 21) then validated.
//! - T-20-02-02: DonorName stored as normalised slug only -- raw name never persisted (PII minimisation).
//! - T-20-02-04: CSV downloads stream to cache file; spawn_blocking isolates CPU; warn + skip on failure.
//! - ENR-06: evidence_json is always a flat String -- never a nested serde_json::Value.
//! - T-21-03-02: 6-digit ambiguous numbers disambiguated via local entity table lookup + CH API residual
//!   fallback; all DB queries use bound parameters; company_number never interpolated into SurrealQL.
//! - T-21-03-01: 6-digit ambiguous numbers capped at confidence 0.60/pending_review regardless of
//!   disambiguation outcome (D-16); original_company_number + disambiguation_result persisted in
//!   evidence_json for audit (D-17).

use std::collections::HashMap;
use std::io::BufReader;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};
use urban_integrity_core::config::ElectoralCommissionConfig;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{NodeWeight, PoliticalActorData};

use urban_integrity_schema::ch_number::normalise_ch_number;

use crate::adapters::ch_psc::write_identity_link;
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{
    IdentityLinkRecord, LinkStatus, AUTO_LINK_THRESHOLD, CANDIDATE_THRESHOLD,
    SUB_CANDIDATE_THRESHOLD,
};

/// Electoral Commission donations CSV URL -- returns up to 500,000 rows in a single CSV.
const EC_CSV_URL: &str = "https://search.electoralcommission.org.uk/api/csv/Donations?start=0&rows=500000&query=&sort=AcceptedDate&order=desc&et=pp&date=Accepted&register=gb&donorStatus=individual&donorStatus=company&donorStatus=unincorporatedassociation&isIrishSourceYes=true&isIrishSourceNo=true&includeOutsideSection75=true";

// -- CSV helpers --------------------------------------------------------------

/// Look up a field by header name within a `csv::StringRecord`.
///
/// `csv::StringRecord` is positional; the header record provides the name -> index map.
/// Trims whitespace from header names (EC CSV has occasional BOM-prefixed headers).
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

// -- Confidence routing -------------------------------------------------------

/// Route a composite score to the appropriate LinkStatus tier.
///
/// Returns `None` when the score is below `SUB_CANDIDATE_THRESHOLD` (discard).
pub fn classify_individual_confidence(score: f64) -> Option<LinkStatus> {
    if score >= AUTO_LINK_THRESHOLD {
        Some(LinkStatus::AutoLinked)
    } else if score >= CANDIDATE_THRESHOLD {
        Some(LinkStatus::PendingReview)
    } else if score >= SUB_CANDIDATE_THRESHOLD {
        Some(LinkStatus::SubCandidate)
    } else {
        None
    }
}

// -- Company row processing ---------------------------------------------------

/// Result of processing a single company donor row.
pub struct CompanyRowResult {
    pub company_number: String,
    pub evidence_json: String,
    pub donor_name: String,
    pub party_name: String,
    pub accepted_date: Option<chrono::DateTime<Utc>>,
    /// Original raw company number string from the CSV — persisted in evidence_json for audit (D-17).
    pub original_company_number: String,
    /// Transformations applied during normalisation (for evidence_json audit trail, D-17).
    pub normalisation_transforms: Vec<String>,
    /// True when the number is a bare 6-digit sequence — caller must write at 0.60/pending_review (D-16).
    pub is_prefix_ambiguous: bool,
    /// Result of disambiguation lookup for ambiguous 6-digit numbers — set in run() after local/CH lookup.
    /// None for non-ambiguous numbers; Some("local_entity_found:<id>"), Some("ch_api_found:<normalised>"),
    /// or Some("unresolved") for ambiguous numbers after lookup.
    pub disambiguation_result: Option<String>,
}

/// Process a single company donor row.
///
/// Returns `None` when the row should be skipped (empty or non-normalisable
/// `CompanyRegistrationNumber`).
///
/// # Security (T-20-02-01, T-21-03-02, POL-06)
/// `company_number` is first normalised via `normalise_ch_number` (strips trailing
/// punctuation, removes spaces, detects 6-digit ambiguity) and then validated inside
/// the normaliser before being returned. Raw user input is never interpolated into SQL.
///
/// The `disambiguation_result` field is left `None` here — disambiguation against the
/// local entity table or CH API happens asynchronously in `run()`.
pub fn process_company_row(
    record: &csv::StringRecord,
    headers: &csv::StringRecord,
) -> Option<CompanyRowResult> {
    // Step (a): read the raw field.
    let raw_number = get_field_by_name(record, headers, "CompanyRegistrationNumber")
        .unwrap_or("")
        .trim()
        .to_string();

    // Step (b): return None if empty.
    if raw_number.is_empty() {
        return None;
    }

    // Step (c): normalise — POL-06: non-normalisable numbers are skipped with a warning.
    let normalised = match normalise_ch_number(&raw_number) {
        Some(n) => n,
        None => {
            warn!(
                company_number = %raw_number,
                "electoral_commission: CH number not normalisable -- skipping row (POL-06)"
            );
            return None;
        }
    };

    let donor_name = get_field_by_name(record, headers, "DonorName")
        .unwrap_or("")
        .trim()
        .to_string();
    let party_name = get_field_by_name(record, headers, "RegulatedEntityName")
        .unwrap_or("")
        .trim()
        .to_string();
    let value_gbp = get_field_by_name(record, headers, "Value")
        .unwrap_or("")
        .trim()
        .to_string();
    let accepted_date_str = get_field_by_name(record, headers, "AcceptedDate")
        .unwrap_or("")
        .trim()
        .to_string();
    let ec_ref = get_field_by_name(record, headers, "ECRef")
        .unwrap_or("")
        .trim()
        .to_string();

    // Parse AcceptedDate. The EC feed publishes DD/MM/YYYY (e.g. "20/12/2010"),
    // NOT ISO — this parsed with "%Y-%m-%d" and swallowed the failure with
    // `.ok()`, so accepted_date was None for every EC record ever ingested, with
    // no log line to show for it. See crate::ec_date.
    let accepted_date = crate::ec_date::parse_ec_date(&accepted_date_str)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| Utc.from_utc_datetime(&ndt));
    if accepted_date.is_none() && !accepted_date_str.is_empty() {
        warn!(
            ec_ref = %ec_ref,
            accepted_date = %accepted_date_str,
            "EC donation: unparseable AcceptedDate — donation stored without a date, \
             so the correlation detector cannot window it"
        );
    }

    // Build evidence_json as a flat String (ENR-06, T-20-02-04, D-17).
    // T-20-02-02: store slug not raw name — consistent with individual donor path (PII minimisation).
    // D-17: original_company_number and normalisation_transforms are persisted for audit trail.
    let donor_name_slug = urban_integrity_core::stream::normalise_name(&donor_name);
    // ENR-06: evidence_json must be a flat String — no nested arrays or objects.
    // normalisation_transforms is serialised as a comma-joined string (not a JSON array)
    // to preserve the flat-field invariant required by the identity_links TYPE string column.
    let transforms_str = normalised.transforms_applied.join(",");
    let evidence_json = serde_json::json!({
        "donor_name_slug": donor_name_slug,
        "party_name": party_name,
        "value_gbp": value_gbp,
        // Normalised to ISO so downstream readers get one format. Falls back to
        // the raw feed string when unparseable, so the audit trail never loses
        // what the EC actually published. Rows written before this fix hold
        // DD/MM/YYYY, which is why ec_date::parse_ec_date accepts both.
        "accepted_date": crate::ec_date::to_iso(&accepted_date_str)
            .unwrap_or_else(|| accepted_date_str.clone()),
        "ec_ref": ec_ref,
        "donor_status": "company",
        "original_company_number": normalised.original,
        "normalisation_transforms": transforms_str,
    })
    .to_string();

    // Step (d): populate struct with normalised fields.
    // disambiguation_result is None here — set in run() after local/CH API lookup.
    Some(CompanyRowResult {
        company_number: normalised.normalised.clone(),
        evidence_json,
        donor_name,
        party_name,
        accepted_date,
        original_company_number: normalised.original,
        normalisation_transforms: normalised.transforms_applied,
        is_prefix_ambiguous: normalised.is_prefix_ambiguous,
        disambiguation_result: None,
    })
}

// -- Struct definition --------------------------------------------------------

/// Electoral Commission donations register adapter (Phase 20, POL-01).
///
/// OGL v3 open data -- no API key required. Enabled by default.
/// When `config.enabled = false`, `is_enabled()` returns false and `run()` returns
/// `Ok(EnrichmentStats::default())` immediately (graceful degradation).
pub struct ElectoralCommissionAdapter {
    config: ElectoralCommissionConfig,
    cache_dir: PathBuf,
}

impl ElectoralCommissionAdapter {
    /// Create a new adapter.
    ///
    /// `cache_dir` is the local directory for caching the downloaded CSV.
    pub fn new(config: ElectoralCommissionConfig, cache_dir: PathBuf) -> Self {
        Self { config, cache_dir }
    }
}

// -- EnricherAdapter impl -----------------------------------------------------

#[async_trait]
impl EnricherAdapter for ElectoralCommissionAdapter {
    fn name(&self) -> &'static str {
        "electoral_commission"
    }

    /// Returns true when `config.enabled` is true.
    ///
    /// The Electoral Commission donations data is OGL v3 open data -- no API key is required.
    /// The adapter is enabled by default and disabled only when explicitly set to `false`.
    fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        // Graceful-degradation gate (D-03 pattern)
        if !self.is_enabled() {
            warn!(
                adapter = "electoral_commission",
                "Adapter not enabled -- set enricher.electoral_commission.enabled = true to activate"
            );
            return Ok(EnrichmentStats::default());
        }

        // Step 1: Download EC CSV to cache
        std::fs::create_dir_all(&self.cache_dir)?;
        let cache_path = self.cache_dir.join("donations.csv");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;

        info!(
            adapter = "electoral_commission",
            "Downloading EC donations CSV"
        );
        let resp = match client.get(EC_CSV_URL).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    adapter = "electoral_commission",
                    error = %e,
                    "CSV download failed -- skipping (T-20-02-04)"
                );
                return Ok(EnrichmentStats::default());
            }
        };
        if !resp.status().is_success() {
            warn!(
                adapter = "electoral_commission",
                status = %resp.status(),
                "EC CSV download returned non-2xx — skipping"
            );
            return Ok(EnrichmentStats::default());
        }
        let csv_bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    adapter = "electoral_commission",
                    error = %e,
                    "CSV download body read failed -- skipping"
                );
                return Ok(EnrichmentStats::default());
            }
        };
        if let Err(e) = std::fs::write(&cache_path, &csv_bytes) {
            warn!(
                adapter = "electoral_commission",
                error = %e,
                "CSV cache write failed -- skipping"
            );
            return Ok(EnrichmentStats::default());
        }
        info!(
            adapter = "electoral_commission",
            bytes = csv_bytes.len(),
            "EC donations CSV cached"
        );

        // Step 2: Load entity set from SurrealDB (company_number -> entity_id)
        // Use serde_json::Value rows (SurrealDB pattern -- avoids SurrealValue trait bound issues)
        let entity_rows: Vec<serde_json::Value> = match db
            .query("SELECT entity_id, company_number FROM entity WHERE company_number IS NOT NULL")
            .await
        {
            Ok(mut resp) => resp.take(0).unwrap_or_default(),
            Err(e) => {
                warn!(
                    adapter = "electoral_commission",
                    error = %e,
                    "Failed to load entity set from DB -- skipping"
                );
                return Ok(EnrichmentStats::default());
            }
        };

        // Build HashMap<company_number -> entity_id>
        //
        // IMPORTANT: map to the bare `entity_id` field ("GB-COH:NNN"), NOT the
        // SurrealDB record id `id` ("entity:`GB-COH:NNN`"). The projector keys its
        // entity_table by the bare entity_id emitted on NodeAdded events; emitting
        // an edge src in record-id form makes the projector log "unknown src entity"
        // and skip the DonatesTo edge, leaving the donates_to typed table empty.
        // (Mirrors council_registers, which also keys edge endpoints on the bare id.)
        let company_entity_map: HashMap<String, String> = entity_rows
            .into_iter()
            .filter_map(|r| {
                let entity_id = r["entity_id"].as_str().map(str::to_string)?;
                let cn = r["company_number"].as_str().map(str::to_string)?;
                if cn.is_empty() || entity_id.is_empty() {
                    None
                } else {
                    Some((cn, entity_id))
                }
            })
            .collect();

        // NOTE: the full `SELECT id, name FROM entity` name-load that previously fed the
        // individual-donor fuzzy matcher has been removed alongside that path
        // (2026-06-13 debug session identity-links-fuzzy-matches). Only the
        // company_number exact join below uses the DB entity map now.

        // Step 3: Parse CSV inside spawn_blocking (T-20-02-04 CPU isolation)
        let cache_path_owned = cache_path.clone();
        let rows: Vec<csv::StringRecord> = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&cache_path_owned)?;
            let reader = BufReader::new(file);
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(true)
                .flexible(true)
                .from_reader(reader);

            let mut records = Vec::new();
            for result in rdr.records() {
                match result {
                    Ok(r) => records.push(r),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "electoral_commission: CSV parse error -- skipping row"
                        );
                    }
                }
            }
            Ok::<Vec<csv::StringRecord>, anyhow::Error>(records)
        })
        .await??;

        // Re-read headers synchronously (headers are tiny)
        let headers = {
            let file = std::fs::File::open(&cache_path)?;
            let reader = BufReader::new(file);
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(true)
                .flexible(true)
                .from_reader(reader);
            rdr.headers()?.clone()
        };

        info!(
            adapter = "electoral_commission",
            row_count = rows.len(),
            "EC donations CSV parsed -- processing rows"
        );

        // Step 4: Row processing loop
        let mut candidates_found = 0usize;
        let mut auto_linked = 0usize;
        let mut pending_review_count = 0usize;
        // Individual-donor fuzzy matching is disabled (see individual branch below), so
        // EC never produces sub_candidate links. Kept as a literal 0 for the stats struct.
        let sub_candidate_count = 0usize;
        let mut discarded = 0usize;
        let mut row_count = 0usize;

        for record in &rows {
            row_count += 1;
            let donor_status = get_field_by_name(record, &headers, "DonorStatus")
                .unwrap_or("")
                .trim()
                .to_lowercase();

            if donor_status == "company" {
                // Company donor: normalise + exact company_number join (Phase 21: normalise_ch_number)
                let Some(mut result) = process_company_row(record, &headers) else {
                    continue;
                };

                // Step 4: Disambiguation for 6-digit prefix-ambiguous numbers (D-15).
                // This runs before the entity map lookup so we can try both bare6 and 00-prefixed forms.
                if result.is_prefix_ambiguous {
                    let bare6 = result.company_number.clone();
                    let with00prefix = format!("00{bare6}");

                    // D-15 Step (a): local entity table lookup — prefer local over CH API.
                    let local_result: Vec<serde_json::Value> = db
                        .query("SELECT id FROM entity WHERE company_number IN [$bare6, $with00prefix] LIMIT 2")
                        .bind(("bare6", bare6.clone()))
                        .bind(("with00prefix", with00prefix.clone()))
                        .await
                        .and_then(|mut r| r.take(0))
                        .unwrap_or_default();

                    if local_result.len() == 1 {
                        // Unambiguous local match — record the entity id found.
                        let found_id = local_result[0]["id"].as_str().unwrap_or("").to_string();
                        result.disambiguation_result =
                            Some(format!("local_entity_found:{found_id}"));
                        info!(
                            original = %result.original_company_number,
                            found_id = %found_id,
                            "EC: 6-digit ambiguous number resolved via local entity table (D-15)"
                        );
                    } else if local_result.is_empty() {
                        // D-15 Step (b): CH API residual fallback — only if key is configured.
                        // The enricher uses config.enricher.ch.api_key (URBAN__ENRICHER__CH__API_KEY).
                        // Since ElectoralCommissionAdapter does not hold the CH enricher config, we
                        // check for the env var directly and degrade gracefully if absent (D-15).
                        let ch_api_key = std::env::var("URBAN__ENRICHER__CH__API_KEY")
                            .ok()
                            .filter(|k| !k.trim().is_empty());

                        if let Some(api_key) = ch_api_key {
                            let url = format!(
                                "https://api.company-information.service.gov.uk/company/{bare6}"
                            );
                            let client = reqwest::Client::new();
                            match client.get(&url).basic_auth(&api_key, Some("")).send().await {
                                Ok(resp) if resp.status().is_success() => {
                                    result.disambiguation_result =
                                        Some(format!("ch_api_found:{bare6}"));
                                    info!(
                                        original = %result.original_company_number,
                                        "EC: 6-digit ambiguous number confirmed via CH API residual lookup (D-15)"
                                    );
                                }
                                Ok(resp) => {
                                    warn!(
                                        original = %result.original_company_number,
                                        status = %resp.status(),
                                        "EC: CH API residual lookup non-2xx — setting unresolved (D-15)"
                                    );
                                    result.disambiguation_result = Some("unresolved".to_string());
                                }
                                Err(e) => {
                                    warn!(
                                        original = %result.original_company_number,
                                        error = %e,
                                        "EC: CH API residual lookup failed — degrading gracefully (D-15)"
                                    );
                                    result.disambiguation_result = Some("unresolved".to_string());
                                }
                            }
                        } else {
                            // No CH API key configured — degrade gracefully (D-15, T-21-03-05).
                            warn!(
                                original = %result.original_company_number,
                                "EC: 6-digit ambiguous number unresolved — URBAN__ENRICHER__CH__API_KEY absent (D-15)"
                            );
                            result.disambiguation_result = Some("unresolved".to_string());
                        }
                    } else {
                        // Multiple local matches — ambiguous even locally; mark unresolved.
                        warn!(
                            original = %result.original_company_number,
                            match_count = local_result.len(),
                            "EC: 6-digit ambiguous number matched multiple local entities — unresolved"
                        );
                        result.disambiguation_result = Some("unresolved".to_string());
                    }
                }

                // Lookup entity by company_number (try both bare and 00-prefixed for ambiguous numbers).
                let entity_id_opt = company_entity_map.get(&result.company_number).or_else(|| {
                    if result.is_prefix_ambiguous {
                        company_entity_map.get(&format!("00{}", result.company_number))
                    } else {
                        None
                    }
                });

                let Some(entity_id) = entity_id_opt else {
                    continue;
                };

                candidates_found += 1;

                // D-16: confidence ceiling for 6-digit ambiguous numbers — never auto-linked.
                // Non-ambiguous exact joins retain confidence=1.0 / AutoLinked (existing behaviour).
                let (confidence_val, link_status) = if result.is_prefix_ambiguous {
                    (0.60_f64, LinkStatus::PendingReview)
                } else {
                    (1.0_f64, LinkStatus::AutoLinked)
                };

                // Model the donation recipient (party) as a PoliticalActor node.
                //
                // The schema is `donates_to TYPE RELATION FROM entity TO political_actor`,
                // and the entity read path resolves recipients from PoliticalActor
                // NodeAdded events (queries.rs). The recipient id MUST be a BARE-DOMAIN
                // id (`PA:<party-slug>`) WITHOUT the destination-table prefix — matching
                // the established live convention for political_actor nodes (stored
                // entity_id `PA:<slug>`). The projector (projector.rs:182) composes the
                // RELATE OUT endpoint as `{dst_table}:`{dst_id}`` = a single clean
                // `political_actor:`PA:<slug>``, and the read path (queries.rs:1761) strips
                // exactly one `political_actor:` to recover `PA:<slug>` and resolve the
                // party display name.
                //
                // Do NOT bake `political_actor:` into the id here: the projector would then
                // emit a DOUBLED `political_actor:`political_actor:<slug>`` and name
                // resolution would miss (the read-path strip only removes one prefix).
                // A `party:<slug>` id (a non-existent table, no node) likewise leaves the
                // donates_to table empty.
                let party_slug = urban_integrity_core::stream::normalise_name(&result.party_name);
                let party_node_id = format!("PA:{party_slug}");

                // Upsert the PoliticalActor recipient node so the DonatesTo edge has a
                // valid, existing OUT endpoint and the read path can resolve its name.
                let _ = mutation_tx
                    .send(GraphMutation::AddNode {
                        id: EntityId(party_node_id.clone()),
                        weight: NodeWeight::PoliticalActor(PoliticalActorData {
                            name: result.party_name.clone(),
                            party: Some(result.party_name.clone()),
                            council_name: None,
                        }),
                    })
                    .await;

                // Emit DonatesTo edge (source entity -> recipient political_actor)
                let _ = mutation_tx
                    .send(GraphMutation::AddEdge {
                        src: EntityId(entity_id.clone()),
                        dst: EntityId(party_node_id.clone()),
                        weight: EdgeWeight::DonatesTo {
                            meta: EdgeMeta {
                                created_at: Utc::now(),
                                effective_from: result.accepted_date,
                                effective_to: None,
                            },
                        },
                    })
                    .await;

                // Build final evidence_json including disambiguation_result (D-17).
                // Re-parse the existing evidence_json to add disambiguation_result field.
                let mut evidence_obj: serde_json::Value =
                    serde_json::from_str(&result.evidence_json)
                        .unwrap_or_else(|_| serde_json::json!({}));
                if let Some(obj) = evidence_obj.as_object_mut() {
                    obj.insert(
                        "disambiguation_result".to_string(),
                        match &result.disambiguation_result {
                            Some(s) => serde_json::Value::String(s.clone()),
                            None => serde_json::Value::Null,
                        },
                    );
                }
                let final_evidence_json = evidence_obj.to_string();

                // Write identity_link with confidence/status based on ambiguity (D-16, ENR-06).
                let link_record = IdentityLinkRecord {
                    id: format!("identity_links:{}", uuid::Uuid::new_v4()),
                    source_node_id: entity_id.clone(),
                    target_node_id: party_node_id,
                    resolution_type: "exact_company_number".to_string(),
                    confidence: confidence_val,
                    source: "electoral_commission".to_string(),
                    resolved_at: Utc::now(),
                    evidence_json: final_evidence_json,
                    status: link_status.clone(),
                };
                write_identity_link(db, &link_record).await?;

                match link_status {
                    LinkStatus::AutoLinked => auto_linked += 1,
                    LinkStatus::PendingReview => pending_review_count += 1,
                    _ => {}
                }
            } else if donor_status == "individual" {
                // Individual-donor name-only fuzzy matching is DISABLED (data-integrity
                // defect, 2026-06-13 debug session identity-links-fuzzy-matches).
                //
                // An EC individual donation carries no DOB, company number, or address, so
                // a director↔donor match here is *structurally* name-only — exactly the
                // uncorroborated class that produced ~87k false positives in the charity
                // resolver and forced it back to exact-match-only on 2026-06-11.
                //
                // Two compounding defects made it worse than charity:
                //   1. composite_score is always called with dob_match=false,
                //      nationality_match=false (no corroborating field exists), so every
                //      individual match is gated to the pending_review/sub_candidate band
                //      yet still PERSISTED (classify_individual_confidence keeps anything
                //      >= SUB_CANDIDATE_THRESHOLD 0.50).
                //   2. The first-token pre-filter keyed on the UNSTRIPPED honorific
                //      ("mr"/"mrs" — normalise_name does not strip titles), so it was a
                //      near no-op: every "mr X" donor was compared against every "mr Y"
                //      director, and the +0.30 DoubleMetaphone surname credit lifted
                //      unrelated surnames sharing a phonetic code (e.g. white→wade,
                //      composite 0.756) into the review band.
                //
                // Result: 3,513 false-positive director→party links (93% of identity_links).
                // Until a corroborating signal is available for EC individual donors, these
                // links are NOT written. Company-number exact joins (the branch above) are
                // unaffected and remain the only trustworthy EC link class.
                discarded += 1;
            }
        }

        info!(
            adapter = "electoral_commission",
            rows = row_count,
            company_exact_joins = auto_linked,
            individual_candidates = candidates_found,
            "electoral_commission: processed rows"
        );

        Ok(EnrichmentStats {
            candidates_found,
            auto_linked,
            pending_review: pending_review_count,
            sub_candidate: sub_candidate_count,
            discarded,
            skipped_out_of_jurisdiction: 0,
        })
    }
}

// -- Unit tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::config::ElectoralCommissionConfig;

    /// Helper: build a CSV StringRecord from (headers, values) pairs.
    fn make_record(fields: &[(&str, &str)]) -> (csv::StringRecord, csv::StringRecord) {
        let headers: csv::StringRecord = fields.iter().map(|(h, _)| *h).collect();
        let values: csv::StringRecord = fields.iter().map(|(_, v)| *v).collect();
        (headers, values)
    }

    // Test 1: is_enabled() gate

    #[test]
    fn test_is_enabled_returns_true_when_enabled() {
        let cfg = ElectoralCommissionConfig {
            enabled: true,
            cache_dir: None,
        };
        let adapter = ElectoralCommissionAdapter::new(cfg, PathBuf::from("/tmp"));
        assert!(
            adapter.is_enabled(),
            "is_enabled() must return true when config.enabled = true"
        );
    }

    #[test]
    fn test_is_enabled_returns_false_when_disabled() {
        let cfg = ElectoralCommissionConfig {
            enabled: false,
            cache_dir: None,
        };
        let adapter = ElectoralCommissionAdapter::new(cfg, PathBuf::from("/tmp"));
        assert!(
            !adapter.is_enabled(),
            "is_enabled() must return false when config.enabled = false"
        );
    }

    // Test 2: process_company_row() with valid company number

    #[test]
    fn test_process_company_row_valid_company_number_produces_identity_link_fields() {
        let (headers, record) = make_record(&[
            ("DonorStatus", "company"),
            ("CompanyRegistrationNumber", "12345678"),
            ("DonorName", "Acme Ltd"),
            ("RegulatedEntityName", "Liberal Democrats"),
            ("Value", "5000"),
            ("AcceptedDate", "2023-10-15"),
            ("ECRef", "EC-2023-001"),
        ]);

        let result = process_company_row(&record, &headers)
            .expect("process_company_row must return Some for valid company number");

        assert_eq!(
            result.company_number, "12345678",
            "company_number must match CSV value"
        );

        // evidence_json must be a flat String (ENR-06, T-20-02-04)
        let parsed: serde_json::Value =
            serde_json::from_str(&result.evidence_json).expect("evidence_json must be valid JSON");
        assert!(
            parsed.is_object(),
            "evidence_json must be a JSON object (not nested)"
        );
        // Verify it round-trips to a flat string (no nested objects)
        let as_str = parsed.to_string();
        assert_eq!(
            result.evidence_json, as_str,
            "evidence_json must equal the flat JSON string produced by serde_json::Value::to_string()"
        );
        assert_eq!(
            parsed["donor_status"],
            serde_json::json!("company"),
            "donor_status field must be 'company'"
        );
    }

    // Test 2b: AcceptedDate regression (2026-08-14)
    //
    // The EC feed publishes DD/MM/YYYY. This function parsed "%Y-%m-%d" and
    // discarded the failure with `.ok()`, so `accepted_date` was None for every
    // record ever ingested — silently. Every pre-existing fixture above uses an
    // ISO date, which is exactly why the suite stayed green while the live path
    // was broken. These use the format the EC actually publishes.

    #[test]
    fn test_process_company_row_parses_the_day_first_date_the_ec_feed_publishes() {
        let (headers, record) = make_record(&[
            ("DonorStatus", "company"),
            ("CompanyRegistrationNumber", "12345678"),
            ("DonorName", "Acme Ltd"),
            ("RegulatedEntityName", "Liberal Democrats"),
            // Verbatim from identity_links.evidence_json in the live
            // an_anglesey_aebaa785 database.
            ("AcceptedDate", "20/12/2010"),
            ("Value", "5000"),
            ("ECRef", "EC-2010-001"),
        ]);

        let result = process_company_row(&record, &headers)
            .expect("process_company_row must return Some for a valid company number");

        let date = result
            .accepted_date
            .expect("DD/MM/YYYY AcceptedDate must parse — None here is the original bug");
        assert_eq!(
            date.format("%Y-%m-%d").to_string(),
            "2010-12-20",
            "20/12/2010 is 20 December 2010, not 12 August"
        );
    }

    #[test]
    fn test_evidence_json_accepted_date_is_normalised_to_iso() {
        let (headers, record) = make_record(&[
            ("DonorStatus", "company"),
            ("CompanyRegistrationNumber", "12345678"),
            ("DonorName", "Acme Ltd"),
            ("RegulatedEntityName", "Liberal Democrats"),
            ("AcceptedDate", "01/04/2003"),
            ("Value", "5000"),
            ("ECRef", "EC-2003-001"),
        ]);

        let result = process_company_row(&record, &headers).expect("must return Some");
        let parsed: serde_json::Value =
            serde_json::from_str(&result.evidence_json).expect("evidence_json must be valid JSON");

        assert_eq!(
            parsed["accepted_date"],
            serde_json::json!("2003-04-01"),
            "evidence_json must store ISO so downstream readers get one format"
        );
    }

    /// Cross-site contract: whatever this adapter writes into
    /// `evidence_json.accepted_date` must be readable by the donation-correlation
    /// detector, which is the seam that broke. The adapter wrote DD/MM/YYYY and
    /// the detector parsed ISO, so it skipped every donation and had never fired
    /// on real data. Both now route through `ec_date`.
    #[test]
    fn test_evidence_json_accepted_date_round_trips_through_the_shared_parser() {
        for raw in ["20/12/2010", "01/04/2003", "2023-10-15"] {
            let (headers, record) = make_record(&[
                ("DonorStatus", "company"),
                ("CompanyRegistrationNumber", "12345678"),
                ("DonorName", "Acme Ltd"),
                ("RegulatedEntityName", "Liberal Democrats"),
                ("AcceptedDate", raw),
                ("Value", "5000"),
                ("ECRef", "EC-TEST"),
            ]);

            let result = process_company_row(&record, &headers).expect("must return Some");
            let parsed: serde_json::Value = serde_json::from_str(&result.evidence_json)
                .expect("evidence_json must be valid JSON");
            let stored = parsed["accepted_date"]
                .as_str()
                .expect("accepted_date must be a string");

            assert!(
                crate::ec_date::parse_ec_date(stored).is_some(),
                "detector must be able to parse what the adapter stored for {raw} (got {stored})"
            );
        }
    }

    // Test 3: process_company_row() with empty CompanyRegistrationNumber

    #[test]
    fn test_process_company_row_empty_company_number_returns_none() {
        // Pitfall 2: empty CompanyRegistrationNumber must produce None -- no identity_link written
        let (headers, record) = make_record(&[
            ("DonorStatus", "company"),
            ("CompanyRegistrationNumber", ""),
            ("DonorName", "Mystery Company"),
            ("RegulatedEntityName", "Labour Party"),
            ("Value", "10000"),
            ("AcceptedDate", "2023-11-01"),
            ("ECRef", "EC-2023-002"),
        ]);

        let result = process_company_row(&record, &headers);
        assert!(
            result.is_none(),
            "process_company_row must return None when CompanyRegistrationNumber is empty (Pitfall 2)"
        );
    }

    // Test 4: individual donor confidence routing

    #[test]
    fn test_classify_individual_confidence_routing() {
        // Score below 0.50 -> None (discard)
        assert!(
            classify_individual_confidence(0.49).is_none(),
            "score 0.49 must be discarded (below SUB_CANDIDATE_THRESHOLD)"
        );

        // Score at SUB_CANDIDATE_THRESHOLD boundary (0.50) -> SubCandidate
        assert_eq!(
            classify_individual_confidence(SUB_CANDIDATE_THRESHOLD),
            Some(LinkStatus::SubCandidate),
            "score 0.50 must route to SubCandidate"
        );

        // Score in [0.50, 0.69] -> SubCandidate
        assert_eq!(
            classify_individual_confidence(0.65),
            Some(LinkStatus::SubCandidate),
            "score 0.65 must route to SubCandidate (below CANDIDATE_THRESHOLD 0.70)"
        );

        // Score at CANDIDATE_THRESHOLD boundary (0.70) -> PendingReview
        assert_eq!(
            classify_individual_confidence(CANDIDATE_THRESHOLD),
            Some(LinkStatus::PendingReview),
            "score 0.70 must route to PendingReview"
        );

        // Score in [0.70, 0.84] -> PendingReview
        assert_eq!(
            classify_individual_confidence(0.80),
            Some(LinkStatus::PendingReview),
            "score 0.80 must route to PendingReview (below AUTO_LINK_THRESHOLD 0.85)"
        );

        // Score at AUTO_LINK_THRESHOLD boundary (0.85) -> AutoLinked
        assert_eq!(
            classify_individual_confidence(AUTO_LINK_THRESHOLD),
            Some(LinkStatus::AutoLinked),
            "score 0.85 must route to AutoLinked"
        );

        // Score 1.0 -> AutoLinked
        assert_eq!(
            classify_individual_confidence(1.0),
            Some(LinkStatus::AutoLinked),
            "score 1.0 must route to AutoLinked"
        );
    }

    // Test 6: process_company_row with clean 8-digit number (Phase 21 normalisation)

    #[test]
    fn test_process_company_row_clean_number() {
        let (headers, record) = make_record(&[
            ("CompanyRegistrationNumber", "12345678"),
            ("DonorName", "Test Corp"),
            ("RegulatedEntityName", "Test Party"),
            ("Value", "1000"),
            ("AcceptedDate", "2024-01-01"),
            ("ECRef", "EC-001"),
        ]);
        let result =
            process_company_row(&record, &headers).expect("clean 8-digit number must return Some");
        assert_eq!(result.company_number, "12345678");
        assert!(!result.is_prefix_ambiguous);
        assert_eq!(result.original_company_number, "12345678");
    }

    // Test 7: process_company_row with ambiguous 6-digit number (Phase 21 — D-16)

    #[test]
    fn test_process_company_row_ambiguous_six_digit() {
        let (headers, record) = make_record(&[
            ("CompanyRegistrationNumber", "123456"),
            ("DonorName", "Ambiguous Corp"),
            ("RegulatedEntityName", "Test Party"),
            ("Value", "1000"),
            ("AcceptedDate", "2024-01-01"),
            ("ECRef", "EC-002"),
        ]);
        let result = process_company_row(&record, &headers)
            .expect("6-digit number must return Some (ambiguous but valid)");
        assert!(
            result.is_prefix_ambiguous,
            "6-digit number must set is_prefix_ambiguous=true"
        );
    }

    // Test 8: process_company_row with trailing question mark (Phase 21 normalisation)

    #[test]
    fn test_process_company_row_trailing_question() {
        let (headers, record) = make_record(&[
            ("CompanyRegistrationNumber", "12345678?"),
            ("DonorName", "Trailing Corp"),
            ("RegulatedEntityName", "Test Party"),
            ("Value", "1000"),
            ("AcceptedDate", "2024-01-01"),
            ("ECRef", "EC-003"),
        ]);
        let result = process_company_row(&record, &headers)
            .expect("trailing ? must be stripped and return Some");
        assert_eq!(result.company_number, "12345678");
    }

    // Test 9: process_company_row with invalid company number (Phase 21 normalisation)

    #[test]
    fn test_process_company_row_invalid() {
        let (headers, record) = make_record(&[
            ("CompanyRegistrationNumber", "INVALID"),
            ("DonorName", "Invalid Corp"),
            ("RegulatedEntityName", "Test Party"),
            ("Value", "1000"),
            ("AcceptedDate", "2024-01-01"),
            ("ECRef", "EC-004"),
        ]);
        let result = process_company_row(&record, &headers);
        assert!(
            result.is_none(),
            "INVALID must return None after normalise_ch_number check"
        );
    }

    // Test 5: evidence_json for company donor is a flat String (ENR-06)

    #[test]
    fn test_company_row_evidence_json_is_flat_string_not_nested() {
        // ENR-06: evidence_json must always be a flat String. This test verifies that
        // serde_json::json!({...}).to_string() produces a flat string, not a nested Value.
        let (headers, record) = make_record(&[
            ("DonorStatus", "company"),
            ("CompanyRegistrationNumber", "SC123456"),
            ("DonorName", "Scottish Widget Corp"),
            ("RegulatedEntityName", "Scottish National Party"),
            ("Value", "25000"),
            ("AcceptedDate", "2024-01-20"),
            ("ECRef", "EC-2024-042"),
        ]);

        let result = process_company_row(&record, &headers)
            .expect("must return Some for valid SC-prefixed company number");

        // evidence_json must be a String type (checked by field type: String not Value)
        let evidence: &str = &result.evidence_json;
        assert!(!evidence.is_empty(), "evidence_json must not be empty");

        // Must not start with a double-nested JSON pattern
        let parsed: serde_json::Value =
            serde_json::from_str(evidence).expect("evidence_json must be valid JSON string");

        // Check that no field value is itself a JSON object or array (flat structure only)
        for (key, value) in parsed.as_object().expect("must be JSON object") {
            assert!(
                !value.is_object() && !value.is_array(),
                "evidence_json field '{key}' must not be a nested object/array (ENR-06 flat String requirement)"
            );
        }

        // Verify flat JSON round-trip: String -> parse -> serialize must equal original
        let round_tripped = serde_json::to_string(&parsed).expect("must serialize");
        assert_eq!(
            evidence, round_tripped,
            "evidence_json must survive flat JSON round-trip unchanged"
        );
    }
}
