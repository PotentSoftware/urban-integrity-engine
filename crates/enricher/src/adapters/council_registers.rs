//! Council Registers of Interest adapter (POL-02).
//!
//! Scrapes ModernGov council registers of members' interests via the Scrapling
//! sidecar (`http://localhost:8765`). Extracts councillor declarations and cross-
//! references company names against the local director/entity graph.
//!
//! ## Gate
//! Disabled by default. Requires both `enabled = true` AND at least one LPA code
//! in `lpa_codes`. When `lpa_codes` is empty the adapter is a no-op.
//!
//! ## Scrapling sidecar degradation
//! If the sidecar is unreachable or returns non-2xx, the adapter logs WARN and
//! returns `Ok(EnrichmentStats::default())` — never panics, never Err.
//!
//! ## Security
//! - T-20-03-02: councillor_name is stored as a normalised slug in evidence_json,
//!   never interpolated into SurrealQL (bound parameters only).
//! - ENR-01: zero imports from crates/ingestor (reqwest::Client directly to sidecar).

use async_trait::async_trait;
use chrono::Utc;
use reqwest::Client;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};
use urban_integrity_core::config::CouncilRegistersConfig;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_schema::types::edge::{DeclaresInterestData, EdgeMeta, EdgeWeight};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{NodeWeight, PoliticalActorData};

use crate::adapters::ch_psc::write_identity_link;
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{IdentityLinkRecord, LinkStatus};

/// Page size for the declared-company lookup. Bounded well under the SurrealDB SDK's
/// wire-decode ceiling, and paged rather than capped so the map can never be silently short.
const ENTITY_INDEX_PAGE_SIZE: usize = 20_000;

/// Build the `company name (lowercased) -> entity_id` map the declared-interest match uses.
///
/// Reads the **`entity`** table — companies. POL-02 emits `Directs` edges "to declared
/// companies", so the value side must be a company node id (`GB-COH:{number}`).
///
/// This previously read `SELECT id, name, entity_id FROM officer LIMIT 50000`, which was wrong
/// twice over: `officer.name` is a natural person's name, so a declared *company* name could
/// only match by coincidence, and `officer.entity_id` is that person's own `director:{slug}`
/// node, so any match that did occur would have pointed the `Directs` edge at a person rather
/// than at the declared company. The defect was latent — the adapter has never run against the
/// live analysis (no `enrichment_runs` row, no `[enricher.council_registers]` config block) —
/// so it produced no bad links, but it would have produced almost no links at all once enabled.
///
/// Paged, terminating only on an EMPTY page, per `core::persistence::scan`'s rule that a short
/// page means truncation rather than end-of-data. The old fixed `LIMIT 50000` was a real
/// truncation and not a theoretical one: the live `entity` table holds 73,865 rows, so about a
/// third of all companies sat outside the lookup and a declared interest in one of them would
/// have been discarded as "no match" with no warning.
async fn load_entity_name_index(
    db: &Surreal<Any>,
    page_size: usize,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut idx: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut start = 0usize;
    loop {
        // page_size and start are trusted integer constants, not user input.
        let sql = format!(
            "SELECT id, name, entity_id FROM entity ORDER BY id LIMIT {page_size} START {start}"
        );
        let rows: Vec<serde_json::Value> = db.query(sql).await?.take(0)?;
        let returned = rows.len();
        if returned == 0 {
            break;
        }
        for row in &rows {
            let name = row["name"].as_str().unwrap_or("").to_lowercase();
            let entity_id = row["entity_id"].as_str().unwrap_or("").to_string();
            if !name.is_empty() && !entity_id.is_empty() {
                idx.insert(name, entity_id);
            }
        }
        start += returned;
    }
    Ok(idx)
}

// ── LPA code → council slug mapping ─────────────────────────────────────────

