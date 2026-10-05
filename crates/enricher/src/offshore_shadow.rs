//! Offshore Shadow detector (FUZ-05).
//!
//! ENR-11 status filter: only queries `identity_links` WHERE status IN ['auto_linked','confirmed'].
//! 'candidate' and 'sub_candidate' links MUST NEVER trigger this detector.
//!
//! Alert sink (R-02): writes to `graph_events` WHERE kind='MotifDetected'. No `alert_records` table.
//!
//! SurrealDB RecordId coercion workaround: uses `weight_json CONTAINS` pattern for `graph_events`
//! queries; filters by entity_id Rust-side.
//!
//! # Security
//!
//! T-19-05-01: ENR-11 status gate bypass — both the SurrealQL query and `process_link_rows`
//! reject non-auto_linked/confirmed rows. Tests 1+2 exercise the full filter path.
//! T-19-05-02: SurrealQL injection — CONTAINS pattern is a code constant, not user input.
//! T-19-05-03: DoS — LIMIT 50000 on all graph_events queries.
//! T-19-05-04: Duplicate alerts — compute_motif_hash + alert_already_exists dedup.

use std::collections::HashSet;
use urban_integrity_core::persistence::scan;

use chrono::Utc;
use surrealdb::{engine::any::Any, Surreal};
use tracing::{debug, info, warn};

use crate::evasion::{advance_last_seen_run_id, compute_motif_hash, AlertedHashes};

/// A candidate for an Offshore Shadow alert — passed the ENR-11 gate and has an
/// active AWARDED_TO contract.
#[derive(Debug, PartialEq)]
pub(crate) struct MotifCandidate {
    pub(crate) entity_id: String,
    pub(crate) icij_node_id: String,
}

/// Filter `link_rows` through the ENR-11 status gate and match against
/// `contract_entity_ids`.
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
/// `source_node_id` may be returned as a SurrealDB `RecordId` (e.g. the JSON
/// representation `{"tb":"entity","id":"gb-coh-12345678"}`) rather than a plain
/// string. We check both `as_str()` and `to_string().contains()` to handle both
/// cases, mirroring the approach in `evasion.rs`.
pub(crate) fn process_link_rows(
    rows: &[serde_json::Value],
    contract_entity_ids: &HashSet<String>,
) -> Vec<MotifCandidate> {
    let mut candidates = Vec::new();

    for row in rows {
        // ── ENR-11 status gate ────────────────────────────────────────────
        let status = row["status"].as_str().unwrap_or("");
        if status != "auto_linked" && status != "confirmed" {
            // Reject candidate, sub_candidate, or any unknown status.
            continue;
        }

        // ── Extract entity_id (source_node_id) with RecordId coercion workaround ──
        let entity_id = match row["source_node_id"].as_str() {
            Some(id) => id.to_string(),
            None => {
                // SurrealDB may return a RecordId object; fall back to JSON repr.
                let repr = row["source_node_id"].to_string();
                if repr == "null" || repr.is_empty() {
                    warn!("Offshore Shadow: link row missing source_node_id — skipping");
                    continue;
                }
                repr
            }
        };

        // ── Contract cross-check (Rust-side) ─────────────────────────────
        if !contract_entity_ids.contains(&entity_id) {
            // Also try RecordId JSON representations stored in contract_entity_ids.
            let matched = contract_entity_ids
                .iter()
                .any(|id| entity_id.contains(id.as_str()) || id.contains(entity_id.as_str()));
            if !matched {
                continue;
            }
        }

        // ── Extract ICIJ node id ──────────────────────────────────────────
        let icij_node_id = row["target_node_id"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| row["target_node_id"].to_string());

        candidates.push(MotifCandidate {
            entity_id,
            icij_node_id,
        });
    }

    candidates
}

