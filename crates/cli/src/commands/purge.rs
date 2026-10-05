//! Orphan-purge maintenance command (data-integrity cleanup).
//!
//! Background: the first full CCOD ingest (debug session `ccod-ingest-ooms-surrealdb`)
//! emitted three persistent writes per row for the entire UK property register with no
//! graph-known pre-filter. Two of the three writes persisted UNCONDITIONALLY, while the
//! third (`AddEdge { OwnsProperty }`) is silently rejected by the coordinator for unknown
//! owners (storage::insert_edge returns Err, no WAL). The unconditional writes are:
//!
//! - `AddNode { Property }` → `emit_wal(NodeAdded)` → a `graph_events` row, written even
//!   when the owner `GB-COH:{company_number}` node was never tracked.
//! - `write_identity_link` → a direct `identity_links` row whose `target_node_id`
//!   (`GB-COH:{company_number}`) may point at a company the graph never tracked.
//!
//! Net effect: the property node and its identity_link are left as ORPHANS (the owner edge
//! that would tie them to a real company was dropped) — graph pollution at multi-million
//! scale. The emit loop is now pre-filtered so this never recurs (land_registry_ccod.rs),
//! but the crashed run already left orphans in `data/surreal.db`; this command removes them.
//!
//! # Safety
//!
//! - Default is a DRY RUN: it counts and reports orphans, deletes nothing.
//! - Deletion requires the explicit `--apply` flag.
//! - "Orphan" is defined by OWNERSHIP, not by id format: a Property `graph_events`
//!   NodeAdded row whose id is NOT the destination of any `OwnsProperty` edge — i.e.
//!   nothing owns it. The crashed CCOD run is exactly this case: the `AddNode { Property }`
//!   persisted but the `AddEdge { OwnsProperty }` was rejected for the unknown owner.
//! - This is format-independent and covers BOTH ownership models, which is the key
//!   safety property: CCOD property is owned by a `GB-COH:` company, OCOD property is
//!   owned by a `GB-ROE:`/`GB-OCOD:` OffshoreEntity (overseas). Both legitimate kinds
//!   carry an OwnsProperty edge, so neither is ever misclassified. (The previous
//!   definition derived a `GB-COH:{segment}` owner from the id and flagged any property
//!   whose owner wasn't a known UK company — which wrongly classed EVERY OCOD overseas
//!   record as an orphan: ~93k validated rows that `--apply` would have deleted.)
//! - An empty owned-set aborts the run (refuses to treat the whole register as orphans).

use anyhow::{Context as _, Result};
use clap::Args;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::IsTerminal;
use surrealdb::{engine::any::Any, Surreal};
use tracing::{info, warn};
use urban_integrity_core::config::AppConfig;
use urban_integrity_core::persistence::scan;
use urban_integrity_enricher::adapters::land_registry_ccod::load_known_company_ids;

use crate::db::connect_db;

/// Arguments for the `purge-orphans` command.
#[derive(Debug, Args)]
pub struct PurgeArgs {
    /// Actually delete the orphan records. Without this flag the command is a DRY RUN
    /// that only counts and reports what would be deleted.
    #[arg(long)]
    pub apply: bool,
}

/// True when a property node is an orphan: nothing owns it.
///
/// A property is "owned" iff its id appears as the destination of an `OwnsProperty`
/// edge. This is format-independent and correct for BOTH ownership models:
/// - CCOD: owner is a `GB-COH:` UK company node.
/// - OCOD: owner is a `GB-ROE:`/`GB-OCOD:` OffshoreEntity node (overseas-only).
///
/// Both legit kinds carry an OwnsProperty edge, so only a property whose owner edge
/// was never created (the CCOD crash dropped it for unknown owners) is an orphan.
/// Pure predicate (no I/O) so it is unit-testable without a DB.
pub fn property_is_orphan(owned_property_ids: &HashSet<String>, entity_id: &str) -> bool {
    !owned_property_ids.contains(entity_id)
}

/// Result of scanning for orphans (counts + the ids that would be deleted).
#[derive(Debug, Default)]
pub struct OrphanScan {
    /// `graph_events` NodeAdded rows for Property nodes whose owner is untracked.
    pub orphan_property_node_ids: Vec<String>,
    /// `identity_links` (source=land_registry) whose owner `target_node_id` is untracked.
    pub orphan_link_ids: Vec<String>,
    /// Total `property:`-prefixed NodeAdded rows examined (owned + orphan) — the
    /// denominator for the blast-radius cap (D-07). Populated alongside
    /// `orphan_property_node_ids` in `scan_orphans`, never re-derived elsewhere.
    pub scanned_property_count: usize,
}

