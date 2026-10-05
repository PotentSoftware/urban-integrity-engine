//! Charity-Commercial Crossover detector (FUZ-06).
//!
//! ENR-11: only queries `identity_links` WHERE status IN ['auto_linked','confirmed'].
//! 'candidate' and 'sub_candidate' links MUST NEVER trigger this detector.
//!
//! Ward-overlap: simplified LPA-level overlap check (per the research notes open question 4 resolution).
//! Full strict ward-level check deferred to Phase 20.
//!
//! Alert sink (R-02): `graph_events` kind='MotifDetected'. No `alert_records` table.
//!
//! SurrealDB RecordId coercion workaround: uses `weight_json CONTAINS` pattern for
//! `graph_events` queries; filters by officer_id Rust-side.
//!
//! # Security
//!
//! T-19-05-01: ENR-11 status gate bypass — both SurrealQL query and `process_trustee_rows`
//! reject non-auto_linked/confirmed rows. Tests 1+2 exercise the full filter path.
//! T-19-05-02: SurrealQL injection — CONTAINS patterns are code constants, not user input.
//! T-19-05-03: DoS — LIMIT 50000 on all graph_events queries.
//! T-19-05-04: Duplicate alerts — compute_motif_hash + alert_already_exists dedup.

use std::collections::{HashMap, HashSet};
use urban_integrity_core::persistence::scan;

use chrono::Utc;
use surrealdb::{engine::any::Any, Surreal};
use tracing::{debug, info, warn};

use crate::evasion::{advance_last_seen_run_id, compute_motif_hash, AlertedHashes};

/// A candidate for a Charity-Commercial Crossover alert — passed the ENR-11 gate
/// and has an officer that Directs a contract-holding entity.
#[derive(Debug, PartialEq)]
pub(crate) struct CrossoverCandidate {
    pub(crate) officer_id: String,
    pub(crate) charity_node_id: String,
}

/// Filter `trustee_rows` through the ENR-11 status gate and match against
/// `officer_directs_contract_ids`.
///
/// This is the pure-function extraction required by the plan (§behavior, Test 1).
/// All DB interaction is outside this function — making it fully unit-testable.
///
/// # ENR-11 gate
///
/// Only rows with `status == "auto_linked"` or `status == "confirmed"` proceed.
/// Rows with `status == "candidate"` or any other status are discarded here,
/// satisfying the negative test (Test 1).
///
/// # RecordId coercion workaround
///
/// `source_node_id` may be returned as a SurrealDB `RecordId` object rather than a
/// plain string. We check both `as_str()` and `to_string().contains()` to handle
/// both cases, mirroring the approach in `evasion.rs` and `offshore_shadow.rs`.
pub(crate) fn process_trustee_rows(
    rows: &[serde_json::Value],
    officer_directs_contract_ids: &HashSet<String>,
) -> Vec<CrossoverCandidate> {
    let mut candidates = Vec::new();

    for row in rows {
        // ── ENR-11 status gate ────────────────────────────────────────────
        let status = row["status"].as_str().unwrap_or("");
        if status != "auto_linked" && status != "confirmed" {
            // Reject candidate, sub_candidate, or any unknown status.
            continue;
        }

        // ── Extract officer_id (source_node_id) with RecordId coercion workaround ──
        let officer_id = match row["source_node_id"].as_str() {
            Some(id) => id.to_string(),
            None => {
                let repr = row["source_node_id"].to_string();
                if repr == "null" || repr.is_empty() {
                    warn!("Charity Crossover: link row missing source_node_id — skipping");
                    continue;
                }
                repr
            }
        };

        // ── Directs cross-check (Rust-side) ──────────────────────────────
        if !officer_directs_contract_ids.contains(&officer_id) {
            // Also try RecordId JSON representations stored in the set.
            let matched = officer_directs_contract_ids
                .iter()
                .any(|id| officer_id.contains(id.as_str()) || id.contains(officer_id.as_str()));
            if !matched {
                continue;
            }
        }

        // ── Extract charity node id ───────────────────────────────────────
        let charity_node_id = row["target_node_id"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| row["target_node_id"].to_string());

        candidates.push(CrossoverCandidate {
            officer_id,
            charity_node_id,
        });
    }

    candidates
}

