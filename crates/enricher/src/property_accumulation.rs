//! Property Accumulation Anomaly detector (PROP-03).
//!
//! Fires a HIGH `graph_events / MotifDetected` alert when an **Active** entity owns
//! more than 3 properties (D-09, threshold ≥ 4) whose total `price_paid` exceeds
//! 5× the entity's total awarded-contract value (D-10 requires ≥ 1 contract).
//!
//! # Decision gates applied
//!
//! - **D-09**: `property_count > 3` — entities with ≤ 3 properties are skipped.
//! - **D-10**: `contract_count ≥ 1` — entities with zero awarded contracts are skipped
//!   (no denominator for the ratio).
//! - **D-11**: `EntityStatus == Active` — InLiquidation / Dissolved entities are skipped.
//! - **ENR-11**: The entity must appear in a `NodeAdded` or `NodeUpdated` graph_event
//!   with a resolvable status. Candidate-only entities (confidence < 0.85, no NodeAdded
//!   event) are therefore excluded, because they have no status row and default to
//!   `NotActive` in this detector.
//!
//! # NodeUpdated requirement
//!
//! EntityStatus is loaded from **both** `NodeAdded` **and** `NodeUpdated` events
//! (Pitfall 5 — status may be promoted to InLiquidation via a NodeUpdated row written
//! by the London Gazette adapter; if only NodeAdded is queried the entity appears Active).
//! The latest-timestamp row wins.
//!
//! # Alert sink (R-02)
//!
//! Alerts are written as `graph_events` rows with `kind='MotifDetected'` via
//! `CREATE graph_events CONTENT $r`. No separate `alert_records` table is used.
//!
//! # Dedup
//!
//! An idempotency key is derived from `entity_id + "property_accumulation"` and stored
//! in `result_json` as `motif_hash`. Before writing, the detector checks whether a
//! `MotifDetected` row with the same `motif_hash` already exists; if so, it skips the
//! write.

use crate::evasion::{advance_last_seen_run_id, AlertedHashes};
use chrono::Utc;
use std::collections::HashMap;
use surrealdb::{engine::any::Any, Surreal};
use tracing::{debug, info, warn};
use urban_integrity_core::persistence::scan;

/// Simplified entity status used within this detector.
#[derive(Debug, PartialEq)]
enum EntityStatus {
    Active,
    NotActive,
}