pub async fn run(args: PurgeArgs) -> Result<()> {
    let config = AppConfig::load().context("Config load failed")?;
    let db = connect_db(&config).await?;

    // A property is an orphan when NOTHING owns it: its id is not the destination of
    // any OwnsProperty edge. Load the owned-property set first — this is format-
    // independent and covers BOTH ownership models (GB-COH companies for CCOD,
    // OffshoreEntity for OCOD overseas), so legitimate OCOD property is never
    // misclassified as a missing-GB-COH-owner orphan (the prior definition's bug).
    let owned = load_owned_property_ids(&db).await?;
    info!(
        owned_properties = owned.len(),
        "purge-orphans: loaded owned-property set (OwnsProperty edge destinations)"
    );

    if owned.is_empty() {
        // An empty owned-set means EVERY property would be classed an orphan. That is
        // almost certainly a failed graph_events query (or unprojected ownership
        // edges), not a real "delete every property" intent. Refuse so a transient
        // query failure can never mass-delete.
        warn!(
            "purge-orphans: owned-property set is EMPTY — refusing to scan/delete. \
             No OwnsProperty edges found in graph_events (cold DB or a failed query). \
             Investigate before purging."
        );
        println!(
            "Refusing to purge: no OwnsProperty edges found, so every property would be \
             treated as an orphan, which is unsafe. Ensure ownership edges exist \
             (graph_events kind='EdgeAdded' OwnsProperty) and retry."
        );
        return Ok(());
    }

    // The identity_links orphan scan still cross-references the graph-known GB-COH set.
    let known = load_known_company_ids(&db).await;

    let scan = scan_orphans(&db, &known, &owned).await?;

    println!("Orphan scan (CCOD ingest cleanup):");
    println!(
        "  Orphan Property nodes (graph_events NodeAdded, no owner edge): {}",
        scan.orphan_property_node_ids.len()
    );
    println!(
        "  Orphan identity_links (source=land_registry, untracked owner):  {}",
        scan.orphan_link_ids.len()
    );

    if scan.orphan_property_node_ids.is_empty() && scan.orphan_link_ids.is_empty() {
        println!("\nNo orphans found — nothing to purge.");
        return Ok(());
    }

    if !args.apply {
        println!("\nDRY RUN — nothing deleted. Re-run with `--apply` to delete the records above.");
        return Ok(());
    }

    // --- Blast-radius cap (D-07) ------------------------------------------------------------
    // The empty-owned-set guard above only refuses when the owned set is EMPTY. It would not
    // have caught the historical 93,052-orphan misclassification: that owned set was
    // non-empty, just built from the wrong definition. This is a second, independent safety
    // property — refuse to apply when the orphan share of the scanned population is
    // implausibly large, regardless of why. Same dual-channel warn!+println! shape as the
    // empty-set guard; unlike that guard (which exits 0, nothing to investigate further), this
    // is a hard error (D-06's "hard error not warning" framing applies to a safety abort too).
    if blast_radius_exceeded(
        scan.orphan_property_node_ids.len(),
        scan.scanned_property_count,
    ) {
        let ratio = scan.orphan_property_node_ids.len() as f64 / scan.scanned_property_count as f64;
        warn!(
            orphan_count = scan.orphan_property_node_ids.len(),
            scanned_property_count = scan.scanned_property_count,
            ratio,
            threshold = BLAST_RADIUS_MAX_RATIO,
            "purge-orphans: blast-radius cap exceeded — refusing to apply"
        );
        println!(
            "\nRefusing to apply: orphan Property nodes are {:.2}% of the scanned population \
             ({} of {}), which exceeds the {:.2}% blast-radius cap. This looks like a \
             misclassification, not a real cleanup target — investigate the orphan definition \
             before purging (see INT-01/D-07).",
            ratio * 100.0,
            scan.orphan_property_node_ids.len(),
            scan.scanned_property_count,
            BLAST_RADIUS_MAX_RATIO * 100.0
        );
        anyhow::bail!(
            "purge-orphans --apply refused: blast-radius cap exceeded ({:.4} > {:.4})",
            ratio,
            BLAST_RADIUS_MAX_RATIO
        );
    }

    // --- Apply: single invocation, two passes (D-01) ---------------------------------------
    // The scan above IS the read-only first pass; everything below is the confirm-then-delete
    // second pass, over the SAME in-memory id list the scan produced above — never re-derived
    // by a post-confirmation re-scan (D-04). There is no separate `--dry-run` subcommand and
    // none is added here: a second `purge-orphans --apply` invocation after a first `--apply`
    // that refused confirmation simply re-runs this whole flow from the top, including a fresh
    // scan and a fresh digest.
    let mut confirmation_ids: Vec<String> = scan
        .orphan_property_node_ids
        .iter()
        .cloned()
        .chain(scan.orphan_link_ids.iter().cloned())
        .collect();
    confirmation_ids.sort_unstable();
    confirmation_ids.dedup();

    let digest = confirmation_digest(&confirmation_ids);
    println!("\nConfirmation digest for THIS scan: {digest}");

    match confirm_interactively(&digest) {
        Ok(true) => {}
        Ok(false) => {
            warn!(
                "purge-orphans: typed confirmation did not match the scan digest — \
                 refusing to delete."
            );
            println!(
                "\nConfirmation failed: what you typed did not match the digest above. \
                 Nothing deleted. Re-run `purge-orphans --apply` to try again."
            );
            anyhow::bail!("purge-orphans --apply aborted: confirmation digest mismatch");
        }
        Err(e) => {
            warn!(error = %e, "purge-orphans: confirmation refused before any delete");
            println!("\n{e}\nNothing deleted.");
            return Err(e);
        }
    }

    // --- Delete orphan identity_links, then orphan Property graph_events rows --------------
    // Uses the EXACT in-memory id lists `scan` produced above (D-04) — never re-scanned after
    // confirmation.
    let deleted_links = delete_orphan_links(&db, &scan.orphan_link_ids).await?;
    let deleted_nodes = delete_orphan_property_nodes(&db, &scan.orphan_property_node_ids).await?;

    info!(
        deleted_links,
        deleted_nodes, "purge-orphans: deletion complete"
    );
    println!(
        "\nApplied: deleted {deleted_links} orphan identity_links and \
         {deleted_nodes} orphan Property graph_events rows."
    );

    Ok(())
}

/// Page size for full-table scans. The SurrealDB 3.0.2 server / 3.1.0-beta.2 SDK pairing
/// fails to deserialize very large result sets (a ~55k-row response decodes; ~443k does not),
/// so scans are fetched in bounded pages well under that threshold and reassembled in Rust.
const SCAN_PAGE_SIZE: usize = 20_000;

/// Fetch all rows for a SELECT in bounded `LIMIT/START` pages so no single response is large
/// enough to overflow the SDK↔server wire decode. `select` must end before any LIMIT/START and
/// MUST carry a stable `ORDER BY` so paging neither skips nor repeats rows.
async fn fetch_all_paged(db: &Surreal<Any>, select: &str) -> Result<Vec<serde_json::Value>> {
    let mut all: Vec<serde_json::Value> = Vec::new();
    let mut start = 0usize;
    loop {
        // SCAN_PAGE_SIZE and start are trusted integer constants, not user input.
        let paged = format!("{select} LIMIT {SCAN_PAGE_SIZE} START {start}");
        let mut page: Vec<serde_json::Value> = db
            .query(&paged)
            .await
            .with_context(|| format!("paged scan failed at START {start}"))?
            .take::<Vec<serde_json::Value>>(0)?;
        let n = page.len();
        all.append(&mut page);
        if n < SCAN_PAGE_SIZE {
            break;
        }
        start += SCAN_PAGE_SIZE;
    }
    Ok(all)
}

/// Load the set of property node ids that ARE owned — the `dst_id` of every
/// OwnsProperty edge in `graph_events`. A property whose id is absent from this set
/// has no owner edge and is an orphan (see [`property_is_orphan`]). Paged + ORDER BY
/// id so the (potentially large) edge scan stays under the SDK wire-decode limit and
/// dodges the `idx_graph_events_kind` planner quirk (the bare predicate without an
/// `ORDER BY` can return zero rows while `count()` is non-zero).
async fn load_owned_property_ids(db: &Surreal<Any>) -> Result<HashSet<String>> {
    // Paging alone was not enough: the `kind = 'EdgeAdded' AND string::contains(...)` pairing is
    // the planner-bug shape, so the scan could return a truncated set however it was paged. A
    // short read here marks owned properties as orphans.
    let edge_rows = scan::scan_events_by_type(db, "EdgeAdded", "OwnsProperty")
        .await
        .context("graph_events OwnsProperty-edge scan failed")?;

    let mut owned = HashSet::new();
    for row in &edge_rows {
        if let Some(dst) = row["payload"]["dst_id"].as_str() {
            owned.insert(dst.to_string());
        }
    }
    Ok(owned)
}

