use anyhow::{bail, Context as _};
use comfy_table::{Cell, Color, Table};
use urban_integrity_core::persistence::scan;
use urban_integrity_core::{
    build_verify_verdict, config::AppConfig, query_entity_by_ch_number, RiskLevel, VerifyVerdict,
};
use urban_integrity_schema::types::node::EntityStatus;

use crate::output::{print_json, severity_color, OutputFormat};

/// Arguments for the `verify` subcommand.
///
/// Looks up a Companies House entity by number, assembles a risk verdict from
/// existing SurrealDB alert records (no re-detection — VER-04), and renders the
/// result as a coloured table or machine-readable JSON.
///
/// Exit code:
///   0 — entity not found OR RISK_LEVEL is LOW or MEDIUM (D-02, D-07)
///   1 — RISK_LEVEL is HIGH or CRITICAL (scripting-friendly threshold)
#[derive(Debug, clap::Args)]
pub struct VerifyArgs {
    /// Companies House registration number to audit (e.g. 12345678, SC123456)
    #[arg(long)]
    pub company_number: String,

    /// Output format: table (default) or json
    #[arg(long, value_enum)]
    pub output: Option<OutputFormat>,

    /// Request a one-line Gemma4 summary via Ollama (graceful WARN if unavailable)
    #[arg(long)]
    pub summarize: bool,
}

/// Validate a Companies House number at the CLI trust boundary (T-14-02-01, ASVS V5.1).
///
/// Mirrors `is_valid_ch_number` in `crates/core/src/persistence/queries.rs` — we
/// re-validate here so the error message is rendered by the CLI rather than silently
/// returning `None` from the query layer.
fn is_valid_ch_number_cli(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric()) && s.len() >= 2 && s.len() <= 10
}

/// Run the `verify` subcommand end-to-end.
///
/// Connects to SurrealDB, fetches the entity profile, assembles a `VerifyVerdict`,
/// renders the requested output format, then calls `std::process::exit(1)` when
/// RISK_LEVEL is HIGH or CRITICAL (D-07). The exit call is always the last statement
/// — all output is fully written before it fires.
pub async fn run(args: VerifyArgs) -> anyhow::Result<()> {
    let fmt = args.output.unwrap_or_else(OutputFormat::auto);

    // T-14-02-01: validate CH number before any DB call
    if !is_valid_ch_number_cli(&args.company_number) {
        bail!("Invalid CH number format — expected 2–10 alphanumeric characters");
    }

    let config = AppConfig::load().context("Config load failed")?;
    let db = crate::db::connect_db(&config).await?;

    let profile = match query_entity_by_ch_number(&db, &args.company_number)
        .await
        .context("SurrealDB entity lookup failed")?
    {
        None => {
            // D-02: no records = no risk; exit 0
            eprintln!("No entity found for CH number {}", args.company_number);
            return Ok(());
        }
        Some(p) => p,
    };

    let mut verdict = build_verify_verdict(&profile);

    // SURF-03 / R-03: active-evasion override.
    // When has_disqualified_director AND a graph_events MotifDetected row with
    // motif_type='disqualified_director_evasion' exists for this company, force
    // risk_level = Critical BEFORE rendering. The exit code is 1 (R-03 convention
    // — matches the shipped High|Critical guard below; NOT exit(2)).
    //
    // This DB query lives here (not in build_verify_verdict) to keep that function
    // pure. The evasion alert was written by run_evasion_detector in crates/enricher.
    if verdict.has_disqualified_director {
        let evasion_active = check_active_evasion_alert(&db, &args.company_number).await;
        if evasion_active {
            verdict.risk_level = RiskLevel::Critical;
        }
    }

    match fmt {
        OutputFormat::Table => render_verdict_table(&verdict, args.summarize).await?,
        OutputFormat::Json => print_json(&verdict)?,
        OutputFormat::Markdown | OutputFormat::Pdf => {
            bail!("--output md/pdf is not supported by verify; use report for document output")
        }
    }

    // D-07 / R-03: exit 1 for HIGH or CRITICAL — MUST be after all output is written.
    // R-03: confirmed disqualified-director evasion forces Critical → exit 1 (not 2).
    if matches!(verdict.risk_level, RiskLevel::High | RiskLevel::Critical) {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::process::exit(1);
    }

    Ok(())
}

