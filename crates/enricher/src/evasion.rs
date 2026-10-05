//! Disqualified Director Evasion CRITICAL detector (CHA-04).
//!
//! Queries `identity_links` for all confirmed/auto-linked disqualified-director
//! records, checks whether the linked officer is currently Active AND inside the
//! ban window, and fires a CRITICAL `graph_events / MotifDetected` alert when so.
//!
//! This detector bypasses the T-Digest P_c scoring path entirely — a confirmed
//! disqualified-director evasion is CRITICAL regardless of the P_c score.
//!
//! # Alert sink (R-02)
//!
//! Alerts are written as `graph_events` rows with `kind='MotifDetected'` using the
//! SAME write path as the main WAL writer (`CREATE graph_events CONTENT $r`).
//! There is NO `alert_records` table — using one would violate R-02 and break the
//! existing `alerts`, `entity`, and `verify` read paths (which query `graph_events`).
//!
//! # Dedup (T-17-03-05)
//!
//! An idempotency key is derived from `officer_id + case_number` and stored in the
//! `result_json` as `motif_hash`. Before writing, the detector checks whether a
//! `MotifDetected` row with the same `motif_hash` already exists; if so, it skips
//! the write. This prevents duplicate alerts on re-runs.
//!
//! A dedup hit is a **re-confirmation**, not a non-event: the detector evaluated the condition
//! and found it still true. Phase 46's staleness display (D-05/D-06) reads `last_seen_run_id`,
//! so the hit advances that field on the existing row via [`advance_last_seen_run_id`] rather
//! than returning silently. Skipping it entirely is the WR-01 defect from the phase 46 review — see that
//! function's own doc for why a frozen `last_seen_run_id` makes an alert re-confirmed on every
//! run read as "Last confirmed N runs ago" with N climbing forever.
//!
//! # ENR-11 negative-test seam
//!
//! The status filter (`status IN ['auto_linked','confirmed']`) is the gate that
//! prevents candidate links (confidence < 0.85) from ever reaching this detector.
//! The `evasion_candidate_suppressed` test depends on this filter being correct.
//!
//! # Security
//!
//! T-17-03-02: SurrealQL injection — all queries use bound parameters; stream data
//! (officer_id, case_number) is never string-formatted into SurrealQL.
//! T-17-03-03: Candidate link elevation — `status IN ['auto_linked','confirmed']`
//! filter prevents sub-threshold links from triggering CRITICAL alerts (ENR-11).
//! T-17-03-05: Duplicate alerts on re-run — motif_hash dedup key prevents double-fire.

use chrono::Utc;
use surrealdb::{engine::any::Any, Surreal};
use tracing::{debug, info, warn};
use urban_integrity_core::persistence::{queries, scan};