/// Scan for orphan Property nodes and orphan identity_links.
///
/// Read-only: issues paged SELECTs and filters in Rust so the orphan predicate is exercised
/// in pure, testable code rather than buried in SurrealQL. A property is an orphan when its
/// id is absent from `owned` (the set of OwnsProperty edge destinations); identity_links are
/// cross-referenced against the graph-known GB-COH set.
async fn scan_orphans(
    db: &Surreal<Any>,
    known: &HashSet<String>,
    owned: &HashSet<String>,
) -> Result<OrphanScan> {
    let mut scan = OrphanScan::default();

    // 1. Orphan Property nodes: NodeAdded `property:` rows whose id is NOT owned (no
    //    OwnsProperty edge points at it). This used to pair the indexed `kind =` equality
    //    with an unindexed `string::starts_with` predicate on `payload.entity_id` in one
    //    SurrealQL WHERE — the exact shape the planning notes' broadened planner-bug rule flags as
    //    suspect, independent of whether the second predicate is CONTAINS or starts_with
    //    (phase 28 research Pitfall 1). It measured correct on 2026-08-21 (105,809 = 105,809),
    //    but "measures correct today" is not proof of safety on the next run — the bug's
    //    documented behaviour is silent, run-to-run non-deterministic truncation. Migrated
    //    to the same planner-bug-safe helper the sibling edge scan
    //    (`load_owned_property_ids`, three lines above) already uses: the `kind`/marker
    //    filter runs in SurrealQL (safe — a single indexed `kind` equality paired with the
    //    *unindexed* `weight_json CONTAINS` predicate, never a second predicate on
    //    `entity_id`), and the `property:` discrimination runs in Rust afterwards, exactly
    //    like `load_owned_property_ids`'s `dst_id` filter.
    let node_rows = scan::scan_events_by_type(db, "NodeAdded", "Property")
        .await
        .context("graph_events property-node scan failed")?;

    for row in &node_rows {
        let entity_id = match row["payload"]["entity_id"].as_str() {
            Some(s) => s,
            None => continue,
        };
        // The "Property" marker matches `weight_json CONTAINS "Property"` — no other
        // NodeWeight variant's serde tag contains that substring (Entity, Officer,
        // Location, Contract, Ward, PoliticalActor, Psc, Charity, OffshoreEntity), so this
        // filter is belt-and-braces against a marker match inside an unrelated field value
        // rather than a real collision.
        if !entity_id.starts_with("property:") {
            continue;
        }
        // Denominator for the blast-radius cap (D-07) — every property row examined,
        // owned or orphan, counted before the orphan filter below narrows it.
        scan.scanned_property_count += 1;
        if !property_is_orphan(owned, entity_id) {
            continue;
        }
        // Capture the record id (string form) so we can DELETE it by record key.
        if let Some(rec_id) = record_id_string(row.get("id")) {
            scan.orphan_property_node_ids.push(rec_id);
        }
    }

    // 2. Orphan identity_links: land_registry-sourced links whose owner target is untracked.
    //    Paged + ORDER BY id for stable scan ('land_registry' is a trusted literal).
    let link_rows = fetch_all_paged(
        db,
        "SELECT id, target_node_id FROM identity_links \
         WHERE source = 'land_registry' ORDER BY id",
    )
    .await
    .context("identity_links scan failed")?;

    for row in &link_rows {
        let target = match row.get("target_node_id").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };
        // target_node_id is the owner GB-COH id; orphan when not in the known set.
        if known.contains(target) {
            continue;
        }
        if let Some(rec_id) = record_id_string(row.get("id")) {
            scan.orphan_link_ids.push(rec_id);
        }
    }

    Ok(scan)
}

// ── INT-03 / D-14: retire superseded OCOD owner events ──────────────────────────────────────
//
// `graph_events` is append-only. 28-03 fixed `owner_key` so a genuine company number keys an
// overseas owner on its own (`GB-OCOD:fc025391`) instead of once per country string
// (`GB-OCOD:germany-fc025391`, `GB-OCOD:hamburg-fc025391`). Re-running the OCOD adapter under
// the fixed key therefore ADDS a correctly-keyed node beside the fragmented ones — the fix
// would read as successful in code and change nothing in the live graph. The superseded events
// have to be retired first; that is what this scan and its delete path do.
//
// Scope is deliberately ALL `GB-OCOD:`-sourced rows, not just the owners whose key text
// changes: the OCOD adapter carries no incremental/skip guard, so a full
// `enrich --source land_registry_ocod` re-run re-emits every owner regardless
// (phase 28 research Pitfall 3). Purging only the merged subset would leave the unaffected
// ~104k properties with two OwnsProperty edges each after the re-run.
//
// `GB-ROE:` keys are never touched — D-09 does not change them, so deleting them would be
// pure data loss for no gain. Property `NodeAdded` events are likewise left alone: the
// projector's `UPSERT` is idempotent on an unchanged property id, so re-emitting them is a
// harmless no-op.

/// The two id sets the D-14 purge retires, both `graph_events` record ids.
#[derive(Debug, Default)]
pub struct StaleOcodScan {
    /// `NodeAdded` rows for `OffshoreEntity` weights keyed `GB-OCOD:`.
    pub stale_nodes: Vec<String>,
    /// `EdgeAdded` rows for `OwnsProperty` weights whose `src_id` is such an owner.
    pub stale_edges: Vec<String>,
}

impl StaleOcodScan {
    /// The sorted, de-duplicated union of both halves — what the confirmation digest is
    /// computed over and what the delete layer receives.
    ///
    /// Both halves live in the same table and are deleted in one confirmed operation, so the
    /// operator's typed digest must commit to both: a digest over the node half alone would
    /// leave the edge half uncommitted between report and delete. De-duplication is not
    /// cosmetic either — [`delete_in_batches`] hard-errors when removed != sent (D-06), so one
    /// id sent twice would abort the whole purge.
    pub fn confirmation_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .stale_nodes
            .iter()
            .cloned()
            .chain(self.stale_edges.iter().cloned())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn total(&self) -> usize {
        self.stale_nodes.len() + self.stale_edges.len()
    }
}

/// Record ids of the rows whose `payload[id_field]` is a `GB-OCOD:` key.
///
/// Pure (no I/O) so the purge scope — the part that decides what gets deleted — is exercised
/// in unit tests rather than only against a live database, the same layering
/// [`property_is_orphan`] follows for `purge-orphans`.
fn stale_ocod_ids(rows: &[serde_json::Value], id_field: &str) -> Vec<String> {
    rows.iter()
        .filter(|r| {
            r["payload"][id_field]
                .as_str()
                .is_some_and(|s| s.starts_with("GB-OCOD:"))
        })
        .filter_map(|r| record_id_string(r.get("id")))
        .collect()
}

/// Scan `graph_events` for the OCOD owner nodes and ownership edges the D-14 re-run supersedes.
///
/// Both halves go through [`scan::scan_events_by_type`] and filter in Rust — never a SurrealQL
/// `kind =` equality paired with a `CONTAINS`/`starts_with` predicate, which is the documented
/// row-losing planner shape (T-28-UnderScan). An under-count here is not a cosmetic miss: a
/// missed `OwnsProperty` edge would survive the purge pointing at an owner node that did not,
/// leaving a dangling edge the re-run cannot repair.
pub async fn scan_stale_ocod_owner_events(db: &Surreal<Any>) -> Result<StaleOcodScan> {
    let node_rows = scan::scan_events_by_type(db, "NodeAdded", "OffshoreEntity")
        .await
        .context("graph_events OffshoreEntity-node scan failed")?;
    let stale_nodes = stale_ocod_ids(&node_rows, "entity_id");

    let edge_rows = scan::scan_events_by_type(db, "EdgeAdded", "OwnsProperty")
        .await
        .context("graph_events OwnsProperty-edge scan failed")?;
    let stale_edges = stale_ocod_ids(&edge_rows, "src_id");

    Ok(StaleOcodScan {
        stale_nodes,
        stale_edges,
    })
}

/// Delete the scanned stale OCOD rows through the same real-removed-count layer
/// `purge-orphans` uses (D-05/D-06) — one call site, one implementation, one fail-loud
/// mismatch check. Exposed for the DB-gated projector regression test, which drives the
/// seed → purge → re-emit → project cycle without the CLI's interactive confirmation I/O
/// (Pattern 3: pure logic / I/O / DB call sites in distinct layers).
pub async fn delete_stale_ocod_events(db: &Surreal<Any>, ids: &[String]) -> Result<usize> {
    delete_in_batches(db, "graph_events", ids).await
}

