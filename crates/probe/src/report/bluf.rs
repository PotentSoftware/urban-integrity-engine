//! D-07's Bottom Line Up Front — derived, leads with structural blindness: how many probes could
//! not evaluate (`DataGap` + `Blocked`) and how much of the evaluated population was structurally
//! incapable (aggregate `population` vs `capable`), before any HIT count.
//!
//! A reader who stops at this section must never be able to mistake "structurally couldn't check"
//! for "checked and found nothing" — Python's hardcoded no-hits paragraph
//! (`analyst_probe.py:2615-2621`) made exactly that mistake and is deleted here, not corrected
//! (D-07).

use crate::plural;

use super::ReportEntry;

use urban_integrity_schema::types::verdict::Verdict;

/// The six raw numbers [`render`]'s four BLUF sentences are formatted from — extracted so
/// `crates/workbench::report_view::from_battery` (D-15) can build `BlufView` from the SAME
/// computation `render` itself uses, rather than re-deriving the filter/sum logic at a second
/// call site where the two could silently drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlufNumbers {
    pub could_not_evaluate: u64,
    pub total_probes: u64,
    pub skipped: u64,
    pub population: u64,
    pub capable: u64,
    pub hit_count: u64,
}

/// The exact filter/sum logic `render` below formats into prose — the sole producer both
/// `render` and any other caller (e.g. `crates/workbench::report_view`) must go through.
pub fn compute(entries: &[ReportEntry]) -> BlufNumbers {
    let total_probes = entries.len() as u64;
    let could_not_evaluate = entries
        .iter()
        .filter(|e| {
            matches!(
                e.finding.verdict,
                Verdict::DataGap { .. } | Verdict::Blocked { .. }
            )
        })
        .count() as u64;
    let skipped = entries
        .iter()
        .filter(|e| matches!(e.finding.verdict, Verdict::Skipped { .. }))
        .count() as u64;
    let population: u64 = entries.iter().filter_map(|e| e.population()).sum();
    let capable: u64 = entries.iter().filter_map(|e| e.capable()).sum();
    let hit_count = entries
        .iter()
        .filter(|e| matches!(e.finding.verdict, Verdict::Hit { .. }))
        .count() as u64;

    BlufNumbers {
        could_not_evaluate,
        total_probes,
        skipped,
        population,
        capable,
        hit_count,
    }
}