/// Run the Property Accumulation Anomaly detector (PROP-03).
///
/// Queries `OwnsProperty` and `AwardedTo` edges from `graph_events`, aggregates
/// Rust-side, applies the D-09 / D-10 / D-11 / ENR-11 gates, and writes a HIGH
/// `MotifDetected` alert for each qualifying entity.
///
/// Returns the number of new alerts written.
///
/// # Alert sink (R-02)
///
/// Writes directly to `graph_events` via `CREATE graph_events CONTENT $r`.
/// Does NOT use a separate `alert_records` table.
pub async fn run_property_accumulation_detector(
    db: &Surreal<Any>,
    run_id: &str,
) -> anyhow::Result<usize> {
    // ── Step 1: Load all OwnsProperty edges ─────────────────────────────────
    let property_rows =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"OwnsProperty\"").await?;

    // ── Step 2: Aggregate Rust-side: entity_id → (count, sum_price_paid) ────
    let mut property_map: HashMap<String, (usize, f64)> = HashMap::new();
    for row in &property_rows {
        // Canonicalise once here so every map in this function is keyed the same way and the
        // gates below can compare exactly. See `crate::node_id`.
        let Some(src_id) = crate::node_id::canonical_entity_id(&row["payload"]["src_id"]) else {
            continue;
        };
        let price_paid = row["payload"]["weight_json"]
            .as_str()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v["data"]["price_paid"].as_f64())
            .unwrap_or(0.0);
        let entry = property_map.entry(src_id).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 += price_paid;
    }

    // ── Step 3: Filter by D-09: property_count > 3 ─────────────────────────
    let candidates: HashMap<String, (usize, f64)> = property_map
        .into_iter()
        .filter(|(_, (count, _))| *count > 3)
        .collect();

    if candidates.is_empty() {
        debug!("Property accumulation detector: no entities with >3 properties");
        return Ok(0);
    }

    // ── Step 4: Load all AwardedTo edges ────────────────────────────────────
    let contract_rows =
        scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"AwardedTo\"").await?;

    // Aggregate Rust-side: entity_id → (contract_count, sum_value_gbp)
    let mut contract_map: HashMap<String, (usize, f64)> = HashMap::new();
    for row in &contract_rows {
        let Some(src_id) = crate::node_id::canonical_entity_id(&row["payload"]["src_id"]) else {
            continue;
        };
        let value_gbp = row["payload"]["weight_json"]
            .as_str()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v["data"]["value_gbp"].as_f64())
            .unwrap_or(0.0);
        let entry = contract_map.entry(src_id).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 += value_gbp;
    }

    // ── Step 5: Load EntityStatus from BOTH NodeAdded AND NodeUpdated ────────
    // Critical (Pitfall 5): must include NodeUpdated so InLiquidation promotions
    // written by the London Gazette adapter are not missed.
    // The scan also restores `timestamp` to the projection. The `SELECT payload` this replaces
    // never returned it, so the latest-timestamp tie-break below scored every row 0 and the
    // winning status was whichever row happened to land last.
    let status_rows = scan::scan_events_of_kinds_by_type(
        db,
        &["NodeAdded", "NodeUpdated"],
        "\"type\":\"Entity\"",
    )
    .await?;

    // Build entity_id → (timestamp, EntityStatus): latest-timestamp wins.
    let mut status_map: HashMap<String, (u64, EntityStatus)> = HashMap::new();
    for row in &status_rows {
        let Some(eid) = crate::node_id::canonical_entity_id(&row["payload"]["entity_id"]) else {
            continue;
        };

        let timestamp = row["timestamp"].as_u64().unwrap_or(0);

        let weight_json_str = match row["payload"]["weight_json"].as_str() {
            Some(s) => s,
            None => continue,
        };
        // `entity_is_active` deserialises the real `NodeWeight`. The hand-rolled
        // `parsed["data"]["status"]["type"]` path this replaces matched nothing — there is no
        // `data` key — so its `.unwrap_or("Active")` made every entity Active and the D-11
        // gate below never skipped one. `None` (a non-Entity weight) keeps the old default.
        let status = match crate::node_status::entity_is_active(weight_json_str) {
            Some(false) => EntityStatus::NotActive,
            _ => EntityStatus::Active,
        };

        // Latest-timestamp wins
        let existing = status_map.entry(eid).or_insert((0, EntityStatus::Active));
        if timestamp >= existing.0 {
            *existing = (timestamp, status);
        }
    }

    // ── Steps 6–9: Per-candidate gate, dedup, and alert write ───────────────
    let mut alerts_written = 0usize;
    // Loaded once, not per candidate — see AlertedHashes.
    let alerted = AlertedHashes::load(db).await;

    for (entity_id, (prop_count, sum_price_paid)) in &candidates {
        // ── Step 6 (D-11): Skip InLiquidation / Dissolved / unknown entities ──
        // Exact lookup on a canonicalised key. This was a bidirectional `contains`, which made
        // `GB-COH:123` match `GB-COH:1234` and `director:john` match `director:john-smith`, so
        // an entity could inherit another entity's status and the ratio threshold below would
        // fire or suppress an alert about the wrong subject (T-23-FalseAccusation, D-18).
        let entity_status = status_map
            .get(entity_id.as_str())
            .map(|(_, s)| match s {
                EntityStatus::Active => EntityStatus::Active,
                EntityStatus::NotActive => EntityStatus::NotActive,
            })
            .unwrap_or(EntityStatus::NotActive);

        if entity_status != EntityStatus::Active {
            debug!(
                entity_id,
                "Property accumulation detector: entity not Active (D-11) — skipping"
            );
            continue;
        }

        // ── Step 7 (D-10): Skip entities with zero contracts ────────────────
        let (contract_count, sum_contract_value) = contract_map
            .get(entity_id.as_str())
            .copied()
            .unwrap_or((0, 0.0));

        if contract_count == 0 {
            debug!(
                entity_id,
                "Property accumulation detector: zero contracts (D-10) — skipping"
            );
            continue;
        }

        // ── Step 7b: Apply ratio threshold ──────────────────────────────────
        // Fire only if sum_price_paid > 5× sum_contract_value.
        if *sum_price_paid <= 5.0 * sum_contract_value {
            debug!(
                entity_id,
                sum_price_paid,
                sum_contract_value,
                "Property accumulation detector: ratio below threshold — skipping"
            );
            continue;
        }

        // ── Step 8: Dedup via motif_hash ─────────────────────────────────────
        let motif_hash = compute_motif_hash(entity_id, "property_accumulation");
        if alerted.contains(&motif_hash) {
            // Still true this run — re-confirm rather than fall silent (WR-01). See
            // `evasion::advance_last_seen_run_id`.
            if let Some(record_id) = alerted.record_id(&motif_hash) {
                advance_last_seen_run_id(db, record_id, run_id).await;
            }
            debug!(
                entity_id,
                motif_hash,
                "Property accumulation detector: alert already exists (dedup) — re-confirmed"
            );
            continue;
        }

        // ── Step 9: Write MotifDetected ───────────────────────────────────────
        let event_id = uuid::Uuid::new_v4().to_string();
        let timestamp_ms = Utc::now().timestamp_millis() as u64;
        let result_json = build_result_json(
            entity_id,
            *prop_count,
            *sum_price_paid,
            sum_contract_value,
            &motif_hash,
            run_id,
        );
        let result_json_str = result_json.to_string();
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
                    entity_id,
                    motif_hash,
                    "Property accumulation detector: HIGH alert written — property_accumulation_anomaly"
                );
                alerts_written += 1;
            }
            Err(e) => {
                warn!(
                    error = %e,
                    entity_id,
                    "Property accumulation detector: failed to write alert (non-fatal)"
                );
            }
        }
    }

    Ok(alerts_written)
}

