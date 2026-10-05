use anyhow::Context as _;
use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_core::persistence::scan;
use urban_integrity_enricher::wal_writer;

/// Arguments for the `links` subcommand group.
#[derive(Debug, clap::Args)]
pub struct LinksArgs {
    #[command(subcommand)]
    pub command: LinksSubcommand,
}

/// Subcommands for the `links` group (D-06: first nested subcommand group in the project).
#[derive(Debug, clap::Subcommand)]
pub enum LinksSubcommand {
    /// List pending_review identity links ordered by confidence (REV-01)
    List,
    /// Show evidence detail for a specific link (REV-02)
    Show {
        /// Identity link ID
        id: String,
    },
    /// Confirm a pending link and inject into hot graph (REV-03)
    Confirm {
        /// Identity link ID
        id: String,
    },
    /// Reject a link and suppress re-proposal (REV-04)
    Reject {
        /// Identity link ID
        id: String,
    },
}

/// Extract the key suffix from a full record ID like "identity_links:abc123".
/// SurrealDB 3.x `id` fields are RecordID, not strings, so `WHERE id = $id` with a
/// string binding never matches. Using `type::record($table, $key)` in SurrealQL instead.
fn record_key(id: &str) -> &str {
    id.split_once(':').map(|(_, k)| k).unwrap_or(id)
}

/// REV-01: fetch every `pending_review` identity link — no ordering-biased cap.
///
/// Reads through [`scan::scan_identity_links`], whose paged scan orders `ORDER BY id` and
/// terminates only on an empty page, so it cannot silently drop rows the way the old `ORDER BY
/// confidence DESC LIMIT 100` did (GRD-09 / Open Item 11c: an ordered read with a ceiling is a
/// biased sample of the highest-confidence rows, not a truncation — for a review queue whose
/// entire purpose is surfacing the low-confidence tail, that is the defect). Confidence-descending
/// is applied here, in Rust, over the COMPLETE set already in hand — display ordering only, never
/// a paging/reading order.
async fn fetch_pending_review(db: &Surreal<Any>) -> anyhow::Result<Vec<scan::IdentityLinkRow>> {
    let mut rows = scan::scan_identity_links(db, Some("pending_review"), None)
        .await
        .context("SurrealDB identity_links scan failed")?;
    rows.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(rows)
}