/// Arguments for the `purge-ocod-owners` command.
#[derive(Debug, Args)]
pub struct PurgeOcodOwnersArgs {
    /// Actually delete the superseded OCOD owner events. Without this flag the command is a
    /// DRY RUN that only counts and reports what would be deleted.
    #[arg(long)]
    pub apply: bool,
}

/// D-14: retire superseded `GB-OCOD:` owner nodes and ownership edges from `graph_events`.
///
/// Same single-invocation, scan-then-confirm-then-delete flow as [`run`] — the scan below IS
/// the read-only first pass, and the delete works from the exact ids it produced, never a
/// post-confirmation re-scan (D-04). It reuses [`confirmation_digest`],
/// [`confirm_interactively`] and [`delete_in_batches`] directly rather than shipping a second
/// implementation, so the non-TTY refusal and the real-removed-count check apply here
/// unchanged. There is no `--yes`/`--force` escape here either.
///
/// D-07's blast-radius cap is deliberately NOT applied: for `purge-orphans` an implausibly
/// large share signals a misclassified definition, but here selecting 100% of the
/// `GB-OCOD:`-sourced rows is the *correct* and intended scope (Pitfall 3), so a ratio cap
/// would fire on the success case. The controls that do apply are the typed digest, the
/// operator's out-of-band backup (D-08), and the fail-loud removed-count check.
pub async fn run_ocod_owners(args: PurgeOcodOwnersArgs) -> Result<()> {
    let config = AppConfig::load().context("Config load failed")?;
    let db = connect_db(&config).await?;

    let scan = scan_stale_ocod_owner_events(&db).await?;
    info!(
        stale_nodes = scan.stale_nodes.len(),
        stale_edges = scan.stale_edges.len(),
        "purge-ocod-owners: scan complete"
    );

    println!("Superseded OCOD owner events (D-14, INT-03):");
    println!(
        "  OffshoreEntity NodeAdded rows keyed GB-OCOD:                   {}",
        scan.stale_nodes.len()
    );
    println!(
        "  OwnsProperty EdgeAdded rows sourced from a GB-OCOD: owner:     {}",
        scan.stale_edges.len()
    );
    println!("  (GB-ROE:-keyed owners are never in scope and are not counted above.)");

    if scan.total() == 0 {
        println!("\nNo superseded OCOD owner events found — nothing to purge.");
        return Ok(());
    }

    if !args.apply {
        println!(
            "\nDRY RUN — nothing deleted. Re-run with `--apply` to delete the events above, \
             then re-run `enrich --source land_registry_ocod` and `project` to re-emit them \
             under the corrected owner key."
        );
        return Ok(());
    }

    let confirmation_ids = scan.confirmation_ids();
    let digest = confirmation_digest(&confirmation_ids);
    println!("\nConfirmation digest for THIS scan: {digest}");

    match confirm_interactively(&digest) {
        Ok(true) => {}
        Ok(false) => {
            warn!(
                "purge-ocod-owners: typed confirmation did not match the scan digest — \
                 refusing to delete."
            );
            println!(
                "\nConfirmation failed: what you typed did not match the digest above. \
                 Nothing deleted. Re-run `purge-ocod-owners --apply` to try again."
            );
            anyhow::bail!("purge-ocod-owners --apply aborted: confirmation digest mismatch");
        }
        Err(e) => {
            warn!(error = %e, "purge-ocod-owners: confirmation refused before any delete");
            println!("\n{e}\nNothing deleted.");
            return Err(e);
        }
    }

    let deleted = delete_stale_ocod_events(&db, &confirmation_ids).await?;
    info!(deleted, "purge-ocod-owners: deletion complete");
    println!(
        "\nApplied: deleted {deleted} superseded OCOD owner graph_events rows \
         ({} node + {} edge). Re-run `enrich --source land_registry_ocod` and `project` to \
         re-emit them under the corrected owner key.",
        scan.stale_nodes.len(),
        scan.stale_edges.len()
    );

    Ok(())
}

/// Extract a `table:key` string from a SurrealDB `id` field, which may serialise as a
/// JSON string (`"identity_links:abc"`) or as a RecordID object. Returns the string form;
/// callers pass it through [`record_key`] to get the key usable with `type::record`.
fn record_id_string(id: Option<&serde_json::Value>) -> Option<String> {
    let v = id?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    // RecordID object shape: {"tb": "...", "id": {"String": "..."}} or similar — fall back
    // to the JSON string form, which record_key can still split on the first ':'.
    Some(v.to_string().trim_matches('"').to_string())
}

/// Extract the key suffix from a full record id like `identity_links:abc123`, unquoted.
///
/// SurrealDB renders any record id whose key is not a bare identifier in quoted form —
/// backticks (`graph_events:` + backtick + `NodeAdded_GB-OCOD_1811435-fc013582` + backtick) or,
/// in some renderings, angle brackets. Live `graph_events` keys are always that shape.
///
/// The quotes are display syntax, NOT part of the key. Leaving them on makes
/// `RecordId::new(table, key)` address a key whose literal characters include the quote marks,
/// which matches no row — so the delete removes nothing. That is exactly how the first live
/// D-14 purge failed (2026-08-22): `sent 10000 keys, SurrealDB removed 0`, caught by D-06's
/// mismatch check before anything else ran. The split takes the FIRST colon only, so colons
/// inside a quoted key stay part of the key.
fn record_key(id: &str) -> &str {
    let key = id.split_once(':').map(|(_, k)| k).unwrap_or(id);
    if let Some(inner) = key.strip_prefix('`').and_then(|k| k.strip_suffix('`')) {
        return inner;
    }
    if let Some(inner) = key
        .strip_prefix('\u{27e8}')
        .and_then(|k| k.strip_suffix('\u{27e9}'))
    {
        return inner;
    }
    key
}

/// Delete batch size for orphan cleanup. Each batch is ONE server-side `DELETE ... RETURN
/// BEFORE` round-trip (D-05), so the request carries the keys and the response carries the
/// removed rows — kept at the pre-fix value of 10,000 because the spike that locked this
/// syntax (`crates/cli/tests/purge_delete_spike.rs`, 28-01) only measured a 3-row throwaway
/// table and the tables this function targets (`property`, `identity_links`, `graph_events`)
/// carry no large blob fields — a `RETURN BEFORE` payload per row is a few hundred bytes at
/// most, well under the ~55k-row SDK wire-decode ceiling `SCAN_PAGE_SIZE`'s comment already
/// documents. `yield_now` between batches lets surrealkv flush tombstone memtables so a
/// multi-million-row delete doesn't balloon resident memory. Re-measure before assuming this
/// still holds if a future caller targets a table with large per-row payloads.
const DELETE_BATCH_SIZE: usize = 10_000;

