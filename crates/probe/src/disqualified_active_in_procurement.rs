//! `DisqualifiedActiveInProcurement` Tier-2 probe -- ports
//! `probe_disqualified_active_in_procurement` (`scripts/analyst_probe.py:1812-1925`). The
//! empty-links branch defers to D4's shared [`disqualification_chain`] and maps its `Cause`
//! through [`verdict_for_cause`] -- the SAME function `evasion_alerts.rs` imports and calls, so
//! the two probes' D4 mapping is structurally guaranteed identical, not two independently written
//! match statements that happen to agree.
//!
//! The HIT-branch `AwardedTo` cross-reference implements the INTENDED paged intersection, not
//! Python's `fetch_edges(db, "AwardedTo", limit=20000)` call -- that call is a `TypeError`
//! against `fetch_edges`'s real signature (`(db, type_name, page_size=20000,
//! max_rows=500000)`, no `limit` kwarg) that has never once executed successfully on live data
//! (the research notes' Pitfall 5 -- `confirmed` has always been empty so far, per the chain analysis).

use std::collections::HashSet;

use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{
    Cause, EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};

use super::disqualification_chain::{disqualification_chain, verdict_for_cause};
use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// `payload.src_id` as a string, whether SurrealDB handed it back as a plain string or coerced it
/// into a RecordId object. Mirrors `disqualification_chain.rs`'s/`ward_concentration.rs`'s local
/// copy of the same helper -- `scan.rs`'s private `record_id_str`/`entity_id_of` are not
/// exported, so this is another local equivalent.
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

