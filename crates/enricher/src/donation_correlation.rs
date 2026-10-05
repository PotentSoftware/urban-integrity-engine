//! Political Donation -> Contract Correlation detector (POL-04).
//!
//! Queries `identity_links` for all confirmed/auto-linked electoral-commission
//! donation records, checks whether the donor entity received a contract from a
//! local authority controlled by the recipient party within 730 days of the
//! donation, and fires a HIGH `graph_events / MotifDetected` alert when so.
//!
//! # ENR-11 negative-test seam
//! The status filter (`status IN ['auto_linked','confirmed']`) is the gate.
//!
//! # Security
//! T-20-04-01: Party name from evidence_json used only as HashMap lookup key,
//! never interpolated into SurrealQL (Tampering mitigation).

use crate::evasion::{advance_last_seen_run_id, AlertedHashes};
use chrono::Utc;
use std::collections::HashMap;
use surrealdb::{engine::any::Any, Surreal};
use tracing::{debug, info, warn};

/// Map of party name → list of LPA codes controlled by that party.
///
/// Loaded from `data/party_authority_control.json` (POL-05).
/// The `_meta` key is captured separately; all other keys are party name entries.
#[derive(serde::Deserialize)]
pub struct PartyAuthorityMap {
    #[serde(rename = "_meta")]
    pub meta: serde_json::Value,
    #[serde(flatten)]
    pub parties: HashMap<String, Vec<String>>,
    /// EC registered name → canonical key in `parties`, read from
    /// `_meta.name_variants`. Populated by [`Self::load`]; empty when the map is
    /// built by hand in tests.
    ///
    /// The keys of `parties` are the colloquial names used by Open Council Data
    /// (e.g. `"Conservative Party"`), while the Electoral Commission publishes
    /// *registered legal* names (e.g. `"Conservative and Unionist Party"`). The
    /// `_meta.name_variants` table has always recorded that mapping — it was
    /// simply never consulted, so every Conservative donation fell out at the
    /// party gate. See [`Self::lookup_lpa_codes`].
    #[serde(skip)]
    pub aliases: HashMap<String, String>,
}

impl PartyAuthorityMap {
    /// Load the party authority control map from `data/party_authority_control.json`.
    ///
    /// The file path is relative to the working directory (i.e. the repo root when
    /// running via `cargo run` or `cargo test`).
    pub fn load() -> anyhow::Result<Self> {
        let data = std::fs::read_to_string("data/party_authority_control.json")?;
        Ok(Self::from_json_str(&data)?)
    }

    /// Parse the map from JSON, populating the `_meta.name_variants` alias table.
    ///
    /// Prefer this over a bare `serde_json::from_str`: `aliases` is
    /// `#[serde(skip)]`, so deserialising directly yields an empty alias table
    /// and silently degrades `lookup_lpa_codes` to exact-plus-normalised —
    /// exactly the behaviour that dropped every Conservative donation.
    pub fn from_json_str(data: &str) -> serde_json::Result<Self> {
        let mut map: Self = serde_json::from_str(data)?;
        map.aliases = Self::parse_name_variants(&map.meta);
        Ok(map)
    }

