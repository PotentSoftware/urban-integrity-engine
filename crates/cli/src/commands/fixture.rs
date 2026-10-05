//! `urban-integrity fixture seed --planted --analysis <slug>` (spec §4.1): writes the planted
//! fixture into an analysis provisioned with `analysis create --fixture`, and nothing else.

use anyhow::{bail, Context, Result};
use urban_integrity_core::config::AppConfig;

#[derive(Debug, clap::Args)]
pub struct FixtureArgs {
    #[command(subcommand)]
    pub command: FixtureCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum FixtureCommand {
    /// Seed a fixture graph into the selected analysis database.
    Seed(SeedArgs),
}

#[derive(Debug, clap::Args)]
pub struct SeedArgs {
    /// Write the planted fixture (`test-support::planted`).
    #[arg(long)]
    pub planted: bool,
    /// Wipe `graph_events` in the target first. Required when the table is not empty.
    #[arg(long)]
    pub reseed: bool,
}

/// Rows of a `SELECT count() … GROUP ALL` response, reduced to the count.
///
/// Mirrors `core::persistence::oracle`'s private `extract_count` (which this crate cannot reach)
/// and keeps its distinction: NO row means an empty match set — a legitimate 0 — while a row
/// lacking an integer `count` means the response shape changed under a SurrealDB bump, which is
/// an error, never a 0. `unwrap_or(0)` conflates the two, and a silent 0 here would let the
/// seeder decide a populated `graph_events` was empty and skip the `--reseed` guard entirely.
fn extract_count(rows: &[serde_json::Value]) -> Result<i64> {
    let Some(row) = rows.first() else {
        return Ok(0);
    };
    row.get("count")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "graph_events count returned a row without an integer `count` field ({row}); \
                 refusing to treat an unreadable count as zero"
            )
        })
}

/// The guard, separated so it is unit-testable without a DB.
pub fn check_target(row_fixture: bool, db_name: &str) -> Result<()> {
    if !row_fixture {
        bail!(
            "analysis '{db_name}' was not created with `analysis create --fixture`; refusing to \
             seed a fixture into it"
        );
    }
    Ok(())
}

pub async fn run(args: FixtureArgs) -> Result<()> {
    let config = AppConfig::load().context("Config load failed")?;
    match args.command {
        FixtureCommand::Seed(seed) => seed_cmd(&config, seed).await,
    }
}

async fn seed_cmd(config: &AppConfig, args: SeedArgs) -> Result<()> {
    if !args.planted {
        bail!("nothing to seed: pass --planted");
    }
    let Some(analysis_id) = config.analysis.as_deref() else {
        bail!("pass --analysis <slug>; the seeder never targets the default database");
    };
    let db_name = crate::db::resolve_analysis_db(config).await?;
    let catalog = crate::registry::connect_catalog(config).await?;
    let row = crate::registry::get_analysis(&catalog, analysis_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("unknown analysis '{analysis_id}'"))?;
    check_target(row.fixture, &db_name)?;

    let db = crate::db::connect_db(config).await?;
    let rows: Vec<serde_json::Value> = db
        .query("SELECT count() FROM graph_events GROUP ALL")
        .await?
        .take(0)?;
    let existing = extract_count(&rows)?;
    if existing > 0 {
        if !args.reseed {
            bail!("graph_events already holds {existing} rows in '{db_name}'; pass --reseed to wipe and rewrite");
        }
        db.query("DELETE graph_events").await?.check()?;
        println!("Wiped {existing} graph_events rows from {db_name}");

        // `seed_planted` also writes `identity_links` (Phase 49), `enrichment_runs` (Phase 49),
        // and (Phase 50 plan 10) `land_flip_runs`/`land_flip_gate1_survivors`. Only `graph_events`
        // was wiped above until now, so a second `--reseed` on the same analysis DB left stale
        // rows behind: `identity_links`' `CREATE type::record('identity_links', $key)` errors
        // outright on a duplicate key (`Database record 'identity_links:planted-psc1-a' already
        // exists`), and `land_flip_gate1_survivors`' plain `CREATE` (no fixed key) would have
        // silently accumulated a second survivor row per reseed, breaking `land-flip-candidates`'
        // own D-17 invariant (`survivors.len() == population`) on the very next probe run. This
        // analysis DB (`analysis create --fixture`) holds nothing but the planted fixture, so a
        // full wipe of these four tables is safe and keeps the script's own "idempotent" doc
        // comment (`scripts/toy-analysis.sh`) true. `enrichment_runs`/`land_flip_runs:current` are
        // UPSERTed by `seed_planted` (no key-collision risk), but wiping them here too keeps every
        // reseed byte-identical rather than merely non-erroring.
        for table in [
            "identity_links",
            "enrichment_runs",
            "land_flip_runs",
            "land_flip_gate1_survivors",
        ] {
            db.query(format!("DELETE {table}")).await?.check()?;
        }
    }

    let graph = urban_integrity_test_support::planted::planted_graph();
    let (nodes, edges) = urban_integrity_test_support::events::seed_planted(&db, &graph).await?;
    println!("Seeded planted fixture into {db_name}: {nodes} nodes, {edges} edges");
    println!(
        "Manifest: {} reachable nodes from ward {}",
        graph.manifest.reachable_nodes, graph.manifest.ward_code
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_non_fixture_analysis() {
        let err = check_target(false, "an_anglesey_aebaa785").unwrap_err();
        assert!(err.to_string().contains("--fixture"));
    }

    #[test]
    fn accepts_fixture_analysis() {
        check_target(true, "an_planted_fixture_00000000").unwrap();
    }

    #[test]
    fn no_row_is_an_empty_table_not_an_error() {
        assert_eq!(extract_count(&[]).unwrap(), 0);
    }

    #[test]
    fn a_row_with_a_count_reads_it() {
        let rows = vec![serde_json::json!({ "count": 82 })];
        assert_eq!(extract_count(&rows).unwrap(), 82);
    }

    /// The state `unwrap_or(0)` would have silently reported as an empty table, letting the
    /// seeder skip `--reseed` and write the fixture on top of existing rows.
    #[test]
    fn a_row_without_an_integer_count_is_an_error() {
        let rows = vec![serde_json::json!({ "total": 82 })];
        let err = extract_count(&rows).unwrap_err();
        assert!(err.to_string().contains("refusing to treat"));
    }
}
