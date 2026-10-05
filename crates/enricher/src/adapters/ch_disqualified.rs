//! CH Disqualified Directors adapter (CHA-02).
//!
//! Consumes the Companies House disqualified-officers stream, parses natural
//! disqualification payloads, and creates `DisqualifiedFrom` edges + `identity_link`s
//! at confidence 0.90–0.95 via normalised name + DOB (month+year) matching.
//!
//! # Three-field-minimum invariant (CHA-02 / T-17-03-01)
//!
//! // INVARIANT: three-field minimum
//!
//! A link is only emitted when the officer's name AND at least the birth year match a
//! known graph node. Name-only matches (no DOB) are silently discarded — they do not
//! produce a link, a candidate, or a log entry at INFO level. This is the cardinal-sin
//! guard: false accusations via common names are prevented at the type level.
//!
//! Confidence tier (D-08):
//! - name + DOB month + year → 0.95 (full month+year match)
//! - name + year only (no month in payload) → 0.90
//! - name only (no DOB) → None → discard, no link (ENR-11)
//!
//! # Security
//!
//! T-17-03-01: False disqualification accusation — mitigated by three-field-minimum
//! invariant (source comment present, ENR-11 type gate).
//! T-17-03-02: SurrealQL injection — mitigated by bound parameters throughout; stream
//! data is never string-formatted into SurrealQL.
//! T-17-03-04: PII minimisation — DOB is used for matching only, not stored on the edge;
//! evidence_json stores only slug+month+year+score (TYPE string, ENR-06).

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_core::persistence::scan::scan_weights_by_type;
use urban_integrity_core::stream::normalise_name;
use urban_integrity_schema::types::edge::{DisqualifiedFromData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{DisqualificationData, NodeWeight};

use crate::adapters::ch_psc::IDENTITY_LINK_UPSERT;
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{
    link_status_str, ConfidenceScore, ConfirmedLink, IdentityLinkRecord, LinkStatus,
};

/// CH disqualified-officers stream URL.
pub const DISQUALIFIED_STREAM_URL: &str =
    "https://stream.companieshouse.gov.uk/disqualified-officers";

// ── Serde types for the raw disqualified-officers stream payload ─────────────

/// Top-level stream event envelope.
#[derive(Debug, Deserialize)]
pub struct DisqStreamEvent {
    pub resource_id: Option<String>,
    pub data: Option<DisqEventData>,
}

/// The `data` field of a disqualified-officer stream event.
///
/// Pitfall 3: `date_of_birth` is a "YYYY-MM" string for natural officers — no day component.
#[derive(Debug, Deserialize, Clone)]
pub struct DisqEventData {
    pub kind: Option<String>,
    pub forename: Option<String>,
    pub surname: Option<String>,
    pub title: Option<String>,
    /// DOB as "YYYY-MM" string (month+year only for natural officers).
    /// May also appear without a month as "YYYY" if only the year is known.
    pub date_of_birth: Option<String>,
    pub disqualifications: Option<Vec<Disqualification>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Disqualification {
    /// ISO date "YYYY-MM-DD" from which the ban begins.
    pub disqualified_from: Option<String>,
    /// ISO date "YYYY-MM-DD" at which the ban ends.
    pub disqualified_until: Option<String>,
    /// CH case identifier — maps to `DisqualifiedFromData.case_number`.
    pub case_identifier: Option<String>,
}

// ── DOB parsing ──────────────────────────────────────────────────────────────

/// Parsed date-of-birth fields extracted from the "YYYY-MM" or "YYYY" DOB string.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDob {
    pub year: i32,
    /// `Some(month)` when the month component was present in the payload.
    pub month: Option<u32>,
}

/// Parse a CH disqualified-officer DOB string into structured year+month.
///
/// Accepts "YYYY-MM" (month+year) and "YYYY" (year only).
/// Returns `None` for any other format.
pub fn parse_dob(dob: &str) -> Option<ParsedDob> {
    let dob = dob.trim();
    // Try "YYYY-MM" first
    if dob.len() == 7 && dob.as_bytes().get(4) == Some(&b'-') {
        let year_str = &dob[..4];
        let month_str = &dob[5..];
        let year: i32 = year_str.parse().ok()?;
        let month: u32 = month_str.parse().ok()?;
        if (1..=12).contains(&month) {
            return Some(ParsedDob {
                year,
                month: Some(month),
            });
        }
    }
    // Try "YYYY" (year only)
    if dob.len() == 4 {
        let year: i32 = dob.parse().ok()?;
        return Some(ParsedDob { year, month: None });
    }
    None
}

// ── Confidence assignment (D-08) ─────────────────────────────────────────────

/// Assign a confidence score per the D-08 three-field-minimum invariant.
///
/// // INVARIANT: three-field minimum
///
/// - name + DOB month + year → 0.95
/// - name + year only (no month) → 0.90
/// - name only (no DOB) → None → discard, no link (ENR-11)
///
/// The `name_matched` parameter must be `true` — callers must have already
/// confirmed a name match before calling this function. Passing `false` is a
/// programming error and will always return `None`.
pub fn assign_confidence(
    name_matched: bool,
    dob_month: Option<u32>,
    dob_year: Option<i32>,
) -> Option<f64> {
    // INVARIANT: TWO-field minimum — name alone is never enough (ENR-11).
    //
    // This comment said "three-field minimum" until 2026-09-03. The match below has always been
    // two: name + DOB year scores 0.90, which is above `AUTO_LINK_THRESHOLD` (0.85), so it
    // auto-links without a month. Only the "name alone is never enough" half was ever true.
    // `docs/gdpr-basis.md` retracted the three-field claim on 2026-08-23; this comment and
    // `docs/abbreviations.md` outlived the retraction. A comment asserting a stricter safety
    // invariant than the code enforces is worse than no comment: it is the thing a reviewer
    // checks instead of the match arms.
    if !name_matched {
        return None;
    }
    match (dob_month, dob_year) {
        (Some(_month), Some(_year)) => Some(0.95), // full month+year match
        (None, Some(_year)) => Some(0.90),         // year-only match
        _ => None,                                 // name only — discard, no link (ENR-11)
    }
}

// ── Officer matching ─────────────────────────────────────────────────────────

/// Check whether an officer node exists for the given entity_id.
///
/// Reads `payload.weight_json` (a plain string) rather than `payload.entity_id`
/// (coerced to RecordId by SurrealDB 3.x for `director:*` entries, breaking string
/// equality silently). Reconstructs the expected key from the stored officer name.
async fn officer_node_exists_disq(db: &Surreal<Any>, entity_id: &str) -> bool {
    // Only handles "director:{slug}" — disq adapter never checks OFF: composites.
    let Some(slug) = entity_id.strip_prefix("director:") else {
        return false;
    };

    let weights = scan_weights_by_type(db, "NodeAdded", "\"type\":\"Officer\"")
        .await
        .unwrap_or_default();

    weights.iter().any(|d| {
        d.get("name")
            .and_then(|v| v.as_str())
            .map(|n| normalise_name(n).replace(' ', "-"))
            .map(|s| s == slug)
            .unwrap_or(false)
    })
}

/// Attempt to match a disqualified officer against known graph nodes by name slug.
///
/// Falls back directly to `director:{slug}` (no company number available for
/// disqualified payloads, so OFF: composites cannot be built).
pub async fn match_disqualified_officer(db: &Surreal<Any>, name_slug: &str) -> Option<String> {
    // Director keys use hyphens (pa_link_officers.py slug format), not spaces.
    let director_id = format!("director:{}", name_slug.replace(' ', "-"));
    if officer_node_exists_disq(db, &director_id).await {
        return Some(director_id);
    }
    None
}

// ── Edge data building ───────────────────────────────────────────────────────

/// Parse an ISO "YYYY-MM-DD" date string to `DateTime<Utc>`.
fn parse_iso_date(s: &str) -> Option<DateTime<Utc>> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| Utc.from_utc_datetime(&ndt))
}

