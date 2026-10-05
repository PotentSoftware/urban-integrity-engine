//! `DisqualifiedDirectorEdges` Tier-2 probe -- Plan 33-08 Task 1.
//!
//! Ports Python's `probe_disqualified_director_edges` faithfully
//! (`scripts/analyst_probe.py:1472-1536`) -- including its narrower-than-obvious emptiness check
//! (`total_risk_edges == 0`, computed from `DisqualifiedFrom + Controls + OffshoreLink` only;
//! `DonatesTo`/`TrusteeOf` are reported in the coverage table but never counted toward that
//! magnitude, matching Python exactly) -- routed entirely through [`super::attributed`] (VRD-08/
//! D1). No leaks apply to this probe (that is D2's `council_registers_political_actors` pair, see
//! this plan's Task 2); this port needs no correction, only faithful transcription.

use std::collections::{BTreeMap, BTreeSet};

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::{queries, scan};
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, SourceAttribution, Verdict,
};

use super::attributed;
use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// The 10 OSINT adapter source keys whose `enrichment_runs` marker signals that SOME OSINT
/// adapter ran -- mirrors Python's `_OSINT_SOURCES` exactly (`analyst_probe.py:497-501`).
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

/// Cap on how many disqualified-director node ids are individually named in the evidence text --
/// a DISPLAY cap only, never applied to the reported count (Python's `disq_directors[:8]`,
/// `analyst_probe.py:1519`).
const DISPLAY_CAP: usize = 8;

/// Build a `DataGap` `Finding` from an [`attributed::AttributionError`] (or any other
/// mid-evaluation scan failure) -- `Probe::run` is infallible by design (PRB-08), so every error
/// path returns a `Finding`, never propagates.
///
/// `attributions` carries whatever `Attributed.producers` were already collected by the time this
/// early-return branch fires -- `vec![]` for a branch reached before any `attributed::` call has
/// succeeded, or the concatenation of every producer list gathered so far for a branch reached
/// partway through the 5-call sequence (this plan's `<interfaces>` rule).
fn data_gap_finding(
    probe: &str,
    missing_input: impl Into<String>,
    severity: Severity,
    attributions: Vec<SourceAttribution>,
) -> Finding {
    let missing_input = missing_input.into();
    Finding {
        probe: probe.to_string(),
        verdict: Verdict::data_gap(missing_input.clone()),
        summary: missing_input,
        cause: None,
        severity,
        evidence: vec![],
        attributions,
        remediation_hint: None,
    }
}