/// Run the Disqualified Director Evasion detector.
///
/// Queries all `auto_linked` or `confirmed` disqualified-director identity links
/// (ENR-11 status gate), loads the officer's current `EntityStatus` and the
/// `DisqualifiedFrom` edge window, and fires a CRITICAL `graph_events / MotifDetected`
/// alert for each active officer whose ban window contains today.
///
/// Returns the number of new alerts written.
///
/// # Alert sink (R-02)
///
/// Writes directly to `graph_events` via `CREATE graph_events CONTENT $r`.
/// Does NOT use a separate `alert_records` table.
///
/// # ENR-11 status filter (negative-test seam)
/// SELECT ... WHERE source = 'ch_disqualified' AND status IN ['auto_linked','confirmed']
pub async fn run_evasion_detector(db: &Surreal<Any>, run_id: &str) -> anyhow::Result<usize> {
    // ── Step 1: Fetch all confirmed/auto_linked disqualified identity links ──
    //
    // status IN ['auto_linked','confirmed'] is the ENR-11 gate.
    // Candidate links (status='candidate') are excluded here — they never reach
    // the alert path, satisfying the negative test (evasion_candidate_suppressed).
    let link_rows: Vec<serde_json::Value> = db
        .query(
            "SELECT source_node_id, target_node_id, evidence_json \
             FROM identity_links \
             WHERE source = $source \
               AND status IN [$auto_linked, $confirmed]",
        )
        .bind(("source", "ch_disqualified"))
        .bind(("auto_linked", "auto_linked"))
        .bind(("confirmed", "confirmed"))
        .await?
        .take(0)?;

    if link_rows.is_empty() {
        debug!("Evasion detector: no confirmed/auto_linked ch_disqualified links found");
        return Ok(0);
    }

    let today = Utc::now();
    let mut alerts_written = 0usize;
    // Loaded once, not per candidate — see AlertedHashes.
    let mut alerted = AlertedHashes::load(db).await;

    for link_row in &link_rows {
        // source_node_id is the disq record ("disq:officer-id-001")
        // target_node_id is the officer ("director:john-smith")
        let officer_id = match link_row["target_node_id"].as_str() {
            Some(id) => id,
            None => {
                warn!("Evasion detector: link row missing target_node_id — skipping");
                continue;
            }
        };
        let disq_node_id = match link_row["source_node_id"].as_str() {
            Some(id) => id,
            None => {
                warn!("Evasion detector: link row missing source_node_id — skipping");
                continue;
            }
        };

        // ── Step 2: Load officer EntityStatus ──────────────────────────────
        // We need to check if the officer is currently Active.
        // Query graph_events for the most recent NodeAdded event for this officer.
        let officer_status = load_officer_status(db, officer_id).await;
        let is_active = matches!(officer_status, OfficerStatus::Active);
        if !is_active {
            debug!(
                officer_id,
                "Evasion detector: officer not Active — skipping"
            );
            continue;
        }

        // ── Step 3: Load the DisqualifiedFrom ban window ────────────────────
        // Query graph_events EdgeAdded events for a DisqualifiedFrom edge where
        // src matches the officer_id and dst matches the disq_node_id.
        let ban_window = load_ban_window(db, officer_id, disq_node_id).await;
        let (disq_from, disq_until, case_number) = match ban_window {
            Some(w) => w,
            None => {
                debug!(
                    officer_id,
                    disq_node_id, "Evasion detector: no DisqualifiedFrom edge found — skipping"
                );
                continue;
            }
        };

        // ── Step 4: Check if today falls within [disqualified_from, disqualified_until] ──
        let in_window = today >= disq_from && today <= disq_until;
        if !in_window {
            debug!(
                officer_id,
                "Evasion detector: officer outside ban window — skipping"
            );
            continue;
        }

        // ── Step 5: Dedup check (T-17-03-05) ───────────────────────────────
        // Compute a deterministic hash of officer_id+case_number to avoid
        // duplicate alerts on re-runs.
        let motif_hash = compute_motif_hash(officer_id, case_number.as_deref().unwrap_or(""));
        if alerted.contains(&motif_hash) {
            // Still true this run — re-confirm rather than fall silent (WR-01). See
            // `advance_last_seen_run_id`.
            if let Some(record_id) = alerted.record_id(&motif_hash) {
                advance_last_seen_run_id(db, record_id, run_id).await;
            }
            debug!(
                officer_id,
                motif_hash, "Evasion detector: alert already exists (dedup) — re-confirmed"
            );
            continue;
        }

        // ── Step 6: Write the CRITICAL graph_events MotifDetected alert ────
        // result_json must satisfy parse_alert_row (queries.rs:1131):
        //   result["motif_type"]: string
        //   result["risk_score"]: f64 = 1.0 (forces CRITICAL via classify_risk >= 0.9)
        //   result["motif_hash"]: string (dedup key)
        let description = format!(
            "Active director is subject to a live disqualification order under CDDA 1986{}.",
            case_number
                .as_deref()
                .map(|c| format!(" (case: {c})"))
                .unwrap_or_default()
        );
        let result_json =
            build_result_json(&description, officer_id, disq_node_id, &motif_hash, run_id);

        let result_json_str = result_json.to_string();

        // Build an EventRecord that mirrors what the WAL writer sends (surreal.rs:109).
        // CREATE graph_events CONTENT $r — same path as main.rs:154.
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
                alerted.insert(motif_hash.clone());
                info!(
                    officer_id,
                    motif_hash,
                    "Evasion detector: CRITICAL alert written — disqualified_director_evasion"
                );
                alerts_written += 1;
            }
            Err(e) => {
                warn!(
                    error = %e,
                    officer_id,
                    "Evasion detector: failed to write alert (non-fatal)"
                );
            }
        }
    }

    Ok(alerts_written)
}