/// Run the Offshore Shadow detector.
///
/// Queries all `auto_linked` or `confirmed` ICIJ offshore identity links (ENR-11),
/// cross-checks against entities with active AWARDED_TO contracts, and fires a HIGH
/// `graph_events / MotifDetected` alert for each confirmed offshore contract-holder.
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
/// SELECT ... WHERE source = 'icij' AND status IN ['auto_linked','confirmed']
pub async fn run_offshore_shadow_detector(
    db: &Surreal<Any>,
    run_id: &str,
) -> anyhow::Result<usize> {
    // ── Step 1: Fetch all confirmed/auto_linked ICIJ offshore identity links ──
    //
    // status IN ['auto_linked','confirmed'] is the ENR-11 gate.
    // Candidate links (status='candidate') are excluded here.
    let link_rows: Vec<serde_json::Value> = db
        .query(
            "SELECT source_node_id, target_node_id, evidence_json, status \
             FROM identity_links \
             WHERE source = $source \
               AND status IN [$auto_linked, $confirmed]",
        )
        .bind(("source", "icij"))
        .bind(("auto_linked", "auto_linked"))
        .bind(("confirmed", "confirmed"))
        .await?
        .take(0)?;

    if link_rows.is_empty() {
        debug!("Offshore Shadow: no confirmed/auto_linked icij links found");
        return Ok(0);
    }

    // ── Step 2: Build contract_entity_ids ───────────────────────────────────
    //
    // SurrealDB RecordId coercion workaround: use CONTAINS pattern + Rust-side filter.
    // Never use WHERE payload.entity_id = $id for director:* / GB-COH:* patterns.
    let contract_rows: Vec<serde_json::Value> =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"AwardedTo\"").await?;

    let mut contract_entity_ids: HashSet<String> = HashSet::new();
    for row in &contract_rows {
        // src_id is the entity (contract-holder) in an AwardedTo edge.
        let src = row["payload"]["src_id"].as_str().unwrap_or("");
        if !src.is_empty() {
            contract_entity_ids.insert(src.to_string());
        }
        // Also capture JSON representation for RecordId objects.
        let src_json = row["payload"]["src_id"].to_string();
        if src_json != "null" && !src_json.is_empty() && src_json != src {
            contract_entity_ids.insert(src_json);
        }
    }

    // ── Step 3: Filter candidates through ENR-11 gate + contract cross-check ──
    let candidates = process_link_rows(&link_rows, &contract_entity_ids);

    if candidates.is_empty() {
        debug!("Offshore Shadow: no candidates after ENR-11 filter + contract cross-check");
        return Ok(0);
    }

    // ── Step 4: Write MotifDetected alerts (R-02) ───────────────────────────
    let today = Utc::now();
    let mut alerts_written = 0usize;
    // Loaded once, not per candidate — see AlertedHashes.
    let alerted = AlertedHashes::load(db).await;

    for candidate in &candidates {
        // CR-04: use entity_id + icij_node_id so each distinct offshore link pair
        // gets a unique dedup key — the previous fixed string "offshore_shadow" caused
        // two different ICIJ links for the same entity to collide and suppress alerts.
        let motif_hash = compute_motif_hash(&candidate.entity_id, &candidate.icij_node_id);

        // Dedup check (T-19-05-04)
        if alerted.contains(&motif_hash) {
            // Still true this run — re-confirm rather than fall silent (WR-01). See
            // `evasion::advance_last_seen_run_id`.
            if let Some(record_id) = alerted.record_id(&motif_hash) {
                advance_last_seen_run_id(db, record_id, run_id).await;
            }
            debug!(
                entity_id = %candidate.entity_id,
                motif_hash,
                "Offshore Shadow: alert already exists (dedup) — re-confirmed"
            );
            continue;
        }

        let description = format!(
            "Active contract entity has confirmed offshore link: {}",
            candidate.entity_id
        );
        let result_json = build_result_json(
            &description,
            &candidate.entity_id,
            &candidate.icij_node_id,
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
                    entity_id = %candidate.entity_id,
                    motif_hash,
                    "Offshore Shadow: HIGH alert written — offshore_shadow"
                );
                alerts_written += 1;
            }
            Err(e) => {
                warn!(
                    error = %e,
                    entity_id = %candidate.entity_id,
                    "Offshore Shadow: failed to write alert (non-fatal)"
                );
            }
        }
    }

    Ok(alerts_written)
}

