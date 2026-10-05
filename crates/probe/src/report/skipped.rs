//! D-19's dedicated Skipped-probes section: every probe this run's gate predicate skipped, with
//! its typed `SkipCause` rendered via `SkipCause::describe()` — never re-derived from `summary`
//! prose (matches this codebase's D-05 precedent: assert/render on the typed cause, not a
//! substring).
//!
//! Deliberately the ONE home for `Verdict::Skipped` findings: they are NOT added to
//! `DETAIL_GROUPS` (`report/mod.rs`) — a `Skipped` finding has no "what was measured" summary the
//! way a `Hit`/`Weak`/`Clear`/`NotApplicable`/`DataGap`/`Blocked` finding does (it was never run
//! at all), so a bare `DETAIL_GROUPS` row would render an empty or misleading summary paragraph.
//! This section always renders — even with zero skips — so the reader never has to infer its
//! absence meant "not applicable" vs "forgot to render" (GAT-05).

use super::ReportEntry;
use crate::plural;
use urban_integrity_schema::types::verdict::Verdict;

/// Renders "## Skipped Probes" plus one bullet per skipped entry: `probe name — cause.describe()`.
/// Renders an explicit "No probes were skipped" statement when the run was fully ungated or every
/// probe passed its gate — the section always exists so the reader never has to infer its absence
/// meant "not applicable" vs "forgot to render".
pub fn render(entries: &[ReportEntry]) -> String {
    let skipped: Vec<&ReportEntry> = entries
        .iter()
        .filter(|e| matches!(e.finding.verdict, Verdict::Skipped { .. }))
        .collect();

    let mut out = String::new();
    out.push_str("## Skipped Probes\n\n");

    if skipped.is_empty() {
        out.push_str("No probes were skipped in this run.\n");
        return out;
    }

    out.push_str(&format!(
        "{} skipped by the gate predicate:\n\n",
        plural(skipped.len() as u64, "probe", "probes")
    ));

    for entry in &skipped {
        let Verdict::Skipped { cause } = &entry.finding.verdict else {
            unreachable!("filtered to Verdict::Skipped above");
        };
        out.push_str(&format!(
            "- {} — {}\n",
            entry.finding.probe,
            cause.describe()
        ));
    }

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
    fn renders_both_probe_names_and_both_causes_describe_text() {
        let upstream = Verdict::classify(10, 10, 0, 1, "");
        let entries = vec![
            entry(
                "dependency-skipped",
                Verdict::skipped_dependency("identity-integrity", &upstream),
            ),
            entry(
                "tier-aggregate-skipped",
                Verdict::skipped_tier_aggregate("Tier-1", 7),
            ),
        ];

        let rendered = render(&entries);

        assert!(
            rendered.contains("dependency-skipped"),
            "expected the Dependency-caused skip's probe name in:\n{rendered}"
        );
        assert!(
            rendered.contains("upstream 'identity-integrity' returned Clear"),
            "expected the Dependency-caused skip's cause description in:\n{rendered}"
        );
        assert!(
            rendered.contains("tier-aggregate-skipped"),
            "expected the TierAggregate-caused skip's probe name in:\n{rendered}"
        );
        assert!(
            rendered.contains("no Tier-1 probe (of 7 checked) returned Hit or Weak"),
            "expected the TierAggregate-caused skip's cause description in:\n{rendered}"
        );
    }

    #[test]
    fn zero_skipped_entries_renders_an_explicit_no_probes_skipped_statement() {
        let entries = vec![entry("hit-1", Verdict::classify(10, 10, 5, 1, ""))];

        let rendered = render(&entries);

        assert!(!rendered.is_empty(), "the section must always render");
        assert!(
            rendered.contains("No probes were skipped"),
            "expected an explicit no-skips statement, not an empty/absent section, in:\n{rendered}"
        );
    }
}