// ── Officer status helper ────────────────────────────────────────────────────

/// Simplified officer status for the evasion detector.
#[derive(Debug, PartialEq)]
enum OfficerStatus {
    Active,
    NotActive, // Dissolved, InLiquidation, or not found
}

/// Load an officer's current status from graph_events NodeAdded rows.
///
/// Returns `Active` if the most recent NodeAdded event for this officer contains
/// EntityStatus::Active (or is missing a status field — defaults to Active per
/// the schema's `#[serde(default)]`).
///
/// # SurrealDB 3.x RecordId coercion workaround
///
/// `payload.entity_id` is coerced to a SurrealDB `RecordId` when it matches the
/// `table:key` pattern (e.g. `director:john-smith`). Equality/CONTAINS comparisons
/// on a RecordId field via a bound string parameter fail silently. We work around
/// this by fetching all `NodeAdded` events for Officer/Entity nodes and filtering
/// Rust-side by entity_id, mirroring the approach in `queries.rs:query_entity_by_id`.
async fn load_officer_status(db: &Surreal<Any>, officer_id: &str) -> OfficerStatus {
    // Both node-weight kinds, then the shared fold. This scanned `NodeAdded` alone, so a
    // director whose dissolution arrived as a `NodeUpdated` correction still read Active and
    // the gate at the call site never suppressed the CRITICAL alert — a false accusation
    // against a named individual (T-23), on the live 2,477-dissolved case that motivated the
    // fold. We accept Entity as well as Officer: planning-portal directors use that type.
    let mut rows = Vec::new();
    for marker in ["\"type\":\"Officer\"", "\"type\":\"Entity\""] {
        match scan::scan_events_of_kinds_by_type(db, queries::NODE_WEIGHT_KINDS, marker).await {
            Ok(found) => rows.extend(found),
            Err(e) => {
                warn!(error = %e, officer_id, "Evasion detector: failed to load officer status");
                return OfficerStatus::NotActive;
            }
        }
    }

    let folded = queries::fold_latest_node_rows(rows);

    // Exact id match after RecordId coercion. The previous arm was
    // `eid == officer_id || row[..].to_string().contains(officer_id)`, so `director:john`
    // substring-matched `director:john-smith`'s row and `.find()` returned whichever row came
    // first — another person's status, from a fuzzy identifier join of exactly the kind the
    // exact-match-only policy (2026-06-13) removed everywhere else.
    let matched = folded.into_iter().find(|row| {
        queries::extract_id_string(&row["payload"]["entity_id"]).is_some_and(|id| id == officer_id)
    });

    let row = match matched {
        Some(r) => r,
        None => {
            debug!(
                officer_id,
                "Evasion detector: officer node not found in graph_events"
            );
            // Not in the graph at all — NotActive, so the caller skips. The alert is
            // CRITICAL and names an individual, so an officer we cannot verify does not get
            // one. (This comment used to open by claiming the opposite default; the code has
            // always returned NotActive.)
            return OfficerStatus::NotActive;
        }
    };

    // Parse weight_json to extract EntityStatus.
    let weight_json = match row["payload"]["weight_json"].as_str() {
        Some(s) => s,
        None => return OfficerStatus::Active, // missing field defaults to Active
    };

    // Officer nodes carry no status at all (`OfficerData` has no such field); planning-portal
    // directors are Entity nodes and do. `None` therefore means "no status concept here",
    // which keeps this function's long-standing default of Active.
    //
    // The hand-rolled path this replaces (`parsed["data"]["status"]["type"]`, defaulting to
    // "Active") matched nothing: `NodeWeight` is serde(tag="type") with no `data` key, and the
    // date sits under `content`, not `date` as the old comment claimed.
    match crate::node_status::entity_is_active(weight_json) {
        Some(false) => OfficerStatus::NotActive,
        _ => OfficerStatus::Active,
    }
}

