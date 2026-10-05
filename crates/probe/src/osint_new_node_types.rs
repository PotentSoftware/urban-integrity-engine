//! `OsintNewNodeTypes` -- the 5-term OSINT node-type coverage sweep (Plan 33-07).
//!
//! Ports `probe_osint_new_node_types` (`scripts/analyst_probe.py:1396-1469`). All 5 counts are
//! now `Attributed`-routed through D1's registry rather than a raw `payload.weight_json CONTAINS`
//! scan issued directly from this probe (VRD-08's structural routing-ban). The stale
//! `_CLI_DROPS_MUTATION_NOTE` remediation string (fixed Phase 21, 2026-06-14 -- CLI `enrich` now
//! carries the graph-mutation channel through) is deliberately NOT ported; the research notes'
//! Anti-Pattern section documents why reproducing it here would mislead a reader of a probe run
//! against a current build.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::{queries, scan};
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, SourceAttribution, Verdict,
};

use super::attributed;
use super::{EvidenceScope, MotifKind, Probe, Strategy};

/// Cap on how many node ids are individually named (and ref'd) per term's evidence row -- a
/// DISPLAY cap only, never applied to the reported count. Same shape as
/// `disqualified_director_edges.rs`'s `DISPLAY_CAP` (T-49-01).
const DISPLAY_CAP: usize = 8;

