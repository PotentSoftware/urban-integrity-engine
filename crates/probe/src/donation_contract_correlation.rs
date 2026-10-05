//! `DonationContractCorrelation` -- ports `probe_donation_contract_correlation`
//! (`scripts/analyst_probe.py:2190-2260`).
//!
//! # Four zero-alert causes, not two (D-12, D-13)
//!
//! `donation_correlation.rs`'s detector has four distinct paths that all end in "zero alerts":
//! no EC links at all; links present but all failing the ENR-11 status gate; gated links whose
//! donor party is absent from `data/party_authority_control.json`; and gated+mapped links with
//! no contract inside the 730-day window. Reporting all four as a single `Clear` would be the
//! exact false-clean defect VRD-06 exists to close -- a fresh clone with `data/*` gitignored
//! would report every donor's party as unmapped, and folding that into `Clear` claims a
//! measured-clean result the data cannot support.
//!
//! Encoding: `population` = every `electoral_commission` `identity_links` row; `capable` = the
//! subset passing BOTH the ENR-11 status gate AND the party-map lookup. `capable < population`
//! -> `NotApplicable`, naming which exclusion dominates. `capable == population` with zero
//! `MotifDetected` rows -> `Clear`, whose reason states this is a genuine temporal miss (D-13) --
//! text that must differ observably from the `NotApplicable` branch's reason, so the two zero-
//! alert causes are distinguishable by prose in a test assertion, not just by verdict type.
//!
//! Note ENR-11 here is the identity-link status gate (`donation_correlation.rs:153`), not the
//! entity Active/dissolved check Python's evidence text claims (`analyst_probe.py:2226`) -- that
//! evidence line is stale and must not be copied forward verbatim.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_enricher::donation_correlation::PartyAuthorityMap;
use urban_integrity_schema::types::verdict::{EvidenceRow, Finding, Severity, Verdict};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

pub struct DonationContractCorrelation;

