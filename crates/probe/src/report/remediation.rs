//! D-09/D-12's `Recommended Remediation` section — a pure function of `Vec<Finding>` +
//! `Strategy`. Triggers are `Verdict` variant and `Cause` (both already typed), never a
//! Python-style regex over prose.
//!
//! Only two typed trigger sources are read here (per this plan's `<interfaces>` and D-10):
//! `entry.finding.cause` (the disqualification-chain pair) and
//! `entry.finding.remediation_hint` (ward-concentration). D-10 drops the CH-coverage item's
//! trigger entirely — no third source is added here to replace it.

use urban_integrity_schema::types::verdict::{Cause, RemediationHint};

use crate::{plural, Strategy};

use super::ReportEntry;

/// The exact `Vec<String>` of remediation item bullets [`render`] joins — extracted so
/// `crates/workbench::report_view::from_battery` (D-15) can reuse the same trigger derivation
/// without re-deriving it at a second call site.
pub fn items(entries: &[ReportEntry], strategy: &Strategy) -> Vec<String> {
    // `Strategy` carries no signal this function currently reads — the parameter is kept only
    // because plan 35-09 locked this signature for every section, including this one.
    let _ = strategy;

    let mut items: Vec<String> = Vec::new();

    for entry in entries {
        if matches!(entry.finding.cause, Some(Cause::LinksNotElevated)) {
            items.push(format!(
                "- **{probe}**: review the ENR-11 status-elevation gate holding back \
                 sub-threshold candidates — {summary}",
                probe = entry.finding.probe,
                summary = entry.finding.summary,
            ));
        }

        if let Some(RemediationHint::WardsUnattributed(n)) = entry.finding.remediation_hint {
            items.push(format!(
                "- **{probe}**: {count} lack ward attribution, limiting IMD weighting \
                 accuracy — {summary}",
                probe = entry.finding.probe,
                count = plural(n, "contract", "contracts"),
                summary = entry.finding.summary,
            ));
        }
    }

    items
}

/// Recommended Remediation (D-09/D-12).
///
/// Iterates [`items`]'s list. Every triggered item states an action plus a reference to the
/// triggering finding's own `probe` name and `summary` — no independent factual assertion beyond
/// what that finding already measured (D-12): every number a reader sees comes from the finding
/// that measured it.
pub fn render(entries: &[ReportEntry], strategy: &Strategy) -> String {
    let items = items(entries, strategy);

    let mut out = String::from("## Recommended Remediation\n\n");
    if items.is_empty() {
        out.push_str(
            "No remediation items apply — no finding in this battery carried a triggering \
             Cause or RemediationHint.\n",
        );
    } else {
        for item in items {
            out.push_str(&item);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use urban_integrity_schema::types::verdict::PopulationSource;
    use urban_integrity_schema::types::verdict::{Finding, Severity, Verdict};

    use super::*;

    use crate::MotifKind;

    fn base_finding(probe: &str) -> Finding {
        Finding {
            probe: probe.to_string(),
            verdict: Verdict::classify(5, 5, 0, 1, ""),
            summary: format!("{probe} summary sentence."),
            cause: None,
            severity: Severity::Info,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        }
    }

    fn entry(finding: Finding) -> ReportEntry {
        ReportEntry {
            tier: "Tier-2",
            hypothesis: "synthetic hypothesis",
            motif_kind: MotifKind::Absence,
            finding,
            population_source: PopulationSource::SelfReported {
                reason: "test fixture",
            },
        }
    }

    #[test]
    fn links_not_elevated_cause_triggers_remediation_referencing_probe_and_enr11_gate() {
        let mut finding = base_finding("disqualified-active-in-procurement");
        finding.cause = Some(Cause::LinksNotElevated);
        finding.summary = "3 candidates excluded by the ENR-11 gate.".to_string();
        let entries = vec![entry(finding)];
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(
            rendered.contains("disqualified-active-in-procurement"),
            "must reference the triggering finding's probe name, got:\n{rendered}"
        );
        assert!(
            rendered.contains("ENR-11"),
            "must reference the ENR-11 gate, got:\n{rendered}"
        );
        assert!(
            rendered.contains("3 candidates excluded by the ENR-11 gate."),
            "must include the triggering finding's own summary verbatim rather than restating \
             the fact independently, got:\n{rendered}"
        );
    }

    #[test]
    fn wards_unattributed_hint_states_the_figure_sourced_from_the_typed_hint() {
        let mut finding = base_finding("ward-concentration");
        finding.remediation_hint = Some(RemediationHint::WardsUnattributed(12));
        let entries = vec![entry(finding)];
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(
            rendered.contains("ward-concentration"),
            "must reference the triggering finding's probe name, got:\n{rendered}"
        );
        assert!(
            rendered.contains("12 contracts"),
            "must state the 12-ward-unattributed figure sourced from the typed \
             RemediationHint, got:\n{rendered}"
        );
    }

    #[test]
    fn no_triggers_present_returns_non_empty_no_remediation_statement() {
        let entries = vec![entry(base_finding("clean-probe"))];
        let strategy = Strategy::default();

        let rendered = render(&entries, &strategy);

        assert!(
            !rendered.is_empty(),
            "must not return an empty string when no remediation items apply"
        );
        assert!(
            rendered.contains("No remediation items apply"),
            "must explicitly state that no remediation items apply, not merely render \
             nothing, got:\n{rendered}"
        );
    }

    /// D-09's direct falsification target: `remediation.rs`'s production code must never contain
    /// a probe-name string comparison (`if e.finding.probe == "..."`-shaped) as its trigger
    /// logic. Triggers are `Cause`/`RemediationHint` only.
    ///
    /// Scans only the PRODUCTION portion of this file (everything before the first
    /// `#[cfg(test)]` marker) — scanning the whole file would trivially fail against this very
    /// assertion's own source text, which must name the forbidden shape to check for it.
    #[test]
    fn production_code_contains_no_probe_name_string_comparison() {
        let src = include_str!("remediation.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields at least one segment");
        let forbidden = ["finding", ".probe =="].concat();
        assert!(
            !production.contains(&forbidden),
            "remediation.rs's production code must not gate on a probe-name string comparison \
             — triggers must be typed (Cause/RemediationHint), never inferred from \
             `finding.probe`"
        );
    }
}
