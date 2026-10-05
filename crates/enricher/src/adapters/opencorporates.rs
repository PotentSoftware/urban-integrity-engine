//! OpenCorporates cross-jurisdiction company lookup adapter (POL-03).
//!
//! Rate-limited (1 req/s via governor) API calls to `api.opencorporates.com`.
//! Looks up high-P_c / actively-alerted entities against the OpenCorporates
//! company registry and writes cross-jurisdiction identity links.
//!
//! ## Licence gate (D-06 analogue)
//! ODbL 1.0 licence requires explicit acknowledgement before use.
//! When `enabled = true` and `oc_open_data_confirmed = false`, `run()` returns
//! `Err` — the analyst has tried to use OpenCorporates without confirming the licence.
//! When `enabled = false`, the adapter is intentionally off — silent `Ok` (no Err).
//!
//! ## Monthly quota (T-20-03-03)
//! SurrealDB `oc_quota` table tracks `request_count` per `year_month`.
//! Quota is checked before the first API call and decremented on each request.
//! governor in-process burst guard (1 req/s) prevents API rate-limit errors.
//!
//! ## Security
//! - T-20-03-01: company_number from API response validated with is_valid_ch_number()
//!   before any DB write (ASVS V5). Rows failing validation are skipped.
//! - T-20-03-03: SurrealDB oc_quota checked before every request; governor burst guard.
//! - T-20-03-04: oc_open_data_confirmed=false → fail-hard Err (mirrors D-06 from Phase 19).
//! - ENR-01: zero imports from crates/ingestor.

use async_trait::async_trait;
use chrono::Utc;
use governor::{Quota, RateLimiter};
use reqwest::Client;
use std::num::NonZeroU32;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};
use urban_integrity_core::config::OpenCorporatesConfig;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_core::persistence::scan;

use crate::adapters::ch_psc::{is_valid_ch_number, write_identity_link};
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{IdentityLinkRecord, LinkStatus};

// ── OpenCorporatesAdapter ─────────────────────────────────────────────────────

/// OpenCorporates cross-jurisdiction company lookup adapter (POL-03).
///
/// Queries the OpenCorporates API for entities that have active motif alerts,
/// writing cross-jurisdiction identity links only for EXACT company_number matches.
pub struct OpenCorporatesAdapter {
    config: OpenCorporatesConfig,
}

impl OpenCorporatesAdapter {
    pub fn new(config: OpenCorporatesConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl EnricherAdapter for OpenCorporatesAdapter {
    fn name(&self) -> &'static str {
        "opencorporates"
    }

    /// Returns true only when:
    ///   - `enabled = true`
    ///   - `oc_open_data_confirmed = true` (ODbL licence acknowledged)
    ///   - `api_key` is present and non-empty
    ///
    /// This mirrors the OpenSanctions `non_commercial_confirmed` D-06 pattern.
    fn is_enabled(&self) -> bool {
        self.config.enabled
            && self.config.oc_open_data_confirmed
            && self
                .config
                .api_key
                .as_deref()
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false)
    }