/// Compare the actually-removed count against what was sent; hard error on any mismatch
/// (D-06), never a warning. Pure, injectable without a DB — mirrors
/// `crates/ingestor/src/backfill/anomaly_sink.rs::record_anomaly`'s fail-loud `bail!` shape
/// exactly: never `let _ =` a SurrealDB write `Result`, never treat a count mismatch as a
/// `warn!`-only event.
fn check_delete_count(table: &str, sent: usize, removed: usize) -> Result<()> {
    if removed != sent {
        anyhow::bail!(
            "delete count mismatch on {table}: sent {sent} keys, SurrealDB removed {removed}"
        );
    }
    Ok(())
}

/// Delete records by key in bounded, server-side batches, using the LOCKED
/// `DELETE type::table($table) WHERE id IN $ids RETURN BEFORE` syntax proven live against a
/// throwaway `TestNs` in `crates/cli/tests/purge_delete_spike.rs` (28-01) — copied verbatim,
/// not re-derived. `removed.len()` is the REAL number of rows SurrealDB actually removed
/// (D-05), never `chunk.len()` (the size of the batch sent) — the exact shape of this
/// project's two worst incidents (the ICIJ adapter logging 814k rows written against an empty
/// table; PPD reporting 2,795,151 written while staging zero) applied to a delete instead of
/// a write. Any mismatch is a hard error via [`check_delete_count`] (D-06).
async fn delete_in_batches(db: &Surreal<Any>, table: &str, ids: &[String]) -> Result<usize> {
    let mut deleted = 0usize;
    for chunk in ids.chunks(DELETE_BATCH_SIZE) {
        let keys: Vec<surrealdb::types::RecordId> = chunk
            .iter()
            .map(|id| surrealdb::types::RecordId::new(table, record_key(id)))
            .collect();
        let removed: Vec<serde_json::Value> = db
            .query("DELETE type::table($table) WHERE id IN $ids RETURN BEFORE")
            .bind(("table", table.to_string()))
            .bind(("ids", keys))
            .await
            .with_context(|| format!("batched delete on {table} failed ({} keys)", chunk.len()))?
            .take(0)?;
        let removed_count = removed.len(); // D-05: the REAL removed-count, never chunk.len()
        check_delete_count(table, chunk.len(), removed_count)?; // D-06: hard error on mismatch
        deleted += removed_count;
        // Pace so surrealkv flushes tombstone memtables between batches.
        tokio::task::yield_now().await;
    }
    Ok(deleted)
}

/// Blast-radius cap (D-07): refuse to apply when the orphan set exceeds this share of the
/// scanned population. The existing empty-owned-set guard (`run()`, above) only refuses when
/// the owned set is EMPTY — it would not have caught the historical 93,052-orphan
/// misclassification, since that owned set was non-empty, just built from the wrong
/// definition. Set with headroom above the live-measured near-zero baseline: 0 orphan
/// Property nodes of 105,809 scanned (`an_anglesey_aebaa785`, measured 2026-08-21, re-measured
/// 2026-08-22 in 28-01) — 1% is a low single-digit-percent cap, ample above today's real
/// count, while still catching a misclassification anywhere near the historical bug's scale
/// (~87% of the scanned population), which sails past this threshold by two orders of
/// magnitude.
const BLAST_RADIUS_MAX_RATIO: f64 = 0.01;

/// True when `orphan_count` exceeds `BLAST_RADIUS_MAX_RATIO` of `scanned_count`.
/// `scanned_count == 0` is handled without dividing by zero (returns `false`) — in practice
/// this is unreachable in `run()` because the pre-existing empty-owned-set guard runs first
/// and returns before any blast-radius check, but the pure fn stays total regardless of call
/// order. Pure, unit-testable without a DB.
fn blast_radius_exceeded(orphan_count: usize, scanned_count: usize) -> bool {
    if scanned_count == 0 {
        return false;
    }
    (orphan_count as f64 / scanned_count as f64) > BLAST_RADIUS_MAX_RATIO
}

/// Delete orphan identity_links in bounded batches.
async fn delete_orphan_links(db: &Surreal<Any>, ids: &[String]) -> Result<usize> {
    delete_in_batches(db, "identity_links", ids).await
}

/// Delete orphan Property `graph_events` rows in bounded batches.
async fn delete_orphan_property_nodes(db: &Surreal<Any>, ids: &[String]) -> Result<usize> {
    delete_in_batches(db, "graph_events", ids).await
}

// ── INT-02 confirmation gate (D-01..D-04) ───────────────────────────────────────────────────
//
// Three layers, kept deliberately separate (the phase 28 research notes "Pattern 3"):
//   1. `confirmation_digest`/`digest_matches` — pure, unit-testable, no I/O.
//   2. `confirm_interactively` — I/O (stdin/stdout), the process-boundary safety mechanism.
//   3. the delete call sites in `run()` above — unchanged by this plan except for what feeds
//      them (the exact in-memory scan ids, never re-derived — D-04).
//
// No `--yes`, no `--force`, no env-var escape exists anywhere in this file. Do not add one.

/// SHA-256 over the sorted, de-duplicated orphan record ids, truncated to the first 12 hex
/// chars (6 bytes). Caller MUST pass already-sorted-and-deduped ids (mirrors
/// `crates/core/src/analysis/naming.rs::wardset_hash_full`'s shape exactly).
///
/// This is D-02's typed confirmation token: it is derived from THIS run's scan output, so it
/// cannot be pre-baked into a script, an alias, or shell history — unlike a fixed `y/N` prompt
/// or a static `--yes` flag. That is precisely the shape that let the surrealkv OOM fix get
/// bypassed five times in one day from shell history (2026-06-17); see INT-02/D-02.
fn confirmation_digest(sorted_ids: &[String]) -> String {
    use std::fmt::Write;
    let mut hasher = Sha256::new();
    for id in sorted_ids {
        hasher.update(id.as_bytes());
        hasher.update(b"\n"); // delimiter so ["ab","c"] != ["a","bc"]
    }
    // Format bytes → hex manually to avoid pulling in the `hex` crate.
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(12);
    for b in digest.iter().take(6) {
        // 6 bytes = 12 hex chars
        write!(hex, "{b:02x}").expect("writing to String never fails");
    }
    hex
}

/// Compare a typed confirmation line against the expected digest.
///
/// Trims leading/trailing whitespace (what a typed terminal line carries) but performs an
/// EXACT, case-sensitive comparison otherwise — no case-folding. A digest is a security
/// control here (the phase 28 research notes' V5 Input Validation note), not a UX nicety: a "close enough"
/// match defeats the purpose of proving the operator read and typed the exact output.
fn digest_matches(typed: &str, expected: &str) -> bool {
    typed.trim() == expected
}