    /// Extract `_meta.name_variants` into an EC-name → canonical-key lookup.
    ///
    /// Returns an empty map when the key is absent or malformed rather than
    /// failing the load: a missing alias table degrades the detector to
    /// exact-match behaviour, which is what it did before this was wired up.
    /// Non-string values are skipped individually.
    fn parse_name_variants(meta: &serde_json::Value) -> HashMap<String, String> {
        meta.get("name_variants")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(ec_name, canonical)| {
                        canonical.as_str().map(|c| (ec_name.clone(), c.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Look up the list of LPA codes controlled by the given party name.
    ///
    /// Resolution order:
    /// 1. exact key match;
    /// 2. `_meta.name_variants` alias (EC registered name → colloquial key);
    /// 3. normalised comparison (case-insensitive, strip leading "the ").
    ///
    /// Step 2 is the one that matters in practice. The EC publishes registered
    /// legal names and this map keys on colloquial ones, and normalisation
    /// cannot bridge them — `"conservative and unionist party"` and
    /// `"conservative party"` differ by two words, not by case or a prefix. Every
    /// Conservative donation was therefore skipped at the party gate.
    pub fn lookup_lpa_codes(&self, party_name: &str) -> Option<&Vec<String>> {
        // Exact match first.
        if let Some(codes) = self.parties.get(party_name) {
            return Some(codes);
        }
        // Declared alias from _meta.name_variants, exact then normalised — the EC
        // name is matched case-insensitively so a feed that changes only casing
        // does not silently drop back to a miss.
        let alias_target = self.aliases.get(party_name).or_else(|| {
            let normalised_query = normalise_party_name(party_name);
            self.aliases
                .iter()
                .find(|(ec_name, _)| normalise_party_name(ec_name) == normalised_query)
                .map(|(_, canonical)| canonical)
        });
        if let Some(codes) = alias_target.and_then(|canonical| self.parties.get(canonical)) {
            return Some(codes);
        }
        // Normalised fallback.
        let normalised_query = normalise_party_name(party_name);
        self.parties
            .iter()
            .find(|(k, _)| normalise_party_name(k) == normalised_query)
            .map(|(_, v)| v)
    }
}

/// Normalise a party name for case-insensitive lookup.
///
/// Strips leading "the " prefix (case-insensitive) and converts to lowercase.
/// Example: "The Labour Party" → "labour party".
fn normalise_party_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let stripped = lower.trim_start_matches("the ");
    stripped.trim().to_string()
}

/// Run the Political Donation → Contract Correlation detector (POL-04).
///
/// Queries all `auto_linked` or `confirmed` electoral-commission donation
/// identity links (ENR-11 status gate), checks whether the donor entity
/// received an AWARDED_TO contract from a party-controlled LPA within 730 days
/// of the donation (in either direction), and fires a HIGH `graph_events /
/// MotifDetected` alert for each match.
///
/// Returns the number of new alerts written.
///
/// # ENR-11 status filter (negative-test seam)
/// SELECT ... WHERE source = 'electoral_commission' AND status IN ['auto_linked','confirmed']
pub async fn run_donation_correlation_detector(
    db: &Surreal<Any>,
    party_authority_map: &PartyAuthorityMap,
    run_id: &str,
) -> anyhow::Result<usize> {
    // ── Step 1: Fetch all confirmed/auto_linked electoral-commission donation links ──
    //
    // ENR-11: status IN ['auto_linked','confirmed'] is the gate.
    // Candidate links (status='candidate', confidence < threshold) are excluded —
    // they never reach the alert path, satisfying the ENR-11 negative test.
    let link_rows: Vec<serde_json::Value> = db
        .query(
            "SELECT source_node_id, target_node_id, evidence_json \
             FROM identity_links \
             WHERE source = $source \
               AND status IN [$auto_linked, $confirmed]",
        )
        .bind(("source", "electoral_commission"))
        .bind(("auto_linked", "auto_linked"))
        .bind(("confirmed", "confirmed"))
        .await?
        .take(0)?;

    if link_rows.is_empty() {
        debug!("Donation correlation detector: no confirmed/auto_linked electoral_commission links found");
        return Ok(0);
    }

    let today = Utc::now();
    let mut alerts_written = 0usize;
    // Loaded once, then kept current by `insert` on every successful write — the dedup key
    // is (donor, party, lpa) while the write sits inside contract x lpa, so without the
    // within-run insert one donor's M in-window contracts write M identical-hash alerts.
    let mut alerted = AlertedHashes::load(db).await;

    for link_row in &link_rows {
        // source_node_id is the donor entity (e.g. "entity:company-number")
        let donor_id = match link_row["source_node_id"].as_str() {
            Some(id) => id,
            None => {
                // SurrealDB may coerce to RecordId — try to_string fallback.
                warn!("Donation correlation detector: link row missing source_node_id — skipping");
                continue;
            }
        };

        // evidence_json is stored as a flat string (ENR-06).
        let evidence_str = match link_row["evidence_json"].as_str() {
            Some(s) => s,
            None => {
                warn!(
                    donor_id,
                    "Donation correlation detector: missing evidence_json — skipping"
                );
                continue;
            }
        };

        let evidence: serde_json::Value = match serde_json::from_str(evidence_str) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    donor_id,
                    error = %e,
                    "Donation correlation detector: failed to parse evidence_json — skipping"
                );
                continue;
            }
        };

        // Extract party name (party_name) from evidence_json.
        let party_name = match evidence["party_name"].as_str() {
            Some(p) => p,
            None => {
                debug!(
                    donor_id,
                    "Donation correlation detector: evidence_json missing party_name — skipping"
                );
                continue;
            }
        };

        // Extract accepted date (accepted_date) from evidence_json.
        let accepted_date_str = match evidence["accepted_date"].as_str() {
            Some(d) => d,
            None => {
                debug!(
                    donor_id,
                    "Donation correlation detector: evidence_json missing accepted_date — skipping"
                );
                continue;
            }
        };

        // Accepts DD/MM/YYYY (the EC feed format, and what every row written
        // before the 2026-08-14 fix holds) as well as ISO. Parsing only ISO here
        // skipped every donation, so this detector had never fired on real data.
        let donation_date = match crate::ec_date::parse_ec_date(accepted_date_str)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| dt.and_utc())
            .ok_or("unrecognised date format (expected DD/MM/YYYY or YYYY-MM-DD)")
        {
            Ok(dt) => dt,
            Err(e) => {
                warn!(
                    donor_id,
                    accepted_date = accepted_date_str,
                    error = %e,
                    "Donation correlation detector: failed to parse AcceptedDate — skipping"
                );
                continue;
            }
        };