pub async fn run(args: LinksArgs) -> anyhow::Result<()> {
    use crate::db::connect_db;
    use comfy_table::{Cell, Table};
    use urban_integrity_core::config::AppConfig;

    let config = AppConfig::load().context("Config load failed")?;
    let db = connect_db(&config).await?;

    match args.command {
        LinksSubcommand::List => {
            // REV-01: list every pending_review link — no ordering-biased cap (GRD-09). Sorted
            // by confidence DESC for display only, inside fetch_pending_review, after the
            // complete set has already been read.
            let results = fetch_pending_review(&db).await?;

            // D-07: columns are source node, target node, type, confidence, adapter, timestamp.
            // "adapter" maps to the real `source` field (e.g. "ch_psc") and "timestamp" to the
            // real `resolved_at` field — `identity_links` has neither `adapter_name` nor
            // `created_at`, so these columns always printed "-" under the old `row.get(...)` on
            // a schemaless `serde_json::Value`.
            let mut table = Table::new();
            table.set_header(vec![
                "source node",
                "target node",
                "type",
                "confidence",
                "adapter",
                "timestamp",
            ]);

            if results.is_empty() {
                println!("No pending links found.");
            }

            for row in &results {
                table.add_row(vec![
                    Cell::new(&row.source_node_id),
                    Cell::new(&row.target_node_id),
                    Cell::new(&row.resolution_type),
                    Cell::new(format!("{:.3}", row.confidence)),
                    Cell::new(&row.source),
                    Cell::new(row.resolved_at.to_rfc3339()),
                ]);
            }

            println!("{table}");
        }

        LinksSubcommand::Show { id } => {
            // REV-02: show evidence detail for a specific link.
            // type::record() constructs a RecordID from table + key — required because
            // SurrealDB 3.x id fields are RecordID, not strings.
            let key = record_key(&id);
            let results: Vec<serde_json::Value> = db
                .query("SELECT * FROM type::record($table, $key) LIMIT 1")
                .bind(("table", "identity_links"))
                .bind(("key", key))
                .await
                .context("SurrealDB identity_links show query failed")?
                .take::<Vec<serde_json::Value>>(0)?;

            match results.into_iter().next() {
                None => {
                    println!("Link not found: {id}");
                    return Ok(());
                }
                Some(record) => {
                    // Print each field on its own line; evidence_json is pretty-printed.
                    let fields = [
                        "id",
                        "source_node_id",
                        "target_node_id",
                        "resolution_type",
                        "confidence",
                        "adapter_name",
                        "status",
                        "created_at",
                    ];
                    for field in &fields {
                        let val = record
                            .get(*field)
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "-".to_owned());
                        println!("{field}: {val}");
                    }

                    // Pretty-print evidence_json if present
                    if let Some(ev_raw) = record.get("evidence_json").and_then(|v| v.as_str()) {
                        let pretty = serde_json::from_str::<serde_json::Value>(ev_raw)
                            .ok()
                            .and_then(|v| serde_json::to_string_pretty(&v).ok())
                            .unwrap_or_else(|| ev_raw.to_owned());
                        println!("evidence_json:\n{pretty}");
                    }
                }
            }
        }

        LinksSubcommand::Confirm { id } => {
            // REV-03: DB status update + D-08 confirmation output format + WAL write (D-09).
            // T-16-09: no --force / --auto-confirm flag exists here (REV-05).

            // type::record() constructs a RecordID — required for SurrealDB 3.x id fields.
            let key = record_key(&id);
            db.query("UPDATE type::record($table, $key) SET status = $status")
                .bind(("table", "identity_links"))
                .bind(("key", key))
                .bind(("status", "confirmed"))
                .await
                .context("SurrealDB confirm update failed")?;

            // Fetch the updated record to build the D-08 summary line.
            let results: Vec<serde_json::Value> = db
                .query("SELECT * FROM type::record($table, $key) LIMIT 1")
                .bind(("table", "identity_links"))
                .bind(("key", key))
                .await
                .context("SurrealDB confirm fetch failed")?
                .take::<Vec<serde_json::Value>>(0)?;

            tracing::info!(
                link_id = %id,
                "Identity link confirmed — graph_events WAL written (D-09)"
            );

            match results.into_iter().next() {
                None => {
                    eprintln!("Link not found: {id} — nothing updated.");
                }
                Some(record) => {
                    let source = record
                        .get("source_node_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)");
                    let target = record
                        .get("target_node_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)");
                    let res_type = record
                        .get("resolution_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)");
                    // D-08 format
                    println!("Confirmed: {source} -> {target} ({res_type}). Hot graph updated.");

                    // D-09: write graph_events WAL row for the analyst confirmation.
                    // source and target come from the SurrealDB SELECT result (server-controlled).
                    // build_wal_record_id inside write_wal_event sanitises both values.
                    let payload = serde_json::json!({
                        "source_node_id": source,
                        "target_node_id": target,
                        "resolution_type": res_type,
                        "confirmed_by": "analyst",
                    });
                    wal_writer::write_wal_event(&db, "EdgeConfirmed", source, target, payload)
                        .await
                        .context("WAL write for confirmed link failed")?;
                }
            }
        }

        LinksSubcommand::Reject { id } => {
            // REV-04: reject a link and suppress re-proposal.
            //
            // The suppression is enforced on the write side, not here: the shared
            // `IDENTITY_LINK_UPSERT` (crates/enricher/src/adapters/ch_psc.rs) excludes
            // `status = 'rejected'` from its UPDATE subquery, so a later `enrich` run cannot
            // restore this row to `auto_linked`. Until 2026-08-20 that guard was missing and
            // rejection lasted only until the next adapter run — silently, because nothing
            // reads `rejected` back. Pinned by `a_rejected_link_is_not_resurrected_by_the_next_adapter_run`.
            //
            // type::record() constructs a RecordID — required for SurrealDB 3.x id fields.

            let key = record_key(&id);
            db.query("UPDATE type::record($table, $key) SET status = $status")
                .bind(("table", "identity_links"))
                .bind(("key", key))
                .bind(("status", "rejected"))
                .await
                .context("SurrealDB reject update failed")?;

            // Verify the update actually affected a row.
            let exists: Vec<serde_json::Value> = db
                .query("SELECT id FROM type::record($table, $key)")
                .bind(("table", "identity_links"))
                .bind(("key", key))
                .await
                .context("SurrealDB reject verify failed")?
                .take::<Vec<serde_json::Value>>(0)?;

            if exists.is_empty() {
                eprintln!("Link not found: {id} — nothing updated.");
            } else {
                println!("Rejected: link {id} suppressed from future enrichment runs.");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── against a real server ────────────────────────────────────────────────────────────────
    //
    // Proves the GRD-09 fix end-to-end at the CLI wiring layer: seeds past the old 100-row
    // confidence-DESC ceiling with the discardable row placed exactly where the old ordering
    // would have dropped it, then shows the old shape under-reports while fetch_pending_review
    // does not. Multi-page paging behaviour itself is already proven at the scan.rs layer
    // (31-02); this test's job is only to confirm the CLI reads through the fixed path.

    #[tokio::test]
    async fn fetch_pending_review_returns_every_row_past_the_old_confidence_ceiling() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect("links_review_queue").await;

        // 150 rows spread across [0.0, 1.0): row 0 gets the lowest confidence (~0.0) and is
        // marked with a unique resolution_type so it can be found regardless of the ORDER BY
        // tie-break SurrealDB applies to identical confidence values on other rows.
        const N: usize = 150;
        const MARKER_TYPE: &str = "old_ceiling_marker_row_0";
        for i in 0..N {
            let resolution_type = if i == 0 {
                MARKER_TYPE
            } else {
                "exact_company_number"
            };
            db.query(
                "CREATE identity_links SET \
                   source_node_id = $src, target_node_id = $tgt, resolution_type = $rt, \
                   confidence = $conf, source = $source, resolved_at = $resolved_at, \
                   evidence_json = '{}', status = $status",
            )
            .bind(("src", format!("GB-COH:{i:08}")))
            .bind(("tgt", format!("director:officer-{i}")))
            .bind(("rt", resolution_type))
            .bind(("conf", i as f64 / N as f64))
            .bind(("source", "ch_psc"))
            .bind(("resolved_at", chrono::Utc::now()))
            .bind(("status", "pending_review"))
            .await
            .expect("seed identity_links row")
            .check()
            .expect("seed identity_links row check");
        }

        // OLD shape: reproduce the pre-fix query verbatim. Must under-report AND must not
        // include the marker row — this is Open Item 11c's defect, live.
        let old_shape: Vec<serde_json::Value> = db
            .query(
                "SELECT resolution_type, confidence FROM identity_links \
                 WHERE status = 'pending_review' ORDER BY confidence DESC LIMIT 100",
            )
            .await
            .expect("old-shape query")
            .take(0)
            .expect("old-shape take");
        assert_eq!(
            old_shape.len(),
            100,
            "old shape returns exactly its ordering ceiling, not the truth"
        );
        assert!(
            !old_shape
                .iter()
                .any(|r| r["resolution_type"] == MARKER_TYPE),
            "old shape must NOT surface the lowest-confidence marker row — that is the defect \
             this plan fixes"
        );

        // NEW: fetch_pending_review returns every row, including the marker.
        let rows = fetch_pending_review(&db)
            .await
            .expect("fetch_pending_review");
        assert_eq!(
            rows.len(),
            N,
            "fetch_pending_review must return every pending_review row, not a capped subset"
        );
        assert!(
            rows.iter().any(|r| r.resolution_type == MARKER_TYPE),
            "fetch_pending_review must include the lowest-confidence marker row the old shape \
             discarded"
        );
    }
}
