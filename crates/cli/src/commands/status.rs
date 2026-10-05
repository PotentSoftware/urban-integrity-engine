//! `status` — what is actually in the selected analysis database.
//!
//! # Why this reads the database rather than the engine
//!
//! This command shipped as a stub: it printed four zero literals with a
//! `// Stub — real stats fetched from gRPC GetGraphStats in Phase 5 plan 03` comment, never
//! loaded `AppConfig`, and never opened a connection. It therefore exited 0 with
//! `Nodes: 0, Edges: 0` against a populated database, an empty one, and no database at all —
//! three states a status command exists to tell apart. `docs/GETTING-STARTED.md` had been
//! recommending it as the way to confirm a fresh install had reached SurrealDB.
//!
//! The original plan was to fetch the numbers over gRPC from a running `serve`. That is the
//! wrong source for a CLI: `GetGraphStats` reports the in-process ArcSwap CSR snapshot
//! (`crates/api/src/grpc/integrity.rs`), which exists only while a server is up, so `status`
//! would report nothing whenever it is most likely to be run — before the server starts, or
//! after an ingest, from an operator's shell. Every other read-only subcommand goes through
//! `connect_db`, and so does this one, which also makes `--analysis` work for free.
//!
//! # What the numbers mean
//!
//! Two different things are reported, and conflating them is the mistake this module is
//! shaped to avoid:
//!
//! - **`graph_events`** is the append-only log — the ingested truth, counted live.
//! - **nodes / edges** are read from `projection_runs:current`, the marker the projector
//!   stamps. They describe *what the last completed projection wrote*, not what is in the
//!   projected tables right now.
//!
//! Those two can legitimately disagree: `project` is destructive-then-rebuild and takes over
//! an hour on the live corpus, so events ingested since the last projection are in the log and
//! not yet in the graph. Reporting a single fused "node count" would hide that.
//!
//! `finished_at_ms` is the only field that distinguishes a complete projection from one that
//! died — `project` wipes and rebuilds, so an interrupted run leaves tables that look exactly
//! like a finished smaller graph (`crates/schema/src/db.rs`, and the 2026-08-11 incident it
//! records). This command therefore reports three projection states, never two, and never
//! collapses "no marker" into "complete" the way a bare boolean would.

use crate::db::connect_db;
use crate::output::{print_json, OutputFormat};
use anyhow::Context;
use serde::Serialize;
use urban_integrity_core::config::AppConfig;

#[derive(Debug, clap::Args)]
pub struct StatusArgs {
    /// Output format
    #[arg(long, value_enum)]
    pub output: Option<OutputFormat>,
}

/// The three states of `projection_runs:current`, kept distinct on purpose.
///
/// A two-state boolean would have to fold `NeverRun` into one of the other two, and both
/// foldings are wrong: calling it complete reports an unprojected database as ready, and
/// calling it unfinished reports a fresh install as damaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionState {
    /// `finished_at_ms` is present and non-null — the projector stamped the success path.
    Complete,
    /// A marker exists but carries no `finished_at_ms`: the run is either still going or died
    /// (SIGKILL included, which no in-process handler could catch). The projected tables may
    /// be a partial rebuild.
    Unfinished,
    /// No marker at all — `project` has never run against this database, or it predates the
    /// marker. Either way the projected tables cannot be vouched for.
    NeverRun,
}

