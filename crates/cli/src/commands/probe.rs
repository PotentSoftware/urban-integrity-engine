//! `urban-integrity-cli probe` — the CLI entry point wiring the 22-probe battery
//! (`urban_integrity_probe`) into a real subcommand.
//!
//! Phase 43 plan 01 extracted the battery, dependency graph, population-source partition and
//! Markdown report writer into the standalone `crates/probe` workspace member, so
//! `crates/workbench` can invoke the battery in-process without creating a
//! `workbench -> cli` dependency cycle. This module is now a thin wrapper: it owns the
//! CLI-specific concerns — `ProbeArgs` (clap), loading `AppConfig`, opening both SurrealDB
//! connections, table rendering and artifact writing — and calls
//! `urban_integrity_probe::run::execute` for the battery itself.
//!
//! # Single-config-load invariant (T-32-01)
//!
//! Both database connections this command opens — the analysis DB (`connect_db`) and the
//! `_catalog` DB (`connect_catalog`), the second needed only by `IdentityIntegrity` — are derived
//! from the SAME `AppConfig::load()` result inside one `execute()` call.
//! `OpencorporatesCrossJurisdiction`'s three boolean config gates are read from that same
//! `config` value, never a second independent load. There is no second, independent config read
//! that could resolve the two connections onto different analyses.
//!
//! Report-writing (`evidence-report.md`) is wired in `run()`, below: after the stdout table
//! prints, `urban_integrity_probe::report::render_evidence_report` assembles the full artifact
//! from the same `BatteryResult` and it is written to disk via `std::fs::write`.

use anyhow::Context;
use comfy_table::{Cell, Color, Table};
use urban_integrity_core::config::AppConfig;
use urban_integrity_probe::run::{BatteryResult, OcProbeGates, RunOptions};
use urban_integrity_schema::types::verdict::Verdict;

/// Arguments for `urban-integrity probe`.
///
/// `--output` and `--json` are the two flags beyond Python's argparse-free original
/// (the research notes' CLI Surface section) — `--output` exists so the report destination is
/// overridable without env vars, matching the `report` subcommand's own `--output-file`
/// convention; `--json` exists so the raw `Finding` battery can be captured in a machine-readable
/// shape independent of the Markdown report.
#[derive(Debug, clap::Args)]
pub struct ProbeArgs {
    /// Path to write `evidence-report.md` to. Defaults to `evidence-report.md` in the current
    /// working directory (matching Python's convention, per the research notes).
    #[arg(long)]
    pub output: Option<std::path::PathBuf>,

    /// Path to write the full 22-`Finding` battery to as JSON. Durable per RPT-01/D-05 — this
    /// flag itself is a normal CLI feature like `--output` — but the harness that CONSUMES this
    /// JSON (Plan 36-05's `compare.py`) is explicitly throwaway per D-07. Independent of
    /// `--output`: a `--json`-only invocation writes no Markdown report.
    #[arg(long)]
    pub json: Option<std::path::PathBuf>,

    /// Run the complete, ungated 22-probe battery — gating is the default (D-17); this flag opts
    /// out so a gated run can always be compared against the complete one (GAT-04). Note the
    /// dependency graph is still validated either way (GAT-03) -- this flag only chooses which
    /// dispatch function runs AFTER validation passes.
    #[arg(long)]
    pub full_battery: bool,
}

/// The testable core of the subcommand — `run` below is a thin table-rendering wrapper around
/// this. Loads config, connects both SurrealDB handles, then calls
/// `urban_integrity_probe::run::execute` with already-connected dependencies.
pub async fn execute(args: &ProbeArgs) -> anyhow::Result<BatteryResult> {
    let config = AppConfig::load().context("Config load failed")?;
    let db = crate::db::connect_db(&config).await?;
    let catalog = crate::registry::connect_catalog(&config).await?;
    let db_name = crate::db::resolve_analysis_db(&config).await?;

    urban_integrity_probe::run::execute(
        &db,
        catalog,
        db_name,
        OcProbeGates::from_config(&config),
        RunOptions {
            full_battery: args.full_battery,
        },
    )
    .await
}