/// Build a `DisqualifiedFrom` edge weight from the first disqualification record.
///
/// Sets EdgeMeta.effective_from = disqualified_from and effective_to = disqualified_until
/// as required by Pitfall 5 / the temporal-edge invariant.
///
/// `case_number` is mapped from `case_identifier`.
/// Build the `Disqualification` node carrying the CDDA order itself.
///
/// The node exists because the order is not company-specific: the Companies House stream
/// supplies only the two ban dates and the case identifier, with no company at all.
pub fn build_disqualification_node(disq: &Disqualification) -> NodeWeight {
    NodeWeight::Disqualification(DisqualificationData {
        case_number: disq.case_identifier.clone(),
        disqualified_from: disq.disqualified_from.as_deref().and_then(parse_iso_date),
        disqualified_until: disq.disqualified_until.as_deref().and_then(parse_iso_date),
    })
}

pub fn build_disqualified_from_edge(disq: &Disqualification) -> EdgeWeight {
    let disqualified_from = disq.disqualified_from.as_deref().and_then(parse_iso_date);
    let disqualified_until = disq.disqualified_until.as_deref().and_then(parse_iso_date);
    let case_number = disq.case_identifier.clone();

    EdgeWeight::DisqualifiedFrom {
        meta: EdgeMeta {
            created_at: Utc::now(),
            effective_from: disqualified_from,
            effective_to: disqualified_until,
        },
        data: DisqualifiedFromData {
            disqualified_from,
            disqualified_until,
            case_number,
        },
    }
}

