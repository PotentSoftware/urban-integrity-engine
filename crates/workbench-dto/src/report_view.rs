//! `ReportView` — the Analysis page's wire shape: the BLUF summary, contradiction checks, the six
//! fixed probe groups (each holding zero or more probes), adapted notes and remediation items.
//!
//! No field on any of these types is `#[serde(default)]`. Every field must be explicitly supplied
//! by the server-side producer — an omitted field is a serialization error the developer sees
//! immediately, not a silently-defaulted zero that looks like real data.

use serde::{Deserialize, Serialize};

use crate::entity_ref::EntityRef;
use crate::verdict_view::VerdictView;

/// The Analysis page's top-level wire shape. `groups` is always exactly 6 entries in report order
/// (Confirmed/Suggestive/Data-Integrity/NotApplicable/VerifiedNegative/Skipped) — a `Vec` not a
/// fixed array so serde needs no custom array support, but the producer must always emit exactly
/// 6; that invariant is enforced server-side, not type-level, consistent with this crate's "view
/// types carry no domain invariant" rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportView {
    pub bluf: BlufView,
    pub contradiction_checks: Vec<ContradictionCheckView>,
    pub groups: Vec<ProbeGroupView>,
    pub adapted_notes: Vec<String>,
    pub remediation_items: Vec<String>,
    pub run_at: String,
}

/// Six raw numbers backing the report's BLUF ("bottom line up front") sentences. The frontend
/// formats the four fixed BLUF sentences from these, mirroring `population_headline`'s
/// logic-in-a-plain-function pattern.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlufView {
    pub could_not_evaluate: u64,
    pub total_probes: u64,
    pub skipped: u64,
    pub population: u64,
    pub capable: u64,
    pub hit_count: u64,
}

/// One of the report's six fixed probe groups. No per-group badge field — the frontend derives
/// the group's badge styling from `name` via a static client-side lookup keyed on the 6 fixed
/// group name strings; this is a deliberate discretion call, not a scope cut, since D-17's
/// "representative badge" requirement is satisfied by styling, not a server-computed
/// pseudo-verdict for a group with no single real verdict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeGroupView {
    pub name: String,
    pub probes: Vec<ProbeView>,
}

/// A single probe's rendered result. `verdict` reuses `VerdictView` directly — never re-derived.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeView {
    pub name: String,
    pub tier: String,
    pub hypothesis: String,
    pub verdict: VerdictView,
    pub measurement_summary: String,
    pub evidence: Vec<EvidenceRowView>,
    /// This probe's motif classification (D-06, phase 50) — the plain wire spelling of
    /// `urban_integrity_probe::MotifKind::name()`, e.g. `"Star"`, `"Chips"`, `"Absence"`. The
    /// frontend matches on this string to pick a card's thumbnail renderer; it is never
    /// constructed or inferred client-side (T-50-04) — an unrecognised value must fall through
    /// to an `Absence`-equivalent default, never a panic.
    ///
    /// `#[serde(default)]` is load-bearing for BACKWARD COMPATIBILITY, not convenience: this
    /// field was added in phase 50, and `workbench_report:latest` rows persisted before it
    /// carry no `motif_kind` key. Without the default, `report_persistence::load` fails the
    /// whole row with `missing field `motif_kind`` and the workbench boots "with no cached
    /// report" — the operator sees "No probe run yet for this analysis" on an analysis that
    /// has one. An empty string is deliberate: it is not a real `MotifKind::name()` spelling,
    /// so the frontend's unrecognised-value fall-through renders it as unknown rather than
    /// claiming a motif the stored run never classified.
    #[serde(default)]
    pub motif_kind: String,
    #[serde(deserialize_with = "required_option")]
    pub cause: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub remediation: Option<String>,
}

/// One row of evidence backing a probe's verdict, with `EntityRef`s pointing at the entities
/// involved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceRowView {
    pub text: String,
    pub refs: Vec<EntityRef>,
}

/// One of the report's cross-probe contradiction checks. `status` is a plain string (not a
/// tagged enum, same "no domain invariant to protect" rationale as `EntityRef.kind`), one of
/// `"contradiction"` / `"clean"` / `"could_not_evaluate"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContradictionCheckView {
    pub label: String,
    pub text: String,
    pub status: String,
}

