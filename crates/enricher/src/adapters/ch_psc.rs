//! CH PSC beneficial-ownership adapter (CHA-01).
//!
//! Consumes the Companies House persons-with-significant-control stream,
//! parses individual PSC payloads into `Psc` nodes + `Controls` edges, and
//! creates confidence-1.0 `identity_link`s when a PSC matches a known officer.
//!
//! Corporate PSCs (`kind: corporate-entity-person-with-significant-control`) are
//! explicitly skipped — out of scope for Phase 17 (natural persons only).
//!
//! Security (T-17-02-01): All SurrealDB writes use bound parameters (`$id`, `$tp`).
//! `normalise_name` strips input to a safe slug; stream data is never string-formatted
//! into SurrealQL.
//!
//! Security (T-17-02-02): `evidence_json` stores only name_slug + company_number +
//! match metadata (minimised). Full PSC payloads are never logged at INFO or above.
//!
//! Security (T-17-02-03): `company_number` is re-validated with `is_valid_ch_number`
//! before any DB write.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_core::persistence::scan;
use urban_integrity_core::persistence::scan::{scan_entities_by_type, scan_weights_by_type};
use urban_integrity_core::stream::normalise_name;
use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{NodeWeight, PscData};

use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{
    link_status_str, ConfidenceScore, ConfirmedLink, IdentityLinkRecord, LinkStatus,
};

/// CH PSC stream URL.
pub const PSC_STREAM_URL: &str =
    "https://stream.companieshouse.gov.uk/persons-with-significant-control";

// ── Serde types for the raw PSC stream payload ─────────────────────────────

/// Top-level stream event envelope.
#[derive(Debug, Deserialize)]
pub struct PscStreamEvent {
    pub resource_uri: Option<String>,
    pub data: Option<PscEventData>,
}

/// The `data` field of a PSC stream event.
#[derive(Debug, Deserialize, Clone)]
pub struct PscEventData {
    pub kind: String,
    pub name: Option<String>,
    pub name_elements: Option<NameElements>,
    pub natures_of_control: Option<Vec<String>>,
    pub notified_on: Option<String>,
    /// links.self carries the same path as resource_uri — used as fallback.
    pub links: Option<PscLinks>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NameElements {
    pub forename: Option<String>,
    pub surname: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PscLinks {
    #[serde(rename = "self")]
    pub self_link: Option<String>,
}

// ── Company number extraction ────────────────────────────────────────────────

/// Extract the Companies House company number from a `resource_uri` or `links.self` path.
///
/// The PSC individual resource path is:
/// `/company/{company_number}/persons-with-significant-control/...`
///
/// The company number is NEVER read from a `data.company_number` field — that field
/// does not exist on PSC individual resources (Pitfall 2 / CHA-01 requirement).
///
/// # Security (T-17-02-03)
/// The extracted number is later re-validated with `is_valid_ch_number` before any
/// DB write; this function only extracts the raw path segment.
pub fn extract_company_number(resource_uri: &str, links_self: Option<&str>) -> Option<String> {
    // Try resource_uri first, then links.self as fallback.
    let path = if !resource_uri.is_empty() {
        resource_uri
    } else {
        links_self?
    };

    // Path format: /company/{company_number}/persons-with-significant-control/...
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // ["company", "{number}", "persons-with-significant-control", ...]
    if segments.len() >= 2 && segments[0] == "company" {
        Some(segments[1].to_string())
    } else {
        None
    }
}

/// Returns `true` when the PSC kind indicates a corporate entity (out of scope).
///
/// Corporate PSCs must be skipped — only natural persons are modelled in Phase 17.
pub fn skip_if_corporate(kind: &str) -> bool {
    kind.contains("corporate-entity") || kind.contains("legal-person")
}

// ── CH number validation (mirrors queries.rs:172) ──────────────────────────

/// Validate a Companies House number before using it in a DB query.
///
/// Accepts 8 digits, or 2-letter prefix + 6 digits (SC/NI/OC/...).
/// Rejects anything with special characters to prevent SurrealQL injection
/// (T-17-02-01 / T-17-02-03 mitigation).
pub fn is_valid_ch_number(s: &str) -> bool {
    if s.is_empty() || s.len() > 10 {
        return false;
    }
    // Accept pure digits (e.g. "12345678") or prefix+digits (e.g. "SC123456")
    let bytes = s.as_bytes();
    let all_safe = bytes.iter().all(|&b| b.is_ascii_alphanumeric());
    if !all_safe {
        return false;
    }
    // Must contain at least 2 digits (prevents single-char inputs passing)
    let digit_count = bytes.iter().filter(|&&b| b.is_ascii_digit()).count();
    digit_count >= 2
}

// ── Officer matching (R-01) ──────────────────────────────────────────────────

/// Attempt to match a PSC against a known officer node (R-01).
///
/// Try order:
/// 1. Exact `OFF:{company_number}-{slug}` composite key (CH officers).
/// 2. Fall back to `director:{slug}` (planning-portal officers).
/// 3. If neither matches, return `None` — standalone Psc node + Controls edge, no identity_link.
///
/// Returns the matching node ID string if found.
///
/// # Security (T-17-02-01)
/// The company_number and slug are validated / normalised before use. The lookup
/// uses a Rust-side existence check (CONTAINS test on payload) rather than
/// constructing a SurrealQL string from user input.
pub async fn match_officer(
    db: &Surreal<Any>,
    company_number: &str,
    name_slug: &str,
) -> Option<OfficerMatch> {
    // Director keys use hyphens (pa_link_officers.py slug format), not spaces.
    let hyphen_slug = name_slug.replace(' ', "-");
    let off_composite = format!("OFF:{company_number}-{hyphen_slug}");
    let director_slug = format!("director:{hyphen_slug}");

    for candidate_id in [&off_composite, &director_slug] {
        // Note: officer_node_exists uses weight_json-based matching (avoids RecordId coercion),
        // and returns the node's REAL entity id — which for a composite hit is the
        // `director:{slug}` the node is actually stored under, not the key that found it.
        if let Some(found) = officer_node_exists(db, candidate_id).await {
            return Some(OfficerMatch {
                node_id: found,
                company_corroborated: candidate_id == &off_composite,
            });
        }
    }
    None
}

/// A PSC matched against a known officer node.
///
/// The node id and the corroboration class are **two separate facts**, and this type exists
/// because they used to be one string. `match_officer` returned whichever key had matched, that
/// key became the `identity_link`'s `target_node_id`, and `build_identity_link` recovered the
/// match class by testing the string for an `OFF:` prefix.
///
/// That conflation broke when PR #38 re-keyed every officer to `director:{slug}`: measured live
/// 2026-08-16, all **6,666** `off_composite` links carry a `target_node_id` of the form
/// `OFF:{cn}-{slug}` matching **no node in the database** — 0 officer rows and 0 officer events
/// use that id form. Keeping the two facts apart means a company-corroborated match can point at
/// the officer's real id, so the class can be recorded without inventing a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficerMatch {
    /// The officer node's real `entity_id` — what an `identity_link` must point at.
    pub node_id: String,
    /// Whether a company number corroborated the name match, rather than the name alone.
    pub company_corroborated: bool,
}

/// Check whether a node with the given entity_id exists in `graph_events`.
///
/// Reconstructs the expected key from `payload.weight_json` (a plain string, never
/// coerced by SurrealDB) rather than reading `payload.entity_id` (coerced to RecordId
/// by SurrealDB 3.x for `director:*` entries, making string equality silently fail).
///
/// Handles "director:{slug}" by normalising the stored name and comparing slugs.
/// For "OFF:{cn}-{slug}" composites, additionally checks the company_number field.
async fn officer_node_exists(db: &Surreal<Any>, entity_id: &str) -> Option<String> {
    // Early-exit scan: this answers "does this officer exist?", so materialising every Officer
    // node per call would make a point read O(table).
    let hit = scan::find_event_by_type(db, "NodeAdded", "\"type\":\"Officer\"", |row| {
        let Some(d) = row["payload"]["weight_json"]
            .as_str()
            .and_then(|wj| serde_json::from_str::<serde_json::Value>(wj).ok())
        else {
            return false;
        };
        // Match on every ordering the node's name could key under, not just the as-written one.
        // CH writes "SURNAME, Forenames" and this adapter's PSC slugs are forename-first, so a
        // single `normalise_name` comparison misses the whole CH-sourced population — the same
        // 18-vs-6,659 gap `officer_name_slugs` was written to close, which this existence check
        // never applied.
        let Some(name) = d.get("name").and_then(|v| v.as_str()) else {
            return false;
        };
        let company = d.get("company_number").and_then(|v| v.as_str());
        officer_name_slugs(name).into_iter().any(|slug| {
            format!("director:{slug}") == entity_id
                || company.is_some_and(|cn| format!("OFF:{cn}-{slug}") == entity_id)
        })
    })
    .await
    .ok()
    .flatten()?;

    // The node's REAL id, not the key that found it. Returning the key is what left 6,666
    // `off_composite` links pointing at `OFF:` ids that exist nowhere.
    extract_officer_entity_id(&hit)
}

/// Read `payload.entity_id` off a `graph_events` row as a plain string.
///
/// SurrealDB 3.x coerces `payload.entity_id` to a `RecordId` for `director:*` values, so it comes
/// back as an object rather than a string; both shapes are handled.
fn extract_officer_entity_id(row: &serde_json::Value) -> Option<String> {
    let v = &row["payload"]["entity_id"];
    if let Some(s) = v.as_str() {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    // RecordId form: {"tb": "director", "id": {"String": "smith-john"}} and near variants.
    let tb = v.get("tb").and_then(|t| t.as_str())?;
    let id = v.get("id")?;
    let id_str = id
        .as_str()
        .map(str::to_owned)
        .or_else(|| id.get("String").and_then(|x| x.as_str()).map(str::to_owned))?;
    Some(format!("{tb}:{id_str}"))
}

// ── PscAdapter ───────────────────────────────────────────────────────────────

/// CH PSC beneficial-ownership adapter.
///
/// Consumes the PSC stream, emits `Psc` nodes + `Controls` edges via `mutation_tx`,
/// and writes confidence-1.0 `identity_link`s to SurrealDB when a PSC matches a
/// known officer node.
///
/// Is disabled (`is_enabled() == false`) when `api_key` is absent or empty (CHA-03).
pub struct PscAdapter {
    /// CH Streaming API key for the enricher (separate from the ingestor key).
    /// None or empty → `is_enabled()` returns false (graceful degradation).
    pub api_key: Option<String>,
}

impl PscAdapter {
    pub fn new(api_key: Option<String>) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl EnricherAdapter for PscAdapter {
    fn name(&self) -> &'static str {
        "ch_psc"
    }

    /// Returns true only when a non-empty, non-whitespace API key is present.
    ///
    /// Mirrors the `ChEnricherConfig::is_enabled()` pattern (CHA-03):
    /// treats a whitespace-only key as absent.
    fn is_enabled(&self) -> bool {
        self.api_key
            .as_deref()
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }

    /// Run a single PSC enrichment pass.
    ///
    /// In the real adapter this would invoke `run_ch_stream`. For Phase 17 the
    /// run loop is structured so unit tests can drive it without a live CH stream:
    /// the parsing and matching logic is fully exercised via fixture-based tests,
    /// and the run() body can be wired to a real stream when a live key is present.
    ///
    /// Run one PSC enrichment pass: iterate every company number in the graph,
    /// fetch its PSC list from the CH REST API, and pipe each item through the
    /// existing `process_psc_line` pipeline to write identity_links.
    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        if !self.is_enabled() {
            warn!(
                adapter = "ch_psc",
                "Adapter not enabled — api_key absent or empty (CHA-03)"
            );
            return Ok(EnrichmentStats::default());
        }

        let api_key = self.api_key.as_deref().unwrap_or_default().to_string();
        let mut stats = EnrichmentStats::default();

        // Pre-load all officer IDs once (one DB scan, no RecordId coercion, O(1) lookup per PSC).
        let officer_id_set = build_officer_id_set(db).await?;
        info!(
            adapter = "ch_psc",
            officer_id_count = officer_id_set.len(),
            "Officer ID set pre-loaded"
        );

        let company_numbers = fetch_company_numbers_from_graph(db).await?;
        info!(
            adapter = "ch_psc",
            company_count = company_numbers.len(),
            "Fetching PSC beneficial ownership via CH REST API"
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        for company_number in &company_numbers {
            let url = format!(
                "https://api.companieshouse.gov.uk/company/{}/persons-with-significant-control",
                company_number
            );
            let resp = match client
                .get(&url)
                .basic_auth(&api_key, None::<&str>)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    debug!(company_number, error = %e, "PSC REST: request failed — skipping");
                    continue;
                }
            };
            if !resp.status().is_success() {
                if resp.status().as_u16() != 404 {
                    debug!(
                        company_number,
                        status = resp.status().as_u16(),
                        "PSC REST: non-200 — skipping"
                    );
                }
                continue;
            }
            let body: serde_json::Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    debug!(company_number, error = %e, "PSC REST: JSON parse failed — skipping");
                    continue;
                }
            };

