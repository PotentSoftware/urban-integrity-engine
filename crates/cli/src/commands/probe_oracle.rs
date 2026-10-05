//! `urban-integrity probe-oracle` — ORC-05's comparison command and ORC-08's coverage/exclusion
//! statement.
//!
//! Runs the real 22-probe battery unchanged (`probe::run::execute`), then for every probe whose
//! declared population is structurally checkable against `graph_events`, calls Plan 38-01's
//! `persistence::oracle` counting primitives and renders a comparison table flagging every
//! disagreement on one row. The remaining probes cannot be independently checked this way — each
//! one is named in an explicit coverage table with a specific, source-cited reason, never a
//! blanket "Tier-2" label (ORC-08). This closes the gap `260831-ffx` exploited: the correct number
//! sat on disk, unread, for four days because nothing compared it against what the probes
//! reported.

use anyhow::Context;
use comfy_table::{Cell, Color, Table};
use surrealdb::{engine::any::Any, Surreal};

use urban_integrity_probe::population_source::{self, Exclusion, CHECKED_PROBES, EXCLUDED_PROBES};
use urban_integrity_probe::report::ReportEntry;

/// Per-probe delta tolerance: `(probe name, max allowed absolute difference, justification)`.
/// Empty today -- every checkable probe defaults to an exact-match expectation (delta 0), per
/// the phase's own recommendation. A non-zero entry must always carry a non-empty justification
/// string (see [`probe_delta_is_justified`]).
const PROBE_DELTA: &[(&str, u64, &str)] = &[];

/// D-05's self-check: every non-zero delta entry in a `PROBE_DELTA`-shaped table must carry a
/// non-empty justification string. Exercised against both the real (empty) `PROBE_DELTA` and
/// hand-built fixtures in `tests`, below -- this proves the check logic itself, independent of
/// whether the real table ever grows a non-zero entry.
fn probe_delta_is_justified(deltas: &[(&str, u64, &str)]) -> bool {
    deltas
        .iter()
        .all(|(_, delta, reason)| *delta == 0 || !reason.is_empty())
}

/// D-07's disagreement pointer. Returns `Some` naming `docs/ADJUDICATIONS.md` exactly when at
/// least one checked probe disagrees with the oracle this run, `None` otherwise (non-vacuity --
/// the pointer must never appear when nothing disagreed).
fn disagreement_pointer(report: &OracleReport) -> Option<&'static str> {
    if report.checked.iter().any(|r| r.agrees == Some(false)) {
        Some(
            "\nOne or more probes disagree with the oracle. See docs/ADJUDICATIONS.md for prior \
             adjudications before re-investigating.",
        )
    } else {
        None
    }
}

/// One checked probe's comparison result: its declared population beside the oracle's
/// independently recomputed count, on one row.
#[derive(Debug, Clone)]
pub struct ComparisonRow {
    pub probe: &'static str,
    /// `None` when the probe's `Verdict` this run was `DataGap`/`Blocked` -- nothing was
    /// measured, so there is nothing to compare (never a false disagreement against 0).
    pub declared: Option<u64>,
    pub oracle: u64,
    pub delta_allowed: u64,
    /// `None` exactly when `declared` is `None`.
    pub agrees: Option<bool>,
}

/// The full comparison + coverage report: every checkable probe's comparison row, plus every
/// excluded probe's reason.
#[derive(Debug, Clone)]
pub struct OracleReport {
    pub checked: Vec<ComparisonRow>,
    pub excluded: Vec<Exclusion>,
}

