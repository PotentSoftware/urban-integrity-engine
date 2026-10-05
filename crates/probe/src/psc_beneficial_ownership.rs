//! `PscBeneficialOwnership` -- ports `probe_psc_beneficial_ownership`
//! (`scripts/analyst_probe.py:1539-1616`).
//!
//! Zero `ch_psc` identity_links routes through the D6 `Strategy.enrichment_runs` memo -- `Clear`
//! when the `"ch_psc"` marker is present (adapter ran, genuinely zero graph overlap, the same D6
//! mechanism `disqualification_chain`'s sibling probes apply to a single-marker check), `DataGap`
//! when it is absent (adapter has never run at all). The non-empty branch mirrors Python's
//! `HIT if confirmed else (WEAK if pending else CLEAR)` table on `confirmed`/`pending` link
//! counts -- there is no magnitude threshold in that table, only "is `confirmed` non-empty" then
//! "is `pending` non-empty", so `Verdict::classify`'s `matches`/`hit_threshold` pair is chosen
//! per-branch to reproduce it exactly (mirrors `ward_concentration.rs`'s documented non-obvious
//! use of the same API for an identical "no threshold, just non-empty" shape).

use std::collections::HashSet;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};

use super::{EvidenceScope, MotifKind, Probe, Strategy};

/// Cap on how many officer node ids are individually named in the evidence text and attached as
/// `RecordRef`s -- a DISPLAY cap only, never applied to the reported count. Mirrors
/// `disqualified_director_edges.rs`'s `DISPLAY_CAP` (T-49-01: `graph_href_for_refs` has no URL
/// cap downstream, so an uncapped ref list is a real deep-link overflow risk).
const DISPLAY_CAP: usize = 8;

/// UNIX-seconds timestamp -> `YYYY-MM-DD`, mirroring Python's `_ts_to_day`
/// (`scripts/analyst_probe.py:364-371`). `RunMarker.last_run_at` is always plain UNIX seconds
/// (`crates/core/src/persistence/scan.rs`'s doc comment on the sole writer,
/// `crates/enricher/src/engine.rs:414-437`), never milliseconds, so no `while n > … { n /= 1000 }`
/// normalisation is needed here.
fn day_of(last_run_at: i64) -> String {
    chrono::DateTime::from_timestamp(last_run_at, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| last_run_at.to_string())
}

pub struct PscBeneficialOwnership;

