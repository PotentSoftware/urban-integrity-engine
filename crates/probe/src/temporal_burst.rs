//! `TemporalBurst` Tier-1 probe — ports `probe_temporal_burst` (`scripts/analyst_probe.py:1126-1151`).
//!
//! Python's own evidence text never states the total Contract population it was measured
//! against (`f"Busiest ingestion day: {top[0][1]} applications."`) — a 3-contract corpus reads
//! identically to a 300-contract one (VRD-06's missing-denominator hazard, ROADMAP success
//! criterion 4). This port always states BOTH the busiest-day count AND the total population.

use std::collections::HashMap;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// Verdict flips from `Clear` to `Weak` at this many Contracts landing on the same ingestion
/// day. Cites `analyst_probe.py:1147`'s `top[0][1] >= 10`.
const WEAK_THRESHOLD: u64 = 10;

/// The missing-denominator hazard fix (VRD-06) — see module doc comment.
pub struct TemporalBurst;

#[async_trait::async_trait]
impl Probe for TemporalBurst {
    fn name(&self) -> &'static str {
        "temporal-burst"
    }

    /// Searchable by the findings-by-entity index (PIV-02): the busiest-day row carries an `Entity` ref per application id, capped for display.
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::NamesNodes
    }

    /// A bag of named-entity chips, no dedicated motif renderer (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Chips
    }

    fn tier(&self) -> &'static str {
        "Tier-1"
    }

    fn hypothesis(&self) -> &'static str {
        "Is there a burst of Contract ingestion on a single day, relative to the total Contract \
         population evaluated — of which only those carrying a usable timestamp are capable of \
         showing one?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        // NOT `scan_entities_by_type` — that helper drops the row's `timestamp` field, which
        // this probe needs to bucket by ingestion day.
        let rows = match scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Contract\"").await {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result — Probe::run is
                // infallible by design (PRB-08).
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("Contract scan failed: {e}")),
                    summary: "Contract scan failed — temporal analysis not possible.".to_string(),
                    cause: None,
                    // No Python analog (Python has no scan-failure branch here) — defaulted
                    // per 35-01 convention: an analog-less DataGap/Blocked branch is Medium.
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: format!("Contract scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // `population` is counted through the SAME gate `scan_latest_entities_by_type` applies — a
        // parseable `entity_id` AND a parseable `weight_json` — because `ward-concentration`
        // derives ITS population from that helper, and the report's contradiction sub-check 1a
        // compares the two figures for equality. Counting raw `rows.len()` here would put the two
        // denominators back out of step in the opposite direction (the helper drops unparseable
        // rows; `scan_events_by_type` returns them).
        //
        // Deduplication adds the OTHER half of that parity: `graph_events` is append-only, so a
        // re-ingested Contract appears once per ingest, and counting rows counted events rather
        // than Contracts — 105,613 against 45,874 distinct on `an_anglesey_aebaa785`, 2026-08-31
        // (bucket (a), `36-DIFFERENTIAL-RESULTS.md`). This probe cannot use the shared
        // `scan_latest_entities_by_type` helper: it needs the raw row's `timestamp`, which that
        // signature drops. It also wants the opposite tie-break — EARLIEST event, not latest —
        // because a burst means "many applications arrived on one day", and letting a re-ingest
        // re-date a Contract would manufacture a phantom burst on the re-ingest day.
        //
        // Earliest is taken by `timestamp`, never by position: `graph_events` record ids are
        // random, so the `ORDER BY id` the scan pages by is NOT chronological and "first row seen"
        // would be an arbitrary pick among a Contract's events.
        let mut earliest: HashMap<String, u64> = HashMap::new();
        for row in &rows {
            let Some(entity_id) = scan::entity_id_of(&row["payload"]) else {
                continue;
            };
            let Some(weight_json) = row["payload"]["weight_json"].as_str() else {
                continue;
            };
            if serde_json::from_str::<serde_json::Value>(weight_json).is_err() {
                continue;
            }
            // `u64::MAX` marks "no usable timestamp": the Contract still counts toward
            // `population` below, but stays out of `capable` and out of every day bucket, which is
            // exactly the distinction this probe exists to report.
            let ts = row["timestamp"].as_u64().unwrap_or(u64::MAX);
            earliest
                .entry(entity_id)
                .and_modify(|best| *best = (*best).min(ts))
                .or_insert(ts);
        }

        let mut population: u64 = 0;
        let mut days: HashMap<String, u64> = HashMap::new();
        for ts in earliest.values().copied() {
            population += 1;
            if ts == u64::MAX {
                continue;
            }
            // `graph_events.timestamp` is millis by convention — every writer goes through
            // `coordinator.rs`'s `emit_wal` (`.as_millis() as u64`) — so no seconds/millis
            // disambiguation is done here, unlike Python's `_ts_to_day`. That is a convention,
            // NOT an invariant, and it already has one measured exception: one company node's
            // `NodeUpdated` carries `1786644766` (seconds) against its own `NodeAdded`'s
            // `1784669527728` (millis) — one row of 977,302 on `an_anglesey_aebaa785`, tracked as
            // L8 in the projection-truncation investigation notes. No Contract
            // event is affected today, so no day bucket here is wrong. If one ever is, a
            // seconds-valued Contract lands in a 1970 bucket rather than being rejected: it stays
            // counted as `capable`, and the busiest-day figure is unaffected unless the spurious
            // day wins. Fix belongs at the writer, not by guessing the unit here — inferring it
            // from magnitude would silently absorb the defect this probe exists to expose.
            let Some(dt) = chrono::DateTime::from_timestamp_millis(ts as i64) else {
                continue;
            };
            let day = dt.format("%Y-%m-%d").to_string();
            *days.entry(day).or_insert(0) += 1;
        }

        if days.is_empty() {
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(
                    "no usable timestamps on Contract nodes — temporal analysis not possible",
                ),
                summary: "No usable timestamps on Contract nodes — temporal analysis not possible."
                    .to_string(),
                cause: None,
                // Python: DATA_GAP/INFO (`scripts/analyst_probe.py:1136-1141`).
                severity: Severity::Info,
                evidence: vec![EvidenceRow {
                    text: "Zero Contract nodes carried a usable timestamp — temporal analysis \
                           not possible."
                        .to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // `capable` — VRD-01's second count — is the structurally-eligible subset of
        // `population`: the Contracts that produced a usable ingestion-day bucket. A Contract
        // whose `graph_events.timestamp` is absent or non-numeric is IN the population this probe
        // evaluated and is NOT capable of contributing to a burst pattern, which is exactly what
        // the two counts are for. Reporting the bucketed count as the population instead hid the
        // exclusion entirely and made this probe's population incomparable to
        // `ward-concentration`'s.
        let capable: u64 = days.values().sum();
        let excluded: u64 = population - capable;

        let (busiest_day, &busiest_count) = days
            .iter()
            .max_by_key(|(_, &count)| count)
            .expect("days is non-empty, checked above");

        // Clamping to 0 below the threshold is what makes `Verdict::classify`'s built-in
        // "matches==0 -> Clear" branch reproduce Python's "<10 -> CLEAR, >=10 -> WEAK" binary
        // split — a non-obvious mapping from Python's threshold-gated binary split onto
        // `classify`'s three-tier model.
        let matches = if busiest_count >= WEAK_THRESHOLD {
            busiest_count
        } else {
            0
        };

        // `hit_threshold: u64::MAX` is deliberate, same "never Hit" reasoning as
        // GeographicConcentration (Python's vocabulary for this probe is WEAK/CLEAR only).
        //
        // When `excluded > 0`, `classify` returns `NotApplicable` unconditionally — a deliberate
        // trade this probe now makes: a burst measured over an incomplete subset is reported as
        // an incomplete measurement with its exclusion count stated, rather than as a Weak/Clear
        // verdict over a denominator the reader cannot see. The busiest-day figures stay in the
        // summary and evidence either way, so the signal is never dropped, only re-labelled.
        let verdict = Verdict::classify(
            population,
            capable,
            matches,
            u64::MAX,
            &format!(
                "{} carried no usable graph_events timestamp and could not be bucketed by \
                 ingestion day",
                plural(excluded, "Contract", "Contracts")
            ),
        );

        // Short ledger-cell summary — names the busiest day, its count, AND the total population
        // (the population figure is not removed from the rendered surface entirely, per this
        // task's instruction), plus a dedicated EvidenceRow below duplicating the population
        // figure so the report's evidence section states it independently of the summary cell.
        // Wave 3's contradiction check (plan 35-12) reads `Finding.population()` (the Verdict's
        // own Coverage) directly, not either of these text fields — this widening is
        // presentation-only and does not touch how `population` above is computed.
        let summary = format!(
            "Busiest ingestion day: {} on {busiest_day}, of {} with a usable timestamp, out of \
             {} evaluated. Caveat: graph-ingestion timestamps, not portal submission dates.",
            plural(busiest_count, "application", "applications"),
            plural(capable, "total Contract", "total Contracts"),
            plural(population, "Contract", "Contracts")
        );
        let mut evidence = vec![EvidenceRow {
            text: format!(
                "{} with a usable timestamp, of {} evaluated.",
                plural(capable, "total Contract", "total Contracts"),
                plural(population, "Contract", "Contracts")
            ),
            refs: vec![],
        }];
        if excluded > 0 {
            evidence.push(EvidenceRow {
                text: format!(
                    "{} excluded — no usable graph_events timestamp, so no ingestion day could \
                     be derived.",
                    plural(excluded, "Contract", "Contracts")
                ),
                refs: vec![],
            });
        }

        // Name the busiest day's own contracts, so the burst can be put on the canvas rather than
        // only counted. Re-derives each id's day from `earliest` with the SAME millis conversion
        // the bucketing above uses — a second, different conversion here would be a way for the
        // named set to disagree with the count it sits under.
        const REF_CAP: usize = 20;
        let mut busiest_ids: Vec<&String> = earliest
            .iter()
            .filter(|(_, ts)| **ts != u64::MAX)
            .filter(|(_, ts)| {
                chrono::DateTime::from_timestamp_millis(**ts as i64)
                    .map(|dt| &dt.format("%Y-%m-%d").to_string() == busiest_day)
                    .unwrap_or(false)
            })
            .map(|(id, _)| id)
            .collect();
        busiest_ids.sort();
        evidence.push(EvidenceRow {
            text: format!(
                "{} on {busiest_day} (showing up to {REF_CAP})",
                plural(busiest_count, "application", "applications")
            ),
            refs: busiest_ids
                .iter()
                .take(REF_CAP)
                .map(|id| RecordRef {
                    kind: RecordKind::Entity,
                    id: (*id).clone(),
                })
                .collect(),
        });

        // Python: severity is LOW in both the WEAK and CLEAR branches
        // (`scripts/analyst_probe.py:1148`) — same severity regardless of verdict.
        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            severity: Severity::Low,
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::node::{ContractData, NodeWeight};

    async fn seed_contract_event(db: &Surreal<Any>, entity_id: &str, timestamp_ms: u64) {
        let weight = NodeWeight::Contract(ContractData {
            name: format!("Contract {entity_id}"),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("NodeAdded-{entity_id}"),
                    "timestamp": timestamp_ms,
                    "kind": "NodeAdded",
                    "payload": { "entity_id": entity_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed contract event");
    }

    /// Same shape as `seed_contract_event`, but the event carries no numeric `timestamp` — the
    /// row `temporal_burst` cannot bucket and `ward_concentration` counts anyway.
    async fn seed_timestampless_contract_event(db: &Surreal<Any>, entity_id: &str) {
        let weight = NodeWeight::Contract(ContractData {
            name: format!("Contract {entity_id}"),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("NodeAdded-{entity_id}"),
                    "timestamp": serde_json::Value::Null,
                    "kind": "NodeAdded",
                    "payload": { "entity_id": entity_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed timestamp-less contract event");
    }

    /// A Contract with no usable timestamp is IN the population and NOT capable — the exclusion
    /// is reported, not hidden by shrinking the denominator.
    ///
    /// The bucketed count used to be reported as the population, which made this probe's
    /// population incomparable to `ward-concentration`'s (that probe counts every Contract) and
    /// left the report's contradiction sub-check 1a comparing two figures that differ by design.
    #[tokio::test]
    async fn temporal_burst_untimestamped_contract_is_excluded_not_removed_from_population() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: temporal_burst_untimestamped_contract_is_excluded_not_removed_from_population (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        const DAY_MS: u64 = 86_400_000;
        const BASE_MS: u64 = 1_700_000_000_000;
        for i in 0..3u64 {
            seed_contract_event(&db, &format!("GB-CF:TB-EXCL-{i}"), BASE_MS + i * DAY_MS).await;
        }
        seed_timestampless_contract_event(&db, "GB-CF:TB-EXCL-NOTS").await;

        let probe = TemporalBurst;
        let finding = probe.run(&db, &mut st).await;

        let coverage = finding
            .verdict
            .coverage()
            .expect("a measured verdict must carry a Coverage");
        assert_eq!(
            coverage.population(),
            4,
            "the untimestamped Contract must stay in the population, got {:?}",
            finding.verdict
        );
        assert_eq!(
            coverage.capable(),
            3,
            "only the timestamped Contracts are capable of showing a burst, got {:?}",
            finding.verdict
        );
        assert_eq!(coverage.excluded(), 1);
        assert!(
            matches!(finding.verdict, Verdict::NotApplicable { .. }),
            "an incomplete measurement is NotApplicable with its exclusion stated, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.text.contains("excluded") && e.text.contains('1')),
            "the exclusion count must be stated in evidence, got: {:?}",
            finding.evidence
        );
    }

    #[tokio::test]
    async fn temporal_burst_zero_contracts_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: temporal_burst_zero_contracts_is_data_gap (set URBAN_DB_TESTS=1)");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        let probe = TemporalBurst;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero Contract nodes must be DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn temporal_burst_spread_across_days_is_clear_with_population_in_evidence() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: temporal_burst_spread_across_days_is_clear_with_population_in_evidence (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // 8 Contracts, each on a distinct day (86_400_000 ms apart), busiest day count = 1 < 10.
        const DAY_MS: u64 = 86_400_000;
        const BASE_MS: u64 = 1_700_000_000_000;
        for i in 0..8u64 {
            seed_contract_event(&db, &format!("GB-CF:TB-CLEAR-{i}"), BASE_MS + i * DAY_MS).await;
        }

        let probe = TemporalBurst;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "8 contracts spread across 8 distinct days must be Clear, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains(&8u64.to_string()),
            "evidence must state the total population figure, got: {}",
            finding.summary
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.text.contains(&8u64.to_string())),
            "the dedicated EvidenceRow must also state the total population figure, got: {:?}",
            finding.evidence
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // 8 contracts, one per distinct day, all with usable timestamps -- none excluded.
        assert_eq!(
            coverage.population(),
            8,
            "population must count all 8 seeded contracts"
        );
        assert_eq!(
            coverage.capable(),
            8,
            "capable must equal population -- every contract carries a usable timestamp"
        );
    }

    #[tokio::test]
    async fn temporal_burst_ten_on_one_day_is_weak_with_both_figures_in_evidence() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: temporal_burst_ten_on_one_day_is_weak_with_both_figures_in_evidence (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        const BASE_MS: u64 = 1_700_000_000_000;
        // 10 Contracts on the same day.
        for i in 0..10u64 {
            seed_contract_event(&db, &format!("GB-CF:TB-WEAK-{i}"), BASE_MS + i * 1_000).await;
        }

        let probe = TemporalBurst;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "10 contracts on one day must be Weak, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("10"),
            "evidence must state the busiest-day count, got: {}",
            finding.summary
        );
        assert!(
            finding.summary.contains(&10u64.to_string()),
            "evidence must state the total population figure, got: {}",
            finding.summary
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.text.contains(&10u64.to_string())),
            "the dedicated EvidenceRow must also state the total population figure, got: {:?}",
            finding.evidence
        );

        // The burst itself must be pivotable: every contract seeded on that day is named, so
        // "Show on graph" puts the burst on the canvas rather than only its size.
        let busiest_row = finding
            .evidence
            .iter()
            .find(|row| !row.refs.is_empty())
            .expect("the busiest day must emit a row carrying refs");
        let ids: Vec<&str> = busiest_row.refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 10, "all ten same-day contracts, got {ids:?}");
        for i in 0..10u64 {
            let expected = format!("GB-CF:TB-WEAK-{i}");
            assert!(
                ids.contains(&expected.as_str()),
                "busiest-day row must reference {expected}, got {ids:?}"
            );
        }
    }

    /// PROVE THE DAY FILTER IS REAL: contracts on a different day must not be named in the
    /// busiest day's row, or the "refs" would just be every contract the probe saw.
    #[tokio::test]
    async fn temporal_burst_busiest_day_row_names_only_that_days_contracts() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: temporal_burst_busiest_day_row_names_only_that_days_contracts (set URBAN_DB_TESTS=1)");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        const BASE_MS: u64 = 1_700_000_000_000;
        const ONE_DAY_MS: u64 = 86_400_000;
        for i in 0..10u64 {
            seed_contract_event(&db, &format!("GB-CF:TB-BUSY-{i}"), BASE_MS + i * 1_000).await;
        }
        // One contract a week later — a different, quieter day.
        seed_contract_event(&db, "GB-CF:TB-OTHER-DAY", BASE_MS + 7 * ONE_DAY_MS).await;

        let probe = TemporalBurst;
        let finding = probe.run(&db, &mut st).await;

        let busiest_row = finding
            .evidence
            .iter()
            .find(|row| !row.refs.is_empty())
            .expect("the busiest day must emit a row carrying refs");
        let ids: Vec<&str> = busiest_row.refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 10, "only the busiest day's ten, got {ids:?}");
        assert!(
            !ids.contains(&"GB-CF:TB-OTHER-DAY"),
            "a contract from another day must not be named, got {ids:?}"
        );
    }
}
