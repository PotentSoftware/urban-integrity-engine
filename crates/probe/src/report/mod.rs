//! Report-writer module skeleton (Phase 35 plan 35-09).
//!
//! This plan establishes every section function's FINAL signature so Wave 3 (plans 35-10/35-11/
//! 35-12) can each independently fill in real bodies without touching each other's files or
//! renegotiating a shape, and so Wave 4's `run.rs` wiring plan (35-13) can depend only on
//! `render_evidence_report`'s signature, not on knowing what any section actually says.
//!
//! `pub mod` declarations for the 7 section modules, `render_evidence_report`'s real
//! assembly/ordering logic, and the smoke test are added in this same plan's later tasks —
//! `ReportEntry` and its `Deref` impl are the piece this first task needs in place so the 7 stub
//! files can reference it in their own signatures.

pub mod adapted;
pub mod bluf;
pub mod contradictions;
pub mod evidence;
pub mod ledger;
pub mod legend;
pub mod remediation;
pub mod skipped;

use urban_integrity_schema::types::verdict::{Finding, PopulationSource, Verdict};

use super::{MotifKind, Strategy};

/// Pairs each [`Finding`] with its producing probe's `tier()`/`hypothesis()` — the only place
/// those per-probe-constant values are visible outside the `Probe` trait itself, since D-01
/// locked them OFF of `Finding` (they are per-probe constants, not per-finding data; see
/// `crates/cli/src/commands/probe/mod.rs`'s `Probe` trait doc comments).
///
/// Built once at battery-dispatch time (Wave 4, plan 35-13, inside `run.rs::execute`, which
/// already holds both the `probes: Vec<Box<dyn Probe>>` list and the `Vec<Finding>` `run_probes`
/// returns, in the same 1:1 order) — every report section function operates on `&[ReportEntry]`,
/// never on a bare `&[Finding]`, so tier/hypothesis are always available without a probe-name
/// lookup table anywhere in the report module (which would reproduce D-09's forbidden pattern in
/// a new place).
#[derive(Debug)]
pub struct ReportEntry {
    /// This entry's producing probe's tier — exactly `"Tier-1"` or `"Tier-2"`.
    pub tier: &'static str,
    /// This entry's producing probe's one-sentence hypothesis.
    pub hypothesis: &'static str,
    /// This entry's producing probe's motif classification (D-06, phase 50) — what shape the
    /// finding board draws this probe's card in. A per-probe constant, computed once in
    /// `run.rs::execute()` from `Probe::motif_kind()`, same as `tier`/`hypothesis` above.
    pub motif_kind: MotifKind,
    /// The finding itself.
    pub finding: Finding,
    /// VRD-11/VRD-13: where this entry's population figure comes from — independently
    /// recomputed by the oracle, or the probe's own self-report. Computed unconditionally in
    /// `run.rs::execute()` for every probe (PD-03), rendered in the Ledger's Provenance column
    /// and, for `Clear` findings, as a per-entry sentence in the detail group (PD-06).
    pub population_source: PopulationSource,
}

impl std::ops::Deref for ReportEntry {
    type Target = Finding;

    /// Lets any code (including future plans) that only needs `Finding`'s own fields (`.probe`,
    /// `.verdict`, `.summary`, `.severity`, `.evidence`, ...) read them directly off a
    /// `&ReportEntry` via deref coercion without an explicit `.finding` — reserve the explicit
    /// `.finding` field access for constructing a NEW owned `Finding` value or moving it out.
    fn deref(&self) -> &Finding {
        &self.finding
    }
}

/// Predicate selecting the entries a [`DETAIL_GROUPS`] heading owns. A plain `fn` pointer rather
/// than a closure so the table can be a `const`.
///
/// `pub` (Phase 43 plan 05, D-15): `crates/workbench::report_view::from_battery` iterates this
/// SAME table to build `ReportView`'s 6 groups, so the two partitions can never independently
/// drift apart.
pub type InGroup = fn(&Verdict) -> bool;

