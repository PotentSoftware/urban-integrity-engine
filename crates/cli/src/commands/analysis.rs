//! `analysis` subcommand: provision (create) and inspect (list) analyses.

use anyhow::{bail, Context, Result};
use std::process::Stdio;
use urban_integrity_core::config::AppConfig;

use crate::registry::{self, AnalysisRow};
use urban_integrity_core::analysis::naming;

/// Whether an analysis provisioned at `created_at` (RFC3339) is still inside the
/// Phoenix detection window of `window_days`, measured against `now`.
///
/// Basis is the registry `created_at` (not event timestamps — the WAL `timestamp`
/// on graph_events is unreliable). `now` is a parameter so this stays pure and
/// offline-testable. An analysis is "inside the window" when it is younger than
/// `window_days`; such analyses hold evidence still within a live detection
/// window and are refused by `analysis remove --apply` unless `--decommission`.
fn within_window(
    created_at: &str,
    window_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<bool> {
    let created = chrono::DateTime::parse_from_rfc3339(created_at)
        .with_context(|| format!("cannot parse created_at as RFC3339: {created_at:?}"))?
        .with_timezone(&chrono::Utc);
    let age = now.signed_duration_since(created);
    Ok(age < chrono::Duration::days(window_days))
}

#[derive(Debug, clap::Args)]
pub struct AnalysisArgs {
    #[command(subcommand)]
    pub command: AnalysisCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum AnalysisCommand {
    /// Provision a new analysis (optionally cloning a baseline with --from)
    Create(CreateArgs),
    /// List provisioned analyses
    List,
    /// Remove (retire) an analysis: dry-run by default, --apply to drop
    Remove(RemoveArgs),
}

#[derive(Debug, clap::Args)]
pub struct CreateArgs {
    /// Human label, slugified into the database name (e.g. "Black Country 2026")
    #[arg(long)]
    pub label: String,
    /// File of ONS GSS ward codes, one per line
    #[arg(long)]
    pub wards_file: std::path::PathBuf,
    /// Baseline database to clone from (e.g. uk_national). Omit to provision empty.
    #[arg(long)]
    pub from: Option<String>,
    /// ONS boundary edition the ward codes are from (recorded in the registry)
    #[arg(long, default_value = "unknown")]
    pub boundary_vintage: String,
    /// Mark this analysis as a fixture target: `fixture seed` will only write into an analysis
    /// created with this flag.
    #[arg(long)]
    pub fixture: bool,
}

#[derive(Debug, clap::Args)]
pub struct RemoveArgs {
    /// The analysis database name to remove (the registry db_name, e.g. an_foo_deadbeef)
    pub db_name: String,
    /// Actually perform the removal. Without this, `remove` is a dry-run report.
    #[arg(long)]
    pub apply: bool,
    /// Permit removing an analysis still inside the Phoenix detection window —
    /// explicit intent to retire live evidence.
    #[arg(long)]
    pub decommission: bool,
}

/// Entry point used by main.rs — loads config, then dispatches.
pub async fn run(args: AnalysisArgs) -> Result<()> {
    let config = AppConfig::load().context("Config load failed")?;
    match args.command {
        AnalysisCommand::Create(create) => create_analysis(&config, create).await,
        AnalysisCommand::List => list(&config).await,
        AnalysisCommand::Remove(remove) => remove_analysis(&config, remove).await,
    }
}

/// Provision a new analysis: validate wards, compute the content-addressable
/// db name, guard idempotency + baseline existence, then clone (optional),
/// apply schema, create the checkpoint dir, and flip the registry row to active.
pub async fn create_analysis(config: &AppConfig, args: CreateArgs) -> Result<()> {
    // 1. Read + validate ward codes.
    let contents = std::fs::read_to_string(&args.wards_file)
        .with_context(|| format!("cannot read wards file {:?}", args.wards_file))?;
    let ward_codes: Vec<String> = contents
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    if ward_codes.is_empty() {
        bail!("wards file {:?} contained no ward codes", args.wards_file);
    }
    for code in &ward_codes {
        naming::validate_ward_code(code).context("invalid ward code in wards file")?;
    }

    // 2. Compute the content-addressable db name; refuse reserved targets.
    let db_name = naming::analysis_db_name(&args.label, &ward_codes)?;
    if naming::is_reserved(&db_name) {
        bail!("'{db_name}' is a reserved name and cannot be a create target");
    }
    let wardset_hash = naming::wardset_hash_full(&ward_codes)?;
    let checkpoint_path = naming::checkpoint_path_for(&db_name);

    // 3. Registry idempotency guard.
    let cat = registry::connect_catalog(config).await?;
    if let Some(existing) = registry::get_analysis(&cat, &db_name).await? {
        bail!(
            "analysis already exists: {} (label {:?}, status {}) — same ward set",
            existing.db_name,
            existing.label,
            existing.status
        );
    }

    // 4. Baseline existence guard (spec §7): a missing --from DB must error
    //    BEFORE any provisioning, otherwise the clone would silently be empty.
    if let Some(ref baseline) = args.from {
        if !database_exists(&cat, baseline).await? {
            bail!(
                "--from baseline database '{baseline}' does not exist in namespace '{}'",
                config.surreal.namespace
            );
        }
    }

    // 5. Write the registry row as `provisioning` FIRST, so an interrupted create
    //    never leaves an `active` half-built analysis (spec §7).
    let now = chrono::Utc::now().to_rfc3339();
    let row = AnalysisRow {
        db_name: db_name.clone(),
        label: args.label.clone(),
        ward_codes: ward_codes.clone(),
        wardset_hash,
        baseline_source: args.from.clone(),
        baseline_vintage: args.from.as_ref().map(|_| now.clone()),
        boundary_vintage: Some(args.boundary_vintage.clone()),
        schema_version: 1,
        checkpoint_path: checkpoint_path.clone(),
        status: "provisioning".into(),
        created_at: now,
        retired_at: None,
        notes: None,
        coverage_baseline_json: None,
        fixture: args.fixture,
    };
    registry::insert_analysis(&cat, &row).await?;

    // 6. Clone the baseline (if any) → new DB. On failure, tear down the
    //    half-built DB and leave the row as `provisioning` (never `active`).
    if let Some(ref baseline) = args.from {
        if let Err(e) = clone_baseline(config, baseline, &db_name).await {
            let _ = teardown_db(&cat, &db_name).await;
            bail!("clone from '{baseline}' failed: {e}");
        }
    }

    // Ensure schema present on the new DB (idempotent) even when provisioned empty.
    crate::db::connect_to_db(config, &db_name).await?;
    std::fs::create_dir_all(&checkpoint_path)
        .with_context(|| format!("cannot create checkpoint dir {checkpoint_path}"))?;

    // 7. Flip to active only on full success.
    registry::set_status(&cat, &db_name, "active").await?;

    println!("Provisioned analysis {db_name}");
    println!("  label:      {}", args.label);
    println!("  wards:      {}", ward_codes.len());
    println!(
        "  baseline:   {}",
        args.from.as_deref().unwrap_or("(none, empty)")
    );
    println!("  checkpoint: {checkpoint_path}");
    Ok(())
}

/// List provisioned analyses (newest first).
pub async fn list(config: &AppConfig) -> Result<()> {
    let cat = registry::connect_catalog(config).await?;
    let rows = registry::list_analyses(&cat).await?;
    if rows.is_empty() {
        println!("No analyses provisioned.");
        return Ok(());
    }
    println!(
        "{:<32} {:<24} {:>6} {:<14} {:<12} CREATED",
        "DB_NAME", "LABEL", "WARDS", "BASELINE", "STATUS"
    );
    for r in rows {
        println!(
            "{:<32} {:<24} {:>6} {:<14} {:<12} {}",
            r.db_name,
            r.label,
            r.ward_codes.len(),
            r.baseline_source.as_deref().unwrap_or("-"),
            r.status,
            r.created_at
        );
    }
    Ok(())
}

/// Remove (retire) an analysis. Dry-run by default; `--apply` drops the database,
/// deletes its checkpoint dir, and marks the registry row `retired`.
pub async fn remove_analysis(config: &AppConfig, args: RemoveArgs) -> Result<()> {
    // 1. Policy hard-stop — reserved databases are NEVER removable, even with
    //    --apply --decommission. Checked before any DB work.
    if naming::is_reserved(&args.db_name) {
        bail!(
            "'{}' is a reserved database and can never be removed",
            args.db_name
        );
    }

    // 2. Look up the registry row — remove operates only on tracked analyses.
    let cat = registry::connect_catalog(config).await?;
    let row = registry::get_analysis(&cat, &args.db_name)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown analysis '{}' — run `urban-integrity analysis list` to see known analyses",
                args.db_name
            )
        })?;
    if row.status == "retired" {
        bail!("analysis '{}' is already retired", args.db_name);
    }

    // 3. Retention window (D6): read the window from detector config so the
    //    guardrail can't drift from the detector. The fallback matches
    //    DetectorConfigRaw's own default (180) — an absent config value must not
    //    make the guard behave differently from the detector itself.
    let window_days = config.detector.phoenix_window_days.unwrap_or(180);
    let in_window = within_window(&row.created_at, window_days, chrono::Utc::now())?;

    // 4. Event count for the report (best-effort; the analysis DB may be empty).
    // This figure feeds the operator's decision to destroy an analysis. `unwrap_or(0)` made an
    // UNREACHABLE database display as "events: 0" — i.e. "empty, safe to remove" — on exactly
    // the evidence that could not support it. Show the failure instead of vouching for 0.
    let events = match count_graph_events(config, &row.db_name).await {
        Ok(n) => n.to_string(),
        Err(e) => format!("UNAVAILABLE ({e:#})"),
    };

    // 5. Dry-run (default): report and stop, mutating nothing.
    if !args.apply {
        println!("DRY RUN — would remove analysis {}", row.db_name);
        println!("  label:      {}", row.label);
        println!("  wards:      {}", row.ward_codes.len());
        println!("  events:     {events}");
        println!("  created_at: {}", row.created_at);
        println!(
            "  in Phoenix window ({window_days}d): {}",
            if in_window {
                "YES — needs --decommission"
            } else {
                "no"
            }
        );
        println!("Pass --apply to remove.");
        return Ok(());
    }

    // 6. Apply: refuse to retire evidence still inside the window unless the
    //    operator explicitly decommissions.
    if in_window && !args.decommission {
        bail!(
            "analysis '{}' holds data inside the {window_days}-day Phoenix window; \
             pass --decommission to retire it deliberately",
            row.db_name
        );
    }

    // Crash-safe ordering: mark `retiring`, drop the DB, delete the checkpoint
    // dir, then stamp `retired`. An interrupted remove stays `retiring` so
    // nobody trusts a half-dropped analysis.
    registry::set_status(&cat, &row.db_name, "retiring").await?;
    teardown_db(&cat, &row.db_name).await?;
    match std::fs::remove_dir_all(&row.checkpoint_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| {
                format!("failed to delete checkpoint dir {}", row.checkpoint_path)
            });
        }
    }
    let now = chrono::Utc::now().to_rfc3339();
    registry::mark_retired(&cat, &row.db_name, &now).await?;

    println!("Removed analysis {} (retired {now})", row.db_name);
    Ok(())
}