            let items = body
                .get("items")
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default();

            for item in &items {
                // Skip corporate PSCs (natural persons only, Phase 17).
                let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if skip_if_corporate(kind) {
                    continue;
                }

                stats.candidates_found += 1;

                // Build display name from name_elements (preferred) or name field.
                let display_name = {
                    let forename = item
                        .get("name_elements")
                        .and_then(|ne| ne.get("forename"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let surname = item
                        .get("name_elements")
                        .and_then(|ne| ne.get("surname"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let full = format!("{forename} {surname}").trim().to_string();
                    if full.is_empty() {
                        let raw = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        strip_title_prefix(raw).to_string()
                    } else {
                        full
                    }
                };

                let name_slug = normalise_name(&display_name);
                // Director keys use hyphens (pa_link_officers.py slug format).
                let hyphen_slug = name_slug.replace(' ', "-");

                // In-memory officer match — O(1), no per-item DB scan.
                let off_key = format!("OFF:{company_number}-{hyphen_slug}");
                let dir_key = format!("director:{hyphen_slug}");
                let officer_match = officer_id_set
                    .get(&off_key)
                    .or_else(|| officer_id_set.get(&dir_key))
                    .cloned();

                // Emit PSC node + Controls edge via mutation_tx (best-effort; channel may be
                // closed in CLI enrich mode where GraphCoordinator is not running).
                let psc_id = format!("psc:{company_number}-{hyphen_slug}");
                let notified_on_dt =
                    item.get("notified_on")
                        .and_then(|v| v.as_str())
                        .and_then(|s| {
                            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                                .ok()
                                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
                        });
                let natures_of_control = item
                    .get("natures_of_control")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    });
                let _ = mutation_tx
                    .send(GraphMutation::AddNode {
                        id: EntityId(psc_id.clone()),
                        weight: NodeWeight::Psc(PscData {
                            name: display_name.clone(),
                            company_number: Some(company_number.clone()),
                            natures_of_control,
                        }),
                    })
                    .await;
                let _ = mutation_tx
                    .send(GraphMutation::AddEdge {
                        src: EntityId(psc_id.clone()),
                        dst: EntityId(format!("GB-COH:{company_number}")),
                        weight: EdgeWeight::Controls {
                            meta: EdgeMeta {
                                created_at: Utc::now(),
                                effective_from: notified_on_dt,
                                effective_to: None,
                            },
                        },
                    })
                    .await;

                // Write identity_link when officer matched.
                if let Some(ref officer_id) = officer_match {
                    if is_valid_ch_number(company_number) {
                        if let Some(link) = build_identity_link(
                            &psc_id,
                            &officer_id.node_id,
                            officer_id.company_corroborated,
                            &hyphen_slug,
                            company_number,
                        ) {
                            write_identity_link(db, link.record()).await?;
                            stats.auto_linked += 1;
                        }
                    }
                }
            }

            // CH REST API rate limit: ~600 req/5 min. 500ms is conservative.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        info!(
            adapter = "ch_psc",
            candidates = stats.candidates_found,
            auto_linked = stats.auto_linked,
            "PSC enrichment complete"
        );
        Ok(stats)
    }
}