/// The per-verdict detail groups, in render order — every one of the six pre-GAT-05 `Verdict`
/// variants is owned by exactly one heading, so no finding's prose can fall out of the document.
///
/// `Clear` was originally the one variant with no group. A clean result then survived only as a
/// row in the Full Probe Ledger: the verdict and the counts, never the summary explaining what
/// was measured to reach it. That drops precisely the reasoning a reader needs to tell "checked
/// and found nothing" apart from "could not check" — the distinction D-07's BLUF ordering exists
/// to protect — so `Clear` renders its own group like every other verdict.
///
/// `Verdict::Skipped` (GAT-05, the seventh variant) is DELIBERATELY NOT a row here — a skipped
/// probe never ran, so there is no "what was measured" summary to render alongside a heading the
/// way every other variant has. Its one dedicated home is `skipped::render`, called directly
/// after the legend and before this loop (see `render_evidence_report` below); the non-vacuous
/// `every_verdict_variant_renders_in_detail_groups_or_the_skipped_section` test proves every one
/// of the 7 variants lands in one of these two homes, never neither.
///
/// Order is most-actionable first, verified-clean last. D-05 fixes the placement of the BLUF and
/// the legend only; ordering within the detail set is this module's own choice.
pub const DETAIL_GROUPS: [(&str, InGroup); 5] = [
    ("Confirmed Findings", |v| matches!(v, Verdict::Hit { .. })),
    ("Suggestive Patterns", |v| matches!(v, Verdict::Weak { .. })),
    ("Data-Integrity Gaps", |v| {
        matches!(v, Verdict::DataGap { .. } | Verdict::Blocked { .. })
    }),
    ("Not Applicable", |v| {
        matches!(v, Verdict::NotApplicable { .. })
    }),
    ("Verified Negative Findings", |v| {
        matches!(v, Verdict::Clear(_))
    }),
];