// ── Identity link building ───────────────────────────────────────────────────

/// Build a `ConfirmedLink` for a disqualified officer → known node match.
///
/// Evidence json (ENR-06 TYPE string): minimal — slug, month (if present), year, score.
/// DOB is NOT stored on the edge — only used for matching and recorded in evidence_json.
///
/// Returns `None` when `assign_confidence` returns `None` (name-only case), or when
/// `ConfirmedLink::try_new` rejects a sub-threshold score (should not happen given
/// D-08 scores of 0.90/0.95 are both above AUTO_LINK_THRESHOLD 0.85).
pub fn build_disq_identity_link(
    source_node_id: &str,
    target_node_id: &str,
    name_slug: &str,
    dob: &ParsedDob,
) -> Option<ConfirmedLink> {
    let score_val = assign_confidence(true, dob.month, Some(dob.year))?;
    let score = ConfidenceScore::new(score_val).ok()?;

    let match_type = if dob.month.is_some() {
        "name+dob_full"
    } else {
        "name+year"
    };

    // ENR-06: evidence_json must be a flat string, never a nested SurrealDB object.
    // T-17-03-04: store only slug + month + year + score — no full name, no PII beyond slug.
    let mut evidence = serde_json::json!({
        "name_slug": name_slug,
        "dob_year": dob.year,
        "match": match_type,
        "score": score_val,
    });
    if let Some(month) = dob.month {
        evidence["dob_month"] = serde_json::json!(month);
    }

    let record = IdentityLinkRecord {
        id: format!("identity_links:{}", uuid::Uuid::new_v4()),
        source_node_id: source_node_id.to_string(),
        target_node_id: target_node_id.to_string(),
        resolution_type: match_type.to_string(),
        confidence: score_val,
        source: "ch_disqualified".to_string(),
        resolved_at: Utc::now(),
        evidence_json: evidence.to_string(),
        status: LinkStatus::AutoLinked,
    };

    ConfirmedLink::try_new(score, record)
}

/// Public test helper that exposes `build_disq_identity_link` without DB.
pub fn build_disq_identity_link_for_test(
    source_node_id: &str,
    target_node_id: &str,
    name_slug: &str,
    dob: &ParsedDob,
) -> Option<ConfirmedLink> {
    build_disq_identity_link(source_node_id, target_node_id, name_slug, dob)
}

/// Write a confirmed identity link to SurrealDB using the subquery-guarded UPSERT (ENR-02).
///
/// Reuses the exact same pattern as `ch_psc.rs:write_identity_link` — bound parameters
/// throughout (T-17-03-02), evidence_json as TYPE string (ENR-06).
pub async fn write_disq_identity_link(
    db: &Surreal<Any>,
    record: &IdentityLinkRecord,
) -> Result<()> {
    let result: surrealdb::Result<Vec<serde_json::Value>> = db
        .query(IDENTITY_LINK_UPSERT)
        .bind(("src", record.source_node_id.clone()))
        .bind(("tgt", record.target_node_id.clone()))
        .bind(("res_type", record.resolution_type.clone()))
        .bind(("confidence", record.confidence))
        .bind(("source", record.source.clone()))
        .bind(("resolved_at", record.resolved_at))
        .bind(("evidence_json", record.evidence_json.clone()))
        .bind(("status", link_status_str(&record.status)))
        .await
        .and_then(|mut r| r.take(0));

    if let Err(ref e) = result {
        warn!(
            error = %e,
            src = %record.source_node_id,
            tgt = %record.target_node_id,
            "Disqualified: identity_link write failed"
        );
    }

    result
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("Disqualified identity_link write failed: {}", e))
}

// ── Core processing ──────────────────────────────────────────────────────────

/// Result of processing a single disqualified-officer stream event.
#[derive(Debug)]
pub struct ProcessedDisq {
    pub name_slug: String,
    pub dob: Option<ParsedDob>,
    pub officer_match: Option<String>,
    pub identity_link_written: bool,
    pub edge_written: bool,
}