/// Count `graph_events` in an analysis DB (report helper). Connecting applies
/// SCHEMA_DEF idempotently; a fresh/empty DB simply returns 0.
async fn count_graph_events(config: &AppConfig, db_name: &str) -> Result<i64> {
    let db = crate::db::connect_to_db(config, db_name).await?;
    let mut r = db
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await
        .context("graph_events count query failed")?;
    let rows: Vec<serde_json::Value> = r.take(0).context("graph_events count decode failed")?;
    // Fail-loud posture (WR-01 class): no row is a genuinely empty table; a row without an
    // integer `count` is a response-shape change, not a 0.
    match rows.first() {
        None => Ok(0),
        Some(r) => r.get("count").and_then(|v| v.as_i64()).ok_or_else(|| {
            anyhow::anyhow!(
                "graph_events count: response row carried no integer `count` field \
                 (SurrealDB response shape changed?): {r}"
            )
        }),
    }
}

/// Whether a database named `name` exists in the current namespace.
/// Uses `INFO FOR NS`, whose `databases` field is a map of name → definition.
pub async fn database_exists(
    cat: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    name: &str,
) -> Result<bool> {
    let mut r = cat
        .query("INFO FOR NS")
        .await
        .context("INFO FOR NS failed")?;
    // Decode as a Vec (proven SDK pattern in registry.rs); INFO FOR NS yields a
    // single object row whose `databases` field maps db name → definition.
    let rows: Vec<serde_json::Value> = r.take(0).context("INFO FOR NS decode failed")?;
    let exists = rows
        .first()
        .and_then(|v| v.get("databases"))
        .and_then(|v| v.as_object())
        .map(|m| m.contains_key(name))
        .unwrap_or(false);
    Ok(exists)
}

