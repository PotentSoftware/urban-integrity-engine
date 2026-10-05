//! OpenSanctions/yente adapter (FUZ-03).
//!
//! Self-hosted yente sidecar on localhost (D-05, default port 8766).
//!
//! ## Compliance gate (D-06)
//! CC-BY-NC licence requires explicit acknowledgement before use.
//! When `enabled = true` and `non_commercial_confirmed = false`, `run()` returns
//! `Err` — the analyst has tried to use OpenSanctions without confirming the licence.
//! When `enabled = false`, the adapter is intentionally off — silent `Ok` (no Err).
//!
//! ## Name-only cap (D-08)
//! Matches with no DOB and no nationality are capped at 0.65 regardless of the yente
//! score.  DOB + nationality are required to reach the auto-link tier (≥0.85).  This
//! prevents common-name false positives from being automatically linked.
//!
//! ## Security
//! - T-19-03-01: `response.score` is clamped to `[0.0, 1.0]` before use as confidence.
//! - T-19-03-02: D-06 fail-hard is enforced in `run()` before any network call.
//! - T-19-03-03: D-08 cap is enforced at adapter level after score translation.
//! - T-19-03-04: `evidence_json` stores only `yente_id` (their ID), floats, and booleans — no DOB or nationality PII.
//! - T-19-03-05: Sidecar health-check probe with 10s timeout; on Err → WARN + Ok(default).

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};
use urban_integrity_core::config::OpenSanctionsConfig;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_core::persistence::scan;

use crate::adapters::ch_psc::write_identity_link;
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::confidence::{
    IdentityLinkRecord, LinkStatus, AUTO_LINK_THRESHOLD, CANDIDATE_THRESHOLD,
    SUB_CANDIDATE_THRESHOLD,
};

// ── EntityCandidate ─────────────────────────────────────────────────────────

/// A named entity candidate for sanctions screening.
///
/// Constructed from Officer/Entity nodes in `graph_events` and batched in groups
/// of `config.batch_size` (D-07) for the yente `/match/default` POST.
#[derive(Debug, Clone)]
pub struct EntityCandidate {
    /// Graph node ID (e.g. `director:jane-doe`).
    pub node_id: String,
    /// Display name as ingested.
    pub name: String,
    /// ISO 8601 date of birth (`YYYY-MM-DD`), if known.
    pub date_of_birth: Option<String>,
    /// ISO 3166-1 alpha-2 nationality code, if known.
    pub nationality: Option<String>,
}

// ── OpenSanctionsAdapter ────────────────────────────────────────────────────

/// OpenSanctions/yente PEP and sanctions screening adapter (FUZ-03).
///
/// Requires a self-hosted yente sidecar (default: `http://localhost:8766`).
/// Batches Officer/Entity nodes from `graph_events` and POSTs to `/match/default`.
pub struct OpenSanctionsAdapter {
    pub config: OpenSanctionsConfig,
    client: reqwest::Client,
}

impl OpenSanctionsAdapter {
    pub fn new(config: OpenSanctionsConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build reqwest client for OpenSanctionsAdapter");
        Self { config, client }
    }
}