// ── Ban window loader ────────────────────────────────────────────────────────

/// Load the DisqualifiedFrom ban window for a given officer → disq edge.
///
/// Returns `Some((disq_from, disq_until, case_number))` if found, `None` otherwise.
async fn load_ban_window(
    db: &Surreal<Any>,
    officer_id: &str,
    disq_node_id: &str,
) -> Option<(chrono::DateTime<Utc>, chrono::DateTime<Utc>, Option<String>)> {
    // SurrealDB 3.x RecordId coercion workaround: payload.src_id and payload.dst_id
    // are coerced to RecordId when they match the `table:key` pattern. Equality and
    // CONTAINS comparisons fail silently. Workaround: fetch all EdgeAdded events for
    // DisqualifiedFrom edges and filter Rust-side by src_id and dst_id.
    let results = scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"DisqualifiedFrom\"").await;

    let rows = match results {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "Evasion detector: failed to load ban window edge");
            return None;
        }
    };

    // Filter Rust-side: find the row whose src_id and dst_id match.
    let row = rows.into_iter().find(|row| {
        let src = row["payload"]["src_id"].as_str().unwrap_or("");
        let dst = row["payload"]["dst_id"].as_str().unwrap_or("");
        // Also check the JSON representation in case SurrealDB returned RecordId
        let src_json = row["payload"]["src_id"].to_string();
        let dst_json = row["payload"]["dst_id"].to_string();
        (src == officer_id || src_json.contains(officer_id))
            && (dst == disq_node_id || dst_json.contains(disq_node_id))
    })?;
    let weight_json_str = row["payload"]["weight_json"].as_str()?;
    let weight: serde_json::Value = serde_json::from_str(weight_json_str).ok()?;

    // EdgeWeight::DisqualifiedFrom serialises as {"type":"DisqualifiedFrom","meta":{...},"data":{...}}
    if weight["type"].as_str()? != "DisqualifiedFrom" {
        return None;
    }

    let data = &weight["data"];
    let disq_from_str = data["disqualified_from"].as_str()?;
    let disq_until_str = data["disqualified_until"].as_str()?;

    let disq_from = chrono::DateTime::parse_from_rfc3339(disq_from_str)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))?;
    let disq_until = chrono::DateTime::parse_from_rfc3339(disq_until_str)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))?;

    let case_number = data["case_number"].as_str().map(|s| s.to_string());

    Some((disq_from, disq_until, case_number))
}

// ── Dedup helpers ────────────────────────────────────────────────────────────

/// Compute a deterministic dedup key from officer_id + case_number (T-17-03-05).
///
/// Returns the first 32 hex characters (128 bits) of SHA-256(`officer_id::case_number`).
/// Using a cryptographic hash rather than raw hex-encoding prevents:
///   1. Key length growing linearly with input size (inflating graph_events rows).
///   2. Separator-injection collisions — two different input pairs can produce the
///      same hex-encoded string if one of the values embeds the `::` separator.
///
/// SHA-256 is stable across Rust toolchain versions (unlike std HashMap).
pub fn compute_motif_hash(officer_id: &str, case_number: &str) -> String {
    use sha2::{Digest, Sha256};
    let input = format!("{}::{}", officer_id, case_number);
    let digest = Sha256::digest(input.as_bytes());
    // 32 hex chars = 16 bytes = 128 bits; collision-resistant for dedup use.
    // Format each byte individually — [u8] does not implement LowerHex directly.
    digest[..16]
        .iter()
        .fold(String::with_capacity(32), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{:02x}", b);
            acc
        })
}