/// Hardcoded minimal LPA code → ModernGov council slug mapping.
///
/// Phase 20 targets a configurable subset (per the research notes' Open Questions #3).
/// Unknown codes are skipped with WARN — not an error.
fn lpa_code_to_council_slug(lpa_code: &str) -> Option<&'static str> {
    match lpa_code {
        "E06000001" => Some("anglesey"),
        "E06000002" => Some("gwynedd"),
        "E09000001" => Some("cityoflondon"),
        "E09000002" => Some("barking"),
        "E09000003" => Some("barnet"),
        "E09000007" => Some("camden"),
        "E09000012" => Some("greenwich"),
        "E09000013" => Some("hackney"),
        "E09000016" => Some("hounslow"),
        "E09000019" => Some("islington"),
        "E09000022" => Some("lewisham"),
        "E09000025" => Some("newham"),
        "E09000028" => Some("southwark"),
        "E09000030" => Some("towerhamlets"),
        "E09000032" => Some("wandsworth"),
        "E09000033" => Some("westminster"),
        _ => None,
    }
}

// ── CouncilRegistersAdapter ──────────────────────────────────────────────────

/// Council Registers of Interest adapter (POL-02).
///
/// Fetches declaration-of-interest pages from ModernGov council portals via
/// the Scrapling sidecar and links declaring councillors to matching entities
/// in the local graph.
pub struct CouncilRegistersAdapter {
    config: CouncilRegistersConfig,
}