/// `Attributed.producers` -> `Vec<SourceAttribution>`, the typed carrier `Finding.attributions`
/// requires (RPT-05 check 2).
fn to_attributions(producers: &[(&'static str, u64)]) -> Vec<SourceAttribution> {
    producers
        .iter()
        .map(|(source, value)| SourceAttribution {
            source,
            value: *value,
        })
        .collect()
}

pub struct DisqualifiedDirectorEdges;

#[async_trait::async_trait]
impl Probe for DisqualifiedDirectorEdges {
    fn name(&self) -> &'static str {
        "osint-risk-edges"
    }

    /// Searchable by the findings-by-entity index (PIV-02): the roll-call row carries an `Entity` ref per intersecting id.
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::NamesNodes
    }

    /// A single typed edge between two entities (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::TypedEdge
    }

    fn tier(&self) -> &'static str {
        "Tier-2"
    }

    fn hypothesis(&self) -> &'static str {
        "Do disqualified-director edges intersect with other OSINT signals -- Controls, \
         OffshoreLink, donation, TrusteeOf?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        // DisqualifiedFrom: fetch the raw rows once (needed for the director-id display list
        // below) and derive the Attributed count from those SAME rows via `from_rows`, rather
        // than issuing a second, redundant query for the count alone.
        let disq_rows = match scan::scan_events_by_payload_contains(
            db,
            "EdgeAdded",
            "weight_json",
            "\"type\":\"DisqualifiedFrom\"",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                // No Python analog -- defaulted to Severity::Medium per 35-01 convention.
                // Nothing collected yet, so attributions: vec![].
                return data_gap_finding(
                    self.name(),
                    format!("DisqualifiedFrom scan failed: {e}"),
                    Severity::Medium,
                    vec![],
                );
            }
        };
        let disq = match attributed::from_rows("DisqualifiedFrom", "ch_disqualified", &disq_rows) {
            Ok(a) => a,
            Err(e) => {
                // No Python analog -- Severity::Medium; from_rows failed, so nothing collected.
                return data_gap_finding(
                    self.name(),
                    format!("DisqualifiedFrom attribution failed: {e}"),
                    Severity::Medium,
                    vec![],
                );
            }
        };

        // Controls: fetch-then-attribute, same shape as DisqualifiedFrom above (Assumption A3,
        // the plan 49-01 summary -- on the live corpus Controls is the edge type actually driving this
        // probe's Hits, and it had no ref-construction path until this widening).
        let controls_rows = match scan::scan_events_by_payload_contains(
            db,
            "EdgeAdded",
            "weight_json",
            "\"type\":\"Controls\"",
        )
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                // DisqualifiedFrom succeeded before this call -- carry its producers forward.
                return data_gap_finding(
                    self.name(),
                    format!("Controls scan failed: {e}"),
                    Severity::Medium,
                    to_attributions(&disq.producers),
                );
            }
        };
        let controls = match attributed::from_rows("Controls", "ch_psc", &controls_rows) {
            Ok(a) => a,
            Err(e) => {
                return data_gap_finding(
                    self.name(),
                    format!("Controls attribution failed: {e}"),
                    Severity::Medium,
                    to_attributions(&disq.producers),
                );
            }
        };
        // `OffshoreLink` has an empty producer slice in REGISTRY (no adapter emits it today) --
        // there is no single `source` to attribute to, so this routes through
        // `count_across_producers` (D3's shape) instead of `count_for_source`.
        let offshore =
            match attributed::count_across_producers(db, "EdgeAdded", "OffshoreLink", None).await {
                Ok(a) => a,
                Err(e) => {
                    return data_gap_finding(
                        self.name(),
                        format!("OffshoreLink attribution failed: {e}"),
                        Severity::Medium,
                        to_attributions(
                            &[disq.producers.as_slice(), controls.producers.as_slice()].concat(),
                        ),
                    )
                }
            };
        let donation = match attributed::count_for_source(
            db,
            "EdgeAdded",
            "DonatesTo",
            "electoral_commission",
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                return data_gap_finding(
                    self.name(),
                    format!("DonatesTo attribution failed: {e}"),
                    Severity::Medium,
                    to_attributions(
                        &[
                            disq.producers.as_slice(),
                            controls.producers.as_slice(),
                            offshore.producers.as_slice(),
                        ]
                        .concat(),
                    ),
                )
            }
        };
        let trustee =
            match attributed::count_for_source(db, "EdgeAdded", "TrusteeOf", "charity_commission")
                .await
            {
                Ok(a) => a,
                Err(e) => {
                    return data_gap_finding(
                        self.name(),
                        format!("TrusteeOf attribution failed: {e}"),
                        Severity::Medium,
                        to_attributions(
                            &[
                                disq.producers.as_slice(),
                                controls.producers.as_slice(),
                                offshore.producers.as_slice(),
                                donation.producers.as_slice(),
                            ]
                            .concat(),
                        ),
                    )
                }
            };

        // Python's exact, narrower-than-obvious magnitude: DonatesTo/TrusteeOf are reported in
        // the coverage table below but never counted here. Do not "fix" this -- it is not a named
        // defect in this phase's scope (this plan's `<interfaces>` note).
        let total_risk_edges = disq.value + controls.value + offshore.value;

        // D-02: the coverage table moves to `evidence`, one EvidenceRow per edge type -- `summary`
        // stays a single sentence. `attributions` concatenates ALL FIVE `attributed::` calls' own
        // producers (this plan's `<interfaces>` rule) -- by this point in the function all five
        // have already succeeded (every early return above happens before reaching here).
        let mut evidence = vec![
            EvidenceRow {
                text: format!(
                    "{:>6}  DisqualifiedFrom -- individuals barred from directorships (CRITICAL risk)",
                    disq.value
                ),
                refs: vec![],
            },
            EvidenceRow {
                text: format!(
                    "{:>6}  Controls -- beneficial ownership / PSC chains (HIGH risk)",
                    controls.value
                ),
                refs: vec![],
            },
            EvidenceRow {
                text: format!(
                    "{:>6}  OffshoreLink -- connections to offshore jurisdictions (HIGH risk)",
                    offshore.value
                ),
                refs: vec![],
            },
            EvidenceRow {
                text: format!(
                    "{:>6}  DonatesTo -- political donation bridges (MEDIUM risk)",
                    donation.value
                ),
                refs: vec![],
            },
            EvidenceRow {
                text: format!(
                    "{:>6}  TrusteeOf -- charity trustee relationships (MEDIUM risk)",
                    trustee.value
                ),
                refs: vec![],
            },
        ];
        let attributions = to_attributions(
            &[
                disq.producers.as_slice(),
                controls.producers.as_slice(),
                offshore.producers.as_slice(),
                donation.producers.as_slice(),
                trustee.producers.as_slice(),
            ]
            .concat(),
        );

        if total_risk_edges == 0 {
            // Implies `disq.value == 0` too (it is one of the three summands) -- matches
            // Python's `if total_risk_edges == 0 and not disq_edges` exactly (the second half of
            // that condition is always true whenever the first is).
            let osint_ran = {
                let runs = st.enrichment_runs(db).await;
                OSINT_SOURCES.iter().any(|s| runs.contains_key(*s))
            } || matches!(
                scan::scan_identity_links(db, None, None).await,
                Ok(rows) if !rows.is_empty()
            );
            let missing_input = if osint_ran {
                "No OSINT-sourced risk edges in the graph, but enrichment HAS run (run markers \
                 and/or identity links exist)."
                    .to_string()
            } else {
                "No OSINT-sourced risk edges in the graph. Phase 17+ adapters (CH PSC, \
                 Disqualified Directors, ICIJ) have not run yet."
                    .to_string()
            };
            evidence.push(EvidenceRow {
                text: missing_input.clone(),
                refs: vec![],
            });
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(missing_input.clone()),
                summary: missing_input,
                cause: None,
                // DATA_GAP/MEDIUM at analyst_probe.py:1498/1508 -- both the osint_ran and
                // !osint_ran sub-branches share this severity in Python.
                severity: Severity::Medium,
                evidence,
                attributions,
                remediation_hint: None,
            };
        }

        // Widened (phase 50 plan 2, Pitfall 1 fix): pair each officer (`src_id`) with the
        // specific entities they direct/control (`dst_id`), rather than two independent flat id
        // lists with `dst_id` discarded. `BTreeMap`/`BTreeSet` give deterministic ascending
        // officer/target order for free -- matching `shared_exact_address.rs`'s own "sorted,
        // ties broken by id" discipline without a manual `.sort()`/`.dedup()` pass. This is a
        // DISPLAY-only widening: `disq.value`/`controls.value`/`total_risk_edges`/`severity`/
        // `summary` are computed above, unchanged.
        if disq.value > 0 {
            let mut officer_targets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for r in &disq_rows {
                let Some(officer) = queries::extract_id_string(&r["payload"]["src_id"]) else {
                    continue;
                };
                let entry = officer_targets.entry(officer).or_default();
                if let Some(target) = queries::extract_id_string(&r["payload"]["dst_id"]) {
                    entry.insert(target);
                }
            }
            for (officer, targets) in officer_targets.iter().take(DISPLAY_CAP) {
                let capped: Vec<&String> = targets.iter().take(DISPLAY_CAP).collect();
                let mut refs = vec![RecordRef {
                    kind: RecordKind::Entity,
                    id: officer.clone(),
                }];
                refs.extend(capped.iter().map(|id| RecordRef {
                    kind: RecordKind::Entity,
                    id: (*id).clone(),
                }));
                evidence.push(EvidenceRow {
                    text: format!(
                        "DisqualifiedFrom officer {officer} directs {} (showing up to \
                         {DISPLAY_CAP}): {}",
                        plural(targets.len() as u64, "entity", "entities"),
                        capped
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    refs,
                });
            }
        }

        if controls.value > 0 {
            let mut officer_targets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for r in &controls_rows {
                let Some(officer) = queries::extract_id_string(&r["payload"]["src_id"]) else {
                    continue;
                };
                let entry = officer_targets.entry(officer).or_default();
                if let Some(target) = queries::extract_id_string(&r["payload"]["dst_id"]) {
                    entry.insert(target);
                }
            }
            for (officer, targets) in officer_targets.iter().take(DISPLAY_CAP) {
                let capped: Vec<&String> = targets.iter().take(DISPLAY_CAP).collect();
                let mut refs = vec![RecordRef {
                    kind: RecordKind::Entity,
                    id: officer.clone(),
                }];
                refs.extend(capped.iter().map(|id| RecordRef {
                    kind: RecordKind::Entity,
                    id: (*id).clone(),
                }));
                evidence.push(EvidenceRow {
                    text: format!(
                        "Controls officer {officer} controls {} (showing up to {DISPLAY_CAP}): {}",
                        plural(targets.len() as u64, "entity", "entities"),
                        capped
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    refs,
                });
            }
        }

        let severity = if disq.value > 0 {
            Severity::Critical
        } else {
            Severity::High
        };
        let summary = format!(
            "{}, {} Controls, {} OffshoreLink. {}",
            plural(
                disq.value,
                "DisqualifiedFrom edge",
                "DisqualifiedFrom edges"
            ),
            controls.value,
            offshore.value,
            if disq.value > 0 {
                "DISQUALIFIED DIRECTORS ACTIVE IN PROCUREMENT -- cross-reference contract awards \
                 immediately."
            } else {
                "Beneficial ownership and offshore structure data present."
            }
        );

        // `population == capable == matches == total_risk_edges`, `hit_threshold == 1`: every
        // risk edge found is itself the measured, fully-capable population, and any nonzero
        // total is unconditionally a Hit (matches Python's binary HIT/DATA_GAP shape -- there is
        // no WEAK branch in the original).
        let verdict =
            // `Clear` is structurally unreachable here: the `total_risk_edges == 0` guard
            // above returns `DataGap` before this line, so `total_risk_edges > 0` always,
            // making `matches > 0 >= hit_threshold (1)` -- unconditionally `Hit`. Proven by
            // `disqualified_from_zero_but_controls_nonzero_is_hit_high`.
            Verdict::classify(total_risk_edges, total_risk_edges, total_risk_edges, 1, "");

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
    use chrono::Utc;
    use urban_integrity_schema::types::edge::{DisqualifiedFromData, EdgeMeta, EdgeWeight};

    use super::*;

    fn meta() -> EdgeMeta {
        EdgeMeta {
            created_at: Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

    /// Seed one `EdgeAdded` `graph_events` row carrying `weight` -- the same raw shape every
    /// adapter's `AddEdge` mutation ultimately produces (`src_id`/`dst_id`/`weight_json`).
    async fn seed_edge(
        db: &Surreal<Any>,
        event_id: &str,
        src: &str,
        dst: &str,
        weight: EdgeWeight,
    ) {
        let weight_json = serde_json::to_string(&weight).expect("serialize edge weight");
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": {
                        "src_id": src,
                        "dst_id": dst,
                        "weight_json": weight_json,
                    },
                }),
            ))
            .await
            .expect("send seed CREATE")
            .check()
            .expect("seed CREATE check");
    }

    async fn seed_disqualified_from(db: &Surreal<Any>, event_id: &str, src: &str, dst: &str) {
        seed_edge(
            db,
            event_id,
            src,
            dst,
            EdgeWeight::DisqualifiedFrom {
                meta: meta(),
                data: DisqualifiedFromData {
                    disqualified_from: None,
                    disqualified_until: None,
                    case_number: None,
                },
            },
        )
        .await;
    }

    async fn seed_controls(db: &Surreal<Any>, event_id: &str, src: &str, dst: &str) {
        seed_edge(
            db,
            event_id,
            src,
            dst,
            EdgeWeight::Controls { meta: meta() },
        )
        .await;
    }

    async fn seed_donates_to(db: &Surreal<Any>, event_id: &str, src: &str, dst: &str) {
        seed_edge(
            db,
            event_id,
            src,
            dst,
            EdgeWeight::DonatesTo { meta: meta() },
        )
        .await;
    }

    fn skip_or_connect(prefix: &str, suffix: &str) -> Option<()> {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: {prefix}::{suffix} (set URBAN_DB_TESTS=1)");
            return None;
        }
        Some(())
    }

    #[tokio::test]
    async fn all_five_edge_types_zero_is_data_gap() {
        if skip_or_connect("disqualified_director_edges", "all_zero").is_none() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_disq_director_edges",
            "all_zero",
        )
        .await;

        let finding = DisqualifiedDirectorEdges
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap, got {:?}",
            finding.verdict
        );
    }

    /// Python's exact, narrower-than-obvious emptiness check: `total_risk_edges` (DisqualifiedFrom
    /// + Controls + OffshoreLink) is zero even though DonatesTo is nonzero -- must still be
    /// DataGap, proving the magnitude was NOT accidentally widened to include DonatesTo/TrusteeOf.
    #[tokio::test]
    async fn total_risk_edges_zero_but_donates_to_nonzero_is_still_data_gap() {
        if skip_or_connect("disqualified_director_edges", "donates_to_only").is_none() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_disq_director_edges",
            "donates_to_only",
        )
        .await;
        seed_donates_to(&db, "edge-donates-1", "political_actor:pa1", "entity:e1").await;

        let finding = DisqualifiedDirectorEdges
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap even with a nonzero DonatesTo count, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn disqualified_from_nonzero_is_hit_critical() {
        if skip_or_connect("disqualified_director_edges", "disq_nonzero").is_none() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_disq_director_edges",
            "disq_nonzero",
        )
        .await;
        seed_disqualified_from(&db, "edge-disq-1", "director:jane-smith", "disq:case1").await;

        let finding = DisqualifiedDirectorEdges
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "expected Hit, got {:?}",
            finding.verdict
        );
        // Severity is the typed `Finding::severity` field since 35-01 widened `Finding` -- it
        // is no longer a `[SEVERITY]` tag embedded in the summary prose, so assert on the field
        // (the coverage table's fixed per-row risk labels would make a prose substring match
        // pass vacuously anyway).
        assert_eq!(
            finding.severity,
            Severity::Critical,
            "a non-zero DisqualifiedFrom count must be Critical, got {:?}",
            finding.severity
        );
    }

    #[tokio::test]
    async fn disqualified_from_zero_but_controls_nonzero_is_hit_high() {
        if skip_or_connect("disqualified_director_edges", "controls_only").is_none() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_disq_director_edges",
            "controls_only",
        )
        .await;
        seed_controls(&db, "edge-controls-1", "psc:p1", "entity:e1").await;

        let finding = DisqualifiedDirectorEdges
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "expected Hit, got {:?}",
            finding.verdict
        );
        // Same note as above: severity is the typed field, not prose.
        assert_eq!(
            finding.severity,
            Severity::High,
            "severity must be High (not Critical) when DisqualifiedFrom is zero, got {:?}",
            finding.severity
        );
        assert!(
            !matches!(finding.verdict, Verdict::Clear(_)),
            "osint-risk-edges must never reach Clear -- total_risk_edges==0 is guarded to DataGap before classify runs, so matches is always >0 and hit_threshold=1, forcing Hit"
        );
    }

    /// Task 1 (phase 50 plan 2, widened from the earlier Task 2 draft of this test): 9 distinct
    /// officers, one Controls edge each (distinct PSC `src_id` -> distinct `entity:e{i}`
    /// `dst_id`) -- proves the widening caps at DISPLAY_CAP=8 *officers* (one `EvidenceRow` per
    /// officer, ordered ascending by id), each row pairing its officer with its own target, and
    /// that the widening alone drives a Hit (`total_risk_edges == controls.value == 9`).
    #[tokio::test]
    async fn osint_risk_edges_controls_only_attaches_capped_entity_refs() {
        if skip_or_connect("disqualified_director_edges", "controls_capped_refs").is_none() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_disq_director_edges",
            "controls_capped_refs",
        )
        .await;
        for i in 0..9 {
            seed_controls(
                &db,
                &format!("edge-controls-{i}"),
                &format!("psc:p{i}"),
                &format!("entity:e{i}"),
            )
            .await;
        }

        let finding = DisqualifiedDirectorEdges
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "expected Hit, got {:?}",
            finding.verdict
        );
        let controls_rows: Vec<_> = finding
            .evidence
            .iter()
            .filter(|r| r.text.starts_with("Controls officer "))
            .collect();
        assert_eq!(
            controls_rows.len(),
            DISPLAY_CAP,
            "expected exactly DISPLAY_CAP (8) officer rows (one per officer, capped at 9 -> 8), \
             got {}: {:?}",
            controls_rows.len(),
            controls_rows.iter().map(|r| &r.text).collect::<Vec<_>>()
        );
        for row in &controls_rows {
            assert_eq!(
                row.refs.len(),
                2,
                "each officer row must carry exactly 2 refs (officer + its 1 target), got {:?}",
                row.refs
            );
            assert!(
                row.refs.iter().all(|r| r.kind == RecordKind::Entity),
                "every Controls ref must be RecordKind::Entity, got {:?}",
                row.refs
            );
        }
        // Officer order is ascending by id (BTreeMap) -- the first capped row must be psc:p0.
        assert_eq!(controls_rows[0].refs[0].id, "psc:p0");
    }

    /// Task 1 (phase 50 plan 2, widened from the earlier Task 2 draft of this test): one
    /// DisqualifiedFrom edge AND one Controls edge together -- proves the Controls widening did
    /// not regress the DisqualifiedFrom ref path; each edge type contributes its own
    /// officer-and-target row.
    #[tokio::test]
    async fn osint_risk_edges_disqualified_from_and_controls_both_attach_refs() {
        if skip_or_connect("disqualified_director_edges", "disq_and_controls_both_ref").is_none() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_disq_director_edges",
            "disq_and_controls_both_ref",
        )
        .await;
        seed_disqualified_from(&db, "edge-disq-1", "director:jane-smith", "disq:case1").await;
        seed_controls(&db, "edge-controls-1", "psc:p1", "entity:e1").await;

        let finding = DisqualifiedDirectorEdges
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "expected Hit, got {:?}",
            finding.verdict
        );
        let disq_row = finding
            .evidence
            .iter()
            .find(|r| r.text.starts_with("DisqualifiedFrom officer "))
            .expect("a DisqualifiedFrom officer evidence row must be present");
        let controls_row = finding
            .evidence
            .iter()
            .find(|r| r.text.starts_with("Controls officer "))
            .expect("a Controls officer evidence row must be present");
        assert_eq!(
            disq_row.refs.len(),
            2,
            "expected exactly 2 refs (officer + target)"
        );
        assert_eq!(
            controls_row.refs.len(),
            2,
            "expected exactly 2 refs (officer + target)"
        );
        assert_eq!(disq_row.refs[0].id, "director:jane-smith");
        assert_eq!(disq_row.refs[1].id, "disq:case1");
        assert_eq!(controls_row.refs[0].id, "psc:p1");
        assert_eq!(controls_row.refs[1].id, "entity:e1");
        assert_ne!(
            disq_row.refs[0].id, controls_row.refs[0].id,
            "the two edge types' refs must carry different ids, got {:?} vs {:?}",
            disq_row.refs[0].id, controls_row.refs[0].id
        );
        assert_eq!(disq_row.refs[0].kind, RecordKind::Entity);
        assert_eq!(controls_row.refs[0].kind, RecordKind::Entity);
    }
}