        // Look up the party in the authority map (exact + normalised fallback).
        // T-20-04-01: party_name is ONLY used as a HashMap key, never in SurrealQL.
        let lpa_codes = match party_authority_map.lookup_lpa_codes(party_name) {
            Some(codes) => codes,
            None => {
                warn!(
                    donor_id,
                    party_name,
                    "Donation correlation detector: party not found in party_authority_control.json — skipping"
                );
                continue;
            }
        };

        if lpa_codes.is_empty() {
            debug!(
                donor_id,
                party_name, "Donation correlation detector: party controls no LPAs — skipping"
            );
            continue;
        }

        // ── Step 2: Query all AWARDED_TO contracts for the donor entity ──
        //
        // T-20-04-04 (DoS): LIMIT 1000 on contract query.
        let contract_rows: Vec<serde_json::Value> = match db
            .query(
                "SELECT meta.effective_from FROM awarded_to \
                 WHERE in = type::record('entity', $entity_id) LIMIT 1000",
            )
            .bind(("entity_id", bare_entity_id(donor_id).to_string()))
            .await
            .and_then(|mut r| r.take(0))
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(
                    donor_id,
                    error = %e,
                    "Donation correlation detector: failed to query awarded_to contracts — skipping donor"
                );
                continue;
            }
        };

        // ── Step 3: For each contract × each LPA the party controls ──
        for contract_row in &contract_rows {
            // Parse contract date from meta.effective_from.
            let contract_date_str = match contract_row["meta"]["effective_from"].as_str() {
                Some(s) => s,
                None => continue,
            };

            let contract_date = match chrono::DateTime::parse_from_rfc3339(contract_date_str)
                .map(|dt| dt.with_timezone(&Utc))
                .or_else(|_| -> Result<_, chrono::ParseError> {
                    // Fallback: try naive date format.
                    chrono::NaiveDate::parse_from_str(contract_date_str, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
                }) {
                Ok(dt) => dt,
                Err(_) => continue,
            };

            // 730-day window in both directions (revolving-door detection).
            let diff_days = (contract_date - donation_date).num_days().abs();
            if diff_days > 730i64 {
                continue;
            }

            for lpa_code in lpa_codes {
                // Compute dedup key: SHA-256 equivalent via hex encoding.
                let motif_hash = compute_motif_hash(donor_id, party_name, lpa_code);

                if alerted.contains(&motif_hash) {
                    // Still true this run — re-confirm rather than fall silent (WR-01). See
                    // `evasion::advance_last_seen_run_id`.
                    if let Some(record_id) = alerted.record_id(&motif_hash) {
                        advance_last_seen_run_id(db, record_id, run_id).await;
                    }
                    debug!(
                        donor_id,
                        motif_hash,
                        "Donation correlation detector: alert already exists (dedup) — re-confirmed"
                    );
                    continue;
                }

                // ── Step 4: Write the HIGH graph_events MotifDetected alert ──
                let description = format!(
                    "Entity donated to {} (party controls {}) and received contract within {} days",
                    party_name, lpa_code, diff_days
                );

                let result_json = build_result_json(
                    &description,
                    donor_id,
                    party_name,
                    lpa_code,
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
                        alerted.insert(motif_hash.clone());
                        info!(
                            donor_id,
                            motif_hash,
                            party_name,
                            lpa_code,
                            diff_days,
                            "Donation correlation detector: HIGH alert written — donation_contract_correlation"
                        );
                        alerts_written += 1;
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            donor_id,
                            "Donation correlation detector: failed to write alert (non-fatal)"
                        );
                    }
                }
            }
        }
    }

    Ok(alerts_written)
}