// ── Graph helpers ────────────────────────────────────────────────────────────

/// Every slug an officer's name should be findable under.
///
/// Companies House returns officer names **surname-first with a comma** — `"EXAMPLE, Jane
/// Alexandra"`. The PSC register returns the same person forename-first — `"Jane Alexandra
/// Example"`. `normalise_name` strips the comma to whitespace but preserves word order, so the
/// two sides slug to `example-jane-alexandra` and `jane-alexandra-example` and can never be
/// equal. Measured 2026-08-03 against `an_anglesey_aebaa785` (21,923 officers × 17,298 PSCs):
/// **18 key matches as-shipped, 6,659 once the comma convention is parsed** — the PSC↔officer
/// join was running at 0.3% of its yield.
///
/// This is a deterministic reordering of a documented convention followed by exact string
/// equality — not a similarity match. It does not reopen the exact-match-only policy: no
/// scores, no phonetics, no threshold. Both orderings are emitted so an officer whose name
/// carries no comma (already forename-first) keeps matching as before.
fn officer_name_slugs(name: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    let as_written = normalise_name(name).replace(' ', "-");
    if !as_written.is_empty() {
        out.push(as_written);
    }
    // "SURNAME, Forenames" → "Forenames SURNAME". Only the first comma is structural; anything
    // after it (", Dr", ", Jr") stays attached to the forenames, which is what CH does too.
    if let Some((surname, forenames)) = name.split_once(',') {
        let surname = surname.trim();
        let forenames = forenames.trim();
        if !surname.is_empty() && !forenames.is_empty() {
            let reordered = normalise_name(&format!("{forenames} {surname}")).replace(' ', "-");
            if !reordered.is_empty() && !out.contains(&reordered) {
                out.push(reordered);
            }
        }
    }
    out
}

/// The company number an officer node is scoped to.
///
/// **Read the weight first, the entity id second.** The two sources are two eras:
///
/// - Officers used to be keyed `OFF:{company_number}-{NAME_SLUG}` by `scripts/ch_backfill.py`,
///   with the company number living **only** in the id. Reading `weight_json.company_number`
///   back then always yielded `None`, so no `OFF:` key was built and every PSC fell through to
///   the name-only `director:` path (measured 2026-08-03: 6,862 PSC matches, **0** company-scoped).
/// - PR #38 (2026-08-15) re-keyed every officer to `director:{slug}` so one human is one node
///   across companies, and moved the company scope **into `OfficerData::company_number`**. The
///   entity id stopped carrying it.
///
/// Reading only the id was correct for the first era and became a silent no-op in the second.
/// Measured live 2026-08-16 on `an_anglesey_aebaa785`: **0 of 22,324** officer nodes still use an
/// `OFF:` id, while **1,299 carry `company_number` in their weight** — so the id path returned
/// `None` for 100% of officers and the `off_composite` corroboration class could no longer be
/// produced at all. The id fallback is kept for any node still written the old way.
fn officer_company_number<'a>(
    entity_id: &'a str,
    weight: &'a serde_json::Value,
) -> Option<&'a str> {
    if let Some(cn) = weight.get("company_number").and_then(|v| v.as_str()) {
        if !cn.is_empty() {
            return Some(cn);
        }
    }
    let rest = entity_id.strip_prefix("OFF:")?;
    // The slug may contain '-', the company number may not: split on the FIRST '-' only.
    let (cn, _slug) = rest.split_once('-')?;
    (!cn.is_empty()).then_some(cn)
}

/// Every key an officer node should be findable under, paired with its **real entity id**.
///
/// The id and the key are not the same string and must never be conflated: a company-scoped
/// match is a statement about *corroboration*, while the entity id is what any `identity_link`
/// has to point at. Conflating them is how 6,666 `off_composite` links came to carry a
/// `target_node_id` — `OFF:{cn}-{slug}` — that matches no node in the database.
fn officer_index_entries(entity_id: &str, weight: &serde_json::Value) -> Vec<(String, bool)> {
    let Some(name) = weight.get("name").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    if name.is_empty() {
        return Vec::new();
    }
    let company = officer_company_number(entity_id, weight);
    let mut out = Vec::new();
    for slug in officer_name_slugs(name) {
        match company {
            // Company-scoped: the composite key corroborates the name with an identifier, so a
            // PSC at company A can no longer match a same-named officer at company B on the
            // name alone — the failure class that produced 196 cross-company links on 2026-08-03.
            Some(cn) => out.push((format!("OFF:{cn}-{slug}"), true)),
            None => out.push((format!("director:{slug}"), false)),
        }
    }
    out
}

/// Pre-load all known officer entity IDs into a HashSet for fast in-memory lookup.
///
/// Keys come from [`officer_index_entries`], which reads the company scope from the officer's
/// **weight** and falls back to its entity id (see [`officer_company_number`] for why both).
///
/// The two officer populations are keyed differently on purpose, and conflating them is what
/// produced 196 cross-company links on 2026-08-03:
///
/// - **Officers whose company number is known** get **only** the company-scoped
///   `OFF:{cn}-{slug}` key. Emitting a bare `director:{slug}` for them as well would let a PSC
///   at company A match an officer of the same name at company B — an exact string match, but
///   on a name alone, with no corroborating identifier. That is the failure class purged from
///   the charity and EC adapters on 2026-06-13, and the reason those 196 links existed.
/// - **Officers with no company number** get the `director:` key. Planning-portal officers
///   (written by `pa_link_officers.py`) are name-keyed by design — it is the project's cross-LPA
///   dedup convention — and carry nothing to scope them by.
///
/// Note the split is on **what the node knows**, not on which id scheme it uses: since PR #38
/// every officer is `director:`-keyed, so the old "CH officers are `OFF:`-keyed" reading of this
/// rule silently classified all 22,324 of them as unscopable.
///
/// Every ordering from [`officer_name_slugs`] is indexed under whichever scheme applies.
async fn build_officer_id_set(
    db: &Surreal<Any>,
) -> anyhow::Result<std::collections::HashMap<String, OfficerMatch>> {
    let officers = scan_entities_by_type(db, "NodeAdded", "\"type\":\"Officer\"").await?;

    let mut index = std::collections::HashMap::new();
    for (entity_id, weight) in &officers {
        for (key, company_corroborated) in officer_index_entries(entity_id, weight) {
            // Value is the officer's REAL id, so a lookup by composite key still yields the
            // `director:{slug}` a link has to point at.
            index.insert(
                key,
                OfficerMatch {
                    node_id: entity_id.clone(),
                    company_corroborated,
                },
            );
        }
    }
    Ok(index)
}

