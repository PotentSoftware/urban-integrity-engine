//! `CouncilRegistersPoliticalActors` Tier-2 probe -- Plan 33-08 Task 2, D2's two-leak fix.
//!
//! Ports Python's `probe_council_registers_political_actors`
//! (`scripts/analyst_probe.py:2032-2114`) for STRUCTURE only -- D2 (the phase 33 design notes, LOCKED)
//! replaces the leaky `Directs` edge count with `DeclaresInterest` and adds a run-marker gate
//! Python's original never had. This is VRD-08's own named defect: the Python original reported
//! `HIT/HIGH -- "3 PoliticalActor nodes and 24277 Directs edges"` when council registers had
//! never run and the 24,277 edges belonged entirely to `planning_identity`. Three independent
//! failure modes, three independent fixes:
//!
//! 1. **The marker gate** -- checked FIRST, structurally, before either count query executes.
//!    With no `enrichment_runs:council_registers` row, this returns `DataGap` unconditionally,
//!    regardless of what either count below would otherwise report.
//! 2. **The edge-type leak** -- counts `DeclaresInterest` (council_registers' own edge, single
//!    producer), never `Directs` (a `planning_identity`/CH-ingest edge type entirely outside
//!    council registers' provenance; `EdgeWeight::Directs` is schema-rejected from a
//!    `political_actor` source (the POL-02 directs-relate-rejected investigation notes, cited in
//!    D2) -- Council Registers literally cannot produce this edge type today).
//! 3. **The node-attribution leak** -- `PoliticalActor` has two producers
//!    (`council_registers`, `electoral_commission`); `is_council_registers_political_actor`
//!    (the REGISTRY discriminator, keyed on `council_name` presence) narrows the count to only
//!    the council_registers-shaped rows, excluding electoral_commission's `PA:{slug}` donation
//!    recipients.
//!
//! Directs edges are NOT counted here -- see `DeclaresInterest` above. A future edit
//! reintroducing a `Directs`-based count would silently reopen VRD-08's defect; this file's own
//! grep-based acceptance criterion (plan 33-08 Task 2) and this doc comment both exist to make
//! that reintroduction visible at review time, not just at test time (T-33-19).

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::attribution::is_council_registers_political_actor;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, Severity, SourceAttribution, Verdict,
};

use super::attributed;
use super::{EvidenceScope, MotifKind, Probe, Strategy};

pub struct CouncilRegistersPoliticalActors;