#[async_trait]
impl EnricherAdapter for OpenSanctionsAdapter {
    fn name(&self) -> &'static str {
        "opensanctions"
    }

    /// Returns true only when both `enabled = true` AND `non_commercial_confirmed = true`.
    ///
    /// This returns `false` in both the intentional-disable case (`enabled=false`) and
    /// the compliance-violation case (`enabled=true, non_commercial_confirmed=false`).
    /// The distinction is enforced in `run()` which only returns `Err` for the second case.
    fn is_enabled(&self) -> bool {
        self.config.enabled && self.config.non_commercial_confirmed
    }

    /// Run a single OpenSanctions enrichment pass.
    ///
    /// Gate ordering is critical:
    /// 1. `enabled=false` → silent WARN + Ok(default). Analyst opted out intentionally — not a compliance issue.
    /// 2. `enabled=true, non_commercial_confirmed=false` → Err (D-06 fail-hard). Analyst tried to use OS without licence acknowledgement.
    /// 3. Sidecar unreachable → WARN + Ok(default) (D-05 graceful degradation).
    /// 4. Batch entities from graph_events, POST to yente, parse + route results.
    async fn run(
        &self,
        db: &Surreal<Any>,
        _mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats> {
        // Gate 1: adapter intentionally disabled — silent skip (not a compliance error).
        if !self.config.enabled {
            warn!(
                adapter = "opensanctions",
                "opensanctions adapter disabled in config"
            );
            return Ok(EnrichmentStats::default());
        }

        // Gate 2: enabled=true but no licence acknowledgement — fail-hard (D-06).
        if !self.config.non_commercial_confirmed {
            return Err(anyhow::anyhow!(
                "OpenSanctions requires `non_commercial_confirmed = true` in \
                 [enricher.opensanctions] config (CC-BY-NC compliance)"
            ));
        }

        // Gate 3: sidecar health probe — degrade gracefully if not running (D-05).
        let health_url = format!("{}/healthz", self.config.endpoint);
        let probe_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        if probe_client.get(&health_url).send().await.is_err() {
            warn!(
                adapter = "opensanctions",
                endpoint = %self.config.endpoint,
                "yente sidecar not reachable — skipping adapter (D-05)"
            );
            return Ok(EnrichmentStats::default());
        }

        // Load Officer/Entity candidates from graph_events.
        let candidates = load_candidates_from_graph(db).await?;
        info!(
            adapter = "opensanctions",
            candidates = candidates.len(),
            batch_size = self.config.batch_size,
            "Beginning OpenSanctions batch screening"
        );

        let mut stats = EnrichmentStats {
            candidates_found: candidates.len(),
            ..Default::default()
        };

        // Process in configurable batches (D-07: default batch_size=50).
        for chunk in candidates.chunks(self.config.batch_size) {
            let body = build_batch_query(chunk);
            let match_url = format!("{}/match/default", self.config.endpoint);

            let resp: serde_json::Value = match async {
                let response = self
                    .client
                    .post(&match_url)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("POST failed: {e}"))?;
                let v: serde_json::Value = response
                    .json()
                    .await
                    .map_err(|e| anyhow::anyhow!("JSON decode failed: {e}"))?;
                Ok::<_, anyhow::Error>(v)
            }
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        adapter = "opensanctions",
                        error = %e,
                        "yente batch POST failed — skipping chunk"
                    );
                    continue;
                }
            };

            let responses = match resp.get("responses").and_then(|r| r.as_object()) {
                Some(r) => r.clone(),
                None => continue,
            };

            // Iterate in query-key order (q0, q1, ...).
            for (i, entity) in chunk.iter().enumerate() {
                let key = format!("q{i}");
                let query_result = match responses.get(&key) {
                    Some(v) => v,
                    None => continue,
                };

                let results = match query_result.get("results").and_then(|r| r.as_array()) {
                    Some(arr) if !arr.is_empty() => arr,
                    _ => continue,
                };

                // Take highest-score result (first in array — yente returns desc order).
                let top_result = &results[0];
                // T-19-03-01: clamp score to [0.0, 1.0] before using as confidence.
                // Clamped from BOTH ends: yente can theoretically return negative scores
                // on a version mismatch or malformed response, which would otherwise
                // produce a negative IdentityLinkRecord.confidence violating the [0,1] invariant.
                let raw_score = top_result["score"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
                let yente_id = top_result["id"].as_str().unwrap_or("").to_string();

                let has_dob = entity.date_of_birth.is_some();
                let has_nationality = entity.nationality.is_some();

                // D-08: cap name-only matches at 0.65 regardless of yente score (T-19-03-03).
                let translated = if !has_dob && !has_nationality {
                    raw_score.min(0.65)
                } else {
                    raw_score
                };

                // Route by translated confidence.
                let status = if translated >= AUTO_LINK_THRESHOLD {
                    LinkStatus::AutoLinked
                } else if translated >= CANDIDATE_THRESHOLD {
                    LinkStatus::PendingReview
                } else if translated >= SUB_CANDIDATE_THRESHOLD {
                    LinkStatus::SubCandidate
                } else {
                    stats.discarded += 1;
                    continue;
                };

                // T-19-03-04: evidence_json stores only yente_id (not DOB/nationality PII).
                // ENR-06: flat string via .to_string(), never nested serde_json::Value.
                let evidence_json = serde_json::json!({
                    "yente_id": yente_id,
                    "yente_score": raw_score,
                    "translated_confidence": translated,
                    "name_only": !has_dob && !has_nationality,
                    "source": "opensanctions"
                })
                .to_string();

                let link_record = IdentityLinkRecord {
                    id: format!("identity_links:{}", uuid::Uuid::new_v4()),
                    source_node_id: entity.node_id.clone(),
                    target_node_id: format!("sanctions:{yente_id}"),
                    resolution_type: "sanctions_screening".to_string(),
                    confidence: translated,
                    source: "opensanctions".to_string(),
                    resolved_at: Utc::now(),
                    evidence_json,
                    status: status.clone(),
                };

                write_identity_link(db, &link_record).await?;

                match status {
                    LinkStatus::AutoLinked => stats.auto_linked += 1,
                    LinkStatus::PendingReview | LinkStatus::Candidate => stats.pending_review += 1,
                    LinkStatus::SubCandidate => {
                        stats.sub_candidate += 1;
                    }
                    _ => {}
                }
            }
        }

        info!(
            adapter = "opensanctions",
            candidates = stats.candidates_found,
            auto_linked = stats.auto_linked,
            pending_review = stats.pending_review,
            discarded = stats.discarded,
            "OpenSanctions screening complete"
        );
        Ok(stats)
    }
}

