//! `ElectoralCommissionDonations` -- ports `probe_electoral_commission_donations`
//! (`scripts/analyst_probe.py:1928-2029`).
//!
//! DonatesTo has exactly one registered producer (`electoral_commission`), so the edge count
//! routes through `attributed::from_rows` with no discriminator needed and the refusal path
//! unreachable -- still called through `Attributed` per ROADMAP criterion 6's "every count
//! presented as evidence for a named source" requirement, not because a refusal could otherwise
//! fire here. The rows are fetched once via `scan::scan_events_by_payload_contains` (rather than
//! going through `count_for_source`'s internal scan) so the same rows that produced the count can
//! also produce `RecordRef`s for the donor-side entity ids (phase 49 plan 8, Task 2) -- the
//! fetch-then-attribute shape `disqualified_director_edges.rs`'s `Controls` widening (phase 49
//! plan 4) already established.
//!
//! Both-empty (`ec_links` AND `donates_count` both zero) routes through the D6
//! `Strategy.enrichment_runs` memo, same Clear-vs-DataGap split `psc_beneficial_ownership.rs`
//! uses. Otherwise: `HIT if confirmed else (WEAK if pending or donates_count > 0 else
//! DATA_GAP)` -- Python's own fall-through has no CLEAR branch once `ec_links` is non-empty (or
//! `donates_count > 0`), so the "candidate/rejected-only, zero DonatesTo" case is DataGap, not an
//! invented Clear -- deliberately NOT modelled through `Verdict::classify`'s Hit/Weak/Clear rule
//! alone, since that rule has no DataGap outcome to fall back to.
//!
//! `DonatesTo` edges are asymmetric: `src_id` is the donor entity already in the graph, `dst_id`
//! is a `PoliticalActor` node (`PA:<slug>`), not an `Entity`. Only the donor side is ref-able as
//! `RecordKind::Entity` (phase 49 plan 8, Task 2).

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::{queries, scan};
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, SourceAttribution, Verdict,
};

use super::attributed;
use super::{EvidenceScope, MotifKind, Probe, Strategy};

/// Cap on how many entity ids are individually named in an evidence row -- a DISPLAY cap only,
/// never applied to the reported count. Mirrors `disqualified_director_edges.rs`'s identical
/// constant.
const DISPLAY_CAP: usize = 8;

/// UNIX-seconds timestamp -> `YYYY-MM-DD`, mirroring Python's `_ts_to_day`
/// (`scripts/analyst_probe.py:364-371`). See `psc_beneficial_ownership.rs`'s identical helper for
/// why no millisecond normalisation is needed.
fn day_of(last_run_at: i64) -> String {
    chrono::DateTime::from_timestamp(last_run_at, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| last_run_at.to_string())
}

pub struct ElectoralCommissionDonations;

