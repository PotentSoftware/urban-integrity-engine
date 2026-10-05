//! `AnalystReviewQueue` Tier-2 probe — ports `probe_analyst_review_queue`
//! (`scripts/analyst_probe.py:1295-1353`), Open Item 11c's live defect class: the pending-review
//! backlog, already closed structurally by [`scan::scan_identity_links_paged`]'s `ORDER BY id`
//! shape (no confidence-DESC ceiling to hide the low-confidence tail).
//!
//! The underlying read is NOT capped — `pending` is the full result of
//! [`scan::scan_identity_links`]. Sorting by confidence descending is DISPLAY-ONLY (a top-10
//! sample); the reported COUNT is `pending.len()`, computed before any sort/truncation touches
//! it.

use std::collections::HashMap;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// The pending-review backlog — see module doc comment.
pub struct AnalystReviewQueue;

#[async_trait::async_trait]
impl Probe for AnalystReviewQueue {
    fn name(&self) -> &'static str {
        "osint-analyst-queue"
    }

    /// **Not** searchable by the findings-by-entity index (PIV-02): its refs are `IdentityLink` record ids, not node ids; same shape as `osint-confirmed-intel`.
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::NamesLinksOnly
    }

    /// A bag of named-entity chips, no dedicated motif renderer (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Chips
    }

    fn tier(&self) -> &'static str {
        "Tier-2"
    }

    fn hypothesis(&self) -> &'static str {
        "How many identity links sit at 0.70-0.84 confidence awaiting analyst sign-off, and \
         which are the highest-confidence ones to clear first?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        let pending = match scan::scan_identity_links(db, Some("pending_review"), None).await {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result — Probe::run is
                // infallible by design (PRB-08).
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("identity_links scan failed: {e}")),
                    summary: "identity_links scan failed — analyst review queue not available."
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

        if pending.is_empty() {
            if !st.osint_active {
                let summary = "No pending_review links. Either OSINT enrichment has not run \
                                yet, or all links are outside the 0.70-0.84 review window."
                    .to_string();
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(summary.clone()),
                    summary,
                    cause: None,
                    // analyst_probe.py:1302 — this branch's DATA_GAP/INFO pair, 1:1.
                    severity: Severity::Info,
                    evidence: vec![EvidenceRow {
                        text: "No probe earlier in the battery set Strategy::osint_active, so \
                               the empty queue cannot be read as a measured clean result."
                            .to_string(),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
            // population = capable = matches = 0 — a measured, genuinely empty queue IS the
            // Clear result (VRD-02), distinct from the DataGap branch above.
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::classify(0, 0, 0, 1, ""),
                summary: "No links currently need analyst review.".to_string(),
                cause: None,
                // analyst_probe.py:1311 — this branch's CLEAR/INFO pair, 1:1.
                severity: Severity::Info,
                evidence: vec![EvidenceRow {
                    text: "OSINT enrichment is active and the pending_review queue is empty — \
                           a measured clean result, not an unmeasured gap."
                        .to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // The COUNT is captured here, BEFORE the display sort/truncation below touches anything
        // — a large queue must never be under-reported by the display cap.
        let n = pending.len() as u64;

        // Display-only top-10 sample, sorted by confidence descending — matches Python's
        // `pending.sort(key=lambda r: r.get("confidence", 0), reverse=True)`.
        let mut display: Vec<&scan::IdentityLinkRow> = pending.iter().collect();
        display.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        // D-02: the top-10 display sample is EVIDENCE, not summary prose — each row carries a
        // typed `IdentityLink` pivot so the analyst can open the exact link this line describes.
        let mut evidence: Vec<EvidenceRow> = vec![EvidenceRow {
            text: format!(
                "Top {} by confidence (confirm with `urban-integrity links confirm <id>`):",
                display.len().min(10)
            ),
            refs: vec![],
        }];
        for r in display.iter().take(10) {
            evidence.push(EvidenceRow {
                text: format!(
                    "{:.3}  {}  ->  {}  [{}]",
                    r.confidence, r.source_node_id, r.target_node_id, r.source
                ),
                refs: vec![RecordRef {
                    kind: RecordKind::IdentityLink,
                    id: r.id.clone(),
                }],
            });
        }
        if display.len() > 10 {
            evidence.push(EvidenceRow {
                text: format!("... and {} more", display.len() - 10),
                refs: vec![],
            });
        }

        // Python's `by_adapter`/`by_type` Counter.most_common(5) blocks
        // (`analyst_probe.py:1341-1342`). `Counter.most_common` has a stable insertion-order
        // tie-break; Rust's `HashMap` iteration order is non-deterministic, so sort explicitly by
        // (count desc, name asc) — the same documented divergence `identity_link_registry.rs`
        // records for its own top-5.
        let mut by_adapter: HashMap<&str, u64> = HashMap::new();
        let mut by_type: HashMap<&str, u64> = HashMap::new();
        for r in &pending {
            *by_adapter.entry(r.source.as_str()).or_insert(0) += 1;
            *by_type.entry(r.resolution_type.as_str()).or_insert(0) += 1;
        }
        let top5 = |counts: &HashMap<&str, u64>| -> Vec<(String, u64)> {
            let mut v: Vec<(String, u64)> =
                counts.iter().map(|(k, n)| ((*k).to_string(), *n)).collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            v.truncate(5);
            v
        };
        evidence.push(EvidenceRow {
            text: "By adapter:".to_string(),
            refs: vec![],
        });
        for (name, n) in top5(&by_adapter) {
            evidence.push(EvidenceRow {
                text: format!("{n} via {name}"),
                refs: vec![],
            });
        }
        evidence.push(EvidenceRow {
            text: "By resolution type:".to_string(),
            refs: vec![],
        });
        for (name, n) in top5(&by_type) {
            evidence.push(EvidenceRow {
                text: format!("{n} {name}"),
                refs: vec![],
            });
        }

        let summary = format!(
            "{} pending analyst sign-off. Unreviewed links are excluded from hot-graph \
             traversal — confirm or reject to activate.",
            plural(n, "link", "links")
        );

        // `hit_threshold: u64::MAX` is the deliberate "never Hit" idiom — Python's vocabulary for
        // this probe's non-empty branch is WEAK-only, same idiom as
        // `temporal_burst.rs`/`ward_concentration.rs`.
        let verdict = Verdict::classify(n, n, n, u64::MAX, "");

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            // analyst_probe.py:1347 — this branch's WEAK/MEDIUM pair, 1:1.
            severity: Severity::Medium,
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_pending_link(db: &Surreal<Any>, idx: usize, confidence: f64) {
        let sni = format!("entity:tier2-queue-src-{idx}");
        let tni = format!("entity:tier2-queue-dst-{idx}");
        db.query(
            "CREATE identity_links CONTENT { \
                 source_node_id: $sni, \
                 target_node_id: $tni, \
                 resolution_type: 'exact_company_number', \
                 confidence: $confidence, \
                 source: 'ch_psc', \
                 resolved_at: d'2024-04-12T00:00:00Z', \
                 evidence_json: '{}', \
                 status: 'pending_review' \
             }",
        )
        .bind(("sni", sni))
        .bind(("tni", tni))
        .bind(("confidence", confidence))
        .await
        .expect("failed to seed identity_links row")
        .check()
        .expect("identity_links CREATE returned a statement error");
    }

    #[tokio::test]
    async fn analyst_review_queue_zero_pending_osint_inactive_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: analyst_review_queue_zero_pending_osint_inactive_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "queue_gap",
        )
        .await;
        let mut st = Strategy {
            osint_active: false,
            ..Strategy::default()
        };

        let probe = AnalystReviewQueue;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero pending rows with osint_active=false must be DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn analyst_review_queue_zero_pending_osint_active_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: analyst_review_queue_zero_pending_osint_active_is_clear (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "queue_clear",
        )
        .await;
        let mut st = Strategy {
            osint_active: true,
            ..Strategy::default()
        };

        let probe = AnalystReviewQueue;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "zero pending rows with osint_active=true must be Clear, got {:?}",
            finding.verdict
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            0,
            "population must be 0 -- zero pending rows in this fixture"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "capable must equal population (0) when Clear is reached"
        );
    }

    #[tokio::test]
    async fn analyst_review_queue_large_backlog_reports_full_count_not_display_cap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: analyst_review_queue_large_backlog_reports_full_count_not_display_cap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "queue_weak",
        )
        .await;
        let mut st = Strategy::default();

        // N = 15 > the display sample's 10-row cap.
        const N: usize = 15;
        for i in 0..N {
            seed_pending_link(&db, i, 0.70 + (i as f64) * 0.001).await;
        }

        let probe = AnalystReviewQueue;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "N > 0 pending rows must be Weak, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains(&N.to_string()),
            "evidence must state the FULL count ({N}), not the 10-row display cap, got: {}",
            finding.summary
        );
    }
}