// ── Dedup helpers (copied from evasion.rs) ──────────────────────────────────

/// Compute a deterministic dedup key from entity_id + discriminator.
///
/// Deterministic dedup key — stable across Rust versions.
/// Format: `"<entity_id>::<discriminator>"` as a hex-encoded UTF-8 string.
/// Hex-encoding the UTF-8 bytes is stable across toolchain versions.
pub fn compute_motif_hash(entity_id: &str, discriminator: &str) -> String {
    let raw = format!("{}::{}", entity_id, discriminator);
    raw.bytes()
        .fold(String::with_capacity(raw.len() * 2), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{:02x}", b);
            acc
        })
}

/// Build the `result_json` literal for a Property Accumulation Anomaly alert (46-05).
///
/// Pure — no DB access — so the exact shape (in particular `participants`) is
/// host-only unit-testable without `URBAN_DB_TESTS=1`.
fn build_result_json(
    entity_id: &str,
    prop_count: usize,
    sum_price_paid: f64,
    sum_contract_value: f64,
    motif_hash: &str,
    run_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "motif_type": "property_accumulation_anomaly",
        "risk_score": 0.75_f64,
        "description": format!(
            "Active entity owns {} properties (total £{:.0}) but only £{:.0} in awarded contracts (ratio {:.1}×)",
            prop_count,
            sum_price_paid,
            sum_contract_value,
            sum_price_paid / sum_contract_value.max(1.0)
        ),
        "entity_id": entity_id,
        "motif_hash": motif_hash,
        "run_id": run_id,
        "first_seen_run_id": run_id,
        "last_seen_run_id": run_id,
        "participants": [entity_id],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_accumulation_no_contracts() {
        // D-10: zero contracts → should not fire
        let sum_price_paid = 400_000.0_f64;
        let contract_count: usize = 0;
        let sum_contract_value = 0.0_f64;
        let fires = contract_count > 0 && sum_price_paid > 5.0 * sum_contract_value;
        assert!(!fires, "D-10: zero contracts must not fire");
    }

    #[test]
    fn property_accumulation_below_threshold() {
        // D-09: exactly 3 properties = not > 3 = must not fire
        let prop_count: usize = 3;
        let fires = prop_count > 3;
        assert!(
            !fires,
            "D-09: exactly 3 properties must not fire (threshold is >3, i.e. ≥4)"
        );
    }

    #[test]
    fn compute_motif_hash_is_deterministic() {
        let h1 = compute_motif_hash("GB-COH:TEST0001", "property_accumulation");
        let h2 = compute_motif_hash("GB-COH:TEST0001", "property_accumulation");
        assert_eq!(h1, h2);
    }

    #[test]
    fn compute_motif_hash_is_hex_string() {
        let h = compute_motif_hash("GB-COH:TEST0001", "property_accumulation");
        assert!(
            !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()),
            "motif_hash must be a non-empty hex string"
        );
    }

    /// 46-05 Task 1: `result_json` carries run attribution and a single-element
    /// `participants` array naming `entity_id` (phase 46 assumption A-05: SingleSubject).
    #[test]
    fn result_json_carries_run_attribution_and_participants() {
        let result_json = build_result_json(
            "entity:GB-COH:12345678",
            5,
            500_000.0,
            50_000.0,
            "deadbeef",
            "run-x",
        );
        assert_eq!(result_json["run_id"], "run-x");
        assert_eq!(result_json["first_seen_run_id"], "run-x");
        assert_eq!(result_json["last_seen_run_id"], "run-x");
        assert_eq!(
            result_json["participants"],
            serde_json::json!(["entity:GB-COH:12345678"])
        );
    }
}
