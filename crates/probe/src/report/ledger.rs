//! The Full Probe Ledger — a summary table of all 22 rows (tier, probe, verdict, severity,
//! population/capable). `ReportEntry::tier` supplies the Tier column this function could not
//! otherwise answer (see `report/mod.rs`'s `ReportEntry` doc comment).
//!
//! Does NOT render per-finding evidence detail — that is `evidence::render_evidence`'s job,
//! called separately per entry in the per-verdict detail sections `report/mod.rs` builds
//! directly.
//!
//! Built as literal `| cell | cell |` Markdown strings via `format!` only — no terminal-table
//! crate (the research notes' Pitfall 4) — a Markdown table has no fixed column width, so nothing here
//! truncates. The verdict emoji/label pair is hand-authored against the same 6 `Verdict`
//! variants D-11's `legend.rs` (plan 35-10) renders — a different, concurrently-executing plan
//! owns that file, so the vocabulary is kept in lockstep by construction (same variants, same
//! declaration order) rather than by sharing code across the two files.

use super::ReportEntry;
use urban_integrity_schema::types::verdict::{PopulationSource, Verdict};

/// The Full Probe Ledger summary table (RPT-04, criterion 4).
///
/// Renders exactly one row per `ReportEntry`, in `entries`' given order — never re-sorted or
/// re-bucketed, since iteration order is the one dispatch-order guarantee this phase gets for
/// free. Every cell is pipe-escaped before interpolation so a `|` in a probe name cannot break
/// the table structure. `Population`/`Capable` render `-` (never the literal word `None`) for
/// verdicts carrying no `Coverage` (`DataGap`/`Blocked`).
pub fn render(entries: &[ReportEntry]) -> String {
    let mut out = String::new();
    out.push_str("## Full Probe Ledger\n\n");
    out.push_str("| Tier | Probe | Verdict | Severity | Population | Capable | Provenance |\n");
    out.push_str("|------|-------|---------|----------|------------|---------|------------|\n");
    for entry in entries {
        let tier = escape_cell(entry.tier);
        let probe = escape_cell(&entry.finding.probe);
        let (emoji, label) = verdict_emoji_label(&entry.finding.verdict);
        let verdict_cell = escape_cell(&format!("{emoji} {label}"));
        let severity = escape_cell(&format!("{:?}", entry.finding.severity));
        let population = entry
            .population()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());
        let capable = entry
            .capable()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".to_string());
        let provenance = escape_cell(&provenance_cell(entry));
        out.push_str(&format!(
            "| {tier} | {probe} | {verdict_cell} | {severity} | {population} | {capable} | {provenance} |\n"
        ));
    }
    out
}

/// The Provenance cell (VRD-13): `-` when no population was measured this run (matching the
/// existing `-` convention for Population/Capable); otherwise `Oracle: N` for an
/// oracle-corroborated population or the compact `Self-reported` label. The exclusion REASON is
/// deliberately not rendered here — the cell stays compact; the reason renders as prose in the
/// Clear detail group (PD-06's two-place split).
fn provenance_cell(entry: &ReportEntry) -> String {
    if entry.finding.verdict.coverage().is_none() {
        return "-".to_string();
    }
    match entry.population_source {
        PopulationSource::OracleCorroborated { oracle_count } => format!("Oracle: {oracle_count}"),
        PopulationSource::SelfReported { .. } => "Self-reported".to_string(),
    }
}

/// The fixed 6-variant emoji/label pair — `🔴 Hit`, `🟡 Weak`, `🟢 Clear`, `⚪ NotApplicable`,
/// `❓ DataGap`, `🚫 Blocked` — matching D-11's legend vocabulary (`legend.rs`, plan 35-10). Not a
/// shared function between the two files (they are filled in by two different, concurrently
/// executing worktree agents that must not touch each other's files); both are hand-authored
/// against the same fixed `Verdict` variant set instead, in the enum's own declaration order.
fn verdict_emoji_label(v: &Verdict) -> (&'static str, &'static str) {
    match v {
        Verdict::Hit { .. } => ("🔴", "Hit"),
        Verdict::Weak { .. } => ("🟡", "Weak"),
        Verdict::Clear(_) => ("🟢", "Clear"),
        Verdict::NotApplicable { .. } => ("⚪", "NotApplicable"),
        Verdict::DataGap { .. } => ("❓", "DataGap"),
        Verdict::Blocked { .. } => ("🚫", "Blocked"),
        Verdict::Skipped { .. } => ("⏭️", "Skipped"),
    }
}

