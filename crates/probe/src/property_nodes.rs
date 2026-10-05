//! `PropertyNodes` -- D3's any-registered-producer-counts-as-capability fix (Plan 33-07).
//!
//! Ports `probe_property_nodes` (`scripts/analyst_probe.py:1356-1393`) for its EVIDENCE SHAPE
//! only, NOT its verdict logic. Python always returns `DATA_GAP` when the `Property` node count
//! is 0, regardless of whether the CCOD adapter ever ran -- and always attributes every non-zero
//! count solely to "Land Registry CCOD data", even though `land_registry_ocod` (97,500 live
//! records) is a second, independent producer of the same `Property` node type. Both are exactly
//! the defect D3/VRD-04 exist to close: a false `DataGap` over a populated graph (checking only
//! one adapter's `enrichment_runs` marker), and a false single-producer attribution (crediting
//! every row to CCOD even when some or all came from OCOD). This port deliberately diverges on
//! both counts -- see this plan's `<interfaces>` section.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::attribution::{producer_of_property, REGISTRY};
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, SourceAttribution, Verdict,
};

use super::attributed::{self, Attributed};
use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// Cap on how many Property node ids are individually named in the node-listing evidence row --
/// a DISPLAY cap only, never applied to the reported count (T-49-01; matches
/// `disqualified_director_edges.rs`'s `DISPLAY_CAP` exactly).
const DISPLAY_CAP: usize = 8;

/// Converts an [`Attributed`] value's already-split per-producer breakdown into
/// `Finding.attributions` -- copied directly, never re-derived, matching plan 35-04's
/// `council_registers_political_actors.rs`/`electoral_commission_donations.rs` wiring exactly.
fn attributions_of(attributed: &Attributed) -> Vec<SourceAttribution> {
    attributed
        .producers
        .iter()
        .map(|(source, value)| SourceAttribution {
            source,
            value: *value,
        })
        .collect()
}

pub struct PropertyNodes;