impl ProjectionState {
    fn label(self) -> &'static str {
        match self {
            ProjectionState::Complete => "complete",
            ProjectionState::Unfinished => "UNFINISHED — partial or still running",
            ProjectionState::NeverRun => "never run",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EngineStatus {
    /// The database actually connected to, after `--analysis` resolution.
    pub database: String,
    /// Live `count()` over the append-only event log.
    pub graph_events: u64,
    pub projection: ProjectionState,
    /// Millisecond epoch stamped on the success path only; `None` unless `Complete`.
    pub projection_finished_at_ms: Option<i64>,
    /// Written by the last projection run. `None` when no marker carries the field, which is
    /// not the same as zero — a marker without counts is a shape change, not an empty graph.
    pub nodes: Option<u64>,
    pub edges: Option<u64>,
    /// Edges the projector could not attach (unknown endpoint). Part of the marker, and worth
    /// surfacing: a large value means the projection completed while dropping data.
    pub edges_skipped: Option<u64>,
}

/// Classify the marker row into one of the three states.
///
/// Deliberately NOT the `projection_is_complete(None) == true` convention used inside the PPD
/// adapter (`crates/enricher/src/adapters/land_registry_ppd.rs`). That function answers "may I
/// derive an allowlist from this?", where treating a pre-marker database as usable is the
/// right call. `status` answers "what is the state?", where reporting a database that has
/// never been projected as `complete` would be a lie.
fn classify_projection(marker: Option<&serde_json::Value>) -> ProjectionState {
    match marker {
        None => ProjectionState::NeverRun,
        Some(row) => match row.get("finished_at_ms") {
            Some(v) if !v.is_null() => ProjectionState::Complete,
            _ => ProjectionState::Unfinished,
        },
    }
}

/// Read a `u64` field from the marker, distinguishing absent from zero.
///
/// `unwrap_or(0)` here would reproduce the exact defect this codebase has paid for repeatedly:
/// a field that vanished from the response reads as a legitimate count of nothing.
fn marker_u64(marker: Option<&serde_json::Value>, field: &str) -> Option<u64> {
    marker?.get(field)?.as_u64()
}

fn marker_i64(marker: Option<&serde_json::Value>, field: &str) -> Option<i64> {
    marker?.get(field)?.as_i64()
}

/// Extract `count` from a `GROUP ALL` response.
///
/// Three outcomes, not two. An empty response is a real zero — SurrealDB returns no row at all
/// for an empty match set. A row that came back *without* an integer `count` field is an error,
/// because it means the response shape changed under a SurrealDB bump; reporting that as 0
/// would be the ICIJ silent-write failure again, where a log claimed 814k rows written against
/// an empty table.
fn extract_count(rows: &[serde_json::Value]) -> anyhow::Result<u64> {
    let Some(row) = rows.first() else {
        return Ok(0);
    };
    row.get("count")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "SurrealDB returned a row from `SELECT count() FROM graph_events GROUP ALL` with \
                 no integer `count` field: {row}. Refusing to report this as 0 — an unexpected \
                 response shape is not an empty table."
            )
        })
}

/// Thousands separators. Local rather than reaching into `output.rs`'s private `format_gbp`,
/// which is `f64`-based and would round a count through a float.
fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

fn render_optional(value: Option<u64>) -> String {
    match value {
        Some(n) => format_count(n),
        None => "unknown (not recorded in the projection marker)".to_string(),
    }
}