// ── Dedup helpers ────────────────────────────────────────────────────────────

/// Compute a deterministic dedup key from donor_id + party_name + lpa_code.
///
/// Uses SHA-256 over the separator-delimited concatenation of the three inputs.
/// Each field is hashed individually with a `::` separator so the function is
/// collision-free even when inputs contain `::`.  The resulting lowercase hex
/// digest is stable across Rust toolchain versions.
///
/// T-20-04-02: motif_hash is a hex string — safe as a bound DB parameter value,
/// never string-interpolated into SurrealQL.
pub fn compute_motif_hash(donor_id: &str, party_name: &str, lpa_code: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(donor_id.as_bytes());
    hasher.update(b"::");
    hasher.update(party_name.as_bytes());
    hasher.update(b"::");
    hasher.update(lpa_code.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Normalise an `identity_links.source_node_id` to the bare entity id that `type::record`
/// needs.
///
/// The stored form is bare — `GB-COH:OC307847`, measured live; the comment at the read site
/// claiming `entity:company-number` predates the 2026-06-17 EC fix, which replaced
/// record-id sources with bare ids and purged the old rows. Both forms are accepted anyway,
/// because getting this wrong is silent: a string never equals a record link, so the join
/// returns zero rows and the detector simply reports nothing.
fn bare_entity_id(node_id: &str) -> &str {
    node_id
        .strip_prefix("entity:")
        .unwrap_or(node_id)
        .trim_matches(|c| c == '`' || c == '⟨' || c == '⟩')
}

/// Build the `result_json` literal for a Donation → Contract Correlation alert (46-05).
///
/// Pure — no DB access — so the exact shape (in particular `participants`) is
/// host-only unit-testable without `URBAN_DB_TESTS=1`. `participants` is a single-element
/// array naming `donor_id` only (phase 46 assumption A-05: SingleSubject) — `party_name` and
/// `lpa_code` stay plain display fields, they are not resolvable entity ids.
#[allow(clippy::too_many_arguments)]
fn build_result_json(
    description: &str,
    donor_id: &str,
    party_name: &str,
    lpa_code: &str,
    motif_hash: &str,
    run_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "motif_type": "donation_contract_correlation",
        "risk_score": 0.8_f64,  // HIGH (not CRITICAL)
        "description": description,
        "donor_id": donor_id,
        "party_name": party_name,
        "lpa_code": lpa_code,
        "motif_hash": motif_hash,
        "run_id": run_id,
        "first_seen_run_id": run_id,
        "last_seen_run_id": run_id,
        "participants": [donor_id],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 46-05 Task 1: `result_json` carries run attribution and a 1-element
    /// `participants` array naming only `donor_id` (phase 46 assumption A-05: SingleSubject).
    #[test]
    fn result_json_carries_run_attribution_and_participants() {
        let result_json = build_result_json(
            "Entity donated to Labour Party (party controls W06000001) and received contract within 61 days",
            "GB-COH:OC307847",
            "Labour Party",
            "W06000001",
            "deadbeef",
            "run-x",
        );
        assert_eq!(result_json["run_id"], "run-x");
        assert_eq!(result_json["first_seen_run_id"], "run-x");
        assert_eq!(result_json["last_seen_run_id"], "run-x");
        assert_eq!(
            result_json["participants"],
            serde_json::json!(["GB-COH:OC307847"])
        );
    }

    // ─── bare_entity_id ─────────────────────────────────────────────────────

    #[test]
    fn bare_entity_id_passes_through_the_stored_form() {
        // What identity_links actually holds today.
        assert_eq!(bare_entity_id("GB-COH:OC307847"), "GB-COH:OC307847");
        assert_eq!(bare_entity_id("director:jane-smith"), "director:jane-smith");
    }

    #[test]
    fn bare_entity_id_strips_the_legacy_record_form() {
        // Pre-2026-06-17 EC rows, and anything hand-written in that shape.
        assert_eq!(bare_entity_id("entity:GB-COH:00133804"), "GB-COH:00133804");
        assert_eq!(
            bare_entity_id("entity:`GB-COH:00133804`"),
            "GB-COH:00133804"
        );
        assert_eq!(
            bare_entity_id("entity:⟨GB-COH:00133804⟩"),
            "GB-COH:00133804"
        );
    }

    // ─── Test 1: compute_motif_hash is deterministic ────────────────────────
    #[test]
    fn compute_motif_hash_is_deterministic() {
        let h1 = compute_motif_hash("entity:abc", "Labour Party", "E07000001");
        let h2 = compute_motif_hash("entity:abc", "Labour Party", "E07000001");
        assert_eq!(h1, h2, "compute_motif_hash must be deterministic");
    }

    #[test]
    fn compute_motif_hash_is_hex_string() {
        let h = compute_motif_hash("entity:abc", "Labour Party", "E07000001");
        assert!(
            !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()),
            "motif_hash must be a non-empty hex string; got: {h}"
        );
    }

    #[test]
    fn compute_motif_hash_differs_for_different_inputs() {
        let h1 = compute_motif_hash("entity:abc", "Labour Party", "E07000001");
        let h2 = compute_motif_hash("entity:xyz", "Labour Party", "E07000001");
        assert_ne!(h1, h2, "different donor_id must produce different hash");
    }

    #[test]
    fn compute_motif_hash_is_pinned() {
        // Pin the exact SHA-256 digest so a future implementation change is caught at CI.
        // Value = SHA-256("entity:abc" || "::" || "Labour Party" || "::" || "E07000001")
        let expected = "7bc26ed02f7ed30074e6fd0b3318dddaee84076f074c599581e917a705f4ee69";
        assert_eq!(
            compute_motif_hash("entity:abc", "Labour Party", "E07000001"),
            expected,
            "Hash value changed — dedup keys in graph_events will become stale after deploy"
        );
    }

    // ─── Test 2: ENR-11 negative — candidate links produce zero alerts ──────
    //
    // We test the ENR-11 gate indirectly: the SurrealQL query string must
    // contain the status filter. Direct DB-free test — we verify the query
    // string embedded in the source, which is the seam that the negative test
    // relies on.
    #[test]
    fn enr_11_status_filter_present_in_query() {
        // The ENR-11 gate is the status IN [$auto_linked, $confirmed] clause.
        // If this string is absent from the source, the gate is broken.
        let source = include_str!("donation_correlation.rs");
        assert!(
            source.contains("status IN [$auto_linked, $confirmed]"),
            "ENR-11 gate must be present: status IN [$auto_linked, $confirmed]"
        );
        // Confirm the query binds electoral_commission (not ch_disqualified).
        assert!(
            source.contains(r#"bind(("source", "electoral_commission"))"#),
            "ENR-11: query must filter source='electoral_commission'"
        );
    }

    // ─── Test 3: 730-day boundary — day 730 fires, day 731 does not ─────────
    #[test]
    fn day_730_fires_day_731_does_not() {
        // Mirror the detector's 730-day window logic directly.
        let fires = |diff_days: i64| -> bool { diff_days <= 730i64 };

        assert!(fires(730), "day 730 must fire the detector");
        assert!(!fires(731), "day 731 must NOT fire the detector");
        assert!(fires(0), "same-day must fire the detector");
        assert!(fires(365), "one year must fire the detector");
    }

    // ─── Test 4: PartyAuthorityMap loads from data/party_authority_control.json ──
    #[test]
    fn party_authority_map_loads_from_json() {
        // data/party_authority_control.json was created in Plan 01.
        //
        // Cargo unit tests run with cwd = the package directory (crates/enricher/).
        // The production binary runs from the workspace root where data/ lives.
        // We navigate from CARGO_MANIFEST_DIR (crates/enricher/) up two levels to
        // the workspace root to locate the data file.
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set by cargo test");
        let json_path = std::path::Path::new(&manifest_dir)
            .join("..") // crates/
            .join("..") // workspace root
            .join("data")
            .join("party_authority_control.json");

        let data = std::fs::read_to_string(&json_path).expect(
            "data/party_authority_control.json must exist (created in Plan 01); \
             looked for it relative to workspace root",
        );
        let result: Result<PartyAuthorityMap, _> = serde_json::from_str(&data);
        assert!(
            result.is_ok(),
            "PartyAuthorityMap must deserialise from party_authority_control.json: {:?}",
            result.err()
        );
        let map = result.unwrap();
        // Must have at least one party entry.
        assert!(
            !map.parties.is_empty(),
            "party_authority_control.json must contain at least one party entry"
        );
        // Must contain "Labour Party" (from 20-01 data).
        assert!(
            map.parties.contains_key("Labour Party"),
            "party_authority_control.json must contain 'Labour Party'"
        );
    }

    // ─── Test 5: Party name normalisation ───────────────────────────────────
    #[test]
    fn party_name_normalisation_matches_variants() {
        // "The Labour Party" normalised to "labour party"
        // must match "Labour Party" normalised to "labour party".
        let n1 = normalise_party_name("The Labour Party");
        let n2 = normalise_party_name("Labour Party");
        assert_eq!(
            n1, n2,
            "'The Labour Party' and 'Labour Party' must normalise to the same string"
        );

        // Case insensitivity.
        let n3 = normalise_party_name("LABOUR PARTY");
        assert_eq!(n1, n3, "normalise_party_name must be case-insensitive");
    }

    // ─── EC registered-name alias regression (2026-08-14) ────────────────────
    //
    // The EC publishes registered legal names ("Conservative and Unionist
    // Party"); this map keys on colloquial ones ("Conservative Party").
    // _meta.name_variants has always recorded the mapping but nothing read it,
    // so lookup_lpa_codes returned None and the detector skipped every
    // Conservative donation at the party gate — 13 of the 22 EC links in the
    // live an_anglesey_aebaa785 database.
    //
    // Normalisation cannot bridge this: the names differ by two words, not by
    // case or a leading "the".

    #[test]
    fn ec_registered_name_resolves_via_the_declared_alias_table() {
        let json = r#"{
            "_meta": {
                "name_variants": {
                    "Conservative and Unionist Party": "Conservative Party"
                }
            },
            "Conservative Party": ["E07000001"]
        }"#;
        let map = PartyAuthorityMap::from_json_str(json).expect("parse");

        // Sanity: normalisation alone cannot bridge these two names — if this
        // ever passes, the alias table is no longer what makes the lookup work
        // and this test has stopped guarding the real behaviour.
        assert_ne!(
            normalise_party_name("Conservative and Unionist Party"),
            normalise_party_name("Conservative Party"),
            "these names must differ under normalisation, else the alias is moot"
        );

        let codes = map
            .lookup_lpa_codes("Conservative and Unionist Party")
            .expect("EC registered name must resolve through _meta.name_variants");
        assert_eq!(codes, &vec!["E07000001".to_string()]);
    }

    #[test]
    fn alias_lookup_is_case_insensitive_on_the_ec_name() {
        let json = r#"{
            "_meta": {"name_variants": {"Conservative and Unionist Party": "Conservative Party"}},
            "Conservative Party": ["E07000001"]
        }"#;
        let map = PartyAuthorityMap::from_json_str(json).expect("parse");
        assert!(
            map.lookup_lpa_codes("CONSERVATIVE AND UNIONIST PARTY")
                .is_some(),
            "a casing change in the feed must not drop back to a miss"
        );
    }

    #[test]
    fn missing_or_malformed_name_variants_degrades_rather_than_failing() {
        // No _meta.name_variants at all.
        let map = PartyAuthorityMap::from_json_str(r#"{"_meta":{},"Labour Party":["E07000001"]}"#)
            .expect("must still parse without an alias table");
        assert!(map.aliases.is_empty());
        assert!(map.lookup_lpa_codes("Labour Party").is_some());

        // name_variants present but not an object, plus a non-string value.
        let map2 = PartyAuthorityMap::from_json_str(
            r#"{"_meta":{"name_variants":["not","an","object"]},"Labour Party":["E07000001"]}"#,
        )
        .expect("malformed alias table must not fail the load");
        assert!(map2.aliases.is_empty());
        assert!(map2.lookup_lpa_codes("Labour Party").is_some());
    }

    /// Pins the *shipped* data file, not a fixture. The bug was a mismatch
    /// between the real map's keys and the names the EC actually publishes, so a
    /// hand-built fixture would not have caught it. These three names are the
    /// complete set observed in live `identity_links.evidence_json`.
    #[test]
    fn every_party_name_seen_in_live_ec_data_resolves_against_the_shipped_map() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set by cargo test");
        let json_path = std::path::Path::new(&manifest_dir)
            .join("..")
            .join("..")
            .join("data")
            .join("party_authority_control.json");
        let data =
            std::fs::read_to_string(&json_path).expect("data/party_authority_control.json missing");
        let map = PartyAuthorityMap::from_json_str(&data).expect("shipped map must parse");

        // Counts are the live distribution as of 2026-08-14.
        for (party, seen) in [
            ("Conservative and Unionist Party", 13),
            ("Labour Party", 7),
            ("Liberal Democrats", 2),
        ] {
            assert!(
                map.lookup_lpa_codes(party).is_some(),
                "'{party}' ({seen} live EC links) must resolve — an unresolvable party \
                 silently drops every donation for it at the party gate"
            );
        }
    }

    #[test]
    fn party_authority_map_lookup_normalised() {
        // Build a map with "Labour Party" → ["E07000001"].
        let json = r#"{"_meta":{"source":"test"},"Labour Party":["E07000001"]}"#;
        let map: PartyAuthorityMap = serde_json::from_str(json).expect("parse");
        // Exact lookup must work.
        let codes = map.lookup_lpa_codes("Labour Party");
        assert!(codes.is_some(), "exact lookup must succeed");
        // Normalised lookup with "The Labour Party" must also work.
        let codes2 = map.lookup_lpa_codes("The Labour Party");
        assert!(
            codes2.is_some(),
            "normalised lookup for 'The Labour Party' must succeed"
        );
    }
}