#[async_trait::async_trait]
impl Probe for ElectoralCommissionDonations {
    fn name(&self) -> &'static str {
        "phase20-ec-donations"
    }

    /// Searchable by the findings-by-entity index (PIV-02) as of Phase 49: confirmed EC
    /// identity-links name the target node id, and (per Task 2 of this plan) `DonatesTo` edges
    /// name their donor-side entity id -- both halves this probe already computes are refed,
    /// nothing deferred to a later plan.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::NamesNodes
    }

    /// A bag of named-entity chips, no dedicated motif renderer (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Chips
    }

    fn tier(&self) -> &'static str {
        "Tier-2"
    }

    fn hypothesis(&self) -> &'static str {
        "Do Electoral Commission donation records exist for entities also present in the \
         procurement graph?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        let ec_links = match scan::scan_identity_links(db, None, Some("electoral_commission")).await
        {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result -- Probe::run is
                // infallible by design (PRB-08). No Python analog -- defaulted to Medium per
                // 35-01/35-02's analog-less-DataGap convention. Before the `attributed::` call,
                // so nothing measured to attribute.
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!(
                        "electoral_commission identity_links scan failed: {e}"
                    )),
                    summary: "electoral_commission identity_links scan failed -- EC donations \
                              cannot be evaluated."
                        .to_string(),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // DonatesTo has exactly one registered producer -- the refusal path is unreachable, but
        // the count is still routed through `Attributed` per ROADMAP criterion 6. Fetch the raw
        // rows once (needed for the donor-id display list below) and derive the count from those
        // SAME rows via `from_rows`, rather than issuing a second, redundant query for the count
        // alone -- the same fetch-then-attribute shape
        // `disqualified_director_edges.rs`'s `Controls` widening already established.
        let donates_rows = match scan::scan_events_by_payload_contains(
            db,
            "EdgeAdded",
            "weight_json",
            "\"type\":\"DonatesTo\"",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                // No Python analog -- defaulted to Medium per 35-01/35-02's analog-less-DataGap
                // convention. Nothing collected yet, so attributions: vec![].
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("DonatesTo scan failed: {e}")),
                    summary: format!("DonatesTo scan failed: {e}"),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };
        let donates =
            match attributed::from_rows("DonatesTo", "electoral_commission", &donates_rows) {
                Ok(a) => a,
                Err(e) => {
                    // No Python analog -- `Attributed` refusal is this milestone's own mechanism.
                    // Defaulted to Medium per 35-01/35-02's analog-less-DataGap convention. The call
                    // itself failed, so nothing was measured to attribute.
                    return Finding {
                        probe: self.name().to_string(),
                        verdict: Verdict::data_gap(format!("DonatesTo attribution failed: {e}")),
                        summary: format!("DonatesTo attribution failed: {e}"),
                        cause: None,
                        severity: Severity::Medium,
                        evidence: vec![],
                        attributions: vec![],
                        remediation_hint: None,
                    };
                }
            };
        let donates_count = donates.value;
        // `donates` succeeded past this point -- every branch below is downstream of the single
        // `attributed::` call in this file, so all of them report the SAME attribution list.
        let attributions: Vec<SourceAttribution> = donates
            .producers
            .iter()
            .map(|&(source, value)| SourceAttribution { source, value })
            .collect();

        if ec_links.is_empty() && donates_count == 0 {
            let marker = st
                .enrichment_runs(db)
                .await
                .get("electoral_commission")
                .cloned();
            return match marker {
                Some(marker) => {
                    // Ran, zero graph overlap -- a data-coverage observation, not an action
                    // item. Same D6 mechanism as `psc_beneficial_ownership.rs`. Python analog:
                    // CLEAR/INFO (analyst_probe.py:1949).
                    let day = day_of(marker.last_run_at);
                    let summary = format!("electoral_commission ran {day} -- 0 graph overlap.");
                    Finding {
                        probe: self.name().to_string(),
                        // Zero ec_links and zero DonatesTo edges IS the population this probe
                        // evaluated -- matches=0 -> Clear unconditionally.
                        verdict: Verdict::classify(0, 0, 0, 1, ""),
                        summary,
                        cause: None,
                        severity: Severity::Info,
                        evidence: vec![EvidenceRow {
                            text: format!(
                                "candidates_found={}, 0 linked, 0 DonatesTo edges. The EC \
                                 donations adapter executed; no donor in the EC register \
                                 intersects an entity currently in the graph. No action \
                                 required.",
                                marker.candidates_found
                            ),
                            refs: vec![],
                        }],
                        attributions: attributions.clone(),
                        remediation_hint: None,
                    }
                }
                // Python analog: DATA_GAP/MEDIUM (analyst_probe.py:1961).
                None => Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(
                        "No Electoral Commission identity links or DonatesTo edges found",
                    ),
                    summary: "No Electoral Commission identity links or DonatesTo edges found."
                        .to_string(),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: "Run `urban-integrity enrich --source electoral` to download the \
                               EC donations CSV."
                            .to_string(),
                        refs: vec![],
                    }],
                    attributions: attributions.clone(),
                    remediation_hint: None,
                },
            };
        }

        let total = ec_links.len() as u64;
        let confirmed: Vec<_> = ec_links
            .iter()
            .filter(|r| r.status == "auto_linked" || r.status == "confirmed")
            .collect();
        let pending: Vec<_> = ec_links
            .iter()
            .filter(|r| r.status == "pending_review")
            .collect();

        let summary = format!(
            "{}: {} confirmed, {} pending. {} in graph.",
            super::plural(total, "EC donation link", "EC donation links"),
            confirmed.len(),
            pending.len(),
            super::plural(donates_count, "DonatesTo edge", "DonatesTo edges")
        );

        // No `Verdict::classify` fallback here -- Python's own table has no CLEAR outcome once
        // this branch is reached at all (see module doc comment), so the "candidate/rejected
        // only, zero pending, zero DonatesTo" case is DataGap by explicit construction, never an
        // invented Clear.
        let verdict = if !confirmed.is_empty() {
            Verdict::classify(
                total,
                total,
                confirmed.len() as u64,
                confirmed.len() as u64,
                "",
            )
        } else if !pending.is_empty() || donates_count > 0 {
            // Sentinel: 0 < 1 < 2 -> Weak, never Clear/Hit.
            Verdict::classify(total, total, 1, 2, "")
        } else {
            Verdict::data_gap(
                "EC identity links present but none confirmed, pending, or DonatesTo-edge-backed",
            )
        };
        // Python analog: severity = "HIGH" if confirmed else "MEDIUM" (analyst_probe.py:2013) --
        // applies uniformly to both the Weak and DataGap sub-branches above, exactly as Python's
        // single `severity` assignment does.
        let severity = if !confirmed.is_empty() {
            Severity::High
        } else {
            Severity::Medium
        };

        // Task 1: confirmed identity-link targets -- capped `RecordRef`s built from `confirmed`
        // (already computed above), sorted, deduped, capped at `DISPLAY_CAP`.
        let mut evidence: Vec<EvidenceRow> = Vec::new();
        if !confirmed.is_empty() {
            let mut ids: Vec<String> = confirmed.iter().map(|r| r.target_node_id.clone()).collect();
            ids.sort();
            ids.dedup();
            let capped: Vec<&String> = ids.iter().take(DISPLAY_CAP).collect();
            evidence.push(EvidenceRow {
                text: format!(
                    "Confirmed EC donor/recipient nodes ({} unique, showing up to {DISPLAY_CAP}): {}",
                    ids.len(),
                    capped
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                refs: capped
                    .iter()
                    .map(|id| RecordRef {
                        kind: RecordKind::Entity,
                        id: (*id).clone(),
                    })
                    .collect(),
            });
        }

        // Task 2: DonatesTo edges' donor-side (`src_id`) entity ids -- capped `RecordRef`s built
        // from `donates_rows` (already fetched above). `dst_id` is a `PoliticalActor` node, not
        // an `Entity`, and is deliberately excluded (module doc comment).
        if donates_count > 0 {
            let mut ids: Vec<String> = donates_rows
                .iter()
                .filter_map(|r| queries::extract_id_string(&r["payload"]["src_id"]))
                .collect();
            ids.sort();
            ids.dedup();
            let capped: Vec<&String> = ids.iter().take(DISPLAY_CAP).collect();
            evidence.push(EvidenceRow {
                text: format!(
                    "Donor entity nodes with DonatesTo edges ({} unique, showing up to \
                     {DISPLAY_CAP}): {}",
                    ids.len(),
                    capped
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                refs: capped
                    .iter()
                    .map(|id| RecordRef {
                        kind: RecordKind::Entity,
                        id: (*id).clone(),
                    })
                    .collect(),
            });
        }

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            severity,
            evidence,
            attributions,
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight};

    /// Local duplicate of `scan.rs`'s private `seed_identity_link` test helper -- see
    /// `psc_beneficial_ownership.rs`'s identical copy for the rationale.
    async fn seed_identity_link(
        db: &Surreal<Any>,
        key: &str,
        source_node_id: &str,
        target_node_id: &str,
        confidence: f64,
        source: &str,
        status: &str,
    ) {
        db.query(
            "CREATE type::record('identity_links', $key) SET \
             source_node_id = $src, target_node_id = $tgt, resolution_type = 'exact_ch_number', \
             confidence = $conf, source = $source, resolved_at = $resolved_at, \
             evidence_json = '{}', status = $status",
        )
        .bind(("key", key.to_string()))
        .bind(("src", source_node_id.to_string()))
        .bind(("tgt", target_node_id.to_string()))
        .bind(("conf", confidence))
        .bind(("source", source.to_string()))
        .bind(("status", status.to_string()))
        .bind(("resolved_at", chrono::Utc::now()))
        .await
        .expect("seed identity_links row")
        .check()
        .expect("seed identity_links row check");
    }

    /// Local duplicate of `surreal_integration.rs`'s private `seed_enrichment_run` -- see
    /// `psc_beneficial_ownership.rs`'s identical copy for the rationale.
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

    async fn seed_donates_to_edge(db: &Surreal<Any>, event_id: &str, src_id: &str, dst_id: &str) {
        let edge = EdgeWeight::DonatesTo {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
        };
        let weight_json = serde_json::to_string(&edge).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed DonatesTo edge");
    }

    #[tokio::test]
    async fn ec_zero_links_zero_donates_no_marker_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ec_zero_links_zero_donates_no_marker_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_no_marker",
        )
        .await;
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero ec_links and zero DonatesTo edges with no marker must be DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn ec_zero_links_zero_donates_with_marker_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ec_zero_links_zero_donates_with_marker_is_clear (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_with_marker",
        )
        .await;
        seed_enrichment_run(&db, "electoral_commission", 13).await;
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "zero ec_links/DonatesTo but a live enrichment_runs marker must be Clear, never \
             DataGap (D6's proof), got {:?}",
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
        // The marker's candidates_found: 13 is a self-reported adapter statistic surfaced in
        // evidence text, NOT the Coverage population. The population this probe reports is
        // the zero measured links/DonatesTo edges -- the two numbers must not be confused.
        assert_eq!(
            coverage.population(),
            0,
            "population must be 0 -- zero measured ec links and DonatesTo edges"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "capable must equal population (0) on the zero-link marker branch"
        );
    }

    #[tokio::test]
    async fn ec_confirmed_link_is_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: ec_confirmed_link_is_hit (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_confirmed",
        )
        .await;
        seed_identity_link(
            &db,
            "ec_confirmed_1",
            "donor:acme-holdings",
            "GB-COH:12345678",
            1.0,
            "electoral_commission",
            "auto_linked",
        )
        .await;
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "a confirmed EC donation link must be Hit, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn ec_zero_confirmed_but_donates_edges_present_is_weak() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ec_zero_confirmed_but_donates_edges_present_is_weak (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_donates_only",
        )
        .await;
        // Zero identity_links, but a real DonatesTo edge exists -- must still route past the
        // both-empty gate (donates_count > 0) into the confirmed/pending/donates_count table,
        // landing on WEAK per Python's `donates_count > 0` disjunct.
        seed_donates_to_edge(&db, "donates-1", "donor:acme-holdings", "GB-COH:12345678").await;
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "zero confirmed but a real DonatesTo edge must be Weak, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn ec_candidate_only_zero_donates_is_data_gap_not_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ec_candidate_only_zero_donates_is_data_gap_not_clear (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_candidate_only",
        )
        .await;
        // Non-empty ec_links (candidate status only), zero confirmed, zero pending, zero
        // DonatesTo edges -- Python's fall-through has NO Clear branch here.
        seed_identity_link(
            &db,
            "ec_candidate_1",
            "donor:maybe-holdings",
            "GB-COH:87654321",
            0.3,
            "electoral_commission",
            "candidate",
        )
        .await;
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "candidate-only ec_links with zero pending and zero DonatesTo edges must be \
             DataGap, proving there is no invented Clear branch, got {:?}",
            finding.verdict
        );
    }

    /// Task 3: 9 distinct confirmed identity-links, zero pending, zero DonatesTo edges --
    /// proves the confirmed-links ref row is correctly capped and kinded.
    #[tokio::test]
    async fn ec_donations_confirmed_links_attach_capped_entity_refs() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ec_donations_confirmed_links_attach_capped_entity_refs (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_confirmed_capped_refs",
        )
        .await;
        for i in 0..9 {
            seed_identity_link(
                &db,
                &format!("ec_confirmed_capped_{i}"),
                &format!("donor:acme-{i}"),
                &format!("GB-COH:{i:08}"),
                1.0,
                "electoral_commission",
                "auto_linked",
            )
            .await;
        }
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "9 confirmed links must be Hit, got {:?}",
            finding.verdict
        );
        let confirmed_row = finding
            .evidence
            .iter()
            .find(|r| r.text.contains("Confirmed EC donor/recipient nodes"))
            .expect("a Confirmed EC donor/recipient nodes evidence row must be present");
        assert!(
            confirmed_row.text.contains("9 unique"),
            "expected the text to state 9 unique ids, got {:?}",
            confirmed_row.text
        );
        assert_eq!(
            confirmed_row.refs.len(),
            DISPLAY_CAP,
            "expected exactly DISPLAY_CAP (8) refs, got {}",
            confirmed_row.refs.len()
        );
        assert!(
            confirmed_row
                .refs
                .iter()
                .all(|r| r.kind == RecordKind::Entity),
            "every confirmed-link ref must be RecordKind::Entity, got {:?}",
            confirmed_row.refs
        );
    }

    /// Task 3: only pending identity-links (zero confirmed, zero DonatesTo) -- proves no ref is
    /// attached when neither widening's population is non-empty.
    #[tokio::test]
    async fn ec_donations_pending_only_attaches_no_refs() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: ec_donations_pending_only_attaches_no_refs (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_pending_only_no_refs",
        )
        .await;
        seed_identity_link(
            &db,
            "ec_pending_only_1",
            "donor:maybe-holdings",
            "GB-COH:11111111",
            0.5,
            "electoral_commission",
            "pending_review",
        )
        .await;
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            finding.evidence.iter().all(|r| r.refs.is_empty()),
            "pending-only, zero DonatesTo must carry no refs on any evidence row, got {:?}",
            finding.evidence
        );
    }

    /// Task 3: 9 distinct DonatesTo edges (distinct donor `src_id` per edge), zero
    /// identity-links -- proves the DonatesTo-refs row is correctly capped and kinded.
    #[tokio::test]
    async fn ec_donations_donates_to_edges_attach_capped_entity_refs() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ec_donations_donates_to_edges_attach_capped_entity_refs (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_donates_capped_refs",
        )
        .await;
        for i in 0..9 {
            seed_donates_to_edge(
                &db,
                &format!("ec_donates_capped_{i}"),
                &format!("donor:donor-{i}"),
                "PA:some-party",
            )
            .await;
        }
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "zero confirmed links but 9 DonatesTo edges must be Weak, got {:?}",
            finding.verdict
        );
        let donates_row = finding
            .evidence
            .iter()
            .find(|r| r.text.contains("Donor entity nodes with DonatesTo edges"))
            .expect("a Donor entity nodes with DonatesTo edges evidence row must be present");
        assert!(
            donates_row.text.contains("9 unique"),
            "expected the text to state 9 unique ids, got {:?}",
            donates_row.text
        );
        assert_eq!(
            donates_row.refs.len(),
            DISPLAY_CAP,
            "expected exactly DISPLAY_CAP (8) refs, got {}",
            donates_row.refs.len()
        );
        assert!(
            donates_row
                .refs
                .iter()
                .all(|r| r.kind == RecordKind::Entity),
            "every DonatesTo ref must be RecordKind::Entity, got {:?}",
            donates_row.refs
        );
    }

    /// Task 3: one confirmed identity-link AND one DonatesTo edge together, with different ids
    /// on each side -- proves neither widening regressed the other's ref path.
    #[tokio::test]
    async fn ec_donations_confirmed_links_and_donates_edges_both_attach_refs() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: ec_donations_confirmed_links_and_donates_edges_both_attach_refs (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "ec_confirmed_and_donates_both",
        )
        .await;
        seed_identity_link(
            &db,
            "ec_both_link_1",
            "donor:link-side",
            "GB-COH:22222222",
            1.0,
            "electoral_commission",
            "confirmed",
        )
        .await;
        seed_donates_to_edge(&db, "ec_both_edge_1", "donor:edge-side", "PA:some-party").await;
        let mut st = Strategy::default();

        let probe = ElectoralCommissionDonations;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "a confirmed link plus a DonatesTo edge must be Hit, got {:?}",
            finding.verdict
        );
        let confirmed_row = finding
            .evidence
            .iter()
            .find(|r| r.text.contains("Confirmed EC donor/recipient nodes"))
            .expect("a Confirmed EC donor/recipient nodes evidence row must be present");
        let donates_row = finding
            .evidence
            .iter()
            .find(|r| r.text.contains("Donor entity nodes with DonatesTo edges"))
            .expect("a Donor entity nodes with DonatesTo edges evidence row must be present");
        assert_eq!(
            confirmed_row.refs.len(),
            1,
            "expected exactly 1 confirmed-link ref, got {:?}",
            confirmed_row.refs
        );
        assert_eq!(
            donates_row.refs.len(),
            1,
            "expected exactly 1 DonatesTo ref, got {:?}",
            donates_row.refs
        );
        assert_ne!(
            confirmed_row.refs[0].id, donates_row.refs[0].id,
            "the two halves' refs must carry different ids, got {:?} vs {:?}",
            confirmed_row.refs[0].id, donates_row.refs[0].id
        );
        assert_eq!(confirmed_row.refs[0].kind, RecordKind::Entity);
        assert_eq!(donates_row.refs[0].kind, RecordKind::Entity);
    }
}