/// Collect unique CH company numbers from Entity NodeAdded events in graph_events.
///
/// Scanned through [`scan_weights_by_type`] rather than a direct query. The previous form paired
/// the indexed `kind` equality with two `CONTAINS` predicates under a `LIMIT 5000`, which is the
/// SurrealDB 3.x planner-bug shape: measured 2026-07-29 against `an_anglesey_aebaa785`, `count()`
/// on that exact predicate reported **14,198** while the `SELECT` returned **39**. The adapter had
/// been enriching 0.27% of its candidate companies and reporting success — its
/// `enrichment_runs:ch_psc` marker recorded `candidates_found: 0` and the `psc` table stayed
/// empty.
async fn fetch_company_numbers_from_graph(db: &Surreal<Any>) -> anyhow::Result<Vec<String>> {
    let weights = scan_weights_by_type(db, "NodeAdded", "\"type\":\"Entity\"").await?;

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for d in &weights {
        if let Some(cn) = d.get("company_number").and_then(|v| v.as_str()) {
            if !cn.is_empty() && seen.insert(cn.to_string()) {
                out.push(cn.to_string());
            }
        }
    }
    Ok(out)
}

// ── Core processing logic ────────────────────────────────────────────────────

/// Process a single NDJSON line from the PSC stream.
///
/// This is the unit-testable entry point for the PSC parsing + matching pipeline.
/// Called by the `StreamEventHandler` in the live run loop, and directly by tests.
///
/// Returns `None` for corporate PSCs (skipped) or malformed payloads.
/// Returns `Some(ProcessedPsc)` for individual PSCs.
pub async fn process_psc_line(
    db: &Surreal<Any>,
    mutation_tx: &Sender<GraphMutation>,
    line: &str,
) -> Result<Option<ProcessedPsc>> {
    // Parse the top-level stream event envelope.
    let event: PscStreamEvent = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => {
            debug!(error = %e, "PSC stream: malformed line — skipping");
            return Ok(None);
        }
    };

    let data = match event.data {
        Some(d) => d,
        None => {
            debug!("PSC stream: event has no data field — skipping");
            return Ok(None);
        }
    };

    // Skip corporate PSCs (out of scope, Phase 17 natural persons only).
    if skip_if_corporate(&data.kind) {
        debug!(kind = %data.kind, "PSC stream: corporate/legal-person kind — skipping");
        return Ok(None);
    }

    // Extract company number from resource_uri / links.self (never from data.company_number).
    let resource_uri = event.resource_uri.as_deref().unwrap_or("");
    let links_self = data.links.as_ref().and_then(|l| l.self_link.as_deref());
    let company_number = match extract_company_number(resource_uri, links_self) {
        Some(n) => n,
        None => {
            debug!("PSC stream: could not extract company_number from resource_uri — skipping");
            return Ok(None);
        }
    };

    // Security (T-17-02-03): re-validate the company number before any DB call.
    if !is_valid_ch_number(&company_number) {
        warn!(
            company_number = %company_number,
            "PSC stream: invalid company_number extracted — skipping"
        );
        return Ok(None);
    }

    // Build the display name from name_elements (forename + surname), falling back to `name`.
    let display_name = resolve_display_name(&data);

    // Normalise the name to a slug for officer matching (D-09 / R-01).
    let name_slug = normalise_name(&display_name);

    // Build the Psc node data.
    let psc_node_data = PscData {
        name: display_name.clone(),
        company_number: Some(company_number.clone()),
        natures_of_control: data.natures_of_control.clone(),
    };

    // Emit the Psc node (always, regardless of officer match — D-07).
    let psc_id = format!("psc:{company_number}-{name_slug}");
    let add_node = GraphMutation::AddNode {
        id: EntityId(psc_id.clone()),
        weight: NodeWeight::Psc(psc_node_data),
    };
    if let Err(e) = mutation_tx.send(add_node).await {
        warn!(error = %e, "PSC stream: failed to send AddNode mutation — channel closed?");
    }

    // Emit the Controls edge: Psc → GB-COH:{company_number}.
    let company_entity_id = format!("GB-COH:{company_number}");
    let notified_on_dt = data.notified_on.as_deref().and_then(|s| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
    });

    let edge_meta = EdgeMeta {
        created_at: Utc::now(),
        effective_from: notified_on_dt,
        effective_to: None,
    };
    let add_edge = GraphMutation::AddEdge {
        src: EntityId(psc_id.clone()),
        dst: EntityId(company_entity_id),
        weight: EdgeWeight::Controls { meta: edge_meta },
    };
    if let Err(e) = mutation_tx.send(add_edge).await {
        warn!(error = %e, "PSC stream: failed to send AddEdge mutation — channel closed?");
    }

    // Attempt to match an officer node (R-01: OFF composite first, director slug fallback).
    let officer_match = match_officer(db, &company_number, &name_slug).await;

    // If a match is found, write a confidence-1.0 identity_link.
    let identity_link = if let Some(ref officer_id) = officer_match {
        let link = build_identity_link(
            &psc_id,
            &officer_id.node_id,
            officer_id.company_corroborated,
            &name_slug,
            &company_number,
        );
        if let Some(confirmed) = link {
            write_identity_link(db, confirmed.record()).await?;
            Some(confirmed)
        } else {
            None
        }
    } else {
        None
    };

    Ok(Some(ProcessedPsc {
        psc_id,
        company_number,
        name_slug,
        officer_match,
        identity_link_written: identity_link.is_some(),
    }))
}

/// Result of processing a single PSC stream event.
#[derive(Debug)]
pub struct ProcessedPsc {
    pub psc_id: String,
    pub company_number: String,
    pub name_slug: String,
    pub officer_match: Option<OfficerMatch>,
    pub identity_link_written: bool,
}

/// Build a confidence-1.0 ConfirmedLink for a PSC → officer exact match.
///
/// The `evidence_json` is a minimal flat JSON string (ENR-06):
/// never a nested object, never contains full PII beyond name_slug + company_number.
///
/// Security (T-17-02-02): evidence_json stores only sanitised slug + metadata.
fn build_identity_link(
    psc_id: &str,
    officer_id: &str,
    company_corroborated: bool,
    name_slug: &str,
    company_number: &str,
) -> Option<ConfirmedLink> {
    let score = ConfidenceScore::new(1.0).ok()?;

    // The match CLASS comes from the caller, not from the shape of `officer_id`. Reading it back
    // off an `OFF:` prefix required the id to double as the class marker, which is why every
    // `off_composite` link written before 2026-08-16 targets a node that does not exist.
    let resolution_type = if company_corroborated {
        "off_composite"
    } else {
        "director_slug"
    };

    // ENR-06: evidence_json must be a flat string, never a nested SurrealDB object.
    let evidence_json = serde_json::json!({
        "name_slug": name_slug,
        "company_number": company_number,
        "match": resolution_type,
        "score": 1.0
    })
    .to_string();

    let record = IdentityLinkRecord {
        id: format!("identity_links:{}", uuid::Uuid::new_v4()),
        source_node_id: psc_id.to_string(),
        target_node_id: officer_id.to_string(),
        resolution_type: resolution_type.to_string(),
        confidence: 1.0,
        source: "ch_psc".to_string(),
        resolved_at: Utc::now(),
        evidence_json,
        status: LinkStatus::AutoLinked,
    };

    ConfirmedLink::try_new(score, record)
}

