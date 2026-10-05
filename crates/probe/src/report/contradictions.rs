//! RPT-05/D-15's contradiction surfacing — exactly two derivable checks, each a pure function of
//! the findings, requiring no new queries.
//!
//! **Check 1 — independently-measured populations that disagree.** The research notes' Open Question
//! flagged that a single equality across all three of `ward-concentration`, `temporal-burst`, and
//! `identity-integrity` would cry wolf on every healthy run: `identity-integrity` measures a
//! planning-filtered SUBSET of the unfiltered Contract population `ward-concentration` and
//! `temporal-burst` measure (which are identical to each other). This module resolves that as
//! TWO sub-comparisons instead of one equality: (1a) `ward-concentration` vs `temporal-burst`
//! must be EQUAL (same unfiltered population, measured independently per 32-CONTEXT D3's
//! no-shared-cache rule); (1b) `identity-integrity` must never EXCEED the larger of the other two
//! (a filtered subset can never exceed its superset). Either firing is "check 1 found a
//! contradiction".
//!
//! The two sub-comparisons are evaluated and reported INDEPENDENTLY, each against only the
//! figures it actually reads. A single shared population guard covering all three probes made 1a
//! unreachable in production: `identity-integrity` reports `DataGap` (no measured population)
//! whenever the `_catalog.analyses` coverage baseline is absent or has regressed, which is its
//! standing state on `an_anglesey_aebaa785`, so RPT-05's headline check rendered "could not
//! evaluate" on every run without ever comparing `ward-concentration` against `temporal-burst`.
//! 1b does not read `ward-concentration` vs `temporal-burst` equality, and 1a does not read
//! `identity-integrity` at all; neither may abort the other.
//!
//! **Check 2 — attributed evidence for a source with no run marker.** `Finding.attributions`
//! knows which source produced each term; `Strategy.enrichment_runs` knows which adapters have a
//! run marker this battery. Non-zero attributed evidence for a markerless source is exactly the
//! ICIJ-silent-write / `donates_to`-junk-event failure class this project has been bitten by
//! twice (see MEMORY.md).
//!
//! Every outcome — contradiction / clean / could-not-evaluate — renders distinguishable text
//! (D-15's "clean vs did-not-run must be distinguishable" rule), so a reader can never mistake
//! "this check did not run" for "this check ran and found nothing".

use crate::{plural, Strategy};

use super::ReportEntry;

const WARD_PROBE: &str = "ward-concentration";
const TEMPORAL_PROBE: &str = "temporal-burst";
const IDENTITY_PROBE: &str = "identity-integrity";

/// One of the three probes check 1 reads, as seen from the report: `None` means the probe was not
/// in this battery at all, `Some(None)` means it ran but measured nothing (`DataGap`/`Blocked`),
/// `Some(Some(n))` means it measured `n`.
type Measured = Option<Option<u64>>;

fn measured(entries: &[ReportEntry], probe: &str) -> Measured {
    entries
        .iter()
        .find(|e| e.finding.probe == probe)
        .map(|e| e.finding.population())
}

/// Why a probe's figure is unusable, or `None` when it is usable — the text each sub-check quotes
/// in its own "could not evaluate" line, so a reader is always told WHICH probe was missing and
/// whether it was absent or merely unmeasured.
fn unusable(probe: &str, state: Measured) -> Option<String> {
    match state {
        None => Some(format!("{probe} absent from this battery")),
        Some(None) => Some(format!(
            "{probe} carried no measured population this run (DataGap/Blocked)"
        )),
        Some(Some(_)) => None,
    }
}

const SUB_1A: &str = "- **1a** (ward-concentration vs temporal-burst)";
const SUB_1B: &str = "- **1b** (identity-integrity vs its unfiltered superset)";

