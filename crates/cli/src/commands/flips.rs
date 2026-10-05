//! `flips` — analyst review surface for Pre-Permission Land Flip candidates (FLIP-04).
//!
//! Mirrors the proven `links` command (D-02/D-03) but routes only to the
//! `land_flip_candidates` review queue and NEVER injects into the hot graph: a confirmed
//! land-flip candidate is a review artifact, not a graph edge, and no buyer is named in the
//! graph (T-23-HotGraph). `confirm`/`dismiss` carry a free-text `--note`.
//!
//! # Security
//!
//! - T-23-LogLeak: `analyst_note` is NEVER logged at INFO — only id/status go to INFO.
//! - T-23-Tamper-CLI: every value (id key, status, note) is bound via `.bind(...)`; no
//!   string interpolation.
//! - T-23-HotGraph: confirm/dismiss do NOT write graph_events/WAL (unlike `links confirm`).

use anyhow::Context as _;
use urban_integrity_core::persistence::scan;

/// Arguments for the `flips` subcommand group.
#[derive(Debug, clap::Args)]
pub struct FlipsArgs {
    #[command(subcommand)]
    pub command: FlipsSubcommand,
}

/// Subcommands for the `flips` review queue (FLIP-04; D-02/D-03).
#[derive(Debug, clap::Subcommand)]
pub enum FlipsSubcommand {
    /// List pending land-flip candidates (review queue, newest first)
    List,
    /// Show all three gate-evidence sections for one candidate
    Show {
        /// Land-flip candidate ID
        id: String,
    },
    /// Confirm a candidate as a genuine land-flip (does NOT touch the hot graph)
    Confirm {
        /// Land-flip candidate ID
        id: String,
        /// Optional free-text analyst note
        #[arg(long)]
        note: Option<String>,
    },
    /// Dismiss a candidate and suppress re-proposal (does NOT touch the hot graph)
    Dismiss {
        /// Land-flip candidate ID
        id: String,
        /// Optional free-text analyst note
        #[arg(long)]
        note: Option<String>,
    },
}

/// Extract the key suffix from a full record ID like "land_flip_candidates:abc123".
/// SurrealDB 3.x `id` fields are RecordID, not strings, so `WHERE id = $id` with a string
/// binding never matches — use `type::record($table, $key)` in SurrealQL instead.
fn record_key(id: &str) -> &str {
    id.split_once(':').map(|(_, k)| k).unwrap_or(id)
}

const TABLE: &str = "land_flip_candidates";

/// FLIP-04: fetch every `pending_review` land-flip candidate — no ordering-biased cap.
///
/// Reads through [`scan::scan_land_flip_candidates`], whose paged scan orders `ORDER BY id` and
/// terminates only on an empty page, so it cannot silently drop rows the way the old `ORDER BY
/// created_at DESC LIMIT 100` did (GRD-09 / Open Item 11c: a newest-first cap hides the oldest
/// stale rows, which is exactly what this review queue exists to surface). `created_at`
/// descending is applied here, in Rust, over the COMPLETE set already in hand — display ordering
/// only, never a paging/reading order. RFC3339 strings sort lexicographically in date order
/// (convention already used at `registry.rs:118-119`).
async fn fetch_pending_review(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut rows = scan::scan_land_flip_candidates(db, Some("pending_review"))
        .await
        .context("SurrealDB land_flip_candidates scan failed")?;
    rows.sort_by(|a, b| {
        b["created_at"]
            .as_str()
            .unwrap_or("")
            .cmp(a["created_at"].as_str().unwrap_or(""))
    });
    Ok(rows)
}