/// Process a single NDJSON line from the disqualified-officers stream.
///
/// Unit-testable entry point for the disqualified parsing + matching pipeline.
pub async fn process_disq_line(
    db: &Surreal<Any>,
    mutation_tx: &Sender<GraphMutation>,
    line: &str,
) -> Result<Option<ProcessedDisq>> {
    let event: DisqStreamEvent = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => {
            debug!(error = %e, "Disqualified stream: malformed line — skipping");
            return Ok(None);
        }
    };

    let data = match event.data {
        Some(d) => d,
        None => {
            debug!("Disqualified stream: event has no data field — skipping");
            return Ok(None);
        }
    };

    // Skip non-natural disqualifications (corporate etc).
    if let Some(kind) = &data.kind {
        if !kind.contains("natural") {
            debug!(kind = %kind, "Disqualified stream: non-natural kind — skipping");
            return Ok(None);
        }
    }

    // Build display name from forename + surname.
    let forename = data.forename.as_deref().unwrap_or("").trim();
    let surname = data.surname.as_deref().unwrap_or("").trim();
    let display_name = format!("{forename} {surname}").trim().to_string();
    if display_name.is_empty() {
        debug!("Disqualified stream: empty name — skipping");
        return Ok(None);
    }

    let name_slug = normalise_name(&display_name);

    // Parse DOB (month+year or year-only). Pitfall 3: no day component.
    let dob = data.date_of_birth.as_deref().and_then(parse_dob);

    // INVARIANT: three-field minimum — name+year minimum required.
    // Without DOB year, confidence would be None → discard immediately.
    let confidence = assign_confidence(
        true,
        dob.as_ref().and_then(|d| d.month),
        dob.as_ref().map(|d| d.year),
    );
    if confidence.is_none() {
        debug!(
            name_slug = %name_slug,
            "Disqualified stream: name-only match (no DOB) — discarding (ENR-11)"
        );
        return Ok(Some(ProcessedDisq {
            name_slug,
            dob: None,
            officer_match: None,
            identity_link_written: false,
            edge_written: false,
        }));
    }

    // Attempt to match a known officer node.
    let officer_match = match_disqualified_officer(db, &name_slug).await;

    // Build the disqualification node id (using resource_id as discriminator).
    let resource_id = event
        .resource_id
        .as_deref()
        .unwrap_or(&name_slug)
        .to_string();
    let disq_node_id = format!("disq:{resource_id}");

    let mut identity_link_written = false;
    let mut edge_written = false;

    // Get the first disqualification record for edge data.
    let first_disq = data.disqualifications.as_ref().and_then(|v| v.first());

    if let Some(officer_id) = &officer_match {
        // Write the DisqualifiedFrom edge via mutation_tx.
        if let Some(disq_rec) = first_disq {
            // The disqualification node MUST be emitted before the edge that points at it.
            // It never was: the edge's `dst` was `disq:{resource_id}` for which no node kind
            // existed, so every DisqualifiedFrom edge would have been dropped as `unknown_dst`
            // at projection. See the POL-02 directs-relate-rejected investigation notes.
            let add_node = GraphMutation::AddNode {
                id: EntityId(disq_node_id.clone()),
                weight: build_disqualification_node(disq_rec),
            };
            if let Err(e) = mutation_tx.send(add_node).await {
                warn!(error = %e, "Disqualified: failed to send AddNode mutation");
            }

            let edge = build_disqualified_from_edge(disq_rec);
            let add_edge = GraphMutation::AddEdge {
                src: EntityId(officer_id.clone()),
                dst: EntityId(disq_node_id.clone()),
                weight: edge,
            };
            if let Err(e) = mutation_tx.send(add_edge).await {
                warn!(error = %e, "Disqualified: failed to send AddEdge mutation");
            } else {
                edge_written = true;
            }
        }

        // Write the identity link (officer → disq record).
        if let Some(ref dob_parsed) = dob {
            if let Some(link) =
                build_disq_identity_link(&disq_node_id, officer_id, &name_slug, dob_parsed)
            {
                write_disq_identity_link(db, link.record()).await?;
                identity_link_written = true;
            }
        }
    }

    Ok(Some(ProcessedDisq {
        name_slug,
        dob,
        officer_match,
        identity_link_written,
        edge_written,
    }))
}

// ── DisqualifiedAdapter ──────────────────────────────────────────────────────

/// CH Disqualified Directors adapter (CHA-02 / CHA-03).
///
/// Consumes the disqualified-officers stream, matches officers by normalised
/// name + DOB (month+year), and writes `DisqualifiedFrom` edges + `identity_link`s
/// at confidence 0.90–0.95.
///
/// Is disabled (`is_enabled() == false`) when `api_key` is absent or empty (CHA-03).
pub struct DisqualifiedAdapter {
    /// CH Streaming API key for the enricher (separate from the ingestor key).
    pub api_key: Option<String>,
}

impl DisqualifiedAdapter {
    pub fn new(api_key: Option<String>) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl EnricherAdapter for DisqualifiedAdapter {
    fn name(&self) -> &'static str {
        "ch_disqualified"
    }

    /// Returns true only when a non-empty, non-whitespace API key is present.
    fn is_enabled(&self) -> bool {
        self.api_key
            .as_deref()
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }

    /// Run a single disqualified-directors enrichment pass.
    ///
    /// The live stream loop is intentionally not wired for Phase 17 unit tests —
    /// all business logic (parser, matcher, link builder, edge builder) is
    /// independently testable offline via fixture tests.
    #[allow(unused_variables)]
    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        if !self.is_enabled() {
            warn!(
                adapter = "ch_disqualified",
                "Adapter not enabled — api_key absent or empty (CHA-03)"
            );
            return Ok(EnrichmentStats::default());
        }