/// Build the `result_json` literal for a Disqualified Director Evasion alert (46-05).
///
/// Pure — no DB access — so the exact shape (in particular `participants`) is
/// host-only unit-testable without `URBAN_DB_TESTS=1`. `participants` names the pair
/// (phase 46 assumption A-05: Structural), officer then disq record, in that order.
fn build_result_json(
    description: &str,
    officer_id: &str,
    disq_node_id: &str,
    motif_hash: &str,
    run_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "motif_type": "disqualified_director_evasion",
        "risk_score": 1.0_f64,
        "description": description,
        "officer_id": officer_id,
        "disq_node_id": disq_node_id,
        "motif_hash": motif_hash,
        "run_id": run_id,
        "first_seen_run_id": run_id,
        "last_seen_run_id": run_id,
        "participants": [officer_id, disq_node_id],
    })
}

/// The `motif_hash` of every alert already in `graph_events`.
///
/// Load this **once per detector run** and test candidates against it with
/// [`AlertedHashes::contains`]. The per-candidate query it replaces was a `LIMIT 1` existence
/// check, which the planner bug could answer "no" for a hash that is present — turning "already
/// emitted?" into "emit it again". Duplicate alerts are harder to spot than missing ones, because
/// nothing looks empty.
///
/// Hoisting the scan also keeps the cost linear. Checking per candidate means one full
/// `MotifDetected` scan per candidate; at the current 1,430 alerts that is a single page either
/// way, but the shape is O(candidates × alerts) and degrades badly as alerts accumulate.
#[derive(Debug, Default)]
pub(crate) struct AlertedHashes(std::collections::HashMap<String, Option<String>>);

impl AlertedHashes {
    /// Read every existing alert's `motif_hash`, keeping the `graph_events` record id that
    /// carries it so a re-detection can advance that row's `last_seen_run_id`
    /// ([`advance_last_seen_run_id`]) instead of vanishing into a bare `continue`.
    ///
    /// A load failure yields an **empty** set, which re-emits rather than suppresses. That is the
    /// safe direction: a duplicate alert is visible and can be dropped, whereas a suppressed one
    /// is silently absent.
    pub(crate) async fn load(db: &Surreal<Any>) -> Self {
        let rows = scan::scan_events_by_kind(db, "MotifDetected")
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "alert dedup: failed to load existing motif hashes — \
                                   candidates may re-emit");
                Vec::new()
            });
        Self(
            rows.iter()
                .filter_map(|r| {
                    let hash = extract_motif_hash(r["payload"]["result_json"].as_str()?)?;
                    Some((hash, r["id"].as_str().map(str::to_owned)))
                })
                .collect(),
        )
    }

    pub(crate) fn contains(&self, motif_hash: &str) -> bool {
        self.0.contains_key(motif_hash)
    }

    /// The `graph_events` record id of the row already carrying `motif_hash`, when there is one
    /// to advance. `None` for a hash written during THIS run — its `last_seen_run_id` is already
    /// the current run, so there is nothing to advance — and `None` for a row whose `id` could
    /// not be read.
    pub(crate) fn record_id(&self, motif_hash: &str) -> Option<&str> {
        self.0.get(motif_hash).and_then(|id| id.as_deref())
    }

    /// Record a hash written during this run.
    ///
    /// `load` reads the set once, before the write loop. Without this, a key produced twice
    /// inside one run is deduped only on the *next* run — the opposite of what T-17-03-05 asks
    /// for. Insert on success only, so a failed write can still be retried.
    pub(crate) fn insert(&mut self, motif_hash: String) {
        self.0.insert(motif_hash, None);
    }

    #[cfg(test)]
    pub(crate) fn from_hashes<I: IntoIterator<Item = String>>(hashes: I) -> Self {
        Self(hashes.into_iter().map(|h| (h, None)).collect())
    }

    #[cfg(test)]
    pub(crate) fn from_pairs<I: IntoIterator<Item = (String, String)>>(pairs: I) -> Self {
        Self(pairs.into_iter().map(|(h, id)| (h, Some(id))).collect())
    }
}

