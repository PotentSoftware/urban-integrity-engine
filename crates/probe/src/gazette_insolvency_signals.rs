//! `GazetteInsolvencySignals` -- ports `probe_gazette_insolvency_signals`
//! (`scripts/analyst_probe.py:1619-1699`).
//!
//! Python's original reads the InLiquidation row set with a raw `LIMIT 500` (CR-02: row-
//! returning, `kind` filtered client-side in Python, not in the `WHERE` clause) -- the exact
//! truncation-risk shape this port must NOT reproduce. `scan::scan_events_by_payload_contains`
//! pages the whole table (T-33-14), closing that hazard: no ceiling on the underlying data, only
//! on the evidence text's DISPLAY sample of up to 20 flagged entities (the reported intersection
//! COUNT is always the full size, never capped).
//!
//! The fetched InLiquidation row set is wrapped via `attributed::from_rows` for the evidence
//! figure -- this is the first probe in the phase to exercise that path for real: it needs the
//! raw rows for the AwardedTo intersection, not just a count, so `from_rows` avoids a second
//! query while still proving the reported figure is attributed to `london_gazette` (InLiquidation
//! has exactly one registered producer, no discriminator).

use std::collections::HashSet;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, SourceAttribution, Verdict,
};

use super::attributed;
use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// `payload.entity_id`/`payload.src_id` as a string, whether SurrealDB handed it back as a plain
/// string or coerced it into a RecordId object. Local equivalent of `scan.rs`'s private
/// `entity_id_of`/`record_id_str` -- neither is exported, so this mirrors `ward_concentration.rs`'s
/// identical local copy (this plan's `<interfaces>` note: `scan.rs`'s own visibility is out of
/// scope here).
fn record_id_str(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    let tb = v.get("tb").and_then(|x| x.as_str())?;
    let id = v.get("id")?;
    let id_str = id
        .as_str()
        .map(str::to_owned)
        .or_else(|| id.get("String").and_then(|x| x.as_str()).map(str::to_owned))
        .or_else(|| id.as_i64().map(|n| n.to_string()))?;
    Some(format!("{tb}:{id_str}"))
}

/// Converts an [`attributed::Attributed`] value's already-split per-producer breakdown into
/// `Finding.attributions` -- copied directly, never re-derived, matching plan 35-04's wiring.
fn attributions_of(attributed: &attributed::Attributed) -> Vec<SourceAttribution> {
    attributed
        .producers
        .iter()
        .map(|(source, value)| SourceAttribution {
            source,
            value: *value,
        })
        .collect()
}

pub struct GazetteInsolvencySignals;

