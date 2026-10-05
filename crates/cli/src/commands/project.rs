use anyhow::Result;
use clap::Args;
use std::sync::Arc;
use tracing::info;
use urban_integrity_core::config::AppConfig;
use urban_integrity_ingestor::projector::{Projector, RawSqlConfig};

use crate::db::{connect_db, resolve_analysis_db};

#[derive(Debug, Args)]
pub struct ProjectArgs {
    /// Show what would happen without writing to the native tables.
    /// (Currently a no-op placeholder — full dry-run requires deeper refactor.)
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: ProjectArgs) -> Result<()> {
    if args.dry_run {
        println!("Dry-run not yet implemented for project — remove --dry-run to proceed.");
        return Ok(());
    }

    let config = AppConfig::load()?;
    let db = Arc::new(connect_db(&config).await?);

    // Resolve `--analysis` to a database name ONCE and use it for both halves of the
    // projection.
    //
    // These two connections must agree. `connect_db` honours `--analysis`, but the raw
    // HTTP config below used to read `config.surreal.database` straight from config.toml
    // — so `project --analysis X` upserted nodes into X while relating edges into
    // whatever config.toml happened to point at. Measured 2026-07-28: a run against a
    // test analysis wrote 37,211 `awarded_to`, 80,435 `located_at` and 8,295 `issued_by`
    // rows into a DIFFERENT, live analysis, and left the target's relation tables empty.
    // The projector wipes relation tables before writing, so this silently clobbered
    // derived data belonging to an analysis the operator never named.
    let db_name = resolve_analysis_db(&config).await?;

    // Build the raw HTTP SQL config used by Projector::relate_edge to bypass the
    // SurrealDB 3.0.x server / 3.1.x SDK flatbuffers codec mismatch on relation records.
    let http_base = config
        .surreal
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");
    let raw = RawSqlConfig {
        base_url: http_base,
        namespace: config.surreal.namespace.clone(),
        database: db_name.clone(),
        auth: match (
            config.surreal.username.as_deref(),
            config.surreal.password.as_deref(),
        ) {
            (Some(u), Some(p)) => Some((u.to_owned(), p.to_owned())),
            _ => None,
        },
    };

    // Name the target database explicitly. This command wipes derived tables, so the
    // operator should be able to see which analysis is about to be rewritten without
    // having to reason about how --analysis and config.toml interact.
    println!("Projecting graph_events into native SurrealDB tables in '{db_name}'…");
    // Not "first" any more: each half is wiped only once its own stream has loaded and
    // been verified, so a failed load leaves the existing projection intact.
    println!(
        "(This rebuilds entity/officer/location/contract/ward/political_actor and all \
         relation tables — each wiped once its events have loaded.)"
    );

    let projector = Projector::new(db, raw);
    let stats = projector.run().await?;

    info!(
        nodes_upserted = stats.nodes_upserted,
        edges_related = stats.edges_related,
        nodes_skipped = stats.nodes_skipped,
        edges_skipped = stats.edges_skipped,
        "project complete"
    );

    println!(
        "Done: {} nodes upserted, {} edges related, {} nodes skipped, {} edges skipped",
        stats.nodes_upserted, stats.edges_related, stats.nodes_skipped, stats.edges_skipped,
    );

    // Print the breakdown, not just the total. A bare "9,000 edges skipped" reads as acceptable
    // attrition; "9,000 rejected by SurrealDB" reads as a bug, and it is the operator running
    // this command who has to tell the difference.
    if stats.edges_skipped > 0 {
        println!("  of which: {} src endpoint not projected, {} dst endpoint not projected, {} malformed, {} rejected by SurrealDB, {} never delivered",
            stats.edges_skipped_unknown_src,
            stats.edges_skipped_unknown_dst,
            stats.edges_skipped_malformed,
            stats.edges_skipped_rejected,
            stats.edges_skipped_undelivered,
        );
        let unattributed = stats
            .edges_skipped
            .saturating_sub(stats.skipped_attributed());
        if unattributed > 0 {
            println!("  WARNING: {unattributed} dropped edge(s) have no recorded cause — the breakdown above is incomplete");
        }
    }

    Ok(())
}