/// Sorted, deduped `payload.entity_id` strings from a set of `NodeAdded`/`NodeUpdated` rows --
/// this probe's rows all carry the id at that top-level payload field (never inside
/// `weight_json`), matching `seed_node`'s own test-seed shape below.
fn entity_ids_from_rows(rows: &[serde_json::Value]) -> Vec<String> {
    let mut ids: Vec<String> = rows
        .iter()
        .filter_map(|r| queries::extract_id_string(&r["payload"]["entity_id"]))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// One per-term evidence row. When `with_refs` is true and `count > 0`, the row names up to
/// `DISPLAY_CAP` of `ids` and carries a matching capped `RecordRef{kind: Entity}` per id (same
/// capped-slice-feeds-both-text-and-refs shape as `disqualified_director_edges.rs:313-341`).
/// `Property` passes `with_refs: false` and an empty `ids` slice -- it stays a pure aggregate
/// count in this probe; its per-node refs are `property_nodes.rs`'s job (Phase 49 plan 49-07).
fn term_row(
    prefix: &str,
    term_label: &str,
    count: u64,
    ids: &[String],
    with_refs: bool,
) -> EvidenceRow {
    if with_refs && count > 0 {
        let capped: Vec<&String> = ids.iter().take(DISPLAY_CAP).collect();
        EvidenceRow {
            text: format!(
                "{prefix} {term_label}={count} ({} unique, showing up to {DISPLAY_CAP}): {}.",
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
        }
    } else {
        EvidenceRow {
            text: format!("{prefix} {term_label}={count}."),
            refs: vec![],
        }
    }
}

/// The 10 adapter names Python's own `_OSINT_SOURCES` tuple lists
/// (`scripts/analyst_probe.py:497-501`), mirrored literally -- not invented, not the "15" this
/// plan's own behaviour prose miscounts elsewhere.
const OSINT_SOURCES: [&str; 10] = [
    "ch_psc",
    "ch_disqualified",
    "icij",
    "charity_commission",
    "land_registry_ccod",
    "electoral_commission",
    "opencorporates",
    "opensanctions",
    "london_gazette",
    "council_registers",
];

pub struct OsintNewNodeTypes;

#[async_trait::async_trait]
impl Probe for OsintNewNodeTypes {
    fn name(&self) -> &'static str {
        "osint-new-node-types"
    }

    /// Searchable by the findings-by-entity index (PIV-02) as of Phase 49: `Psc`/`Charity`/
    /// `OffshoreEntity`/`InLiquidation` each attach `RecordKind::Entity` refs; `Property` stays
    /// an aggregate count here -- its refs are `property-nodes.rs`'s job (Phase 49 plan 49-07).
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
        "Do any of the newer OSINT-sourced node types (Psc, Charity, Property, OffshoreEntity, \
         InLiquidation) appear in the graph?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        macro_rules! try_count {
            ($fut:expr, $severity:expr) => {
                match $fut.await {
                    Ok(a) => a,
                    Err(e) => {
                        return Finding {
                            probe: self.name().to_string(),
                            verdict: Verdict::data_gap(format!("OSINT node scan failed: {e}")),
                            summary: format!("OSINT node scan failed: {e}"),
                            cause: None,
                            // No Python analog for a per-term scan failure (Python's version
                            // issues one raw query per term with no error branch) -- defaulted
                            // to Severity::Medium per 35-01's convention for a DataGap branch.
                            severity: $severity,
                            evidence: vec![],
                            attributions: vec![],
                            remediation_hint: None,
                        };
                    }
                }
            };
        }

        // Fetch-then-attribute: the raw rows are needed for `RecordRef` construction below, so
        // each of these 4 terms fetches once via `scan::` and derives its `Attributed` from those
        // SAME rows via `attributed::from_rows` -- never the row-discarding
        // `attributed::count_for_source` (mirrors `disqualified_director_edges.rs:107-138`).
        // Both new macros share `try_count!`'s exact error-Finding shape ("OSINT node scan
        // failed: {e}", `evidence: vec![]`, `attributions: vec![]`) so a failure at either step
        // is indistinguishable in behaviour from the old single-call failure.
        macro_rules! try_rows {
            ($fut:expr, $severity:expr) => {
                match $fut.await {
                    Ok(rows) => rows,
                    Err(e) => {
                        return Finding {
                            probe: self.name().to_string(),
                            verdict: Verdict::data_gap(format!("OSINT node scan failed: {e}")),
                            summary: format!("OSINT node scan failed: {e}"),
                            cause: None,
                            severity: $severity,
                            evidence: vec![],
                            attributions: vec![],
                            remediation_hint: None,
                        };
                    }
                }
            };
        }
        macro_rules! try_attr {
            ($res:expr, $severity:expr) => {
                match $res {
                    Ok(a) => a,
                    Err(e) => {
                        return Finding {
                            probe: self.name().to_string(),
                            verdict: Verdict::data_gap(format!("OSINT node scan failed: {e}")),
                            summary: format!("OSINT node scan failed: {e}"),
                            cause: None,
                            severity: $severity,
                            evidence: vec![],
                            attributions: vec![],
                            remediation_hint: None,
                        };
                    }
                }
            };
        }

        let psc_rows = try_rows!(
            scan::scan_events_by_payload_contains(
                db,
                "NodeAdded",
                "weight_json",
                "\"type\":\"Psc\""
            ),
            Severity::Medium
        );
        let psc = try_attr!(
            attributed::from_rows("Psc", "ch_psc", &psc_rows),
            Severity::Medium
        );

        let charity_rows = try_rows!(
            scan::scan_events_by_payload_contains(
                db,
                "NodeAdded",
                "weight_json",
                "\"type\":\"Charity\""
            ),
            Severity::Medium
        );
        let charity = try_attr!(
            attributed::from_rows("Charity", "charity_commission", &charity_rows),
            Severity::Medium
        );

        // Raw total across both Property producers -- no per-producer split here, that belongs
        // to `property_nodes.rs` (D3, Plan 33-07 Task 1). Property stays aggregate-only in THIS
        // probe (49-09's objective): no fetch, no refs.
        let property = try_count!(
            attributed::count_across_producers(db, "NodeAdded", "Property", None),
            Severity::Medium
        );

        let offshore_rows = try_rows!(
            scan::scan_events_by_payload_contains(
                db,
                "NodeAdded",
                "weight_json",
                "\"type\":\"OffshoreEntity\""
            ),
            Severity::Medium
        );
        let offshore_entity = try_attr!(
            attributed::from_rows("OffshoreEntity", "land_registry_ocod", &offshore_rows),
            Severity::Medium
        );

        let in_liquidation_rows = try_rows!(
            scan::scan_events_by_payload_contains(
                db,
                "NodeUpdated",
                "weight_json",
                "\"type\":\"InLiquidation\""
            ),
            Severity::Medium
        );
        let in_liquidation = try_attr!(
            attributed::from_rows("InLiquidation", "london_gazette", &in_liquidation_rows),
            Severity::Medium
        );

        // Every branch below runs only after all 5 `attributed::` calls above succeeded, so the
        // merged producer list is safe to build once here and reuse (moved, never re-derived) at
        // every remaining Finding site.
        let attributions: Vec<SourceAttribution> = psc
            .producers
            .iter()
            .chain(charity.producers.iter())
            .chain(property.producers.iter())
            .chain(offshore_entity.producers.iter())
            .chain(in_liquidation.producers.iter())
            .map(|(source, value)| SourceAttribution {
                source,
                value: *value,
            })
            .collect();

        let psc = psc.value;
        let charity = charity.value;
        let property = property.value;
        let offshore_entity = offshore_entity.value;
        let in_liquidation = in_liquidation.value;

        let total = psc + charity + property + offshore_entity + in_liquidation;

        // D-05: report OSINT *nodes* (4 terms) and OSINT *status updates* (1 term,
        // InLiquidation) as two explicitly labelled units instead of one merged sentence.
        // `total` above is untouched -- this is a display-only split. Each of the 4 node terms
        // now gets its OWN row (was one merged `nodes_text` sentence) so refs can be hung on a
        // row whose text names exactly one term -- `Property`'s row carries no refs (`with_refs:
        // false`), the other three do.
        let psc_ids = entity_ids_from_rows(&psc_rows);
        let charity_ids = entity_ids_from_rows(&charity_rows);
        let offshore_ids = entity_ids_from_rows(&offshore_rows);
        let in_liquidation_ids = entity_ids_from_rows(&in_liquidation_rows);

        let total_text = format!("Total: {total} (5 terms, nodes + status updates).");
        let evidence_rows = vec![
            term_row("OSINT nodes added:", "Psc", psc, &psc_ids, true),
            term_row("OSINT nodes added:", "Charity", charity, &charity_ids, true),
            term_row("OSINT nodes added:", "Property", property, &[], false),
            term_row(
                "OSINT nodes added:",
                "OffshoreEntity",
                offshore_entity,
                &offshore_ids,
                true,
            ),
            term_row(
                "OSINT status updates:",
                "InLiquidation",
                in_liquidation,
                &in_liquidation_ids,
                true,
            ),
            EvidenceRow {
                text: total_text,
                refs: vec![],
            },
        ];

        // `Clear` is structurally unreachable for this probe. This `total == 0` guard
        // returns `DataGap` before ANY of the three `classify` call sites below, so every
        // reachable one has `total > 0` and therefore `matches > 0` -- `classify` reaches
        // `Clear` only at `matches == 0`. Proven by `other_nonzero_no_offshore_no_psc_is_weak`.
        if total == 0 {
            let runs = st.enrichment_runs(db).await;
            let any_osint_ran = OSINT_SOURCES.iter().any(|s| runs.contains_key(*s));
            if any_osint_ran {
                let summary = "Zero OSINT-sourced nodes/updates in the graph, but at least one \
                                OSINT adapter has run (enrichment_runs marker present)."
                    .to_string();
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(summary.clone()),
                    summary,
                    cause: None,
                    severity: Severity::Medium,
                    evidence: evidence_rows.clone(),
                    attributions,
                    remediation_hint: None,
                };
            }
            let summary = "Zero OSINT-sourced nodes/updates in the graph. Phase 17-19 adapters \
                            have not run."
                .to_string();
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(summary.clone()),
                summary,
                cause: None,
                severity: Severity::Medium,
                evidence: evidence_rows.clone(),
                attributions,
                remediation_hint: None,
            };
        }

        // Priority-ordered exactly as Python's `elif` chain: OffshoreEntity checked first.
        if offshore_entity > 0 {
            let summary = format!(
                "{total} OSINT nodes/updates. OFFSHORE ENTITIES PRESENT ({offshore_entity}) -- \
                 requires immediate cross-reference with contract awardees."
            );
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::classify(total, total, total, 1, ""),
                summary,
                cause: None,
                // Python: `severity = "CRITICAL"` for the OffshoreEntity>0 branch
                // (analyst_probe.py:1450).
                severity: Severity::Critical,
                evidence: evidence_rows.clone(),
                attributions,
                remediation_hint: None,
            };
        }
        if psc > 0 {
            let summary = format!(
                "{total} OSINT nodes/updates including {psc} PSC records -- beneficial ownership \
                 graph active."
            );
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::classify(total, total, total, 1, ""),
                summary,
                cause: None,
                // Python: `severity = "HIGH"` for the Psc>0 branch (analyst_probe.py:1455).
                severity: Severity::High,
                evidence: evidence_rows.clone(),
                attributions,
                remediation_hint: None,
            };
        }
        let summary = format!("{total} OSINT nodes/updates present.");
        Finding {
            probe: self.name().to_string(),
            // hit_threshold set above `total` so any nonzero, non-Offshore, non-Psc mix always
            // lands as Weak, matching Python's else-branch WEAK/MEDIUM.
            verdict: Verdict::classify(total, total, total, total + 1, ""),
            summary,
            cause: None,
            // Python: `severity = "MEDIUM"` for the else branch (analyst_probe.py:1459).
            severity: Severity::Medium,
            evidence: evidence_rows.clone(),
            attributions,
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::node::{
        CharityData, EntityData, EntityStatus, NodeWeight, OffshoreEntityData, PropertyData,
        PscData,
    };

    async fn seed_node(db: &Surreal<Any>, kind: &str, entity_id: &str, weight: &NodeWeight) {
        let weight_json = serde_json::to_string(weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("{kind}-{entity_id}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": kind,
                    "payload": { "entity_id": entity_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed node event")
            .check()
            .expect("seed node event returned a statement error");
    }

    async fn seed_enrichment_run(db: &Surreal<Any>, source: &str) {
        db.query(format!(
            "UPSERT enrichment_runs:{source} SET source = '{source}', last_run_at = $last_run_at, \
             candidates_found = $candidates_found, auto_linked = $auto_linked, \
             pending_review = $pending_review, discarded = $discarded"
        ))
        .bind(("last_run_at", 1_756_000_000i64))
        .bind(("candidates_found", 0i64))
        .bind(("auto_linked", 0i64))
        .bind(("pending_review", 0i64))
        .bind(("discarded", 0i64))
        .await
        .expect("seed enrichment_runs row")
        .check()
        .expect("seed enrichment_runs row check");
    }

    fn psc_weight(name: &str) -> NodeWeight {
        NodeWeight::Psc(PscData {
            name: name.to_string(),
            company_number: None,
            natures_of_control: None,
        })
    }

    fn charity_weight(name: &str) -> NodeWeight {
        NodeWeight::Charity(CharityData {
            name: name.to_string(),
            charity_number: None,
            registered_name: None,
        })
    }

    fn property_weight() -> NodeWeight {
        NodeWeight::Property(PropertyData {
            title_number: Some("TN1".to_string()),
            tenure: None,
            price_paid: None,
            company_number: None,
            country_incorporated: None,
        })
    }

    fn offshore_weight(name: &str) -> NodeWeight {
        NodeWeight::OffshoreEntity(OffshoreEntityData {
            name: name.to_string(),
            jurisdiction: None,
            icij_node_id: None,
        })
    }

    fn entity_in_liquidation(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::InLiquidation(chrono::Utc::now()),
            incorporation_date: None,
        })
    }

    #[tokio::test]
    async fn zero_counts_no_marker_is_data_gap_plain() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: zero_counts_no_marker_is_data_gap_plain (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_osint_new_node_types",
            "no_marker",
        )
        .await;
        let mut st = Strategy::default();
        let probe = OsintNewNodeTypes;

        let finding = probe.run(&db, &mut st).await;
        assert!(matches!(finding.verdict, Verdict::DataGap { .. }));
        assert!(finding
            .summary
            .contains("Phase 17-19 adapters have not run"));
    }

    #[tokio::test]
    async fn zero_counts_with_marker_is_data_gap_without_stale_note() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: zero_counts_with_marker_is_data_gap_without_stale_note (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_osint_new_node_types",
            "marker_zero",
        )
        .await;
        seed_enrichment_run(&db, "ch_psc").await;
        let mut st = Strategy::default();
        let probe = OsintNewNodeTypes;

        let finding = probe.run(&db, &mut st).await;
        assert!(matches!(finding.verdict, Verdict::DataGap { .. }));
        assert!(
            !finding.summary.contains("CLI"),
            "must not port the stale _CLI_DROPS_MUTATION_NOTE, got: {}",
            finding.summary
        );
        assert!(
            !finding.summary.contains("drops the graph-mutation channel"),
            "must not port the stale _CLI_DROPS_MUTATION_NOTE, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn offshore_entity_nonzero_is_hit_even_with_psc_nonzero() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: offshore_entity_nonzero_is_hit_even_with_psc_nonzero (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_osint_new_node_types",
            "offshore_priority",
        )
        .await;
        seed_node(
            &db,
            "NodeAdded",
            "offshore-1",
            &offshore_weight("Offshore Co"),
        )
        .await;
        seed_node(&db, "NodeAdded", "psc-1", &psc_weight("Jane Smith")).await;
        let mut st = Strategy::default();
        let probe = OsintNewNodeTypes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "expected Hit, got {:?}",
            finding.verdict
        );
        assert!(finding.summary.contains("OFFSHORE ENTITIES PRESENT"));
    }

    #[tokio::test]
    async fn psc_nonzero_no_offshore_is_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: psc_nonzero_no_offshore_is_hit (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_osint_new_node_types",
            "psc_only",
        )
        .await;
        seed_node(&db, "NodeAdded", "psc-1", &psc_weight("Jane Smith")).await;
        let mut st = Strategy::default();
        let probe = OsintNewNodeTypes;

        let finding = probe.run(&db, &mut st).await;
        assert!(matches!(finding.verdict, Verdict::Hit { .. }));
        assert!(finding
            .summary
            .contains("beneficial ownership graph active"));
    }

    #[tokio::test]
    async fn other_nonzero_no_offshore_no_psc_is_weak() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: other_nonzero_no_offshore_no_psc_is_weak (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_osint_new_node_types",
            "charity_only",
        )
        .await;
        seed_node(
            &db,
            "NodeAdded",
            "charity-1",
            &charity_weight("Test Charity"),
        )
        .await;
        let mut st = Strategy::default();
        let probe = OsintNewNodeTypes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Weak { .. }),
            "expected Weak, got {:?}",
            finding.verdict
        );
        assert!(
            !matches!(finding.verdict, Verdict::Clear(_)),
            "osint-new-node-types must never reach Clear -- the total==0 guard returns DataGap before any classify call, so every reachable classify call has total>0, making matches>0 always"
        );
    }

    #[tokio::test]
    async fn property_and_in_liquidation_count_toward_total() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: property_and_in_liquidation_count_toward_total (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_osint_new_node_types",
            "property_inliq",
        )
        .await;
        seed_node(&db, "NodeAdded", "property-1", &property_weight()).await;
        seed_node(
            &db,
            "NodeUpdated",
            "entity-inliq-1",
            &entity_in_liquidation("Ltd In Liquidation"),
        )
        .await;
        let mut st = Strategy::default();
        let probe = OsintNewNodeTypes;

        let finding = probe.run(&db, &mut st).await;
        assert!(matches!(finding.verdict, Verdict::Weak { .. }));
        assert!(finding
            .evidence
            .iter()
            .any(|e| e.text.contains("Property=1")));
        assert!(finding
            .evidence
            .iter()
            .any(|e| e.text.contains("InLiquidation=1")));
        assert!(finding
            .evidence
            .iter()
            .any(|e| e.text.starts_with("OSINT nodes added:") && e.text.contains("Property=1")));
        assert!(finding
            .evidence
            .iter()
            .any(|e| e.text.starts_with("OSINT status updates:")
                && e.text.contains("InLiquidation=1")));
        assert!(finding
            .evidence
            .iter()
            .any(|e| e.text.starts_with("Total:") && e.text.contains("(5 terms")));
        // 6 rows now: Psc, Charity, Property, OffshoreEntity, InLiquidation, Total -- was 3
        // (merged nodes_text + updates_text + total_text) before this plan split each of the 4
        // node terms into its own row.
        assert_eq!(finding.evidence.len(), 6);
    }

    #[tokio::test]
    async fn osint_new_node_types_offshore_priority_branch_caps_refs_at_display_cap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: osint_new_node_types_offshore_priority_branch_caps_refs_at_display_cap \
                 (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_osint_new_node_types",
            "offshore_display_cap",
        )
        .await;
        for i in 0..9 {
            seed_node(
                &db,
                "NodeAdded",
                &format!("offshore-{i}"),
                &offshore_weight(&format!("Offshore Co {i}")),
            )
            .await;
        }
        let mut st = Strategy::default();
        let probe = OsintNewNodeTypes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "expected Hit, got {:?}",
            finding.verdict
        );
        let offshore_row = finding
            .evidence
            .iter()
            .find(|e| e.text.contains("OffshoreEntity=9"))
            .expect("an OffshoreEntity=9 evidence row must exist");
        assert_eq!(
            offshore_row.refs.len(),
            8,
            "expected refs capped at DISPLAY_CAP=8 from 9 distinct nodes, got {}",
            offshore_row.refs.len()
        );
        assert!(offshore_row
            .refs
            .iter()
            .all(|r| r.kind == RecordKind::Entity));
    }

    #[tokio::test]
    async fn osint_new_node_types_all_four_convertible_terms_attach_refs_property_stays_aggregate()
    {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: osint_new_node_types_all_four_convertible_terms_attach_refs_property_stays_aggregate \
                 (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_osint_new_node_types",
            "four_terms_refed",
        )
        .await;
        seed_node(&db, "NodeAdded", "psc-1", &psc_weight("Jane Smith")).await;
        seed_node(
            &db,
            "NodeAdded",
            "charity-1",
            &charity_weight("Test Charity"),
        )
        .await;
        seed_node(
            &db,
            "NodeAdded",
            "offshore-1",
            &offshore_weight("Offshore Co"),
        )
        .await;
        seed_node(
            &db,
            "NodeUpdated",
            "entity-inliq-1",
            &entity_in_liquidation("Ltd In Liquidation"),
        )
        .await;
        let mut st = Strategy::default();
        let probe = OsintNewNodeTypes;

        let finding = probe.run(&db, &mut st).await;

        let psc_row = finding
            .evidence
            .iter()
            .find(|e| e.text.contains("Psc=1"))
            .expect("a Psc=1 evidence row must exist");
        assert_eq!(psc_row.refs.len(), 1);
        assert_eq!(psc_row.refs[0].kind, RecordKind::Entity);

        let charity_row = finding
            .evidence
            .iter()
            .find(|e| e.text.contains("Charity=1"))
            .expect("a Charity=1 evidence row must exist");
        assert_eq!(charity_row.refs.len(), 1);
        assert_eq!(charity_row.refs[0].kind, RecordKind::Entity);

        let offshore_row = finding
            .evidence
            .iter()
            .find(|e| e.text.contains("OffshoreEntity=1"))
            .expect("an OffshoreEntity=1 evidence row must exist");
        assert_eq!(offshore_row.refs.len(), 1);
        assert_eq!(offshore_row.refs[0].kind, RecordKind::Entity);

        let in_liquidation_row = finding
            .evidence
            .iter()
            .find(|e| {
                e.text.starts_with("OSINT status updates:") && e.text.contains("InLiquidation=1")
            })
            .expect("an InLiquidation=1 evidence row must exist");
        assert_eq!(in_liquidation_row.refs.len(), 1);
        assert_eq!(in_liquidation_row.refs[0].kind, RecordKind::Entity);

        let property_row = finding
            .evidence
            .iter()
            .find(|e| e.text.starts_with("OSINT nodes added:") && e.text.contains("Property=0"))
            .expect("a Property=0 evidence row must exist");
        assert!(
            property_row.refs.is_empty(),
            "Property must stay aggregate-only in this probe -- got {} refs",
            property_row.refs.len()
        );
    }
}
