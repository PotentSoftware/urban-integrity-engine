//! D-26's golden Markdown fixture -- captured from the CURRENT (pre-move)
//! `crates/cli/src/commands/probe/report/mod.rs::render_evidence_report` before Task 2 moves the
//! report writer into this crate. `GOLDEN_REPORT` is the exact stripped-of-timestamp output of
//! `render_evidence_report(&entries, &Strategy::default())` against a fixture covering every one
//! of the 7 `Verdict` kinds (Hit, Weak, Clear, NotApplicable, DataGap, Blocked, Skipped), with the
//! `Clear` kind appearing twice to also cover both `PopulationSource` provenance shapes
//! (`OracleCorroborated` and `SelfReported`) -- 8 `ReportEntry` values in total. Every line
//! starting with the literal prefix `"Generated: "` has already been stripped.
//!
//! Task 3's `byte_identical_markdown_across_the_extraction` rebuilds the identical fixture using
//! `urban_integrity_probe`'s own types (now that the crate exists) and asserts the result equals
//! this constant verbatim (`assert_eq!`, full-string equality -- the D-26 proof).

pub const GOLDEN_REPORT: &str = r#"# UrbanIntegrity Evidence Report


## Bottom Line Up Front

2 probes of 8 could not evaluate at all (data gap or blocked).

1 probe skipped by gating (see the Skipped Probes section for why).

Across every probe that did run, 37 records of 40 records evaluated were structurally capable of producing a finding.

1 finding confirmed.

## Verdict Legend