#[async_trait::async_trait]
impl Probe for GazetteInsolvencySignals {
    fn name(&self) -> &'static str {
        "gazette-insolvency-signals"
    }

    /// Searchable by the findings-by-entity index (PIV-02): each flagged row is one id and carries its `Entity` ref.
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
        "Tier-2"
    }

    fn hypothesis(&self) -> &'static str {
        "Does any entity the London Gazette reports as in liquidation still hold a live \
         contract award in the graph?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        let inliq_rows = match scan::scan_events_by_payload_contains(
            db,
            "NodeUpdated",
            "weight_json",
            "InLiquidation",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                // A scan failure is a DataGap Finding, not a propagated Result -- Probe::run
                // is infallible by design (PRB-08).
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("InLiquidation scan failed: {e}")),
                    summary: "InLiquidation event scan failed -- gazette insolvency \
                                  signals cannot be evaluated."
                        .to_string(),
                    cause: None,
                    // No direct Python branch analog for a raw scan failure; nearest analog is
                    // analyst_probe.py:2551's generic probe-exception handler (DATA_GAP/INFO).
                    severity: Severity::Info,
                    evidence: vec![EvidenceRow {
                        text: format!("InLiquidation scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        if inliq_rows.is_empty() {
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap("No NodeUpdated events with InLiquidation status found"),
                summary: "No NodeUpdated events with InLiquidation status found.".to_string(),
                cause: None,
                // analyst_probe.py:1641 — this branch's DATA_GAP/MEDIUM pair, 1:1.
                severity: Severity::Medium,
                evidence: vec![EvidenceRow {
                    text: "Run `./scripts/enrich.sh --source london_gazette` to poll the \
                           Gazette API."
                        .to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // D1: wraps the already-fetched row set (no second query) into an Attributed value --
        // proves the reported InLiquidation figure is attributed to london_gazette without a
        // second query. InLiquidation has no discriminator, so this is exactly `inliq_rows.len()`.
        let attributed_inliq =
            match attributed::from_rows("InLiquidation", "london_gazette", &inliq_rows) {
                Ok(a) => a,
                Err(e) => {
                    return Finding {
                        probe: self.name().to_string(),
                        verdict: Verdict::data_gap(format!(
                            "InLiquidation attribution failed: {e}"
                        )),
                        summary: "InLiquidation row set could not be attributed -- gazette \
                              insolvency signals cannot be evaluated."
                            .to_string(),
                        cause: None,
                        // No Python analog (attribution is a Rust-side D1 mechanism) —
                        // defaulted to Severity::Medium per 35-01's analog-less DataGap rule.
                        severity: Severity::Medium,
                        evidence: vec![EvidenceRow {
                            text: format!("Attribution error: {e}"),
                            refs: vec![],
                        }],
                        attributions: vec![],
                        remediation_hint: None,
                    };
                }
            };
        let population = attributed_inliq.value;

        let inliq_entities: HashSet<String> = inliq_rows
            .iter()
            .filter_map(|r| record_id_str(&r["payload"]["entity_id"]))
            .collect();

        let awarded_rows = match scan::scan_events_by_payload_contains(
            db,
            "EdgeAdded",
            "weight_json",
            "\"type\":\"AwardedTo\"",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("AwardedTo edge scan failed: {e}")),
                    summary: "AwardedTo edge scan failed -- gazette insolvency intersection \
                              cannot be evaluated."
                        .to_string(),
                    cause: None,
                    // No direct Python branch analog for a raw scan failure; nearest analog is
                    // analyst_probe.py:2551's generic probe-exception handler (DATA_GAP/INFO).
                    severity: Severity::Info,
                    evidence: vec![EvidenceRow {
                        text: format!("AwardedTo scan error: {e}"),
                        refs: vec![],
                    }],
                    // The InLiquidation figure WAS attributed before this scan failed, so its
                    // producer split is carried through rather than dropped.
                    attributions: attributions_of(&attributed_inliq),
                    remediation_hint: None,
                };
            }
        };
        let awarded_entities: HashSet<String> = awarded_rows
            .iter()
            .filter_map(|r| record_id_str(&r["payload"]["src_id"]))
            .collect();

        let mut flagged: Vec<String> = inliq_entities
            .intersection(&awarded_entities)
            .cloned()
            .collect();
        flagged.sort();

        let summary = format!(
            "Entities with InLiquidation status (from Gazette): {population}. Entities with \
             AwardedTo edges (contract holders): {}. Intersection -- in liquidation AND holds \
             contracts: {}.",
            awarded_entities.len(),
            flagged.len()
        );

        // D-02: the flagged-entity roll-call and the no-intersection explanation both used to be
        // appended to `summary`; they are evidence rows now, and every flagged entity carries a
        // typed `Entity` pivot so the analyst can open it directly.
        let mut evidence: Vec<EvidenceRow> = Vec::new();
        if flagged.is_empty() {
            evidence.push(EvidenceRow {
                text: format!(
                    "{} marked InLiquidation by the Gazette, but none intersect with \
                     contract-award records. Expected when Gazette-matched companies hold no \
                     Contracts Finder awards.",
                    plural(population, "entity", "entities")
                ),
                refs: vec![],
            });
        } else {
            evidence.push(EvidenceRow {
                text: format!(
                    "{} in liquidation AND holding live contracts:",
                    plural(flagged.len() as u64, "entity", "entities")
                ),
                refs: vec![],
            });
            // T-33-14: this is a DISPLAY cap only -- the reported intersection COUNT above is
            // the full `flagged.len()`, never capped.
            for id in flagged.iter().take(20) {
                evidence.push(EvidenceRow {
                    text: id.clone(),
                    refs: vec![RecordRef {
                        kind: RecordKind::Entity,
                        id: id.clone(),
                    }],
                });
            }
            if flagged.len() > 20 {
                evidence.push(EvidenceRow {
                    text: format!("... and {} more", flagged.len() - 20),
                    refs: vec![],
                });
            }
        }

        // No Weak branch -- Python's own vocabulary here is only HIT/CRITICAL or CLEAR/MEDIUM
        // (`analyst_probe.py:1683/1689`).
        let verdict = Verdict::classify(population, population, flagged.len() as u64, 1, "");
        let severity = if flagged.is_empty() {
            Severity::Medium
        } else {
            Severity::Critical
        };

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            severity,
            evidence,
            attributions: attributions_of(&attributed_inliq),
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{AwardedToData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

    async fn seed_inliquidation_event(db: &Surreal<Any>, entity_id: &str) {
        let weight = NodeWeight::Entity(EntityData {
            name: format!("Entity {entity_id}"),
            company_number: None,
            status: EntityStatus::InLiquidation(chrono::Utc::now()),
            incorporation_date: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("NodeUpdated-{entity_id}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeUpdated",
                    "payload": { "entity_id": entity_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed InLiquidation event");
    }

    async fn seed_awarded_to_edge(db: &Surreal<Any>, event_id: &str, src_id: &str, dst_id: &str) {
        let edge = EdgeWeight::AwardedTo {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: AwardedToData {
                details: String::new(),
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
            .expect("failed to seed AwardedTo edge");
    }

    #[tokio::test]
    async fn gazette_zero_inliquidation_is_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: gazette_zero_inliquidation_is_data_gap (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "gazette_zero",
        )
        .await;
        let mut st = Strategy::default();

        let probe = GazetteInsolvencySignals;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero InLiquidation events must be DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn gazette_inliquidation_with_zero_intersection_is_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: gazette_inliquidation_with_zero_intersection_is_clear (set \
                 URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "gazette_zero_intersect",
        )
        .await;
        seed_inliquidation_event(&db, "GB-COH:GAZ-0001").await;
        seed_inliquidation_event(&db, "GB-COH:GAZ-0002").await;
        // An AwardedTo edge exists, but its src does NOT overlap the InLiquidation set.
        seed_awarded_to_edge(
            &db,
            "awarded-1",
            "GB-COH:NOT-INLIQ",
            "planning:some-contract",
        )
        .await;
        let mut st = Strategy::default();

        let probe = GazetteInsolvencySignals;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "InLiquidation entities present but zero intersection with AwardedTo must be \
             Clear, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains('2'),
            "evidence must state the InLiquidation population figure, got: {}",
            finding.summary
        );
        assert!(
            finding.summary.contains('1'),
            "evidence must state the AwardedTo population figure, got: {}",
            finding.summary
        );
    }

    #[tokio::test]
    async fn gazette_intersection_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: gazette_intersection_hit (set URBAN_DB_TESTS=1)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "gazette_hit",
        )
        .await;
        seed_inliquidation_event(&db, "GB-COH:GAZ-HIT-1").await;
        // The SAME entity id also appears as the src of an AwardedTo edge.
        seed_awarded_to_edge(
            &db,
            "awarded-hit-1",
            "GB-COH:GAZ-HIT-1",
            "planning:contract-1",
        )
        .await;
        let mut st = Strategy::default();

        let probe = GazetteInsolvencySignals;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "an InLiquidation entity that also holds an AwardedTo edge must be Hit, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|row| row.text.contains("GAZ-HIT-1")
                    && row
                        .refs
                        .iter()
                        .any(|r| r.kind == RecordKind::Entity && r.id.contains("GAZ-HIT-1"))),
            "evidence must name the flagged entity and carry an Entity pivot for it, got: {:?}",
            finding.evidence
        );
    }

    #[tokio::test]
    async fn gazette_population_figure_is_not_capped_at_twenty() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: gazette_population_figure_is_not_capped_at_twenty (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_probe_tier2",
            "gazette_over_twenty",
        )
        .await;
        // 25 InLiquidation entities, zero AwardedTo overlap -- proves the reported population
        // figure equals the full seeded count, not the 20-entry DISPLAY cap that only applies
        // to the (here empty) flagged-entities list.
        const N: usize = 25;
        for i in 0..N {
            seed_inliquidation_event(&db, &format!("GB-COH:GAZ-BIG-{i:03}")).await;
        }
        let mut st = Strategy::default();

        let probe = GazetteInsolvencySignals;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "zero-intersection population test must be Clear, got {:?}",
            finding.verdict
        );
        assert!(
            finding.summary.contains(&N.to_string()),
            "evidence must state the FULL seeded InLiquidation population ({N}), not the \
             20-entry display cap, got: {}",
            finding.summary
        );
        assert!(
            !finding.summary.contains("... and"),
            "the flagged-entities DISPLAY cap message must never fire when flagged is empty, \
             got: {}",
            finding.summary
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // The real VRD-12 proof. The summary substring check above is Pitfall 3 -- a string
        // mentioning 25 is not evidence that 25 is what Coverage reports. This asserts the
        // struct directly, and is what actually proves the 20-entry DISPLAY cap never leaks
        // into the population figure.
        assert_eq!(
            coverage.population(),
            25,
            "population must count the full seeded InLiquidation population (25), not the 20-entry display cap"
        );
        assert_eq!(
            coverage.capable(),
            25,
            "capable must equal population -- all 25 seeded entities were readable"
        );
    }
}