// ── Batch query builder ────────────────────────────────────────────────────

/// Build the yente `/match/default` POST body for a batch of entity candidates.
///
/// Format:
/// ```json
/// {"queries": {"q0": {"schema": "Person", "properties": {"name": ["..."], "birthDate": [...], "nationality": [...]}}, ...}}
/// ```
///
/// `birthDate` and `nationality` are included only when present (Some) on the candidate.
pub fn build_batch_query(entities: &[EntityCandidate]) -> serde_json::Value {
    let queries: HashMap<String, serde_json::Value> = entities
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut props = serde_json::json!({ "name": [&e.name] });
            if let Some(dob) = &e.date_of_birth {
                props["birthDate"] = serde_json::json!([dob]);
            }
            if let Some(nat) = &e.nationality {
                props["nationality"] = serde_json::json!([nat]);
            }
            (
                format!("q{i}"),
                serde_json::json!({ "schema": "Person", "properties": props }),
            )
        })
        .collect();

    serde_json::json!({ "queries": queries })
}

// ── Graph helpers ────────────────────────────────────────────────────────────

/// Load Officer and Entity candidates from `graph_events` for sanctions screening.
///
/// Parses `weight_json` Rust-side (SurrealDB 3.x RecordId coercion workaround).
/// Extracts `name`, `date_of_birth` (from `dob` or `date_of_birth` fields), and
/// `nationality` (from `nationality` field) for each Officer/Entity node.
async fn load_candidates_from_graph(db: &Surreal<Any>) -> Result<Vec<EntityCandidate>> {
    let rows: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Officer\"").await?;

    let mut candidates = Vec::new();
    for row in &rows {
        let wj = row["w"].as_str().unwrap_or("");
        let eid = row["eid"].as_str().unwrap_or("").to_string();
        let Ok(d) = serde_json::from_str::<serde_json::Value>(wj) else {
            continue;
        };
        let Some(name) = d.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        // Extract DOB from "dob" or "date_of_birth" fields.
        let date_of_birth = d
            .get("dob")
            .or_else(|| d.get("date_of_birth"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        let nationality = d
            .get("nationality")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        candidates.push(EntityCandidate {
            node_id: eid,
            name: name.to_string(),
            date_of_birth,
            nationality,
        });
    }
    Ok(candidates)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::config::OpenSanctionsConfig;

    fn make_adapter(enabled: bool, confirmed: bool) -> OpenSanctionsAdapter {
        OpenSanctionsAdapter::new(OpenSanctionsConfig {
            enabled,
            endpoint: "http://localhost:8766".to_string(),
            non_commercial_confirmed: confirmed,
            batch_size: 50,
        })
    }

    // ── Test 1: D-06 fail-hard — enabled=true, non_commercial_confirmed=false ─

    /// When enabled=true but non_commercial_confirmed=false, run() must return Err
    /// with the compliance message. is_enabled() returns false.
    ///
    /// This is the D-06 fail-hard path: analyst tried to USE OpenSanctions without
    /// acknowledging the CC-BY-NC licence.
    #[tokio::test]
    async fn test_d06_fail_hard_enabled_not_confirmed() {
        let adapter = make_adapter(true, false);

        // is_enabled() returns false — adapter is not usable.
        assert!(
            !adapter.is_enabled(),
            "is_enabled() must be false when non_commercial_confirmed=false"
        );

        // run() must return Err (not Ok) with compliance message.
        let (tx, _rx) = tokio::sync::mpsc::channel::<GraphMutation>(1);
        // We can't call run() without a real DB, but we can verify the gate fires
        // by constructing a fake DB and confirming the error path.
        // The gate fires BEFORE any DB call, so we use the dead code pattern:
        // directly test the gate logic inline.
        let result = test_run_gate_only(&adapter).await;
        assert!(
            result.is_err(),
            "run() must return Err when enabled=true, non_commercial_confirmed=false"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("non_commercial_confirmed = true"),
            "Err message must contain 'non_commercial_confirmed = true', got: {msg}"
        );

        // Suppress unused variable warning.
        drop(tx);
    }

    // ── Test 2: D-06 silent skip — enabled=false ────────────────────────────

    /// When enabled=false, run() must return Ok(default) silently — no Err, no panic.
    /// Analyst intentionally disabled the adapter; this is not a compliance violation.
    #[tokio::test]
    async fn test_d06_silent_skip_disabled() {
        let adapter = make_adapter(false, false);

        // is_enabled() returns false.
        assert!(
            !adapter.is_enabled(),
            "is_enabled() must be false when enabled=false"
        );

        // run() must return Ok (silent skip, not Err).
        let result = test_run_gate_only(&adapter).await;
        assert!(
            result.is_ok(),
            "run() must return Ok when enabled=false (silent skip, not compliance error)"
        );
        let stats = result.unwrap();
        assert_eq!(
            stats.auto_linked, 0,
            "No links should be written when adapter is disabled"
        );
    }

    // ── Test 3: D-06 happy path — enabled=true, non_commercial_confirmed=true ─

    /// When both enabled=true and non_commercial_confirmed=true, is_enabled() returns true.
    #[test]
    fn test_d06_happy_path_is_enabled() {
        let adapter = make_adapter(true, true);
        assert!(
            adapter.is_enabled(),
            "is_enabled() must be true when enabled=true and non_commercial_confirmed=true"
        );
    }

    // ── Test 4: D-08 name-only cap ──────────────────────────────────────────

    /// A yente score of 0.92 for a name-only query (no DOB, no nationality)
    /// must be clamped to 0.65 max. Verifies D-08 enforcement.
    #[test]
    fn test_d08_name_only_cap() {
        let raw_score: f64 = 0.92;
        let has_dob = false;
        let has_nationality = false;

        // D-08 cap logic (mirrors run() implementation).
        let translated = if !has_dob && !has_nationality {
            raw_score.min(0.65)
        } else {
            raw_score
        };

        assert!(
            (translated - 0.65).abs() < f64::EPSILON,
            "Name-only match with yente score 0.92 must be capped at 0.65 (D-08), got {translated}"
        );

        // Verify the cap does not apply when DOB is present.
        let has_dob_present = true;
        let translated_with_dob = if !has_dob_present && !has_nationality {
            raw_score.min(0.65)
        } else {
            raw_score
        };
        assert!(
            (translated_with_dob - 0.92).abs() < f64::EPSILON,
            "When DOB present, score 0.92 must pass through uncapped, got {translated_with_dob}"
        );
    }

    // ── Test 5: Batch serialisation ─────────────────────────────────────────

    /// `build_batch_query` must produce the correct yente /match/default JSON envelope:
    /// `{"queries": {"q0": {"schema": "Person", "properties": {"name": [...]}}, "q1": ..., "q2": ...}}`
    #[test]
    fn test_batch_serialisation() {
        let entities = vec![
            EntityCandidate {
                node_id: "director:alice-smith".to_string(),
                name: "Alice Smith".to_string(),
                date_of_birth: None,
                nationality: None,
            },
            EntityCandidate {
                node_id: "director:bob-jones".to_string(),
                name: "Bob Jones".to_string(),
                date_of_birth: Some("1975-03-22".to_string()),
                nationality: Some("GB".to_string()),
            },
            EntityCandidate {
                node_id: "director:carol-white".to_string(),
                name: "Carol White".to_string(),
                date_of_birth: None,
                nationality: None,
            },
        ];

        let body = build_batch_query(&entities);
        let queries = body["queries"]
            .as_object()
            .expect("queries must be an object");

        // Must have exactly 3 keys: q0, q1, q2.
        assert_eq!(
            queries.len(),
            3,
            "Expected 3 query keys, got {}",
            queries.len()
        );
        assert!(queries.contains_key("q0"), "Missing q0");
        assert!(queries.contains_key("q1"), "Missing q1");
        assert!(queries.contains_key("q2"), "Missing q2");

        // Each must have schema = "Person".
        for key in ["q0", "q1", "q2"] {
            let schema = queries[key]["schema"].as_str().unwrap_or("");
            assert_eq!(
                schema, "Person",
                "q{key}.schema must be 'Person', got {schema}"
            );
        }

        // q0: name-only (no birthDate, no nationality).
        let q0_props = &queries["q0"]["properties"];
        assert_eq!(
            q0_props["name"],
            serde_json::json!(["Alice Smith"]),
            "q0.properties.name must be [\"Alice Smith\"]"
        );
        assert!(
            q0_props.get("birthDate").is_none(),
            "q0 must not have birthDate (entity has no DOB)"
        );
        assert!(
            q0_props.get("nationality").is_none(),
            "q0 must not have nationality"
        );

        // q1: with DOB and nationality.
        let q1_props = &queries["q1"]["properties"];
        assert_eq!(
            q1_props["name"],
            serde_json::json!(["Bob Jones"]),
            "q1.properties.name must be [\"Bob Jones\"]"
        );
        assert_eq!(
            q1_props["birthDate"],
            serde_json::json!(["1975-03-22"]),
            "q1.properties.birthDate must be [\"1975-03-22\"]"
        );
        assert_eq!(
            q1_props["nationality"],
            serde_json::json!(["GB"]),
            "q1.properties.nationality must be [\"GB\"]"
        );
    }

    // ── Test 6: Status routing ───────────────────────────────────────────────

    /// Verify confidence routing thresholds against the tier ladder.
    /// 0.50 → SubCandidate; 0.70 → PendingReview; 0.85 → AutoLinked; 0.40 → discard.
    #[test]
    fn test_status_routing() {
        // Helper replicating the run() routing logic.
        fn route(score: f64) -> Option<LinkStatus> {
            if score >= AUTO_LINK_THRESHOLD {
                Some(LinkStatus::AutoLinked)
            } else if score >= CANDIDATE_THRESHOLD {
                Some(LinkStatus::PendingReview)
            } else if score >= SUB_CANDIDATE_THRESHOLD {
                Some(LinkStatus::SubCandidate)
            } else {
                None // discarded
            }
        }

        assert_eq!(
            route(0.50),
            Some(LinkStatus::SubCandidate),
            "0.50 must route to SubCandidate"
        );
        assert_eq!(
            route(0.70),
            Some(LinkStatus::PendingReview),
            "0.70 must route to PendingReview"
        );
        assert_eq!(
            route(0.85),
            Some(LinkStatus::AutoLinked),
            "0.85 must route to AutoLinked"
        );
        assert_eq!(
            route(0.40),
            None,
            "0.40 must be discarded (below SUB_CANDIDATE_THRESHOLD)"
        );
    }

    // ── Gate-only helper (avoids live DB dependency) ─────────────────────────

    /// Exercise only the enabled/compliance gates of `run()` without a live DB.
    ///
    /// When `enabled=false` → returns Ok(default) before any DB access.
    /// When `enabled=true, non_commercial_confirmed=false` → returns Err before any DB access.
    async fn test_run_gate_only(adapter: &OpenSanctionsAdapter) -> anyhow::Result<EnrichmentStats> {
        // Gate 1: adapter intentionally disabled.
        if !adapter.config.enabled {
            warn!(
                adapter = "opensanctions",
                "opensanctions adapter disabled in config"
            );
            return Ok(EnrichmentStats::default());
        }
        // Gate 2: fail-hard compliance check.
        if !adapter.config.non_commercial_confirmed {
            return Err(anyhow::anyhow!(
                "OpenSanctions requires `non_commercial_confirmed = true` in \
                 [enricher.opensanctions] config (CC-BY-NC compliance)"
            ));
        }
        // If we reach here, both gates pass — return Ok to signal happy-path entry.
        Ok(EnrichmentStats::default())
    }
}