pub async fn run(args: StatusArgs) -> anyhow::Result<()> {
    let fmt = args.output.unwrap_or_else(OutputFormat::auto);
    if matches!(fmt, OutputFormat::Markdown | OutputFormat::Pdf) {
        anyhow::bail!("--format md/pdf is only supported by the report command");
    }

    let config = AppConfig::load().context("Config load failed")?;
    let database = crate::db::resolve_analysis_db(&config).await?;
    let db = connect_db(&config).await?;

    // Bare server-side aggregate: no `kind` equality, no `CONTAINS`, no bare `LIMIT`, so
    // neither query_shape_guard rule applies. `count()` on a predicate is the proven-safe
    // family (see `crates/core/src/persistence/oracle.rs`'s `count_events`).
    let event_rows: Vec<serde_json::Value> = db
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await
        .context("Counting graph_events failed")?
        .take(0)
        .context("Unexpected response shape counting graph_events")?;
    let graph_events = extract_count(&event_rows)?;

    // A failed marker read is NOT "no marker". Collapsing the two would report a wedged
    // instance or an auth failure as a database that has simply never been projected.
    let marker_rows: Vec<serde_json::Value> = db
        .query("SELECT * FROM projection_runs:current")
        .await
        .context("Reading projection_runs:current failed")?
        .take(0)
        .context("Unexpected response shape reading projection_runs:current")?;
    let marker = marker_rows.first();

    let status = EngineStatus {
        database,
        graph_events,
        projection: classify_projection(marker),
        projection_finished_at_ms: marker_i64(marker, "finished_at_ms"),
        nodes: marker_u64(marker, "nodes_upserted"),
        edges: marker_u64(marker, "edges_related"),
        edges_skipped: marker_u64(marker, "edges_skipped"),
    };

    match fmt {
        OutputFormat::Json => print_json(&status)?,
        OutputFormat::Table => {
            println!("Engine Status");
            println!("  Database:      {}", status.database);
            println!("  Graph events:  {}", format_count(status.graph_events));
            println!();
            println!("  Projection:    {}", status.projection.label());
            match status.projection {
                ProjectionState::Complete => {
                    println!("    Nodes:         {}", render_optional(status.nodes));
                    println!("    Edges:         {}", render_optional(status.edges));
                    println!(
                        "    Edges skipped: {}",
                        render_optional(status.edges_skipped)
                    );
                }
                ProjectionState::Unfinished => {
                    println!(
                        "    The projected tables are a partial rebuild or a run still in \
                         flight. `project` wipes before it rebuilds, so a partial projection is \
                         indistinguishable from a finished smaller graph by row count alone — \
                         only this marker tells them apart. Re-run `project` before trusting \
                         any graph read."
                    );
                }
                ProjectionState::NeverRun => {
                    println!(
                        "    No projection marker. The projected node and relation tables have \
                         not been built from the {} event(s) above — run `project`.",
                        format_count(status.graph_events)
                    );
                }
            }
        }
        OutputFormat::Markdown | OutputFormat::Pdf => unreachable!("rejected above"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_engine_status_serializes_all_fields() {
        let s = EngineStatus {
            database: "an_anglesey_aebaa785".to_string(),
            graph_events: 977_302,
            projection: ProjectionState::Complete,
            projection_finished_at_ms: Some(1_787_433_819_730),
            nodes: Some(358_467),
            edges: Some(507_179),
            edges_skipped: Some(99),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"graph_events\":977302"), "got: {json}");
        assert!(json.contains("\"nodes\":358467"), "got: {json}");
        assert!(json.contains("\"edges\":507179"), "got: {json}");
        assert!(json.contains("\"edges_skipped\":99"), "got: {json}");
        assert!(json.contains("\"projection\":\"complete\""), "got: {json}");
        assert!(
            json.contains("\"database\":\"an_anglesey_aebaa785\""),
            "got: {json}"
        );
    }

    /// The three states must stay three. Folding `NeverRun` into either neighbour is the
    /// specific mistake this enum exists to prevent.
    #[test]
    fn classify_projection_distinguishes_all_three_states() {
        assert_eq!(classify_projection(None), ProjectionState::NeverRun);
        assert_eq!(
            classify_projection(Some(&json!({ "started_at_ms": 1 }))),
            ProjectionState::Unfinished,
            "a marker with no finished_at_ms is a run that died or is still going"
        );
        assert_eq!(
            classify_projection(Some(&json!({ "finished_at_ms": null }))),
            ProjectionState::Unfinished,
            "an explicit null finished_at_ms must not read as complete"
        );
        assert_eq!(
            classify_projection(Some(&json!({ "finished_at_ms": 1_787_433_819_730i64 }))),
            ProjectionState::Complete
        );
    }

    /// `finished_at_ms: 0` is a real stamp, not a falsy absence. The PPD adapter's own tests
    /// pin the opposite convention for its own question, so this is worth stating here.
    #[test]
    fn classify_projection_treats_a_zero_timestamp_as_complete() {
        assert_eq!(
            classify_projection(Some(&json!({ "finished_at_ms": 0 }))),
            ProjectionState::Complete
        );
    }

    #[test]
    fn marker_fields_distinguish_absent_from_zero() {
        let complete = json!({ "nodes_upserted": 0, "edges_related": 12 });
        assert_eq!(marker_u64(Some(&complete), "nodes_upserted"), Some(0));
        assert_eq!(marker_u64(Some(&complete), "edges_related"), Some(12));
        assert_eq!(
            marker_u64(Some(&complete), "edges_skipped"),
            None,
            "a field the marker does not carry must be None, never 0"
        );
        assert_eq!(marker_u64(None, "nodes_upserted"), None);
    }

    #[test]
    fn extract_count_reads_an_empty_response_as_zero() {
        assert_eq!(extract_count(&[]).unwrap(), 0);
    }

    #[test]
    fn extract_count_reads_a_real_count() {
        assert_eq!(
            extract_count(&[json!({ "count": 977_302 })]).unwrap(),
            977_302
        );
    }

    /// The whole point of `extract_count`: a response whose shape changed must fail loudly
    /// rather than report the database as empty.
    #[test]
    fn extract_count_refuses_a_row_without_an_integer_count() {
        let err = extract_count(&[json!({ "total": 5 })])
            .expect_err("a row with no `count` field must be an error, not 0");
        assert!(err.to_string().contains("not an empty table"), "got: {err}");

        let err = extract_count(&[json!({ "count": "977302" })])
            .expect_err("a string count is a shape change, not a count");
        assert!(
            err.to_string().contains("no integer `count` field"),
            "got: {err}"
        );
    }

    #[test]
    fn format_count_groups_thousands() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(99), "99");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(977_302), "977,302");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    /// Restored from the pre-2026-09-03 stub's test module. `test_status_run_json_output_ok`
    /// and `test_status_run_table_output_ok` were NOT restored: they asserted `run(..).is_ok()`,
    /// which only held because the stub never opened a connection. Against the real command they
    /// would need a live SurrealDB, so they would have to become DB-gated -- and a gated
    /// `is_ok()` assertion proves nothing the pure functions above do not already prove, while
    /// costing a skip-by-return that reads as passing when it never ran.
    ///
    /// These two survive ungated because the md/pdf rejection is deliberately checked at the top
    /// of `run`, before `AppConfig::load`. That ordering is the thing under test: a caller asking
    /// for an unsupported format gets told so, rather than a connection error from a database it
    /// was never going to print to.
    #[tokio::test]
    async fn markdown_output_is_rejected_before_any_connection_is_attempted() {
        let result = run(StatusArgs {
            output: Some(OutputFormat::Markdown),
        })
        .await;
        let msg = result
            .expect_err("Markdown output must return Err for status")
            .to_string();
        assert!(
            msg.contains("report command"),
            "the error must name the command that does support md, got: {msg}"
        );
        assert!(
            !msg.contains("Config load failed") && !msg.contains("SurrealDB"),
            "the format check must run BEFORE config load and connect, so this must not be a \
             connection or config error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn pdf_output_is_rejected_before_any_connection_is_attempted() {
        let result = run(StatusArgs {
            output: Some(OutputFormat::Pdf),
        })
        .await;
        let msg = result
            .expect_err("Pdf output must return Err for status")
            .to_string();
        assert!(msg.contains("report command"), "got: {msg}");
        assert!(
            !msg.contains("Config load failed") && !msg.contains("SurrealDB"),
            "the format check must run BEFORE config load and connect, got: {msg}"
        );
    }

    #[test]
    fn render_optional_never_shows_a_missing_field_as_zero() {
        assert_eq!(render_optional(Some(0)), "0");
        assert_eq!(
            render_optional(None),
            "unknown (not recorded in the projection marker)"
        );
    }
}
