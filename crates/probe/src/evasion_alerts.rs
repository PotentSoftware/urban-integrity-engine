//! `EvasionAlerts` Tier-2 probe -- ports `probe_evasion_alerts`
//! (`scripts/analyst_probe.py:2300-2406`). The empty-branch (zero `MotifDetected` rows for
//! `disqualified_director_evasion`) defers to D4's shared [`disqualification_chain`] and applies
//! [`verdict_for_cause`] VERBATIM -- the SAME function `disqualified_active_in_procurement.rs`
//! imports and calls, NOT Python's own local mapping for this probe (`analyst_probe.py:2329-2344`,
//! which returns `DATA_GAP` for `links-not-elevated`). This is a second, deliberate divergence
//! from Python for this probe pair (D4, RPT-01 differential bucket (b)): the underlying fact -- a
//! real excluded population held back by the ENR-11 gate -- means the same thing regardless of
//! which probe is asking, so both probes report `NotApplicable` for that cause, not just
//! `disqualified_active_in_procurement`.

use std::collections::HashSet;

use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{Cause, EvidenceRow, Finding, Severity, Verdict};

use super::disqualification_chain::{disqualification_chain, verdict_for_cause};
use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

pub struct EvasionAlerts;

#[async_trait::async_trait]
impl Probe for EvasionAlerts {
    fn name(&self) -> &'static str {
        "osint-evasion-alerts"
    }

    /// Can-name-entities, deferred: `rows` from `scan_events_by_payload_contains` (~lines
    /// 50-56) are held, and each row's parsed `result_json` (which carries `officer_id`/
    /// `disq_node_id` per the seeded fixture shape) is discarded after only `motif_hash` is read
    /// in the dedup loop (~lines 133-146).
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
        "Does a MotifDetected disqualified_director_evasion alert exist for a director \
         currently subject to a live Companies House disqualification order?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        // CR-02: row-returning scan, `kind` filtered by `scan_events_by_payload_contains` itself
        // (it pages by `kind` first, then filters the payload field in Rust) -- never the
        // indexed-`kind` + unindexed-`CONTAINS` planner-bug shape.
        let rows = match scan::scan_events_by_payload_contains(
            db,
            "MotifDetected",
            "result_json",
            "\"disqualified_director_evasion\"",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("MotifDetected scan failed: {e}")),
                    summary: "MotifDetected scan failed -- evasion alerts cannot be evaluated."
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

        if rows.is_empty() {
            // D4: report the measured break in the chain, applying the SAME `verdict_for_cause`
            // mapping `disqualified_active_in_procurement.rs` uses -- see this module's doc
            // comment for why this deliberately does NOT reproduce Python's own local mapping for
            // this probe.
            let state = disqualification_chain(db).await;
            let excluded = state.excluded_count();
            let verdict = verdict_for_cause(&state);
            // Cause-to-severity mapping is this plan's own convention (not a Python analog --
            // Python's local mapping for this probe is deliberately NOT ported, see this module's
            // doc comment) and must match `disqualified_active_in_procurement.rs` (Wave 2 plan
            // 35-08) exactly, since both probes share the identical `Cause` set from D4's table.
            let severity = match state.cause {
                Cause::NoOfficerNodes | Cause::AdapterNeverRan => Severity::Medium,
                Cause::NoRegisterOverlap | Cause::NoLiveBan => Severity::Info,
                Cause::LinksNotElevated => Severity::Low,
            };
            let counts = format!(
                "Officer nodes: {}. director:* Entity nodes: {}. ch_disqualified links: {} \
                 ({} auto_linked/confirmed). DisqualifiedFrom edges: {}.",
                state.officer_nodes,
                state.director_entities,
                state.links.len(),
                state.elevated_count,
                state.disq_edges
            );
            return Finding {
                probe: self.name().to_string(),
                verdict,
                summary: format!(
                    "No disqualified_director_evasion alerts found (excluded population: \
                     {excluded})."
                ),
                cause: Some(state.cause),
                severity,
                evidence: vec![
                    EvidenceRow {
                        text: state.explain,
                        refs: vec![],
                    },
                    EvidenceRow {
                        text: counts,
                        refs: vec![],
                    },
                ],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // Parse each result_json blob and dedup by motif_hash -- matches Python's `seen_hashes`
        // set exactly (`analyst_probe.py:2370-2377`).
        let mut seen: HashSet<String> = HashSet::new();
        let mut unique_count = 0u64;
        for row in &rows {
            let Some(rj) = row["payload"]["result_json"].as_str() else {
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(rj) else {
                continue;
            };
            let Some(hash) = parsed.get("motif_hash").and_then(|v| v.as_str()) else {
                continue;
            };
            if seen.insert(hash.to_string()) {
                unique_count += 1;
            }
        }

        let unique_str = plural(
            unique_count,
            "confirmed evasion alert",
            "confirmed evasion alerts",
        );
        let summary = format!("{unique_str} ({} total incl. duplicates).", rows.len());

        Finding {
            probe: self.name().to_string(),
            // No magnitude threshold in Python -- any non-empty unique alert set is HIT.
            verdict: Verdict::classify(1, 1, 1, 1, ""),
            summary,
            cause: None,
            // Maps directly to analyst_probe.py:2402's HIT/CRITICAL branch.
            severity: Severity::Critical,
            evidence: vec![EvidenceRow {
                text: "Each alert represents an active director subject to a live Insolvency \
                       Service disqualification order (CDDA 1986) currently directing a \
                       procurement entity. Immediate escalation warranted."
                    .to_string(),
                refs: vec![],
            }],
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

/// Regression guard (Task 2's structural acceptance criterion): this probe's empty-branch D4
/// mapping must resolve through the SAME `verdict_for_cause` function
/// `disqualified_active_in_procurement.rs` calls, never a locally reimplemented match statement
/// -- that is what makes "both probes match D4's table" a structural guarantee rather than a
/// coincidence of two independently-written match arms. `probe_tier2_e2e.rs` (Plan 33-03's file,
/// out of this plan's `files_modified`) is not touched here; this lightweight source-text check
/// lives in this file instead, so it stays in this plan's own scope.
#[cfg(test)]
mod structural_guard {
    #[test]
    fn evasion_alerts_calls_the_shared_verdict_for_cause_not_a_local_reimplementation() {
        let src = include_str!("evasion_alerts.rs");
        assert!(
            src.contains(
                "use super::disqualification_chain::{disqualification_chain, verdict_for_cause};"
            ),
            "evasion_alerts.rs must import verdict_for_cause from disqualification_chain.rs, \
             not reimplement D4's match arms locally"
        );
        assert!(
            src.contains("verdict_for_cause(&state)"),
            "evasion_alerts.rs's empty-branch must call verdict_for_cause(&state), \
             not build its own Verdict per-cause"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_officer_node(db: &Surreal<Any>, officer_id: &str) {
        use urban_integrity_schema::types::node::{NodeWeight, OfficerData};
        let weight = NodeWeight::Officer(OfficerData {
            name: officer_id.to_string(),
            officer_role: None,
            appointed_on: None,
            resigned_on: None,
            company_number: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("node-{officer_id}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeAdded",
                    "payload": { "entity_id": officer_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed officer node");
    }

    async fn seed_enrichment_run(db: &Surreal<Any>, source: &str) {
        db.query(format!(
            "UPSERT enrichment_runs:{source} SET source = '{source}', \
             last_run_at = $last_run_at, candidates_found = $candidates_found, \
             auto_linked = $auto_linked, pending_review = $pending_review, \
             discarded = $discarded"
        ))
        .bind(("last_run_at", 1_756_000_000i64))
        .bind(("candidates_found", 7i64))
        .bind(("auto_linked", 2i64))
        .bind(("pending_review", 1i64))
        .bind(("discarded", 4i64))
        .await
        .expect("seed enrichment_runs row")
        .check()
        .expect("seed enrichment_runs row check");
    }

    async fn seed_ch_disqualified_link(
        db: &Surreal<Any>,
        source_node_id: &str,
        target_node_id: &str,
        status: &str,
    ) {
        db.query(
            "CREATE identity_links CONTENT { \
                 source_node_id: $sni, \
                 target_node_id: $tni, \
                 resolution_type: 'exact_officer', \
                 confidence: 1.0, \
                 source: 'ch_disqualified', \
                 resolved_at: d'2024-04-12T00:00:00Z', \
                 evidence_json: '{}', \
                 status: $status \
             }",
        )
        .bind(("sni", source_node_id.to_string()))
        .bind(("tni", target_node_id.to_string()))
        .bind(("status", status.to_string()))
        .await
        .expect("failed to seed ch_disqualified identity_link")
        .check()
        .expect("ch_disqualified identity_link CREATE returned a statement error");
    }

    async fn seed_evasion_motif(db: &Surreal<Any>, event_id: &str, motif_hash: &str) {
        let result_json = serde_json::json!({
            "motif_type": "disqualified_director_evasion",
            "risk_score": 1.0_f64,
            "description": "test evasion alert",
            "officer_id": "director:jane-smith",
            "disq_node_id": "disq:e1",
            "motif_hash": motif_hash,
        })
        .to_string();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "MotifDetected",
                    "payload": { "result_json": result_json },
                }),
            ))
            .await
            .expect("failed to seed evasion MotifDetected event");
    }

    #[tokio::test]
    async fn no_officer_nodes_maps_to_data_gap_via_shared_chain() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: no_officer_nodes_maps_to_data_gap_via_shared_chain (evasion)");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_evasion", "e1").await;
        let mut st = Strategy::default();

        let probe = EvasionAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(matches!(
            finding.cause,
            Some(urban_integrity_schema::types::verdict::Cause::NoOfficerNodes)
        ));
        assert!(matches!(finding.verdict, Verdict::DataGap { .. }));
    }

    #[tokio::test]
    async fn adapter_never_ran_maps_to_data_gap_via_shared_chain() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: adapter_never_ran_maps_to_data_gap_via_shared_chain (evasion)");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_evasion", "e2").await;
        let mut st = Strategy::default();

        seed_officer_node(&db, "officer:jane-smith").await;

        let probe = EvasionAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(matches!(
            finding.cause,
            Some(urban_integrity_schema::types::verdict::Cause::AdapterNeverRan)
        ));
        assert!(matches!(finding.verdict, Verdict::DataGap { .. }));
    }

    #[tokio::test]
    async fn no_register_overlap_maps_to_clear_via_shared_chain() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: no_register_overlap_maps_to_clear_via_shared_chain (evasion)");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_evasion", "e3").await;
        let mut st = Strategy::default();

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;

        let probe = EvasionAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(matches!(
            finding.cause,
            Some(urban_integrity_schema::types::verdict::Cause::NoRegisterOverlap)
        ));
        // VRD-12: prove the Coverage VALUE end-to-end through the full Probe::run, not just
        // that the Clear branch was reached. The single seeded officer node is the whole population.
        let coverage = match finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            1,
            "one seeded officer node must surface as population 1, not a hardcoded 0"
        );
        assert_eq!(coverage.capable(), 1);
    }

    #[tokio::test]
    async fn links_not_elevated_maps_to_not_applicable_not_datagap_diverging_from_python() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: links_not_elevated_maps_to_not_applicable_not_datagap_diverging_from_python"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_evasion", "e4").await;
        let mut st = Strategy::default();

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;
        seed_ch_disqualified_link(&db, "disq:c1", "officer:jane-smith", "candidate").await;
        seed_ch_disqualified_link(&db, "disq:c2", "officer:john-doe", "candidate").await;

        let probe = EvasionAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(matches!(
            finding.cause,
            Some(urban_integrity_schema::types::verdict::Cause::LinksNotElevated)
        ));
        assert!(
            matches!(finding.verdict, Verdict::NotApplicable { .. }),
            "evasion_alerts must diverge from Python's own DATA_GAP mapping here and report \
             NotApplicable, per D4's single table -- got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains(&2u64.to_string()),
            "the excluded population figure must be present in the evidence string, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn no_live_ban_maps_to_clear_via_shared_chain() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: no_live_ban_maps_to_clear_via_shared_chain (evasion)");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_evasion", "e5").await;
        let mut st = Strategy::default();

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;
        seed_ch_disqualified_link(&db, "disq:e1", "officer:jane-smith", "confirmed").await;

        let probe = EvasionAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(matches!(
            finding.cause,
            Some(urban_integrity_schema::types::verdict::Cause::NoLiveBan)
        ));
        // VRD-12: prove the Coverage VALUE end-to-end through the full Probe::run, not just
        // that the Clear branch was reached. Here the population is the elevated ch_disqualified link count, which is also 1.
        let coverage = match finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            1,
            "one elevated ch_disqualified link must surface as population 1, not a hardcoded 0"
        );
        assert_eq!(coverage.capable(), 1);
    }

    #[tokio::test]
    async fn duplicate_motif_hash_is_deduped_to_one() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: duplicate_motif_hash_is_deduped_to_one (evasion)");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_evasion", "e6").await;
        let mut st = Strategy::default();

        // Two MotifDetected rows sharing one motif_hash -- the reported unique count must be 1.
        seed_evasion_motif(&db, "evasion-dup-1", "sharedhash123").await;
        seed_evasion_motif(&db, "evasion-dup-2", "sharedhash123").await;

        let probe = EvasionAlerts;
        let finding = probe.run(&db, &mut st).await;

        assert!(matches!(finding.verdict, Verdict::Hit { .. }));
        assert!(
            finding.summary.starts_with("1 confirmed evasion alert ("),
            "two rows sharing one motif_hash must dedup to a reported count of 1, got: {}",
            finding.summary
        );
        assert!(
            finding.summary.contains("2 total incl. duplicates"),
            "the total-including-duplicates figure must still be reported, got: {}",
            finding.summary
        );
    }
}