#[async_trait::async_trait]
impl Probe for PropertyNodes {
    fn name(&self) -> &'static str {
        "property-nodes"
    }

    /// Searchable by the findings-by-entity index (PIV-02) as of Phase 49: non-zero Property
    /// counts attach `RecordKind::Entity` refs to the underlying node ids, alongside the
    /// pre-existing producer-name split (which stays a count, never a ref).
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
        "Does the graph hold Property nodes from either CCOD or OCOD, checked against a \
         per-producer enrichment_runs marker rather than CCOD alone?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        // Fetch the raw rows once, purely to recover ids for RecordRef construction below.
        // `attributed::count_across_producers` cannot both split-by-producer AND hand back rows
        // in one call without a new `attributed.rs` variant (Open Question 2's recommendation
        // against adding one) -- this is the one-extra-query exception the research notes sanction for
        // this probe. The scan-failure branch reuses the exact text/severity shape of the
        // existing `count_across_producers` scan-failure branch immediately below.
        //
        // Uses `scan_events_by_payload_contains` (field `weight_json`), not the sibling
        // whole-marker helper this module used to call here -- the two return an identical row
        // set and shape (both delegate to the same `id, event_id, kind, payload, timestamp`
        // projection, just filtering `kind`/marker in a different order), but
        // `probe_tier2_e2e.rs`'s Tier-2 routing-ban guard (VRD-08/GRD-03) bans that sibling
        // helper's name as a substring in every Tier-2 probe file outside its own allowlist, and
        // this file was never added to it. Found by 49-11's full `URBAN_DB_TESTS=1 cargo test
        // --workspace` gate -- 49-07 (and this file's own research-notes-sourced code example) only
        // ever ran scoped `-p urban-integrity-probe` tests, never the CLI crate's guard suite, so
        // the violation shipped uncaught. `disqualified_director_edges.rs` already proves this
        // exact substitution for the identical "raw row fetch for RecordRef construction"
        // purpose, so this is a drop-in fix, not a behavior change (Rule 1).
        let rows = match scan::scan_events_by_payload_contains(
            db,
            "NodeAdded",
            "weight_json",
            "\"type\":\"Property\"",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("Property scan failed: {e}")),
                    summary: format!("Property scan failed: {e}"),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: format!("Property node scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        let attributed = match attributed::count_across_producers(
            db,
            "NodeAdded",
            "Property",
            Some(producer_of_property),
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("Property scan failed: {e}")),
                    summary: format!("Property scan failed: {e}"),
                    cause: None,
                    // No Python analog (this scan-failure branch is Rust-specific
                    // infrastructure handling, PRB-08) — defaulted to Severity::Medium per
                    // 35-01's convention for an analog-less DataGap branch.
                    severity: Severity::Medium,
                    evidence: vec![EvidenceRow {
                        text: format!("Property node scan error: {e}"),
                        refs: vec![],
                    }],
                    // The call that failed is the sole source of `attributions` in this file —
                    // nothing was measured, so `attributions: vec![]` here.
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // D1's registry is the single source of truth for which producers can emit `Property` --
        // never a hardcoded pair of adapter names, so a third future emitter is automatically
        // covered by this lookup (D3's whole point).
        let property_producers: &'static [&'static str] = REGISTRY
            .iter()
            .find(|e| e.term == "Property")
            .map(|e| e.producers)
            .unwrap_or(&[]);

        if attributed.value == 0 {
            let runs = st.enrichment_runs(db).await;
            let ran: Vec<&'static str> = property_producers
                .iter()
                .copied()
                .filter(|p| runs.contains_key(*p))
                .collect();

            if ran.is_empty() {
                // D3: never report "the CCOD adapter is disabled" alone -- name every registered
                // producer that has no marker, so a check hardcoded to one adapter name cannot
                // silently pass this branch.
                let names = property_producers.join(", ");
                let summary = format!(
                    "Zero Property nodes in graph, and no enrichment_runs marker for any \
                     registered Property producer ({names}) -- neither has run."
                );
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(summary.clone()),
                    summary,
                    cause: None,
                    // analyst_probe.py:1372 — the zero-count branch's DATA_GAP/LOW pair, 1:1.
                    severity: Severity::Low,
                    evidence: property_producers
                        .iter()
                        .map(|p| EvidenceRow {
                            text: format!("{p}: registered producer, no enrichment_runs marker"),
                            refs: vec![],
                        })
                        .chain(std::iter::once(EvidenceRow {
                            text: "Dataset: HM Land Registry 'Use Land and Property Data' \
                                   (CCOD/OCOD bulk CSV) — \
                                   https://use-land-property-data.service.gov.uk/datasets/ccod"
                                .to_string(),
                            refs: vec![],
                        }))
                        .collect(),
                    // Nothing was counted, so there is no per-producer split to attribute.
                    attributions: vec![],
                    remediation_hint: None,
                };
            }

            // At least one registered producer has run and genuinely measured zero Property
            // rows -- a real, measured empty result, not an unmeasured gap.
            let ran_names = ran.join(", ");
            let summary = format!(
                "Zero Property nodes in graph. {} that HAVE run: {ran_names}. This is a \
                 measured empty result, not a data gap.",
                plural(
                    ran.len() as u64,
                    "Registered producer",
                    "Registered producers"
                )
            );
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::classify(0, 0, 0, 1, ""),
                summary,
                cause: None,
                // No Python analog — Python returns DATA_GAP for every zero count, which is
                // precisely the defect D3 diverges from. Defaulted to Severity::Info per
                // 35-01's convention for an analog-less Clear branch.
                severity: Severity::Info,
                evidence: property_producers
                    .iter()
                    .map(|p| EvidenceRow {
                        text: format!(
                            "{p}: {}",
                            if ran.contains(p) {
                                "ran, produced zero Property nodes"
                            } else {
                                "no enrichment_runs marker"
                            }
                        ),
                        refs: vec![],
                    })
                    .collect(),
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // Non-zero: Hit, matching Python's unconditional HIT-on-nonzero. Evidence states the
        // total AND the per-producer split -- always naming BOTH known producers, even when one
        // is silently at zero (D3: omitting a zero-count producer from the evidence line is
        // exactly the kind of silent gap this probe exists to prevent from recurring in reverse).
        let value = attributed.value;
        let split_text = property_producers
            .iter()
            .map(|name| {
                let count = attributed
                    .producers
                    .iter()
                    .find(|(p, _)| p == name)
                    .map(|(_, c)| *c)
                    .unwrap_or(0);
                format!("{name}={count}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let summary = format!(
            "{} in graph from Land Registry data. Per-producer split: {split_text}.",
            plural(value, "Property node", "Property nodes")
        );
        // Node-listing evidence row: capped RecordRefs built from the SAME rows fetched at the
        // top of this function, never a second scan. The pre-existing split_text row above is
        // left untouched and keeps `refs: vec![]` -- a producer name is not a graph node
        // (plan 44-21), only the underlying entity ids are.
        let mut ids: Vec<String> = rows
            .iter()
            .filter_map(|r| r["payload"]["entity_id"].as_str().map(str::to_string))
            .collect();
        ids.sort();
        ids.dedup();
        let capped: Vec<&String> = ids.iter().take(DISPLAY_CAP).collect();
        let mut evidence = vec![EvidenceRow {
            text: format!(
                "{value} Property nodes (corporate title ownership). Per-producer split: \
                 {split_text}."
            ),
            refs: vec![],
        }];
        evidence.push(EvidenceRow {
            text: format!(
                "Property nodes ({} unique, showing up to {DISPLAY_CAP}): {}",
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

        Finding {
            probe: self.name().to_string(),
            verdict: Verdict::classify(value, value, value, 1, ""),
            summary,
            cause: None,
            // analyst_probe.py:1389 — the non-zero branch's HIT/HIGH pair, 1:1.
            severity: Severity::High,
            evidence,
            // Copied straight from the already-split per-producer breakdown — never re-derived.
            attributions: attributions_of(&attributed),
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::node::{NodeWeight, PropertyData};

    async fn seed_property(db: &Surreal<Any>, entity_id: &str, country_incorporated: Option<&str>) {
        let weight = NodeWeight::Property(PropertyData {
            title_number: Some(format!("TN-{entity_id}")),
            tenure: Some("Freehold".to_string()),
            price_paid: Some(100_000.0),
            company_number: None,
            country_incorporated: country_incorporated.map(|s| s.to_string()),
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("NodeAdded-{entity_id}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeAdded",
                    "payload": { "entity_id": entity_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed property event")
            .check()
            .expect("seed property event returned a statement error");
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

    #[tokio::test]
    async fn property_nodes_zero_rows_no_marker_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: property_nodes_zero_rows_no_marker_is_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_property_nodes",
            "no_marker",
        )
        .await;
        let mut st = Strategy::default();
        let probe = PropertyNodes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap, got {:?}",
            finding.verdict
        );
        assert!(finding.summary.contains("land_registry_ccod"));
        assert!(finding.summary.contains("land_registry_ocod"));
    }

    #[tokio::test]
    async fn property_nodes_zero_rows_ccod_marker_only_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: property_nodes_zero_rows_ccod_marker_only_is_clear (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_property_nodes",
            "ccod_only",
        )
        .await;
        seed_enrichment_run(&db, "land_registry_ccod").await;
        let mut st = Strategy::default();
        let probe = PropertyNodes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "expected Clear when ONLY the CCOD marker is present, got {:?}",
            finding.verdict
        );
        assert!(finding.summary.contains("land_registry_ccod"));
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            0,
            "population must be 0 -- CCOD marker present, zero property rows"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "capable must equal population (0) when no property rows exist"
        );
    }

    #[tokio::test]
    async fn property_nodes_zero_rows_ocod_marker_only_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: property_nodes_zero_rows_ocod_marker_only_is_clear (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_property_nodes",
            "ocod_only",
        )
        .await;
        seed_enrichment_run(&db, "land_registry_ocod").await;
        let mut st = Strategy::default();
        let probe = PropertyNodes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "expected Clear when ONLY the OCOD marker is present, got {:?}",
            finding.verdict
        );
        assert!(finding.summary.contains("land_registry_ocod"));
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            0,
            "population must be 0 -- OCOD marker present, zero property rows"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "capable must equal population (0) when no property rows exist"
        );
    }

    #[tokio::test]
    async fn property_nodes_nonzero_rows_is_hit_with_correct_total() {
        // D3's non-zero branch: `attributed.value` is `rows.len()`, computed independently of
        // `inner_data`/the splitter, so the Verdict and total hold regardless of how the
        // per-producer split resolves. The two tests below cover that split itself.
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: property_nodes_nonzero_rows_is_hit_with_correct_total (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_property_nodes",
            "nonzero_total",
        )
        .await;
        seed_property(&db, "ccod-1", None).await;
        seed_property(&db, "ccod-2", None).await;
        seed_property(&db, "ocod-1", Some("Jersey")).await;
        let mut st = Strategy::default();
        let probe = PropertyNodes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { matches: 3, .. }),
            "expected Hit with matches=3, got {:?}",
            finding.verdict
        );
        assert!(finding.summary.contains("3 Property nodes"));
    }

    #[tokio::test]
    async fn property_nodes_nonzero_rows_states_per_producer_split() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: property_nodes_nonzero_rows_states_per_producer_split (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_property_nodes",
            "nonzero",
        )
        .await;
        // Two CCOD rows (no country_incorporated), one OCOD row (country_incorporated present).
        seed_property(&db, "ccod-1", None).await;
        seed_property(&db, "ccod-2", None).await;
        seed_property(&db, "ocod-1", Some("Jersey")).await;
        let mut st = Strategy::default();
        let probe = PropertyNodes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { matches: 3, .. }),
            "expected Hit with matches=3, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("land_registry_ccod=2"),
            "summary must name land_registry_ccod's count, got: {}",
            finding.summary
        );
        assert!(
            finding.summary.contains("land_registry_ocod=1"),
            "summary must name land_registry_ocod's count, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn property_nodes_nonzero_rows_names_silent_producer_at_zero() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: property_nodes_nonzero_rows_names_silent_producer_at_zero (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_property_nodes",
            "silent_producer",
        )
        .await;
        // All rows attributed to land_registry_ocod, zero to land_registry_ccod -- the split
        // line must still name land_registry_ccod, with 0.
        seed_property(&db, "ocod-only-1", Some("Bermuda")).await;
        let mut st = Strategy::default();
        let probe = PropertyNodes;

        let finding = probe.run(&db, &mut st).await;
        assert!(matches!(finding.verdict, Verdict::Hit { matches: 1, .. }));
        assert!(
            finding.summary.contains("land_registry_ccod=0"),
            "the silent producer must still be named with 0, got: {}",
            finding.summary
        );
        assert!(finding.summary.contains("land_registry_ocod=1"));
    }

    #[tokio::test]
    async fn property_nodes_nonzero_rows_attaches_capped_entity_refs_alongside_split() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: property_nodes_nonzero_rows_attaches_capped_entity_refs_alongside_split \
                 (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_property_nodes",
            "capped_refs",
        )
        .await;
        // 5 CCOD rows, 4 OCOD rows -- 9 total, one over DISPLAY_CAP.
        for i in 0..5 {
            seed_property(&db, &format!("ccod-{i}"), None).await;
        }
        for i in 0..4 {
            seed_property(&db, &format!("ocod-{i}"), Some("Jersey")).await;
        }
        let mut st = Strategy::default();
        let probe = PropertyNodes;

        let finding = probe.run(&db, &mut st).await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { matches: 9, .. }),
            "expected Hit with matches=9, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains("land_registry_ccod=5"),
            "summary must name land_registry_ccod's count, got: {}",
            finding.summary
        );
        assert!(
            finding.summary.contains("land_registry_ocod=4"),
            "summary must name land_registry_ocod's count, got: {}",
            finding.summary
        );

        let total_refs: usize = finding.evidence.iter().map(|e| e.refs.len()).sum();
        assert_eq!(
            total_refs, 8,
            "refs must be capped at DISPLAY_CAP=8, got {total_refs}"
        );
        assert!(
            finding
                .evidence
                .iter()
                .flat_map(|e| e.refs.iter())
                .all(|r| r.kind == RecordKind::Entity),
            "every ref must be RecordKind::Entity"
        );

        // The pre-existing split-text row is unaffected by the cap -- it still carries refs: [].
        let split_row = finding
            .evidence
            .iter()
            .find(|e| e.text.contains("Per-producer split"))
            .expect("split-text row must still exist");
        assert!(
            split_row.refs.is_empty(),
            "the split-text row must never carry refs"
        );
    }
}