/// Read one confirmation line from stdin and compare it against `digest`.
///
/// D-03: if stdin is not a terminal, this is a HARD refusal — returns `Err`, not `Ok(false)`,
/// so the caller cannot mistake "no confirmation possible" for "confirmation declined". No
/// `--yes`, no `--force`, no env-var escape exists to bypass this check; CI/cron/scripts must
/// be categorically unable to reach the delete path.
fn confirm_interactively(digest: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "purge-orphans --apply refused: stdin is not a terminal (no --yes, no --force, \
             no env escape — see INT-02/D-03)"
        );
    }
    println!("Type the digest above to confirm deletion: ");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("failed to read confirmation input from stdin")?;
    Ok(digest_matches(&line, digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_key_strips_table_prefix() {
        assert_eq!(record_key("identity_links:abc123"), "abc123");
        assert_eq!(record_key("graph_events:NodeAdded_x"), "NodeAdded_x");
        assert_eq!(record_key("nokey"), "nokey");
    }

    #[test]
    fn record_key_strips_the_backticks_surrealdb_quotes_keys_with() {
        // SurrealDB renders any record id whose key is not a bare identifier in backtick-quoted
        // form. Live `graph_events` keys are exactly that shape:
        //   graph_events:`NodeAdded_GB-OCOD_1811435-fc013582_GB-OCOD_1811435-fc013582`
        // Leaving the backticks on makes `RecordId::new(table, key)` address a key whose literal
        // characters INCLUDE the backticks, which matches nothing — the delete then removes 0
        // rows. Found live 2026-08-22 when the D-14 purge aborted on D-06's mismatch check
        // (sent 10000, removed 0) against an_anglesey_aebaa785.
        assert_eq!(
            record_key("graph_events:`NodeAdded_GB-OCOD_1811435-fc013582`"),
            "NodeAdded_GB-OCOD_1811435-fc013582",
            "a backtick-quoted key must be unquoted before it is used to address a record"
        );
        assert_eq!(
            record_key("identity_links:`land_registry:GB-COH:12345678`"),
            "land_registry:GB-COH:12345678",
            "colons INSIDE a quoted key belong to the key and must survive the split"
        );
        // An unquoted key is unchanged, and a stray backtick inside a key is not stripped from
        // the middle — only the surrounding quotes go.
        assert_eq!(record_key("graph_events:plain_key"), "plain_key");
    }

    #[test]
    fn property_with_owner_edge_is_not_orphan() {
        let mut owned = HashSet::new();
        owned.insert("property:12345678_BM177079".to_string());
        assert!(!property_is_orphan(&owned, "property:12345678_BM177079"));
    }

    #[test]
    fn property_without_owner_edge_is_orphan() {
        // The CCOD crash signature: a Property node whose OwnsProperty edge was
        // rejected (unknown owner) — so nothing owns it.
        let owned: HashSet<String> = HashSet::new();
        assert!(property_is_orphan(&owned, "property:12345678_BM177079"));
    }

    #[test]
    fn confirmation_digest_is_deterministic_and_order_independent() {
        // Caller sorts+dedups first — confirmation_digest itself does not sort. Feed the SAME
        // set in two different pre-sorted orderings and confirm the same digest results.
        let mut ids_forward = vec![
            "identity_links:aaa".to_string(),
            "property:bbb".to_string(),
            "property:ccc".to_string(),
        ];
        ids_forward.sort_unstable();
        ids_forward.dedup();

        let mut ids_reversed = vec![
            "property:ccc".to_string(),
            "property:bbb".to_string(),
            "identity_links:aaa".to_string(),
        ];
        ids_reversed.sort_unstable();
        ids_reversed.dedup();

        assert_eq!(
            ids_forward, ids_reversed,
            "sorting must converge to the same Vec"
        );
        let digest_a = confirmation_digest(&ids_forward);
        let digest_b = confirmation_digest(&ids_reversed);
        assert_eq!(digest_a, digest_b, "same id set must always hash the same");
        assert_eq!(digest_a.len(), 12, "digest must be 12 hex chars (6 bytes)");

        let mut different_ids = vec!["property:zzz".to_string(), "property:bbb".to_string()];
        different_ids.sort_unstable();
        different_ids.dedup();
        assert_ne!(
            confirmation_digest(&ids_forward),
            confirmation_digest(&different_ids),
            "a different id set must produce a different digest"
        );
    }

    #[test]
    fn digest_matches_trims_whitespace_but_never_case_folds() {
        assert!(
            digest_matches("  abc123def456  ", "abc123def456"),
            "leading/trailing whitespace from a typed line must be trimmed"
        );
        assert!(
            !digest_matches("ABC123DEF456", "abc123def456"),
            "comparison must be exact, no case-folding — this is a security control, not a UX nicety"
        );
    }

    #[test]
    fn confirm_interactively_returns_err_not_ok_false_when_stdin_is_not_a_terminal() {
        // The test harness's stdin is not a terminal in the normal run configuration
        // (phase 28 research Pitfall 2), so this exercises the real D-03 refusal path directly.
        let result = confirm_interactively("deadbeef0000");
        let err = result.expect_err("non-TTY stdin must return Err, not Ok(false)");
        let msg = err.to_string();
        assert!(
            msg.contains("INT-02") && msg.contains("D-03"),
            "refusal message must name INT-02/D-03: {msg}"
        );
        assert!(
            msg.contains("--yes") && msg.contains("--force") && msg.contains("env escape"),
            "refusal message must explicitly state no --yes, no --force, no env escape exist: {msg}"
        );
    }

    #[test]
    fn ocod_overseas_owned_property_is_kept_even_though_owner_is_not_gb_coh() {
        // Regression for the OCOD landmine: an overseas-owned property has a
        // slugified owner-name segment that is NOT a GB-COH company, but it
        // carries an OwnsProperty edge (its owner is an OffshoreEntity). Its id is
        // therefore in the owned set and it must NOT be classed an orphan — the old
        // GB-COH-derived definition wrongly deleted all 93k such OCOD records.
        let mut owned = HashSet::new();
        owned.insert("property:crimson-investments-assets-inc_esx135632".to_string());
        assert!(!property_is_orphan(
            &owned,
            "property:crimson-investments-assets-inc_esx135632"
        ));
    }

    #[test]
    fn delete_count_match_is_ok() {
        // "sent 5 keys, removed 5" -> Ok. Injectable, no DB needed (D-05's precondition —
        // Task 2 proves the same shape against a real SurrealDB).
        assert!(check_delete_count("property", 5, 5).is_ok());
    }

    #[test]
    fn delete_count_mismatch_bails_naming_table_and_both_counts() {
        // "sent 5 keys, removed 3" -> Err naming both numbers and the table name (D-06),
        // mirroring anomaly_sink.rs::record_anomaly's bail! message shape exactly.
        let err = check_delete_count("property", 5, 3).expect_err("mismatch must be an Err");
        let msg = err.to_string();
        assert!(
            msg.contains("property"),
            "message must name the table: {msg}"
        );
        assert!(
            msg.contains('5'),
            "message must contain the sent count (5): {msg}"
        );
        assert!(
            msg.contains('3'),
            "message must contain the removed count (3): {msg}"
        );
    }

    #[test]
    fn blast_radius_exceeded_true_strictly_above_threshold() {
        // 2 of 100 = 2% > 1% threshold -> exceeded.
        assert!(blast_radius_exceeded(2, 100));
    }

    #[test]
    fn blast_radius_exceeded_false_at_threshold() {
        // 1 of 100 = 1% == threshold, not strictly ABOVE it -> not exceeded.
        assert!(!blast_radius_exceeded(1, 100));
    }

    #[test]
    fn blast_radius_exceeded_false_below_threshold() {
        assert!(!blast_radius_exceeded(0, 100));
    }

    #[test]
    fn blast_radius_exceeded_handles_zero_scanned_without_dividing_by_zero() {
        // scanned_count == 0 delegates to the pre-existing empty-owned-set guard, which runs
        // first in run() and returns before this is ever called — but the pure fn must stay
        // total (never panic) regardless of call order.
        assert!(!blast_radius_exceeded(5, 0));
    }

    /// DB-gated (28-01 Task 2 / Open Question 1): the migrated `scan_orphans` property-node
    /// scan (now `scan::scan_events_by_type(db, "NodeAdded", "Property")` + a Rust-side
    /// `property:` filter, replacing the suspect `kind = 'NodeAdded' AND` paired with a
    /// `string::starts_with` predicate on `payload.entity_id` shape) must discriminate
    /// correctly — returning exactly the seeded Property nodes, never the decoy non-property
    /// nodes seeded alongside them. This proves the new shape is *correct*, not just that it
    /// doesn't crash.
    #[tokio::test]
    async fn scan_orphans_migrated_property_scan_discriminates_correctly() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_purgescan", "main")
                .await;

        // Seed 3 Property NodeAdded events, all unowned (owned set stays empty below) so
        // every one of them is an orphan and must be counted.
        for i in 0..3 {
            seed_node_added(
                &db,
                &format!("prop-evt-{i}"),
                &format!("property:test-{i}"),
                &property_weight_json(),
            )
            .await;
        }

        // Seed 2 non-property NodeAdded decoys that must NOT be counted, even though they
        // share `kind = 'NodeAdded'` with the property rows above.
        for i in 0..2 {
            seed_node_added(
                &db,
                &format!("ent-evt-{i}"),
                &format!("GB-COH:{i:08}"),
                &entity_weight_json(&format!("Decoy Co {i}")),
            )
            .await;
        }

        let known: HashSet<String> = HashSet::new();
        let owned: HashSet<String> = HashSet::new(); // nothing owned -> every property node is an orphan

        let scan = scan_orphans(&db, &known, &owned)
            .await
            .expect("scan_orphans failed");

        assert_eq!(
            scan.orphan_property_node_ids.len(),
            3,
            "must return exactly the 3 seeded property nodes, not the 2 decoy entity nodes; \
             got {:?}",
            scan.orphan_property_node_ids
        );
    }

    /// DB-gated (Task 2 / INT-05, D-18/D-19): proves `delete_in_batches`'s real-removed-count
    /// semantics (D-05) against a real SurrealDB — not just the injected pair Task 1's unit
    /// tests exercise. Seeds 3 orphan `property` rows (no owning edge) and 2 non-orphan rows
    /// (each with a real `OwnsProperty` edge, mirroring
    /// `crates/core/tests/surreal_integration.rs::seed_owns_property`'s RELATE shape), deletes
    /// the 3 orphan ids directly, and asserts `removed_count == 3` with exactly the 2
    /// non-orphan rows left behind.
    #[tokio::test]
    async fn delete_in_batches_reports_real_removed_count_against_seeded_property_rows() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_purgedelete", "main")
                .await;

        // 3 orphan property rows: exist, nothing owns them.
        for i in 0..3 {
            db.query("CREATE type::record('property', $key) CONTENT { entity_id: $key }")
                .bind(("key", format!("orphan-{i}")))
                .await
                .expect("seed orphan property row failed")
                .check()
                .expect("seed orphan property row returned a statement error");
        }

        // 2 non-orphan property rows, each with a real OwnsProperty edge from a seeded owner
        // entity (schema: `owns_property TYPE RELATION FROM entity | offshore_entity TO
        // property`, all-`SET`/CONTENT with a bound `datetime` per the datetime-coercion rule (docs/DEVELOPMENT.md Standing Rule 2)).
        for i in 0..2 {
            let prop_key = format!("owned-{i}");
            db.query("CREATE type::record('property', $key) CONTENT { entity_id: $key }")
                .bind(("key", prop_key.clone()))
                .await
                .expect("seed owned property row failed")
                .check()
                .expect("seed owned property row returned a statement error");

            let entity_thing = format!("entity:`owner-{i}`");
            let property_thing = format!("property:`{prop_key}`");
            db.query(format!(
                "RELATE {entity_thing}->owns_property->{property_thing} CONTENT {{ \
                     meta: {{ \
                         created_at: d'2024-04-12T00:00:00Z', \
                         effective_from: NONE, \
                         effective_to: NONE \
                     }}, \
                     data: {{ price_paid: NONE, tenure: NONE }} \
                 }} RETURN NONE"
            ))
            .await
            .expect("seed owns_property edge failed")
            .check()
            .expect("seed owns_property edge returned a statement error");
        }

        let orphan_ids: Vec<String> = (0..3).map(|i| format!("property:orphan-{i}")).collect();
        let removed = delete_in_batches(&db, "property", &orphan_ids)
            .await
            .expect("delete_in_batches must succeed against the 3 seeded orphan rows");
        assert_eq!(
            removed, 3,
            "delete_in_batches must report the REAL removed-count (D-05)"
        );

        let count_rows: Vec<serde_json::Value> = db
            .query("SELECT count() FROM property GROUP ALL")
            .await
            .expect("count query failed")
            .take(0)
            .expect("failed to decode count() result");
        let remaining = count_rows.first().and_then(|r| r["count"].as_i64());
        assert_eq!(
            remaining,
            Some(2),
            "exactly the 2 non-orphan property rows must remain"
        );
    }

    /// DB-gated regression for the live D-14 abort (2026-08-22): a delete driven by ids read
    /// back FROM the database — not hand-built in the test — must actually remove rows.
    ///
    /// The pre-existing `delete_in_batches_reports_real_removed_count_against_seeded_property_rows`
    /// test constructs its ids as `format!("property:orphan-{i}")`, so it never exercises
    /// `record_id_string`, and therefore never saw the backtick-quoted form SurrealDB returns
    /// for a key that is not a bare identifier. That gap is why the delete path passed its
    /// fixture tests and still removed 0 rows against the live corpus. This test closes it by
    /// seeding a key that REQUIRES quoting, reading the id back through the same scan path the
    /// purge uses, and asserting the row is really gone.
    #[tokio::test]
    async fn delete_in_batches_removes_rows_whose_ids_came_back_quoted_from_the_db() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_purgequoted", "main")
                .await;

        // A key in the live shape: hyphens and colons force SurrealDB to render it quoted.
        let key = "NodeAdded_GB-OCOD_1811435-fc013582_GB-OCOD_1811435-fc013582";
        db.query("CREATE type::record('graph_events', $key) CONTENT { kind: 'NodeAdded' }")
            .bind(("key", key.to_string()))
            .await
            .expect("seed graph_events row failed")
            .check()
            .expect("seed graph_events row returned a statement error");

        // Read the id back the way the purge scans do, through record_id_string.
        let rows: Vec<serde_json::Value> = db
            .query("SELECT id FROM graph_events")
            .await
            .expect("id read-back failed")
            .take(0)
            .expect("failed to decode id read-back");
        let ids: Vec<String> = rows
            .iter()
            .filter_map(|r| record_id_string(r.get("id")))
            .collect();
        assert_eq!(ids.len(), 1, "one seeded row expected; got {rows:?}");

        let removed = delete_in_batches(&db, "graph_events", &ids)
            .await
            .expect("delete must succeed on an id the database itself handed back");
        assert_eq!(
            removed, 1,
            "a db-supplied (quoted) id must address the real row — this is the live D-14 abort"
        );

        let count_rows: Vec<serde_json::Value> = db
            .query("SELECT count() FROM graph_events GROUP ALL")
            .await
            .expect("count query failed")
            .take(0)
            .expect("failed to decode count() result");
        // An empty table answers `count() ... GROUP ALL` with 0 here rather than with no row,
        // so accept either shape and assert on the number itself.
        let remaining = count_rows
            .first()
            .and_then(|r| r["count"].as_i64())
            .unwrap_or(0);
        assert_eq!(remaining, 0, "the table must be empty after the delete");
    }

    /// DB-gated (Task 2 / INT-05, D-18/D-19): proves D-06's hard-error mismatch path against a
    /// REAL SurrealDB, simulating a concurrent-removal race — seed 1 orphan row, delete it OUT
    /// OF BAND (as if another process removed it between the scan and the confirmed apply),
    /// then call `delete_in_batches` with that same id and assert the `Result` is an `Err`
    /// naming the mismatch.
    #[tokio::test]
    async fn delete_in_batches_bails_when_a_row_was_removed_out_of_band_before_the_call() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
            return;
        }
        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_purgedelete2", "main")
                .await;

        db.query("CREATE type::record('property', $key) CONTENT { entity_id: $key }")
            .bind(("key", "raced"))
            .await
            .expect("seed raced property row failed")
            .check()
            .expect("seed raced property row returned a statement error");

        // Out-of-band removal — simulates a real concurrent-removal race between the scan the
        // operator confirmed against and this call.
        db.query("DELETE property:raced")
            .await
            .expect("out-of-band delete failed")
            .check()
            .expect("out-of-band delete returned a statement error");

        let ids = vec!["property:raced".to_string()];
        let result = delete_in_batches(&db, "property", &ids).await;
        let err = result.expect_err("a removed-count mismatch (sent 1, removed 0) must be an Err");
        let msg = err.to_string();
        assert!(
            msg.contains("property") && msg.contains('1') && msg.contains('0'),
            "mismatch message must name the table and both counts (D-06): {msg}"
        );
    }

    // ── D-14: stale OCOD owner-event scan (INT-03) ─────────────────────────────────────────

    /// Row shaped like `scan_events_by_type`'s projection: `{id, kind, payload}`.
    fn ev_row(id: &str, field: &str, value: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "kind": "NodeAdded",
            "payload": { field: value },
        })
    }

    #[test]
    fn scan_stale_ocod_node_filter_selects_only_gb_ocod_keys() {
        // GB-ROE: keys must survive untouched: D-09 does not change them, so purging them
        // would be pure data loss for no gain. Anything else (a stray GB-COH:, a row with no
        // entity_id at all) is likewise out of scope.
        let rows = vec![
            ev_row("graph_events:a", "entity_id", "GB-OCOD:germany-fc025391"),
            ev_row("graph_events:b", "entity_id", "GB-OCOD:hamburg-fc025391"),
            ev_row("graph_events:c", "entity_id", "GB-ROE:OE123456"),
            ev_row("graph_events:d", "entity_id", "GB-COH:12345678"),
            serde_json::json!({ "id": "graph_events:e", "payload": {} }),
        ];
        let ids = stale_ocod_ids(&rows, "entity_id");
        assert_eq!(
            ids,
            vec!["graph_events:a".to_string(), "graph_events:b".to_string()],
            "only GB-OCOD:-keyed OffshoreEntity rows are in the D-14 purge scope; \
             GB-ROE: must never be selected"
        );
    }

    #[test]
    fn scan_stale_ocod_edge_filter_covers_every_gb_ocod_source_not_just_merged_ones() {
        // Pitfall 3: a full `enrich --source land_registry_ocod` re-run re-emits EVERY owner,
        // not only the ~1,244 whose key text changes. So the edge scope is ALL GB-OCOD:-sourced
        // OwnsProperty edges — including `GB-OCOD:jersey-blackstone`, a D-10 no-number owner
        // whose key is UNCHANGED by the fix. Narrowing to the merged subset would double
        // `owns_property` for every unaffected property.
        let rows = vec![
            ev_row("graph_events:e1", "src_id", "GB-OCOD:germany-fc025391"),
            ev_row("graph_events:e2", "src_id", "GB-OCOD:jersey-blackstone"),
            ev_row("graph_events:e3", "src_id", "GB-ROE:OE123456"),
            ev_row("graph_events:e4", "src_id", "GB-COH:12345678"),
        ];
        let ids = stale_ocod_ids(&rows, "src_id");
        assert_eq!(
            ids,
            vec!["graph_events:e1".to_string(), "graph_events:e2".to_string()],
            "every GB-OCOD:-sourced OwnsProperty edge is in scope, unchanged-key owners included"
        );
    }

    #[test]
    fn scan_stale_ocod_digest_is_computed_over_nodes_and_edges_together() {
        // The D-14 purge deletes two id sets from the SAME table in one confirmed operation,
        // so the operator's typed digest must commit to both halves — a digest over the node
        // half alone would let the edge half change between report and delete.
        let scan = StaleOcodScan {
            stale_nodes: vec!["graph_events:n1".to_string()],
            stale_edges: vec!["graph_events:e1".to_string()],
        };
        let combined = scan.confirmation_ids();
        assert_eq!(
            combined,
            vec!["graph_events:e1".to_string(), "graph_events:n1".to_string()],
            "confirmation ids must be the sorted, deduped union of both halves"
        );
        let nodes_only = confirmation_digest(&scan.stale_nodes);
        assert_ne!(
            confirmation_digest(&combined),
            nodes_only,
            "the digest must commit to the edge half too, not just the nodes"
        );
    }

    #[test]
    fn scan_stale_ocod_confirmation_ids_dedupe_a_row_appearing_in_both_halves() {
        // Defensive: the two halves are disjoint by construction (different payload fields),
        // but the delete layer hard-errors on a sent/removed mismatch (D-06), and sending the
        // same record id twice would make removed < sent and abort the whole purge.
        let scan = StaleOcodScan {
            stale_nodes: vec!["graph_events:x".to_string()],
            stale_edges: vec!["graph_events:x".to_string()],
        };
        assert_eq!(
            scan.confirmation_ids(),
            vec!["graph_events:x".to_string()],
            "a duplicated id must be sent to the delete layer exactly once"
        );
    }

    /// Seeds one `graph_events` `NodeAdded` row directly, mirroring the shape
    /// `crates/ingestor/tests/projector_donates_to.rs::seed_node` uses.
    async fn seed_node_added(
        db: &Surreal<Any>,
        event_id: &str,
        entity_id: &str,
        weight_json: &str,
    ) {
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": event_id,
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeAdded",
                    "payload": { "entity_id": entity_id, "weight_json": weight_json },
                }),
            ))
            .await
            .expect("failed to seed graph_events row");
    }

    fn property_weight_json() -> String {
        use urban_integrity_schema::types::node::{NodeWeight, PropertyData};
        serde_json::to_string(&NodeWeight::Property(PropertyData {
            title_number: None,
            tenure: None,
            price_paid: None,
            company_number: None,
            country_incorporated: None,
        }))
        .expect("PropertyData serialises")
    }

    fn entity_weight_json(name: &str) -> String {
        use urban_integrity_schema::types::node::{EntityData, NodeWeight};
        serde_json::to_string(&NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: Default::default(),
            incorporation_date: None,
        }))
        .expect("EntityData serialises")
    }
}