        let api_key = self.api_key.as_deref().unwrap_or_default().to_string();
        let mut stats = EnrichmentStats::default();

        let officers = fetch_officer_names_from_graph(db).await?;
        info!(
            adapter = "ch_disqualified",
            officer_count = officers.len(),
            "Checking disqualification status via CH REST API"
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        for (slug, name) in &officers {
            let resp = match client
                .get("https://api.companieshouse.gov.uk/search/disqualified-officers")
                .query(&[("q", name.as_str()), ("items_per_page", "5")])
                .basic_auth(&api_key, None::<&str>)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    debug!(slug, error = %e, "Disqualified REST: request failed — skipping");
                    continue;
                }
            };
            if !resp.status().is_success() {
                continue;
            }
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    debug!(slug, error = %e, "Disqualified REST: parse failed — skipping");
                    continue;
                }
            };

            let items = body
                .get("items")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default();

            for item in &items {
                stats.candidates_found += 1;
                // Normalise date_of_birth: search API returns ISO datetime
                // ("1980-06-01T00:00:00.000Z"); parse_dob expects "YYYY-MM" or "YYYY".
                let dob = item
                    .get("date_of_birth")
                    .and_then(|v| v.as_str())
                    .map(|s| s.get(..7).unwrap_or(s).to_string())
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null);

                let synthetic = serde_json::json!({
                    "resource_id": slug,
                    "data": {
                        "kind": "natural-disqualification",
                        "forename": item.get("forename").cloned().unwrap_or(serde_json::Value::Null),
                        "surname": item.get("surname").cloned().unwrap_or(serde_json::Value::Null),
                        "date_of_birth": dob,
                        "disqualifications": item.get("disqualifications").cloned().unwrap_or(serde_json::json!([])),
                    }
                });

                match process_disq_line(db, mutation_tx, &synthetic.to_string()).await {
                    Ok(Some(_)) => stats.auto_linked += 1,
                    Ok(None) => {}
                    Err(e) => {
                        warn!(slug, error = %e, "Disqualified REST: item processing error");
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        info!(
            adapter = "ch_disqualified",
            candidates = stats.candidates_found,
            auto_linked = stats.auto_linked,
            "Disqualified enrichment complete"
        );
        Ok(stats)
    }
}

// ── Graph helpers ────────────────────────────────────────────────────────────

