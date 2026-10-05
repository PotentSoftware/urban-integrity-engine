//! `ReportView` assembly (D-15/D-20) — builds the Analysis page's wire shape from a completed
//! `BatteryResult`, reusing the Markdown report writer's own grouping/formatting primitives
//! (`crates/probe::report::{bluf, adapted, remediation, contradictions, DETAIL_GROUPS}`) rather
//! than re-deriving them, so a DB-gated test
//! (`crates/workbench/tests/report_view_equals_finding_list.rs`) can prove the two are the same
//! partition of the same `Finding` list.

use urban_integrity_probe::report::{bluf, contradictions, ReportEntry, DETAIL_GROUPS};
use urban_integrity_probe::run::BatteryResult;
use urban_integrity_schema::types::verdict::{
    Cause, EvidenceRow, RecordKind, RecordRef, RemediationHint, Verdict,
};
use urban_integrity_workbench_dto::{
    BlufView, ContradictionCheckView, EntityRef, EvidenceRowView, ProbeGroupView, ProbeView,
    ReportView,
};

use crate::graph::label_index::LabelIndex;
use crate::verdict_conversion::to_verdict_view;

/// One of `"contradiction"` / `"could_not_evaluate"` / `"clean"` — priority-ordered so a text
/// containing both markers (should not happen in practice, but the priority makes the mapping
/// total and unambiguous) always resolves to the more actionable state.
fn contradiction_status(text: &str) -> &'static str {
    if text.contains("CONTRADICTION") {
        "contradiction"
    } else if text.contains("could not evaluate") {
        "could_not_evaluate"
    } else {
        "clean"
    }
}