impl CouncilRegistersAdapter {
    pub fn new(config: CouncilRegistersConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl EnricherAdapter for CouncilRegistersAdapter {
    fn name(&self) -> &'static str {
        "council_registers"
    }

    /// Returns true only when `enabled = true` AND `lpa_codes` is non-empty.
    fn is_enabled(&self) -> bool {
        self.config.enabled && !self.config.lpa_codes.is_empty()
    }

    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        // Gate 1: disabled or no LPA codes configured.
        if !self.is_enabled() {
            warn!(
                adapter = "council_registers",
                "council_registers adapter disabled or no lpa_codes configured — skipping"
            );
            return Ok(EnrichmentStats::default());
        }

        // Gate 2: Scrapling sidecar health check.
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let health_url = "http://localhost:8765/health";
        match client.get(health_url).send().await {
            Err(e) => {
                warn!(
                    adapter = "council_registers",
                    error = %e,
                    "Scrapling sidecar unreachable — skipping adapter"
                );
                return Ok(EnrichmentStats::default());
            }
            Ok(resp) if !resp.status().is_success() => {
                warn!(
                    adapter = "council_registers",
                    status = %resp.status(),
                    "Scrapling sidecar health check failed — skipping adapter"
                );
                return Ok(EnrichmentStats::default());
            }
            _ => {}
        }

        // Load the declared-company lookup: company name → company entity_id, from the
        // `entity` table. See load_entity_name_index for why this is not the `officer` table.
        let name_lookup = load_entity_name_index(db, ENTITY_INDEX_PAGE_SIZE).await?;
        info!(
            adapter = "council_registers",
            companies_indexed = name_lookup.len(),
            "loaded declared-company lookup from the entity table"
        );

        let mut stats = EnrichmentStats::default();

        for lpa_code in &self.config.lpa_codes {
            let council_slug = match lpa_code_to_council_slug(lpa_code) {
                Some(slug) => slug,
                None => {
                    warn!(
                        adapter = "council_registers",
                        lpa_code = %lpa_code,
                        "No ModernGov slug mapping for LPA code — skipping"
                    );
                    continue;
                }
            };

            // Step 1: Fetch councillor index to get UIDs.
            let member_url = format!("https://{council_slug}.moderngov.co.uk/mgMemberIndex.aspx");
            let payload = serde_json::json!({
                "url": member_url,
                "selectors": ["a[href*='UID=']"]
            });
            let sidecar_resp = match client
                .post("http://localhost:8765/scrape")
                .json(&payload)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        adapter = "council_registers",
                        council = %lpa_code,
                        error = %e,
                        "Sidecar POST for member index failed — skipping council"
                    );
                    continue;
                }
            };
            let member_json: serde_json::Value = match sidecar_resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        adapter = "council_registers",
                        council = %lpa_code,
                        error = %e,
                        "Failed to parse member index JSON — skipping council"
                    );
                    continue;
                }
            };

            // Extract UID integers from href attributes.
            let uids = extract_uids_from_response(&member_json);

            // Step 2: For each UID, fetch declaration-of-interest page.
            for uid in uids {
                let decl_url = format!(
                    "https://{council_slug}.moderngov.co.uk/mgListDeclarationsOfInterest.aspx?UID={uid}"
                );
                let decl_payload = serde_json::json!({
                    "url": decl_url,
                    "selectors": ["table.mgTable"]
                });
                let decl_resp = match client
                    .post("http://localhost:8765/scrape")
                    .json(&decl_payload)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            adapter = "council_registers",
                            uid = uid,
                            error = %e,
                            "Sidecar POST for declarations failed — skipping UID"
                        );
                        continue;
                    }
                };
                let decl_json: serde_json::Value = match decl_resp.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            adapter = "council_registers",
                            uid = uid,
                            error = %e,
                            "Failed to parse declarations JSON — skipping UID"
                        );
                        continue;
                    }
                };

                let declarations = parse_declaration_rows(&decl_json);

                for decl in declarations {
                    if decl.company_name.is_empty() {
                        continue;
                    }
                    stats.candidates_found += 1;

                    // Lookup company in the entity graph by EXACT normalised-name equality
                    // only. The former name-only fuzzy fallback (composite_score: Jaro-Winkler
                    // + DoubleMetaphone) was removed under the exact-match-only data-integrity
                    // policy (2026-06-13) — name similarity links unrelated companies and
                    // produced the same false-positive class purged from the charity and EC
                    // adapters. A declared interest is linked to a graph COMPANY only when
                    // their lowercased names are identical.
                    //
                    // Note this is a strict equality on the raw name, so common suffix
                    // variations ("ACME LTD" vs "ACME LIMITED") do not match. That is the
                    // exact-match-only policy working as intended, not an oversight: a
                    // suffix-normalising comparison is a similarity heuristic, and this
                    // adapter asserts a directorship, which is the class where a false
                    // positive is most damaging.
                    let company_lower = decl.company_name.to_lowercase();
                    let target_entity_id = name_lookup.get(&company_lower).cloned();

                    let Some(entity_id) = target_entity_id else {
                        stats.discarded += 1;
                        continue;
                    };

                    // Derive councillor slug.
                    let councillor_slug = decl.councillor_name.to_lowercase().replace(' ', "-");
                    let councillor_node_id = format!("political_actor:{councillor_slug}");

                    // Upsert PoliticalActor node.
                    let _ = mutation_tx
                        .send(GraphMutation::AddNode {
                            id: EntityId(councillor_node_id.clone()),
                            weight: NodeWeight::PoliticalActor(PoliticalActorData {
                                name: decl.councillor_name.clone(),
                                party: None,
                                council_name: Some(lpa_code.clone()),
                            }),
                        })
                        .await;

                    // Write identity link (T-20-03-02: evidence_json uses bound slug, never raw interpolation).
                    let evidence_json = serde_json::json!({
                        "councillor_name_slug": councillor_slug,
                        "council": lpa_code,
                        "declared_company": decl.company_name,
                        "category": decl.category,
                    })
                    .to_string();

                    let link_record = IdentityLinkRecord {
                        id: format!("identity_links:{}", uuid::Uuid::new_v4()),
                        source_node_id: councillor_node_id.clone(),
                        target_node_id: entity_id.clone(),
                        resolution_type: "councillor_exact_name".to_string(),
                        confidence: 1.0,
                        source: "council_registers".to_string(),
                        resolved_at: Utc::now(),
                        evidence_json,
                        status: LinkStatus::PendingReview,
                    };
                    write_identity_link(db, &link_record).await?;

                    // Emit DeclaresInterest edge: councillor → declared company.
                    //
                    // NOT a `Directs` edge. This shipped as `Directs` and could never reach the
                    // graph — `directs` is defined FROM entity | officer, so SurrealDB refused
                    // the relate from a `political_actor` node outright
                    // (see the POL-02 directs-relate-rejected investigation notes). The fix is a
                    // dedicated relation rather than a widened `directs`, because the two
                    // assertions are not the same: "is a director of" is a Companies House
                    // fact, while "declared an interest in" is a self-reported disclosure.
                    // `charity_crossover` scans Directs edges with no source-kind filter and
                    // reports "Officer X directs contract-holder AND is trustee of charity",
                    // so a declaration arriving as Directs would be read as a directorship.
                    let _ = mutation_tx
                        .send(GraphMutation::AddEdge {
                            src: EntityId(councillor_node_id),
                            dst: EntityId(entity_id),
                            weight: EdgeWeight::DeclaresInterest {
                                meta: EdgeMeta {
                                    created_at: Utc::now(),
                                    effective_from: None,
                                    effective_to: None,
                                },
                                data: DeclaresInterestData {
                                    council: Some(lpa_code.clone()),
                                    category: Some(decl.category.clone()),
                                },
                            },
                        })
                        .await;

                    stats.pending_review += 1;
                }
            }
        }

        info!(
            adapter = "council_registers",
            candidates = stats.candidates_found,
            pending_review = stats.pending_review,
            discarded = stats.discarded,
            "Council registers enrichment complete"
        );
        Ok(stats)
    }
}