/// Build the `result_json` literal for an Offshore Shadow alert (46-05).
///
/// Pure — no DB access — so the exact shape (in particular `participants`) is
/// host-only unit-testable without `URBAN_DB_TESTS=1`. `participants` names the pair
/// (phase 46 assumption A-05: Structural), entity then ICIJ node, in that order.
fn build_result_json(
    description: &str,
    entity_id: &str,
    icij_node_id: &str,
    motif_hash: &str,
    run_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "motif_type": "offshore_shadow",
        "risk_score": 0.9_f64,
        "description": description,
        "entity_id": entity_id,
        "icij_node_id": icij_node_id,
        "motif_hash": motif_hash,
        "run_id": run_id,
        "first_seen_run_id": run_id,
        "last_seen_run_id": run_id,
        "participants": [entity_id, icij_node_id],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 46-05 Task 1: `result_json` carries run attribution and a 2-element
    /// `participants` array (phase 46 assumption A-05: Structural), entity then ICIJ node.
    #[test]
    fn result_json_carries_run_attribution_and_participants() {
        let result_json = build_result_json(
            "Active contract entity has confirmed offshore link: entity:GB-COH:12345678",
            "entity:GB-COH:12345678",
            "icij:node-1",
            "deadbeef",
            "run-x",
        );
        assert_eq!(result_json["run_id"], "run-x");
        assert_eq!(result_json["first_seen_run_id"], "run-x");
        assert_eq!(result_json["last_seen_run_id"], "run-x");
        assert_eq!(
            result_json["participants"],
            serde_json::json!(["entity:GB-COH:12345678", "icij:node-1"])
        );
    }

    /// Test 1 (ENR-11 negative — MANDATORY, full detector path):
    ///
    /// A link row with status='candidate' and confidence=0.72 must produce ZERO
    /// MotifCandidates even when the entity_id matches a contract-holder.
    /// This proves the full filter path rejects candidate-status rows.
    #[test]
    fn test_enr11_negative_candidate_produces_zero_candidates() {
        let link_rows = vec![serde_json::json!({
            "source_node_id": "entity:test-corp",
            "target_node_id": "icij:xyz",
            "status": "candidate",
            "confidence": 0.72_f64,
            "source": "icij",
        })];

        let mut contract_entity_ids = HashSet::new();
        contract_entity_ids.insert("entity:test-corp".to_string());

        let result = process_link_rows(&link_rows, &contract_entity_ids);

        assert!(
            result.is_empty(),
            "ENR-11 negative test FAILED: candidate-status row produced {:?} MotifCandidates, expected 0",
            result.len()
        );
    }

    /// Test 2 (ENR-11 positive — gate works both ways):
    ///
    /// Same inputs except row status = "auto_linked", confidence = 0.88.
    /// Must return one MotifCandidate with entity_id = "entity:test-corp".
    #[test]
    fn test_enr11_positive_auto_linked_produces_one_candidate() {
        let link_rows = vec![serde_json::json!({
            "source_node_id": "entity:test-corp",
            "target_node_id": "icij:xyz",
            "status": "auto_linked",
            "confidence": 0.88_f64,
            "source": "icij",
        })];

        let mut contract_entity_ids = HashSet::new();
        contract_entity_ids.insert("entity:test-corp".to_string());

        let result = process_link_rows(&link_rows, &contract_entity_ids);

        assert_eq!(
            result.len(),
            1,
            "ENR-11 positive test FAILED: auto_linked row produced {} candidates, expected 1",
            result.len()
        );
        assert_eq!(result[0].entity_id, "entity:test-corp");
    }

    /// Test 2b (PROP-06 negative — MANDATORY): an OCOD↔ICIJ name-only link sits at
    /// `pending_review`@0.65. It must produce ZERO offshore-shadow candidates — the
    /// ≥0.85 (auto_linked/confirmed) ENR-11 gate is untouched, so a deliberately-
    /// bounded 0.65 compliance link can never fire the detector (defamation guard).
    #[test]
    fn test_offshore_shadow_ignores_065() {
        let link_rows = vec![serde_json::json!({
            "source_node_id": "entity:test-corp",
            "target_node_id": "icij:xyz",
            "status": "pending_review",
            "confidence": 0.65_f64,
            "source": "land_registry_ocod",
        })];

        let mut contract_entity_ids = HashSet::new();
        contract_entity_ids.insert("entity:test-corp".to_string());

        let result = process_link_rows(&link_rows, &contract_entity_ids);

        assert!(
            result.is_empty(),
            "PROP-06 negative test FAILED: pending_review@0.65 produced {} candidates, expected 0",
            result.len()
        );
    }

    /// Test 3: compute_motif_hash returns a stable hex string.
    #[test]
    fn test_compute_motif_hash_stable() {
        let h1 = compute_motif_hash("entity:gb-coh-12345678", "offshore_shadow");
        let h2 = compute_motif_hash("entity:gb-coh-12345678", "offshore_shadow");
        assert_eq!(h1, h2, "motif_hash must be deterministic");
        assert!(!h1.is_empty(), "motif_hash must not be empty");
        assert!(
            h1.chars().all(|c| c.is_ascii_hexdigit()),
            "motif_hash must be a hex string"
        );
    }

    /// Test 4: process_link_rows with empty inputs returns empty Vec without error.
    #[test]
    fn test_process_link_rows_empty_inputs() {
        let result = process_link_rows(&[], &HashSet::new());
        assert!(result.is_empty());
    }

    /// Test 5: entity NOT in contract set → skipped (no false positives).
    #[test]
    fn test_entity_not_in_contract_set_is_skipped() {
        let link_rows = vec![serde_json::json!({
            "source_node_id": "entity:not-a-contractor",
            "target_node_id": "icij:xyz",
            "status": "auto_linked",
            "confidence": 0.90_f64,
        })];
        let mut contract_entity_ids = HashSet::new();
        contract_entity_ids.insert("entity:some-other-corp".to_string());

        let result = process_link_rows(&link_rows, &contract_entity_ids);
        assert!(
            result.is_empty(),
            "entity not in contract set should produce no candidates"
        );
    }

    /// Test 6: confirmed status (not just auto_linked) is also accepted by the gate.
    #[test]
    fn test_confirmed_status_is_accepted() {
        let link_rows = vec![serde_json::json!({
            "source_node_id": "entity:gb-coh-99999999",
            "target_node_id": "icij:panama-456",
            "status": "confirmed",
            "confidence": 0.95_f64,
        })];
        let mut contract_entity_ids = HashSet::new();
        contract_entity_ids.insert("entity:gb-coh-99999999".to_string());

        let result = process_link_rows(&link_rows, &contract_entity_ids);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].icij_node_id, "icij:panama-456");
    }

    /// Test 7: sub_candidate status → rejected (same as candidate — not actionable).
    #[test]
    fn test_sub_candidate_status_is_rejected() {
        let link_rows = vec![serde_json::json!({
            "source_node_id": "entity:test-corp",
            "target_node_id": "icij:xyz",
            "status": "sub_candidate",
            "confidence": 0.55_f64,
        })];
        let mut contract_entity_ids = HashSet::new();
        contract_entity_ids.insert("entity:test-corp".to_string());

        let result = process_link_rows(&link_rows, &contract_entity_ids);
        assert!(
            result.is_empty(),
            "sub_candidate rows must not fire Offshore Shadow alerts"
        );
    }

    /// Test 8: RecordId JSON coercion path — source_node_id is a JSON object, not a plain string.
    #[test]
    fn test_recordid_coercion_source_node_id() {
        // SurrealDB may return source_node_id as {"tb":"entity","id":"gb-coh-12345678"}
        let link_rows = vec![serde_json::json!({
            "source_node_id": {"tb": "entity", "id": "gb-coh-12345678"},
            "target_node_id": "icij:offshore-123",
            "status": "auto_linked",
            "confidence": 0.88_f64,
        })];
        // contract_entity_ids contains the string form; partial match must succeed
        let mut contract_entity_ids = HashSet::new();
        contract_entity_ids.insert("gb-coh-12345678".to_string());

        let result = process_link_rows(&link_rows, &contract_entity_ids);
        assert_eq!(
            result.len(),
            1,
            "RecordId coercion should allow partial match: got {} candidates",
            result.len()
        );
    }

    /// Test 9: missing source_node_id (null) → row skipped gracefully.
    #[test]
    fn test_null_source_node_id_is_skipped() {
        let link_rows = vec![serde_json::json!({
            "source_node_id": null,
            "target_node_id": "icij:xyz",
            "status": "auto_linked",
            "confidence": 0.90_f64,
        })];
        let result = process_link_rows(&link_rows, &HashSet::new());
        assert!(result.is_empty(), "null source_node_id must be skipped");
    }

    /// Test 10: multiple rows — only matching ones returned.
    #[test]
    fn test_multiple_rows_only_matching_returned() {
        let link_rows = vec![
            serde_json::json!({"source_node_id": "entity:contractor-a", "target_node_id": "icij:1", "status": "auto_linked", "confidence": 0.90_f64}),
            serde_json::json!({"source_node_id": "entity:not-contractor", "target_node_id": "icij:2", "status": "auto_linked", "confidence": 0.88_f64}),
            serde_json::json!({"source_node_id": "entity:contractor-b", "target_node_id": "icij:3", "status": "confirmed", "confidence": 0.95_f64}),
            serde_json::json!({"source_node_id": "entity:contractor-a", "target_node_id": "icij:4", "status": "candidate", "confidence": 0.72_f64}),
        ];
        let mut contract_entity_ids = HashSet::new();
        contract_entity_ids.insert("entity:contractor-a".to_string());
        contract_entity_ids.insert("entity:contractor-b".to_string());

        let result = process_link_rows(&link_rows, &contract_entity_ids);
        assert_eq!(result.len(), 2, "should return contractor-a auto_linked + contractor-b confirmed, not the non-contractor or the candidate");
        let ids: Vec<&str> = result.iter().map(|c| c.entity_id.as_str()).collect();
        assert!(ids.contains(&"entity:contractor-a"));
        assert!(ids.contains(&"entity:contractor-b"));
    }
}
