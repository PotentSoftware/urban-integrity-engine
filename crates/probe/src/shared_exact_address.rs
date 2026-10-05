//! `SharedExactAddress` Tier-1 probe (`Finding.probe = "address-cluster-exact"`, NOT the Python
//! function name) — ports `probe_shared_exact_address` (`scripts/analyst_probe.py:984-1026`).
//!
//! Three fixes over the Python original:
//!
//! 1. **The `Strategy.identity_blocked` read (ROADMAP success criterion 5).** When
//!    `identity-integrity` (32-03's other probe, dispatched earlier per `GRAPH_PROBES` order)
//!    has set `st.identity_blocked = true`, this probe cannot trust its own
//!    distinct-applicants-per-address count and must report `Blocked` — checked BEFORE the
//!    `Clear` fallthrough, exactly mirroring Python's `if not shared and st.identity_blocked`
//!    (`:1000`).
//! 2. **The missing denominator (the other half of VRD-06's hazard, ROADMAP success criterion
//!    4).** Python's own evidence states neither branch's population — `{len(shared)}
//!    address(es) shared...` and `"No exact address shared..."` both omit how many locations
//!    were actually evaluated. This port always states `population` (distinct locations with
//!    ≥1 applicant) in both the `Hit` and `Clear` evidence strings.
//! 3. **No `Clear` on a population of zero (added 2026-08-23, post-Phase-32 review).** Python
//!    falls through to its no-sharing message whether it evaluated ten thousand locations or
//!    none, and the initial port inherited that: `Verdict::classify(0, 0, …)` satisfies
//!    `capable == population` and so produces a well-formed `Clear` — a clean bill of health for
//!    a set that was never measured. That is the hazard class VRD-06 exists to eliminate, and the
//!    other three VRD-06 probes (`geographic_concentration.rs:100`, `TemporalBurst`,
//!    `PrecomputedAlerts`) already short-circuited to `DataGap`; this probe was the odd one out.
//!    It now does the same. Note the branch sits AFTER the `Blocked` check, not before — see the
//!    comment at that branch for why.

use std::collections::{HashMap, HashSet};

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan;
use urban_integrity_schema::types::verdict::{
    EvidenceRow, Finding, RecordKind, RecordRef, Severity, Verdict,
};

use super::{plural, EvidenceScope, MotifKind, Probe, Strategy};

/// `LocatedAt`/`AwardedTo` payload's `src_id`/`dst_id` as a string, whether SurrealDB handed it
/// back as a plain string or coerced it into a RecordId object.
///
/// Same mechanism as `persistence::scan`'s private `record_id_str` (`scan.rs:427-439`),
/// reimplemented here since that helper is not exported and this module must not add raw
/// SurrealQL to route around it.
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

/// The missing-denominator hazard fix (VRD-06) and the `Strategy.identity_blocked` cross-probe
/// dependency (success criterion 5) — see module doc comment.
pub struct SharedExactAddress;

