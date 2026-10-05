//! `IdentityLinkRegistry` Tier-2 probe — ports `probe_identity_link_registry`
//! (`scripts/analyst_probe.py:1158-1230`), the OSINT-layer health check. This is the one probe
//! whose `Strategy` writes (`osint_active`, `confirmed_link_count`, `pending_review_count`) every
//! other Tier-2 probe in the real battery (Plan 33-09) reads — its behavior must be exactly right.
//!
//! Reads the WHOLE `identity_links` table unfiltered (`source`-filtered attribution does not
//! apply here — this probe's population IS every row, regardless of adapter) via
//! [`scan::scan_identity_links`]. When the table is empty, D6's `Strategy::enrichment_runs` memo
//! is what distinguishes "OSINT never ran" (`DataGap`) from "OSINT ran, found zero cross-registry
//! overlap" (`Clear`) — a distinction Python's original also draws
//! (`scripts/analyst_probe.py:1163-1189`).

use std::collections::HashMap;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan::{self, IdentityLinkRow};
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// Bounded sample size for the confirmed/auto-linked target-node refs row — see
/// `disqualified_director_edges.rs`'s identical constant. Load-bearing per T-49-01: this probe's
/// population is plausibly the whole `identity_links` table (13,627+ rows historically on the
/// live corpus), so an uncapped ref list would produce an oversized `/graph?ids=...` URL.
const DISPLAY_CAP: usize = 8;

/// The OSINT-layer health check — see module doc comment.
pub struct IdentityLinkRegistry;