pub async fn run(args: FlipsArgs) -> anyhow::Result<()> {
    use crate::db::connect_db;
    use comfy_table::{Cell, Table};
    use urban_integrity_core::config::AppConfig;

    let config = AppConfig::load().context("Config load failed")?;
    let db = connect_db(&config).await?;

    match args.command {
        FlipsSubcommand::List => {
            // FLIP-04: list every pending_review candidate — no ordering-biased cap (GRD-09),
            // newest-first for display only, applied inside fetch_pending_review after the
            // complete set has already been read.
            let results = fetch_pending_review(&db).await?;

            let mut table = Table::new();
            table.set_header(vec![
                "id",
                "tier",
                "severity",
                "planning_ref",
                "ppd_date",
                "price_gbp",
                "corr_conf",
                "date_source",
            ]);

            if results.is_empty() {
                println!("No pending land-flip candidates found.");
            }

            for row in &results {
                let get = |k: &str| {
                    row.get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or("-")
                        .to_owned()
                };
                let id = row
                    .get("id")
                    .map(|v| v.to_string().trim_matches('"').to_owned())
                    .unwrap_or_else(|| "-".to_owned());
                let price = row
                    .get("ppd_price_gbp")
                    .and_then(|v| v.as_i64())
                    .map(|n| format!("£{n}"))
                    .unwrap_or_else(|| "-".to_owned());
                let corr = row
                    .get("corroborating_link_confidence")
                    .and_then(|v| v.as_f64())
                    .map(|f| format!("{f:.3}"))
                    .unwrap_or_else(|| "-".to_owned());

                table.add_row(vec![
                    Cell::new(&id),
                    Cell::new(get("tier")),
                    Cell::new(get("severity")),
                    Cell::new(get("planning_ref")),
                    Cell::new(get("ppd_date_of_transfer")),
                    Cell::new(&price),
                    Cell::new(&corr),
                    Cell::new(get("date_source")),
                ]);
            }

            println!("{table}");
        }

        FlipsSubcommand::Show { id } => {
            // FLIP-04: show the three gate-evidence sections for one candidate.
            let key = record_key(&id);
            let results: Vec<serde_json::Value> = db
                .query("SELECT * FROM type::record($table, $key) LIMIT 1")
                .bind(("table", TABLE))
                .bind(("key", key))
                .await
                .context("SurrealDB land_flip_candidates show query failed")?
                .take::<Vec<serde_json::Value>>(0)?;

            match results.into_iter().next() {
                None => {
                    println!("Candidate not found: {id}");
                    return Ok(());
                }
                Some(record) => {
                    let field = |k: &str| {
                        record
                            .get(k)
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "-".to_owned())
                    };

                    println!("id:                       {}", field("id"));
                    println!("status:                   {}", field("status"));
                    println!("tier:                     {}", field("tier"));
                    println!("severity:                 {}", field("severity"));
                    println!(
                        "buyer_identity_confirmed: {}",
                        field("buyer_identity_confirmed")
                    );
                    println!("planning_ref:             {}", field("planning_ref"));
                    if record
                        .get("suppression_reason")
                        .is_some_and(|v| !v.is_null())
                    {
                        println!("suppression_reason:       {}", field("suppression_reason"));
                    }
                    println!("created_at:               {}", field("created_at"));

                    // gate-1: structural address match
                    println!("\n── gate-1 (address) ──");
                    println!("  ppd_postcode: {}", field("ppd_postcode"));
                    println!("  ppd_paon:     {}", field("ppd_paon"));
                    println!("  ppd_street:   {}", field("ppd_street"));
                    println!("  ppd_txn_id:   {}", field("ppd_transaction_id"));
                    println!("  price_gbp:    {}", field("ppd_price_gbp"));

                    // gate-2: temporal window
                    println!("\n── gate-2 (temporal) ──");
                    println!(
                        "  ppd_date_of_transfer:      {}",
                        field("ppd_date_of_transfer")
                    );
                    println!(
                        "  application_received_date: {}",
                        field("application_received_date")
                    );
                    println!(
                        "  temporal_offset_days:      {}",
                        field("temporal_offset_days")
                    );
                    println!("  date_source:               {}", field("date_source"));

                    // gate-3: connection corroboration
                    println!("\n── gate-3 (corroboration) ──");
                    println!(
                        "  corroborating_link_id:         {}",
                        field("corroborating_link_id")
                    );
                    println!(
                        "  corroborating_link_confidence: {}",
                        field("corroborating_link_confidence")
                    );
                    println!(
                        "  corroborating_entity_type:     {}",
                        field("corroborating_entity_type")
                    );

                    if record.get("analyst_note").is_some_and(|v| !v.is_null()) {
                        println!("\nanalyst_note: {}", field("analyst_note"));
                    }
                }
            }
        }

        FlipsSubcommand::Confirm { id, note } => {
            update_status(&db, &id, "confirmed", note.as_deref(), "Confirmed").await?;
        }

        FlipsSubcommand::Dismiss { id, note } => {
            update_status(&db, &id, "dismissed", note.as_deref(), "Dismissed").await?;
        }
    }

    Ok(())
}