/// The key half of a `graph_events` record id, for `type::record('graph_events', $id)`.
///
/// A scan returns `id` as the full `"graph_events:key"` string; `type::record`'s second argument
/// is the key alone, so passing the full string through would address
/// `graph_events:⟨graph_events:key⟩` and silently match nothing. A value with no prefix is
/// already a bare key and is returned unchanged.
pub(crate) fn graph_event_key(record_id: &str) -> &str {
    record_id
        .strip_prefix("graph_events:")
        .unwrap_or(record_id)
        .trim_matches('\u{2068}')
}

/// `result_json` with `last_seen_run_id` advanced to `run_id`, leaving `first_seen_run_id`
/// untouched. `None` when the blob does not parse as a JSON object — a row this detector cannot
/// safely rewrite is left exactly as it is.
///
/// PROVE THIS CAN FAIL: also assign `current["first_seen_run_id"]` here and
/// `advancing_last_seen_preserves_first_seen` fails.
pub(crate) fn with_last_seen_run_id(result_json: &str, run_id: &str) -> Option<String> {
    let mut current: serde_json::Value = serde_json::from_str(result_json).ok()?;
    if !current.is_object() {
        return None;
    }
    current["last_seen_run_id"] = serde_json::json!(run_id);
    Some(current.to_string())
}

/// Re-confirm an alert this run detected again but did not re-write: advance the existing row's
/// `last_seen_run_id` to `run_id`.
///
/// # Why a dedup hit is a re-confirmation, not a non-event
///
/// These post-adapter detectors `CREATE`-once and dedup on `motif_hash` forever (T-17-03-05).
/// A dedup hit therefore means the detector evaluated the condition and *found it still true* —
/// which is precisely what `last_seen_run_id` records. Before Phase 46's staleness display
/// (D-05/D-06) that distinction cost nothing, because nothing read the field. It does now:
/// `runs_since_last_seen` counts every run of this motif type since `last_seen_run_id`, so a
/// frozen value makes an alert the detector re-confirms on every single run render as
/// "Last confirmed N runs ago" with N climbing forever. Skipping the write here is what the
/// phase 46 code review's WR-01 finding is about.
///
/// Failure is warned and swallowed — advancing staleness metadata must never abort a detector
/// run — but it is never silent, and the write's `Result` is consumed rather than discarded
/// (the ICIJ failure shape).
pub(crate) async fn advance_last_seen_run_id(db: &Surreal<Any>, record_id: &str, run_id: &str) {
    let key = graph_event_key(record_id);
    let existing: surrealdb::Result<Vec<serde_json::Value>> = db
        .query("SELECT payload.result_json AS result_json FROM type::record('graph_events', $id)")
        .bind(("id", key.to_string()))
        .await
        .and_then(|mut resp| resp.take(0));
    let current = match existing {
        Ok(rows) => rows
            .into_iter()
            .next()
            .and_then(|row| row["result_json"].as_str().map(str::to_owned)),
        Err(e) => {
            warn!(error = %e, record_id, "last_seen_run_id: read-before-write failed — \
                                          staleness for this alert stays at its previous run");
            return;
        }
    };
    let Some(current) = current else {
        warn!(
            record_id,
            "last_seen_run_id: existing alert has no result_json — not advancing"
        );
        return;
    };
    let Some(updated) = with_last_seen_run_id(&current, run_id) else {
        warn!(
            record_id,
            "last_seen_run_id: existing result_json is not a JSON object — not advancing"
        );
        return;
    };
    let write: surrealdb::Result<Vec<serde_json::Value>> = db
        .query(
            "UPDATE type::record('graph_events', $id) \
             SET payload.result_json = $result_json",
        )
        .bind(("id", key.to_string()))
        .bind(("result_json", updated))
        .await
        .and_then(|mut resp| resp.take(0));
    match write {
        Ok(_) => debug!(
            record_id,
            run_id, "last_seen_run_id advanced — alert re-confirmed by this run"
        ),
        Err(e) => warn!(error = %e, record_id, "last_seen_run_id: write failed (non-fatal)"),
    }
}