/// Drop a database (best-effort teardown of a failed clone). `db_name` is a
/// validated `^[a-z][a-z0-9_]*$` identifier, so the interpolation is injection-safe.
async fn teardown_db(
    cat: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    db_name: &str,
) -> Result<()> {
    cat.query(format!("REMOVE DATABASE IF EXISTS `{db_name}`"))
        .await
        .context("teardown REMOVE DATABASE failed")?;
    Ok(())
}

/// Locate the `surreal` binary: `SURREAL_BIN` env, else `~/.surrealdb/surreal`,
/// else `surreal` on PATH (mirrors scripts/rebuild-anglesey.sh).
fn surreal_bin() -> String {
    if let Ok(p) = std::env::var("SURREAL_BIN") {
        return p;
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = format!("{home}/.surrealdb/surreal");
        if std::path::Path::new(&candidate).is_file() {
            return candidate;
        }
    }
    "surreal".to_string()
}

/// Clone one database into another (whole-DB) via `surreal export | import`.
///
/// One-directional and read-only on the source (spec §4 contamination guard):
/// this NEVER writes back to `src_db`. Carries `graph_events` + cursor tables so
/// streaming can later resume against the clone.
pub async fn clone_baseline(config: &AppConfig, src_db: &str, dst_db: &str) -> Result<()> {
    let bin = surreal_bin();
    let endpoint = config
        .surreal
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");
    let user = config.surreal.username.as_deref().unwrap_or("root");
    let pass = config.surreal.password.as_deref().unwrap_or("root");
    let ns = &config.surreal.namespace;

    let tmp =
        std::env::temp_dir().join(format!("ui-clone-{}.surql", uuid::Uuid::new_v4().simple()));
    let tmp_str = tmp.to_string_lossy().to_string();

    // Export source → temp file.
    let export = tokio::process::Command::new(&bin)
        .args([
            "export",
            "--endpoint",
            &endpoint,
            "-u",
            user,
            "-p",
            pass,
            "--namespace",
            ns,
            "--database",
            src_db,
            &tmp_str,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| {
            format!("failed to launch `{bin} export` — is the surreal binary present?")
        })?;
    if !export.status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "surreal export of '{src_db}' failed: {}",
            String::from_utf8_lossy(&export.stderr)
        );
    }

    // Import temp file → destination.
    let import = tokio::process::Command::new(&bin)
        .args([
            "import",
            "--endpoint",
            &endpoint,
            "-u",
            user,
            "-p",
            pass,
            "--namespace",
            ns,
            "--database",
            dst_db,
            &tmp_str,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("failed to launch `{bin} import`"))?;

    let _ = std::fs::remove_file(&tmp);

    if !import.status.success() {
        bail!(
            "surreal import into '{dst_db}' failed: {}",
            String::from_utf8_lossy(&import.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn within_window_true_for_recent() {
        let now = Utc::now();
        let created = (now - Duration::days(10)).to_rfc3339();
        assert!(within_window(&created, 180, now).unwrap());
    }

    #[test]
    fn within_window_false_for_old() {
        let now = Utc::now();
        let created = (now - Duration::days(800)).to_rfc3339();
        assert!(!within_window(&created, 180, now).unwrap());
    }

    #[test]
    fn within_window_boundary_just_inside() {
        let now = Utc::now();
        // 179 days old with a 180-day window → still inside.
        let created = (now - Duration::days(179)).to_rfc3339();
        assert!(within_window(&created, 180, now).unwrap());
    }

    #[test]
    fn within_window_boundary_just_outside() {
        let now = Utc::now();
        // 181 days old with a 180-day window → outside.
        let created = (now - Duration::days(181)).to_rfc3339();
        assert!(!within_window(&created, 180, now).unwrap());
    }

    #[test]
    fn within_window_rejects_malformed() {
        let now = Utc::now();
        assert!(within_window("not-a-date", 180, now).is_err());
        assert!(within_window("", 180, now).is_err());
    }
}