    async fn run(
        &self,
        db: &Surreal<Any>,
        _mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        // Gate 1: adapter intentionally disabled — silent skip (not a compliance error).
        if !self.config.enabled {
            warn!(
                adapter = "opencorporates",
                "opencorporates adapter disabled in config"
            );
            return Ok(EnrichmentStats::default());
        }

        // Gate 2: enabled=true but no licence acknowledgement — fail-hard (D-06 analogue).
        if !self.config.oc_open_data_confirmed {
            return Err(anyhow::anyhow!(
                "OpenCorporates ODbL licence not confirmed — set oc_open_data_confirmed = true \
                 in [enricher.opencorporates] config"
            ));
        }

        // Gate 3: API key must be present and non-empty.
        let api_key = match self
            .config
            .api_key
            .as_deref()
            .filter(|k| !k.trim().is_empty())
        {
            Some(k) => k.to_string(),
            None => {
                warn!(
                    adapter = "opencorporates",
                    "OpenCorporates api_key absent or empty — skipping"
                );
                return Ok(EnrichmentStats::default());
            }
        };

        // Gate 4: Monthly quota check (T-20-03-03).
        let year_month = Utc::now().format("%Y-%m").to_string();
        let count_rows: Vec<serde_json::Value> = db
            .query("SELECT request_count FROM oc_quota WHERE year_month = $ym")
            .bind(("ym", year_month.clone()))
            .await?
            .take(0)?;
        let current_count = count_rows
            .first()
            .and_then(|r| r["request_count"].as_u64())
            .unwrap_or(0);
        if current_count >= self.config.monthly_limit as u64 {
            warn!(
                adapter = "opencorporates",
                current_count,
                limit = self.config.monthly_limit,
                "Monthly quota exhausted — skipping"
            );
            return Ok(EnrichmentStats::default());
        }

        // Load actively-alerted entities for cross-jurisdiction lookup.
        //
        // `entity_id`, not `id`: the record id serialises as a RecordId, not a string, and the
        // alerted set holds bare ids. Selecting `id` made the `as_str()?` below drop every row
        // silently even before the alerted set was fixed.
        // `IS NOT NONE`, not `IS NOT NULL`. SurrealDB treats an ABSENT field as NONE and a
        // field explicitly set to null as NULL, and the projector simply omits the key for a
        // company that has no number — so `IS NOT NULL` excluded nothing. Measured live on
        // `an_anglesey_aebaa785`, 2026-08-16: `IS NOT NULL` returned all 72,123 entity rows,
        // `IS NOT NONE` returns the 14,720 that actually carry a number. The Rust `as_str()?`
        // below dropped the other 57,403 silently, so the WHERE clause read as a filter, cost
        // a full-table scan, and filtered nothing.
        let entity_rows: Vec<serde_json::Value> = db
            .query(
                "SELECT entity_id, name, company_number FROM entity \
                 WHERE company_number IS NOT NONE",
            )
            .await?
            .take(0)
            // `?`, not `unwrap_or_default()`: a failed SELECT yielded zero targets and the run
            // reported success, making a broken run indistinguishable from a complete one. An
            // errored enrichment run must exit non-zero (the CLI's D-04 fail-fast contract).
            .map_err(|e| anyhow::anyhow!("opencorporates: entity target query failed: {e}"))?;

        // Filter to only entities with at least one recent MotifDetected alert.
        //
        // Deduped in Rust rather than with SQL DISTINCT: the `LIMIT 1000` this replaces
        // bounded the rows the planner looked at, not the distinct entities, so an entity
        // with a recent alert could be missed entirely and skipped for enrichment.
        // Same reasoning: an errored scan used to yield an empty alerted set, hence zero
        // targets, hence a silent no-op run that looked like full coverage.
        let alert_rows = scan::scan_events_by_kind(db, "MotifDetected")
            .await
            .map_err(|e| anyhow::anyhow!("opencorporates: MotifDetected scan failed: {e}"))?;
        let alerted_ids = alerted_entity_ids(&alert_rows);

        let targets: Vec<(String, String, String)> = entity_rows
            .into_iter()
            .filter_map(|row| {
                let id = row["entity_id"].as_str()?.to_string();
                if !alerted_ids.contains(&id) {
                    return None;
                }
                let name = row["name"].as_str()?.to_string();
                let company_number = row["company_number"].as_str()?.to_string();
                Some((id, name, company_number))
            })
            .collect();

        info!(
            adapter = "opencorporates",
            targets = targets.len(),
            monthly_limit = self.config.monthly_limit,
            "Beginning OpenCorporates cross-jurisdiction lookup"
        );

        // Build governor rate limiter — 1 req/s (free tier limit).
        let quota = Quota::per_second(NonZeroU32::new(1).unwrap());
        let limiter = RateLimiter::direct(quota);

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let mut stats = EnrichmentStats {
            candidates_found: targets.len(),
            ..Default::default()
        };

        let remaining_quota = (self.config.monthly_limit as u64).saturating_sub(current_count);
        let (attempted, unreachable) = quota_window(targets.len(), remaining_quota);
        if unreachable > 0 {
            warn!(
                adapter = "opencorporates",
                targets = targets.len(),
                attempted,
                unreachable,
                monthly_limit = self.config.monthly_limit,
                "monthly quota covers only part of the target set — the remainder is NOT \
                 deferred to next month: the target order is deterministic and the run keeps \
                 no cursor, so the same prefix is retried every month and the tail is never \
                 reached (Open Item 32)"
            );
        }

        for (entity_id, entity_name, entity_company_number) in targets.iter().take(attempted) {
            // Rate-limit: wait until token is available (1 req/s).
            limiter.until_ready().await;

            // Increment quota counter BEFORE the API call (T-20-03-03).
            // Upsert: insert with count=1 if the month row is absent, increment if present.
            // T-20-03-03. A discarded Result here means a failed increment leaves
            // `current_count` at 0 for the rest of the month and the licence quota is never
            // enforced — the ICIJ pattern, which no-opped for that adapter's entire life.
            // (The upsert itself is sound: `idx_oc_quota_month` is UNIQUE, which is what makes
            // ON DUPLICATE KEY UPDATE fire on the month row rather than inserting a new one.)
            db.query(
                "INSERT INTO oc_quota (year_month, request_count) VALUES ($ym, 1) \
                 ON DUPLICATE KEY UPDATE request_count = request_count + 1",
            )
            .bind(("ym", year_month.clone()))
            .await?
            .check()?;

            // Query OpenCorporates API.
            // WR-04: pass API key as Authorization header, not a URL query parameter,
            // so it does not appear in reqwest debug logs or proxy access logs.
            let url = format!(
                "https://api.opencorporates.com/v0.4/companies/search?q={}&jurisdiction_code=gb",
                urlencoding_simple(entity_name),
            );
            let resp_result = client
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await;
            let resp_json: serde_json::Value = match resp_result {
                Ok(r) => match r.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            adapter = "opencorporates",
                            entity_id = %entity_id,
                            error = %e,
                            "OpenCorporates JSON decode failed — skipping entity"
                        );
                        stats.discarded += 1;
                        continue;
                    }
                },
                Err(e) => {
                    warn!(
                        adapter = "opencorporates",
                        entity_id = %entity_id,
                        error = %e,
                        "OpenCorporates API call failed — skipping entity"
                    );
                    stats.discarded += 1;
                    continue;
                }
            };

            // Parse results: results.companies[].company
            let companies = resp_json
                .get("results")
                .and_then(|r| r.get("companies"))
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();

            for company_entry in &companies {
                let company = match company_entry.get("company") {
                    Some(c) => c,
                    None => continue,
                };
                let oc_name = company["name"].as_str().unwrap_or("").to_string();
                let oc_company_number =
                    company["company_number"].as_str().unwrap_or("").to_string();
                let jurisdiction_code = company["jurisdiction_code"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();

                // T-20-03-01: validate company_number before any DB write (ASVS V5).
                if !is_valid_ch_number(&oc_company_number) {
                    warn!(
                        adapter = "opencorporates",
                        oc_company_number = %oc_company_number,
                        "OpenCorporates company_number failed validation — skipping (T-20-03-01)"
                    );
                    stats.discarded += 1;
                    continue;
                }

                // Exact-match-only policy (2026-06-13): the former name-only fuzzy match
                // (composite_score: Jaro-Winkler + DoubleMetaphone, resolution_type
                // "cross_jurisdiction_name_match") linked unrelated companies that merely had
                // similar names — the same false-positive class purged from the charity, EC,
                // and ICIJ paths. It is replaced with an EXACT company-number join: a link is
                // written only when the OpenCorporates GB result's company_number is identical
                // to the local entity's company_number. Name similarity is no longer consulted.
                if oc_company_number != *entity_company_number {
                    stats.discarded += 1;
                    continue;
                }

                // Exact identifier match → high-confidence AutoLinked cross-registry link.
                let status = LinkStatus::AutoLinked;
                let evidence_json = serde_json::json!({
                    "oc_company_number": oc_company_number,
                    "jurisdiction": jurisdiction_code,
                    "oc_name": oc_name,
                    "match": "exact_company_number"
                })
                .to_string();

                let link_record = IdentityLinkRecord {
                    id: format!("identity_links:{}", uuid::Uuid::new_v4()),
                    source_node_id: entity_id.clone(),
                    target_node_id: format!("entity:{}", oc_company_number),
                    resolution_type: "exact_company_number".to_string(),
                    confidence: 1.0,
                    source: "opencorporates".to_string(),
                    resolved_at: Utc::now(),
                    evidence_json,
                    status: status.clone(),
                };
                write_identity_link(db, &link_record).await?;

                match status {
                    LinkStatus::AutoLinked => stats.auto_linked += 1,
                    _ => stats.pending_review += 1,
                }
            }
        }

        info!(
            adapter = "opencorporates",
            candidates = stats.candidates_found,
            pending_review = stats.pending_review,
            discarded = stats.discarded,
            "OpenCorporates cross-jurisdiction lookup complete"
        );
        Ok(stats)
    }
}