| Emoji | Verdict | Meaning |
|---|---|---|
| 🔴 | Hit | A capable population was fully evaluated and matches met the probe's actionable threshold. |
| 🟡 | Weak | A capable population was fully evaluated and some matches were found, but below the probe's actionable threshold. |
| 🟢 | Clear | A capable population was fully evaluated and no matches were found. |
| ⚪ | NotApplicable | Only part of the population was structurally eligible to be evaluated; the excluded portion is reported alongside the reason. |
| ❓ | DataGap | The probe could not run at all because a required input was missing — nothing was measured. |
| 🚫 | Blocked | The probe could not run because an unmet prerequisite (typically another probe's own DataGap or a Strategy flag) prevented it. |
| ⏭️ | Skipped | The probe's gate predicate found no qualifying upstream signal (a declared dependency did not return Hit/Weak, or — for a Tier-2 probe — no Tier-1 probe did), so it was never run at all; see the Skipped Probes section for its specific cause. |

## Skipped Probes

1 probe skipped by the gate predicate:

- golden-fixture-skipped — upstream 'golden-fixture-hit' returned Clear

## How the Search Adapted

1. Every registered probe ran, in a fixed order, against 8 probe results — `Strategy` fields let a later probe read what an earlier one found, but no probe's own verdict determines whether a later probe runs at all.
2. No OSINT adapter had a recorded run by the time OSINT-dependent probes executed, so those probes reported DataGap rather than assuming a clean result.

## Confirmed Findings

### golden-fixture-hit

> **Hypothesis:** golden fixture: Hit

golden fixture Hit summary
Aggregate measurement — no individual records available to pivot to; re-run `urban-integrity-cli probe` against the same database state to reproduce this figure.

## Suggestive Patterns

### golden-fixture-weak

> **Hypothesis:** golden fixture: Weak

golden fixture Weak summary
Aggregate measurement — no individual records available to pivot to; re-run `urban-integrity-cli probe` against the same database state to reproduce this figure.

## Data-Integrity Gaps

### golden-fixture-data-gap

> **Hypothesis:** golden fixture: DataGap

golden fixture DataGap summary
Aggregate measurement — no individual records available to pivot to; re-run `urban-integrity-cli probe` against the same database state to reproduce this figure.

### golden-fixture-blocked

> **Hypothesis:** golden fixture: Blocked

golden fixture Blocked summary
Aggregate measurement — no individual records available to pivot to; re-run `urban-integrity-cli probe` against the same database state to reproduce this figure.

## Not Applicable

### golden-fixture-not-applicable

> **Hypothesis:** golden fixture: NotApplicable

golden fixture NotApplicable summary
Aggregate measurement — no individual records available to pivot to; re-run `urban-integrity-cli probe` against the same database state to reproduce this figure.

## Verified Negative Findings

### golden-fixture-clear-corroborated

> **Hypothesis:** golden fixture: Clear (oracle-corroborated)

golden fixture corroborated clear
Aggregate measurement — no individual records available to pivot to; re-run `urban-integrity-cli probe` against the same database state to reproduce this figure.

> **Population source:** oracle-corroborated — independently recomputed population = 0.

### golden-fixture-clear-self-reported

> **Hypothesis:** golden fixture: Clear (self-reported)

golden fixture self-reported clear
Aggregate measurement — no individual records available to pivot to; re-run `urban-integrity-cli probe` against the same database state to reproduce this figure.

> **Population source:** self-reported only, not independently checked — golden fixture, oracle cannot check

## Full Probe Ledger

| Tier | Probe | Verdict | Severity | Population | Capable | Provenance |
|------|-------|---------|----------|------------|---------|------------|
| Tier-1 | golden-fixture-hit | 🔴 Hit | High | 10 | 10 | Self-reported |
| Tier-1 | golden-fixture-weak | 🟡 Weak | Medium | 10 | 10 | Self-reported |
| Tier-1 | golden-fixture-clear-corroborated | 🟢 Clear | Info | 0 | 0 | Oracle: 0 |
| Tier-2 | golden-fixture-clear-self-reported | 🟢 Clear | Info | 10 | 10 | Self-reported |
| Tier-1 | golden-fixture-not-applicable | ⚪ NotApplicable | Info | 10 | 7 | Self-reported |
| Tier-2 | golden-fixture-data-gap | ❓ DataGap | Medium | - | - | - |
| Tier-2 | golden-fixture-blocked | 🚫 Blocked | Medium | - | - | - |
| Tier-2 | golden-fixture-skipped | ⏭️ Skipped | Info | - | - | - |

## Contradiction Checks

**Check 1** (independently-measured populations):
- **1a** (ward-concentration vs temporal-burst): could not evaluate — ward-concentration absent from this battery; temporal-burst absent from this battery.
- **1b** (identity-integrity vs its unfiltered superset): could not evaluate — identity-integrity absent from this battery.

**Check 2** (attributed evidence vs run markers): could not evaluate — no adapter run markers were read this battery.

## Recommended Remediation

No remediation items apply — no finding in this battery carried a triggering Cause or RemediationHint.
"#;

use urban_integrity_probe::report::ReportEntry;
use urban_integrity_probe::MotifKind;
use urban_integrity_probe::Strategy;
use urban_integrity_schema::types::verdict::{Finding, PopulationSource, Severity, Verdict};

/// Builds the exact 8-`ReportEntry` fixture whose rendering produced [`GOLDEN_REPORT`], as owned
/// data, using `urban_integrity_probe`'s own types directly (not via `crates/cli`) -- by the time
/// this crate exists, `ReportEntry`/`Verdict` construction is only reachable through it.
///
/// The 8 variants, one per `Verdict` kind, with `Clear` doubled to cover both `PopulationSource`
/// provenance shapes:
///
/// 1. Hit -- `golden-fixture-hit`, Tier-1, `Verdict::classify(10, 10, 5, 1, "")`
/// 2. Weak -- `golden-fixture-weak`, Tier-1, `Verdict::classify(10, 10, 3, 5, "")`
/// 3. Clear (oracle-corroborated) -- `golden-fixture-clear-corroborated`, Tier-1,
///    `Verdict::classify(0, 0, 0, 1, "")`,
///    `PopulationSource::OracleCorroborated { oracle_count: 0 }`
/// 4. Clear (self-reported) -- `golden-fixture-clear-self-reported`, Tier-2,
///    `Verdict::classify(10, 10, 0, 1, "")`,
///    `PopulationSource::SelfReported { reason: "golden fixture, oracle cannot check" }`
/// 5. NotApplicable -- `golden-fixture-not-applicable`, Tier-1,
///    `Verdict::classify(10, 7, 0, 1, "3 structurally excluded")`
/// 6. DataGap -- `golden-fixture-data-gap`, Tier-2, `Verdict::data_gap("missing input")`
/// 7. Blocked -- `golden-fixture-blocked`, Tier-2, `Verdict::blocked("unmet prerequisite")`
/// 8. Skipped -- `golden-fixture-skipped`, Tier-2,
///    `Verdict::skipped_dependency("golden-fixture-hit", &clear_upstream)` where
///    `clear_upstream = Verdict::classify(10, 10, 0, 1, "")`
///
/// Every entry not listed above uses `PopulationSource::SelfReported { reason: "golden fixture" }`.
pub fn golden_fixture_entries() -> Vec<ReportEntry> {
    let clear_upstream = Verdict::classify(10, 10, 0, 1, "");
    vec![
        ReportEntry {
            tier: "Tier-1",
            hypothesis: "golden fixture: Hit",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "golden-fixture-hit".to_string(),
                verdict: Verdict::classify(10, 10, 5, 1, ""),
                summary: "golden fixture Hit summary".to_string(),
                cause: None,
                severity: Severity::High,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            },
            population_source: PopulationSource::SelfReported {
                reason: "golden fixture",
            },
        },
        ReportEntry {
            tier: "Tier-1",
            hypothesis: "golden fixture: Weak",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "golden-fixture-weak".to_string(),
                verdict: Verdict::classify(10, 10, 3, 5, ""),
                summary: "golden fixture Weak summary".to_string(),
                cause: None,
                severity: Severity::Medium,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            },
            population_source: PopulationSource::SelfReported {
                reason: "golden fixture",
            },
        },
        ReportEntry {
            tier: "Tier-1",
            hypothesis: "golden fixture: Clear (oracle-corroborated)",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "golden-fixture-clear-corroborated".to_string(),
                verdict: Verdict::classify(0, 0, 0, 1, ""),
                summary: "golden fixture corroborated clear".to_string(),
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
            hypothesis: "golden fixture: Clear (self-reported)",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "golden-fixture-clear-self-reported".to_string(),
                verdict: Verdict::classify(10, 10, 0, 1, ""),
                summary: "golden fixture self-reported clear".to_string(),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            },
            population_source: PopulationSource::SelfReported {
                reason: "golden fixture, oracle cannot check",
            },
        },
        ReportEntry {
            tier: "Tier-1",
            hypothesis: "golden fixture: NotApplicable",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "golden-fixture-not-applicable".to_string(),
                verdict: Verdict::classify(10, 7, 0, 1, "3 structurally excluded"),
                summary: "golden fixture NotApplicable summary".to_string(),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            },
            population_source: PopulationSource::SelfReported {
                reason: "golden fixture",
            },
        },
        ReportEntry {
            tier: "Tier-2",
            hypothesis: "golden fixture: DataGap",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "golden-fixture-data-gap".to_string(),
                verdict: Verdict::data_gap("missing input"),
                summary: "golden fixture DataGap summary".to_string(),
                cause: None,
                severity: Severity::Medium,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            },
            population_source: PopulationSource::SelfReported {
                reason: "golden fixture",
            },
        },
        ReportEntry {
            tier: "Tier-2",
            hypothesis: "golden fixture: Blocked",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "golden-fixture-blocked".to_string(),
                verdict: Verdict::blocked("unmet prerequisite"),
                summary: "golden fixture Blocked summary".to_string(),
                cause: None,
                severity: Severity::Medium,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            },
            population_source: PopulationSource::SelfReported {
                reason: "golden fixture",
            },
        },
        ReportEntry {
            tier: "Tier-2",
            hypothesis: "golden fixture: Skipped",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "golden-fixture-skipped".to_string(),
                verdict: Verdict::skipped_dependency("golden-fixture-hit", &clear_upstream),
                summary: "golden fixture Skipped summary".to_string(),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            },
            population_source: PopulationSource::SelfReported {
                reason: "golden fixture",
            },
        },
    ]
}

/// D-26's byte-identical proof: builds the identical fixture via `urban_integrity_probe`'s own
/// types, renders it, strips every `"Generated: "` line, and asserts full-string equality against
/// [`GOLDEN_REPORT`] -- not `.contains(...)`. `render_evidence_report` moved verbatim from
/// `crates/cli` in Task 2, so this proves the extraction changed no byte of the Markdown output.
#[test]
fn byte_identical_markdown_across_the_extraction() {
    let entries = golden_fixture_entries();
    let strategy = Strategy::default();

    let report = urban_integrity_probe::report::render_evidence_report(&entries, &strategy);
    let stripped: String = report
        .lines()
        .filter(|line| !line.starts_with("Generated: "))
        .map(|line| format!("{line}\n"))
        .collect();

    assert_eq!(
        stripped, GOLDEN_REPORT,
        "render_evidence_report's output (stripped of its Generated: timestamp line) must be \
         byte-identical to the pre-extraction capture (D-26) -- any difference here means the \
         move changed report output, not just its location"
    );
}