#[async_trait::async_trait]
impl Probe for PscBeneficialOwnership {
    fn name(&self) -> &'static str {
        "psc-beneficial-ownership"
    }

    /// Searchable by the findings-by-entity index (PIV-02) as of Phase 49: confirmed
    /// beneficial-ownership links name the officer node id they resolved to.
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
        "Do PSC beneficial-ownership records exist for entities in the procurement graph, and do \
         any indicate control by a disqualified or sanctioned individual?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        let psc_links = match scan::scan_identity_links(db, None, Some("ch_psc")).await {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result -- Probe::run is
                // infallible by design (PRB-08).
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("ch_psc identity_links scan failed: {e}")),
                    summary: "ch_psc identity_links scan failed -- beneficial ownership cannot \
                              be evaluated."
                        .to_string(),
                    cause: None,
                    // No Python analog (this scan-failure branch is Rust-specific
                    // infrastructure handling, PRB-08) — defaulted to Severity::Medium per
                    // 35-01's convention for an analog-less DataGap branch.
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: format!("ch_psc identity_links scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        if psc_links.is_empty() {
            let marker = st.enrichment_runs(db).await.get("ch_psc").cloned();
            return match marker {
                Some(marker) => {
                    // Ran, zero graph overlap -- a data-coverage observation, not an action
                    // item. D6: the same lazy memo `disqualification_chain`'s sibling probes
                    // read, applied here to a single-marker check.
                    let day = day_of(marker.last_run_at);
                    let summary = format!(
                        "ch_psc ran {day} -- 0 graph overlap (candidates_found={}, 0 linked). \
                         The PSC adapter executed; the Companies House PSC register simply does \
                         not intersect any entity currently in the graph. No action required.",
                        marker.candidates_found
                    );
                    Finding {
                        probe: self.name().to_string(),
                        // population/capable are both 0 -- zero psc_links IS the population this
                        // probe evaluated. matches=0 -> Clear unconditionally regardless of
                        // hit_threshold.
                        verdict: Verdict::classify(0, 0, 0, 1, ""),
                        summary,
                        cause: None,
                        // Python's probe_psc_beneficial_ownership (analyst_probe.py:1554-1559)
                        // reports "INFO" for this exact branch.
                        severity: Severity::Info,
                        evidence: vec![],
                        attributions: vec![],
                        remediation_hint: None,
                    }
                }
                None => Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap("No ch_psc identity links found"),
                    summary: "No ch_psc identity links found -- run `urban-integrity enrich \
                              --source psc` with URBAN__ENRICHER__CH__API_KEY set to populate \
                              beneficial ownership chains."
                        .to_string(),
                    cause: None,
                    // Python's probe_psc_beneficial_ownership (analyst_probe.py:1561-1567)
                    // reports "HIGH" for this exact branch -- no adapter has ever run.
                    severity: Severity::High,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
            };
        }

        let total = psc_links.len() as u64;
        let confirmed: Vec<_> = psc_links
            .iter()
            .filter(|r| r.status == "auto_linked" || r.status == "confirmed")
            .collect();
        let pending: Vec<_> = psc_links
            .iter()
            .filter(|r| r.status == "pending_review")
            .collect();
        let officer_targets: HashSet<&str> = confirmed
            .iter()
            .map(|r| r.target_node_id.as_str())
            .collect();

        // See module doc comment: classify's matches/hit_threshold pair is chosen per-branch so
        // its own Hit/Weak/Clear rule reproduces Python's 3-way table exactly, with no magnitude
        // threshold semantics invented along the way.
        let (matches, hit_threshold) = if !confirmed.is_empty() {
            // matches == hit_threshold -> Hit, regardless of magnitude.
            (confirmed.len() as u64, confirmed.len() as u64)
        } else if !pending.is_empty() {
            // Sentinel: 0 < 1 < 2 -> Weak, never Clear/Hit.
            (1, 2)
        } else {
            // matches == 0 -> Clear unconditionally; hit_threshold is unused on this path.
            (0, 1)
        };
        let verdict = Verdict::classify(total, total, matches, hit_threshold, "");

        let summary = format!(
            "{} across {}.",
            super::plural(
                confirmed.len() as u64,
                "confirmed beneficial-ownership link",
                "confirmed beneficial-ownership links"
            ),
            super::plural(
                officer_targets.len() as u64,
                "officer node",
                "officer nodes"
            )
        );
        let mut evidence = vec![EvidenceRow {
            text: format!(
                "Total ch_psc {} scanned.",
                super::plural(total, "identity link", "identity links")
            ),
            refs: vec![],
        }];
        if !pending.is_empty() {
            evidence.push(EvidenceRow {
                text: format!(
                    "{} awaiting analyst review.",
                    super::plural(pending.len() as u64, "link", "links")
                ),
                refs: vec![],
            });
        }
        if !confirmed.is_empty() {
            let mut ids: Vec<String> = confirmed.iter().map(|r| r.target_node_id.clone()).collect();
            ids.sort();
            ids.dedup();
            // Same DISPLAY_CAP slice feeds both the text list and the RecordRef list below --
            // one capped iteration, no separate uncapped pass (mirrors
            // disqualified_director_edges.rs's DISPLAY_CAP shape).
            let capped: Vec<&String> = ids.iter().take(DISPLAY_CAP).collect();
            evidence.push(EvidenceRow {
                text: format!(
                    "Officer nodes named by confirmed beneficial-ownership links ({} unique, \
                     showing up to {DISPLAY_CAP}): {}",
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
            // Python's probe_psc_beneficial_ownership (analyst_probe.py:1601) computes
            // `severity = "HIGH" if confirmed else "MEDIUM"` for this exact branch.
            severity: if !confirmed.is_empty() {
                Severity::High
            } else {
                Severity::Medium
            },
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors `crates/core/src/persistence/scan.rs`'s private `seed_identity_link` test helper
    /// -- neither that one nor any other is exported, so this is a local equivalent per this
    /// codebase's established convention of duplicating small seed helpers per test module
    /// (`ward_concentration.rs`'s local `record_id_str` is the same pattern applied to a
    /// non-test helper).
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

    /// Mirrors `crates/core/tests/surreal_integration.rs`'s private `seed_enrichment_run` --
    /// same local-duplication rationale as `seed_identity_link` above.
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

    #[tokio::test]
    async fn psc_zero_links_no_marker_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: psc_zero_links_no_marker_is_data_gap (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "psc_no_marker",
        )
        .await;
        let mut st = Strategy::default();

        let probe = PscBeneficialOwnership;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero ch_psc links with no enrichment_runs marker must be DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn psc_zero_links_with_marker_is_clear_not_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: psc_zero_links_with_marker_is_clear_not_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "psc_with_marker",
        )
        .await;
        seed_enrichment_run(&db, "ch_psc", 42).await;
        let mut st = Strategy::default();

        let probe = PscBeneficialOwnership;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "zero ch_psc links but a live enrichment_runs marker must be Clear, never DataGap \
             (D6's proof), got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("42"),
            "evidence must cite the marker's candidates_found, got: {}",
            finding.summary
        );
        assert!(
            finding.summary.contains("2025-08-24"),
            "evidence must cite the marker's last_run_at (as a formatted day), got: {}",
            finding.summary
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // A genuine zero-population Clear: the marker proves the adapter ran, and it found
        // nothing. The 42 in `candidates_found` above is a self-reported adapter statistic
        // surfaced in evidence text -- it is NOT the Coverage population, which is the zero
        // measured links. This is the VRD-11 hazard shape in its purest form.
        assert_eq!(
            coverage.population(),
            0,
            "population must be 0 -- the marker branch reports zero measured ch_psc links"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "capable must equal population (0) on the zero-link marker branch"
        );
    }

    #[tokio::test]
    async fn psc_confirmed_links_is_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: psc_confirmed_links_is_hit (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "psc_confirmed",
        )
        .await;
        seed_identity_link(
            &db,
            "psc_confirmed_1",
            "psc:jane-doe",
            "director:jane-doe",
            0.9,
            "ch_psc",
            "auto_linked",
        )
        .await;
        let mut st = Strategy::default();

        let probe = PscBeneficialOwnership;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "a confirmed PSC link must be Hit, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn psc_beneficial_ownership_confirmed_links_attach_capped_entity_refs() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: psc_beneficial_ownership_confirmed_links_attach_capped_entity_refs \
                 (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "psc_refs_capped",
        )
        .await;
        // 9 distinct confirmed links, one more than DISPLAY_CAP (8), each with a distinct
        // target_node_id.
        for i in 0..9 {
            seed_identity_link(
                &db,
                &format!("psc_refs_capped_{i}"),
                &format!("psc:planted-{i}"),
                &format!("officer:planted-{i}"),
                0.9,
                "ch_psc",
                "confirmed",
            )
            .await;
        }
        let mut st = Strategy::default();

        let probe = PscBeneficialOwnership;
        let finding = probe.run(&db, &mut st).await;

        let refs_row = finding
            .evidence
            .iter()
            .find(|e| !e.refs.is_empty())
            .expect("a ref-bearing evidence row must exist for 9 confirmed links");
        assert_eq!(
            refs_row.refs.len(),
            DISPLAY_CAP,
            "ref count must be capped at DISPLAY_CAP even though 9 distinct ids exist, got {:?}",
            refs_row.refs
        );
        assert!(
            refs_row.refs.iter().all(|r| r.kind == RecordKind::Entity),
            "every ref must be RecordKind::Entity, got {:?}",
            refs_row.refs
        );
        assert!(
            refs_row.text.contains("9 unique"),
            "evidence text must state the true total (9 unique), got: {}",
            refs_row.text
        );
    }

    #[tokio::test]
    async fn psc_beneficial_ownership_pending_only_attaches_no_refs() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: psc_beneficial_ownership_pending_only_attaches_no_refs \
                 (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "psc_refs_pending_only",
        )
        .await;
        seed_identity_link(
            &db,
            "psc_refs_pending_only_1",
            "psc:pending-only",
            "director:pending-only",
            0.75,
            "ch_psc",
            "pending_review",
        )
        .await;
        let mut st = Strategy::default();

        let probe = PscBeneficialOwnership;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            finding.evidence.iter().all(|e| e.refs.is_empty()),
            "no evidence row may carry refs when confirmed is empty, got {:?}",
            finding.evidence
        );
    }

    #[tokio::test]
    async fn psc_pending_only_is_weak() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: psc_pending_only_is_weak (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "psc_pending",
        )
        .await;
        seed_identity_link(
            &db,
            "psc_pending_1",
            "psc:john-smith",
            "director:john-smith",
            0.75,
            "ch_psc",
            "pending_review",
        )
        .await;
        let mut st = Strategy::default();

        let probe = PscBeneficialOwnership;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "zero confirmed but a pending PSC link must be Weak, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn psc_candidate_only_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: psc_candidate_only_is_clear (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "psc_candidate",
        )
        .await;
        seed_identity_link(
            &db,
            "psc_candidate_1",
            "psc:al-jones",
            "director:al-jones",
            0.4,
            "ch_psc",
            "candidate",
        )
        .await;
        let mut st = Strategy::default();

        let probe = PscBeneficialOwnership;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "candidate-only (no confirmed, no pending) PSC links must be Clear, got {:?}",
            finding.verdict
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // NOT a zero-population Clear. `total` is ALL ch_psc links (1 seeded here), and the
        // candidate status only keeps it out of the confirmed/pending sets -- so this is
        // "1 record checked, 0 matches", not "nothing was there". The plan predicted 0;
        // pinning the real value is what VRD-12 is for.
        assert_eq!(
            coverage.population(),
            1,
            "population is the total ch_psc link count (1 seeded), not the confirmed/pending count"
        );
        assert_eq!(
            coverage.capable(),
            1,
            "capable must equal population -- the seeded link was readable"
        );
    }
}