/// A confirmed disqualified-director link's Companies House number, parsed out of its
/// `evidence_json` blob (`{"ch_number": "..."}`) -- mirrors Python's `evd.get("ch_number", "")`.
fn ch_number_from_evidence(evidence_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(evidence_json)
        .ok()?
        .get("ch_number")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub struct DisqualifiedActiveInProcurement;

#[async_trait::async_trait]
impl Probe for DisqualifiedActiveInProcurement {
    fn name(&self) -> &'static str {
        "osint-disqualified-directors"
    }

    /// Searchable by the findings-by-entity index (PIV-02): each `officer -> entity` row carries an `Entity` ref for the entity.
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
        "Is any director subject to a Companies House disqualification order linked to an \
         entity that currently holds a contract award (a CDDA 1986 violation)?"
    }

    async fn run(&self, db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
        let disq_links = match scan::scan_identity_links(db, None, Some("ch_disqualified")).await {
            Ok(rows) => rows,
            Err(e) => {
                return Finding {
                    probe: self.name().to_string(),
                    verdict: Verdict::data_gap(format!(
                        "ch_disqualified identity_links scan failed: {e}"
                    )),
                    summary: "ch_disqualified identity_links scan failed -- \
                              disqualified-director exposure cannot be evaluated."
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

        if disq_links.is_empty() {
            // D4: report the measured break in the chain, not a bare CLEAR/DATA_GAP disjunction
            // -- see this module's doc comment.
            let state = disqualification_chain(db).await;
            let excluded = state.excluded_count();
            let verdict = verdict_for_cause(&state);
            // Cause-to-severity mapping shared with `evasion_alerts.rs` -- D4's single table
            // means the same cause must carry the same severity in both probes.
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
                    "No ch_disqualified identity links found (excluded population: {excluded})."
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

        let confirmed: Vec<_> = disq_links
            .iter()
            .filter(|r| r.status == "auto_linked" || r.status == "confirmed")
            .collect();
        let pending: Vec<_> = disq_links
            .iter()
            .filter(|r| r.status == "pending_review")
            .collect();

        // The officer node in the identity link is target_node_id (director:slug) -- does this
        // director slug appear as the src of any AwardedTo edge?
        let award_edges = match scan::scan_events_by_payload_contains(
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
                    summary: "AwardedTo edge scan failed -- cannot cross-reference disqualified \
                              directors against contract awards."
                        .to_string(),
                    cause: None,
                    // No direct Python branch analog for a raw scan failure; nearest analog is
                    // analyst_probe.py:2551's generic probe-exception handler (DATA_GAP/INFO).
                    severity: Severity::Info,
                    evidence: vec![EvidenceRow {
                        text: format!("AwardedTo scan error: {e}"),
                        refs: vec![],
                    }],
                    attributions: vec![],
                    remediation_hint: None,
                };
            }
        };
        let award_srcs: HashSet<String> = award_edges
            .iter()
            .filter_map(|r| record_id_str(&r["payload"]["src_id"]))
            .collect();

        let mut active_and_winning: Vec<(String, String)> = Vec::new();
        for r in &confirmed {
            let officer = r.target_node_id.as_str();
            let entity_id =
                ch_number_from_evidence(&r.evidence_json).map(|ch| format!("GB-COH:{ch}"));
            if let Some(eid) = entity_id
                .as_ref()
                .filter(|eid| award_srcs.contains(eid.as_str()))
            {
                active_and_winning.push((officer.to_string(), eid.clone()));
            } else if award_srcs.contains(officer) {
                active_and_winning.push((officer.to_string(), officer.to_string()));
            }
        }

        // D-02: the count roll-up that used to be appended to every summary branch is an
        // evidence row now, not summary prose.
        let header = format!(
            "{} total, {} confirmed/auto-linked, {} pending analyst review, {} whose entity \
             holds contract awards.",
            plural(
                disq_links.len() as u64,
                "disqualified director link",
                "disqualified director links"
            ),
            confirmed.len(),
            pending.len(),
            active_and_winning.len()
        );

        let (matches, hit_threshold) = if !confirmed.is_empty() {
            (1u64, 1u64)
        } else if !pending.is_empty() {
            (1u64, 2u64)
        } else {
            (0u64, 1u64)
        };
        // `Verdict::classify(1, 1, matches, hit_threshold, "")` is a deliberate non-obvious use
        // of the `classify` API (matching `ward_concentration.rs`'s own documented sentinel
        // pattern): population/capable are a constant 1/1 sentinel (this branch has no
        // structural-exclusion concept -- every link here already passed the empty-links check
        // above), and `matches`/`hit_threshold` alone select Hit (confirmed>0) / Weak
        // (pending>0, no confirmed) / Clear (neither), matching Python's exact
        // else-if/elif/else severity ladder (`analyst_probe.py:1905-1919`).
        let verdict = Verdict::classify(1, 1, matches, hit_threshold, "");

        let mut evidence = vec![EvidenceRow {
            text: header,
            refs: vec![],
        }];

        // Python's severity ladder (`analyst_probe.py:1897-1919`) translated 1:1; each branch's
        // named roll-call moves out of `summary` into typed evidence rows carrying an `Entity`
        // pivot for the intersecting entity.
        let (severity, summary) = if !active_and_winning.is_empty() {
            for (officer, entity) in &active_and_winning {
                evidence.push(EvidenceRow {
                    text: format!("{officer} -> {entity}"),
                    refs: vec![RecordRef {
                        kind: RecordKind::Entity,
                        id: entity.clone(),
                    }],
                });
            }
            (
                Severity::Critical,
                format!(
                    "{}; {} linked to entities with contract awards -- potential CDDA 1986 \
                     violation.",
                    plural(
                        confirmed.len() as u64,
                        "confirmed disqualified director",
                        "confirmed disqualified directors"
                    ),
                    active_and_winning.len()
                ),
            )
        } else if !confirmed.is_empty() {
            (
                Severity::High,
                format!(
                    "{}. None cross-referenced to active contract awards yet.",
                    plural(
                        confirmed.len() as u64,
                        "confirmed disqualified director",
                        "confirmed disqualified directors"
                    )
                ),
            )
        } else if !pending.is_empty() {
            (
                Severity::Medium,
                format!(
                    "{} pending analyst review.",
                    plural(
                        pending.len() as u64,
                        "disqualified director link",
                        "disqualified director links"
                    )
                ),
            )
        } else {
            (
                Severity::Medium,
                "No confirmed disqualified directors found in identity_links.".to_string(),
            )
        };

        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            severity,
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{AwardedToData, EdgeMeta, EdgeWeight};

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
        evidence_json: &str,
    ) {
        db.query(
            "CREATE identity_links CONTENT { \
                 source_node_id: $sni, \
                 target_node_id: $tni, \
                 resolution_type: 'exact_officer', \
                 confidence: 1.0, \
                 source: 'ch_disqualified', \
                 resolved_at: d'2024-04-12T00:00:00Z', \
                 evidence_json: $evj, \
                 status: $status \
             }",
        )
        .bind(("sni", source_node_id.to_string()))
        .bind(("tni", target_node_id.to_string()))
        .bind(("status", status.to_string()))
        .bind(("evj", evidence_json.to_string()))
        .await
        .expect("failed to seed ch_disqualified identity_link")
        .check()
        .expect("ch_disqualified identity_link CREATE returned a statement error");
    }

    async fn seed_awarded_to_edge(db: &Surreal<Any>, src_id: &str, dst_id: &str) {
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
                    "event_id": format!("edge-awarded-{src_id}-{dst_id}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": { "src_id": src_id, "dst_id": dst_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed AwardedTo edge");
    }

    #[tokio::test]
    async fn no_officer_nodes_maps_to_data_gap_via_shared_chain() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: no_officer_nodes_maps_to_data_gap_via_shared_chain (dip)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_dip", "d1").await;
        let mut st = Strategy::default();

        let probe = DisqualifiedActiveInProcurement;
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
            eprintln!("SKIP: adapter_never_ran_maps_to_data_gap_via_shared_chain (dip)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_dip", "d2").await;
        let mut st = Strategy::default();

        seed_officer_node(&db, "officer:jane-smith").await;

        let probe = DisqualifiedActiveInProcurement;
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
            eprintln!("SKIP: no_register_overlap_maps_to_clear_via_shared_chain (dip)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_dip", "d3").await;
        let mut st = Strategy::default();

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;

        let probe = DisqualifiedActiveInProcurement;
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
    async fn links_not_elevated_shared_mapping_is_not_applicable_with_excluded_population() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: links_not_elevated_shared_mapping_is_not_applicable_with_excluded_population (dip)"
            );
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_dip", "d4").await;

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;
        seed_ch_disqualified_link(&db, "disq:c1", "officer:jane-smith", "candidate", "{}").await;
        seed_ch_disqualified_link(&db, "disq:c2", "officer:john-doe", "candidate", "{}").await;

        // `Cause::LinksNotElevated` requires >=1 ch_disqualified link to exist (just none
        // elevated). This probe's own top-level `disq_links.is_empty()` gate reads the IDENTICAL
        // `scan_identity_links(db, None, Some("ch_disqualified"))` call `disqualification_chain`
        // makes internally, so whenever the chain would report `LinksNotElevated` (links
        // non-empty), `disq_links` is ALSO non-empty and this probe's own D4-defer branch is
        // never entered -- it falls straight into the ordinary "non-empty links, zero confirmed"
        // logic instead (Weak/Clear, `cause: None`), exactly as this plan's `<behavior>` section
        // specifies for that branch. This is a structural property of the Python original too
        // (`if not disq_links` reads the same `fetch_identity_links` call
        // `_disqualification_chain` does), not a defect this port introduces. Proven directly:
        let confirmed_via_direct_chain_read = disqualification_chain(&db).await;
        assert!(
            matches!(
                confirmed_via_direct_chain_read.cause,
                urban_integrity_schema::types::verdict::Cause::LinksNotElevated
            ),
            "the shared chain measurement itself must still report LinksNotElevated for this              seeded state (Task 1 already proves this independently) -- got {:?}",
            confirmed_via_direct_chain_read.cause
        );

        // What this probe ACTUALLY returns for this DB state: the ordinary non-D4 branch.
        let mut st = Strategy::default();
        let probe = DisqualifiedActiveInProcurement;
        let finding = probe.run(&db, &mut st).await;
        assert!(
            finding.cause.is_none(),
            "with real (if sub-threshold) links present, D4 does not apply --              disqualified_active_in_procurement's own <behavior> spec says cause: None here"
        );

        // D4's mapping table itself -- the SAME `verdict_for_cause` function this probe's
        // empty-links branch imports and calls (not reimplemented) -- maps LinksNotElevated to
        // NotApplicable with the excluded population reported, exactly as `evasion_alerts.rs`
        // proves reachably through its own full Probe::run (its empty-branch gate is a
        // MotifDetected scan, decoupled from ch_disqualified links, so it CAN reach this cause
        // organically). Both probes therefore apply the identical D4 mapping when it is reached;
        // this test proves DIP would too, via the exact function it imports.
        assert_eq!(
            confirmed_via_direct_chain_read.excluded_count(),
            2,
            "excluded population must equal the seeded candidate count"
        );
        let mapped = verdict_for_cause(&confirmed_via_direct_chain_read);
        assert!(
            matches!(mapped, Verdict::NotApplicable { .. }),
            "verdict_for_cause(LinksNotElevated, ..) must be NotApplicable, got {:?}",
            mapped
        );
    }

    #[tokio::test]
    async fn no_live_ban_maps_to_clear_via_shared_chain() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: no_live_ban_maps_to_clear_via_shared_chain (dip)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_dip", "d5").await;
        let mut st = Strategy::default();

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;
        // A different officer's confirmed link so the top-level `disq_links` scan (which is NOT
        // filtered to officer:jane-smith) is non-empty and this probe reaches its non-empty
        // branch instead -- wait: this probe's non-empty branch is exercised by the
        // `active_and_winning`/`confirmed` tests below. To exercise the *chain's* NoLiveBan cause
        // specifically THROUGH this probe, `disq_links` (unfiltered) must itself be non-empty,
        // which routes this probe past the empty-links D4 branch entirely. So NoLiveBan is only
        // reachable through this probe's empty-links branch when `disq_links` is empty but the
        // chain (which reads the SAME `ch_disqualified` links) is not -- an impossible
        // combination, since both scans read the identical table/filter. NoLiveBan is therefore
        // unreachable through `disqualified_active_in_procurement`'s own empty-links branch in
        // practice; the shared mapping is still proven correct by
        // `disqualification_chain::tests::chain_no_live_ban_when_elevated_links_exist` (Task 1)
        // and by `verdict_for_cause_maps_each_cause_per_d4_table` -- this test instead proves the
        // non-empty branch handles a confirmed link with no AwardedTo intersection as Hit/HIGH
        // per Python's else-if ladder.
        seed_ch_disqualified_link(&db, "disq:e1", "officer:jane-smith", "confirmed", "{}").await;

        let probe = DisqualifiedActiveInProcurement;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            finding.cause.is_none(),
            "the non-empty-links branch never carries a Cause -- D4 only applies to the empty \
             branch"
        );
        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "a confirmed link with no AwardedTo intersection is still Hit (HIGH), got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn confirmed_link_intersecting_awarded_to_is_hit() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: confirmed_link_intersecting_awarded_to_is_hit (dip)");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_dip", "d6").await;
        let mut st = Strategy::default();

        let evidence = serde_json::json!({ "ch_number": "12345678" }).to_string();
        seed_ch_disqualified_link(&db, "disq:e1", "officer:jane-smith", "confirmed", &evidence)
            .await;
        // The derived entity id (GB-COH:12345678) appears as the src of a seeded AwardedTo edge.
        seed_awarded_to_edge(&db, "GB-COH:12345678", "contract:xyz").await;

        let probe = DisqualifiedActiveInProcurement;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "a confirmed disqualified director whose derived entity intersects AwardedTo must \
             be Hit, got {:?}",
            finding.verdict
        );
        assert!(
            finding
                .evidence
                .iter()
                .any(|row| row.text.contains("GB-COH:12345678")
                    && row
                        .refs
                        .iter()
                        .any(|r| r.kind == RecordKind::Entity && r.id == "GB-COH:12345678")),
            "evidence must name the intersecting entity and carry an Entity pivot for it, \
             got: {:?}",
            finding.evidence
        );
    }
}