/// Run the `probe` subcommand end-to-end: connect, run the battery, render a table, and write
/// `evidence-report.md`.
pub async fn run(args: ProbeArgs) -> anyhow::Result<()> {
    let result = execute(&args).await?;

    let mut table = Table::new();
    table.set_header(vec!["Probe", "Verdict", "Summary"]);
    for entry in &result.entries {
        let (label, color) = render_verdict(&entry.verdict);
        table.add_row(vec![
            Cell::new(&entry.probe),
            Cell::new(label).fg(color),
            Cell::new(&entry.summary),
        ]);
    }
    println!("{table}");

    // Both artifacts are attempted before either failure is reported. The battery has already run
    // by this point, so letting an unwritable `--json` path also cost the caller the Markdown
    // report — or vice versa — throws away work that succeeded. Failures are still fatal, just
    // reported after both attempts rather than short-circuiting the second (review finding L-2).
    let json_outcome = args.json.as_ref().map(|json_path| {
        let findings: Vec<&urban_integrity_schema::types::verdict::Finding> =
            result.entries.iter().map(|e| &e.finding).collect();
        serde_json::to_string_pretty(&findings)
            .context("Failed to serialize findings to JSON")
            .and_then(|json| write_artifact(json_path, &json))
            .map(|()| println!("JSON written to {}", json_path.display()))
    });

    let md =
        urban_integrity_probe::report::render_evidence_report(&result.entries, &result.strategy);
    let path = args
        .output
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("evidence-report.md"));
    let md_outcome =
        write_artifact(&path, &md).map(|()| println!("Report written to {}", path.display()));

    if let Some(outcome) = json_outcome {
        outcome?;
    }
    md_outcome?;

    Ok(())
}

/// Write one probe artifact, creating the parent directory if it does not exist.
///
/// `probe --output reports/evidence.md` used to fail outright when `reports/` was absent, which is
/// a surprising way for a run that has already done all its work to end (review finding L-3). Both
/// `--output` and `--json` go through here so the two flags behave identically — the previous
/// posture, where neither created directories, was at least consistent, and fixing one alone would
/// have traded one inconsistency for another.
fn write_artifact(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        // `parent()` is `Some("")` for a bare filename — creating that is both unnecessary and an
        // error on some platforms.
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create parent directory {}", parent.display())
            })?;
        }
    }
    std::fs::write(path, contents).with_context(|| format!("Failed to write to {}", path.display()))
}

/// Render a `Verdict` as a short label with its coverage figures where present, and a colour
/// matching the codebase's existing severity-colour convention (`crate::output::severity_color`).
fn render_verdict(v: &Verdict) -> (String, Color) {
    match v {
        Verdict::Hit { coverage, matches } => (
            format!(
                "HIT ({}/{}, matches={matches})",
                coverage.population(),
                coverage.capable()
            ),
            Color::Red,
        ),
        Verdict::Weak { coverage, matches } => (
            format!(
                "WEAK ({}/{}, matches={matches})",
                coverage.population(),
                coverage.capable()
            ),
            Color::Yellow,
        ),
        Verdict::Clear(coverage) => (
            format!("CLEAR ({}/{})", coverage.population(), coverage.capable()),
            Color::Green,
        ),
        Verdict::NotApplicable { coverage, reason } => (
            format!(
                "NOT_APPLICABLE ({}/{}) — {reason}",
                coverage.population(),
                coverage.capable()
            ),
            Color::Grey,
        ),
        Verdict::DataGap { missing_input } => {
            (format!("DATA_GAP — {missing_input}"), Color::DarkGrey)
        }
        Verdict::Blocked { prerequisite } => (format!("BLOCKED — {prerequisite}"), Color::Magenta),
        Verdict::Skipped { cause } => (format!("SKIPPED — {}", cause.describe()), Color::Cyan),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_verdict_skipped_dependency_labels_upstream() {
        let upstream = Verdict::classify(10, 10, 0, 1, "");
        let skipped = Verdict::skipped_dependency("identity-integrity", &upstream);

        let (label, _color) = render_verdict(&skipped);

        assert!(label.contains("SKIPPED"), "got: {label}");
        assert!(label.contains("identity-integrity"), "got: {label}");
    }

    #[test]
    fn render_verdict_skipped_tier_aggregate_labels_tier() {
        let skipped = Verdict::skipped_tier_aggregate("Tier-1", 7);

        let (label, _color) = render_verdict(&skipped);

        assert!(label.contains("SKIPPED"), "got: {label}");
        assert!(label.contains("Tier-1"), "got: {label}");
    }

    // ── Task 1: ProbeArgs.full_battery clap parsing ─────────────────────────────────────────

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ProbeArgs,
    }

    #[test]
    fn probe_args_default_parses_full_battery_false() {
        use clap::Parser;

        let cli = TestCli::try_parse_from(["test-cli"]).expect("bare `probe` must parse");
        assert!(
            !cli.args.full_battery,
            "gating must be the default (D-17) -- no flags means full_battery == false"
        );
    }

    #[test]
    fn probe_args_full_battery_flag_parses_true() {
        use clap::Parser;

        let cli = TestCli::try_parse_from(["test-cli", "--full-battery"])
            .expect("`--full-battery` must parse");
        assert!(cli.args.full_battery);
    }
}
