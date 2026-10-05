//! `anomalies` — list and adjudicate what the pipeline refused to guess at.
//!
//! An anomaly is not an alert. An alert says *"this looks like corruption"*; an anomaly says
//! *"this input could not be trusted, and here is exactly what was skipped"*. They are kept apart
//! so data-quality noise never enters an analyst's corruption queue, and so a genuine finding
//! cannot hide among parse failures.

use anyhow::Context as _;

#[derive(Debug, clap::Args)]
pub struct AnomaliesArgs {
    #[command(subcommand)]
    pub command: AnomaliesSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum AnomaliesSubcommand {
    /// List open anomalies, highest severity first
    List {
        /// Only this kind, e.g. `officer_key_collision`
        #[arg(long)]
        kind: Option<String>,
        /// Include reviewed and dismissed rows
        #[arg(long)]
        all: bool,
    },
    /// Show the full evidence for one anomaly
    Show {
        /// Anomaly record id or key
        id: String,
    },
    /// Mark an anomaly as adjudicated
    Review {
        id: String,
        /// Record it as not worth acting on instead of accepted
        #[arg(long)]
        dismiss: bool,
    },
}

/// Strip the table prefix from a record id, if present.
///
/// **Not** `split_once(':')`. An anomaly key legitimately contains colons — it is built from the
/// source (`backfill:officers:audit`) and the subject (`director:smith-…`) — so splitting on the
/// first one truncated the key and `anomalies show` reported every real anomaly as absent.
fn record_key(id: &str) -> &str {
    id.strip_prefix("data_anomalies:")
        .unwrap_or(id)
        .trim_matches('`')
}

pub async fn run(args: AnomaliesArgs) -> anyhow::Result<()> {
    use crate::db::connect_db;
    use comfy_table::{Cell, Table};
    use urban_integrity_core::config::AppConfig;

    let config = AppConfig::load().context("Config load failed")?;
    let db = connect_db(&config).await?;

    match args.command {
        AnomaliesSubcommand::List { kind, all } => {
            // `severity` is a string column, so ordering is done here rather than in SurrealQL —
            // a lexical ORDER BY would put "high" after "low".
            let mut sql = String::from("SELECT * FROM data_anomalies");
            let mut clauses: Vec<&str> = Vec::new();
            if !all {
                clauses.push("status = 'open'");
            }
            if kind.is_some() {
                clauses.push("kind = $kind");
            }
            if !clauses.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&clauses.join(" AND "));
            }
            sql.push_str(" LIMIT 500");

            let mut q = db.query(&sql);
            if let Some(k) = kind.as_deref() {
                q = q.bind(("kind", k.to_string()));
            }
            let mut rows: Vec<serde_json::Value> = q
                .await
                .context("SurrealDB data_anomalies query failed")?
                .take(0)?;

            let rank = |s: &str| match s {
                "high" => 0,
                "medium" => 1,
                _ => 2,
            };
            rows.sort_by_key(|r| rank(r.get("severity").and_then(|v| v.as_str()).unwrap_or("low")));

            if rows.is_empty() {
                println!("No open anomalies.");
                return Ok(());
            }

            let mut table = Table::new();
            table.set_header(vec!["severity", "kind", "subject", "source", "detail"]);
            for r in &rows {
                let get = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("-").to_string();
                let mut detail = get("detail");
                if detail.chars().count() > 90 {
                    detail = detail.chars().take(87).collect::<String>() + "…";
                }
                table.add_row(vec![
                    Cell::new(get("severity")),
                    Cell::new(get("kind")),
                    Cell::new(get("subject")),
                    Cell::new(get("source")),
                    Cell::new(detail),
                ]);
            }
            println!("{table}");
            println!(
                "\n{} anomaly/ies. `anomalies show <id>` for full evidence.",
                rows.len()
            );
        }

        AnomaliesSubcommand::Show { id } => {
            let rows: Vec<serde_json::Value> = db
                .query("SELECT * FROM type::record($tb, $key)")
                .bind(("tb", "data_anomalies"))
                .bind(("key", record_key(&id).to_string()))
                .await
                .context("SurrealDB anomaly lookup failed")?
                .take(0)?;
            match rows.first() {
                None => println!("No anomaly found for {id}"),
                Some(r) => {
                    println!("{}", serde_json::to_string_pretty(r)?);
                    if let Some(ev) = r.get("evidence_json").and_then(|v| v.as_str()) {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(ev) {
                            println!("\nevidence:\n{}", serde_json::to_string_pretty(&parsed)?);
                        }
                    }
                }
            }
        }

        AnomaliesSubcommand::Review { id, dismiss } => {
            let status = if dismiss { "dismissed" } else { "reviewed" };
            let updated: Vec<serde_json::Value> = db
                .query("UPDATE type::record($tb, $key) SET status = $status")
                .bind(("tb", "data_anomalies"))
                .bind(("key", record_key(&id).to_string()))
                .bind(("status", status))
                .await
                .context("SurrealDB anomaly update failed")?
                .take(0)?;
            if updated.is_empty() {
                anyhow::bail!("no anomaly found for {id} — nothing was updated");
            }
            println!("{id} marked {status}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::record_key;

    /// The keys carry colons from both the source and the subject. Splitting on the first one
    /// silently truncates, and the lookup then finds nothing — a wrong answer that looks like
    /// "no such anomaly".
    #[test]
    fn a_key_containing_colons_survives_prefix_stripping() {
        let key = "officer_key_collision_backfill:officers:audit_director:smith-jane-b197006";
        assert_eq!(record_key(key), key, "a bare key must pass through intact");
        assert_eq!(
            record_key(&format!("data_anomalies:{key}")),
            key,
            "only the table prefix may be removed"
        );
        assert_eq!(
            record_key(&format!("data_anomalies:`{key}`")),
            key,
            "SurrealDB backtick-quotes keys with special characters"
        );
    }
}