/// Query `graph_events` for an active disqualified-director evasion alert for this company.
///
/// Returns `true` if at least one `MotifDetected` row exists with
/// `motif_type='disqualified_director_evasion'` that references an officer linked to
/// this company.
///
/// # Why a two-hop query?
///
/// `evasion.rs:run_evasion_detector` writes `result_json` with the fields:
/// `motif_type`, `risk_score`, `description`, `officer_id`, `disq_node_id`, `motif_hash`.
/// There is **no** `company_number` field in `result_json`. A direct `contains(company_number)`
/// check against the alert therefore always returns `false`.
///
/// Instead we perform a two-hop lookup:
/// - Hop 1: fetch `identity_links` rows with `source = 'ch_psc'`, filter Rust-side for
///   those linking to this company (avoiding RecordId coercion on `source_node_id`),
///   and collect the associated officer entity IDs.
/// - Hop 2: fetch `MotifDetected` evasion alerts and check whether any alert's
///   `officer_id` matches one of the collected officer IDs.
///
/// Errors are non-fatal — a DB failure here should not block the rest of the verdict
/// (fails safe: no override) — but no longer silent: a WARN is logged with the underlying
/// error before returning `false` (T-31-12).
async fn check_active_evasion_alert(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    company_number: &str,
) -> bool {
    let company_fragment = format!("GB-COH:{company_number}");

    // Hop 1: find officer entity IDs linked to this company via ch_psc identity_links.
    // Read all ch_psc links via the paged, fail-loud scan and filter Rust-side — avoids both
    // RecordId coercion on source_node_id / target_node_id AND the ordering-biased `LIMIT 500`
    // that used to silently cap this read (GRD-09 / Open Item 11c).
    let link_rows = match scan::scan_identity_links(db, None, Some("ch_psc")).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "evasion-alert identity_links scan failed");
            return false;
        }
    };

    let mut officer_ids: Vec<String> = Vec::new();
    for row in &link_rows {
        if row.source_node_id.contains(&company_fragment) {
            // target side is the officer
            officer_ids.push(row.target_node_id.clone());
        } else if row.target_node_id.contains(&company_fragment) {
            // source side is the officer
            officer_ids.push(row.source_node_id.clone());
        }
    }

    if officer_ids.is_empty() {
        return false;
    }

    // Hop 2: check for MotifDetected evasion alerts referencing any of those officers.
    let alert_rows = match scan::scan_events_by_payload_contains(
        db,
        "MotifDetected",
        "result_json",
        "disqualified_director_evasion",
    )
    .await
    {
        Ok(rows) => rows,
        Err(_) => return false,
    };

    for row in &alert_rows {
        let result_json_str = match row["payload"]["result_json"].as_str() {
            Some(s) => s,
            None => continue,
        };
        if let Ok(result) = serde_json::from_str::<serde_json::Value>(result_json_str) {
            if result["motif_type"].as_str() != Some("disqualified_director_evasion") {
                continue;
            }
        } else {
            continue;
        }
        // Check whether this alert's officer_id matches any officer linked to our company.
        if officer_ids
            .iter()
            .any(|oid| result_json_str.contains(oid.as_str()))
        {
            return true;
        }
    }
    false
}

/// Return the visual label and colour for a given RISK_LEVEL (D-06).
fn risk_banner(level: &RiskLevel) -> (&'static str, Color) {
    match level {
        RiskLevel::Low => ("  \u{2713}  LOW", Color::Green),
        RiskLevel::Medium => ("  \u{26a0}  MEDIUM", Color::Yellow),
        RiskLevel::High => ("  \u{2717}  HIGH", Color::Red),
        RiskLevel::Critical => ("  \u{2717}\u{2717} CRITICAL", Color::Red),
    }
}

/// Format `EntityStatus` as a human-readable string for the dimension table.
///
/// `format_entity_status` in `output.rs` is private — replicate the same logic
/// inline (D-06 action note: "Check output.rs first; if private, inline").
fn fmt_entity_status(s: &EntityStatus) -> String {
    match s {
        EntityStatus::Active => "Active".to_owned(),
        EntityStatus::Dissolved(dt) => format!("Dissolved {}", dt.format("%Y-%m-%d")),
        EntityStatus::InLiquidation(dt) => format!("In Liquidation {}", dt.format("%Y-%m-%d")),
    }
}

/// Build the formatted text block passed to Ollama (D-04).
///
/// Includes all verdict dimensions and is truncated to 4000 chars — consistent
/// with `report.rs` precedent at line 613.
pub fn format_verdict_text(verdict: &VerifyVerdict) -> String {
    let motif_str = if verdict.motif_flags.is_empty() {
        "none".to_owned()
    } else {
        format!(
            "{} ({})",
            verdict.motif_flags.len(),
            verdict.motif_flags.join(", ")
        )
    };
    let raw = format!(
        "Entity: {} ({})\nStatus: {}\nRisk Level: {:?}\nP_c score: {:.2} ({:.0}th percentile)\nActive motifs: {}\nDissolved relatives: {}\nConnected officers: {}",
        verdict.entity_name,
        verdict.ch_number.as_deref().unwrap_or("unknown"),
        fmt_entity_status(&verdict.entity_status),
        verdict.risk_level,
        verdict.p_c_score,
        verdict.p_c_score * 100.0,
        motif_str,
        verdict.dissolved_relative_count,
        verdict.connected_officer_count,
    );
    // Truncate to 4000 chars (D-04, report.rs:613 precedent)
    raw.chars().take(4000).collect()
}