/// `RecordRef` -> `EntityRef` (D-20). For `RecordKind::Entity` ids of the shape
/// `director:<slug>` (`EntityId`'s documented director-keying convention), title-case the
/// hyphen-separated slug into a display name (`director:jane-smith` -> `"Jane Smith"`). Every
/// other id shape — `RecordKind::Entity` ids not shaped `director:...`, `RecordKind::Contract`,
/// `RecordKind::Location`, `RecordKind::CompanyNumber` and `RecordKind::IdentityLink` — falls back
/// to `name == id`: honest, never fabricates a name the id does not encode.
///
/// # `kind` is the node class, resolved through the label index (plan 45-11 Task 2)
///
/// It used to be `RecordKind::name` alone, with a comment claiming D-06 had spelled the two
/// vocabularies to agree. They agree on `Contract` and `Location` and **not** on `Entity`:
/// `RecordKind::Entity` is documented in `crates/schema` as *"the catch-all every node id is
/// addressable as"*, so it says "this id names some graph node", where `node_kind_name`'s
/// `"Entity"` says "this node is a `NodeWeight::Entity`". The workbench's pivot policy
/// (`workbench-web`'s `opens_a_profile`) is defined over the second meaning, because
/// `query_entity_by_id` is.
///
/// Measured on the live analysis's persisted report, 2026-09-16: of 1,844 refs carrying
/// `kind: "Entity"`, **50 named a node class the profile route cannot open** — 20 `CF:` contracts,
/// 20 `LOC:`/`location:` addresses and 10 `ward:` wards — every one of which would have rendered as
/// a link that resolves to "No entity with id … exists in this analysis."
///
/// The index is the same `LabelIndex` `GET /api/entities/search` and the entity profile already
/// resolve names through, built at boot from the replayed graph, so this is a lookup rather than a
/// new read. When it does not hold the id — a `CompanyNumber` (a bare `99000001` is not a node id)
/// or an `IdentityLink` record id, neither of which is ever a node — the fallback is
/// `RecordKind::name`, exactly the previous behaviour.
///
/// The **name** rule is deliberately untouched. The index would supply a better display name than
/// a bare id for most refs, and `profile_map::entity_ref_for` already prefers it, but that is a
/// copy change rather than the correctness defect above; it is noted rather than smuggled in here.
fn entity_ref_from_record_ref(r: &RecordRef, label_index: &LabelIndex) -> EntityRef {
    let kind = label_index
        .index_of(&r.id)
        .and_then(|idx| label_index.entity_ref_at(idx))
        .map(|indexed| indexed.kind)
        .unwrap_or_else(|| r.kind.name().to_string());

    let name = match r.kind {
        RecordKind::Entity if r.id.starts_with("director:") => {
            let slug = r.id.trim_start_matches("director:");
            slug.split('-')
                .map(title_case_word)
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => r.id.clone(),
    };

    EntityRef {
        id: r.id.clone(),
        name,
        kind,
    }
}

/// Title-cases one hyphen-split slug word (`"jane"` -> `"Jane"`). Empty input yields empty
/// output — never panics on a leading/trailing/doubled hyphen.
fn title_case_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn evidence_row_view(row: &EvidenceRow, label_index: &LabelIndex) -> EvidenceRowView {
    EvidenceRowView {
        text: row.text.clone(),
        refs: row
            .refs
            .iter()
            .map(|r| entity_ref_from_record_ref(r, label_index))
            .collect(),
    }
}

/// Exhaustive match over the 5 `Cause` variants (no wildcard arm) — mirrors `VerdictKind::of`'s
/// discipline: an 6th `Cause` variant fails this match to compile rather than silently defaulting.
fn cause_text(cause: Option<Cause>) -> Option<String> {
    cause.map(|c| {
        match c {
            Cause::NoOfficerNodes => "No officer nodes",
            Cause::AdapterNeverRan => "Adapter never ran",
            Cause::NoRegisterOverlap => "No register overlap",
            Cause::LinksNotElevated => "Links not elevated",
            Cause::NoLiveBan => "No live ban",
        }
        .to_string()
    })
}

/// The one `RemediationHint` variant today, formatted with the same pluralisation
/// `crates/probe::report::remediation` itself uses.
fn remediation_text(hint: Option<RemediationHint>) -> Option<String> {
    hint.map(|h| match h {
        RemediationHint::WardsUnattributed(n) => format!(
            "{} lack ward attribution, limiting IMD weighting accuracy",
            urban_integrity_probe::plural(n, "contract", "contracts")
        ),
    })
}

/// One `ProbeView` per [`ReportEntry`] — `.verdict` reuses [`to_verdict_view`] directly, never
/// re-derived.
fn probe_view(entry: &ReportEntry, label_index: &LabelIndex) -> ProbeView {
    ProbeView {
        name: entry.finding.probe.clone(),
        tier: entry.tier.to_string(),
        hypothesis: entry.hypothesis.to_string(),
        verdict: to_verdict_view(&entry.finding.verdict, &entry.population_source),
        measurement_summary: entry.finding.summary.clone(),
        evidence: entry
            .finding
            .evidence
            .iter()
            .map(|row| evidence_row_view(row, label_index))
            .collect(),
        motif_kind: entry.motif_kind.name().to_string(),
        cause: cause_text(entry.finding.cause),
        remediation: remediation_text(entry.finding.remediation_hint),
    }
}

/// Builds the Analysis page's `ReportView` from a completed [`BatteryResult`], reusing the
/// Markdown writer's own grouping/formatting primitives (D-15):
///
/// - `groups`: [`DETAIL_GROUPS`]'s 5 predicates plus one final `Skipped` group, iterated and
///   filtered directly over `result.entries` — the SAME iterate-and-filter shape
///   `render_evidence_report` itself uses (D-08's anti-pattern rule: never a `HashMap` bucket).
/// - `bluf`: [`bluf::compute`]'s numbers, the same computation `bluf::render` formats into
///   prose.
/// - `contradiction_checks`: exactly 2 entries, from [`contradictions::check_1`]/
///   [`contradictions::check_2`]'s own text, each paired with [`contradiction_status`].
/// - `adapted_notes`/`remediation_items`: `crates/probe::report::adapted::steps`/`
///   crates/probe::report::remediation::items`'s own derived lists.
///
/// `label_index` is the boot-time index the evidence refs' node classes are resolved through —
/// see [`entity_ref_from_record_ref`] for the fifty live refs whose `kind` it corrects.
pub fn from_battery(
    result: &BatteryResult,
    run_at: chrono::DateTime<chrono::Utc>,
    label_index: &LabelIndex,
) -> ReportView {
    let mut groups: Vec<ProbeGroupView> = DETAIL_GROUPS
        .iter()
        .map(|(title, in_group)| ProbeGroupView {
            name: (*title).to_string(),
            probes: result
                .entries
                .iter()
                .filter(|e| in_group(&e.finding.verdict))
                .map(|entry| probe_view(entry, label_index))
                .collect(),
        })
        .collect();

    groups.push(ProbeGroupView {
        name: "Skipped".to_string(),
        probes: result
            .entries
            .iter()
            .filter(|e| matches!(e.finding.verdict, Verdict::Skipped { .. }))
            .map(|entry| probe_view(entry, label_index))
            .collect(),
    });

    let n = bluf::compute(&result.entries);
    let bluf_view = BlufView {
        could_not_evaluate: n.could_not_evaluate,
        total_probes: n.total_probes,
        skipped: n.skipped,
        population: n.population,
        capable: n.capable,
        hit_count: n.hit_count,
    };

    let check_1_text = contradictions::check_1(&result.entries);
    let check_2_text = contradictions::check_2(&result.entries, &result.strategy);
    let contradiction_checks = vec![
        ContradictionCheckView {
            label: "Independently-measured populations".to_string(),
            status: contradiction_status(&check_1_text).to_string(),
            text: check_1_text,
        },
        ContradictionCheckView {
            label: "Attributed evidence vs run markers".to_string(),
            status: contradiction_status(&check_2_text).to_string(),
            text: check_2_text,
        },
    ];

    ReportView {
        bluf: bluf_view,
        contradiction_checks,
        groups,
        adapted_notes: urban_integrity_probe::report::adapted::steps(
            &result.entries,
            &result.strategy,
        ),
        remediation_items: urban_integrity_probe::report::remediation::items(
            &result.entries,
            &result.strategy,
        ),
        run_at: run_at.to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use urban_integrity_probe::Strategy;
    use urban_integrity_schema::types::verdict::{Finding, PopulationSource, Severity};

    fn finding(probe: &str, verdict: Verdict) -> Finding {
        Finding {
            probe: probe.to_string(),
            verdict,
            summary: format!("{probe} summary"),
            cause: None,
            severity: Severity::Info,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        }
    }

    fn entry(probe: &str, verdict: Verdict) -> ReportEntry {
        ReportEntry {
            tier: "Tier-1",
            hypothesis: "test hypothesis",
            motif_kind: urban_integrity_probe::MotifKind::Absence,
            finding: finding(probe, verdict),
            population_source: PopulationSource::SelfReported {
                reason: "test fixture",
            },
        }
    }

    fn seven_variant_entries() -> Vec<ReportEntry> {
        let clear_upstream = Verdict::classify(10, 10, 0, 1, "");
        vec![
            entry("hit-probe", Verdict::classify(10, 10, 5, 1, "")),
            entry("weak-probe", Verdict::classify(10, 10, 3, 5, "")),
            entry("clear-probe", Verdict::classify(10, 10, 0, 1, "")),
            entry(
                "not-applicable-probe",
                Verdict::classify(10, 7, 0, 1, "3 structurally excluded"),
            ),
            entry("data-gap-probe", Verdict::data_gap("missing input")),
            entry("blocked-probe", Verdict::blocked("unmet prerequisite")),
            entry(
                "skipped-probe",
                Verdict::skipped_dependency("identity-integrity", &clear_upstream),
            ),
        ]
    }

    /// `from_battery` on a 7-entry fixture (one per `Verdict` variant) produces exactly 6
    /// groups, the Skipped group containing the one Skipped entry, and every other entry in
    /// exactly the group `DETAIL_GROUPS`/the Skipped predicate assigns it to — the non-vacuous
    /// proof `crates/probe::report::mod`'s
    /// `every_verdict_variant_renders_in_detail_groups_or_the_skipped_section` established for
    /// Markdown, applied here to `ReportView`.
    #[test]
    fn from_battery_places_every_verdict_variant_in_exactly_one_of_six_groups() {
        let entries = seven_variant_entries();
        let result = BatteryResult {
            entries,
            strategy: Strategy::default(),
        };

        let view = from_battery(&result, chrono::Utc::now(), &LabelIndex::default());

        assert_eq!(view.groups.len(), 6, "expected exactly 6 groups");

        for entry in &result.entries {
            let homes: usize = view
                .groups
                .iter()
                .filter(|g| g.probes.iter().any(|p| p.name == entry.finding.probe))
                .count();
            assert_eq!(
                homes, 1,
                "probe {} must land in exactly one group, got {homes}",
                entry.finding.probe
            );
        }

        let skipped_group = view
            .groups
            .iter()
            .find(|g| g.name == "Skipped")
            .expect("Skipped group must exist");
        assert_eq!(skipped_group.probes.len(), 1);
        assert_eq!(skipped_group.probes[0].name, "skipped-probe");
    }

    /// The Skipped group on a fixture with zero Skipped entries still has `probes: []`, not
    /// omitted — mirrors `crates/workbench-dto::report_view`'s own wire-level proof, at the
    /// producer level.
    #[test]
    fn skipped_group_with_zero_skipped_entries_is_still_present_and_empty() {
        let entries = vec![entry("clear-probe", Verdict::classify(10, 10, 0, 1, ""))];
        let result = BatteryResult {
            entries,
            strategy: Strategy::default(),
        };

        let view = from_battery(&result, chrono::Utc::now(), &LabelIndex::default());

        let skipped_group = view
            .groups
            .iter()
            .find(|g| g.name == "Skipped")
            .expect("Skipped group must exist even with zero Skipped entries");
        assert!(skipped_group.probes.is_empty());
    }

    #[test]
    fn contradiction_status_prioritises_contradiction_over_could_not_evaluate() {
        assert_eq!(
            contradiction_status("this is a CONTRADICTION"),
            "contradiction"
        );
        assert_eq!(
            contradiction_status("could not evaluate — absent"),
            "could_not_evaluate"
        );
        assert_eq!(contradiction_status("no contradiction found"), "clean");
    }

    #[test]
    fn entity_ref_from_record_ref_title_cases_director_slug() {
        let r = RecordRef {
            kind: RecordKind::Entity,
            id: "director:jane-smith".to_string(),
        };
        let view = entity_ref_from_record_ref(&r, &LabelIndex::default());
        assert_eq!(view.name, "Jane Smith");
        assert_eq!(view.id, "director:jane-smith");
        assert_eq!(view.kind, "Entity");
    }

    #[test]
    fn entity_ref_from_record_ref_falls_back_to_id_for_non_director_shapes() {
        let r = RecordRef {
            kind: RecordKind::CompanyNumber,
            id: "12345678".to_string(),
        };
        let view = entity_ref_from_record_ref(&r, &LabelIndex::default());
        assert_eq!(view.name, "12345678");
        assert_eq!(view.id, "12345678");
        assert_eq!(view.kind, "CompanyNumber");
    }
    /// **`RecordKind::Entity` is a catch-all, and this is where that stops being the analyst's
    /// problem.** A `ward:` id carries `RecordKind::Entity` on every probe that emits one, because
    /// that variant means "some graph node id". Put on the wire unchanged it tells the workbench
    /// the node is a `NodeWeight::Entity`, which is the exact predicate the pivot policy is
    /// defined over — so the row renders as a link and the link 404s.
    ///
    /// Fifty refs on the live analysis's persisted report were in this position (20 `CF:`, 20
    /// `LOC:`/`location:`, 10 `ward:`), out of 1,844 carrying `kind: "Entity"`.
    ///
    /// PROVE THIS CAN FAIL: restore `let kind = r.kind.name();` in `entity_ref_from_record_ref`
    /// and the ward assertion reads `"Entity"`.
    #[test]
    fn a_ward_id_reaches_the_wire_as_a_ward_not_as_the_entity_catch_all() {
        use crate::graph::label_index::LabelIndex;
        use urban_integrity_schema::types::id::EntityId;
        use urban_integrity_schema::types::node::{
            EntityData, EntityStatus as NodeStatus, NodeWeight, WardData,
        };

        let storage = urban_integrity_core::graph::GraphStorage::new();
        storage.insert_node(
            EntityId("ward:X05999001".to_string()),
            NodeWeight::Ward(WardData {
                name: "Example Ward".to_string(),
            }),
        );
        storage.insert_node(
            EntityId("GB-COH:X6999001".to_string()),
            NodeWeight::Entity(EntityData {
                name: "Example Holdings Limited".to_string(),
                company_number: Some("X6999001".to_string()),
                status: NodeStatus::Active,
                incorporation_date: None,
            }),
        );
        let index = LabelIndex::build(&storage);

        let ward = RecordRef {
            kind: RecordKind::Entity,
            id: "ward:X05999001".to_string(),
        };
        assert_eq!(
            entity_ref_from_record_ref(&ward, &index).kind,
            "Ward",
            "the node class must reach the wire, not the RecordKind catch-all"
        );

        // Non-vacuity: a ref that really is an Entity node still says so, so this is a statement
        // about the lookup rather than about a function that stopped emitting "Entity" at all.
        let company = RecordRef {
            kind: RecordKind::Entity,
            id: "GB-COH:X6999001".to_string(),
        };
        assert_eq!(entity_ref_from_record_ref(&company, &index).kind, "Entity");

        // And an id the index does not hold keeps the previous behaviour rather than losing its
        // kind: a `CompanyNumber` is not a node id and never will be in this index.
        let number = RecordRef {
            kind: RecordKind::CompanyNumber,
            id: "12345678".to_string(),
        };
        assert_eq!(
            entity_ref_from_record_ref(&number, &index).kind,
            "CompanyNumber"
        );
    }
}
