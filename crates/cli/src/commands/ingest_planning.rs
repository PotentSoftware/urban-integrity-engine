//! `urban-integrity ingest-planning` — on-demand planning portal scraping.
//!
//! Usage:
//!   urban-integrity ingest-planning --ward X05999001
//!   urban-integrity ingest-planning --all

use anyhow::{bail, Context as _};
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};
use urban_integrity_core::persistence::scan;

static EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

use urban_integrity_core::{config::AppConfig, process_event, GraphMutation};
use urban_integrity_ingestor::planning::{
    firecrawl::scrape_idox_via_firecrawl, mapper::map_planning_item, sidecar::ScrapingSidecar,
};

#[derive(Debug, clap::Args)]
pub struct IngestPlanningArgs {
    /// Ward code to scrape (e.g. X05999001).
    /// Mutually exclusive with --all.
    #[arg(long, conflicts_with = "all")]
    pub ward: Option<String>,

    /// Scrape all configured planning portals.
    /// Mutually exclusive with --ward.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug)]
struct ScrapeResult {
    ward_code: String,
    applications: usize,
    written: usize,
    skipped: usize,
    status: String,
}

/// Load entity_ids of all existing NodeAdded events and (src_id, dst_id) keys of
/// all existing EdgeAdded events from SurrealDB. Used for dedup before inserting.
async fn load_existing_keys(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
) -> anyhow::Result<(HashSet<String>, HashSet<String>)> {
    // Load existing events for dedup (LIMIT guards against flatbuffers buffer overflow
    // on large DBs; some duplicate inserts may slip through on warm DBs but are harmless).
    // Paged to exhaustion. The `LIMIT 20000` this replaces was described as a guard against a
    // flatbuffers buffer overflow, with duplicate inserts written off as "harmless" — but the
    // graph now holds 358,829 NodeAdded and 388,162 EdgeAdded rows, so the preflight was seeing
    // under 3% of existing keys and the dedup it exists to perform was mostly not happening.
    let rows = scan::scan_events_by_kinds(db, &["NodeAdded", "EdgeAdded"])
        .await
        .context("existing-key preflight query failed")?;

    let mut nodes: HashSet<String> = HashSet::new();
    let mut edges: HashSet<String> = HashSet::new();

    for row in rows {
        match row["kind"].as_str() {
            Some("NodeAdded") => {
                if let Some(id) = row["payload"]["entity_id"].as_str() {
                    nodes.insert(id.to_string());
                }
            }
            Some("EdgeAdded") => {
                let src = row["payload"]["src_id"].as_str().unwrap_or("");
                let dst = row["payload"]["dst_id"].as_str().unwrap_or("");
                if !src.is_empty() && !dst.is_empty() {
                    edges.insert(format!("{src}|{dst}"));
                }
            }
            _ => {}
        }
    }

    info!(
        nodes = nodes.len(),
        edges = edges.len(),
        "Existing graph_events keys loaded for dedup"
    );
    Ok((nodes, edges))
}

/// Write a single graph event record to SurrealDB.
async fn write_event(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    kind: &str,
    payload: serde_json::Value,
    timestamp_ms: u64,
) -> anyhow::Result<()> {
    // Unique event_id: content hash (DefaultHasher over full JSON) XOR timestamp,
    // plus a monotonic per-process counter to prevent collisions when two payloads
    // share identical content (e.g. same ward node written for two applications).
    let content_str = payload.to_string();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content_str.hash(&mut hasher);
    let content_hash = hasher.finish();
    let seq = EVENT_SEQ.fetch_add(1, Ordering::Relaxed);
    let event_id = format!("{:x}{:x}{:x}", content_hash, timestamp_ms, seq);
    db.query("CREATE graph_events CONTENT $r")
        .bind((
            "r",
            serde_json::json!({
                "event_id": event_id,
                "timestamp": timestamp_ms,
                "kind": kind,
                "payload": payload,
            }),
        ))
        .await
        .context("graph_events insert failed")?;
    Ok(())
}

pub async fn run(args: IngestPlanningArgs) -> anyhow::Result<()> {
    if !args.all && args.ward.is_none() {
        bail!("Specify --ward <WARD_CODE> or --all");
    }

    let config = AppConfig::load().context("Config load failed — ensure config.toml exists")?;

    if config.ingest.planning_portals.is_empty() {
        bail!(
            "No planning portals configured. Add [[ingest.planning_portals]] entries to \
             config.local.toml (copy config.toml, whose portals are commented-out examples)"
        );
    }

    let portals: Vec<_> = if args.all {
        config.ingest.planning_portals.clone()
    } else {
        let ward = args.ward.as_deref().unwrap();
        let found: Vec<_> = config
            .ingest
            .planning_portals
            .iter()
            .filter(|p| p.ward_code == ward)
            .cloned()
            .collect();
        if found.is_empty() {
            bail!(
                "Ward code '{}' not found in configured planning_portals. \
                 Check [[ingest.planning_portals]] in config.local.toml (or config.toml)",
                ward
            );
        }
        found
    };

    // Connect to SurrealDB — required for dedup check and writing
    let db = crate::db::connect_db(&config).await.context(
        "SurrealDB connection failed — is SurrealDB running? (check config.toml [surreal])",
    )?;

    // Pre-load existing keys once for the whole run (avoids N per-event round-trips).
    // Non-fatal: if the preflight fails (e.g. flatbuffers overflow on a large DB), we
    // proceed with empty sets — some duplicates may be inserted but are harmless.
    let (mut known_nodes, mut known_edges) = load_existing_keys(&db).await.unwrap_or_else(|e| {
        warn!("preflight key-load failed ({e}); proceeding without dedup");
        (HashSet::new(), HashSet::new())
    });

    let sidecar = ScrapingSidecar::with_timeout(
        &config.ingest.scrapling_url,
        config.ingest.scrapling_port,
        config.ingest.scrapling_timeout_secs,
    );

    if !sidecar.is_healthy().await {
        eprintln!(
            "⚠  Scrapling sidecar not reachable at {}:{} — Firecrawl fallback will be attempted per portal",
            config.ingest.scrapling_url, config.ingest.scrapling_port
        );
    }

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut results = Vec::new();

    let fc_key = config
        .ingest
        .firecrawl_api_key
        .as_deref()
        .unwrap_or("")
        .to_owned();

    for portal in &portals {
        info!(ward_code = %portal.ward_code, url = %portal.url, "Scraping portal");

        // Try sidecar first; fall through to Firecrawl when sidecar returns nothing.
        let mut items = sidecar.scrape(&portal.url, &portal.portal_type).await;

        if items.is_empty() && !fc_key.is_empty() && portal.portal_type == "idox" {
            info!(
                ward_code = %portal.ward_code,
                "sidecar returned no results — trying Firecrawl fallback"
            );
            items = scrape_idox_via_firecrawl(&portal.url, &fc_key).await;
        }

        let applications = items.len();

        if items.is_empty() {
            results.push(ScrapeResult {
                ward_code: portal.ward_code.clone(),
                applications: 0,
                written: 0,
                skipped: 0,
                status: "⚠ no data (sidecar + firecrawl both failed or portal empty)".to_string(),
            });
            continue;
        }

        let mut written = 0usize;
        let mut skipped = 0usize;

        for item in &items {
            let events = map_planning_item(item, &portal.ward_code);
            for event in events {
                let mutation = match process_event(event) {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                match mutation {
                    // The ingestion pipeline emits `UpsertNode` for every node; the older
                    // `AddNode` is handled identically here since this on-demand path writes
                    // a fresh `NodeAdded` row and de-dupes by `known_nodes`.
                    GraphMutation::AddNode { ref id, ref weight }
                    | GraphMutation::UpsertNode { ref id, ref weight } => {
                        if known_nodes.contains(&id.0) {
                            skipped += 1;
                            continue;
                        }
                        let payload = serde_json::json!({
                            "entity_id": id.0,
                            "weight_json": serde_json::to_string(weight).unwrap_or_default(),
                        });
                        if let Err(e) = write_event(&db, "NodeAdded", payload, timestamp_ms).await {
                            tracing::warn!(entity_id = %id.0, "NodeAdded write failed: {e}");
                        } else {
                            known_nodes.insert(id.0.clone());
                            written += 1;
                        }
                    }
                    GraphMutation::AddEdge {
                        ref src,
                        ref dst,
                        ref weight,
                    } => {
                        let edge_key = format!("{}|{}", src.0, dst.0);
                        if known_edges.contains(&edge_key) {
                            skipped += 1;
                            continue;
                        }
                        let payload = serde_json::json!({
                            "src_id": src.0,
                            "dst_id": dst.0,
                            "weight_json": serde_json::to_string(weight).unwrap_or_default(),
                        });
                        if let Err(e) = write_event(&db, "EdgeAdded", payload, timestamp_ms).await {
                            tracing::warn!(src = %src.0, dst = %dst.0, "EdgeAdded write failed: {e}");
                        } else {
                            known_edges.insert(edge_key);
                            written += 1;
                        }
                    }
                    GraphMutation::RemoveNode { .. } => {} // planning data doesn't remove nodes
                    GraphMutation::UpdateNode { .. } => {} // planning portal ingestion never emits UpdateNode
                }
            }
        }

        results.push(ScrapeResult {
            ward_code: portal.ward_code.clone(),
            applications,
            written,
            skipped,
            status: "✓ ok".to_string(),
        });
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(["Ward Code", "Applications", "Written", "Skipped", "Status"]);

    for r in &results {
        table.add_row([
            r.ward_code.as_str(),
            &r.applications.to_string(),
            &r.written.to_string(),
            &r.skipped.to_string(),
            r.status.as_str(),
        ]);
    }

    println!("{table}");
    println!(
        "\nTotal: {} ward(s), {} applications, {} events written, {} skipped (already in DB)",
        results.len(),
        results.iter().map(|r| r.applications).sum::<usize>(),
        results.iter().map(|r| r.written).sum::<usize>(),
        results.iter().map(|r| r.skipped).sum::<usize>(),
    );

    Ok(())
}