/// Sub-check 1a — the two probes that scan the SAME unfiltered Contract population must agree.
///
/// Evaluated on its own inputs alone. It previously shared one population guard with 1b, so an
/// unmeasured `identity-integrity` — its live state on `an_anglesey_aebaa785`, where the
/// `_catalog.analyses` baseline row makes it report `DataGap` — aborted this comparison too, and
/// the headline check of RPT-05 never once compared anything in production.
fn check_1a(ward: Measured, temporal: Measured) -> String {
    let blockers: Vec<String> = [
        unusable(WARD_PROBE, ward),
        unusable(TEMPORAL_PROBE, temporal),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !blockers.is_empty() {
        return format!("{SUB_1A}: could not evaluate — {}.\n", blockers.join("; "));
    }
    let ward_pop = ward.flatten().expect("no blocker recorded for ward");
    let temporal_pop = temporal
        .flatten()
        .expect("no blocker recorded for temporal");

    if ward_pop != temporal_pop {
        format!(
            "{SUB_1A}: CONTRADICTION — ward-concentration measured {ward_pop} while \
             temporal-burst measured {temporal_pop} — both scan the identical unfiltered \
             Contract population independently and must agree.\n"
        )
    } else {
        format!(
            "{SUB_1A}: no contradiction found — ward-concentration={ward_pop}, \
             temporal-burst={temporal_pop}.\n"
        )
    }
}

/// Sub-check 1b — a planning-filtered subset can never exceed its unfiltered superset.
///
/// Needs `identity-integrity` plus AT LEAST ONE superset figure; it compares against the larger of
/// whichever of the two are available rather than requiring both, since exceeding either one is
/// already the violation.
fn check_1b(ward: Measured, temporal: Measured, identity: Measured) -> String {
    let Some(identity_pop) = identity.flatten() else {
        return format!(
            "{SUB_1B}: could not evaluate — {}.\n",
            unusable(IDENTITY_PROBE, identity).expect("absent/unmeasured always yields a reason")
        );
    };
    let supersets: Vec<(&str, u64)> = [
        (WARD_PROBE, ward.flatten()),
        (TEMPORAL_PROBE, temporal.flatten()),
    ]
    .into_iter()
    .filter_map(|(probe, pop)| pop.map(|p| (probe, p)))
    .collect();

    if supersets.is_empty() {
        return format!(
            "{SUB_1B}: could not evaluate — neither {WARD_PROBE} nor {TEMPORAL_PROBE} carried a \
             measured population this run.\n"
        );
    }

    let listed = supersets
        .iter()
        .map(|(probe, pop)| format!("{probe}={pop}"))
        .collect::<Vec<_>>()
        .join(", ");
    let largest = supersets
        .iter()
        .map(|(_, pop)| *pop)
        .max()
        .expect("supersets is non-empty, checked above");

    if identity_pop > largest {
        format!(
            "{SUB_1B}: CONTRADICTION — identity-integrity measured {identity_pop}, exceeding \
             {listed} — a planning-filtered subset can never exceed its unfiltered superset.\n"
        )
    } else {
        format!(
            "{SUB_1B}: no contradiction found — identity-integrity={identity_pop} does not \
             exceed {listed}.\n"
        )
    }
}

/// Check 1 — see module doc comment for the two independent sub-comparisons.
///
/// Naming these three probes by string here is check 1's own specification (D-15/RPT-05 define
/// it in terms of exactly these three probes) — not the ad-hoc probe-name lookup table D-09 bans
/// for REMEDIATION triggers. If any of the three is ever renamed, it reads as absent and the
/// sub-checks that need it render "could not evaluate" rather than silently comparing the wrong
/// things.
pub fn check_1(entries: &[ReportEntry]) -> String {
    let ward = measured(entries, WARD_PROBE);
    let temporal = measured(entries, TEMPORAL_PROBE);
    let identity = measured(entries, IDENTITY_PROBE);

    let mut out = String::from("**Check 1** (independently-measured populations):\n");
    out.push_str(&check_1a(ward, temporal));
    out.push_str(&check_1b(ward, temporal, identity));
    out
}

/// Check 2 — attributed evidence for a source with no run marker. See module doc comment.
pub fn check_2(entries: &[ReportEntry], strategy: &Strategy) -> String {
    let Some(runs) = strategy.enrichment_runs.as_ref() else {
        return "**Check 2** (attributed evidence vs run markers): could not evaluate — no \
                adapter run markers were read this battery.\n"
            .to_string();
    };

    let mut checked: u64 = 0;
    let mut fired: Vec<String> = Vec::new();

    for entry in entries {
        for attribution in &entry.finding.attributions {
            if attribution.value == 0 {
                continue;
            }
            checked += 1;
            if !runs.contains_key(attribution.source) {
                fired.push(format!(
                    "{probe}: {value} attributed from '{source}', which carries no \
                     enrichment_runs marker this battery.",
                    probe = entry.finding.probe,
                    value = attribution.value,
                    source = attribution.source,
                ));
            }
        }
    }

    if fired.is_empty() {
        format!(
            "**Check 2** (attributed evidence vs run markers): no contradiction found — {} \
             checked, every attributed source carries a run marker.\n",
            plural(
                checked,
                "attributed evidence row",
                "attributed evidence rows"
            )
        )
    } else {
        let mut out =
            String::from("**Check 2** (attributed evidence vs run markers): CONTRADICTION —\n");
        for item in fired {
            out.push_str("- ");
            out.push_str(&item);
            out.push('\n');
        }
        out
    }
}

/// Contradiction checks (RPT-05, D-15). Runs both checks unconditionally, every time — neither
/// check is ever silently omitted, and every outcome (contradiction / clean / could-not-evaluate)
/// renders as distinguishable text.
pub fn render(entries: &[ReportEntry], strategy: &Strategy) -> String {
    let mut out = String::from("## Contradiction Checks\n\n");
    out.push_str(&check_1(entries));
    // Blank line: check 1 renders its sub-checks as a Markdown list, and a bare `**Check 2**`
    // line directly after a list item is absorbed INTO that item when rendered.
    out.push('\n');
    out.push_str(&check_2(entries, strategy));
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use urban_integrity_schema::types::verdict::PopulationSource;

    use urban_integrity_core::persistence::scan::RunMarker;
    use urban_integrity_schema::types::verdict::{Finding, Severity, SourceAttribution, Verdict};

    use super::*;

    use crate::MotifKind;

    fn finding_with_population(probe: &str, population: u64) -> Finding {
        Finding {
            probe: probe.to_string(),
            verdict: Verdict::classify(population, population, 0, u64::MAX, ""),
            summary: format!("{probe} summary."),
            cause: None,
            severity: Severity::Info,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        }
    }

    fn entry(finding: Finding) -> ReportEntry {
        ReportEntry {
            tier: "Tier-1",
            hypothesis: "synthetic hypothesis",
            motif_kind: MotifKind::Absence,
            finding,
            population_source: PopulationSource::SelfReported {
                reason: "test fixture",
            },
        }
    }

    #[test]
    fn check_1_ward_and_temporal_disagreement_names_both_probes_and_figures() {
        let entries = vec![
            entry(finding_with_population(WARD_PROBE, 100)),
            entry(finding_with_population(TEMPORAL_PROBE, 90)),
            entry(finding_with_population(IDENTITY_PROBE, 50)),
        ];
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(rendered.contains("CONTRADICTION"), "got:\n{rendered}");
        assert!(rendered.contains(WARD_PROBE), "got:\n{rendered}");
        assert!(rendered.contains(TEMPORAL_PROBE), "got:\n{rendered}");
        assert!(rendered.contains("100"), "got:\n{rendered}");
        assert!(rendered.contains("90"), "got:\n{rendered}");
    }

    #[test]
    fn check_1_identity_exceeding_equal_superset_is_a_distinguishable_contradiction() {
        let entries = vec![
            entry(finding_with_population(WARD_PROBE, 100)),
            entry(finding_with_population(TEMPORAL_PROBE, 100)),
            entry(finding_with_population(IDENTITY_PROBE, 150)),
        ];
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(rendered.contains("CONTRADICTION"), "got:\n{rendered}");
        assert!(
            rendered.contains("exceeding ward-concentration=100, temporal-burst=100"),
            "the subset-exceeds-superset case must be distinguishable text from the \
             ward-vs-temporal disagreement case, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("must agree"),
            "1a must not also fire — ward and temporal agree here, got:\n{rendered}"
        );
    }

    #[test]
    fn check_1_all_three_probes_absent_states_could_not_evaluate() {
        let entries = vec![entry(finding_with_population("some-other-probe", 5))];
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(rendered.contains("could not evaluate"), "got:\n{rendered}");
        assert!(
            !rendered.contains("no contradiction found — ward-concentration"),
            "must not falsely claim clean when the inputs are missing, got:\n{rendered}"
        );
    }

    #[test]
    fn check_1_equal_and_non_exceeding_states_no_contradiction_distinguishable_from_could_not_evaluate(
    ) {
        let entries = vec![
            entry(finding_with_population(WARD_PROBE, 100)),
            entry(finding_with_population(TEMPORAL_PROBE, 100)),
            entry(finding_with_population(IDENTITY_PROBE, 60)),
        ];
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(
            rendered.contains("no contradiction found — ward-concentration=100"),
            "got:\n{rendered}"
        );
        assert!(
            !rendered.contains("could not evaluate — ward-concentration"),
            "clean and could-not-evaluate must render distinguishable text, got:\n{rendered}"
        );
    }

    /// The defect this sub-check split exists to fix: `identity-integrity` reporting `DataGap`
    /// used to abort 1a as well, so on the live corpus — where it always does — the
    /// ward-vs-temporal comparison never ran. 1a must reach its comparison regardless.
    #[test]
    fn check_1a_still_evaluates_when_identity_integrity_carries_no_population() {
        let mut entries = vec![
            entry(finding_with_population(WARD_PROBE, 105_613)),
            entry(finding_with_population(TEMPORAL_PROBE, 105_613)),
            entry(finding_with_population(IDENTITY_PROBE, 0)),
        ];
        entries[2].finding.verdict = Verdict::data_gap("no _catalog baseline row");
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(
            rendered.contains(
                "**1a** (ward-concentration vs temporal-burst): no contradiction \
                 found — ward-concentration=105613, temporal-burst=105613"
            ),
            "1a must compare its own two figures even though identity-integrity measured \
             nothing, got:\n{rendered}"
        );
        assert!(
            rendered.contains(
                "**1b** (identity-integrity vs its unfiltered superset): could not evaluate"
            ),
            "1b must independently report that it could not evaluate, got:\n{rendered}"
        );
    }

    /// The converse: 1b still evaluates when a superset probe is missing, comparing against
    /// whichever superset figure IS available.
    #[test]
    fn check_1b_still_evaluates_against_one_available_superset() {
        let entries = vec![
            entry(finding_with_population(TEMPORAL_PROBE, 100)),
            entry(finding_with_population(IDENTITY_PROBE, 150)),
        ];
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(
            rendered.contains("**1b**") && rendered.contains("CONTRADICTION"),
            "1b must fire against the one available superset, got:\n{rendered}"
        );
        assert!(
            rendered.contains(
                "**1a** (ward-concentration vs temporal-burst): could not \
                 evaluate — ward-concentration absent from this battery"
            ),
            "1a must say which probe was absent, got:\n{rendered}"
        );
    }

    #[test]
    fn check_2_attribution_with_no_run_marker_names_source_and_value() {
        let mut finding = finding_with_population("council-registers-political-actors", 5);
        finding.attributions = vec![SourceAttribution {
            source: "council_registers",
            value: 5,
        }];
        let entries = vec![entry(finding)];
        // No "council_registers" key present.
        let strategy = Strategy {
            enrichment_runs: Some(HashMap::new()),
            ..Strategy::default()
        };

        let rendered = render(&entries, &strategy);

        assert!(rendered.contains("CONTRADICTION"), "got:\n{rendered}");
        assert!(rendered.contains("council_registers"), "got:\n{rendered}");
        assert!(rendered.contains('5'), "got:\n{rendered}");
    }

    #[test]
    fn check_2_every_attribution_marked_states_no_contradiction() {
        let mut finding = finding_with_population("council-registers-political-actors", 5);
        finding.attributions = vec![SourceAttribution {
            source: "council_registers",
            value: 5,
        }];
        let entries = vec![entry(finding)];
        let mut runs = HashMap::new();
        runs.insert(
            "council_registers".to_string(),
            RunMarker {
                source: "council_registers".to_string(),
                last_run_at: 0,
                candidates_found: 0,
                auto_linked: 0,
                pending_review: 0,
                discarded: 0,
            },
        );
        let strategy = Strategy {
            enrichment_runs: Some(runs),
            ..Strategy::default()
        };

        let rendered = render(&entries, &strategy);

        assert!(
            rendered.contains("no contradiction found"),
            "got:\n{rendered}"
        );
        assert!(
            rendered.contains("checked"),
            "must explicitly state a checked count rather than silence, got:\n{rendered}"
        );
    }

    #[test]
    fn check_2_no_run_markers_read_states_could_not_evaluate_not_a_false_positive() {
        let mut finding = finding_with_population("council-registers-political-actors", 5);
        finding.attributions = vec![SourceAttribution {
            source: "council_registers",
            value: 5,
        }];
        let entries = vec![entry(finding)];
        let strategy = Strategy::default(); // enrichment_runs is None.

        let rendered = render(&entries, &strategy);

        assert!(rendered.contains("could not evaluate"), "got:\n{rendered}");
        assert!(
            !rendered.contains("CONTRADICTION"),
            "None must not be treated as 'assume every source is markerless' — that would be \
             exactly the cry-wolf false positive check 1's redesign avoids, got:\n{rendered}"
        );
    }

    #[test]
    fn render_never_returns_query_calls() {
        let src = include_str!("contradictions.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields at least one segment");
        assert!(
            !production.contains(".query("),
            "contradictions.rs must issue no DB read — both checks read only \
             entries/strategy, already in hand"
        );
    }
}
