//! Shared disqualification-chain read -- D4's 5-cause measurement, ported from Python's
//! `_disqualification_chain` (`scripts/analyst_probe.py:1702-1809`). Both
//! `disqualified_active_in_procurement` and `evasion_alerts` (Plan 33-05) build their
//! `Cause`-carrying verdicts on top of [`disqualification_chain`], mapping the result through
//! [`verdict_for_cause`] -- the SAME function both probes import and call, never reimplement, so
//! D4's single cause-to-verdict table is a structural guarantee across the two probes, not a
//! coincidence of two independently-written match statements.

use surrealdb::{engine::any::Any, Surreal};

use super::plural;
use urban_integrity_core::persistence::scan::{self, IdentityLinkRow};
use urban_integrity_schema::types::verdict::{Cause, Verdict};

/// `payload.entity_id` as a string, whether SurrealDB handed it back as a plain string or coerced
/// it into a RecordId object (`director:{slug}`-keyed ids match SurrealDB's `table:key`
/// auto-coercion shape). Mirrors `scan.rs`'s private `entity_id_of`/`record_id_str` and
/// `ward_concentration.rs`'s local copy of the same helper -- neither is exported, so this is
/// another local equivalent per this plan's interfaces note (`scan.rs`'s own visibility is out of
/// this plan's scope).
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

/// The result of walking the disqualification chain: which [`Cause`] applies, the measured
/// counts behind it, and a human-readable explanation for the probe's `Finding.summary`.
pub struct DisqualificationChainState {
    pub cause: Cause,
    pub explain: String,
    pub officer_nodes: u64,
    pub director_entities: u64,
    pub disq_edges: u64,
    pub links: Vec<IdentityLinkRow>,
    pub elevated_count: u64,
}

impl DisqualificationChainState {
    /// The sub-threshold `ch_disqualified` candidate count the ENR-11 status gate held back:
    /// every discovered link that did not reach `auto_linked`/`confirmed`.
    ///
    /// Single source of truth. Both consuming probes (`disqualified_active_in_procurement.rs`,
    /// `evasion_alerts.rs`) previously inlined this subtraction to pass it into
    /// [`verdict_for_cause`], which recomputed nothing -- three copies of one formula. The
    /// verdict path now reads it from here, and the probes call this method for their
    /// human-readable `Finding.summary` string, so the figure a reader sees and the figure
    /// `Coverage::excluded` reports can no longer drift apart.
    pub fn excluded_count(&self) -> u64 {
        (self.links.len() as u64).saturating_sub(self.elevated_count)
    }
}