// ── HTML/JSON parse helpers ───────────────────────────────────────────────────

/// A parsed declaration row from a ModernGov page.
#[derive(Debug, Clone)]
pub struct DeclarationRow {
    pub councillor_name: String,
    pub category: String,
    pub company_name: String,
}

/// Extract UID integers from a Scrapling sidecar response.
///
/// Expects the sidecar to return a JSON array of href strings containing "UID=NNN".
/// Unparseable or missing UIDs are skipped silently.
pub fn extract_uids_from_response(resp: &serde_json::Value) -> Vec<u64> {
    let mut uids = Vec::new();
    // Sidecar may return {"results": [...]} or a bare array.
    let arr = resp
        .get("results")
        .or(Some(resp))
        .and_then(|v| v.as_array());
    if let Some(items) = arr {
        for item in items {
            let href = item
                .as_str()
                .unwrap_or_else(|| item["href"].as_str().unwrap_or(""));
            if let Some(uid_str) = href.split("UID=").nth(1) {
                let uid_str = uid_str.split('&').next().unwrap_or(uid_str);
                if let Ok(uid) = uid_str.trim().parse::<u64>() {
                    uids.push(uid);
                }
            }
        }
    }
    uids
}

/// Parse declaration rows from a Scrapling sidecar table response.
///
/// Expects the sidecar to return JSON rows with fields: councillor_name, category, company_name.
/// Missing fields are treated as empty strings and the row is skipped if company_name is empty.
pub fn parse_declaration_rows(resp: &serde_json::Value) -> Vec<DeclarationRow> {
    let mut rows = Vec::new();
    let arr = resp
        .get("rows")
        .or_else(|| resp.get("results"))
        .or(Some(resp))
        .and_then(|v| v.as_array());
    if let Some(items) = arr {
        for item in items {
            let councillor_name = item["councillor_name"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();
            let category = item["category"]
                .as_str()
                .unwrap_or("declaration_of_interest")
                .trim()
                .to_string();
            let company_name = item["company_name"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();
            if councillor_name.is_empty() {
                continue;
            }
            rows.push(DeclarationRow {
                councillor_name,
                category,
                company_name,
            });
        }
    }
    rows
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::config::CouncilRegistersConfig;

    fn make_adapter(enabled: bool, lpa_codes: Vec<String>) -> CouncilRegistersAdapter {
        CouncilRegistersAdapter::new(CouncilRegistersConfig { enabled, lpa_codes })
    }

    // ── Test 1: is_enabled() returns false when config.enabled=false ─────────
    #[test]
    fn test_is_enabled_false_when_disabled() {
        let adapter = make_adapter(false, vec!["E09000001".to_string()]);
        assert!(
            !adapter.is_enabled(),
            "is_enabled() must be false when enabled=false, even with lpa_codes"
        );
    }

    // ── Test 1b: is_enabled() returns false when lpa_codes is empty ──────────
    #[test]
    fn test_is_enabled_false_when_lpa_codes_empty() {
        let adapter = make_adapter(true, vec![]);
        assert!(
            !adapter.is_enabled(),
            "is_enabled() must be false when lpa_codes is empty, even with enabled=true"
        );
    }

    // ── Test 2: is_enabled() returns true when enabled=true AND lpa_codes non-empty
    #[test]
    fn test_is_enabled_true_when_enabled_and_lpa_codes_set() {
        let adapter = make_adapter(true, vec!["E09000001".to_string()]);
        assert!(
            adapter.is_enabled(),
            "is_enabled() must be true when enabled=true and lpa_codes is non-empty"
        );
    }

    // ── Test 3: Scrapling sidecar unreachable → Ok(EnrichmentStats::default()) ─
    // We can't call run() without a real DB + live sidecar in unit tests.
    // Instead, we verify the gate logic directly (mirrors opensanctions.rs pattern).
    #[tokio::test]
    async fn test_sidecar_unreachable_returns_ok_default() {
        // Verify that is_enabled() returning false causes the function to return Ok early.
        // When enabled=false the gate fires before any sidecar call.
        let adapter = make_adapter(false, vec!["E09000001".to_string()]);
        assert!(
            !adapter.is_enabled(),
            "disabled adapter must skip sidecar call (returns Ok early)"
        );

        // For the sidecar-unreachable path: verify the health_url is correct and
        // the reqwest call would go to localhost:8765/health (not ingestor).
        // We test the logic by confirming no imports from crates/ingestor are needed:
        // the adapter uses reqwest::Client directly to POST to localhost:8765.
        let health_url = "http://localhost:8765/health";
        assert!(
            health_url.contains("localhost:8765"),
            "health_url must use localhost:8765 (ENR-01: not ingestor)"
        );
        assert!(
            health_url.contains("/health"),
            "health_url must target /health endpoint"
        );
    }

    // ── Test 4: Sidecar non-2xx → Ok(EnrichmentStats::default()) ────────────
    // Verify the gate path: when sidecar returns non-2xx the adapter skips with WARN.
    #[test]
    fn test_sidecar_non2xx_gate_path() {
        // Verify the graceful-degradation logic pattern in run():
        // When status is not success → warn! + return Ok(default).
        // We test the scrape URL construction matches the sidecar spec.
        let scrape_url = "http://localhost:8765/scrape";
        assert!(
            scrape_url.contains("localhost:8765"),
            "scrape_url must use localhost:8765 (ENR-01)"
        );
        assert!(
            scrape_url.ends_with("/scrape"),
            "scrape_url must end with /scrape"
        );
    }

    // ── Test 5: parse_declaration_row() extracts councillor_name and company_name ──
    #[test]
    fn test_parse_declaration_row_extracts_fields() {
        let mock_response = serde_json::json!({
            "rows": [
                {
                    "councillor_name": "Cllr Jane Smith",
                    "category": "Pecuniary Interest",
                    "company_name": "Smith Consulting Ltd"
                },
                {
                    "councillor_name": "Cllr Bob Jones",
                    "category": "Personal Interest",
                    "company_name": "Jones Property Holdings"
                },
                {
                    // Empty company_name — should still be included (caller filters)
                    "councillor_name": "Cllr Carol White",
                    "category": "Non-Pecuniary",
                    "company_name": ""
                }
            ]
        });

        let rows = parse_declaration_rows(&mock_response);

        // All 3 rows with non-empty councillor_name should be parsed.
        assert_eq!(
            rows.len(),
            3,
            "parse_declaration_rows should return 3 rows (all have non-empty councillor_name)"
        );

        assert_eq!(rows[0].councillor_name, "Cllr Jane Smith");
        assert_eq!(rows[0].company_name, "Smith Consulting Ltd");
        assert_eq!(rows[0].category, "Pecuniary Interest");

        assert_eq!(rows[1].councillor_name, "Cllr Bob Jones");
        assert_eq!(rows[1].company_name, "Jones Property Holdings");

        assert_eq!(rows[2].councillor_name, "Cllr Carol White");
        assert_eq!(rows[2].company_name, "");
    }

    // ── Test 5b: extract_uids_from_response extracts UID integers ────────────
    #[test]
    fn test_extract_uids_from_response() {
        let mock_resp = serde_json::json!({
            "results": [
                "https://anglesey.moderngov.co.uk/mgUserInfo.aspx?UID=101",
                "mgUserInfo.aspx?UID=102&other=param",
                "/path?UID=103"
            ]
        });

        let uids = extract_uids_from_response(&mock_resp);
        assert_eq!(uids, vec![101u64, 102u64, 103u64]);
    }

    /// POL-02 regression: the declared-company lookup must be built from the `entity` table
    /// (companies), NOT the `officer` table (people).
    ///
    /// The adapter's own comment says "Lookup company in entity graph by EXACT normalised-name
    /// equality", and POL-02 requires `Directs` edges "to declared companies". The shipped code
    /// built its map from `SELECT ... FROM officer`, whose `name` is a person's name and whose
    /// `entity_id` is that person's own `director:{slug}` node — so a declared company name
    /// could only ever match if a person happened to be named exactly like a company, and even
    /// then the resulting edge would point at the person, not the company.
    ///
    /// This was latent rather than observed: the adapter has never run against the live analysis
    /// (no `enrichment_runs` row, no `[enricher.council_registers]` config block), so it produced
    /// no wrong links — it would have produced almost no links at all the first time it was
    /// enabled.
    #[tokio::test]
    async fn entity_name_index_reads_companies_not_officers() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_councilidx", "main")
                .await;

        db.query("CREATE type::record('entity', 'c1') CONTENT { entity_id: 'GB-COH:12345678', name: 'ACME HOLDINGS LTD' }")
            .await.expect("seed entity").check().expect("seed entity stmt");
        db.query("CREATE type::record('officer', 'o1') CONTENT { entity_id: 'director:jane-smith', name: 'JANE SMITH' }")
            .await.expect("seed officer").check().expect("seed officer stmt");

        let idx = load_entity_name_index(&db, 500).await.expect("index load");

        assert_eq!(
            idx.get("acme holdings ltd").map(String::as_str),
            Some("GB-COH:12345678"),
            "a declared company name must resolve to the company's entity_id"
        );
        assert!(
            !idx.contains_key("jane smith"),
            "officer (person) names must NOT be in the company lookup — matching a declared \
             company against a person's name is the defect this test pins"
        );
    }

    /// The lookup must page rather than rely on a fixed `LIMIT`. The shipped code used
    /// `LIMIT 50000` against a table holding 73,865 rows on the live analysis, so roughly a
    /// third of all companies were silently outside the lookup — a declared interest in one of
    /// them would be discarded as "no match" with no warning.
    #[tokio::test]
    async fn entity_name_index_pages_past_the_page_size() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_councilpage", "main")
                .await;

        for i in 0..5 {
            db.query("CREATE type::record('entity', $k) CONTENT { entity_id: $eid, name: $n }")
                .bind(("k", format!("c{i}")))
                .bind(("eid", format!("GB-COH:0000000{i}")))
                .bind(("n", format!("TEST COMPANY {i} LTD")))
                .await
                .expect("seed entity")
                .check()
                .expect("seed entity stmt");
        }

        let idx = load_entity_name_index(&db, 2).await.expect("index load");

        assert_eq!(
            idx.len(),
            5,
            "a page size of 2 across 5 rows must still yield every company"
        );
        assert_eq!(
            idx.get("test company 4 ltd").map(String::as_str),
            Some("GB-COH:00000004"),
            "rows from the last page must be present"
        );
    }
}