/// Shared confirm/dismiss path: update status (+ optional note + reviewed_at), then
/// re-SELECT to confirm a row was actually hit. Does NOT write any WAL/graph_events row —
/// candidates are review-queue, not graph edges (T-23-HotGraph).
async fn update_status(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    id: &str,
    status: &str,
    note: Option<&str>,
    verb: &str,
) -> anyhow::Result<()> {
    let key = record_key(id);
    // All values bound — id key, status, and the free-text note (T-23-Tamper-CLI).
    db.query(
        "UPDATE type::record($table, $key) SET status = $status, analyst_note = $note, \
         reviewed_at = time::now()",
    )
    .bind(("table", TABLE))
    .bind(("key", key))
    .bind(("status", status.to_string()))
    .bind(("note", note.map(|s| s.to_string())))
    .await
    .context("SurrealDB land_flip_candidates status update failed")?;

    // T-23-LogLeak: log only id + status at INFO — NEVER the analyst note.
    tracing::info!(candidate_id = %id, status = %status, "Land-flip candidate reviewed");

    let exists: Vec<serde_json::Value> = db
        .query("SELECT id FROM type::record($table, $key)")
        .bind(("table", TABLE))
        .bind(("key", key))
        .await
        .context("SurrealDB land_flip_candidates verify failed")?
        .take::<Vec<serde_json::Value>>(0)?;

    if exists.is_empty() {
        eprintln!("Candidate not found: {id} — nothing updated.");
    } else {
        println!(
            "{verb}: candidate {id} (status = {status}). Review queue only — hot graph untouched."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Wrapper so we can parse the `flips` subcommand shape via clap derive in a test.
    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(subcommand)]
        cmd: FlipsSubcommand,
    }

    #[test]
    fn record_key_strips_table_prefix() {
        assert_eq!(record_key("land_flip_candidates:abc123"), "abc123");
        // No prefix → returned unchanged.
        assert_eq!(record_key("abc123"), "abc123");
        // Composite key with extra colons → only the first split is consumed.
        assert_eq!(record_key("land_flip_candidates:TXN__REF"), "TXN__REF");
    }

    #[test]
    fn parses_confirm_with_note() {
        let cli = TestCli::parse_from([
            "flips",
            "confirm",
            "land_flip_candidates:x",
            "--note",
            "verified",
        ]);
        match cli.cmd {
            FlipsSubcommand::Confirm { id, note } => {
                assert_eq!(id, "land_flip_candidates:x");
                assert_eq!(note.as_deref(), Some("verified"));
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn parses_dismiss_and_list_and_show() {
        assert!(matches!(
            TestCli::parse_from(["flips", "list"]).cmd,
            FlipsSubcommand::List
        ));
        assert!(matches!(
            TestCli::parse_from(["flips", "show", "id1"]).cmd,
            FlipsSubcommand::Show { .. }
        ));
        // Dismiss without a note is valid (note is optional).
        match TestCli::parse_from(["flips", "dismiss", "id2"]).cmd {
            FlipsSubcommand::Dismiss { id, note } => {
                assert_eq!(id, "id2");
                assert!(note.is_none());
            }
            other => panic!("expected Dismiss, got {other:?}"),
        }
    }

    // ── against a real server ────────────────────────────────────────────────────────────────
    //
    // GRD-09 companion to links.rs's confidence-ceiling test: seeds past the old 100-row
    // created_at-DESC ceiling with the discardable (oldest) row placed exactly where the old
    // ordering would have dropped it.

    #[tokio::test]
    async fn fetch_pending_review_returns_every_row_past_the_old_created_at_ceiling() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect("flips_review_queue").await;

        // 150 rows spread across a date range: row 0 is the OLDEST (created_at furthest in the
        // past) and is unambiguously outside the old `ORDER BY created_at DESC LIMIT 100`
        // ceiling regardless of insertion order.
        const N: i64 = 150;
        let now = chrono::Utc::now();
        for i in 0..N {
            let created_at = now - chrono::Duration::days(N - i);
            db.query(
                "CREATE land_flip_candidates SET \
                   tier = 'tier_2', severity = 'medium', status = $status, \
                   buyer_identity_confirmed = false, date_source = 'ppd', \
                   ppd_transaction_id = $txn, ppd_postcode = 'AB1 2CD', \
                   ppd_paon = '12', ppd_street = 'HIGH STREET', ppd_price_gbp = 100000, \
                   ppd_date_of_transfer = $created_at, planning_ref = $planning_ref, \
                   created_at = $created_at",
            )
            .bind(("status", "pending_review"))
            .bind(("txn", format!("TXN-{i:04}")))
            .bind(("planning_ref", format!("PLAN-{i:04}")))
            .bind(("created_at", created_at))
            .await
            .expect("seed land_flip_candidates row")
            .check()
            .expect("seed land_flip_candidates row check");
        }

        // OLD shape: reproduce the pre-fix query verbatim. Must under-report AND must not
        // include the oldest row's marker transaction id.
        let old_shape: Vec<serde_json::Value> = db
            .query(
                "SELECT ppd_transaction_id, created_at FROM land_flip_candidates \
                 WHERE status = 'pending_review' ORDER BY created_at DESC LIMIT 100",
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
                .any(|r| r["ppd_transaction_id"] == "TXN-0000"),
            "old shape must NOT surface the oldest row — that is the defect this plan fixes"
        );

        // NEW: fetch_pending_review returns every row, including the oldest.
        let rows = fetch_pending_review(&db)
            .await
            .expect("fetch_pending_review");
        assert_eq!(
            rows.len(),
            N as usize,
            "fetch_pending_review must return every pending_review row, not a capped subset"
        );
        assert!(
            rows.iter().any(|r| r["ppd_transaction_id"] == "TXN-0000"),
            "fetch_pending_review must include the oldest row the old shape discarded"
        );
    }
}