/// Walks D4's 5-branch decision tree, in the exact order Python's `_disqualification_chain`
/// (`analyst_probe.py:1702-1809`) does:
///
///   1. `officer_nodes == 0`                          -> [`Cause::NoOfficerNodes`]
///   2. `officer_nodes > 0`, no `enrichment_runs` marker -> [`Cause::AdapterNeverRan`]
///   3. marker present, zero `ch_disqualified` links   -> [`Cause::NoRegisterOverlap`]
///   4. links exist, none `auto_linked`/`confirmed`     -> [`Cause::LinksNotElevated`]
///   5. elevated links exist                            -> [`Cause::NoLiveBan`]
///
/// This is a free function, not a [`super::Probe`] -- it is called at most twice per battery run
/// (once per consuming probe), and neither probe holds a `&mut Strategy` reference the other can
/// share through this function's signature. D6's `Strategy.enrichment_runs` memo
/// (`crates/cli/src/commands/probe/mod.rs`) exists to avoid re-querying WITHIN one battery run
/// across multiple `Probe::run` calls that all share the same `&mut Strategy`; this function has
/// no `Strategy` to memoize into, so it reads `enrichment_runs` fresh on every call instead. A
/// future reader should not "fix" this into a `Strategy`-threaded call -- doing so would require
/// plumbing `&mut Strategy` through this free function's signature and both consuming probes'
/// call sites, which is out of this plan's scope and not what D6 asked for (D6's memo amortises
/// re-reads WITHIN a battery run across many probes calling `Strategy::enrichment_runs`, not
/// across the two calls this function itself makes).
pub async fn disqualification_chain(db: &Surreal<Any>) -> DisqualificationChainState {
    let officer_nodes = scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Officer\"")
        .await
        .map(|rows| rows.len() as u64)
        .unwrap_or(0);

    // Directors ARE in the graph -- as Entity nodes keyed `director:{slug}` (the
    // load-bearing dedup invariant). Filtered in Rust after fetching, matching
    // `ward_concentration.rs`'s/`detector_readiness.rs`'s own D4 guard-shape-avoidance pattern --
    // never a fresh `string::starts_with(...)` SurrealQL predicate.
    let director_entities = scan::scan_events_by_type(db, "NodeAdded", "\"type\":\"Entity\"")
        .await
        .map(|rows| {
            rows.iter()
                .filter(|r| {
                    record_id_str(&r["payload"]["entity_id"])
                        .is_some_and(|id| id.starts_with("director:"))
                })
                .count() as u64
        })
        .unwrap_or(0);

    let disq_edges = scan::scan_events_by_type(db, "EdgeAdded", "\"type\":\"DisqualifiedFrom\"")
        .await
        .map(|rows| rows.len() as u64)
        .unwrap_or(0);

    // links.rs count is the finding -- source-filtered, indexed equality (not the CONTAINS
    // planner-bug shape), matching Python's `fetch_identity_links(source_filter="ch_disqualified")`.
    let links = scan::scan_identity_links(db, None, Some("ch_disqualified"))
        .await
        .unwrap_or_default();
    let elevated_count = links
        .iter()
        .filter(|l| l.status == "auto_linked" || l.status == "confirmed")
        .count() as u64;

    // ENR-11 gate (crates/enricher/src/evasion.rs:64-80) is what "elevated" means: `status IN
    // ['auto_linked','confirmed']`.
    let marker = scan::scan_enrichment_runs(db)
        .await
        .unwrap_or_default()
        .get("ch_disqualified")
        .cloned();

    let (cause, explain) = if officer_nodes == 0 {
        // D-03: `plural()` picks the grammatically correct noun form instead of a mechanical
        // trailing-paren-ess suffix banned from probe/report production text.
        let entity_nodes = plural(director_entities, "Entity node", "Entity nodes");
        (
            Cause::NoOfficerNodes,
            format!(
                "Measured cause: the graph holds 0 Officer nodes, so the ch_disqualified \
                 adapter's input set is empty and it issued zero Companies House requests. The \
                 register was never searched -- this is not evidence that it fails to overlap. \
                 Directors are present as {entity_nodes} keyed \
                 `director:{{slug}}`, but every officer-consuming path filters on \
                 \"type\":\"Officer\" -- a typing gap, not a data gap. Phase 26 (PHX-01-05) is \
                 what creates Officer nodes."
            ),
        )
    } else if marker.is_none() {
        let officer_nodes_exist =
            plural(officer_nodes, "Officer node exists", "Officer nodes exist");
        (
            Cause::AdapterNeverRan,
            format!(
                "Measured cause: {officer_nodes_exist}, but the ch_disqualified \
                 adapter has no enrichment_runs marker -- it has never executed. Set \
                 URBAN__ENRICHER__CH__API_KEY (the REST key) and run `urban-integrity enrich \
                 --source psc`."
            ),
        )
    } else if links.is_empty() {
        let day = marker
            .as_ref()
            .and_then(|m| chrono::DateTime::from_timestamp(m.last_run_at, 0))
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "an unknown date".to_string());
        let candidates_found = marker.as_ref().map(|m| m.candidates_found).unwrap_or(0);
        let officer_nodes_desc = plural(officer_nodes, "Officer node", "Officer nodes");
        (
            Cause::NoRegisterOverlap,
            format!(
                "Measured cause: the adapter ran {day} against {officer_nodes_desc} \
                 and searched the register (candidates_found={candidates_found}), but no result \
                 met the three-field minimum (name + at least birth year), so no link was \
                 written. The register genuinely does not intersect this graph."
            ),
        )
    } else if elevated_count == 0 {
        let links_exist = plural(
            links.len() as u64,
            "ch_disqualified link exists",
            "ch_disqualified links exist",
        );
        (
            Cause::LinksNotElevated,
            format!(
                "Measured cause: {links_exist} but none reached \
                 auto_linked/confirmed. The ENR-11 status gate \
                 (crates/enricher/src/evasion.rs:64-80) deliberately keeps sub-threshold \
                 candidates away from a CRITICAL alert. Review them with `urban-integrity links \
                 --status pending_review`."
            ),
        )
    } else {
        let links_exist = plural(
            elevated_count,
            "confirmed disqualified-director link exists",
            "confirmed disqualified-director links exist",
        );
        let edges_carry = plural(
            disq_edges,
            "DisqualifiedFrom edge carries",
            "DisqualifiedFrom edges carry",
        );
        (
            Cause::NoLiveBan,
            format!(
                "Measured cause: {links_exist} and the detector evaluated the set, but none is \
                 both currently Active and inside its ban window ({edges_carry} the windows). \
                 This is a genuine clean result, not a coverage gap."
            ),
        )
    };

    DisqualificationChainState {
        cause,
        explain,
        officer_nodes,
        director_entities,
        disq_edges,
        links,
        elevated_count,
    }
}