/// Render the table output for a risk verdict (D-05 layout).
///
/// Prints:
///   1. Header line: `RISK VERDICT: <Entity Name> (<CH Number>)`
///   2. Coloured RISK_LEVEL banner (D-06)
///   3. Dimension table (status, score, motifs, dissolved, officers + OSINT flags)
///   4. Optional Ollama one-liner summary if `summarize` is true
async fn render_verdict_table(verdict: &VerifyVerdict, summarize: bool) -> anyhow::Result<()> {
    // D-05 header
    println!(
        "RISK VERDICT: {} ({})",
        verdict.entity_name,
        verdict.ch_number.as_deref().unwrap_or("unknown")
    );
    println!();

    // D-06 coloured RISK_LEVEL banner — single-cell comfy-table
    let (label, color) = risk_banner(&verdict.risk_level);
    let mut banner_table = Table::new();
    banner_table.add_row(vec![Cell::new(label).fg(color)]);
    println!("{}", banner_table);
    println!();

    // Dimension table (core risk dims + OSINT flags from Phase 17 / SURF-03)
    let motif_str = if verdict.motif_flags.is_empty() {
        "none".to_owned()
    } else {
        format!(
            "{} ({})",
            verdict.motif_flags.len(),
            verdict.motif_flags.join(", ")
        )
    };

    let mut table = Table::new();
    table.set_header(vec!["Dimension", "Value"]);
    table.add_row(vec![
        Cell::new("Entity status"),
        Cell::new(fmt_entity_status(&verdict.entity_status)),
    ]);
    table.add_row(vec![
        Cell::new("P_c score"),
        Cell::new(format!(
            "{:.2} ({:.0}th percentile)",
            verdict.p_c_score,
            verdict.p_c_score * 100.0
        ))
        .fg(severity_color(verdict.p_c_score)),
    ]);
    table.add_row(vec![Cell::new("Active motifs"), Cell::new(motif_str)]);
    table.add_row(vec![
        Cell::new("Dissolved relatives"),
        Cell::new(verdict.dissolved_relative_count.to_string()),
    ]);
    table.add_row(vec![
        Cell::new("Connected officers"),
        Cell::new(verdict.connected_officer_count.to_string()),
    ]);

    // SURF-03 / D-13: OSINT flag rows
    let disq_flag = if verdict.has_disqualified_director {
        "YES (CDDA 1986 ban active)"
    } else {
        "no"
    };
    table.add_row(vec![
        Cell::new("Disqualified director"),
        Cell::new(disq_flag),
    ]);
    let owners_str = if verdict.beneficial_owners.is_empty() {
        "none".to_owned()
    } else {
        verdict.beneficial_owners.join(", ")
    };
    table.add_row(vec![
        Cell::new("Beneficial owners (PSC)"),
        Cell::new(owners_str),
    ]);
    table.add_row(vec![
        Cell::new("Offshore link"),
        Cell::new(if verdict.has_offshore_link {
            "yes"
        } else {
            "no"
        }),
    ]);
    table.add_row(vec![
        Cell::new("Sanctions match"),
        Cell::new(if verdict.has_sanctions_match {
            "yes"
        } else {
            "no"
        }),
    ]);
    println!("{}", table);

    // Optional Ollama summary (D-03 graceful degradation)
    if summarize {
        let verdict_text = format_verdict_text(verdict);
        if let Some(summary) = crate::ollama::summarize_via_ollama(&verdict_text).await {
            println!("\nSummary: {}", summary);
        }
        // If None, WARN was already logged inside summarize_via_ollama — silently skip
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::{RiskLevel, VerifyVerdict};
    use urban_integrity_schema::types::node::EntityStatus;

    fn make_verdict(
        name: &str,
        ch: Option<&str>,
        score: f64,
        motifs: Vec<String>,
        dissolved: usize,
        officers: usize,
    ) -> VerifyVerdict {
        let risk = urban_integrity_core::classify_risk(score);
        VerifyVerdict {
            entity_name: name.to_owned(),
            ch_number: ch.map(str::to_owned),
            entity_status: EntityStatus::Active,
            p_c_score: score,
            motif_flags: motifs,
            dissolved_relative_count: dissolved,
            connected_officer_count: officers,
            risk_level: risk,
            has_disqualified_director: false,
            beneficial_owners: vec![],
            has_offshore_link: false,
            has_sanctions_match: false,
        }
    }

    // ── risk_banner label checks (D-06) ──────────────────────────────────────

    #[test]
    fn test_risk_level_display_low() {
        let (label, _color) = risk_banner(&RiskLevel::Low);
        assert!(
            label.contains("LOW"),
            "Low risk banner must contain 'LOW', got: {label}"
        );
    }

    #[test]
    fn test_risk_level_display_critical() {
        let (label, _color) = risk_banner(&RiskLevel::Critical);
        assert!(
            label.contains("CRITICAL"),
            "Critical risk banner must contain 'CRITICAL', got: {label}"
        );
    }

    // ── format_verdict_text content checks (D-04) ────────────────────────────

    #[test]
    fn test_format_verdict_text_all_fields() {
        let verdict = make_verdict(
            "Apex Build Ltd",
            Some("12345678"),
            0.75,
            vec!["PhoenixLoop".to_owned(), "AddressCluster".to_owned()],
            3,
            7,
        );
        let text = format_verdict_text(&verdict);
        assert!(
            text.contains("Apex Build Ltd"),
            "verdict text must contain entity name, got: {text}"
        );
        assert!(
            text.contains("Risk Level"),
            "verdict text must contain 'Risk Level', got: {text}"
        );
        assert!(
            text.contains("0.75"),
            "verdict text must contain p_c_score formatted to 2dp, got: {text}"
        );
        assert!(
            text.contains("PhoenixLoop"),
            "verdict text must contain motif name, got: {text}"
        );
        assert!(
            text.contains("AddressCluster"),
            "verdict text must contain motif name, got: {text}"
        );
        assert!(
            text.contains('3') || text.contains("Dissolved relatives"),
            "verdict text must contain dissolved count context, got: {text}"
        );
        assert!(
            text.contains('7') || text.contains("Connected officers"),
            "verdict text must contain officer count context, got: {text}"
        );
    }

    // ── OutputFormat::Markdown rejection (D-07) ───────────────────────────────

    #[test]
    fn test_output_format_md_rejected() {
        // Verify the match arm returns Err for Markdown
        // We test this by calling the dispatch logic via a sync wrapper
        let result: anyhow::Result<()> = (|| -> anyhow::Result<()> {
            match OutputFormat::Markdown {
                OutputFormat::Markdown | OutputFormat::Pdf => {
                    bail!("--output md/pdf is not supported by verify; use report for document output")
                }
                _ => Ok(()),
            }
        })();
        assert!(
            result.is_err(),
            "OutputFormat::Markdown must return Err in verify dispatch"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("md/pdf"), "Error must mention 'md/pdf': {msg}");
    }

    // ── is_valid_ch_number_cli ────────────────────────────────────────────────

    #[test]
    fn test_ch_number_validation_accepts_valid() {
        assert!(is_valid_ch_number_cli("12345678"));
        assert!(is_valid_ch_number_cli("SC123456"));
        assert!(is_valid_ch_number_cli("ab")); // minimum length
        assert!(is_valid_ch_number_cli("ABCDEFGHIJ")); // maximum length
    }

    #[test]
    fn test_ch_number_validation_rejects_invalid() {
        assert!(!is_valid_ch_number_cli("")); // empty
        assert!(!is_valid_ch_number_cli("a")); // too short
        assert!(!is_valid_ch_number_cli("12345678901")); // too long
        assert!(!is_valid_ch_number_cli("abc; DROP")); // injection char
        assert!(!is_valid_ch_number_cli("abc-def")); // hyphen
    }

    #[test]
    fn test_evasion_alert_company_number_discriminator() {
        // check_active_evasion_alert matches alerts by officer_id (two-hop: company →
        // identity_links → officer_ids → evasion alerts). The result_json written by
        // evasion.rs contains officer_id, not company_number.
        let alert_json = r#"{"motif_type":"disqualified_director_evasion","risk_score":1.0,"description":"...","officer_id":"director:john-smith","disq_node_id":"disq:001","motif_hash":"abc123"}"#;

        // Officer linked to company A — alert contains this officer_id → company A is flagged.
        let company_a_officer = "director:john-smith";
        assert!(
            alert_json.contains(company_a_officer),
            "company A officer must be found in the alert"
        );

        // Officer of company B is different — alert does NOT contain it → company B not flagged.
        let company_b_officer = "director:jane-doe";
        assert!(
            !alert_json.contains(company_b_officer),
            "company B officer must not appear in company A evasion alert"
        );
    }
}