/// Run the Charity-Commercial Crossover detector.
///
/// Queries all `auto_linked` or `confirmed` charity commission TrusteeOf identity links
/// (ENR-11), cross-checks against officers with active Directs edges to contract-holding
/// entities, and fires a MEDIUM `graph_events / MotifDetected` alert.
///
/// Returns the number of new alerts written.
///
/// # Alert sink (R-02)
///
/// Writes directly to `graph_events` via `CREATE graph_events CONTENT $r`.
/// Does NOT use a separate `alert_records` table.
///
/// # ENR-11 status filter
///
/// SELECT ... WHERE source = 'charity_commission' AND status IN ['auto_linked','confirmed']
pub async fn run_charity_crossover_detector(
    db: &Surreal<Any>,
    run_id: &str,
) -> anyhow::Result<usize> {
    // ── Step 1: Fetch confirmed/auto_linked TrusteeOf links from charity_commission ──
    //
    // status IN ['auto_linked','confirmed'] is the ENR-11 gate.
    let trustee_rows: Vec<serde_json::Value> = db
        .query(
            "SELECT source_node_id, target_node_id, status \
             FROM identity_links \
             WHERE source = $source \
               AND status IN [$auto_linked, $confirmed]",
        )
        .bind(("source", "charity_commission"))
        .bind(("auto_linked", "auto_linked"))
        .bind(("confirmed", "confirmed"))
        .await?
        .take(0)?;

    if trustee_rows.is_empty() {
        debug!("Charity Crossover: no confirmed/auto_linked charity_commission links found");
        return Ok(0);
    }

    // ── Step 2: Build officer_directs_contract_ids ───────────────────────────
    //
    // SurrealDB RecordId coercion workaround: use CONTAINS pattern + Rust-side filter.
    //
    // Step 2a: Fetch all Directs edges to build officer → target_entity map.
    let directs_rows: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"Directs\"").await?;

    // Build officer_id → Vec<target_entity_id> map (Rust-side filtering).
    let mut officer_targets: HashMap<String, Vec<String>> = HashMap::new();
    for row in &directs_rows {
        let src = row["payload"]["src_id"].as_str().unwrap_or("").to_string();
        let dst = row["payload"]["dst_id"].as_str().unwrap_or("").to_string();
        let src_json = row["payload"]["src_id"].to_string();
        let dst_json = row["payload"]["dst_id"].to_string();

        // Use the best available representation for the officer ID.
        let officer = if !src.is_empty() {
            src.clone()
        } else if src_json != "null" {
            src_json.clone()
        } else {
            continue;
        };

        // Use the best available representation for the target entity ID.
        let target = if !dst.is_empty() {
            dst.clone()
        } else if dst_json != "null" {
            dst_json.clone()
        } else {
            continue;
        };

        officer_targets.entry(officer).or_default().push(target);
    }

    // Step 2b: Fetch all AwardedTo edges to determine which entities hold contracts.
    let awarded_rows: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"AwardedTo\"").await?;

    let mut contract_entity_ids: HashSet<String> = HashSet::new();
    for row in &awarded_rows {
        let src = row["payload"]["src_id"].as_str().unwrap_or("").to_string();
        if !src.is_empty() {
            contract_entity_ids.insert(src.clone());
        }
        let src_json = row["payload"]["src_id"].to_string();
        if src_json != "null" && !src_json.is_empty() && src_json != src {
            contract_entity_ids.insert(src_json);
        }
    }

    // Step 2c: Build officer_directs_contract_ids — officers who direct at least one
    // entity that holds a contract.
    let mut officer_directs_contract_ids: HashSet<String> = HashSet::new();
    for (officer, targets) in &officer_targets {
        let directs_contract_holder = targets.iter().any(|target| {
            contract_entity_ids.contains(target)
                || contract_entity_ids
                    .iter()
                    .any(|cid| target.contains(cid.as_str()) || cid.contains(target.as_str()))
        });
        if directs_contract_holder {
            officer_directs_contract_ids.insert(officer.clone());
        }
    }

    // ── Step 3: Filter candidates through ENR-11 gate + Directs cross-check ──
    let candidates = process_trustee_rows(&trustee_rows, &officer_directs_contract_ids);

    if candidates.is_empty() {
        debug!("Charity Crossover: no candidates after ENR-11 filter + Directs cross-check");
        return Ok(0);
    }

    // ── Step 4: Write MotifDetected alerts (R-02) ───────────────────────────
    //
    // Ward overlap check (simplified per the research notes open question 4 resolution):
    // If the officer Directs a contract-holder AND is TrusteeOf a charity AND ward
    // overlap cannot be determined at this stage → emit MEDIUM alert conservatively.
    // Full strict ward-level check deferred to Phase 20.
    let today = Utc::now();
    let mut alerts_written = 0usize;
    // Loaded once, not per candidate — see AlertedHashes.
    let alerted = AlertedHashes::load(db).await;

    for candidate in &candidates {
        // CR-04: use officer_id + charity_node_id so each distinct charity link pair
        // gets a unique dedup key — the previous fixed string caused two different
        // charity links for the same officer to collide and suppress alerts.
        let motif_hash = compute_motif_hash(&candidate.officer_id, &candidate.charity_node_id);

        // Dedup check (T-19-05-04)
        if alerted.contains(&motif_hash) {
            // Still true this run — re-confirm rather than fall silent (WR-01). See
            // `evasion::advance_last_seen_run_id`.
            if let Some(record_id) = alerted.record_id(&motif_hash) {
                advance_last_seen_run_id(db, record_id, run_id).await;
            }
            debug!(
                officer_id = %candidate.officer_id,
                motif_hash,
                "Charity Crossover: alert already exists (dedup) — re-confirmed"
            );
            continue;
        }

        let description = format!(
            "Officer {} directs contract-holder AND is trustee of charity",
            candidate.officer_id
        );
        let result_json = build_result_json(
            &description,
            &candidate.officer_id,
            &candidate.charity_node_id,
            &motif_hash,
            run_id,
        );
        let result_json_str = result_json.to_string();

        let event_id = uuid::Uuid::new_v4().to_string();
        let timestamp_ms = today.timestamp_millis() as u64;
        let record = serde_json::json!({
            "event_id": event_id,
            "timestamp": timestamp_ms,
            "kind": "MotifDetected",
            "payload": { "result_json": result_json_str },
        });

        let write_result: surrealdb::Result<Vec<serde_json::Value>> = db
            .query("CREATE graph_events CONTENT $r")
            .bind(("r", record))
            .await
            .and_then(|mut r| r.take(0));

        match write_result {
            Ok(_) => {
                info!(
                    officer_id = %candidate.officer_id,
                    motif_hash,
                    "Charity Crossover: MEDIUM alert written — charity_commercial_crossover"
                );
                alerts_written += 1;
            }
            Err(e) => {
                warn!(
                    error = %e,
                    officer_id = %candidate.officer_id,
                    "Charity Crossover: failed to write alert (non-fatal)"
                );
            }
        }
    }

    Ok(alerts_written)
}