/// Fetch unique (slug, display_name) pairs for Officer nodes in graph_events.
///
/// Reads `payload.weight_json` only — avoids `payload.entity_id` which SurrealDB 3.x
/// coerces to a RecordId for `director:*` entries (as_str() returns "" on RecordId,
/// causing the former filter `eid.starts_with("director:")` to always fail and return
/// an empty list, making zero CH API calls).
///
/// Slug is derived via `normalise_name(name).replace(' ', "-")` to match the
/// hyphen-based key format created by `pa_link_officers.py`.
/// Capped at 500 to bound CH API quota.
async fn fetch_officer_names_from_graph(
    db: &Surreal<Any>,
) -> anyhow::Result<Vec<(String, String)>> {
    let weights = scan_weights_by_type(db, "NodeAdded", "\"type\":\"Officer\"").await?;

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for d in &weights {
        let Some(name) = d.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let slug = normalise_name(name).replace(' ', "-");
        if !seen.insert(slug.clone()) {
            continue;
        }
        out.push((slug, name.to_string()));
        if out.len() >= 500 {
            break;
        }
    }
    Ok(out)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::AUTO_LINK_THRESHOLD;

    // ── parse_dob tests ──────────────────────────────────────────────────────

    #[test]
    fn test_parse_dob_full_month_year() {
        let result = parse_dob("1970-03");
        assert_eq!(
            result,
            Some(ParsedDob {
                year: 1970,
                month: Some(3)
            })
        );
    }

    #[test]
    fn test_parse_dob_year_only() {
        let result = parse_dob("1985");
        assert_eq!(
            result,
            Some(ParsedDob {
                year: 1985,
                month: None
            })
        );
    }

    #[test]
    fn test_parse_dob_invalid() {
        assert_eq!(parse_dob("not-a-date"), None);
        assert_eq!(parse_dob(""), None);
        assert_eq!(parse_dob("1970-13"), None); // month 13 invalid
    }

    #[test]
    fn test_parse_dob_december() {
        let result = parse_dob("1990-12");
        assert_eq!(
            result,
            Some(ParsedDob {
                year: 1990,
                month: Some(12)
            })
        );
    }

    // ── assign_confidence (disq_confidence) tests ────────────────────────────

    #[test]
    fn disq_confidence_full_month_year_is_095() {
        let conf = assign_confidence(true, Some(3), Some(1970));
        assert_eq!(conf, Some(0.95));
    }

    #[test]
    fn disq_confidence_year_only_is_090() {
        let conf = assign_confidence(true, None, Some(1970));
        assert_eq!(conf, Some(0.90));
    }

    #[test]
    fn disq_confidence_name_only_is_none() {
        // INVARIANT: three-field minimum — name alone → no link
        let conf = assign_confidence(true, None, None);
        assert_eq!(conf, None);
    }

    #[test]
    fn disq_confidence_name_not_matched_is_none() {
        let conf = assign_confidence(false, Some(3), Some(1970));
        assert_eq!(conf, None);
    }

    #[test]
    fn disq_confidence_at_or_above_auto_link_threshold() {
        // Both 0.90 and 0.95 must be at or above AUTO_LINK_THRESHOLD (0.85)
        let full = assign_confidence(true, Some(3), Some(1970)).unwrap();
        let year_only = assign_confidence(true, None, Some(1970)).unwrap();
        assert!(full >= AUTO_LINK_THRESHOLD);
        assert!(year_only >= AUTO_LINK_THRESHOLD);
    }

    // ── disq_edge_meta tests ─────────────────────────────────────────────────

    #[test]
    fn disq_edge_meta_ban_window_preserved() {
        let disq = Disqualification {
            disqualified_from: Some("2024-01-15".to_string()),
            disqualified_until: Some("2034-01-15".to_string()),
            case_identifier: Some("INS-2023-00123".to_string()),
        };

        let edge = build_disqualified_from_edge(&disq);
        match edge {
            EdgeWeight::DisqualifiedFrom { meta, data } => {
                // effective_from == disqualified_from (Pitfall 5)
                assert!(meta.effective_from.is_some(), "effective_from must be set");
                assert!(meta.effective_to.is_some(), "effective_to must be set");

                // Verify the dates match what was passed
                let from = meta.effective_from.unwrap();
                let until = meta.effective_to.unwrap();
                assert_eq!(from.format("%Y-%m-%d").to_string(), "2024-01-15");
                assert_eq!(until.format("%Y-%m-%d").to_string(), "2034-01-15");

                // DisqualifiedFromData fields
                assert_eq!(data.disqualified_from, meta.effective_from);
                assert_eq!(data.disqualified_until, meta.effective_to);
                assert_eq!(data.case_number, Some("INS-2023-00123".to_string()));
            }
            _ => panic!("Expected DisqualifiedFrom edge"),
        }
    }

    #[test]
    fn disq_edge_meta_case_number_from_case_identifier() {
        let disq = Disqualification {
            disqualified_from: Some("2020-06-01".to_string()),
            disqualified_until: Some("2030-06-01".to_string()),
            case_identifier: Some("CRS-2019-98765".to_string()),
        };
        let edge = build_disqualified_from_edge(&disq);
        match edge {
            EdgeWeight::DisqualifiedFrom { data, .. } => {
                assert_eq!(data.case_number, Some("CRS-2019-98765".to_string()));
            }
            _ => panic!("Expected DisqualifiedFrom edge"),
        }
    }

    #[test]
    fn disq_edge_meta_none_dates_when_absent() {
        let disq = Disqualification {
            disqualified_from: None,
            disqualified_until: None,
            case_identifier: None,
        };
        let edge = build_disqualified_from_edge(&disq);
        match edge {
            EdgeWeight::DisqualifiedFrom { meta, data } => {
                assert!(meta.effective_from.is_none());
                assert!(meta.effective_to.is_none());
                assert!(data.case_number.is_none());
            }
            _ => panic!("Expected DisqualifiedFrom edge"),
        }
    }

    // ── build_disq_identity_link tests ──────────────────────────────────────

    #[test]
    fn disq_identity_link_full_match_is_095() {
        let dob = ParsedDob {
            year: 1970,
            month: Some(3),
        };
        let link = build_disq_identity_link_for_test(
            "disq:abc123",
            "director:john-smith",
            "john smith",
            &dob,
        );
        assert!(link.is_some());
        let confirmed = link.unwrap();
        assert!((confirmed.score().value() - 0.95).abs() < f64::EPSILON);
        assert_eq!(confirmed.record().status, LinkStatus::AutoLinked);
        assert_eq!(confirmed.record().source, "ch_disqualified");
        // evidence_json is a flat string (ENR-06)
        let parsed: serde_json::Value =
            serde_json::from_str(&confirmed.record().evidence_json).unwrap();
        assert_eq!(parsed["name_slug"], "john smith");
        assert_eq!(parsed["dob_year"], 1970);
        assert_eq!(parsed["dob_month"], 3);
        assert_eq!(parsed["match"], "name+dob_full");
        assert!((parsed["score"].as_f64().unwrap() - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn disq_identity_link_year_only_is_090() {
        let dob = ParsedDob {
            year: 1970,
            month: None,
        };
        let link = build_disq_identity_link_for_test(
            "disq:abc123",
            "director:john-smith",
            "john smith",
            &dob,
        );
        assert!(link.is_some());
        let confirmed = link.unwrap();
        assert!((confirmed.score().value() - 0.90).abs() < f64::EPSILON);
        let parsed: serde_json::Value =
            serde_json::from_str(&confirmed.record().evidence_json).unwrap();
        assert_eq!(parsed["match"], "name+year");
        // dob_month should not be present in evidence_json when None
        assert!(parsed["dob_month"].is_null());
    }

    // ── DisqualifiedAdapter is_enabled tests ────────────────────────────────

    #[test]
    fn disq_adapter_disabled_when_no_key() {
        let adapter = DisqualifiedAdapter::new(None);
        assert!(!adapter.is_enabled());
    }

    #[test]
    fn disq_adapter_disabled_when_empty_key() {
        let adapter = DisqualifiedAdapter::new(Some(String::new()));
        assert!(!adapter.is_enabled());
    }

    #[test]
    fn disq_adapter_disabled_when_whitespace_key() {
        let adapter = DisqualifiedAdapter::new(Some("   ".to_string()));
        assert!(!adapter.is_enabled());
    }

    #[test]
    fn disq_adapter_enabled_with_real_key() {
        let adapter = DisqualifiedAdapter::new(Some("live-key-value".to_string()));
        assert!(adapter.is_enabled());
    }

    #[test]
    fn disq_adapter_name() {
        let adapter = DisqualifiedAdapter::new(None);
        assert_eq!(adapter.name(), "ch_disqualified");
    }

    // ── DB integration tests (URBAN_DB_TESTS=1 required) ───────────────────────

    #[cfg(test)]
    async fn connect_test_db() -> urban_integrity_test_support::TestNs {
        urban_integrity_test_support::TestNs::connect("integrity").await
    }

    /// Regression: `fetch_officer_names_from_graph` must read `payload.weight_json`,
    /// NOT `payload.entity_id`.  Pre-fix, `entity_id` was coerced by SurrealDB 3.x to a
    /// RecordId object; `as_str()` returned "" so `starts_with("director:")` was always
    /// false → empty officer list → zero CH API calls.
    #[tokio::test]
    async fn test_fetch_officer_names_uses_weight_json_not_entity_id() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let officer_name = format!("Disq Fetch Test {run_id}");
        // weight_json carries the name; entity_id is deliberately a director: key
        // (which SurrealDB would coerce to RecordId — the old bug path).
        let weight_json = serde_json::json!({
            "type": "Officer",
            "name": officer_name,
        })
        .to_string();
        let record = serde_json::json!({
            "event_id": format!("test-disq-fetch-{run_id}"),
            "timestamp": chrono::Utc::now().timestamp_millis() as u64,
            "kind": "NodeAdded",
            "payload": {
                "entity_id": format!("director:disq-fetch-{run_id}"),
                "weight_json": weight_json,
            }
        });
        let _: surrealdb::Result<Vec<serde_json::Value>> = db
            .query("CREATE graph_events CONTENT $r")
            .bind(("r", record))
            .await
            .and_then(|mut r| r.take(0));

        let officers = fetch_officer_names_from_graph(&db).await.unwrap();

        let expected_slug = normalise_name(&officer_name).replace(' ', "-");
        let found = officers
            .iter()
            .any(|(slug, name)| slug == &expected_slug && name == &officer_name);
        assert!(
            found,
            "fetch_officer_names_from_graph must find officer '{officer_name}' \
             (slug '{expected_slug}') via weight_json; got {officers:?}"
        );
    }

    /// `fetch_officer_names_from_graph` must deduplicate by slug — two NodeAdded events
    /// for officers with the same normalised name produce exactly one entry.
    #[tokio::test]
    async fn test_fetch_officer_names_deduplicates() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let officer_name = format!("Dedup Officer {run_id}");

        for i in 0..2u8 {
            let weight_json = serde_json::json!({
                "type": "Officer",
                "name": officer_name,
            })
            .to_string();
            let record = serde_json::json!({
                "event_id": format!("test-disq-dedup-{run_id}-{i}"),
                "timestamp": chrono::Utc::now().timestamp_millis() as u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": format!("director:dedup-{run_id}"),
                    "weight_json": weight_json,
                }
            });
            let _: surrealdb::Result<Vec<serde_json::Value>> = db
                .query("CREATE graph_events CONTENT $r")
                .bind(("r", record))
                .await
                .and_then(|mut r| r.take(0));
        }

        let officers = fetch_officer_names_from_graph(&db).await.unwrap();
        let slug = normalise_name(&officer_name).replace(' ', "-");
        let count = officers.iter().filter(|(s, _)| s == &slug).count();
        assert_eq!(
            count, 1,
            "fetch_officer_names_from_graph must deduplicate by slug; got {count} for '{slug}'"
        );
    }

    /// `fetch_officer_names_from_graph` must return hyphen-based slugs (matching
    /// the key format from `pa_link_officers.py`), not space-based slugs.
    #[tokio::test]
    async fn test_fetch_officer_names_returns_hyphen_slugs() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let officer_name = format!("Hyphen Slug Test {run_id}");
        let weight_json = serde_json::json!({
            "type": "Officer",
            "name": officer_name,
        })
        .to_string();
        let record = serde_json::json!({
            "event_id": format!("test-hyphen-{run_id}"),
            "timestamp": chrono::Utc::now().timestamp_millis() as u64,
            "kind": "NodeAdded",
            "payload": { "entity_id": "director:hyphen-test", "weight_json": weight_json }
        });
        let _: surrealdb::Result<Vec<serde_json::Value>> = db
            .query("CREATE graph_events CONTENT $r")
            .bind(("r", record))
            .await
            .and_then(|mut r| r.take(0));

        let officers = fetch_officer_names_from_graph(&db).await.unwrap();

        let expected_slug = normalise_name(&officer_name).replace(' ', "-");
        let found = officers.iter().any(|(slug, _)| slug == &expected_slug);
        assert!(
            found,
            "fetch_officer_names_from_graph must return hyphen slug '{expected_slug}'"
        );
        // Explicitly verify no space-based slug was returned for this officer.
        let space_slug = normalise_name(&officer_name); // spaces, not hyphens
        let space_found = officers.iter().any(|(slug, _)| slug == &space_slug);
        assert!(
            !space_found,
            "fetch_officer_names_from_graph must NOT return space slug '{space_slug}'"
        );
    }

    /// Regression: `write_disq_identity_link` must bind status as a plain string
    /// ('auto_linked'), not JSON-quoted ('"auto_linked"').
    #[tokio::test]
    async fn test_write_disq_identity_link_status_plain_string() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let record = crate::confidence::IdentityLinkRecord {
            id: format!("identity_links:status-disq-{run_id}"),
            source_node_id: format!("disq:status-test-{run_id}"),
            target_node_id: format!("director:status-test-{run_id}"),
            resolution_type: "name+dob_full".to_string(),
            confidence: 0.95,
            source: "ch_disqualified".to_string(),
            resolved_at: chrono::Utc::now(),
            evidence_json: "{\"regression\":true}".to_string(),
            status: crate::confidence::LinkStatus::AutoLinked,
        };

        write_disq_identity_link(&db, &record).await.unwrap();

        let rows: Vec<serde_json::Value> = db
            .query(
                "SELECT status FROM identity_links \
                 WHERE source_node_id = $src AND target_node_id = $tgt \
                   AND source = 'ch_disqualified'",
            )
            .bind(("src", record.source_node_id.clone()))
            .bind(("tgt", record.target_node_id.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();

        assert!(
            !rows.is_empty(),
            "disq identity_link must have been written"
        );
        let status = rows[0]["status"].as_str().unwrap_or("(missing)");
        assert_eq!(
            status, "auto_linked",
            "status must be plain 'auto_linked', not '\"auto_linked\"'; got {status:?}"
        );
    }

    /// `write_disq_identity_link` must be idempotent (ENR-02).
    #[tokio::test]
    async fn test_write_disq_identity_link_idempotent() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let record = crate::confidence::IdentityLinkRecord {
            id: format!("identity_links:idem-disq-{run_id}"),
            source_node_id: format!("disq:idem-{run_id}"),
            target_node_id: format!("director:idem-{run_id}"),
            resolution_type: "name+year".to_string(),
            confidence: 0.90,
            source: "ch_disqualified".to_string(),
            resolved_at: chrono::Utc::now(),
            evidence_json: "{\"idem\":true}".to_string(),
            status: crate::confidence::LinkStatus::AutoLinked,
        };

        write_disq_identity_link(&db, &record).await.unwrap();
        write_disq_identity_link(&db, &record).await.unwrap();

        let rows: Vec<serde_json::Value> = db
            .query(
                "SELECT id FROM identity_links \
                 WHERE source_node_id = $src AND target_node_id = $tgt \
                   AND source = 'ch_disqualified'",
            )
            .bind(("src", record.source_node_id.clone()))
            .bind(("tgt", record.target_node_id.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();

        assert_eq!(
            rows.len(),
            1,
            "write_disq_identity_link must be idempotent (ENR-02): expected 1 row, got {}",
            rows.len()
        );
    }
}