/// Outcome of [`backfill_psc_officer_links`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PscLinkBackfillStats {
    /// Psc nodes read from `graph_events`.
    pub pscs_scanned: usize,
    /// Psc nodes whose name matched a known officer key.
    pub matched: usize,
    /// Matches that were already linked and were therefore skipped.
    pub already_linked: usize,
    /// New `identity_links` rows written (zero when `dry_run`).
    pub written: usize,
    /// Matches rejected because the company number was malformed.
    pub invalid_company_number: usize,
}

/// Re-run the PSC↔officer join over data already in the graph, writing any links the
/// live adapter missed — no Companies House calls.
///
/// The join normally happens inside [`PscAdapter::run_once`]'s fetch loop, so correcting the
/// matcher (see [`officer_name_slugs`]) would otherwise mean re-fetching every PSC record from
/// the REST API to re-derive links from data already staged. This reads both sides out of
/// `graph_events` and applies the identical matcher, gate and record builder:
/// [`build_officer_id_set`], the `OFF:`-then-`director:` key preference, [`is_valid_ch_number`]
/// and [`build_identity_link`]. It deliberately shares those functions rather than restating
/// them, so a future change to the adapter's linking cannot silently diverge from this path.
///
/// **Idempotent.** Existing `ch_psc` links are read first and matched pairs already present are
/// skipped, because [`build_identity_link`] mints a fresh UUID per row and would otherwise
/// duplicate every link on a second run.
pub async fn backfill_psc_officer_links(
    db: &Surreal<Any>,
    dry_run: bool,
) -> anyhow::Result<PscLinkBackfillStats> {
    let mut stats = PscLinkBackfillStats::default();

    let officer_id_set = build_officer_id_set(db).await?;
    info!(
        officer_id_count = officer_id_set.len(),
        "psc-links backfill: officer ID set pre-loaded"
    );

    // Pairs already linked by this source, so a re-run is a no-op rather than a duplicator.
    // Paged rather than capped: a ceiling here would silently under-report what already exists
    // and duplicate exactly the rows it failed to see.
    let mut existing: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    let mut start = 0usize;
    loop {
        const PAGE: usize = 20_000;
        let sql = format!(
            "SELECT id, source_node_id, target_node_id FROM identity_links \
             WHERE source = $source ORDER BY id LIMIT {PAGE} START {start}"
        );
        let rows: Vec<serde_json::Value> = db
            .query(sql)
            .bind(("source", "ch_psc"))
            .await?
            .take(0)
            .unwrap_or_default();
        let returned = rows.len();
        if returned == 0 {
            break;
        }
        for r in &rows {
            if let (Some(s), Some(t)) = (
                r.get("source_node_id").and_then(|v| v.as_str()),
                r.get("target_node_id").and_then(|v| v.as_str()),
            ) {
                existing.insert((s.to_owned(), t.to_owned()));
            }
        }
        start += returned;
    }
    info!(
        existing_links = existing.len(),
        "psc-links backfill: existing ch_psc links loaded"
    );

    let pscs = scan_entities_by_type(db, "NodeAdded", "\"type\":\"Psc\"").await?;
    stats.pscs_scanned = pscs.len();

    for (psc_id, weight) in &pscs {
        let Some(name) = weight.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(company_number) = weight.get("company_number").and_then(|v| v.as_str()) else {
            continue;
        };
        let hyphen_slug = normalise_name(name).replace(' ', "-");

        // Same key preference as the adapter: the company-scoped composite first, then the
        // cross-company director slug.
        let off_key = format!("OFF:{company_number}-{hyphen_slug}");
        let dir_key = format!("director:{hyphen_slug}");
        let Some(officer) = officer_id_set
            .get(&off_key)
            .or_else(|| officer_id_set.get(&dir_key))
            .cloned()
        else {
            continue;
        };
        let officer_id = officer.node_id;
        stats.matched += 1;

        if !is_valid_ch_number(company_number) {
            stats.invalid_company_number += 1;
            continue;
        }
        if existing.contains(&(psc_id.clone(), officer_id.clone())) {
            stats.already_linked += 1;
            continue;
        }
        let Some(link) = build_identity_link(
            psc_id,
            &officer_id,
            officer.company_corroborated,
            &hyphen_slug,
            company_number,
        ) else {
            continue;
        };
        if dry_run {
            stats.written += 1;
            continue;
        }
        write_identity_link(db, link.record()).await?;
        stats.written += 1;
    }

    info!(
        pscs_scanned = stats.pscs_scanned,
        matched = stats.matched,
        already_linked = stats.already_linked,
        written = stats.written,
        dry_run,
        "psc-links backfill complete"
    );
    Ok(stats)
}

/// Public test helper that exposes `build_identity_link` for integration tests.
///
/// This allows `tests/psc_adapter.rs` to exercise the link-building logic without
/// needing a live DB or stream connection.
pub fn build_identity_link_for_test(
    psc_id: &str,
    officer_id: &str,
    company_corroborated: bool,
    name_slug: &str,
    company_number: &str,
) -> Option<ConfirmedLink> {
    build_identity_link(
        psc_id,
        officer_id,
        company_corroborated,
        name_slug,
        company_number,
    )
}

/// Resolve a display name from name_elements (preferred) or `name` field (fallback).
fn resolve_display_name(data: &PscEventData) -> String {
    if let Some(ref ne) = data.name_elements {
        let forename = ne.forename.as_deref().unwrap_or("");
        let surname = ne.surname.as_deref().unwrap_or("");
        let full = format!("{forename} {surname}").trim().to_string();
        if !full.is_empty() {
            return full;
        }
    }
    // Fallback to the `name` field; strip titles ("Mr ", "Ms ", etc.) for slug quality.
    let raw = data.name.as_deref().unwrap_or("");
    strip_title_prefix(raw).to_string()
}

/// Strip common title prefixes from a display name.
fn strip_title_prefix(name: &str) -> &str {
    let prefixes = [
        "Mr ", "Mrs ", "Ms ", "Miss ", "Dr ", "Prof ", "Sir ", "Dame ",
    ];
    for p in &prefixes {
        if let Some(rest) = name.strip_prefix(p) {
            return rest.trim_start();
        }
    }
    name
}

/// The identity-link UPSERT, shared verbatim by every adapter that writes a link.
///
/// Note the two predicates differ, deliberately:
///
/// - the `UPDATE` subquery excludes `status = 'rejected'`, so a link an operator has
///   disowned via `links reject` is never silently restored by a later `enrich` run;
/// - the existence check does **not** exclude it, so a rejected row is not re-inserted as a
///   duplicate against the composite unique index `(source_node_id, target_node_id, source)`.
///
/// Before 2026-08-20 both predicates matched on the triple alone and the `SET` re-wrote
/// `status` unconditionally, so rejection lasted exactly until the next adapter run — and
/// nothing anywhere read `rejected` back, so the reversal was invisible. Rejection is the only
/// operator-facing way to disown a false identity link between two named people.
///
/// This SQL lives in one place because it had been duplicated into `ch_disqualified.rs`, which
/// meant the missing guard had to be found and fixed twice.
pub(crate) const IDENTITY_LINK_UPSERT: &str = "UPDATE (SELECT id FROM identity_links \
     WHERE source_node_id = $src \
       AND target_node_id = $tgt \
       AND source = $source \
       AND status != 'rejected') \
   SET source_node_id = $src, \
       target_node_id = $tgt, \
       resolution_type = $res_type, \
       confidence = $confidence, \
       source = $source, \
       resolved_at = $resolved_at, \
       evidence_json = $evidence_json, \
       status = $status; \
   IF (SELECT id FROM identity_links \
     WHERE source_node_id = $src \
       AND target_node_id = $tgt \
       AND source = $source) = [] \
   THEN \
     (INSERT INTO identity_links { \
       source_node_id: $src, \
       target_node_id: $tgt, \
       resolution_type: $res_type, \
       confidence: $confidence, \
       source: $source, \
       resolved_at: $resolved_at, \
       evidence_json: $evidence_json, \
       status: $status \
     }) \
   END";