/// Bottom Line Up Front. Reads only `Finding`-level fields via `ReportEntry`'s `Deref` — does not
/// need `tier`/`hypothesis`.
///
/// Renders, in this fixed order, from [`compute`]'s numbers:
/// 1. How many of the total probes could not evaluate at all (`DataGap` + `Blocked`).
/// 2. How many probes the gate predicate skipped entirely (`Skipped`, GAT-05/D-19) — a gated
///    run's shorter report must never be mistaken for a cleaner one.
/// 3. The aggregate structurally-capable population vs the aggregate population evaluated.
/// 4. Only then, how many confirmed (`Hit`) findings exist.
pub fn render(entries: &[ReportEntry]) -> String {
    let n = compute(entries);

    let mut out = String::new();
    out.push_str("## Bottom Line Up Front\n\n");

    // 1. Structural blindness FIRST — before any hit count.
    out.push_str(&format!(
        "{} of {} could not evaluate at all (data gap or blocked).\n\n",
        plural(n.could_not_evaluate, "probe", "probes"),
        n.total_probes
    ));

    // 2. Gate-predicate skip count (GAT-05/D-19) — always stated, even when zero, so an
    // ungated run or a run where nothing was skipped says so explicitly rather than omitting
    // the sentence.
    out.push_str(&format!(
        "{} skipped by gating (see the Skipped Probes section for why).\n\n",
        plural(n.skipped, "probe", "probes")
    ));

    // 3. Aggregate population vs capable — before any hit count.
    out.push_str(&format!(
        "Across every probe that did run, {} of {} records evaluated were structurally capable \
         of producing a finding.\n\n",
        plural(n.capable, "record", "records"),
        n.population
    ));

    // 4. Only now, the confirmed hit count.
    out.push_str(&format!(
        "{} confirmed.\n",
        plural(n.hit_count, "finding", "findings")
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MotifKind;
    use urban_integrity_schema::types::verdict::PopulationSource;
    use urban_integrity_schema::types::verdict::{Finding, Severity};

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
            motif_kind: MotifKind::Absence,
            finding: finding(probe, verdict),
            population_source: PopulationSource::SelfReported {
                reason: "test fixture",
            },
        }
    }

    #[test]
    fn structural_blindness_precedes_hit_count() {
        let entries = vec![
            entry("gap-1", Verdict::data_gap("no input")),
            entry("gap-2", Verdict::data_gap("no input")),
            entry("blocked-1", Verdict::blocked("prereq unmet")),
            entry("hit-1", Verdict::classify(10, 10, 5, 1, "")),
            entry("hit-2", Verdict::classify(10, 10, 5, 1, "")),
            entry("hit-3", Verdict::classify(10, 10, 5, 1, "")),
        ];

        let rendered = render(&entries);

        let blindness_pos = rendered
            .find("could not evaluate")
            .expect("blindness sentence must be present");
        let hit_pos = rendered
            .find("confirmed")
            .expect("hit-count sentence must be present");

        assert!(
            blindness_pos < hit_pos,
            "structural blindness must be stated before the hit count; got:\n{rendered}"
        );

        // 2 DataGap + 1 Blocked = 3.
        assert!(
            rendered.contains('3'),
            "expected the blind count (3) in:\n{rendered}"
        );
    }

    #[test]
    fn aggregate_population_and_capable_are_stated_and_distinct_when_they_differ() {
        let entries = vec![
            entry("cov-1", Verdict::classify(20, 15, 2, 1, "5 excluded")),
            entry("cov-2", Verdict::classify(10, 10, 0, 1, "")),
        ];

        let rendered = render(&entries);

        // Aggregate population = 30, aggregate capable = 25 — distinct, both must appear.
        assert!(
            rendered.contains("30"),
            "expected aggregate population 30 in:\n{rendered}"
        );
        assert!(
            rendered.contains("25"),
            "expected aggregate capable 25 in:\n{rendered}"
        );
    }

    #[test]
    fn empty_entries_does_not_panic_and_states_zero_evaluated() {
        let entries: Vec<ReportEntry> = vec![];
        let rendered = render(&entries);
        assert!(!rendered.is_empty());
        assert!(
            rendered.contains("0 probes"),
            "expected a zero-probes statement in:\n{rendered}"
        );
    }

    #[test]
    fn skip_count_is_stated_before_the_hit_count_sentence() {
        let upstream = Verdict::classify(10, 10, 0, 1, "");
        let entries = vec![
            entry(
                "skipped-1",
                Verdict::skipped_dependency("identity-integrity", &upstream),
            ),
            entry("skipped-2", Verdict::skipped_tier_aggregate("Tier-1", 7)),
            entry("hit-1", Verdict::classify(10, 10, 5, 1, "")),
        ];

        let rendered = render(&entries);

        assert!(
            rendered.contains("2 probes skipped by gating"),
            "expected the skip-count sentence in:\n{rendered}"
        );

        let skip_pos = rendered
            .find("skipped by gating")
            .expect("skip-count sentence must be present");
        let hit_pos = rendered
            .find("confirmed")
            .expect("hit-count sentence must be present");

        assert!(
            skip_pos < hit_pos,
            "the skip-count sentence must precede the hit-count sentence; got:\n{rendered}"
        );
    }

    #[test]
    fn zero_skips_states_the_count_explicitly_rather_than_omitting_the_sentence() {
        let entries = vec![entry("hit-1", Verdict::classify(10, 10, 5, 1, ""))];

        let rendered = render(&entries);

        assert!(
            rendered.contains("0 probes skipped by gating"),
            "expected an explicit zero-skips statement in:\n{rendered}"
        );
    }

    /// `compute`'s numbers match `render`'s formatted sentence numbers on the same fixture —
    /// both derive from the same computation, so the extraction refactor is behaviour-preserving
    /// (D-15's proof surface for `crates/workbench::report_view` reusing `compute` directly).
    #[test]
    fn compute_numbers_match_render_formatted_sentence_numbers() {
        let entries = vec![
            entry("gap-1", Verdict::data_gap("no input")),
            entry("blocked-1", Verdict::blocked("prereq unmet")),
            entry("hit-1", Verdict::classify(20, 15, 5, 1, "5 excluded")),
            entry("hit-2", Verdict::classify(10, 10, 3, 1, "")),
        ];

        let n = compute(&entries);
        let rendered = render(&entries);

        assert!(rendered.contains(&format!("of {} could not evaluate", n.total_probes)));
        assert!(rendered.contains(&n.could_not_evaluate.to_string()));
        assert!(rendered.contains(&format!("{} probes skipped by gating", n.skipped)));
        assert!(rendered.contains(&format!("of {} records evaluated", n.population)));
        assert!(rendered.contains("confirmed") && rendered.contains(&n.hit_count.to_string()));
        assert_eq!(n.total_probes, 4);
        assert_eq!(n.could_not_evaluate, 2);
        assert_eq!(n.skipped, 0);
        assert_eq!(n.population, 30);
        assert_eq!(n.capable, 25);
        // hit-1 has capable (15) < population (20), so it classifies as NotApplicable, not Hit —
        // only hit-2 (capable == population) is a real Hit.
        assert_eq!(n.hit_count, 1);
    }
}