/// The `motif_hash` value out of a `result_json` blob.
///
/// Parsed rather than substring-matched: `result_json CONTAINS "<hash>"` would also match a hash
/// that merely appears inside some other field.
fn extract_motif_hash(result_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(result_json)
        .ok()?
        .get("motif_hash")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_motif_hash_is_deterministic() {
        let h1 = compute_motif_hash("director:john-smith", "INS-2023-00123");
        let h2 = compute_motif_hash("director:john-smith", "INS-2023-00123");
        assert_eq!(h1, h2);
    }

    /// 46-05 Task 1: `result_json` carries run attribution and a 2-element
    /// `participants` array (phase 46 assumption A-05: Structural), officer then disq record.
    #[test]
    fn result_json_carries_run_attribution_and_participants() {
        let result_json = build_result_json(
            "Active director is subject to a live disqualification order under CDDA 1986.",
            "director:john-smith",
            "disq:officer-id-001",
            "deadbeef",
            "run-x",
        );
        assert_eq!(result_json["run_id"], "run-x");
        assert_eq!(result_json["first_seen_run_id"], "run-x");
        assert_eq!(result_json["last_seen_run_id"], "run-x");
        assert_eq!(
            result_json["participants"],
            serde_json::json!(["director:john-smith", "disq:officer-id-001"])
        );
    }

    /// WR-01 (phase 46 code review): a dedup hit must advance `last_seen_run_id` and leave
    /// `first_seen_run_id` alone. "First seen" is a historical fact and must never move;
    /// "last seen" is the whole staleness signal and must.
    ///
    /// PROVE THIS CAN FAIL: assign `first_seen_run_id` in [`with_last_seen_run_id`] too, and the
    /// first assertion here fails.
    #[test]
    fn advancing_last_seen_preserves_first_seen() {
        let original = serde_json::json!({
            "motif_type": "disqualified_director_evasion",
            "motif_hash": "abc123",
            "first_seen_run_id": "run-001",
            "last_seen_run_id": "run-001",
            "participants": ["director:x", "disq:y"],
        })
        .to_string();

        let advanced = with_last_seen_run_id(&original, "run-009").expect("a JSON object advances");
        let v: serde_json::Value = serde_json::from_str(&advanced).unwrap();

        assert_eq!(
            v["first_seen_run_id"], "run-001",
            "first_seen_run_id is a historical fact — re-confirmation must not move it"
        );
        assert_eq!(
            v["last_seen_run_id"], "run-009",
            "last_seen_run_id must advance to the run that re-confirmed the alert"
        );
        // Non-vacuity: everything else survives the rewrite untouched.
        assert_eq!(v["motif_hash"], "abc123");
        assert_eq!(v["participants"][1], "disq:y");
    }

    #[test]
    fn a_result_json_that_is_not_an_object_is_left_alone() {
        assert!(with_last_seen_run_id("not json at all", "run-2").is_none());
        assert!(
            with_last_seen_run_id("[1,2,3]", "run-2").is_none(),
            "a JSON array is valid JSON but not a rewritable alert blob"
        );
    }

    /// `type::record('graph_events', $id)` takes the key alone. Passing the scan's full
    /// `"graph_events:key"` string would address `graph_events:⟨graph_events:key⟩` and match
    /// nothing — silently, which is the failure mode this project keeps hitting.
    #[test]
    fn graph_event_key_strips_the_table_prefix_exactly_once() {
        assert_eq!(graph_event_key("graph_events:00uivqsamzx6"), "00uivqsamzx6");
        assert_eq!(
            graph_event_key("00uivqsamzx6"),
            "00uivqsamzx6",
            "a bare key is already in the form type::record wants"
        );
    }

    /// A hash loaded from the DB can be advanced; a hash written during THIS run cannot, and
    /// must not be — its `last_seen_run_id` is already the current run.
    #[test]
    fn only_a_preexisting_row_offers_a_record_id_to_advance() {
        let loaded =
            AlertedHashes::from_pairs([("hash-old".to_string(), "graph_events:abc".to_string())]);
        assert_eq!(loaded.record_id("hash-old"), Some("graph_events:abc"));

        let mut within_run = AlertedHashes::default();
        within_run.insert("hash-new".to_string());
        assert!(
            within_run.contains("hash-new"),
            "a hash written this run still suppresses a second write"
        );
        assert_eq!(
            within_run.record_id("hash-new"),
            None,
            "but it offers nothing to advance — it was just written at the current run"
        );
    }

    #[test]
    fn an_already_alerted_hash_suppresses_a_re_emit() {
        let h = compute_motif_hash("director:john-smith", "INS-2023-00123");
        let alerted = AlertedHashes::from_hashes([h.clone()]);
        assert!(alerted.contains(&h));
        assert!(!alerted.contains("some-other-hash"));
    }

    #[test]
    fn an_empty_set_re_emits_rather_than_suppresses() {
        // The failure direction matters: a load error yields an empty set, so candidates
        // re-emit. A duplicate alert is visible and can be dropped; a suppressed one is
        // silently absent.
        let alerted = AlertedHashes::default();
        assert!(!alerted.contains(&compute_motif_hash("director:x", "CASE-1")));
    }

    #[test]
    fn a_motif_hash_is_read_from_the_json_field_not_matched_as_a_substring() {
        let hash = "abc123";
        assert_eq!(
            extract_motif_hash(&format!("{{\"motif_hash\":\"{hash}\"}}")).as_deref(),
            Some(hash)
        );
        // The hash appearing inside some other field is NOT a prior alert for it. The
        // `result_json CONTAINS "<hash>"` form this replaces could not tell the difference.
        assert_eq!(
            extract_motif_hash(&format!("{{\"description\":\"see {hash}\"}}")),
            None
        );
        assert_eq!(extract_motif_hash("not json at all"), None);
    }

    #[test]
    fn compute_motif_hash_differs_for_different_inputs() {
        let h1 = compute_motif_hash("director:john-smith", "INS-2023-00123");
        let h2 = compute_motif_hash("director:jane-doe", "INS-2023-00123");
        assert_ne!(h1, h2);
    }

    #[test]
    fn compute_motif_hash_is_hex_string() {
        let h = compute_motif_hash("director:john-smith", "INS-2023-00123");
        assert!(
            !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()),
            "motif_hash must be a non-empty hex string"
        );
    }

    #[test]
    fn compute_motif_hash_is_pinned() {
        // Pin the exact SHA-256 value (first 32 hex chars) so a toolchain change
        // or accidental re-implementation is caught at CI.
        // Value = SHA-256("director:john-smith::INS-2023-00123")[..16 bytes] as hex.
        // Verified: echo -n "director:john-smith::INS-2023-00123" | sha256sum | head -c 32
        let expected = "776fc3926e2005bf526e3fb75154c08e";
        assert_eq!(
            compute_motif_hash("director:john-smith", "INS-2023-00123"),
            expected,
            "Hash value changed — dedup keys in graph_events will become stale after deploy"
        );
    }
}