/// Copied verbatim from `status_view::required_option` (see that module's doc comment for the
/// full rationale) — do not diverge the two copies' behaviour.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict_view::{CoverageView, PopulationSourceView};

    fn coverage() -> CoverageView {
        CoverageView {
            population: 10,
            capable: 10,
            excluded: 0,
        }
    }

    fn self_reported() -> PopulationSourceView {
        PopulationSourceView::SelfReported {
            reason: "test".to_string(),
        }
    }

    fn entity_ref() -> EntityRef {
        EntityRef {
            id: "director:jane-smith".to_string(),
            name: "Jane Smith".to_string(),
            kind: "Entity".to_string(),
        }
    }

    fn probe_with_verdict(name: &str, verdict: VerdictView) -> ProbeView {
        ProbeView {
            name: name.to_string(),
            tier: "tier-1".to_string(),
            hypothesis: "test hypothesis".to_string(),
            verdict,
            measurement_summary: "test summary".to_string(),
            evidence: vec![EvidenceRowView {
                text: "evidence text".to_string(),
                refs: vec![entity_ref()],
            }],
            motif_kind: "Absence".to_string(),
            cause: None,
            remediation: None,
        }
    }

    /// One `ProbeView` per of the 7 `VerdictView` variants, one probe each — matches
    /// `verdict_view`'s `all_variants` fixture set.
    fn one_probe_per_verdict_variant() -> Vec<ProbeView> {
        vec![
            probe_with_verdict(
                "hit-probe",
                VerdictView::Hit {
                    coverage: coverage(),
                    matches: 3,
                    population_source: self_reported(),
                },
            ),
            probe_with_verdict(
                "weak-probe",
                VerdictView::Weak {
                    coverage: coverage(),
                    matches: 1,
                    population_source: self_reported(),
                },
            ),
            probe_with_verdict(
                "clear-probe",
                VerdictView::Clear {
                    coverage: coverage(),
                    population_source: self_reported(),
                },
            ),
            probe_with_verdict(
                "not-applicable-probe",
                VerdictView::NotApplicable {
                    coverage: coverage(),
                    reason: "3 structurally excluded".to_string(),
                    population_source: self_reported(),
                },
            ),
            probe_with_verdict(
                "data-gap-probe",
                VerdictView::DataGap {
                    missing_input: "missing OSINT source".to_string(),
                },
            ),
            probe_with_verdict(
                "blocked-probe",
                VerdictView::Blocked {
                    prerequisite: "identity-integrity".to_string(),
                },
            ),
            probe_with_verdict(
                "skipped-probe",
                VerdictView::Skipped {
                    cause_description: "upstream 'identity-integrity' returned Clear".to_string(),
                    upstream: Some("identity-integrity".to_string()),
                    tier: None,
                },
            ),
        ]
    }

    fn sample_report() -> ReportView {
        let mut probes = one_probe_per_verdict_variant();
        let skipped_probe = probes.pop().unwrap();

        ReportView {
            bluf: BlufView {
                could_not_evaluate: 1,
                total_probes: 22,
                skipped: 1,
                population: 358_484,
                capable: 358_483,
                hit_count: 1,
            },
            contradiction_checks: vec![
                ContradictionCheckView {
                    label: "check-a".to_string(),
                    text: "contradiction text".to_string(),
                    status: "contradiction".to_string(),
                },
                ContradictionCheckView {
                    label: "check-b".to_string(),
                    text: "clean text".to_string(),
                    status: "clean".to_string(),
                },
            ],
            groups: vec![
                ProbeGroupView {
                    name: "Confirmed".to_string(),
                    probes: vec![probes[0].clone()],
                },
                ProbeGroupView {
                    name: "Suggestive".to_string(),
                    probes: vec![probes[1].clone()],
                },
                ProbeGroupView {
                    name: "VerifiedNegative".to_string(),
                    probes: vec![probes[2].clone()],
                },
                ProbeGroupView {
                    name: "NotApplicable".to_string(),
                    probes: vec![probes[3].clone()],
                },
                ProbeGroupView {
                    name: "DataIntegrity".to_string(),
                    probes: vec![probes[4].clone(), probes[5].clone()],
                },
                ProbeGroupView {
                    name: "Skipped".to_string(),
                    probes: vec![skipped_probe],
                },
            ],
            adapted_notes: vec!["note one".to_string()],
            remediation_items: vec!["remediate this".to_string()],
            run_at: "2026-01-01T10:33:00Z".to_string(),
        }
    }

    /// `ReportView` with one `ProbeView` per of the 7 `VerdictView` variants, grouped into the 6
    /// `ProbeGroupView`s (Skipped group non-empty), round-trips.
    #[test]
    fn report_with_all_seven_verdict_variants_round_trips() {
        let report = sample_report();
        assert_eq!(report.groups.len(), 6);
        let skipped_group = report
            .groups
            .iter()
            .find(|g| g.name == "Skipped")
            .expect("Skipped group must exist");
        assert!(!skipped_group.probes.is_empty());

        let json = serde_json::to_string(&report).expect("ReportView must serialize");
        let back: ReportView = serde_json::from_str(&json).expect("ReportView must deserialize");
        assert_eq!(back, report);
    }

    /// The Skipped group with zero probes still round-trips as `probes: []`, never an omitted
    /// field.
    #[test]
    fn skipped_group_with_zero_probes_round_trips_as_empty_array() {
        let mut report = sample_report();
        let skipped = report
            .groups
            .iter_mut()
            .find(|g| g.name == "Skipped")
            .unwrap();
        skipped.probes = vec![];

        let json = serde_json::to_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let skipped_json = value["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == "Skipped")
            .unwrap();
        assert!(skipped_json["probes"].is_array());
        assert_eq!(skipped_json["probes"].as_array().unwrap().len(), 0);
        assert!(
            skipped_json.as_object().unwrap().contains_key("probes"),
            "probes key must be present even when empty, never omitted"
        );

        let back: ReportView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    /// A `ProbeView` JSON payload missing the `verdict` key fails to deserialize.
    #[test]
    fn probe_view_missing_verdict_key_fails_to_deserialize() {
        let missing_verdict = r#"{
            "name": "test-probe",
            "tier": "tier-1",
            "hypothesis": "test hypothesis",
            "measurement_summary": "test summary",
            "evidence": [],
            "motif_kind": "Absence",
            "cause": null,
            "remediation": null
        }"#;
        assert!(
            serde_json::from_str::<ProbeView>(missing_verdict).is_err(),
            "an omitted `verdict` key must be a deserialization error"
        );
    }

    /// A `ProbeView` persisted BEFORE phase 50 added `motif_kind` still deserializes, rather
    /// than failing the whole stored report. Regression test for the boot-time
    /// `workbench_report: stored view failed to deserialize: missing field `motif_kind``
    /// observed against the live analysis store on 2026-09-17, which silently
    /// degraded `/analysis` to "No probe run yet for this analysis".
    #[test]
    fn a_probe_view_stored_before_motif_kind_existed_still_deserializes() {
        let pre_phase_50 = r#"{
            "name": "test-probe",
            "tier": "tier-1",
            "hypothesis": "test hypothesis",
            "verdict": {
                "kind": "DataGap",
                "missing_input": "test input"
            },
            "measurement_summary": "test summary",
            "evidence": [],
            "cause": null,
            "remediation": null
        }"#;
        let probe: ProbeView = serde_json::from_str(pre_phase_50)
            .expect("a stored view predating `motif_kind` must still load, not fail the report");
        assert_eq!(
            probe.motif_kind, "",
            "the default must be a non-spelling, so the frontend falls through to unknown \
             rather than claiming a motif the stored run never classified"
        );
    }

    /// Two `ContradictionCheckView`s with `status: "contradiction"` and `status: "clean"` produce
    /// visibly distinct wire values — the `"status"` string values themselves differ, not just
    /// the Rust values.
    #[test]
    fn contradiction_check_status_values_are_visibly_distinct_on_the_wire() {
        let contradiction = ContradictionCheckView {
            label: "check-a".to_string(),
            text: "contradiction text".to_string(),
            status: "contradiction".to_string(),
        };
        let clean = ContradictionCheckView {
            label: "check-b".to_string(),
            text: "clean text".to_string(),
            status: "clean".to_string(),
        };

        let contradiction_value = serde_json::to_value(&contradiction).unwrap();
        let clean_value = serde_json::to_value(&clean).unwrap();

        assert_eq!(
            contradiction_value["status"],
            serde_json::json!("contradiction")
        );
        assert_eq!(clean_value["status"], serde_json::json!("clean"));
        assert_ne!(contradiction_value["status"], clean_value["status"]);
    }

    /// `ProbeView.verdict`'s type is `crate::verdict_view::VerdictView` — proven by constructing
    /// one directly and round-tripping it through the field, rather than a re-derived local enum.
    #[test]
    fn probe_view_verdict_field_reuses_verdict_view_directly() {
        let probe = probe_with_verdict(
            "hit-probe",
            VerdictView::Hit {
                coverage: coverage(),
                matches: 3,
                population_source: self_reported(),
            },
        );
        let json = serde_json::to_string(&probe).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        // VerdictView's own `#[serde(tag = "kind")]` discipline is visible through the reused
        // field — a re-derived local enum without that attribute would not produce this shape.
        assert_eq!(value["verdict"]["kind"], serde_json::json!("Hit"));

        let back: ProbeView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, probe);
    }
}