/// D4's single cause-to-verdict mapping table (LOCKED, the phase 32 design notes/this plan's
/// `<interfaces>`), applied identically by BOTH `disqualified_active_in_procurement` and
/// `evasion_alerts` -- imported by both, never reimplemented, so "both probes match D4's table"
/// is a structural guarantee, not two independently-written match statements that happen to
/// agree.
///
/// | Cause               | Verdict        |
/// |----------------------|----------------|
/// | `NoOfficerNodes`     | `DataGap`      |
/// | `AdapterNeverRan`    | `DataGap`      |
/// | `NoRegisterOverlap`  | `Clear`        |
/// | `LinksNotElevated`   | `NotApplicable`|
/// | `NoLiveBan`          | `Clear`        |
///
/// `excluded_count` (only meaningful for [`Cause::LinksNotElevated`] -- the sub-threshold
/// candidate count the ENR-11 gate held back, reported as `NotApplicable`'s `Coverage::excluded`)
/// is read from [`DisqualificationChainState::excluded_count`], never supplied by
/// the caller -- both call sites used to duplicate this computation before passing it in; now it
/// lives in exactly one place. `NoOfficerNodes`/`AdapterNeverRan` carry no `Coverage` at all and
/// read no `state` fields beyond `cause`. `NoRegisterOverlap`/`NoLiveBan` report the caller's real
/// measured population (`state.officer_nodes`/`state.elevated_count` respectively) as BOTH
/// `population` and `capable`, closing TD-v7-01 -- previously both hardcoded `Verdict::classify(0,
/// 0, 0, 1, "")`, making a genuinely clean 0-population result indistinguishable from a clean
/// result over a real, non-trivial population.
pub fn verdict_for_cause(state: &DisqualificationChainState) -> Verdict {
    let excluded_count = state.excluded_count();
    match state.cause {
        Cause::NoOfficerNodes => Verdict::data_gap(
            "0 Officer nodes -- the ch_disqualified adapter's input set is empty (typing gap, \
             not a data gap)",
        ),
        Cause::AdapterNeverRan => {
            Verdict::data_gap("no enrichment_runs marker for ch_disqualified -- adapter never ran")
        }
        Cause::NoRegisterOverlap => {
            Verdict::classify(state.officer_nodes, state.officer_nodes, 0, 1, "")
        }
        Cause::LinksNotElevated => Verdict::classify(
            excluded_count,
            0,
            0,
            1,
            "sub-threshold ch_disqualified candidates held back by the ENR-11 status gate",
        ),
        Cause::NoLiveBan => Verdict::classify(state.elevated_count, state.elevated_count, 0, 1, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::edge::{DisqualifiedFromData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight, OfficerData};

    async fn seed_officer_node(db: &Surreal<Any>, officer_id: &str) {
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

    async fn seed_director_entity_node(db: &Surreal<Any>, director_id: &str) {
        let weight = NodeWeight::Entity(EntityData {
            name: director_id.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        let weight_json = serde_json::to_string(&weight).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("node-{director_id}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeAdded",
                    "payload": { "entity_id": director_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed director entity node");
    }

    async fn seed_disqualified_from_edge(db: &Surreal<Any>, officer_id: &str, disq_id: &str) {
        let edge = EdgeWeight::DisqualifiedFrom {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: DisqualifiedFromData {
                disqualified_from: Some(chrono::Utc::now() - chrono::Duration::days(365)),
                disqualified_until: Some(chrono::Utc::now() + chrono::Duration::days(365)),
                case_number: Some("INS-2024-00001".to_string()),
            },
        };
        let weight_json = serde_json::to_string(&edge).unwrap();
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": format!("edge-disq-{officer_id}-{disq_id}"),
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": {
                        "src_id": officer_id,
                        "dst_id": disq_id,
                        "weight_json": weight_json,
                    },
                }),
            ))
            .await
            .expect("failed to seed DisqualifiedFrom edge");
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
    ) {
        db.query(
            "CREATE identity_links CONTENT { \
                 source_node_id: $sni, \
                 target_node_id: $tni, \
                 resolution_type: 'exact_officer', \
                 confidence: 1.0, \
                 source: 'ch_disqualified', \
                 resolved_at: d'2024-04-12T00:00:00Z', \
                 evidence_json: '{}', \
                 status: $status \
             }",
        )
        .bind(("sni", source_node_id.to_string()))
        .bind(("tni", target_node_id.to_string()))
        .bind(("status", status.to_string()))
        .await
        .expect("failed to seed ch_disqualified identity_link")
        .check()
        .expect("ch_disqualified identity_link CREATE returned a statement error");
    }

    #[tokio::test]
    async fn chain_no_officer_nodes_is_a_typing_gap_not_a_data_gap() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: chain_no_officer_nodes_is_a_typing_gap_not_a_data_gap");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_disq_chain", "c1")
            .await;

        // Zero Officer nodes, but director:*-keyed Entity nodes ARE present -- the "typing gap,
        // not a data gap" evidence the design notes require preserved.
        seed_director_entity_node(&db, "director:jane-smith").await;
        seed_director_entity_node(&db, "director:john-doe").await;

        let state = disqualification_chain(&db).await;
        assert!(
            matches!(state.cause, Cause::NoOfficerNodes),
            "expected NoOfficerNodes, got {:?}",
            state.cause
        );
        assert!(
            state.director_entities > 0,
            "director Entity nodes must be reported present even though Officer nodes are 0"
        );
    }

    #[tokio::test]
    async fn chain_adapter_never_ran_when_officer_nodes_exist_with_no_marker() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: chain_adapter_never_ran_when_officer_nodes_exist_with_no_marker");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_disq_chain", "c2")
            .await;

        seed_officer_node(&db, "officer:jane-smith").await;

        let state = disqualification_chain(&db).await;
        assert!(
            matches!(state.cause, Cause::AdapterNeverRan),
            "expected AdapterNeverRan, got {:?}",
            state.cause
        );
    }

    #[tokio::test]
    async fn chain_no_register_overlap_when_marker_present_and_zero_links() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: chain_no_register_overlap_when_marker_present_and_zero_links");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_disq_chain", "c3")
            .await;

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;

        let state = disqualification_chain(&db).await;
        assert!(
            matches!(state.cause, Cause::NoRegisterOverlap),
            "expected NoRegisterOverlap, got {:?}",
            state.cause
        );

        // TD-v7-01 (VRD-10): the Clear this cause produces must carry the fixture's REAL
        // population, not the 0/0 the pre-fix code hardcoded. Pinning the exact seeded value
        // (1 officer node) rather than only `== state.officer_nodes` keeps the assertion
        // non-vacuous: a regression that reported 0 unconditionally would still satisfy a
        // relative check if `officer_nodes` were also read as 0.
        let coverage = match verdict_for_cause(&state) {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(coverage.population(), state.officer_nodes);
        assert_eq!(
            coverage.population(),
            1,
            "one seeded officer node must surface as population 1, not a hardcoded 0"
        );
        assert_eq!(coverage.capable(), 1);
    }

    #[tokio::test]
    async fn chain_links_not_elevated_reports_the_seeded_candidate_count() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: chain_links_not_elevated_reports_the_seeded_candidate_count");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_disq_chain", "c4")
            .await;

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;
        // Three ch_disqualified links, all sub-threshold ("candidate" -- below the ENR-11 gate).
        seed_ch_disqualified_link(&db, "disq:c1", "officer:jane-smith", "candidate").await;
        seed_ch_disqualified_link(&db, "disq:c2", "officer:john-doe", "candidate").await;
        seed_ch_disqualified_link(&db, "disq:c3", "officer:amy-lee", "candidate").await;

        let state = disqualification_chain(&db).await;
        assert!(
            matches!(state.cause, Cause::LinksNotElevated),
            "expected LinksNotElevated, got {:?}",
            state.cause
        );
        let excluded = (state.links.len() as u64).saturating_sub(state.elevated_count);
        assert_eq!(
            excluded, 3,
            "the excluded (sub-threshold) count must equal the seeded candidate count"
        );
    }

    #[tokio::test]
    async fn chain_no_live_ban_when_elevated_links_exist() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("SKIP: chain_no_live_ban_when_elevated_links_exist");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix("test_disq_chain", "c5")
            .await;

        seed_officer_node(&db, "officer:jane-smith").await;
        seed_enrichment_run(&db, "ch_disqualified").await;
        seed_ch_disqualified_link(&db, "disq:e1", "officer:jane-smith", "confirmed").await;
        seed_disqualified_from_edge(&db, "officer:jane-smith", "disq:e1").await;

        let state = disqualification_chain(&db).await;
        assert!(
            matches!(state.cause, Cause::NoLiveBan),
            "expected NoLiveBan, got {:?}",
            state.cause
        );
        assert_eq!(state.elevated_count, 1);

        // TD-v7-01 (VRD-10): same proof for the NoLiveBan branch, whose real population is
        // the elevated link count rather than the officer-node count.
        let coverage = match verdict_for_cause(&state) {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(coverage.population(), state.elevated_count);
        assert_eq!(
            coverage.population(),
            1,
            "one elevated ch_disqualified link must surface as population 1, not a hardcoded 0"
        );
        assert_eq!(coverage.capable(), 1);
    }

    /// Minimal `DisqualificationChainState` fixture: `explain`/`director_entities`/`disq_edges`
    /// are unread by `verdict_for_cause`, so they stay at placeholder/zero values; callers set
    /// only the fields their branch actually reads.
    fn minimal_state(
        cause: Cause,
        officer_nodes: u64,
        elevated_count: u64,
    ) -> DisqualificationChainState {
        DisqualificationChainState {
            cause,
            explain: String::new(),
            officer_nodes,
            director_entities: 0,
            disq_edges: 0,
            links: vec![],
            elevated_count,
        }
    }

    fn dummy_link() -> IdentityLinkRow {
        IdentityLinkRow {
            id: "identity_links:placeholder".to_string(),
            source_node_id: "disq:placeholder".to_string(),
            target_node_id: "officer:placeholder".to_string(),
            resolution_type: "exact_officer".to_string(),
            confidence: 0.0,
            source: "ch_disqualified".to_string(),
            status: "candidate".to_string(),
            evidence_json: "{}".to_string(),
            resolved_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn verdict_for_cause_maps_each_cause_per_d4_table() {
        assert!(matches!(
            verdict_for_cause(&minimal_state(Cause::NoOfficerNodes, 0, 0)),
            Verdict::DataGap { .. }
        ));
        assert!(matches!(
            verdict_for_cause(&minimal_state(Cause::AdapterNeverRan, 0, 0)),
            Verdict::DataGap { .. }
        ));

        // NoRegisterOverlap: officer_nodes set to an arbitrary non-zero value specifically to
        // prove the field is read, not defaulted -- the pre-fix code hardcoded 0/0 regardless.
        let no_register_overlap = verdict_for_cause(&minimal_state(Cause::NoRegisterOverlap, 5, 0));
        let coverage = match no_register_overlap {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            5,
            "NoRegisterOverlap must report the real officer_nodes count, not a hardcoded 0"
        );

        // NoLiveBan: elevated_count set to an arbitrary non-zero value for the same reason.
        let no_live_ban = verdict_for_cause(&minimal_state(Cause::NoLiveBan, 0, 7));
        let coverage = match no_live_ban {
            Verdict::Clear(coverage) => coverage,
            other => panic!("expected Clear, got {other:?}"),
        };
        assert_eq!(
            coverage.population(),
            7,
            "NoLiveBan must report the real elevated_count, not a hardcoded 0"
        );

        // LinksNotElevated: excluded_count is now computed internally from links.len() -
        // elevated_count, not passed by the caller.
        let mut links_not_elevated_state = minimal_state(Cause::LinksNotElevated, 0, 0);
        links_not_elevated_state.links = vec![dummy_link(), dummy_link(), dummy_link()];
        let not_applicable = verdict_for_cause(&links_not_elevated_state);
        let coverage = match not_applicable {
            Verdict::NotApplicable { coverage, .. } => coverage,
            other => panic!(
                "LinksNotElevated must map to NotApplicable, not DataGap or Clear, got {:?}",
                other
            ),
        };
        assert_eq!(
            coverage.excluded(),
            3,
            "excluded population must equal links.len() - elevated_count"
        );
    }
}