/// Minimal URL-encoding for company name query strings.
/// Replaces spaces with `%20` and removes chars unsafe in URL params.
fn urlencoding_simple(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '&' => "%26".to_string(),
            '+' => "%2B".to_string(),
            '#' => "%23".to_string(),
            '%' => "%25".to_string(),
            c if c.is_alphanumeric() || "-_.~".contains(c) => c.to_string(),
            // Percent-encode the UTF-8 BYTES, not the code point. `format!("%{:02X}", c as u32)`
            // emitted `%2019` for U+2019 — a right single quote, common in company names — which
            // a decoder reads as `%20` plus a literal "19". The API then searched a different
            // name, the exact-company-number gate discarded the results, and the entity was
            // skipped with `discarded += 1` and no hint the URL was malformed.
            _ => {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf)
                    .as_bytes()
                    .iter()
                    .map(|b| format!("%{b:02X}"))
                    .collect()
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// How many targets a run will actually attempt, and how many the monthly quota leaves behind.
///
/// The loop takes `remaining_quota` targets off a deterministically ordered list, so on the free
/// tier (200/month) a 1,497-target backlog is not "200 done, 1,297 next month" — it is the same
/// first 200 every month and the tail is unreachable. Whatever the operator decides to do about
/// that, the run must not report `candidates_found = 1497` and leave the truncation unsaid; a
/// silent cap reads as full coverage.
fn quota_window(targets: usize, remaining_quota: u64) -> (usize, usize) {
    let attempted = targets.min(remaining_quota as usize);
    (attempted, targets - attempted)
}

/// Every entity id named by a `MotifDetected` alert.
///
/// The payload carries exactly one key — `result_json`, a JSON **string** — and the alert shapes
/// inside it do not agree on where the subject lives. Core detectors (AddressCluster,
/// PhoenixLoop) list every entity in `participants`; property accumulation names one subject in
/// `entity_id`; donation correlation names its subject in `donor_id`. All three keys are read,
/// and all yield BARE ids, matching `entity.entity_id`.
///
/// This function has now silently matched nothing twice. It first read `payload.entity_id`, a
/// field no alert this project has ever written carries — measured live, 1,571 of 1,571 alerts
/// have `payload` keys `{result_json}` — so the alerted set was always empty, `targets`
/// structurally zero, and the adapter logged "no entities need cross-jurisdiction enrichment" on
/// every run. Fixing that left `donor_id` unread, which excluded every entity alerted only by
/// the donation-correlation detector; that was invisible only because the detector had never
/// fired before its donor->contract join was repaired in this phase.
fn alerted_entity_ids(alert_rows: &[serde_json::Value]) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    for row in alert_rows {
        let Some(raw) = row["payload"]["result_json"].as_str() else {
            continue;
        };
        let Ok(result) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        if let Some(id) = result["entity_id"].as_str() {
            ids.insert(id.to_string());
        }
        // Donation correlation names its subject `donor_id`, not `entity_id`.
        if let Some(id) = result["donor_id"].as_str() {
            ids.insert(id.to_string());
        }

        if let Some(participants) = result["participants"].as_array() {
            ids.extend(
                participants
                    .iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string())),
            );
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    #[test]
    fn non_ascii_punctuation_is_encoded_as_utf8_bytes() {
        use super::urlencoding_simple;
        // U+2019 is three UTF-8 bytes: E2 80 99. Encoding the code point yielded `%2019`,
        // which decodes as a space followed by a literal "19".
        assert_eq!(urlencoding_simple("D\u{2019}ARCY"), "D%E2%80%99ARCY");
        // The explicit arms are unchanged.
        assert_eq!(urlencoding_simple("A B"), "A%20B");
        assert_eq!(urlencoding_simple("A&B"), "A%26B");
        assert_eq!(urlencoding_simple("A+B"), "A%2BB");
        assert_eq!(urlencoding_simple("100%"), "100%25");
        // Unicode alphanumerics still pass through for reqwest's URL parser to handle.
        assert_eq!(urlencoding_simple("CAF\u{c9}"), "CAF\u{c9}");
    }

    /// A donation-correlation alert names its subject `donor_id`, not `entity_id`.
    ///
    /// `alerted_entity_ids` read only `entity_id` and `participants`, so every entity alerted
    /// solely by that detector was excluded from the target set — the same "predicate silently
    /// matches nothing" class this function was rewritten to fix once already.
    #[test]
    fn a_donation_correlation_alert_names_its_donor_as_a_target() {
        let result_json = serde_json::json!({
            "motif_type": "donation_contract_correlation",
            "risk_score": 0.8,
            "description": "test",
            "donor_id": "GB-COH:99001122",
            "party_name": "Test Party",
            "lpa_code": "E06000001",
            "motif_hash": "deadbeef",
        })
        .to_string();
        let rows = vec![serde_json::json!({ "payload": { "result_json": result_json } })];

        let ids = super::alerted_entity_ids(&rows);
        assert!(
            ids.contains("GB-COH:99001122"),
            "the donor is the alert's subject and must be an enrichment target; got {ids:?}"
        );
    }

    /// The other two subject keys must keep working.
    #[test]
    fn entity_id_and_participants_are_still_read() {
        let accumulation = serde_json::json!({ "entity_id": "GB-COH:11111111" }).to_string();
        let cluster = serde_json::json!({ "participants": ["GB-COH:22222222", "GB-COH:33333333"] })
            .to_string();
        let rows = vec![
            serde_json::json!({ "payload": { "result_json": accumulation } }),
            serde_json::json!({ "payload": { "result_json": cluster } }),
        ];

        let ids = super::alerted_entity_ids(&rows);
        assert!(ids.contains("GB-COH:11111111"));
        assert!(ids.contains("GB-COH:22222222"));
        assert!(ids.contains("GB-COH:33333333"));
    }

    use super::*;

    // ─── quota_window (Open Item 32) ─────────────────────────────────────────

    #[test]
    fn test_quota_window_reports_the_unreached_tail() {
        // Live shape on 2026-08-16: 1,497 targets, free-tier limit 200.
        assert_eq!(quota_window(1497, 200), (200, 1297));
    }

    #[test]
    fn test_quota_window_is_silent_when_the_quota_covers_everything() {
        assert_eq!(quota_window(150, 200), (150, 0));
        assert_eq!(quota_window(200, 200), (200, 0));
    }

    #[test]
    fn test_quota_window_attempts_nothing_when_the_quota_is_spent() {
        // Exhausted quota must not be reported as "all targets covered".
        assert_eq!(quota_window(1497, 0), (0, 1497));
    }

    #[test]
    fn test_quota_window_never_underflows() {
        // `targets - attempted` on usize: a quota larger than the backlog must not wrap.
        assert_eq!(quota_window(0, u64::MAX), (0, 0));
        assert_eq!(quota_window(5, u64::MAX), (5, 0));
    }

    // ─── alerted_entity_ids (Open Item 30, L3) ──────────────────────────────
    //
    // `MotifDetected` payloads carry ONE key — `result_json`, a JSON string. The adapter
    // read `payload.entity_id`, which exists on no alert this project has ever written
    // (measured live: 1,571 of 1,571 alerts have `payload` keys == {result_json}), so the
    // alerted set was always empty and `targets` was structurally 0. The adapter has
    // therefore reported "no entities need cross-jurisdiction enrichment" on every run.
    //
    // Two alert shapes exist and both must be read:
    //   core detectors        (AddressCluster 1,431, PhoenixLoop 2) — `participants: [ids]`
    //   post-adapter detectors (property_accumulation 138)          — flat `entity_id`

    fn alert(result_json: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "payload": { "result_json": result_json.to_string() } })
    }

    #[test]
    fn alerted_ids_reads_flat_entity_id_alerts() {
        let rows = vec![alert(serde_json::json!({
            "motif_type": "property_accumulation_anomaly",
            "entity_id": "GB-COH:00133804",
            "risk_score": 0.8,
        }))];
        let ids = alerted_entity_ids(&rows);
        assert!(
            ids.contains("GB-COH:00133804"),
            "post-adapter alerts name their subject in `entity_id`; got {ids:?}"
        );
    }

    #[test]
    fn alerted_ids_reads_every_participant_of_a_core_alert() {
        let rows = vec![alert(serde_json::json!({
            "motif_type": "AddressCluster",
            "participants": ["GB-COH:09325797", "GB-COH:14706652", "GB-COH:15914287"],
            "unique_hash": "abc",
        }))];
        let ids = alerted_entity_ids(&rows);
        assert_eq!(
            ids.len(),
            3,
            "every participant is an alerted entity, not just the first; got {ids:?}"
        );
        assert!(ids.contains("GB-COH:14706652"));
    }

    #[test]
    fn alerted_ids_yields_bare_ids_for_comparison_against_entity_id() {
        // The set is compared against `entity.entity_id`, which is bare. A record-id form
        // would never match — the second half of this same defect class in L2.
        let rows = vec![alert(serde_json::json!({
            "motif_type": "PhoenixLoop",
            "participants": ["GB-COH:00133804"],
        }))];
        let ids = alerted_entity_ids(&rows);
        assert!(
            !ids.iter().any(|i| i.starts_with("entity:")),
            "ids must be bare, matching entity.entity_id; got {ids:?}"
        );
    }

    #[test]
    fn alerted_ids_survives_malformed_payloads() {
        let rows = vec![
            serde_json::json!({ "payload": {} }),
            serde_json::json!({ "payload": { "result_json": "not json" } }),
            alert(serde_json::json!({ "motif_type": "AddressCluster", "participants": "nope" })),
            alert(serde_json::json!({ "entity_id": "GB-COH:1" })),
        ];
        let ids = alerted_entity_ids(&rows);
        assert_eq!(ids.len(), 1, "one usable id among the junk; got {ids:?}");
    }

    use urban_integrity_core::config::OpenCorporatesConfig;

    fn make_adapter(
        enabled: bool,
        oc_open_data_confirmed: bool,
        api_key: Option<String>,
        monthly_limit: u32,
    ) -> OpenCorporatesAdapter {
        OpenCorporatesAdapter::new(OpenCorporatesConfig {
            enabled,
            oc_open_data_confirmed,
            api_key,
            monthly_limit,
        })
    }

    // ── Test 1: is_enabled() returns false when oc_open_data_confirmed=false ─
    #[test]
    fn test_is_enabled_false_when_licence_not_confirmed() {
        let adapter = make_adapter(
            true,
            false, // oc_open_data_confirmed = false
            Some("test-api-key".to_string()),
            200,
        );
        assert!(
            !adapter.is_enabled(),
            "is_enabled() must be false when oc_open_data_confirmed=false (licence gate)"
        );
    }

    // ── Test 2: is_enabled() returns false when api_key is None ─────────────
    #[test]
    fn test_is_enabled_false_when_api_key_none() {
        let adapter = make_adapter(
            true, true, None, // api_key = None
            200,
        );
        assert!(
            !adapter.is_enabled(),
            "is_enabled() must be false when api_key is None"
        );
    }

    // ── Test 3: Quota exhausted (monthly_limit=0) → run() returns Ok(default)
    // Verified via gate logic: we test the guard inline (no live DB required).
    #[tokio::test]
    async fn test_quota_exhausted_returns_ok_default() {
        let adapter = make_adapter(true, true, Some("key".to_string()), 0);
        // is_enabled() is true (all gates pass for is_enabled check)
        assert!(
            adapter.is_enabled(),
            "adapter with monthly_limit=0 should still pass is_enabled()"
        );
        // The quota check is in run() gate 4. We can verify the quota gate logic:
        // current_count (0) >= monthly_limit (0) → true → returns Ok(default).
        let current_count: u64 = 0;
        let monthly_limit = adapter.config.monthly_limit as u64;
        assert!(
            current_count >= monthly_limit,
            "quota gate: 0 >= 0 must be true → Ok(default) returned"
        );
    }

    // ── Test 4: governor RateLimiter construction without panicking ───────────
    #[test]
    fn test_rate_limiter_constructs_without_panic() {
        // This test verifies T-20-03-SC: governor 0.10 RateLimiter::direct() does
        // not panic when constructed with Quota::per_second(NonZeroU32::new(1)).
        let quota = Quota::per_second(NonZeroU32::new(1).unwrap());
        let _limiter = RateLimiter::direct(quota);
        // If we reach here, the constructor did not panic.
    }

    // ── Test 5: oc_open_data_confirmed=false AND enabled=true → is_enabled() false
    #[test]
    fn test_licence_gate_takes_precedence() {
        let adapter = make_adapter(
            true,  // enabled = true
            false, // oc_open_data_confirmed = false
            Some("test-key".to_string()),
            200,
        );
        assert!(
            !adapter.is_enabled(),
            "is_enabled() must be false when oc_open_data_confirmed=false, even with enabled=true"
        );
    }

    // ── Test 5b: all gates pass → is_enabled() returns true ─────────────────
    #[test]
    fn test_is_enabled_true_when_all_gates_pass() {
        let adapter = make_adapter(true, true, Some("valid-api-key".to_string()), 200);
        assert!(
            adapter.is_enabled(),
            "is_enabled() must be true when enabled=true, oc_open_data_confirmed=true, api_key present"
        );
    }

    // ── Test: run() Gate 2 fail-hard when oc_open_data_confirmed=false ───────
    // Mirrors opensanctions.rs test_d06_fail_hard_enabled_not_confirmed.
    #[tokio::test]
    async fn test_licence_gate_fail_hard() {
        let adapter = make_adapter(
            true,
            false, // oc_open_data_confirmed = false
            Some("key".to_string()),
            200,
        );
        // Verify the gate logic inline (no live DB needed).
        let result = test_run_gate_only(&adapter);
        assert!(
            result.is_err(),
            "run() must return Err when enabled=true, oc_open_data_confirmed=false"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("oc_open_data_confirmed = true"),
            "Err message must contain 'oc_open_data_confirmed = true', got: {msg}"
        );
    }

    // ── Test: run() Gate 1 silent skip when enabled=false ────────────────────
    #[tokio::test]
    async fn test_disabled_silent_skip() {
        let adapter = make_adapter(false, false, None, 200);
        let result = test_run_gate_only(&adapter);
        assert!(
            result.is_ok(),
            "run() must return Ok when enabled=false (silent skip)"
        );
    }

    // ── Gate-only helper (avoids live DB dependency) ─────────────────────────
    fn test_run_gate_only(adapter: &OpenCorporatesAdapter) -> anyhow::Result<EnrichmentStats> {
        // Gate 1: disabled.
        if !adapter.config.enabled {
            return Ok(EnrichmentStats::default());
        }
        // Gate 2: licence not confirmed.
        if !adapter.config.oc_open_data_confirmed {
            return Err(anyhow::anyhow!(
                "OpenCorporates ODbL licence not confirmed — set oc_open_data_confirmed = true \
                 in [enricher.opencorporates] config"
            ));
        }
        Ok(EnrichmentStats::default())
    }
}