/// Assembles the full evidence report: Header -> `bluf::render` -> `legend::render` ->
/// `skipped::render` (GAT-05/D-19 — surfaced early, alongside the BLUF and Legend, not buried
/// after the findings a reader may stop reading at) -> `adapted::render` -> the
/// [`DETAIL_GROUPS`] per-verdict detail groups (built INLINE here by filtering `entries`, never
/// bucketing into a `HashMap`, per D-08's anti-pattern rule) -> `ledger::render` ->
/// `contradictions::render` -> `remediation::render` (the pattern notes' Architecture Patterns
/// diagram, D-05's load-bearing set).
///
/// Each detail entry leads with its probe name and that probe's `hypothesis()`, carried here on
/// [`ReportEntry`]. D-01 kept `hypothesis()` on the `Probe` trait so the report could state what
/// each probe was testing; before this it reached the report writer and was rendered nowhere, so
/// a reader met a bare summary paragraph with no way to tell which probe produced it or what
/// question it answers.
///
/// The header's timestamp line starts with the literal prefix `"Generated: "` so Wave 5's
/// twice-run diff test can strip exactly that line before comparing.
pub fn render_evidence_report(entries: &[ReportEntry], strategy: &Strategy) -> String {
    let mut out = String::new();

    // 1. Header — timestamp via chrono::Utc::now().
    out.push_str("# UrbanIntegrity Evidence Report\n\n");
    out.push_str(&format!(
        "Generated: {}\n\n",
        chrono::Utc::now().to_rfc3339()
    ));

    // 2. Bottom Line Up Front (D-07).
    out.push_str(&bluf::render(entries));
    out.push('\n');

    // 3. Verdict Legend (D-11, RPT-03).
    out.push_str(&legend::render());
    out.push('\n');

    // 4. Skipped Probes (GAT-05, D-19) — directly after the legend, before any findings a
    // reader may stop reading at.
    out.push_str(&skipped::render(entries));
    out.push('\n');

    // 5. How the Search Adapted (D-06).
    out.push_str(&adapted::render(entries, strategy));
    out.push('\n');

    // 6-10. Per-verdict detail groups — iterate-and-filter over `entries` directly, never
    // collected into an intermediate HashMap (D-08).
    for (title, in_group) in DETAIL_GROUPS {
        out.push_str(&format!("## {title}\n\n"));
        for entry in entries.iter().filter(|e| in_group(&e.finding.verdict)) {
            out.push_str(&format!("### {}\n\n", entry.finding.probe));
            out.push_str(&format!("> **Hypothesis:** {}\n\n", entry.hypothesis));
            out.push_str(&entry.finding.summary);
            out.push('\n');
            out.push_str(&evidence::render_evidence(&entry.finding));
            out.push('\n');
            // VRD-11 (PD-06's depth half): a Clear over some population is a materially
            // different claim depending on whether anything independently checked that
            // population. The Ledger's compact Provenance cell is easy to skim past; this
            // sentence sits directly under the summary the reader is already reading, only in
            // the group where the TD-v7-01 hazard lives.
            if title == "Verified Negative Findings" {
                match &entry.population_source {
                    PopulationSource::OracleCorroborated { oracle_count } => {
                        out.push_str(&format!(
                            "> **Population source:** oracle-corroborated — independently \
                             recomputed population = {oracle_count}.\n\n"
                        ));
                    }
                    PopulationSource::SelfReported { reason } if !reason.is_empty() => {
                        out.push_str(&format!(
                            "> **Population source:** self-reported only, not independently \
                             checked — {reason}\n\n"
                        ));
                    }
                    PopulationSource::SelfReported { .. } => {
                        out.push_str(
                            "> **Population source:** self-reported only, not independently \
                             checked.\n\n",
                        );
                    }
                }
            }
        }
    }

    // 11. Full Probe Ledger (RPT-04).
    out.push_str(&ledger::render(entries));
    out.push('\n');

    // 12. Contradiction checks (RPT-05, D-15).
    out.push_str(&contradictions::render(entries, strategy));
    out.push('\n');

    // 13. Recommended Remediation (D-09/D-12).
    out.push_str(&remediation::render(entries, strategy));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::verdict::Severity;

    /// Proves the assembly wiring is live end-to-end: every one of the 8 section functions is
    /// actually CALLED by `render_evidence_report`, not merely compiled alongside it (a section
    /// that is never invoked — e.g. a dropped `legend::render()` call — fails this test).
    ///
    /// Every section is a pure function of its arguments (D-09), so calling one directly here
    /// yields exactly the text `render_evidence_report` embeds for the same inputs — which is
    /// why each section's own output is the assertion. Plan 35-09 originally asserted on its
    /// placeholder `[STUB: ...]` marker strings; that made the test fail by construction the
    /// moment Wave 3 (plans 35-10/35-11/35-12) replaced any stub with a real body, in a file
    /// none of those three plans owns. Keying on each section's actual return value instead
    /// holds for stub and real bodies alike, so it needs no edit as the bodies land.
    ///
    /// This asserts only that each section is REACHED. What each section should SAY is covered
    /// by the sections' own unit tests and by Wave 5's e2e suite (plans 35-15..35-18).
    #[test]
    fn render_evidence_report_runs_every_section_against_a_synthetic_fixture() {
        let entries = vec![
            ReportEntry {
                tier: "Tier-1",
                hypothesis: "A synthetic Hit hypothesis.",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "synthetic-hit".to_string(),
                    verdict: Verdict::classify(10, 10, 3, 1, ""),
                    summary: "A synthetic confirmed finding.".to_string(),
                    cause: None,
                    severity: Severity::High,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "synthetic test fixture",
                },
            },
            ReportEntry {
                tier: "Tier-2",
                hypothesis: "A synthetic DataGap hypothesis.",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "synthetic-gap".to_string(),
                    verdict: Verdict::data_gap("nothing to measure"),
                    summary: "A synthetic data gap.".to_string(),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "synthetic test fixture",
                },
            },
            // VRD-11's report-visible distinction, both sides, on the literal TD-v7-01 hazard
            // shape (Clear over population 0):
            ReportEntry {
                tier: "Tier-1",
                hypothesis: "A synthetic corroborated-Clear hypothesis.",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "synthetic-clear-corroborated".to_string(),
                    verdict: Verdict::classify(0, 0, 0, 1, ""),
                    summary: "A synthetic corroborated clear.".to_string(),
                    cause: None,
                    severity: Severity::Info,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::OracleCorroborated { oracle_count: 0 },
            },
            ReportEntry {
                tier: "Tier-2",
                hypothesis: "A synthetic self-reported-Clear hypothesis.",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "synthetic-clear-self-reported".to_string(),
                    verdict: Verdict::classify(0, 0, 0, 1, ""),
                    summary: "A synthetic self-reported clear.".to_string(),
                    cause: None,
                    severity: Severity::Info,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "identity_links-sourced, oracle cannot check",
                },
            },
            // GAT-05/D-19: a Skipped entry, so skipped::render's assertion below is
            // non-vacuous.
            ReportEntry {
                tier: "Tier-2",
                hypothesis: "A synthetic Skipped hypothesis.",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "synthetic-skipped".to_string(),
                    verdict: Verdict::skipped_tier_aggregate("Tier-1", 7),
                    summary: "A synthetic skipped probe.".to_string(),
                    cause: None,
                    severity: Severity::Info,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "synthetic test fixture",
                },
            },
        ];
        let strategy = Strategy::default();

        let report = render_evidence_report(&entries, &strategy);

        assert!(!report.is_empty(), "report must not be empty");

        for (section, rendered) in [
            ("bluf::render", bluf::render(&entries)),
            ("legend::render", legend::render()),
            ("skipped::render", skipped::render(&entries)),
            ("adapted::render", adapted::render(&entries, &strategy)),
            ("ledger::render", ledger::render(&entries)),
            (
                "contradictions::render",
                contradictions::render(&entries, &strategy),
            ),
            (
                "remediation::render",
                remediation::render(&entries, &strategy),
            ),
        ] {
            assert!(
                !rendered.is_empty(),
                "{section} returned an empty string — it cannot be detected in the assembled \
                 report, so this test could not prove it was called"
            );
            assert!(
                report.contains(&rendered),
                "{section}'s output is absent from the assembled report — that section was \
                 never called by render_evidence_report; got:\n{report}"
            );
        }

        // `evidence::render_evidence` is the one per-finding section: it is called once inside
        // each of the four verdict detail groups, so it is asserted per entry rather than once.
        for entry in &entries {
            let rendered = evidence::render_evidence(&entry.finding);
            assert!(
                !rendered.is_empty(),
                "evidence::render_evidence returned an empty string for probe {}",
                entry.finding.probe
            );
            assert!(
                report.contains(&rendered),
                "evidence::render_evidence output for probe {} is absent from the assembled \
                 report; got:\n{report}",
                entry.finding.probe
            );
        }

        // VRD-11: both provenance sentences render, distinctly, in the assembled report.
        assert!(
            report.contains("oracle-corroborated — independently recomputed population = 0"),
            "the corroborated Clear's provenance sentence must render, got:\n{report}"
        );
        assert!(
            report.contains(
                "self-reported only, not independently checked — identity_links-sourced, \
                 oracle cannot check"
            ),
            "the self-reported Clear's provenance sentence must render with its reason, \
             got:\n{report}"
        );
    }

    /// The concrete fix for the research notes' Pitfall 1's silent-gap risk: `DETAIL_GROUPS` and
    /// `skipped::render` are both fixed, hand-maintained homes for a `Verdict`'s prose, not a
    /// compiler-checked exhaustive match — adding an 8th `Verdict` variant in the future would
    /// NOT fail to compile at either site. This test proves, by presence rather than by trusting
    /// either home's own doc comment, that every one of the 7 variants constructible today lands
    /// in one of the two homes: builds one `ReportEntry` per variant with a distinct, unique
    /// probe name, renders the full report, and asserts each probe name appears in EXACTLY ONE
    /// of the two prose homes.
    ///
    /// It asserts on the homes' own markers -- `### {probe}` for a `DETAIL_GROUPS` entry and
    /// `- {probe} -- ` for a `skipped::render` bullet -- never on whole-report containment.
    /// Containment alone is vacuous: `ledger::render` emits a `| ... | {probe} | ... |` row for
    /// every entry regardless of verdict, so `report.contains(probe)` holds even with
    /// `skipped::render` deleted and every `DETAIL_GROUPS` row dropped. The exactly-one form
    /// also catches the opposite drift, a variant acquiring a second home and rendering twice.
    #[test]
    fn every_verdict_variant_renders_in_detail_groups_or_the_skipped_section() {
        let clear_upstream = Verdict::classify(10, 10, 0, 1, "");

        let entries = vec![
            ReportEntry {
                tier: "Tier-1",
                hypothesis: "variant fixture: Hit",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "variant-fixture-hit".to_string(),
                    verdict: Verdict::classify(10, 10, 5, 1, ""),
                    summary: "variant fixture Hit summary".to_string(),
                    cause: None,
                    severity: Severity::High,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "variant fixture",
                },
            },
            ReportEntry {
                tier: "Tier-1",
                hypothesis: "variant fixture: Weak",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "variant-fixture-weak".to_string(),
                    verdict: Verdict::classify(10, 10, 3, 5, ""),
                    summary: "variant fixture Weak summary".to_string(),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "variant fixture",
                },
            },
            ReportEntry {
                tier: "Tier-1",
                hypothesis: "variant fixture: Clear",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "variant-fixture-clear".to_string(),
                    verdict: Verdict::classify(10, 10, 0, 1, ""),
                    summary: "variant fixture Clear summary".to_string(),
                    cause: None,
                    severity: Severity::Info,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "variant fixture",
                },
            },
            ReportEntry {
                tier: "Tier-1",
                hypothesis: "variant fixture: NotApplicable",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "variant-fixture-not-applicable".to_string(),
                    verdict: Verdict::classify(10, 7, 0, 1, "3 structurally excluded"),
                    summary: "variant fixture NotApplicable summary".to_string(),
                    cause: None,
                    severity: Severity::Info,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "variant fixture",
                },
            },
            ReportEntry {
                tier: "Tier-2",
                hypothesis: "variant fixture: DataGap",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "variant-fixture-data-gap".to_string(),
                    verdict: Verdict::data_gap("missing input"),
                    summary: "variant fixture DataGap summary".to_string(),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "variant fixture",
                },
            },
            ReportEntry {
                tier: "Tier-2",
                hypothesis: "variant fixture: Blocked",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "variant-fixture-blocked".to_string(),
                    verdict: Verdict::blocked("unmet prerequisite"),
                    summary: "variant fixture Blocked summary".to_string(),
                    cause: None,
                    severity: Severity::Medium,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "variant fixture",
                },
            },
            ReportEntry {
                tier: "Tier-2",
                hypothesis: "variant fixture: Skipped",
                motif_kind: MotifKind::Absence,
                finding: Finding {
                    probe: "variant-fixture-skipped".to_string(),
                    verdict: Verdict::skipped_dependency("upstream-probe", &clear_upstream),
                    summary: "variant fixture Skipped summary".to_string(),
                    cause: None,
                    severity: Severity::Info,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                },
                population_source: PopulationSource::SelfReported {
                    reason: "variant fixture",
                },
            },
        ];
        let strategy = Strategy::default();

        let report = render_evidence_report(&entries, &strategy);

        for entry in &entries {
            // The two prose homes' own markers. Deliberately NOT `report.contains(probe)`:
            // `ledger::render` emits a row naming every probe whatever its verdict, so plain
            // containment cannot fail and would prove nothing.
            let detail_home = format!("### {}\n", entry.finding.probe);
            let skipped_home = format!("- {} — ", entry.finding.probe);
            let homes = usize::from(report.contains(&detail_home))
                + usize::from(report.contains(&skipped_home));

            assert_eq!(
                homes, 1,
                "probe {} (verdict {:?}) renders in {homes} prose homes, expected exactly 1 — \
                 0 means it landed in neither DETAIL_GROUPS nor skipped::render, 2 means it \
                 renders twice; got:\n{report}",
                entry.finding.probe, entry.finding.verdict
            );
        }
    }
}