/// Build the full comparison + coverage report from a real battery run's entries.
///
/// One [`ComparisonRow`] per [`CHECKED_PROBES`] entry, one [`Exclusion`] per [`EXCLUDED_PROBES`]
/// entry. The union of both name-sets is checked to equal `entries`'s own probe names in BOTH
/// directions -- every registered probe appears in exactly one of the two tables, and neither
/// table names a probe the battery does not register -- so a probe silently absent from the
/// coverage statement cannot happen unnoticed (ORC-08 / T-38-05).
///
/// These are hard errors, not `debug_assert!`s. CR-02 (phase 38 code review): the workspace defines no
/// `[profile.release]`, so `debug-assertions` defaults to `false` and a `debug_assert!` here is
/// compiled out of the shipped binary -- exactly the build in which a silent coverage gap
/// matters. `bail!` costs one comparison per probe on a command that has already run the whole
/// battery, so there is nothing to buy back by making it conditional.
pub async fn compare(entries: &[ReportEntry], db: &Surreal<Any>) -> anyhow::Result<OracleReport> {
    // The size + both-direction reconciliation checks live in the shared
    // `population_source::verify_registered_probes_covered` (Plan 39-07) so this command and
    // `probe::run::execute()` enforce the identical partition — one copy, no drift.
    let probe_names: Vec<&str> = entries.iter().map(|e| e.probe.as_str()).collect();
    population_source::verify_registered_probes_covered(&probe_names)?;
    if !probe_delta_is_justified(PROBE_DELTA) {
        anyhow::bail!("PROBE_DELTA has a non-zero delta with no justification");
    }

    let mut checked = Vec::with_capacity(CHECKED_PROBES.len());
    for check in CHECKED_PROBES {
        let entry = entries
            .iter()
            .find(|e| e.probe == check.probe)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "probe '{}' not found in battery entries -- CHECKED_PROBES has drifted from \
                     the registered battery",
                    check.probe
                )
            })?;
        let oracle_count = population_source::evaluate(&check.kind, db).await?;
        let declared = entry.population();
        let delta_allowed = PROBE_DELTA
            .iter()
            .find(|(name, _, _)| *name == check.probe)
            .map_or(0, |(_, delta, _)| *delta);
        let agrees = declared.map(|d| d.abs_diff(oracle_count) <= delta_allowed);
        checked.push(ComparisonRow {
            probe: check.probe,
            declared,
            oracle: oracle_count,
            delta_allowed,
            agrees,
        });
    }

    Ok(OracleReport {
        checked,
        excluded: EXCLUDED_PROBES.to_vec(),
    })
}

/// Render the comparison table: Probe, Declared, Oracle, Delta, Agrees -- declared/oracle always
/// on the SAME row, never split across two tables, with disagreements visually flagged.
fn render_comparison_table(report: &OracleReport) -> Table {
    let mut table = Table::new();
    table.set_header(vec!["Probe", "Declared", "Oracle", "Delta", "Agrees"]);
    for row in &report.checked {
        let (declared_str, agrees_str, color) = match (row.declared, row.agrees) {
            (None, _) => (
                "not measured this run".to_string(),
                "not measured this run".to_string(),
                Color::DarkGrey,
            ),
            (Some(d), Some(true)) => (d.to_string(), "AGREE".to_string(), Color::Green),
            (Some(d), Some(false)) => (d.to_string(), "DISAGREE".to_string(), Color::Red),
            (Some(d), None) => (d.to_string(), "UNKNOWN".to_string(), Color::DarkGrey),
        };
        table.add_row(vec![
            Cell::new(row.probe),
            Cell::new(declared_str),
            Cell::new(row.oracle.to_string()),
            Cell::new(row.delta_allowed.to_string()),
            Cell::new(agrees_str).fg(color),
        ]);
    }
    table
}

/// Render the coverage/exclusion table: Probe, Category, Reason -- naming every excluded probe
/// AND its category-specific reason text, never just the probe name (ORC-08).
fn render_coverage_table(report: &OracleReport) -> Table {
    let mut table = Table::new();
    table.set_header(vec!["Probe", "Category", "Reason"]);
    for excl in &report.excluded {
        table.add_row(vec![
            Cell::new(excl.probe),
            Cell::new(format!("{:?}", excl.category)),
            Cell::new(excl.reason),
        ]);
    }
    table
}

/// Build the `--output` artifact's contents: both plain-rendered tables, plus D-07's
/// disagreement pointer appended when [`disagreement_pointer`] returns `Some` (Research Open
/// Question 2: the pointer mirrors into the artifact, not stdout only).
fn render_report_contents(
    report: &OracleReport,
    plain_comparison: &Table,
    plain_coverage: &Table,
) -> String {
    let mut contents = format!(
        "Population Oracle Comparison\n{plain_comparison}\n\nCoverage / Exclusions\n{plain_coverage}\n"
    );
    if let Some(line) = disagreement_pointer(report) {
        contents.push_str(line);
        contents.push('\n');
    }
    contents
}