#[async_trait::async_trait]
impl Probe for SharedExactAddress {
    fn name(&self) -> &'static str {
        "address-cluster-exact"
    }

    /// Searchable by the findings-by-entity index (PIV-02): each cluster row carries an `Entity` ref for the location and one per applicant.
    ///
    /// Plan 45-05 adds no `RecordRef` here; this only declares what is already emitted.
    fn evidence_scope(&self) -> EvidenceScope {
        EvidenceScope::NamesNodes
    }

    /// A hub-and-spoke cluster fanning out from one shared record (D-06's pinned classification, phase 50 plan 1).
    fn motif_kind(&self) -> MotifKind {
        MotifKind::Star
    }

    fn tier(&self) -> &'static str {
        "Tier-1"
    }

    fn hypothesis(&self) -> &'static str {
        "Do 2 or more distinct planning applicants share exactly the same registered address \
         (excluding buyers/formation agents), indicating a possible shell-company ring?"
    }

    // D-14's `depends_on() -> ["identity-integrity"]` was REMOVED on 2026-09-03. It was both
    // unsatisfiable and semantically inverted, and between them they made this probe permanently
    // unreachable under the default gated dispatch.
    //
    // Unsatisfiable: D-07 gates a dependent on its upstream returning Hit or Weak, and
    // `identity-integrity` cannot return either. Its four verdict paths are three `data_gap`s and
    // one `Verdict::classify(total, total, 0, u64::MAX, "")` — `matches` hardcoded to 0 against a
    // `u64::MAX` threshold, as its own comment says ("Python never returns HIT/WEAK for this
    // probe"). So the gate could never open, on any corpus. This was not corpus-dependent bad
    // luck; it was structural.
    //
    // Inverted: `identity-integrity` asks whether applicant-provenance coverage is ADEQUATE. A
    // `Clear` there means the precondition this probe needs is SATISFIED. Gating on Hit/Weak
    // therefore asked to run this probe only when the identity data was bad, and to skip it when
    // the data was good — backwards for a data-quality precondition.
    //
    // The correct mechanism for that precondition already existed and is untouched: the
    // `st.identity_blocked` read below, which fires only when this probe DOES run and tells it not
    // to trust a zero shared-address count. The doc comment there always said the two mechanisms
    // answer different questions and must not be merged. It was right; the edge was the wrong one
    // of the two.
    //
    // Measured cost of the bug: on `an_anglesey_aebaa785` (977,302 graph_events) the default
    // gated run suppressed this probe entirely, while `--full-battery` returns it as a HIT with
    // 28,282 addresses shared by 2+ distinct applicants — the shell-company-ring motif. See
    // the phase 41 plan 07 summary (verdict-gated probe orchestration).

    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
        let located_rows =
            match scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"LocatedAt\"").await {
                Ok(rows) => rows,
                Err(e) => {
                    // A scan failure is a DataGap Finding, not a propagated Result —
                    // Probe::run is infallible by design (PRB-08).
                    return Finding {
                        probe: self.name().to_string(),
                        verdict: Verdict::data_gap(format!("LocatedAt scan failed: {e}")),
                        summary:
                            "LocatedAt scan failed — shared-address clustering cannot be evaluated."
                                .to_string(),
                        cause: None,
                        // No Python analog (Python has no scan-failure branch here) — defaulted
                        // per 35-01 convention: an analog-less DataGap/Blocked branch is Medium.
                        severity: Severity::Medium,
                        evidence: vec![EvidenceRow {
                            text: format!("LocatedAt scan error: {e}"),
                            refs: vec![],
                        }],
                        attributions: vec![],
                        remediation_hint: None,
                    };
                }
            };
        let awarded_rows =
            match scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"AwardedTo\"").await {
                Ok(rows) => rows,
                Err(e) => {
                    return Finding {
                        probe: self.name().to_string(),
                        verdict: Verdict::data_gap(format!("AwardedTo scan failed: {e}")),
                        summary:
                            "AwardedTo scan failed — shared-address clustering cannot be evaluated."
                                .to_string(),
                        cause: None,
                        // No Python analog (Python has no scan-failure branch here) — defaulted
                        // per 35-01 convention: an analog-less DataGap/Blocked branch is Medium.
                        severity: Severity::Medium,
                        evidence: vec![EvidenceRow {
                            text: format!("AwardedTo scan error: {e}"),
                            refs: vec![],
                        }],
                        attributions: vec![],
                        remediation_hint: None,
                    };
                }
            };

        // post-D-04: a planning Contract's LocatedAt edge is sourced from the Contract node,
        // not the applicant Entity, so LocatedAt.src can be a contract id rather than a
        // director/entity id. Resolve it back to the applicant via AwardedTo (src: applicant
        // Entity, dst: Contract) before grouping, so this probe keeps counting distinct
        // APPLICANTS rather than silently counting distinct APPLICATIONS under an unchanged
        // label (`analyst_probe.py:987-993`).
        let mut contract_to_applicant: HashMap<String, String> = HashMap::new();
        for row in &awarded_rows {
            let src = record_id_str(&row["payload"]["src_id"]);
            let dst = record_id_str(&row["payload"]["dst_id"]);
            if let (Some(src), Some(dst)) = (src, dst) {
                contract_to_applicant.insert(dst, src);
            }
        }

        let mut by_loc: HashMap<String, HashSet<String>> = HashMap::new();
        for row in &located_rows {
            let src = record_id_str(&row["payload"]["src_id"]);
            let dst = record_id_str(&row["payload"]["dst_id"]);
            if let (Some(src), Some(dst)) = (src, dst) {
                // Python: `contract_to_applicant.get(e["src"], e["src"])` — fall back to the
                // raw src when no AwardedTo match exists.
                let applicant = contract_to_applicant
                    .get(&src)
                    .cloned()
                    .unwrap_or_else(|| src.clone());
                by_loc.entry(dst).or_default().insert(applicant);
            }
        }

        // The missing denominator (success criterion 4's other half): distinct locations with
        // ≥1 applicant evaluated, stated in every branch's evidence below.
        let population = by_loc.len() as u64;
        let shared: HashMap<&String, &HashSet<String>> =
            by_loc.iter().filter(|(_, apps)| apps.len() >= 2).collect();

        // Branch order matters (success criterion 5): BLOCKED is checked BEFORE the Clear
        // fallthrough, exactly mirroring `analyst_probe.py:1000`'s
        // `if not shared and st.identity_blocked`.
        //
        // D-22 cross-reference: this `st.identity_blocked` read is a separate, narrower signal
        // from `depends_on()` above (see that doc comment). `depends_on()` governs whether this
        // probe's `run()` is invoked AT ALL; this read is consulted only when the probe DOES run,
        // telling it not to trust its own zero-shared-address count. The two mechanisms answer
        // different questions ("does this probe run at all" vs "what should it conclude if it
        // does run") and must not be merged into one.
        if shared.is_empty() && st.identity_blocked {
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::blocked(
                    "identity integrity check failed — cannot evaluate \
                     distinct-applicants-per-address",
                ),
                summary: "Cannot evaluate: applicant identity is broken.".to_string(),
                cause: None,
                // Python: BLOCKED/INFO (`scripts/analyst_probe.py:1000-1006`).
                severity: Severity::Info,
                evidence: vec![EvidenceRow {
                    text: "Applicant identity is broken, so distinct-applicants-per-address is \
                           unmeasurable. Pivoting to geographic clustering."
                        .to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        // Zero population must not reach the Clear fallthrough below. `Verdict::classify(0, 0, …)`
        // satisfies `capable == population` and so yields a well-formed `Clear` — a clean bill of
        // health for a set that was never measured, which is exactly the hazard class this phase
        // exists to eliminate. `GeographicConcentration` (`geographic_concentration.rs:100`),
        // `TemporalBurst` and `PrecomputedAlerts` all short-circuit the same way; this probe was the
        // odd one out until 2026-08-23.
        //
        // Placed AFTER the BLOCKED branch deliberately. On an empty graph both conditions hold at
        // once, and `Blocked` is the more informative answer: it names a broken prerequisite rather
        // than merely absent data. Reversing these two would also regress ROADMAP success criterion
        // 5, whose runtime test (`identity_blocked_propagates_through_the_real_battery_to_address_cluster_exact`)
        // asserts `Blocked` on precisely this empty-database fixture.
        if population == 0 {
            return Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap("no LocatedAt edges with a resolvable applicant"),
                summary: "No locations with \u{2265}1 applicant present — cannot be evaluated."
                    .to_string(),
                cause: None,
                // No Python analog (added 2026-08-23, post-Phase-32 review) — defaulted per
                // 35-01 convention: an analog-less DataGap/Blocked branch is Medium.
                severity: Severity::Medium,
                evidence: vec![EvidenceRow {
                    text: "distinct-applicants-per-address cannot be evaluated: zero locations \
                           with \u{2265}1 applicant."
                        .to_string(),
                    refs: vec![],
                }],
                attributions: vec![],
                remediation_hint: None,
            };
        }

        if !shared.is_empty() {
            let shared_count = shared.len() as u64;
            // `hit_threshold: 1` — deliberately NOT `u64::MAX` like
            // GeographicConcentration/TemporalBurst: Python's own vocabulary for THIS probe has
            // no intermediate WEAK branch (`analyst_probe.py:1009-1020`) — ANY non-empty shared
            // cluster is itself the actionable finding.
            let verdict = Verdict::classify(population, population, shared_count, 1, "");
            let summary = format!(
                "{} shared by 2+ distinct applicants — possible shell-company ring.",
                plural(shared_count, "address", "addresses")
            );
            // The aggregate row keeps its counts and no refs; the per-cluster rows below carry
            // the ids, so "Show on graph" puts one actual cluster on the canvas rather than the
            // union of every cluster this probe found.
            let mut evidence = vec![EvidenceRow {
                text: format!(
                    "{} shared by 2+ distinct applicants, of {} with \u{2265}1 applicant \
                     evaluated.",
                    plural(shared_count, "address", "addresses"),
                    plural(population, "location", "locations")
                ),
                refs: vec![],
            }];
            // Biggest clusters first, ties broken by id so the row order is deterministic.
            const REF_CAP: usize = 20;
            let mut shared_sorted: Vec<(&String, &HashSet<String>)> =
                shared.iter().map(|(l, a)| (*l, *a)).collect();
            shared_sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
            for (location, applicants) in shared_sorted.iter().take(REF_CAP) {
                let mut apps: Vec<&String> = applicants.iter().collect();
                apps.sort();
                let mut refs = vec![RecordRef {
                    kind: RecordKind::Entity,
                    id: (*location).clone(),
                }];
                refs.extend(apps.iter().map(|a| RecordRef {
                    kind: RecordKind::Entity,
                    id: (*a).clone(),
                }));
                evidence.push(EvidenceRow {
                    text: format!("{} applicants at {location}", apps.len()),
                    refs,
                });
            }
            return Finding {
                probe: self.name().to_string(),
                verdict,
                summary,
                cause: None,
                // Python: HIT/HIGH (`scripts/analyst_probe.py:1013-1020`).
                severity: Severity::High,
                evidence,
                attributions: vec![],
                remediation_hint: None,
            };
        }

        let verdict = Verdict::classify(population, population, 0, 1, "");
        let summary = "No exact address shared by multiple distinct applicants.".to_string();
        let evidence = vec![EvidenceRow {
            text: format!("{} evaluated.", plural(population, "location", "locations")),
            refs: vec![],
        }];
        Finding {
            probe: self.name().to_string(),
            verdict,
            summary,
            cause: None,
            // Python: CLEAR/INFO (`scripts/analyst_probe.py:1023-1027`).
            severity: Severity::Info,
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{AwardedToData, EdgeMeta, EdgeWeight, LocatedAtData};

    async fn seed_edge_event(
        db: &Surreal<Any>,
        event_id: &str,
        src_id: &str,
        dst_id: &str,
        weight: &EdgeWeight,
    ) {
        let weight_json = serde_json::to_string(weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": {
                        "src_id": src_id,
                        "dst_id": dst_id,
                        "weight_json": weight_json,
                    },
                }),
            ))
            .await
            .expect("failed to seed edge event");
    }

    fn edge_meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: Some(chrono::Utc::now()),
            effective_to: None,
        }
    }

    fn located_at() -> EdgeWeight {
        EdgeWeight::LocatedAt {
            meta: edge_meta(),
            data: LocatedAtData {
                details: "test".into(),
            },
        }
    }

    fn awarded_to() -> EdgeWeight {
        EdgeWeight::AwardedTo {
            meta: edge_meta(),
            data: AwardedToData {
                details: "test".into(),
            },
        }
    }

    #[test]
    fn record_id_str_passes_through_plain_strings() {
        assert_eq!(
            record_id_str(&serde_json::json!("GB-COH:12345678")).as_deref(),
            Some("GB-COH:12345678")
        );
        assert_eq!(record_id_str(&serde_json::json!("")), None);
    }

    #[test]
    fn record_id_str_reassembles_a_coerced_record_id() {
        let v = serde_json::json!({"tb": "director", "id": {"String": "jane-smith"}});
        assert_eq!(record_id_str(&v).as_deref(), Some("director:jane-smith"));
    }

    #[tokio::test]
    async fn shared_exact_address_blocked_fires_before_clear_when_identity_blocked() {
        // No DB seeding needed — `st.identity_blocked = true` with zero LocatedAt/AwardedTo
        // rows naturally produces `by_loc` empty, so `shared` is empty too, and the BLOCKED
        // branch must fire (success criterion 5's ordering requirement) rather than falling
        // through to Clear. Uses an uninitialised `Surreal::init()` handle exactly like
        // `mod.rs`'s `StubProbeA`/`StubProbeB` pattern (`Probe::run`'s dispatch-order tests) —
        // `scan::scan_events_by_type` here is never reached with a populated table, only with
        // zero rows, which this test proves does not require a live connection to observe: the
        // scan call itself needs *a* connected handle, so this uses the real DB gate, per the
        // plan's own documented fallback for a divergence from the pure-unit-test ideal.
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: shared_exact_address_blocked_fires_before_clear_when_identity_blocked (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy {
            identity_blocked: true,
            ..Default::default()
        };

        let probe = SharedExactAddress;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Blocked { .. }),
            "identity_blocked=true with no shared address must be Blocked, got {:?}",
            finding.verdict
        );
    }

    /// The zero-population fix (2026-08-23). With `identity_blocked` FALSE and no locations at
    /// all, the pre-fix code reached `Verdict::classify(0, 0, 0, 1, "")`, which satisfies
    /// `capable == population` and so returned a well-formed `Clear` — "no address sharing found"
    /// asserted over a set that was never measured. This is the same hazard
    /// `geographic_concentration.rs:100` fixes, and the paired test below pins the branch ORDER so
    /// the fix cannot silently swallow the `Blocked` verdict ROADMAP success criterion 5 depends on.
    #[tokio::test]
    async fn shared_exact_address_zero_population_is_data_gap_not_clear() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: shared_exact_address_zero_population_is_data_gap_not_clear (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        // identity_blocked deliberately FALSE — otherwise the Blocked branch fires first and this
        // test would pass without ever exercising the zero-population branch.
        let mut st = Strategy::default();

        let probe = SharedExactAddress;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::DataGap { .. }),
            "zero locations with an applicant must be DataGap, never a Clear over an unmeasured \
             set, got {:?}",
            finding.verdict
        );
    }

    /// Guards the branch ORDER, not just the branch. On an empty graph BOTH the zero-population
    /// and the `identity_blocked` conditions hold at once; `Blocked` must still win, because it
    /// names a broken prerequisite rather than merely absent data — and because
    /// `identity_blocked_propagates_through_the_real_battery_to_address_cluster_exact`
    /// (`crates/cli/tests/probe_tier1_e2e.rs`) asserts `Blocked` on exactly this fixture. Moving
    /// the DataGap short-circuit above the Blocked check would regress success criterion 5, and
    /// this test fails if anyone does.
    #[tokio::test]
    async fn shared_exact_address_blocked_outranks_zero_population_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: shared_exact_address_blocked_outranks_zero_population_data_gap (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy {
            identity_blocked: true,
            ..Default::default()
        };

        let probe = SharedExactAddress;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Blocked { .. }),
            "on an empty graph both conditions hold; Blocked must outrank the zero-population \
             DataGap, got {:?}",
            finding.verdict
        );
    }

    #[tokio::test]
    async fn shared_exact_address_two_applicants_one_location_is_hit_with_population() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: shared_exact_address_two_applicants_one_location_is_hit_with_population (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // Two Contracts, two distinct applicants, both awarded->located at the SAME Location.
        seed_edge_event(
            &db,
            "awarded-1",
            "GB-COH:applicant-1",
            "planning:contract-1",
            &awarded_to(),
        )
        .await;
        seed_edge_event(
            &db,
            "awarded-2",
            "GB-COH:applicant-2",
            "planning:contract-2",
            &awarded_to(),
        )
        .await;
        seed_edge_event(
            &db,
            "located-1",
            "planning:contract-1",
            "GB-LOC:shared-site",
            &located_at(),
        )
        .await;
        seed_edge_event(
            &db,
            "located-2",
            "planning:contract-2",
            "GB-LOC:shared-site",
            &located_at(),
        )
        .await;
        // A third, unrelated location with a single applicant — must not count as "shared".
        seed_edge_event(
            &db,
            "awarded-3",
            "GB-COH:applicant-3",
            "planning:contract-3",
            &awarded_to(),
        )
        .await;
        seed_edge_event(
            &db,
            "located-3",
            "planning:contract-3",
            "GB-LOC:solo-site",
            &located_at(),
        )
        .await;

        let probe = SharedExactAddress;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Hit { .. }),
            "2 distinct applicants at one location must be Hit, got {:?}",
            finding.verdict
        );
        assert!(
            finding.evidence.iter().any(|e| e.text.contains('1')),
            "evidence must state the shared-address count, got: {:?}",
            finding.evidence
        );
        assert!(
            finding.evidence.iter().any(|e| e.text.contains('2')),
            "evidence must state the total-locations-evaluated population (2), got: {:?}",
            finding.evidence
        );

        // The cluster row must be pivotable: the shared location AND both applicants, so
        // "Show on graph" puts the whole cluster on the canvas rather than one corner of it.
        let cluster_row = finding
            .evidence
            .iter()
            .find(|row| !row.refs.is_empty())
            .expect("the HIT branch must emit a row carrying refs");
        let ids: Vec<&str> = cluster_row.refs.iter().map(|r| r.id.as_str()).collect();
        for expected in [
            "GB-LOC:shared-site",
            "GB-COH:applicant-1",
            "GB-COH:applicant-2",
        ] {
            assert!(
                ids.contains(&expected),
                "cluster row must reference {expected}, got {ids:?}"
            );
        }
        assert!(
            !ids.contains(&"GB-LOC:solo-site"),
            "the unshared location must not appear in a cluster row, got {ids:?}"
        );
    }

    #[tokio::test]
    async fn shared_exact_address_no_shared_address_is_clear_with_population() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!(
                "SKIP: shared_exact_address_no_shared_address_is_clear_with_population (set URBAN_DB_TESTS=1)"
            );
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_probe_tier1", "main")
                .await;
        let mut st = Strategy::default();

        // Three Contracts, three distinct applicants, three distinct locations — no sharing.
        for i in 0..3u32 {
            seed_edge_event(
                &db,
                &format!("awarded-{i}"),
                &format!("GB-COH:applicant-{i}"),
                &format!("planning:contract-{i}"),
                &awarded_to(),
            )
            .await;
            seed_edge_event(
                &db,
                &format!("located-{i}"),
                &format!("planning:contract-{i}"),
                &format!("GB-LOC:site-{i}"),
                &located_at(),
            )
            .await;
        }

        let probe = SharedExactAddress;
        let finding = probe.run(&db, &mut st).await;

        assert!(
            matches!(finding.verdict, Verdict::Clear(_)),
            "3 distinct locations, no sharing, must be Clear, got {:?}",
            finding.verdict
        );
        let population = 3u64;
        assert!(
            finding
                .evidence
                .iter()
                .any(|e| e.text.contains(&population.to_string())),
            "evidence must state the total-locations-evaluated population, got: {:?}",
            finding.evidence
        );
        let coverage = match &finding.verdict {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        // 3 distinct locations, none sharing an address -- a corroborated Clear over a real
        // population, not the zero-population shape.
        assert_eq!(
            coverage.population(),
            3,
            "population must count all 3 seeded distinct locations"
        );
        assert_eq!(
            coverage.capable(),
            3,
            "capable must equal population -- every seeded location has a usable exact address"
        );
    }
}