/// Write a confirmed identity link to SurrealDB using the subquery-guarded UPSERT (ENR-02).
///
/// The subquery UPDATE guard prevents duplicate links on re-run:
/// `UPDATE (SELECT id FROM identity_links WHERE source_node_id = $src AND ...) SET ...`
///
/// Uses bound parameters throughout (T-17-02-01 mitigation).
///
/// `evidence_json` is stored as TYPE string — never a nested object (ENR-06).
pub async fn write_identity_link(db: &Surreal<Any>, record: &IdentityLinkRecord) -> Result<()> {
    // Subquery-guarded UPSERT (ENR-02): updates existing row if present,
    // inserts if absent — composite unique index (source_node_id, target_node_id, source)
    // prevents true duplicates.
    //
    // Security (T-17-02-01): all values are bound parameters, never string-formatted.
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
            "PSC: identity_link write failed"
        );
    }

    result
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("PSC identity_link write failed: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_company_number tests ─────────────────────────────────────────

    #[test]
    fn test_extract_company_number_from_resource_uri() {
        let uri = "/company/12345678/persons-with-significant-control/individual/abcd1234";
        let result = extract_company_number(uri, None);
        assert_eq!(result, Some("12345678".to_string()));
    }

    #[test]
    fn test_extract_company_number_sc_prefix() {
        let uri = "/company/SC123456/persons-with-significant-control/individual/xyz";
        let result = extract_company_number(uri, None);
        assert_eq!(result, Some("SC123456".to_string()));
    }

    #[test]
    fn test_extract_company_number_falls_back_to_links_self() {
        let links_self = "/company/99887766/persons-with-significant-control/individual/efgh5678";
        let result = extract_company_number("", Some(links_self));
        assert_eq!(result, Some("99887766".to_string()));
    }

    #[test]
    fn test_extract_company_number_invalid_path() {
        let result = extract_company_number("/invalid/path", None);
        assert_eq!(result, None);
    }

    // ── skip_if_corporate tests ──────────────────────────────────────────────

    #[test]
    fn test_skip_corporate_entity_kind() {
        assert!(skip_if_corporate(
            "corporate-entity-person-with-significant-control"
        ));
    }

    #[test]
    fn test_skip_legal_person_kind() {
        assert!(skip_if_corporate(
            "legal-person-person-with-significant-control"
        ));
    }

    #[test]
    fn test_individual_kind_not_skipped() {
        assert!(!skip_if_corporate(
            "individual-person-with-significant-control"
        ));
    }

    // ── is_valid_ch_number tests ─────────────────────────────────────────────

    #[test]
    fn test_valid_ch_number_digits() {
        assert!(is_valid_ch_number("12345678"));
    }

    #[test]
    fn test_valid_ch_number_sc_prefix() {
        assert!(is_valid_ch_number("SC123456"));
    }

    #[test]
    fn test_valid_ch_number_ni_prefix() {
        assert!(is_valid_ch_number("NI123456"));
    }

    #[test]
    fn test_invalid_ch_number_special_chars() {
        assert!(!is_valid_ch_number("12345678; DELETE"));
        assert!(!is_valid_ch_number("'quoted'"));
        assert!(!is_valid_ch_number(""));
    }

    // ── normalise_name integration ───────────────────────────────────────────

    #[test]
    fn test_normalise_john_smith() {
        let slug = normalise_name("John Smith");
        assert_eq!(slug, "john smith");
    }

    #[test]
    fn test_strip_title_mr() {
        assert_eq!(strip_title_prefix("Mr John Smith"), "John Smith");
    }

    #[test]
    fn test_strip_title_ms() {
        assert_eq!(strip_title_prefix("Ms Jane Doe"), "Jane Doe");
    }

    #[test]
    fn test_strip_title_no_prefix() {
        assert_eq!(strip_title_prefix("John Smith"), "John Smith");
    }

    // ── build_identity_link tests ────────────────────────────────────────────

    #[test]
    fn test_build_identity_link_off_composite() {
        // The target is the officer's REAL id. `off_composite` is now decided by the
        // corroboration flag, not by an `OFF:` prefix on a synthesised key — writing that key
        // into `target_node_id` is what left 6,666 links pointing at no node.
        let link = build_identity_link(
            "psc:12345678-john-smith",
            "director:john-smith",
            true,
            "john smith",
            "12345678",
        );
        assert!(link.is_some());
        let confirmed = link.unwrap();
        assert!((confirmed.score().value() - 1.0).abs() < f64::EPSILON);
        assert_eq!(confirmed.record().resolution_type, "off_composite");
        assert_eq!(
            confirmed.record().target_node_id,
            "director:john-smith",
            "an off_composite link must target the officer's real node id, never the OFF: key"
        );
        assert_eq!(confirmed.record().status, LinkStatus::AutoLinked);
        assert_eq!(confirmed.record().source, "ch_psc");
        // Verify evidence_json is a flat string (ENR-06)
        assert!(confirmed.record().evidence_json.contains("\"name_slug\""));
        assert!(!confirmed.record().evidence_json.is_empty());
        // Verify it parses as valid JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&confirmed.record().evidence_json).unwrap();
        assert_eq!(parsed["name_slug"], "john smith");
        assert_eq!(parsed["company_number"], "12345678");
        assert_eq!(parsed["match"], "off_composite");
        assert_eq!(parsed["score"], 1.0);
    }

    #[test]
    fn test_build_identity_link_director_slug() {
        let link = build_identity_link(
            "psc:12345678-jane-doe",
            "director:jane-doe",
            false,
            "jane doe",
            "12345678",
        );
        assert!(link.is_some());
        let confirmed = link.unwrap();
        assert_eq!(confirmed.record().resolution_type, "director_slug");
    }

    // ── PscAdapter is_enabled tests ──────────────────────────────────────────

    #[test]
    fn test_psc_adapter_disabled_when_no_key() {
        let adapter = PscAdapter::new(None);
        assert!(!adapter.is_enabled());
    }

    #[test]
    fn test_psc_adapter_disabled_when_empty_key() {
        let adapter = PscAdapter::new(Some(String::new()));
        assert!(!adapter.is_enabled());
    }

    #[test]
    fn test_psc_adapter_disabled_when_whitespace_key() {
        let adapter = PscAdapter::new(Some("   ".to_string()));
        assert!(!adapter.is_enabled());
    }

    #[test]
    fn test_psc_adapter_enabled_with_real_key() {
        let adapter = PscAdapter::new(Some("live-api-key-value".to_string()));
        assert!(adapter.is_enabled());
    }

    #[test]
    fn test_psc_adapter_name() {
        let adapter = PscAdapter::new(None);
        assert_eq!(adapter.name(), "ch_psc");
    }

    // ── resolve_display_name tests (pure function, no DB) ───────────────────────

    #[test]
    fn test_resolve_display_name_prefers_name_elements() {
        let data = PscEventData {
            kind: "individual-person-with-significant-control".to_string(),
            name: Some("Mr John Smith".to_string()),
            name_elements: Some(NameElements {
                forename: Some("John".to_string()),
                surname: Some("Smith".to_string()),
                title: Some("Mr".to_string()),
            }),
            natures_of_control: None,
            notified_on: None,
            links: None,
        };
        // name_elements forename+surname wins over the name field.
        assert_eq!(resolve_display_name(&data), "John Smith");
    }

    #[test]
    fn test_resolve_display_name_falls_back_to_name() {
        let data = PscEventData {
            kind: "individual-person-with-significant-control".to_string(),
            name: Some("Jane Doe".to_string()),
            name_elements: None,
            natures_of_control: None,
            notified_on: None,
            links: None,
        };
        assert_eq!(resolve_display_name(&data), "Jane Doe");
    }

    #[test]
    fn test_resolve_display_name_strips_mr_title_in_fallback() {
        let data = PscEventData {
            kind: "individual-person-with-significant-control".to_string(),
            name: Some("Mr John Smith".to_string()),
            name_elements: None,
            natures_of_control: None,
            notified_on: None,
            links: None,
        };
        // strip_title_prefix removes "Mr " in the name-field fallback path.
        assert_eq!(resolve_display_name(&data), "John Smith");
    }

    #[test]
    fn test_resolve_display_name_empty_name_elements_falls_back() {
        // forename=None, surname=None → full="" → falls back to name field.
        let data = PscEventData {
            kind: "individual-person-with-significant-control".to_string(),
            name: Some("Alice Brown".to_string()),
            name_elements: Some(NameElements {
                forename: None,
                surname: None,
                title: None,
            }),
            natures_of_control: None,
            notified_on: None,
            links: None,
        };
        assert_eq!(resolve_display_name(&data), "Alice Brown");
    }

    // ── DB integration tests (URBAN_DB_TESTS=1 required) ───────────────────────

    /// Helper: connect to SurrealDB test instance.
    ///
    /// Uses the `urban`/`integrity` namespace (isolated from production
    /// `urban_integrity`/`main`) so tests never touch live data.
    #[cfg(test)]
    async fn connect_test_db() -> urban_integrity_test_support::TestNs {
        urban_integrity_test_support::TestNs::connect("integrity").await
    }

    /// The defect that kept the PSC↔officer join at 0.3% of its yield: Companies House
    /// returns officers surname-first with a comma, the PSC register returns them
    /// forename-first, and word order survived normalisation. Measured live: 18 key matches
    /// before, 6,659 after.
    #[test]
    fn officer_slugs_cover_the_ch_surname_first_convention() {
        let slugs = officer_name_slugs("EXAMPLE, Jane Alexandra");
        assert!(
            slugs.contains(&"jane-alexandra-example".to_string()),
            "the PSC register's ordering must be a key: {slugs:?}"
        );
        assert!(
            slugs.contains(&"example-jane-alexandra".to_string()),
            "the as-written ordering must survive so existing matches keep working: {slugs:?}"
        );
    }

    /// A name with no comma is already forename-first — one key, not a bogus second one.
    #[test]
    fn officer_slugs_leave_a_comma_free_name_alone() {
        assert_eq!(
            officer_name_slugs("Jane Alexandra Example"),
            vec!["jane-alexandra-example".to_string()]
        );
    }

    /// Trailing honorifics stay with the forenames, as Companies House writes them.
    #[test]
    fn officer_slugs_handle_a_trailing_honorific() {
        let slugs = officer_name_slugs("DE SAMPLE, Jordan, Dr");
        assert!(
            slugs.contains(&"jordan-dr-de-sample".to_string()),
            "only the first comma is structural: {slugs:?}"
        );
    }

    fn officer_weight(name: &str, company_number: Option<&str>) -> serde_json::Value {
        match company_number {
            Some(cn) => {
                serde_json::json!({"type": "Officer", "name": name, "company_number": cn})
            }
            None => serde_json::json!({"type": "Officer", "name": name}),
        }
    }

    /// Legacy `OFF:`-keyed nodes carry the company scope in the entity id and nothing else.
    #[test]
    fn officer_company_number_falls_back_to_the_entity_id() {
        let w = officer_weight("EXAMPLE, Jane Alexandra", None);
        assert_eq!(
            officer_company_number("OFF:99000042-EXAMPLE__JANE_ALEXANDRA", &w),
            Some("99000042")
        );
        // The slug contains '-'; only the first separator is structural.
        assert_eq!(
            officer_company_number("OFF:SC123456-DE-SAMPLE__JORDAN", &w),
            Some("SC123456")
        );
        // Planning-portal officers are name-keyed and have no company scope anywhere.
        assert_eq!(
            officer_company_number("director:jordan-de-sample", &w),
            None
        );
        assert_eq!(officer_company_number("OFF:-NO_NUMBER", &w), None);
    }

    /// Since PR #38 the scope lives in the WEIGHT and the id is `director:{slug}`. Reading only
    /// the id returned `None` for all 22,324 live officers, killing the `off_composite` class.
    #[test]
    fn officer_company_number_prefers_the_weight() {
        let w = officer_weight("SMITH, John", Some("12345678"));
        assert_eq!(
            officer_company_number("director:smith-john", &w),
            Some("12345678"),
            "a director:-keyed officer carrying a company number IS company-scoped"
        );
        // Weight wins over a legacy id, so a re-keyed node cannot regress to its old scope.
        assert_eq!(
            officer_company_number("OFF:99999999-SMITH__JOHN", &w),
            Some("12345678")
        );
        // An empty string is not a scope.
        let empty = officer_weight("SMITH, John", Some(""));
        assert_eq!(officer_company_number("director:smith-john", &empty), None);
    }

    #[test]
    fn officer_index_entries_scopes_by_the_weight_and_keys_both_orderings() {
        let w = officer_weight("SMITH, John", Some("12345678"));
        let entries = officer_index_entries("director:smith-john", &w);
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["OFF:12345678-smith-john", "OFF:12345678-john-smith"],
            "both name orderings, both company-scoped"
        );
        assert!(
            entries.iter().all(|(_, corroborated)| *corroborated),
            "a known company number corroborates every ordering"
        );
        assert!(
            !keys.iter().any(|k| k.starts_with("director:")),
            "a scoped officer must NOT also get a bare name key — that is the 196 \
             cross-company link failure class"
        );
    }

    #[test]
    fn officer_index_entries_falls_back_to_bare_keys_without_a_company() {
        let w = officer_weight("SMITH, John", None);
        let entries = officer_index_entries("director:smith-john", &w);
        assert_eq!(
            entries,
            vec![
                ("director:smith-john".to_string(), false),
                ("director:john-smith".to_string(), false),
            ]
        );
    }

    #[test]
    fn officer_index_entries_skips_a_nameless_officer() {
        assert!(
            officer_index_entries("director:x", &serde_json::json!({"type": "Officer"})).is_empty()
        );
        let blank = serde_json::json!({"type": "Officer", "name": ""});
        assert!(officer_index_entries("director:x", &blank).is_empty());
    }

    /// A malformed comma form must not produce an empty or half-built key.
    #[test]
    fn officer_slugs_reject_a_dangling_comma() {
        assert_eq!(
            officer_name_slugs("SMITH,"),
            vec!["smith".to_string()],
            "an empty forename half must not yield a second key"
        );
    }

    /// Regression: `build_officer_id_set` must produce `director:{hyphen-slug}` keys, not
    /// `director:{space slug}` keys.  Pre-fix, `normalise_name` returned "john smith"
    /// (spaces) which never matched the hyphen-keyed planning-portal nodes.
    #[tokio::test]
    async fn test_build_officer_id_set_hyphen_slug_regression() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let officer_name = format!("Slug Test Officer {run_id}");
        let weight_json = serde_json::json!({
            "type": "Officer",
            "name": officer_name,
        })
        .to_string();
        let record = serde_json::json!({
            "event_id": format!("test-slug-{run_id}"),
            "timestamp": chrono::Utc::now().timestamp_millis() as u64,
            "kind": "NodeAdded",
            "payload": {
                "entity_id": format!("director:slug-test-{run_id}"),
                "weight_json": weight_json,
            }
        });
        let _: surrealdb::Result<Vec<serde_json::Value>> = db
            .query("CREATE graph_events CONTENT $r")
            .bind(("r", record))
            .await
            .and_then(|mut r| r.take(0));

        let set = build_officer_id_set(&db).await.unwrap();

        // Key must use hyphens, not spaces.
        let expected_key = format!(
            "director:{}",
            normalise_name(&officer_name).replace(' ', "-")
        );
        let space_key = format!("director:{}", normalise_name(&officer_name)); // would be "director:slug test officer ..."
        assert!(
            set.contains_key(&expected_key),
            "build_officer_id_set must produce hyphen-slug key: expected {expected_key}"
        );
        assert!(
            !set.contains_key(&space_key),
            "build_officer_id_set must NOT produce space-slug key: {space_key}"
        );
    }

    /// Regression: `build_officer_id_set` must produce `OFF:{cn}-{slug}` composite keys for an
    /// officer keyed `director:{slug}` that carries `company_number` **in its weight** — the live
    /// shape since PR #38, and the one the old entity-id-only read missed for all 22,324 of them.
    ///
    /// The value must be the officer's REAL id, so a composite-key hit still yields something a
    /// link can point at.
    #[tokio::test]
    async fn test_build_officer_id_set_includes_off_composite() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let officer_name = format!("CH Composite {run_id}");
        let cn = "12345678";
        let expected_slug = normalise_name(&officer_name).replace(' ', "-");
        let real_id = format!("director:{expected_slug}");
        let weight_json = serde_json::json!({
            "type": "Officer",
            "name": officer_name,
            "company_number": cn,
        })
        .to_string();
        let record = serde_json::json!({
            "event_id": format!("test-off-{run_id}"),
            "timestamp": chrono::Utc::now().timestamp_millis() as u64,
            "kind": "NodeAdded",
            "payload": {
                // director:-keyed, NOT OFF:-keyed. The company scope is in the weight.
                "entity_id": real_id,
                "weight_json": weight_json,
            }
        });
        let _: surrealdb::Result<Vec<serde_json::Value>> = db
            .query("CREATE graph_events CONTENT $r")
            .bind(("r", record))
            .await
            .and_then(|mut r| r.take(0));

        let set = build_officer_id_set(&db).await.unwrap();

        let expected_key = format!("OFF:{cn}-{expected_slug}");
        let hit = set.get(&expected_key).unwrap_or_else(|| {
            panic!("build_officer_id_set must include OFF: composite key: {expected_key}")
        });
        assert_eq!(
            hit.node_id, real_id,
            "the composite key must resolve to the officer's real id, not to the key itself"
        );
        assert!(
            hit.company_corroborated,
            "a weight-supplied company number is corroboration"
        );
        assert!(
            !set.contains_key(&format!("director:{expected_slug}")),
            "a company-scoped officer must not also be reachable by a bare name key"
        );
    }

    /// `fetch_company_numbers_from_graph` must deduplicate — two NodeAdded events for
    /// the same company_number should yield exactly one entry.
    #[tokio::test]
    async fn test_fetch_company_numbers_deduplicates() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let cn = format!("DUP{}", &run_id[..6].to_uppercase());

        for i in 0..2u8 {
            let weight_json = serde_json::json!({
                "type": "Entity",
                "name": format!("Dup Co {i} {run_id}"),
                "company_number": cn,
            })
            .to_string();
            let record = serde_json::json!({
                "event_id": format!("test-cn-dup-{run_id}-{i}"),
                "timestamp": chrono::Utc::now().timestamp_millis() as u64,
                "kind": "NodeAdded",
                "payload": {
                    "entity_id": format!("GB-COH:{cn}"),
                    "weight_json": weight_json,
                }
            });
            let _: surrealdb::Result<Vec<serde_json::Value>> = db
                .query("CREATE graph_events CONTENT $r")
                .bind(("r", record))
                .await
                .and_then(|mut r| r.take(0));
        }

        let numbers = fetch_company_numbers_from_graph(&db).await.unwrap();
        let count = numbers.iter().filter(|n| n.as_str() == cn).count();
        assert_eq!(
            count, 1,
            "fetch_company_numbers_from_graph must deduplicate: got {count} occurrences of {cn}"
        );
    }

    /// Regression: `write_identity_link` must store status as a plain string
    /// ('auto_linked'), NOT as a JSON-quoted string ('"auto_linked"').
    ///
    /// Before the fix, serde_json::to_string(&status) was used in the bind,
    /// producing '"auto_linked"' stored verbatim in SurrealDB.
    #[tokio::test]
    async fn test_write_identity_link_status_stored_as_plain_string() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let record = IdentityLinkRecord {
            id: format!("identity_links:status-psc-{run_id}"),
            source_node_id: format!("psc:status-test-{run_id}"),
            target_node_id: format!("director:status-test-{run_id}"),
            resolution_type: "director_slug".to_string(),
            confidence: 1.0,
            source: "ch_psc".to_string(),
            resolved_at: chrono::Utc::now(),
            evidence_json: "{\"regression\":true}".to_string(),
            status: LinkStatus::AutoLinked,
        };

        write_identity_link(&db, &record).await.unwrap();

        let rows: Vec<serde_json::Value> = db
            .query(
                "SELECT status FROM identity_links \
                 WHERE source_node_id = $src AND target_node_id = $tgt AND source = 'ch_psc'",
            )
            .bind(("src", record.source_node_id.clone()))
            .bind(("tgt", record.target_node_id.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();

        assert!(!rows.is_empty(), "identity_link must have been written");
        let status = rows[0]["status"].as_str().unwrap_or("(missing)");
        assert_eq!(
            status, "auto_linked",
            "status must be plain 'auto_linked', not JSON-quoted '\"auto_linked\"'; got {status:?}"
        );
    }

    /// `write_identity_link` must be idempotent — writing the same (src, tgt, source)
    /// twice must result in exactly one row (ENR-02 subquery-guard UPSERT).
    #[tokio::test]
    async fn test_write_identity_link_idempotent() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }
        let db = connect_test_db().await;

        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let record = IdentityLinkRecord {
            id: format!("identity_links:idem-psc-{run_id}"),
            source_node_id: format!("psc:idem-{run_id}"),
            target_node_id: format!("director:idem-{run_id}"),
            resolution_type: "director_slug".to_string(),
            confidence: 1.0,
            source: "ch_psc".to_string(),
            resolved_at: chrono::Utc::now(),
            evidence_json: "{\"idem\":true}".to_string(),
            status: LinkStatus::AutoLinked,
        };

        write_identity_link(&db, &record).await.unwrap();
        write_identity_link(&db, &record).await.unwrap();

        let rows: Vec<serde_json::Value> = db
            .query(
                "SELECT id FROM identity_links \
                 WHERE source_node_id = $src AND target_node_id = $tgt AND source = 'ch_psc'",
            )
            .bind(("src", record.source_node_id.clone()))
            .bind(("tgt", record.target_node_id.clone()))
            .await
            .and_then(|mut r| r.take(0))
            .unwrap_or_default();

        assert_eq!(
            rows.len(),
            1,
            "write_identity_link must be idempotent (ENR-02): expected 1 row, got {}",
            rows.len()
        );
    }
}