/// `payload.result_json`'s `motif_type`, or `"unknown"` if the row is malformed. Local duplicate
/// of `precomputed_alerts.rs`'s helper -- every probe defines its own copy rather than importing
/// a shared one (this plan's `<read_first>` instruction).
fn motif_type_of(row: &serde_json::Value) -> String {
    row["payload"]["result_json"]
        .as_str()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("motif_type")
                .and_then(|t| t.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[async_trait::async_trait]
impl Probe for DonationContractCorrelation {
    fn name(&self) -> &'static str {
        "donation-contract-correlation"
    }

    /// Can-name-entities, deferred: `ec_links`'s rows (each carrying `target_node_id`) are held
    /// and discarded after `.len()`-based counting into `population` (~line 182); the
    /// `MotifDetected`-scanned second half (~line 241-271, `motif_type_of`) also discards
    /// per-row ids and needs the SAME per-`motif_type` field-mapping caveat
    /// `precomputed_alerts.rs`'s `entity_ids_of` documents (49-06) -- a future implementation
    /// must not assume a uniform `participants` field.
    ///
    /// Classified can-name-entities by Phase 49's EVM-01 audit; deferred past this phase per
    /// D-02 ("not every probe has to be covered to the same depth" -- the phase 49 design notes), recorded
    /// in plan 49-10.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::AggregateOnly
    }

    /// No RecordRef at all -- aggregate-only evidence, the card states absence honestly (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Absence
    }

    fn tier(&self) -> &'static str {
        "Tier-2"
    }

    fn hypothesis(&self) -> &'static str {
        "Does an electoral donation from an entity correlate in time (within 730 days, either \
         direction) with a contract award from a party-controlled LPA to that same entity?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        let ec_links = match scan::scan_identity_links(db, None, Some("electoral_commission")).await
        {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result -- Probe::run is
                // infallible by design (PRB-08).
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!(
                        "electoral_commission identity_links scan failed: {e}"
                    )),
                    summary: "electoral_commission identity_links scan failed -- donation-\
                              contract correlation cannot be evaluated."
                        .to_string(),
                    cause: None,
                    // No direct Python branch analog for a raw scan failure; nearest analog is
                    // analyst_probe.py:2551's generic probe-exception handler (DATA_GAP/INFO).
                    severity: Severity::Info,
                    evidence: vec![EvidenceRow {
                        text: format!("Scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        if ec_links.is_empty() {
            // Mirror `electoral_commission_donations.rs`'s own zero-population branch: absence of
            // links is only honestly Clear if the adapter genuinely ran and found nothing.
            let marker = st
                .enrichment_runs(db)
                .await
                .get("electoral_commission")
                .cloned();
            return match marker {
                Some(marker) => {
                    // Maps to analyst_probe.py:2231's CLEAR/INFO branch (EC links present-in-
                    // spirit -- the adapter ran and found nothing -- but here population is
                    // itself zero).
                    let summary = "0 electoral_commission identity_links; adapter ran, no \
                                    donor exists to correlate against a contract."
                        .to_string();
                    Finding {
                        probe: self.name().to_string(),
                        verdict: Verdict::classify(0, 0, 0, 1, ""),
                        summary,
                        cause: None,
                        severity: Severity::Info,
                        evidence: vec![EvidenceRow {
                            text: format!(
                                "enrichment_runs marker: candidates_found={}",
                                marker.candidates_found
                            ),
                            refs: vec![],
                        }],
                        attributions: vec![],
                        remediation_hint: None,
                    }
                }
                None => Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(
                        "electoral_commission adapter has never run -- no donation data exists \
                         to correlate",
                    ),
                    summary: "electoral_commission adapter has never run -- no donation data \
                              exists to correlate."
                        .to_string(),
                    cause: None,
                    // Maps directly to analyst_probe.py:2217's DATA_GAP/HIGH branch (no
                    // correlation alerts and no EC donation links at all).
                    severity: Severity::High,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
            };
        }

        let map = match PartyAuthorityMap::load() {
            Ok(m) => m,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!(
                        "party_authority_control.json load failed: {e} -- donor party mapping \
                         cannot be evaluated"
                    )),
                    summary: format!(
                        "party_authority_control.json load failed: {e} -- donor party mapping \
                         cannot be evaluated."
                    ),
                    cause: None,
                    // No Python analog, defaulted per 35-01 convention: analog-less DataGap
                    // branches default to Severity::Medium.
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: "This is a DataGap, not a computed NotApplicable: the data \
                               required to determine exclusion is itself unavailable."
                            .to_string(),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        let population = ec_links.len() as u64;
        let mut status_excluded: u64 = 0;
        let mut party_unmapped: u64 = 0;

        for row in &ec_links {
            let passes_status = row.status == "auto_linked" || row.status == "confirmed";
            if !passes_status {
                status_excluded += 1;
                continue;
            }
            let party_mapped = serde_json::from_str::<serde_json::Value>(&row.evidence_json)
                .ok()
                .and_then(|v| {
                    v.get("party_name")
                        .and_then(|p| p.as_str())
                        .map(str::to_owned)
                })
                .is_some_and(|p| map.lookup_lpa_codes(&p).is_some());
            if !party_mapped {
                party_unmapped += 1;
            }
        }

        let capable = population - status_excluded - party_unmapped;

        if capable < population {
            let dominant = if status_excluded >= party_unmapped {
                "ENR-11 status gate"
            } else {
                "unmapped donor party"
            };
            let population_str = plural(population, "link", "links");
            let reason = format!(
                "{status_excluded} of {population_str} excluded by the ENR-11 status gate; \
                 {party_unmapped} of {population_str} pass status but the donor party is \
                 unmapped in party_authority_control.json -- dominant exclusion: {dominant}"
            );
            let summary = format!("{status_excluded} of {population_str} excluded ({dominant}).");
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::classify(population, capable, 0, 1, &reason),
                summary,
                cause: None,
                // No direct Python branch analog (this ENR-11/party-map split is a VRD-06 fix
                // introduced by this milestone); analog-less NotApplicable branches default to
                // Severity::Info per 35-01 convention.
                severity: Severity::Info,
                evidence: vec![EvidenceRow {
                    text: reason,
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // capable == population: every link passes both gates. The only remaining zero-alert
        // cause is a genuine temporal miss (D-13) -- re-derived by construction (counting the
        // detector's own MotifDetected output), never re-implemented here.
        let rows = match scan::scan_events_by_kind(db, "MotifDetected").await {
            Ok(rows) => rows,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("MotifDetected scan failed: {e}")),
                    summary: "MotifDetected scan failed -- donation-contract correlation cannot \
                              be evaluated."
                        .to_string(),
                    cause: None,
                    // No direct Python branch analog for a raw scan failure; nearest analog is
                    // analyst_probe.py:2551's generic probe-exception handler (DATA_GAP/INFO).
                    severity: Severity::Info,
                    evidence: vec![EvidenceRow {
                        text: format!("Scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // NEVER wrap this count in `attributed::count_for_source`/`REGISTRY` -- `donation_
        // correlation.rs` lives outside `crates/enricher/src/adapters/`, so a new `REGISTRY`
        // entry would make `every_evidence_term_lists_every_producer` fail with an empty
        // `discovered` set (Pitfall 3).
        let matches = rows
            .iter()
            .filter(|row| motif_type_of(row) == "donation_contract_correlation")
            .count() as u64;

        let verdict = Verdict::classify(population, capable, matches, 1, "");
        let population_str = plural(population, "link", "links");
        let (severity, summary, evidence) = if matches == 0 {
            (
                // Maps to analyst_probe.py:2231's CLEAR/INFO branch.
                Severity::Info,
                format!(
                    "{population_str} from Electoral Commission; no correlation within 730 days."
                ),
                vec![EvidenceRow {
                    text: format!(
                        "All {population_str} pass the ENR-11 status gate and donor party \
                         mapping; no donation fell within 730 days of a contract from a party-\
                         controlled LPA. This is a genuine temporal miss, not an exclusion."
                    ),
                    refs: vec![],
                }],
            )
        } else {
            let matches_str = plural(
                matches,
                "donation_contract_correlation alert",
                "donation_contract_correlation alerts",
            );
            (
                // Maps to analyst_probe.py:2292's HIT/HIGH branch.
                Severity::High,
                format!("{population_str} from Electoral Commission; {matches_str} found."),
                vec![EvidenceRow {
                    text: format!(
                        "All {population_str} pass the ENR-11 status gate and donor party \
                         mapping."
                    ),
                    refs: vec![],
                }],
            )
        };

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            severity,
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cargo unit tests run with cwd = the package directory (crates/cli/), but
    /// `PartyAuthorityMap::load()` reads `data/party_authority_control.json` relative to the
    /// workspace root -- same fix `crates/core/src/config.rs`'s `test_load_default_config` uses
    /// for `AppConfig::load()`'s `config.toml`.
    ///
    /// Resolved from the compile-time `CARGO_MANIFEST_DIR` constant, NOT a relative `"../../"`
    /// off the process's CURRENT cwd -- cargo test runs tests in parallel threads sharing one
    /// process-wide cwd, so a relative chdir compounds when more than one test calls it (the
    /// second caller would climb two levels past a cwd another thread already moved to the
    /// workspace root). An absolute, CARGO_MANIFEST_DIR-derived target is idempotent regardless
    /// of call order or how many tests call it concurrently.
    fn ensure_cwd_is_workspace_root() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::env::set_current_dir(&workspace_root).ok();
    }

    async fn seed_identity_link(
        db: &Surreal<Any>,
        key: &str,
        source_node_id: &str,
        target_node_id: &str,
        source: &str,
        status: &str,
        evidence_json: &str,
    ) {
        db.query(
            "CREATE type::record('identity_links', $key) SET \
             source_node_id = $src, target_node_id = $tgt, resolution_type = 'exact_ch_number', \
             confidence = $conf, source = $source, resolved_at = $resolved_at, \
             evidence_json = $evidence_json, status = $status",
        )
        .bind(("key", key.to_string()))
        .bind(("src", source_node_id.to_string()))
        .bind(("tgt", target_node_id.to_string()))
        .bind(("conf", 1.0f64))
        .bind(("source", source.to_string()))
        .bind(("status", status.to_string()))
        .bind(("resolved_at", chrono::Utc::now()))
        .bind(("evidence_json", evidence_json.to_string()))
        .await
        .expect("seed identity_links row")
        .check()
        .expect("seed identity_links row check");
    }

    async fn seed_enrichment_run(db: &Surreal<Any>, source: &str, candidates_found: i64) {
        db.query(format!(
            "UPSERT enrichment_runs:{source} SET source = '{source}', \
             last_run_at = $last_run_at, candidates_found = $candidates_found, \
             auto_linked = $auto_linked, pending_review = $pending_review, \
             discarded = $discarded"
        ))
        .bind(("last_run_at", 1_756_000_000i64))
        .bind(("candidates_found", candidates_found))
        .bind(("auto_linked", 0i64))
        .bind(("pending_review", 0i64))
        .bind(("discarded", 0i64))
        .await
        .expect("seed enrichment_runs row")
        .check()
        .expect("seed enrichment_runs row check");
    }

    async fn seed_motif_event(db: &Surreal<Any>, motif_type: &str, suffix: &str) {
        let result_json = serde_json::json!({
            "motif_type": motif_type,
            "risk_score": 0.8,
            "description": "test alert",
        })
        .to_string();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("MotifDetected-{motif_type}-{suffix}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "MotifDetected",
                    "payload": { "result_json": result_json },
                }),
            ))
            .await
            .expect("failed to seed MotifDetected event")
            .check()
            .expect("failed to seed MotifDetected event (check)");
    }

    #[tokio::test]
    async fn donation_correlation_zero_links_no_marker_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: donation_correlation_zero_links_no_marker_is_data_gap (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_donation_correlation",
            "no_marker",
        )
        .await;
        let mut st = Strategy::default();

        let probe = DonationContractCorrelation;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero ec_links with no marker must be DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn donation_correlation_zero_links_with_marker_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: donation_correlation_zero_links_with_marker_is_clear (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_donation_correlation",
            "with_marker",
        )
        .await;
        seed_enrichment_run(&db, "electoral_commission", 13).await;
        let mut st = Strategy::default();

        let probe = DonationContractCorrelation;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "zero ec_links but a live enrichment_runs marker must be Clear, got {:?}",
            finding.verdict
        );
        assert!(
            finding.evidence.iter().any(|e| e.text.contains("13")),
            "evidence must cite the marker's candidates_found, got: {:?}",
            finding.evidence
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // The marker's candidates_found: 13 is a self-reported adapter statistic in evidence
        // text, not the Coverage population. The population is the zero measured ec_links.
        assert_eq!(
            coverage.population(),
            0,
            "population must be 0 -- zero ec_links measured on the marker branch"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "capable must equal population (0) on the zero-link marker branch"
        );
    }

    #[tokio::test]
    async fn donation_correlation_exclusion_is_not_applicable() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: donation_correlation_exclusion_is_not_applicable (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        ensure_cwd_is_workspace_root();
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_donation_correlation",
            "exclusion",
        )
        .await;
        // Confirmed, party mapped (present in the real party_authority_control.json).
        seed_identity_link(
            &db,
            "excl_confirmed_mapped",
            "GB-COH:00000001",
            "donor:acme",
            "electoral_commission",
            "confirmed",
            r#"{"party_name": "Labour Party", "accepted_date": "01/01/2024"}"#,
        )
        .await;
        // Candidate status -- ENR-11 exclusion.
        seed_identity_link(
            &db,
            "excl_status_gate",
            "GB-COH:00000002",
            "donor:beta",
            "electoral_commission",
            "candidate",
            r#"{"party_name": "Labour Party", "accepted_date": "01/01/2024"}"#,
        )
        .await;
        // Confirmed, party unmapped.
        seed_identity_link(
            &db,
            "excl_party_unmapped",
            "GB-COH:00000003",
            "donor:gamma",
            "electoral_commission",
            "confirmed",
            r#"{"party_name": "Not A Real Party", "accepted_date": "01/01/2024"}"#,
        )
        .await;
        let mut st = Strategy::default();

        let probe = DonationContractCorrelation;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::NotApplicable { .. }),
            "mixed status-excluded/unmapped links must be NotApplicable, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains('1'),
            "summary must state the exclusion counts, got: {}",
            finding.summary
        );
        assert!(
            finding.summary.contains("ENR-11") || finding.summary.contains("unmapped"),
            "summary must name a specific exclusion cause, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn donation_correlation_temporal_miss_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: donation_correlation_temporal_miss_is_clear (set URBAN_DB_TESTS=1)");
            return;
        }
        ensure_cwd_is_workspace_root();
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_donation_correlation",
            "temporal_miss",
        )
        .await;
        seed_identity_link(
            &db,
            "tm_1",
            "GB-COH:00000011",
            "donor:delta",
            "electoral_commission",
            "auto_linked",
            r#"{"party_name": "Labour Party", "accepted_date": "01/01/2024"}"#,
        )
        .await;
        seed_identity_link(
            &db,
            "tm_2",
            "GB-COH:00000012",
            "donor:epsilon",
            "electoral_commission",
            "confirmed",
            r#"{"party_name": "Labour Party", "accepted_date": "01/01/2024"}"#,
        )
        .await;
        // Zero MotifDetected rows -- genuine temporal miss.
        let mut st = Strategy::default();

        let probe = DonationContractCorrelation;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "fully capable population with zero MotifDetected rows must be Clear, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("730 days"),
            "Clear summary must cite the 730-day window, got: {}",
            finding.summary
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // 2 seeded electoral_commission links, both at a passing status, zero MotifDetected
        // rows -- a fully capable population with no temporal overlap. A corroborated Clear
        // over a real population, not the zero-population shape.
        assert_eq!(
            coverage.population(),
            2,
            "population must count both seeded auto_linked/confirmed ec links"
        );
        assert_eq!(
            coverage.capable(),
            2,
            "capable must equal population -- both links carry a usable accepted_date"
        );

        // Prove the two zero-alert-cause branches are distinguishable by prose, not just by
        // verdict type (D-13).
        let db2 = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_donation_correlation",
            "exclusion_for_diff",
        )
        .await;
        seed_identity_link(
            &db2,
            "diff_confirmed_mapped",
            "GB-COH:00000021",
            "donor:zeta",
            "electoral_commission",
            "confirmed",
            r#"{"party_name": "Labour Party", "accepted_date": "01/01/2024"}"#,
        )
        .await;
        seed_identity_link(
            &db2,
            "diff_status_gate",
            "GB-COH:00000022",
            "donor:eta",
            "electoral_commission",
            "candidate",
            r#"{"party_name": "Labour Party", "accepted_date": "01/01/2024"}"#,
        )
        .await;
        let mut st2 = Strategy::default();
        let exclusion_finding = DonationContractCorrelation.run(&db2, &mut st2).await;

        assert_ne!(
            finding.summary, exclusion_finding.summary,
            "temporal-miss and exclusion summaries must differ observably"
        );
        assert!(
            !exclusion_finding.summary.contains("730"),
            "the exclusion-cause summary must not contain the temporal-miss's '730' text, got: {}",
            exclusion_finding.summary
        );
    }

    #[tokio::test]
    async fn donation_correlation_matched_motif_is_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: donation_correlation_matched_motif_is_hit (set URBAN_DB_TESTS=1)");
            return;
        }
        ensure_cwd_is_workspace_root();
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_donation_correlation",
            "hit",
        )
        .await;
        seed_identity_link(
            &db,
            "hit_1",
            "GB-COH:00000031",
            "donor:theta",
            "electoral_commission",
            "auto_linked",
            r#"{"party_name": "Labour Party", "accepted_date": "01/01/2024"}"#,
        )
        .await;
        seed_motif_event(&db, "donation_contract_correlation", "hit-1").await;
        let mut st = Strategy::default();

        let probe = DonationContractCorrelation;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "a fully capable population with a matching MotifDetected row must be Hit, got {:?}",
            finding.verdict
        );
    }
}