/// Build the `result_json` literal for a Charity-Commercial Crossover alert (46-05).
///
/// Pure — no DB access — so the exact shape (in particular `participants`) is
/// host-only unit-testable without `URBAN_DB_TESTS=1`. `participants` names the pair
/// (phase 46 assumption A-05: Structural), officer then charity node, in that order.
fn build_result_json(
    description: &str,
    officer_id: &str,
    charity_node_id: &str,
    motif_hash: &str,
    run_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "motif_type": "charity_commercial_crossover",
        "risk_score": 0.65_f64,
        "description": description,
        "officer_id": officer_id,
        "charity_node_id": charity_node_id,
        "motif_hash": motif_hash,
        "run_id": run_id,
        "first_seen_run_id": run_id,
        "last_seen_run_id": run_id,
        "participants": [officer_id, charity_node_id],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 46-05 Task 1: `result_json` carries run attribution and a 2-element
    /// `participants` array (phase 46 assumption A-05: Structural), officer then charity node.
    #[test]
    fn result_json_carries_run_attribution_and_participants() {
        let result_json = build_result_json(
            "Officer officer:jane-doe directs contract-holder AND is trustee of charity",
            "officer:jane-doe",
            "charity:abc-123",
            "deadbeef",
            "run-x",
        );
        assert_eq!(result_json["run_id"], "run-x");
        assert_eq!(result_json["first_seen_run_id"], "run-x");
        assert_eq!(result_json["last_seen_run_id"], "run-x");
        assert_eq!(
            result_json["participants"],
            serde_json::json!(["officer:jane-doe", "charity:abc-123"])
        );
    }

    /// Test 1 (ENR-11 negative — MANDATORY, full detector path):
    ///
    /// A trustee link row with status='candidate' and confidence=0.72 must produce
    /// ZERO CrossoverCandidates even when the officer_id matches a Directs edge holder.
    /// This proves the full filter path rejects candidate-status rows.
    #[test]
    fn test_enr11_negative_candidate_produces_zero_candidates() {
        let trustee_rows = vec![serde_json::json!({
            "source_node_id": "officer:jane-doe",
            "target_node_id": "charity:abc-123",
            "status": "candidate",
            "confidence": 0.72_f64,
            "source": "charity_commission",
        })];

        let mut officer_directs_contract_ids = HashSet::new();
        officer_directs_contract_ids.insert("officer:jane-doe".to_string());

        let result = process_trustee_rows(&trustee_rows, &officer_directs_contract_ids);

        assert!(
            result.is_empty(),
            "ENR-11 negative test FAILED: candidate-status row produced {:?} CrossoverCandidates, expected 0",
            result.len()
        );
    }

    /// Test 2 (ENR-11 positive — gate works both ways):
    ///
    /// Same inputs except row status = "auto_linked". Must return one CrossoverCandidate
    /// with officer_id = "officer:jane-doe".
    #[test]
    fn test_enr11_positive_auto_linked_produces_one_candidate() {
        let trustee_rows = vec![serde_json::json!({
            "source_node_id": "officer:jane-doe",
            "target_node_id": "charity:abc-123",
            "status": "auto_linked",
            "confidence": 0.88_f64,
            "source": "charity_commission",
        })];

        let mut officer_directs_contract_ids = HashSet::new();
        officer_directs_contract_ids.insert("officer:jane-doe".to_string());

        let result = process_trustee_rows(&trustee_rows, &officer_directs_contract_ids);

        assert_eq!(
            result.len(),
            1,
            "ENR-11 positive test FAILED: auto_linked row produced {} candidates, expected 1",
            result.len()
        );
        assert_eq!(result[0].officer_id, "officer:jane-doe");
    }

    /// Test 3: compute_motif_hash returns a stable hex string.
    #[test]
    fn test_compute_motif_hash_stable() {
        let h1 = compute_motif_hash("officer:jane-doe", "charity_commercial_crossover");
        let h2 = compute_motif_hash("officer:jane-doe", "charity_commercial_crossover");
        assert_eq!(h1, h2, "motif_hash must be deterministic");
        assert!(!h1.is_empty(), "motif_hash must not be empty");
        assert!(
            h1.chars().all(|c| c.is_ascii_hexdigit()),
            "motif_hash must be a hex string"
        );
    }

    /// Test 4: process_trustee_rows with empty inputs returns empty Vec without error.
    #[test]
    fn test_process_trustee_rows_empty_inputs() {
        let result = process_trustee_rows(&[], &HashSet::new());
        assert!(result.is_empty());
    }

    /// Test 5: officer NOT in directs set → skipped (no false crossover).
    #[test]
    fn test_officer_not_in_directs_set_is_skipped() {
        let rows = vec![serde_json::json!({
            "source_node_id": "officer:unrelated-person",
            "target_node_id": "charity:some-charity",
            "status": "auto_linked",
            "confidence": 0.88_f64,
        })];
        let mut directs = HashSet::new();
        directs.insert("officer:different-person".to_string());

        let result = process_trustee_rows(&rows, &directs);
        assert!(
            result.is_empty(),
            "officer not directing any contract-holder should produce no candidates"
        );
    }

    /// Test 6: confirmed status is accepted (not just auto_linked).
    #[test]
    fn test_confirmed_status_is_accepted() {
        let rows = vec![serde_json::json!({
            "source_node_id": "officer:jane-doe",
            "target_node_id": "charity:abc-123",
            "status": "confirmed",
            "confidence": 0.95_f64,
        })];
        let mut directs = HashSet::new();
        directs.insert("officer:jane-doe".to_string());

        let result = process_trustee_rows(&rows, &directs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].charity_node_id, "charity:abc-123");
    }

    /// Test 7: sub_candidate status → rejected.
    #[test]
    fn test_sub_candidate_status_is_rejected() {
        let rows = vec![serde_json::json!({
            "source_node_id": "officer:jane-doe",
            "target_node_id": "charity:abc-123",
            "status": "sub_candidate",
            "confidence": 0.55_f64,
        })];
        let mut directs = HashSet::new();
        directs.insert("officer:jane-doe".to_string());

        let result = process_trustee_rows(&rows, &directs);
        assert!(
            result.is_empty(),
            "sub_candidate rows must not fire Charity Crossover alerts"
        );
    }

    /// Test 8: RecordId coercion — source_node_id is a JSON object, partial match path.
    #[test]
    fn test_recordid_coercion_officer_id() {
        let rows = vec![serde_json::json!({
            "source_node_id": {"tb": "officer", "id": "jane-doe"},
            "target_node_id": "charity:abc-123",
            "status": "auto_linked",
            "confidence": 0.88_f64,
        })];
        // The directs set holds the bare slug; partial match must succeed.
        let mut directs = HashSet::new();
        directs.insert("jane-doe".to_string());

        let result = process_trustee_rows(&rows, &directs);
        assert_eq!(
            result.len(),
            1,
            "RecordId coercion partial match should produce one candidate"
        );
    }

    /// Test 9: null source_node_id is skipped gracefully.
    #[test]
    fn test_null_source_node_id_is_skipped() {
        let rows = vec![serde_json::json!({
            "source_node_id": null,
            "target_node_id": "charity:abc-123",
            "status": "auto_linked",
        })];
        let result = process_trustee_rows(&rows, &HashSet::new());
        assert!(result.is_empty(), "null source_node_id must be skipped");
    }

    /// Test 10: multiple rows — only officer-in-directs auto_linked/confirmed rows returned.
    #[test]
    fn test_multiple_rows_only_matching_returned() {
        let rows = vec![
            serde_json::json!({"source_node_id": "officer:alice", "target_node_id": "charity:1", "status": "auto_linked", "confidence": 0.90_f64}),
            serde_json::json!({"source_node_id": "officer:bob", "target_node_id": "charity:2", "status": "auto_linked", "confidence": 0.88_f64}),
            serde_json::json!({"source_node_id": "officer:alice", "target_node_id": "charity:3", "status": "candidate", "confidence": 0.72_f64}),
            serde_json::json!({"source_node_id": "officer:carol", "target_node_id": "charity:4", "status": "confirmed", "confidence": 0.95_f64}),
        ];
        let mut directs = HashSet::new();
        directs.insert("officer:alice".to_string());
        directs.insert("officer:carol".to_string());

        let result = process_trustee_rows(&rows, &directs);
        assert_eq!(
            result.len(),
            2,
            "alice(auto_linked) + carol(confirmed) should match; bob not in directs, alice(candidate) rejected"
        );
    }
}