#[async_trait::async_trait]
impl Probe for CouncilRegistersPoliticalActors {
    fn name(&self) -> &'static str {
        "phase20-council-registers"
    }

    /// Can-name-entities, deferred: `pa_rows` from `scan_events_by_payload_contains` (~lines
    /// 184-191) are held per-row with their own `payload.entity_id` available, and that id is
    /// discarded in the party/council_name sample-breakdown loop (~lines 193-206), which reads
    /// only `weight_json` fields.
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
        "Does the graph hold council-registers-attributable political-actor evidence \
         (PoliticalActor nodes, DeclaresInterest edges), correctly distinguished from \
         electoral_commission's own PoliticalActor nodes and from planning_identity's Directs \
         edges?"
    }

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        // THE MARKER GATE -- checked FIRST, structurally, before either count query executes.
        // This is D2's direct fix for VRD-08's own scenario: council registers never ran, yet
        // proxy data (a planning_identity Directs edge, an electoral_commission PoliticalActor
        // node) was present and misread as council-registers evidence.
        let marker = st
            .enrichment_runs(db)
            .await
            .get("council_registers")
            .cloned();
        if marker.is_none() {
            // No Python analog -- the marker gate is D2's own structural addition, checked
            // before either count query runs. Defaulted to Medium per 35-01/35-02's
            // analog-less-DataGap convention.
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(
                    "Council Registers adapter has no enrichment_runs marker -- it has not run",
                ),
                summary: "Council Registers adapter has no enrichment_runs marker -- it has not \
                          run."
                    .to_string(),
                cause: None,
                severity: Severity::Medium,
                evidence: vec![EvidenceRow {
                    text: "Directs edges are NOT counted here -- see DeclaresInterest.".to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // `pa_count`: routed through Attributed's registry-gated discriminator
        // (`is_council_registers_political_actor`, checks `council_name.is_some()`) -- do not
        // write a second, redundant filter in this probe file; the discriminator IS the node
        // filtering.
        let pa = match attributed::count_for_source(
            db,
            "NodeAdded",
            "PoliticalActor",
            "council_registers",
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                // No Python analog -- `Attributed` refusal is this milestone's own mechanism.
                // Defaulted to Medium per 35-01/35-02's analog-less-DataGap convention. Nothing
                // was measured (the call itself failed), so no attributions to report.
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("PoliticalActor attribution failed: {e}")),
                    summary: format!("PoliticalActor attribution failed: {e}"),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };

        // `declares_count`: `DeclaresInterest` has exactly one producer (council_registers) in
        // REGISTRY, so no discriminator is needed -- the refusal path is unreachable here, but
        // this still routes through `Attributed` per criterion 6's wording (every evidence count
        // this probe reports is Attributed-wrapped, not just the ones that structurally need a
        // discriminator).
        let declares = match attributed::count_for_source(
            db,
            "EdgeAdded",
            "DeclaresInterest",
            "council_registers",
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                // No Python analog. Defaulted to Medium per 35-01/35-02's analog-less-DataGap
                // convention. `pa` succeeded above, so its producers ARE known -- report them;
                // `declares` itself failed, so there is nothing from that call to add.
                let attributions: Vec<SourceAttribution> = pa
                    .producers
                    .iter()
                    .map(|&(source, value)| SourceAttribution { source, value })
                    .collect();
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!("DeclaresInterest attribution failed: {e}")),
                    summary: format!("DeclaresInterest attribution failed: {e}"),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions,
                    remediation_hint: None,
                };
            }
        };

        // Both `attributed::count_for_source` calls succeeded past this point -- merge their
        // producer breakdowns once so every downstream branch reports the SAME attribution list
        // (both attribute to `council_registers`, so this is a concatenation, not a
        // dedup/merge decision, per this plan's `<interfaces>` rule).
        let attributions: Vec<SourceAttribution> = pa
            .producers
            .iter()
            .chain(declares.producers.iter())
            .map(|&(source, value)| SourceAttribution { source, value })
            .collect();

        // Sample breakdown (party/council_name presence), computed via the SAME discriminator
        // function this probe's node count already uses -- not a second, hand-rolled check.
        // Read directly off the raw `NodeAdded`/`PoliticalActor` rows' parsed `weight_json`
        // (`NodeWeight::PoliticalActor(PoliticalActorData{..})`'s internally-tagged
        // representation flattens `name`/`party`/`council_name` alongside the `"type"` tag in
        // the SAME JSON object -- there is no nested "content"/"data" wrapper for a NodeWeight
        // newtype variant, unlike EdgeWeight's struct variants). Matches Python's `with_party`/
        // `with_council` sample lines (`analyst_probe.py:2066-2076`).
        let pa_rows = scan::scan_events_by_payload_contains(
            db,
            "NodeAdded",
            "weight_json",
            "\"type\":\"PoliticalActor\"",
        )
        .await
        .unwrap_or_default();
        let (mut with_party, mut with_council) = (0u64, 0u64);
        for row in &pa_rows {
            let Some(wj) = row["payload"]["weight_json"].as_str() else {
                continue;
            };
            let Ok(weight) = serde_json::from_str::<serde_json::Value>(wj) else {
                continue;
            };
            if weight.get("party").and_then(|v| v.as_str()).is_some() {
                with_party += 1;
            }
            if is_council_registers_political_actor(&weight) {
                with_council += 1;
            }
        }

        let mut evidence: Vec<EvidenceRow> = vec![
            format!(
                "{:>6}  PoliticalActor nodes in graph (council_registers-attributed)",
                pa.value
            ),
            format!(
                "{:>6}  DeclaresInterest edges (councillor -> company)",
                declares.value
            ),
            format!(
                "{with_party:>6}  PoliticalActor nodes with party affiliation (Phase 20 field)"
            ),
            format!("{with_council:>6}  PoliticalActor nodes with council_name (Phase 20 field)"),
            "Directs edges are NOT counted here -- see DeclaresInterest.".to_string(),
        ]
        .into_iter()
        .map(|text| EvidenceRow { text, refs: vec![] })
        .collect();

        if pa.value == 0 {
            evidence.push(EvidenceRow {
                text: "Council Registers adapter requires Scrapling sidecar (localhost:8765) \
                       and LPA codes configured in [enricher.council_registers]. Run \
                       `urban-integrity enrich --source council_registers` after configuring."
                    .to_string(),
                refs: vec![],
            });
            let missing_input = "Council Registers adapter ran but found zero PoliticalActor \
                                  nodes with council_name set"
                .to_string();
            // Python analog: pa_count == 0 -> DATA_GAP/MEDIUM (analyst_probe.py:2088-2090).
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap(missing_input.clone()),
                summary: missing_input,
                cause: None,
                severity: Severity::Medium,
                evidence,
                attributions: attributions.clone(),
                remediation_hint: None,
            };
        }

        // `declares_count == 0` is NOT `Weak`, and is NOT `Clear`.
        //
        // Plan 33-08 mandates `Weak` here, carried across from Python's loose
        // "suggestive" WEAK bucket (`analyst_probe.py:2101`, used the same way throughout that
        // file). Rust's `Verdict::Weak { coverage, matches }` is a stricter thing: `classify`
        // reaches it only at `0 < matches < hit_threshold`, so it is unreachable at
        // `matches = 0` without fabricating a match count this probe did not measure.
        //
        // `Clear` was rejected because it asserts a measured-clean result the marker cannot
        // support. Two different worlds produce this exact state -- the scrape captured
        // councillors AND their declarations and there genuinely were none, or it captured the
        // councillor list and never captured declarations -- and `enrichment_runs` records only
        // that the adapter ran, not which. Asserting `Clear` here would claim the first.
        //
        // `NotApplicable` states what is actually known: N councillors were evaluated, none of
        // them is structurally able to yield a declaration edge in this graph, and the reason is
        // named. Operator decision 2026-08-27 (see phase 33 design decision D2's amendment) -- a
        // pre-declared verdict correction for Phase 36's differential (bucket b), in the same
        // class as ROADMAP criterion 4's `links-not-elevated`.
        if declares.value == 0 {
            let summary = format!(
                "{} present but 0 DeclaresInterest edges -- nepotism detector cannot fire.",
                super::plural(pa.value, "PoliticalActor node", "PoliticalActor nodes")
            );
            // Python analog: directs_count == 0 -> WEAK/MEDIUM (analyst_probe.py:2101), severity
            // translated 1:1 even though this branch's Verdict is NotApplicable, not Weak (D2's
            // amendment, see the comment above).
            return Finding {
                probe: self.name().to_string(),
                // `Clear` is structurally unreachable for this probe. The `pa.value == 0`
                // guard above returns `DataGap`, so `pa.value > 0` here, making
                // `capable (0) < population (pa.value)` -- which `classify` maps
                // unconditionally to `NotApplicable`. This corrects the phase 39 research notes' VRD-12
                // inventory, which claimed `Clear` is reachable at this branch when
                // `declares.value == 0`. Proven by
                // `marker_present_pa_nonzero_declares_zero_is_not_applicable`.
                verdict: Verdict::classify(
                    pa.value,
                    0,
                    0,
                    1,
                    "Council Registers ran and produced PoliticalActor nodes, but no \
                     DeclaresInterest edge exists for any of them -- declaration capture cannot \
                     be distinguished from a genuine absence of declarations",
                ),
                summary,
                cause: None,
                severity: Severity::Medium,
                evidence,
                attributions: attributions.clone(),
                remediation_hint: None,
            };
        }

        let summary = format!(
            "{} and {} -- nepotism bridge detector inputs active.",
            super::plural(pa.value, "PoliticalActor node", "PoliticalActor nodes"),
            super::plural(
                declares.value,
                "DeclaresInterest edge",
                "DeclaresInterest edges"
            )
        );
        // Python analog: else branch -> HIT/HIGH (analyst_probe.py:2103-2105).
        Finding {
            probe: self.name().to_string(),
            verdict: Verdict::classify(pa.value, pa.value, declares.value, 1, ""),
            summary,
            cause: None,
            severity: Severity::High,
            evidence,
            attributions,
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use urban_integrity_schema::types::edge::{
        DeclaresInterestData, DirectsData, EdgeMeta, EdgeWeight,
    };
    use urban_integrity_schema::types::node::{NodeWeight, PoliticalActorData};

    use super::*;

    fn edge_meta() -> EdgeMeta {
        EdgeMeta {
            created_at: Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

    async fn seed_node(db: &Surreal<Any>, event_id: &str, entity_id: &str, weight: NodeWeight) {
        let weight_json = serde_json::to_string(&weight).expect("serialize node weight");
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeAdded",
                    "payload": {
                        "entity_id": entity_id,
                        "weight_json": weight_json,
                    },
                }),
            ))
            .await
            .expect("send seed node CREATE")
            .check()
            .expect("seed node CREATE check");
    }

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
            .expect("send seed edge CREATE")
            .check()
            .expect("seed edge CREATE check");
    }

    async fn seed_council_political_actor(db: &Surreal<Any>, event_id: &str, entity_id: &str) {
        seed_node(
            db,
            event_id,
            entity_id,
            NodeWeight::PoliticalActor(PoliticalActorData {
                name: format!("Cllr {entity_id}"),
                party: None,
                council_name: Some("Anglesey Council".to_string()),
            }),
        )
        .await;
    }

    async fn seed_ec_political_actor(db: &Surreal<Any>, event_id: &str, entity_id: &str) {
        seed_node(
            db,
            event_id,
            entity_id,
            NodeWeight::PoliticalActor(PoliticalActorData {
                name: format!("Recipient {entity_id}"),
                party: Some("Labour Party".to_string()),
                council_name: None,
            }),
        )
        .await;
    }

    async fn seed_declares_interest(db: &Surreal<Any>, event_id: &str, src: &str, dst: &str) {
        seed_edge(
            db,
            event_id,
            src,
            dst,
            EdgeWeight::DeclaresInterest {
                meta: edge_meta(),
                data: DeclaresInterestData {
                    council: Some("Anglesey".to_string()),
                    category: None,
                },
            },
        )
        .await;
    }

    /// Seeded using the SAME shape `planning_identity.rs` produces (`entity`/`officer` source) --
    /// `EdgeWeight::Directs` is schema-rejected from a `political_actor` source
    /// (the POL-02 directs-relate-rejected investigation notes, cited by D2), so a genuinely
    /// council-registers-sourced Directs edge cannot exist; this fixture proves the probe
    /// excludes Directs regardless of provenance, using the only schema-legal producer shape.
    async fn seed_directs(db: &Surreal<Any>, event_id: &str, src: &str, dst: &str) {
        seed_edge(
            db,
            event_id,
            src,
            dst,
            EdgeWeight::Directs {
                meta: edge_meta(),
                data: DirectsData {
                    details: String::new(),
                    officer_role: None,
                },
            },
        )
        .await;
    }

    async fn seed_enrichment_run(db: &Surreal<Any>, source: &str) {
        db.query(format!(
            "UPSERT enrichment_runs:{source} SET source = '{source}', last_run_at = $last_run_at, \
             candidates_found = $candidates_found, auto_linked = $auto_linked, \
             pending_review = $pending_review, discarded = $discarded"
        ))
        .bind(("last_run_at", 1_756_000_000i64))
        .bind(("candidates_found", 7i64))
        .bind(("auto_linked", 2i64))
        .bind(("pending_review", 1i64))
        .bind(("discarded", 4i64))
        .await
        .expect("send seed enrichment_runs UPSERT")
        .check()
        .expect("seed enrichment_runs UPSERT check");
    }

    fn skip() -> bool {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: council_registers_political_actors (set URBAN_DB_TESTS=1)");
            return true;
        }
        false
    }

    /// ROADMAP criterion 1, marker -- the DIRECT reproduction-and-fix of the original VRD-08
    /// defect: no `enrichment_runs:council_registers` marker, but a graph seeded with BOTH a
    /// planning_identity-shaped Directs edge AND a council_name-bearing PoliticalActor node.
    /// The original Python probe reported `HIT/HIGH` here; the marker gate must win regardless.
    #[tokio::test]
    async fn no_marker_with_both_proxy_shapes_seeded_is_data_gap() {
        if skip() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_council_pol_actors",
            "no_marker_proxy_shapes",
        )
        .await;
        // Deliberately NO seed_enrichment_run call.
        seed_directs(&db, "edge-directs-1", "officer:jane-smith", "entity:e1").await;
        seed_council_political_actor(&db, "node-pa-1", "political_actor:cllr1").await;

        let finding = CouncilRegistersPoliticalActors
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap (marker gate must win over both proxy shapes) -- this is the exact \
             VRD-08 scenario -- got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn marker_present_pa_count_zero_is_data_gap() {
        if skip() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_council_pol_actors",
            "marker_pa_zero",
        )
        .await;
        seed_enrichment_run(&db, "council_registers").await;

        let finding = CouncilRegistersPoliticalActors
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "expected DataGap, got {:?}",
            finding.verdict
        );
    }

    /// ROADMAP criterion 1, the `declares_count == 0` branch.
    ///
    /// Plan 33-08 specifies `Weak` here, mirroring Python's
    /// `elif directs_count == 0: WEAK` (`analyst_probe.py:2101`). That mandate is unsatisfiable
    /// under this milestone's own verdict model: `Verdict::classify` yields `Weak` only when
    /// `0 < matches < hit_threshold`, so at `matches = 0` the only way to reach it is to
    /// fabricate a match count the probe never measured. `Coverage`'s fields are private and
    /// there is no `Verdict::weak()`, so no sanctioned path expresses the plan's wording.
    ///
    /// Resolved by operator decision 2026-08-27 as `NotApplicable` -- see this probe's `run()`
    /// for why `Clear` was rejected, and phase 33 design decision D2's amendment for the record. This test
    /// was `#[ignore]`d from 2026-08-26 until that decision; it now asserts the agreed encoding,
    /// including `excluded() == population`, so a silent drift back to `Clear` (which would
    /// claim a measured-clean result the `enrichment_runs` marker cannot support) fails here.
    #[tokio::test]
    async fn marker_present_pa_nonzero_declares_zero_is_not_applicable() {
        if skip() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_council_pol_actors",
            "marker_pa_nonzero_declares_zero",
        )
        .await;
        seed_enrichment_run(&db, "council_registers").await;
        seed_council_political_actor(&db, "node-pa-1", "political_actor:cllr1").await;

        let finding = CouncilRegistersPoliticalActors
            .run(&db, &mut Strategy::default())
            .await;
        let Verdict::NotApplicable { coverage, reason } = &finding.verdict else {
            panic!("expected NotApplicable, got {:?}", finding.verdict);
        };
        assert_eq!(
            coverage.population(),
            1,
            "the one seeded council-attributed PoliticalActor node must be reported as the \
             evaluated population, not discarded"
        );
        assert_eq!(
            coverage.capable(),
            0,
            "no councillor can yield a DeclaresInterest edge in this graph"
        );
        assert_eq!(
            coverage.excluded(),
            1,
            "VRD-03 requires NotApplicable to report the excluded population"
        );
        assert!(
            reason.contains("DeclaresInterest"),
            "the exclusion reason must name the missing evidence, got {reason:?}"
        );
        assert!(
            !matches!(finding.verdict, Verdict::Clear(_)),
            "phase20-council-registers must never reach Clear -- declares.value==0 is NotApplicable (capable=0 < population=pa.value), see this file's own doc comment above the classify call site"
        );
    }

    /// The `declares_count > 0` branch: both counts non-zero -> `Hit`.
    ///
    /// Was blocked until 2026-08-26 by `attributed.rs`'s `inner_data()` returning `None` for
    /// every internally-tagged `NodeWeight`, which pinned `pa.value` at 0 and made this branch
    /// unreachable. That helper now falls back to the whole weight, so this runs for real.
    #[tokio::test]
    async fn marker_present_pa_and_declares_nonzero_is_hit_high() {
        if skip() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_council_pol_actors",
            "marker_pa_and_declares_nonzero",
        )
        .await;
        seed_enrichment_run(&db, "council_registers").await;
        seed_council_political_actor(&db, "node-pa-1", "political_actor:cllr1").await;
        seed_declares_interest(&db, "edge-declares-1", "political_actor:cllr1", "entity:e1").await;

        let finding = CouncilRegistersPoliticalActors
            .run(&db, &mut Strategy::default())
            .await;
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "expected Hit, got {:?}",
            finding.verdict
        );
        assert!(
            !matches!(finding.verdict, Verdict::Clear(_)),
            "phase20-council-registers must never reach Clear -- the else branch is unconditionally Hit (matches=declares.value>0>=hit_threshold=1)"
        );
    }

    /// ROADMAP criterion 1, edges: 5 Directs + 2 DeclaresInterest seeded -- the reported edge
    /// count must equal 2 (DeclaresInterest only), never 7.
    #[tokio::test]
    async fn reported_edge_count_excludes_directs_entirely() {
        if skip() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_council_pol_actors",
            "edges_exclude_directs",
        )
        .await;
        seed_enrichment_run(&db, "council_registers").await;
        seed_council_political_actor(&db, "node-pa-1", "political_actor:cllr1").await;
        for i in 0..5 {
            seed_directs(
                &db,
                &format!("edge-directs-{i}"),
                &format!("officer:o{i}"),
                &format!("entity:e{i}"),
            )
            .await;
        }
        for i in 0..2 {
            seed_declares_interest(
                &db,
                &format!("edge-declares-{i}"),
                "political_actor:cllr1",
                &format!("entity:d{i}"),
            )
            .await;
        }

        let finding = CouncilRegistersPoliticalActors
            .run(&db, &mut Strategy::default())
            .await;
        // The per-count roll-up moved from `summary` prose into typed `evidence` rows when
        // 35-01 widened `Finding` -- assert on the evidence row, not the summary.
        let declares_line = finding
            .evidence
            .iter()
            .map(|row| row.text.as_str())
            .find(|t| t.contains("DeclaresInterest edges (councillor -> company)"))
            .unwrap_or("");
        assert!(
            declares_line.trim_start().starts_with("2 "),
            "expected the reported edge count to be exactly 2, got line: {declares_line:?}, \
             full evidence: {:?}",
            finding.evidence
        );
    }

    /// ROADMAP criterion 1, nodes: 3 council-shaped + 4 EC-shaped PoliticalActor nodes seeded --
    /// the reported node count must equal 3 (council_registers-attributed only), never 7.
    ///
    /// The single most load-bearing test in this plan: it is the direct proof of ROADMAP
    /// criterion 1's "nodes" sub-criterion, D2's namesake node-attribution-leak fix.
    ///
    /// Was blocked and `#[ignore]`d until 2026-08-26 by `attributed.rs`'s `inner_data()`, which
    /// returned `None` for every internally-tagged `NodeWeight` and so pinned `pa.value` at 0
    /// for council-shaped and EC-shaped rows alike -- the discriminator was never invoked, and
    /// the count was 0 for the right answer and the wrong one equally. `inner_data()` now falls
    /// back to the whole weight, so this passes through the sanctioned `Attributed` routing
    /// path against real data rather than being asserted from theory.
    #[tokio::test]
    async fn reported_node_count_excludes_electoral_commission_shaped_rows() {
        if skip() {
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_council_pol_actors",
            "nodes_exclude_ec",
        )
        .await;
        seed_enrichment_run(&db, "council_registers").await;
        for i in 0..3 {
            seed_council_political_actor(
                &db,
                &format!("node-council-{i}"),
                &format!("political_actor:council{i}"),
            )
            .await;
        }
        for i in 0..4 {
            seed_ec_political_actor(
                &db,
                &format!("node-ec-{i}"),
                &format!("political_actor:ec{i}"),
            )
            .await;
        }

        let finding = CouncilRegistersPoliticalActors
            .run(&db, &mut Strategy::default())
            .await;
        // Precise assertion on the ACTUAL `pa.value` evidence row (the roll-up moved out of
        // `summary` when 35-01 widened `Finding`), not a loose substring that could accidentally
        // match the unrelated with_council row -- both can contain the digit "3".
        let pa_count_line = finding
            .evidence
            .iter()
            .map(|row| row.text.as_str())
            .find(|t| t.contains("PoliticalActor nodes in graph (council_registers-attributed)"))
            .unwrap_or("");
        assert!(
            !pa_count_line.is_empty(),
            "expected the pa_count evidence row to be present, got: {:?}",
            finding.evidence
        );
        assert!(
            pa_count_line.trim_start().starts_with("3 "),
            "expected the reported node count to be exactly 3 (council_registers-shaped only), \
             got row: {pa_count_line:?}, full evidence: {:?}",
            finding.evidence
        );
    }
}