/// Escapes a literal `|` so it cannot break Markdown table structure — applied to every cell,
/// not just the ones expected to need it (defensive; closes defect (a)'s pipe-escaping half).
fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MotifKind;
    use urban_integrity_schema::types::verdict::{Finding, Severity, Verdict};

    fn finding(probe: &str, verdict: Verdict, severity: Severity) -> Finding {
        Finding {
            probe: probe.to_string(),
            verdict,
            summary: "s".to_string(),
            cause: None,
            severity,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        }
    }

    fn entry(tier: &'static str, probe: &str, verdict: Verdict, severity: Severity) -> ReportEntry {
        entry_with_source(
            tier,
            probe,
            verdict,
            severity,
            PopulationSource::SelfReported {
                reason: "test fixture",
            },
        )
    }

    fn entry_with_source(
        tier: &'static str,
        probe: &str,
        verdict: Verdict,
        severity: Severity,
        population_source: PopulationSource,
    ) -> ReportEntry {
        ReportEntry {
            tier,
            hypothesis: "h",
            motif_kind: MotifKind::Absence,
            finding: finding(probe, verdict, severity),
            population_source,
        }
    }

    /// Counts rendered content rows: lines starting with `| ` that are neither the header row
    /// (identified by containing the literal column name `Tier`) nor the `|---|` separator row.
    fn content_row_count(rendered: &str) -> usize {
        rendered
            .lines()
            .filter(|l| l.starts_with('|'))
            .filter(|l| !l.contains("Tier") || !l.contains("Probe"))
            .filter(|l| !l.trim_start_matches('|').starts_with('-'))
            .count()
    }

    #[test]
    fn renders_exactly_one_row_per_entry() {
        let entries = vec![
            entry(
                "Tier-1",
                "probe-a",
                Verdict::classify(5, 5, 5, 1, ""),
                Severity::High,
            ),
            entry(
                "Tier-1",
                "probe-b",
                Verdict::classify(5, 5, 0, 1, ""),
                Severity::Info,
            ),
            entry(
                "Tier-2",
                "probe-c",
                Verdict::data_gap("missing"),
                Severity::Medium,
            ),
        ];
        let rendered = render(&entries);
        assert_eq!(
            content_row_count(&rendered),
            3,
            "expected exactly one content row per entry, got:\n{rendered}"
        );
    }

    #[test]
    fn long_probe_name_is_never_truncated() {
        let long_name = "x".repeat(200);
        let entries = vec![entry(
            "Tier-1",
            &long_name,
            Verdict::classify(1, 1, 1, 1, ""),
            Severity::Critical,
        )];
        let rendered = render(&entries);
        assert!(
            rendered.contains(&long_name),
            "full 200-char probe name must appear verbatim, nothing here truncates a cell"
        );
    }

    #[test]
    fn pipe_in_probe_name_is_escaped_not_dropped_or_left_raw() {
        let entries = vec![entry(
            "Tier-1",
            "Acme | Co",
            Verdict::classify(1, 1, 0, 1, ""),
            Severity::Info,
        )];
        let rendered = render(&entries);
        assert!(
            rendered.contains("Acme \\| Co"),
            "a literal `|` in a probe name must be escaped as `\\|`, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("| Acme | Co |"),
            "an unescaped pipe would introduce a spurious table cell boundary"
        );
    }

    #[test]
    fn data_gap_and_blocked_render_dash_never_the_word_none() {
        let entries = vec![
            entry(
                "Tier-1",
                "probe-a",
                Verdict::data_gap("missing"),
                Severity::High,
            ),
            entry(
                "Tier-1",
                "probe-b",
                Verdict::blocked("prereq"),
                Severity::High,
            ),
        ];
        let rendered = render(&entries);
        assert!(
            !rendered.contains("None"),
            "population/capable must never render the literal word None, got:\n{rendered}"
        );
        assert!(
            rendered.contains("| - | - |"),
            "population/capable must render `-` for verdicts carrying no Coverage, got:\n{rendered}"
        );
    }

    #[test]
    fn provenance_column_renders_oracle_count_and_self_reported() {
        let entries = vec![
            entry_with_source(
                "Tier-1",
                "probe-corroborated",
                Verdict::classify(5, 5, 0, 1, ""),
                Severity::Info,
                PopulationSource::OracleCorroborated { oracle_count: 42 },
            ),
            entry_with_source(
                "Tier-2",
                "probe-self",
                Verdict::classify(3, 3, 0, 1, ""),
                Severity::Info,
                PopulationSource::SelfReported { reason: "x" },
            ),
            // No Coverage this run: the cell must be `-`, whatever the source says.
            entry_with_source(
                "Tier-2",
                "probe-gap",
                Verdict::data_gap("missing"),
                Severity::Medium,
                PopulationSource::SelfReported {
                    reason: "no population measured this run (DataGap/Blocked)",
                },
            ),
        ];
        let rendered = render(&entries);
        assert!(
            rendered.contains("Oracle: 42"),
            "corroborated row must carry the oracle count, got:\n{rendered}"
        );
        assert!(
            rendered.contains("Self-reported"),
            "self-reported row must carry the compact label, got:\n{rendered}"
        );
        let gap_row = rendered
            .lines()
            .find(|l| l.contains("probe-gap"))
            .expect("gap row present");
        assert!(
            gap_row.trim_end().ends_with("| - |"),
            "no-Coverage row's Provenance cell must be `-`, got: {gap_row}"
        );
    }

    #[test]
    fn hit_verdict_renders_keyed_emoji_and_label() {
        let entries = vec![entry(
            "Tier-1",
            "probe-a",
            Verdict::classify(5, 5, 5, 1, ""),
            Severity::Critical,
        )];
        let rendered = render(&entries);
        assert!(
            rendered.contains("🔴 Hit"),
            "Hit verdict must render its keyed emoji + label, got:\n{rendered}"
        );
    }

    #[test]
    fn skipped_verdict_renders_keyed_emoji_and_label() {
        let upstream = Verdict::classify(10, 10, 0, 1, "");
        let entries = vec![entry(
            "Tier-2",
            "probe-a",
            Verdict::skipped_dependency("identity-integrity", &upstream),
            Severity::Info,
        )];
        let rendered = render(&entries);
        assert!(
            rendered.contains("⏭️ Skipped"),
            "Skipped verdict must render its keyed emoji + label, got:\n{rendered}"
        );
    }
}