/// Write one artifact, creating the parent directory if it does not exist. Copied from
/// `probe/run.rs::write_artifact` rather than made `pub` there, to keep this plan's
/// `files_modified` list from touching `run.rs` (T-38-04).
fn write_artifact(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory {}", parent.display())
            })?;
        }
    }
    std::fs::write(path, contents).with_context(|| format!("Failed to write to {}", path.display()))
}

/// Arguments for `urban-integrity probe-oracle`.
#[derive(Debug, clap::Args)]
pub struct ProbeOracleArgs {
    /// Path to write a plain-text rendering of both tables to. If absent, only stdout is
    /// printed.
    #[arg(long)]
    pub output: Option<std::path::PathBuf>,
}

/// Run `probe-oracle` end-to-end: run the real battery, independently recompute every checkable
/// probe's population, render the comparison + coverage tables, and optionally write them to
/// disk.
pub async fn run(args: ProbeOracleArgs) -> anyhow::Result<()> {
    // WR-05: resolve the analysis database BEFORE the battery runs. `AppConfig` itself is
    // deterministic within one invocation (config.toml + URBAN__* env), but for a non-`main`
    // analysis `connect_db` -> `resolve_analysis_db` is a REGISTRY LOOKUP, not a config read:
    // it maps the analysis id to `db_name` via the `_catalog` `analyses` table and rejects
    // retired/retiring rows. The battery is a long window (the gated CLI test binary alone runs
    // ~30min on the live corpus); an `analysis` retire/repoint inside it would send the oracle's
    // queries at a DIFFERENT database than the probes read, and every row of the comparison
    // table would be a fabricated disagreement. Capture the resolution now and re-check after.
    let config = urban_integrity_core::config::AppConfig::load().context("Config load failed")?;
    let db_name_before = crate::db::resolve_analysis_db(&config).await?;

    // D-20's rationale extended by analogy (the research notes' Pitfall 5/Assumption A4): probe-oracle's
    // own coverage/comparison tables need every CHECKED_PROBES probe to have actually run, for the
    // same reason ORC-07's DB-gated test does -- a gated Tier-1 Clear would otherwise show many
    // Tier-2 rows as `declared: None`, silently reducing the command's value for its own stated
    // purpose. This call site also now inherits the unconditional graph-validation guarantee via
    // execute() -> dispatch_battery for free -- no other change needed here for that part.
    let result = super::probe::execute(&super::probe::ProbeArgs {
        output: None,
        json: None,
        full_battery: true,
    })
    .await?;

    let db_name_after = crate::db::resolve_analysis_db(&config).await?;
    if db_name_before != db_name_after {
        anyhow::bail!(
            "analysis '{}' was repointed while the battery ran ({db_name_before} -> \
             {db_name_after}); the oracle would read a different database than the probes did, \
             fabricating disagreements. Re-run probe-oracle.",
            config.analysis.as_deref().unwrap_or("main")
        );
    }
    let db = crate::db::connect_to_db(&config, &db_name_after).await?;

    let report = compare(&result.entries, &db).await?;

    let comparison_table = render_comparison_table(&report);
    println!("{comparison_table}");

    let coverage_table = render_coverage_table(&report);
    println!("{coverage_table}");

    if let Some(line) = disagreement_pointer(&report) {
        println!("{line}");
    }

    if let Some(path) = args.output.as_ref() {
        // WR-03: render the artifact from de-styled copies. comfy-table applies ANSI SGR styling
        // whenever stdout is a terminal, so reusing the tables printed above would write escape
        // sequences into the file when run interactively (the normal operator case) and plain
        // bytes under a pipe/CI -- neither plain text nor byte-stable across invocations, which
        // defeats diffing the artifact between runs.
        let mut plain_comparison = render_comparison_table(&report);
        plain_comparison.force_no_tty();
        let mut plain_coverage = render_coverage_table(&report);
        plain_coverage.force_no_tty();
        let contents = render_report_contents(&report, &plain_comparison, &plain_coverage);
        write_artifact(path, &contents)?;
        println!("Report written to {}", path.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_probe::population_source::ExclusionCategory;

    fn fixture_row(
        probe: &'static str,
        declared: Option<u64>,
        oracle: u64,
        agrees: Option<bool>,
    ) -> ComparisonRow {
        ComparisonRow {
            probe,
            declared,
            oracle,
            delta_allowed: 0,
            agrees,
        }
    }

    /// WR-04 (phase 38 code review): the `--output` artifact path had no test at any level, and WR-03
    /// (ANSI escapes written into the artifact when run from a terminal) shipped as a direct
    /// consequence. This test renders a fixture report the way `run()` does for the artifact —
    /// de-styled copies — writes it through the real `write_artifact` (bare filename inside a
    /// tempdir cwd is covered by the parent-dir guard; nested path covers create_dir_all), and
    /// asserts the contents are complete and contain no ESC byte.
    #[test]
    fn artifact_contains_every_probe_and_no_ansi_escapes() {
        let report = OracleReport {
            checked: vec![
                fixture_row("ward-concentration", Some(5), 5, Some(true)),
                fixture_row("address-cluster-exact", Some(3), 4, Some(false)),
                fixture_row("temporal-burst", None, 2, None),
            ],
            excluded: vec![Exclusion {
                probe: "land-flip-candidates",
                category: ExclusionCategory::MarkerArithmetic,
                reason: "population sourced from land_flip_runs marker, not graph_events",
            }],
        };

        let mut plain_comparison = render_comparison_table(&report);
        plain_comparison.force_no_tty();
        let mut plain_coverage = render_coverage_table(&report);
        plain_coverage.force_no_tty();
        let contents = format!(
            "Population Oracle Comparison\n{plain_comparison}\n\nCoverage / Exclusions\n{plain_coverage}\n"
        );

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("nested").join("oracle-report.txt");
        write_artifact(&path, &contents).expect("write_artifact failed");

        let on_disk = std::fs::read_to_string(&path).expect("read artifact back");
        for probe in [
            "ward-concentration",
            "address-cluster-exact",
            "temporal-burst",
            "land-flip-candidates",
        ] {
            assert!(
                on_disk.contains(probe),
                "artifact must contain probe name {probe}"
            );
        }
        assert!(
            !on_disk.contains('\u{1b}'),
            "artifact must be plain text with no ANSI escape sequences (WR-03)"
        );
    }

    #[test]
    fn probe_delta_self_check_rejects_unjustified_nonzero_delta_and_accepts_justified() {
        assert!(!probe_delta_is_justified(&[("x", 3, "")]));
        assert!(probe_delta_is_justified(&[("x", 3, "justified")]));
        assert!(probe_delta_is_justified(&[("x", 0, "")]));
    }

    #[test]
    fn comparison_table_places_declared_and_oracle_for_a_disagreeing_probe_on_one_row() {
        let report = OracleReport {
            checked: vec![fixture_row(
                "ward-concentration",
                Some(100),
                105,
                Some(false),
            )],
            excluded: vec![],
        };
        let table = render_comparison_table(&report);
        let rendered = table.to_string();
        let row_line = rendered
            .lines()
            .find(|l| l.contains("ward-concentration"))
            .expect("comparison row for ward-concentration must be present");
        assert!(
            row_line.contains("100"),
            "row must contain the declared value: {row_line}"
        );
        assert!(
            row_line.contains("105"),
            "row must contain the oracle value: {row_line}"
        );
        assert!(
            row_line.contains("DISAGREE"),
            "disagreeing row must be visually flagged: {row_line}"
        );
    }

    #[test]
    fn comparison_table_never_reports_a_false_disagreement_for_an_unmeasured_probe() {
        let report = OracleReport {
            checked: vec![fixture_row("precomputed-alerts", None, 0, None)],
            excluded: vec![],
        };
        let table = render_comparison_table(&report);
        let rendered = table.to_string();
        assert!(rendered.contains("not measured this run"));
        assert!(!rendered.contains("DISAGREE"));
    }

    #[test]
    fn coverage_table_names_every_excluded_probe_and_its_specific_reason() {
        let report = OracleReport {
            checked: vec![],
            excluded: vec![
                *EXCLUDED_PROBES
                    .iter()
                    .find(|e| e.probe == "land-flip-candidates")
                    .expect("present"),
                *EXCLUDED_PROBES
                    .iter()
                    .find(|e| e.probe == "detector-readiness")
                    .expect("present"),
            ],
        };
        let table = render_coverage_table(&report);
        let rendered = table.to_string();
        assert!(rendered.contains("land-flip-candidates"));
        assert!(rendered.contains("marker arithmetic"));
        assert!(rendered.contains("detector-readiness"));
        assert!(rendered.contains("term_counts"));
    }

    #[test]
    fn full_fixture_report_covering_all_22_probes_renders_with_no_probe_missing() {
        let checked: Vec<ComparisonRow> = CHECKED_PROBES
            .iter()
            .map(|c| fixture_row(c.probe, Some(1), 1, Some(true)))
            .collect();
        let excluded: Vec<Exclusion> = EXCLUDED_PROBES.to_vec();
        assert_eq!(checked.len(), 10);
        assert_eq!(excluded.len(), 12);
        let report = OracleReport { checked, excluded };
        let comparison_rendered = render_comparison_table(&report).to_string();
        let coverage_rendered = render_coverage_table(&report).to_string();
        for check in CHECKED_PROBES {
            assert!(
                comparison_rendered.contains(check.probe),
                "{} missing from comparison table",
                check.probe
            );
        }
        for excl in EXCLUDED_PROBES {
            assert!(
                coverage_rendered.contains(excl.probe),
                "{} missing from coverage table",
                excl.probe
            );
        }
    }

    /// D-07: when at least one checked probe disagrees, the pointer fires and names the
    /// adjudication doc.
    #[test]
    fn disagreement_pointer_fires_when_any_row_disagrees() {
        let report = OracleReport {
            checked: vec![fixture_row(
                "ward-concentration",
                Some(100),
                105,
                Some(false),
            )],
            excluded: vec![],
        };
        let pointer = disagreement_pointer(&report);
        assert!(pointer.is_some(), "pointer must fire on disagreement");
        assert!(
            pointer.unwrap().contains("docs/ADJUDICATIONS.md"),
            "pointer must name the adjudication doc"
        );
    }

    /// D-07 non-vacuity: the pointer must be ABSENT when every checked probe agrees (or was not
    /// measured this run) -- proving the pointer is not present-by-default.
    #[test]
    fn disagreement_pointer_absent_when_all_rows_agree() {
        let report = OracleReport {
            checked: vec![
                fixture_row("ward-concentration", Some(100), 100, Some(true)),
                fixture_row("temporal-burst", None, 2, None),
            ],
            excluded: vec![],
        };
        assert!(
            disagreement_pointer(&report).is_none(),
            "pointer must not fire when nothing disagreed"
        );
    }

    /// D-07 / Research Open Question 2: the SAME pointer text appears in the `--output` artifact
    /// contents exactly when a disagreement exists -- not present when every row agrees.
    #[test]
    fn render_report_contents_includes_pointer_only_on_disagreement() {
        let disagreeing = OracleReport {
            checked: vec![fixture_row(
                "ward-concentration",
                Some(100),
                105,
                Some(false),
            )],
            excluded: vec![],
        };
        let agreeing = OracleReport {
            checked: vec![fixture_row(
                "ward-concentration",
                Some(100),
                100,
                Some(true),
            )],
            excluded: vec![],
        };

        let mut disagreeing_comparison = render_comparison_table(&disagreeing);
        disagreeing_comparison.force_no_tty();
        let mut disagreeing_coverage = render_coverage_table(&disagreeing);
        disagreeing_coverage.force_no_tty();
        let disagreeing_contents =
            render_report_contents(&disagreeing, &disagreeing_comparison, &disagreeing_coverage);
        assert!(
            disagreeing_contents.contains("docs/ADJUDICATIONS.md"),
            "artifact contents must contain the pointer when a probe disagrees"
        );

        let mut agreeing_comparison = render_comparison_table(&agreeing);
        agreeing_comparison.force_no_tty();
        let mut agreeing_coverage = render_coverage_table(&agreeing);
        agreeing_coverage.force_no_tty();
        let agreeing_contents =
            render_report_contents(&agreeing, &agreeing_comparison, &agreeing_coverage);
        assert!(
            !agreeing_contents.contains("docs/ADJUDICATIONS.md"),
            "artifact contents must NOT contain the pointer when nothing disagreed"
        );
    }
}