#[async_trait::async_trait]
impl Probe for IdentityLinkRegistry {
    fn name(&self) -> &'static str {
        "identity-link-registry"
    }

    /// Searchable by the findings-by-entity index (PIV-02) as of Phase 49: confirmed/auto_linked
    /// rows name the target node id via `RecordKind::Entity` — never the kind that identifies the
    /// `identity_links` record itself, since that kind never matches an entity
    /// (`ref_names_entity`'s documented rule). The confidence-band counts and `{n} via {adapter}`
    /// rows remain ref-less (an adapter name is not a graph node, plan 44-21) but no longer gate
    /// the probe's overall scope now that at least one row can carry a pivotable ref.
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
        "Does the identity_links table hold any confirmed or pending resolutions at all — the \
         gate every later Tier-2 probe's Strategy reads?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        let rows = match scan::scan_identity_links(db, None, None).await {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result — Probe::run is
                // infallible by design (PRB-08). No Python analog — defaulted to Medium per
                // 35-01's analog-less-DataGap convention.
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("identity_links scan failed: {e}")),
                    summary: "identity_links scan failed — OSINT health check not possible."
                        .to_string(),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        if rows.is_empty() {
            // D6's memo — see module doc comment. Degrades safely to an empty map on its own
            // scan failure, which this probe correctly reads as "no marker" (DataGap), never as
            // a false "definitely ran" Clear.
            let ran = st.enrichment_runs(db).await;
            if !ran.is_empty() {
                let mut sources: Vec<&String> = ran.keys().collect();
                sources.sort();
                let names = sources
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let adapters_ran = plural(sources.len() as u64, "adapter ran", "adapters ran");
                let summary = format!(
                    "identity_links table is empty, but enrichment HAS run: run markers exist \
                     for {names}. The {adapters_ran} and found zero cross-registry overlap \
                     with the current graph — a data-coverage gap, not a missing run."
                );
                // population = capable = matches = 0 — an empty table IS the measured clean
                // result (VRD-02), not an unmeasured gap; `classify`'s `matches == 0 -> Clear`
                // branch is exactly what this needs, no non-obvious sentinel required.
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::classify(0, 0, 0, 1, ""),
                    summary,
                    cause: None,
                    // analyst_probe.py:1170 — this branch's CLEAR/INFO pair, 1:1.
                    severity: Severity::Info,
                    evidence: vec![EvidenceRow {
                        text: "Re-running enrich will not create links until the registries \
                               intersect entities already in the graph."
                            .to_string(),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
            let summary = "identity_links table is empty and no enrichment_runs marker exists \
                            — OSINT enrichment has not run yet."
                .to_string();
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(summary.clone()),
                summary,
                cause: None,
                // analyst_probe.py:1183 — this branch's DATA_GAP/MEDIUM pair, 1:1.
                severity: Severity::Medium,
                evidence: vec![EvidenceRow {
                    text: "Execute `urban-integrity enrich --source all` after wiring the \
                           Phase 17+ adapters (CH PSC, Disqualified Directors, Land Registry, \
                           Charity Commission)."
                        .to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let total = rows.len() as u64;
        // Materialized once, ahead of the per-row counting loop below, so the same rows are
        // available for RecordRef construction after status_counts/adapter_counts are derived —
        // status_counts's values are unaffected by this separate pass.
        let confirmed_rows: Vec<&IdentityLinkRow> = rows
            .iter()
            .filter(|r| r.status == "confirmed" || r.status == "auto_linked")
            .collect();
        let mut status_counts: HashMap<String, u64> = HashMap::new();
        let mut adapter_counts: HashMap<String, u64> = HashMap::new();
        for row in &rows {
            *status_counts.entry(row.status.clone()).or_insert(0) += 1;
            *adapter_counts.entry(row.source.clone()).or_insert(0) += 1;
        }
        let confirmed = status_counts.get("confirmed").copied().unwrap_or(0)
            + status_counts.get("auto_linked").copied().unwrap_or(0);
        let pending = status_counts.get("pending_review").copied().unwrap_or(0);
        let candidate = status_counts.get("candidate").copied().unwrap_or(0);
        let rejected = status_counts.get("rejected").copied().unwrap_or(0);

        st.osint_active = true;
        st.confirmed_link_count = confirmed;
        st.pending_review_count = pending;

        // Python's `Counter.most_common(5)` has its own stable insertion-order tie-break; Rust's
        // `HashMap` iteration order is non-deterministic, so sort explicitly by
        // (count desc, adapter name asc) for a deterministic top-5 instead — a documented
        // divergence, not an accidental one.
        let mut top_adapters: Vec<(&String, &u64)> = adapter_counts.iter().collect();
        top_adapters.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let top5: Vec<String> = top_adapters
            .iter()
            .take(5)
            .map(|(name, n)| format!("{n} {name}"))
            .collect();

        let avg_conf = if rows.is_empty() {
            0.0
        } else {
            rows.iter().map(|r| r.confidence).sum::<f64>() / rows.len() as f64
        };

        let summary = format!(
            "{}: {confirmed} confirmed/auto-linked, {pending} pending review, {}, {rejected} \
             rejected. Avg confidence {avg_conf:.3}. Top adapters: {}.",
            plural(total, "identity link", "identity links"),
            plural(candidate, "candidate", "candidates"),
            top5.join(", ")
        );

        // D-02: Python's status-distribution block (`analyst_probe.py:1200-1213`) as typed
        // evidence rows. No pivot refs — every row here is an aggregate over the whole table,
        // not a single record an analyst can open.
        let mut evidence = vec![
            EvidenceRow {
                text: format!(
                    "auto_linked / confirmed: {confirmed} (>=0.85 confidence — actionable \
                     intelligence)"
                ),
                refs: vec![],
            },
            EvidenceRow {
                text: format!(
                    "pending_review: {pending} (0.70-0.84 confidence — needs analyst sign-off)"
                ),
                refs: vec![],
            },
            EvidenceRow {
                text: format!("candidate: {candidate} (<0.70 confidence — low-priority)"),
                refs: vec![],
            },
            EvidenceRow {
                text: format!("rejected: {rejected}"),
                refs: vec![],
            },
            EvidenceRow {
                text: format!("Average confidence (all links): {avg_conf:.3}"),
                refs: vec![],
            },
        ];
        if !confirmed_rows.is_empty() {
            let mut ids: Vec<String> = confirmed_rows
                .iter()
                .map(|r| r.target_node_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            let capped: Vec<&String> = ids.iter().take(DISPLAY_CAP).collect();
            evidence.push(EvidenceRow {
                text: format!(
                    "Confirmed/auto-linked target nodes ({} unique, showing up to {DISPLAY_CAP}): {}",
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
        for (adapter, n) in top_adapters.iter().take(5) {
            evidence.push(EvidenceRow {
                text: format!("{n} via {adapter}"),
                refs: vec![],
            });
        }

        // `matches: confirmed` with `hit_threshold: 1` reproduces Python's `"HIT" if confirmed >
        // 0` exactly. When `confirmed == 0 && pending > 0`, `matches` switches to `pending` with
        // `hit_threshold: u64::MAX` (never reachable at that magnitude) so `classify` lands on
        // `Weak`, not the `matches == 0 -> Clear` branch it would otherwise take — the same
        // non-obvious "sentinel matches, capped hit_threshold" idiom `ward_concentration.rs`
        // documents for its own WEAK-only split.
        let verdict = if confirmed > 0 {
            Verdict::classify(total, total, confirmed, 1, "")
        } else if pending > 0 {
            Verdict::classify(total, total, pending, u64::MAX, "")
        } else {
            Verdict::classify(total, total, 0, 1, "")
        };

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            // analyst_probe.py:1221 — `severity = "HIGH" if confirmed > 0 else "MEDIUM"`, 1:1.
            severity: if confirmed > 0 {
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

    async fn seed_identity_link(db: &Surreal<Any>, idx: usize, status: &str, source: &str) {
        let sni = format!("entity:tier2-idreg-src-{idx}");
        let tni = format!("entity:tier2-idreg-dst-{idx}");
        db.query(
            "CREATE identity_links CONTENT { \
                 source_node_id: $sni, \
                 target_node_id: $tni, \
                 resolution_type: 'exact_company_number', \
                 confidence: 0.9, \
                 source: $source, \
                 resolved_at: d'2024-04-12T00:00:00Z', \
                 evidence_json: '{}', \
                 status: $status \
             }",
        )
        .bind(("sni", sni))
        .bind(("tni", tni))
        .bind(("source", source.to_string()))
        .bind(("status", status.to_string()))
        .await
        .expect("failed to seed identity_links row")
        .check()
        .expect("identity_links CREATE returned a statement error");
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

    #[tokio::test]
    async fn identity_link_registry_zero_rows_zero_enrichment_runs_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_link_registry_zero_rows_zero_enrichment_runs_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "idreg_empty",
        )
        .await;
        let mut st = Strategy::default();

        let probe = IdentityLinkRegistry;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero rows and zero enrichment_runs markers must be DataGap, got {:?}",
            finding.verdict
        );
        assert!(!st.osint_active, "osint_active must stay false on DataGap");
    }

    #[tokio::test]
    async fn identity_link_registry_zero_rows_enrichment_ran_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_link_registry_zero_rows_enrichment_ran_is_clear (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "idreg_ran_clear",
        )
        .await;
        let mut st = Strategy::default();

        // Enrichment ran (a marker exists) but produced zero identity_links rows.
        seed_enrichment_run(&db, "ch_psc").await;

        let probe = IdentityLinkRegistry;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "zero rows but a live enrichment_runs marker must be Clear, not DataGap — proves the \
             D6 memo distinguishes \"never ran\" from \"ran, no overlap\", got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("ch_psc"),
            "evidence must name the ran source, got: {}",
            finding.summary
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            0,
            "population must be 0 -- zero identity_links rows in this fixture"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "capable must equal population (0) when Clear is reached"
        );
    }

    #[tokio::test]
    async fn identity_link_registry_confirmed_present_is_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_link_registry_confirmed_present_is_hit (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "idreg_hit",
        )
        .await;
        let mut st = Strategy::default();

        seed_identity_link(&db, 0, "confirmed", "ch_psc").await;
        seed_identity_link(&db, 1, "auto_linked", "ch_psc").await;
        seed_identity_link(&db, 2, "pending_review", "land_registry_ccod").await;

        let probe = IdentityLinkRegistry;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "confirmed > 0 must be Hit, got {:?}",
            finding.verdict
        );
        assert!(st.osint_active, "osint_active must be set true");
        assert_eq!(
            st.confirmed_link_count, 2,
            "confirmed_link_count must equal confirmed+auto_linked rows"
        );
        assert_eq!(
            st.pending_review_count, 1,
            "pending_review_count must equal pending_review rows"
        );
    }

    #[tokio::test]
    async fn identity_link_registry_only_pending_is_weak() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: identity_link_registry_only_pending_is_weak (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "idreg_weak",
        )
        .await;
        let mut st = Strategy::default();

        seed_identity_link(&db, 0, "pending_review", "ch_psc").await;
        seed_identity_link(&db, 1, "pending_review", "ch_psc").await;

        let probe = IdentityLinkRegistry;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "confirmed == 0, pending > 0 must be Weak, got {:?}",
            finding.verdict
        );
        assert!(st.osint_active, "osint_active must be set true");
        assert_eq!(st.confirmed_link_count, 0);
        assert_eq!(st.pending_review_count, 2);
    }

    #[tokio::test]
    async fn identity_link_registry_confirmed_rows_attach_capped_entity_refs_excluding_pending() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_link_registry_confirmed_rows_attach_capped_entity_refs_excluding_pending (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "idreg_capped_refs",
        )
        .await;
        let mut st = Strategy::default();

        // 9 distinct confirmed/auto_linked rows -- one more than DISPLAY_CAP (8).
        for idx in 0..9 {
            seed_identity_link(&db, idx, "confirmed", "ch_psc").await;
        }
        // 3 distinct pending rows, using a disjoint idx range so their target_node_ids can never
        // collide with the confirmed rows' ids above.
        for idx in 100..103 {
            seed_identity_link(&db, idx, "pending_review", "land_registry_ccod").await;
        }

        let probe = IdentityLinkRegistry;
        let finding = probe.run(&db, &mut st).await;

        let entity_refs: Vec<&urban_integrity_schema::types::verdict::RecordRef> = finding
            .evidence
            .iter()
            .flat_map(|row| row.refs.iter())
            .filter(|r| r.kind == RecordKind::Entity)
            .collect();
        assert_eq!(
            entity_refs.len(),
            DISPLAY_CAP,
            "expected exactly DISPLAY_CAP (8) Entity refs total, got {}: {:?}",
            entity_refs.len(),
            entity_refs
        );

        let pending_ids: Vec<String> = (100..103)
            .map(|idx| format!("entity:tier2-idreg-dst-{idx}"))
            .collect();
        for r in &entity_refs {
            assert!(
                !pending_ids.contains(&r.id),
                "no ref may come from a pending row, found {}",
                r.id
            );
        }

        let confirmed_refs_row = finding
            .evidence
            .iter()
            .find(|row| row.text.contains("Confirmed/auto-linked target nodes"))
            .expect("a confirmed-refs evidence row must exist");
        assert!(
            confirmed_refs_row.text.contains("9 unique"),
            "evidence text must state the true total (9 unique), got: {}",
            confirmed_refs_row.text
        );
    }

    #[tokio::test]
    async fn identity_link_registry_pending_only_attaches_no_refs() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_link_registry_pending_only_attaches_no_refs (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "idreg_no_refs",
        )
        .await;
        let mut st = Strategy::default();

        seed_identity_link(&db, 0, "pending_review", "ch_psc").await;
        seed_identity_link(&db, 1, "candidate", "ch_psc").await;

        let probe = IdentityLinkRegistry;
        let finding = probe.run(&db, &mut st).await;

        for row in &finding.evidence {
            assert!(
                row.refs.is_empty(),
                "no evidence row may carry a ref when there are zero confirmed/auto_linked rows, \
                 found refs on row: {}",
                row.text
            );
        }
    }

    #[tokio::test]
    async fn identity_link_registry_only_candidate_rejected_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: identity_link_registry_only_candidate_rejected_is_clear (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "idreg_candrej",
        )
        .await;
        let mut st = Strategy::default();

        seed_identity_link(&db, 0, "candidate", "ch_psc").await;
        seed_identity_link(&db, 1, "rejected", "ch_psc").await;

        let probe = IdentityLinkRegistry;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "confirmed == 0, pending == 0 (only candidate/rejected) must be Clear, got {:?}",
            finding.verdict
        );
        assert!(
            st.osint_active,
            "osint_active must be set true even on Clear"
        );
        assert_eq!(st.confirmed_link_count, 0);
        assert_eq!(st.pending_review_count, 0);
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // NOT a zero-population Clear, unlike this file's other Clear test. The probe's final
        // branch is `classify(total, total, 0, 1, "")` where `total` is ALL links (2 here) --
        // the two seeded rows WERE examined, and neither reached confirmed/pending. That is a
        // meaningful, corroborated-by-construction Clear: 2 records checked, 0 matches. Pinning
        // 2 rather than 0 is the point of VRD-12 -- the plan predicted 0 by reading "confirmed ==
        // 0, pending == 0" as the population, and the assertion caught the difference between
        // "nothing was found" and "nothing was there to look at".
        assert_eq!(
            coverage.population(),
            2,
            "population is the total link count (2 seeded), not the confirmed/pending count"
        );
        assert_eq!(
            coverage.capable(),
            2,
            "capable must equal population -- both seeded links were readable"
        );
    }
}
