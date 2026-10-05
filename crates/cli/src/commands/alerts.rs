use crate::output::{build_alert_table, print_json, OutputFormat};
use serde::{Deserialize, Serialize};
use urban_integrity_detector::alert::result::MotifType;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AlertRow {
    pub id: String,
    pub motif_type: String,
    pub score: f64,
    pub ward: Option<String>,
    pub detected_at: String,
    #[serde(default)]
    pub entities: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct AlertsArgs {
    #[command(subcommand)]
    pub command: AlertsCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum AlertsCommand {
    /// List flagged motifs with optional filters
    List {
        /// Minimum P_c score (0.0–1.0)
        #[arg(long, default_value_t = 0.5)]
        min_score: f64,
        /// Filter by ward code (e.g. X05999001)
        #[arg(long)]
        ward: Option<String>,
        /// Filter by motif type. Short alias or canonical name: phoenix, cluster, nepotism,
        /// property, evasion, offshore, charity, donation
        #[arg(long)]
        motif: Option<String>,
        /// Start date (YYYY-MM-DD)
        #[arg(long)]
        from: Option<String>,
        /// End date (YYYY-MM-DD)
        #[arg(long)]
        to: Option<String>,
        /// Shorthand duration filter: Nh (hours) or Nd (days), e.g. 24h, 7d
        #[arg(long, conflicts_with_all = ["from", "to"])]
        since: Option<String>,
        /// Maximum number of results (0 = unlimited)
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Also show alerts an operator has withdrawn (hidden by default)
        #[arg(long)]
        include_withdrawn: bool,
        /// Output format
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
    },
    /// Show full detail for a specific alert
    Show {
        /// Alert ID
        id: String,
        /// Output format
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
    },
    /// Withdraw an alert that a data correction has invalidated.
    ///
    /// Marks rather than deletes. Detection upserts what it matches and never retracts what
    /// it stops matching, so an alert invalidated by a correction otherwise stays in the
    /// queue indefinitely, indistinguishable from a live finding. A withdrawn alert stays
    /// readable via `--include-withdrawn`, because a retracted false positive is evidence
    /// about detector quality and worth keeping.
    Withdraw {
        /// Alert ID (the `event_id`, as shown by `alerts list`)
        id: String,
        /// Why it is being withdrawn — recorded alongside the alert
        #[arg(long)]
        reason: String,
    },
}

fn parse_since(s: &str) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
    use anyhow::Context as _;
    let s = s.trim();
    if let Some(n) = s.strip_suffix('h') {
        let hours: i64 = n.parse().context("invalid hours in --since")?;
        if hours < 0 {
            anyhow::bail!("--since value must be positive (got {}h)", hours);
        }
        Ok(chrono::Utc::now() - chrono::Duration::hours(hours))
    } else if let Some(n) = s.strip_suffix('d') {
        let days: i64 = n.parse().context("invalid days in --since")?;
        if days < 0 {
            anyhow::bail!("--since value must be positive (got {}d)", days);
        }
        Ok(chrono::Utc::now() - chrono::Duration::days(days))
    } else {
        anyhow::bail!("--since format must be Nh or Nd (e.g. 24h, 7d)")
    }
}

/// One typing shortcut per motif type. The canonical name each maps to is NOT written here —
/// [`normalize_motif_type`] resolves the alias against [`MotifType::ALL`], so an alias can only
/// ever name a motif type that actually exists.
const MOTIF_ALIASES: [(&str, MotifType); 8] = [
    ("phoenix", MotifType::PhoenixLoop),
    ("cluster", MotifType::AddressCluster),
    ("nepotism", MotifType::NepotismBridge),
    ("property", MotifType::PropertyAccumulationAnomaly),
    ("evasion", MotifType::DisqualifiedDirectorEvasion),
    ("offshore", MotifType::OffshoreShadow),
    ("charity", MotifType::CharityCommercialCrossover),
    ("donation", MotifType::DonationContractCorrelation),
];

/// Normalise the CLI's user-facing `--motif` value to the canonical `MotifType` wire string
/// stored in `result_json`. Accepts a short alias (`phoenix`, `evasion`, …) or the canonical
/// name itself in any case (`AddressCluster`, `addresscluster`,
/// `disqualified_director_evasion`).
///
/// Without this mapping, `query_alerts`'s `eq_ignore_ascii_case` filter compares e.g. `"cluster"`
/// against `"AddressCluster"` and never matches — `--motif cluster`/`phoenix`/`nepotism` silently
/// returned zero rows regardless of how many matching alerts existed (found 2026-08-06 while
/// capturing the 29-02 LFA-06 baseline: `alerts list --motif cluster` returned `[]` against a
/// database with 1,430 live `AddressCluster` alerts).
///
/// # Why the canonical half is derived, not listed
///
/// This function previously hand-wrote all three mappings. When 46-03 widened `MotifType` from 3
/// variants to 8, the five new ones silently had no alias and no canonical-name handling — the
/// exact drift phase 46 code review finding IN-01 records. Resolving against [`MotifType::ALL`] means a ninth
/// detector is reachable by its canonical name the moment it is declared, and
/// `every_motif_type_has_an_alias` fails if someone adds a variant without giving it a shortcut.
fn normalize_motif_type(input: &str) -> String {
    let lowered = input.to_ascii_lowercase();
    if let Some((_, motif)) = MOTIF_ALIASES.iter().find(|(alias, _)| *alias == lowered) {
        return motif.wire_str().to_string();
    }
    // The canonical name itself, in any case — including the snake_case wire names the
    // post-adapter detectors use, which an operator will copy straight out of a report.
    if let Some(motif) = MotifType::ALL
        .iter()
        .find(|m| m.wire_str().eq_ignore_ascii_case(&lowered))
    {
        return motif.wire_str().to_string();
    }
    // Unknown input: pass through unchanged so an operator's typo still produces the
    // pre-existing (no-match) behaviour rather than silently coercing to a motif they
    // didn't ask for.
    input.to_string()
}

fn apply_slug_substitution(
    text: &str,
    name_map: &std::collections::HashMap<String, String>,
) -> String {
    let mut pairs: Vec<(&String, &String)> = name_map.iter().collect();
    // Longest keys first prevents prefix collisions (e.g. "director:john" matching inside "director:john-smith")
    pairs.sort_by_key(|b| std::cmp::Reverse(b.0.len()));
    let mut result = text.to_owned();
    for (slug, name) in pairs {
        result = result.replace(slug.as_str(), name.as_str());
    }
    result
}

pub async fn run(args: AlertsArgs) -> anyhow::Result<()> {
    use crate::db::connect_db;
    use anyhow::Context as _;
    use urban_integrity_core::{
        config::AppConfig, enrich_alert_participants, get_alert_by_id, query_alerts, AlertFilter,
    };

    match args.command {
        AlertsCommand::List {
            min_score,
            ward,
            motif,
            from,
            to,
            since,
            limit,
            include_withdrawn,
            output,
        } => {
            let fmt = output.unwrap_or_else(OutputFormat::auto);
            let config = AppConfig::load().context("Config load failed")?;
            let db = connect_db(&config).await?;

            // --since sets from_date (already validated to not conflict with --from/--to by clap)
            let computed_from_date = if let Some(ref s) = since {
                Some(parse_since(s)?.to_rfc3339())
            } else {
                from
            };

            if limit == 0 {
                eprintln!(
                    "Warning: no limit set — fetching all matching alerts, this may be slow on large graphs."
                );
            }

            let filter = AlertFilter {
                min_score,
                ward_code: ward,
                motif_type: motif.as_deref().map(normalize_motif_type),
                from_date: computed_from_date,
                to_date: to,
                limit,
                include_withdrawn,
                ..Default::default()
            };

            let page = query_alerts(&db, &filter)
                .await
                .context("SurrealDB query_alerts failed")?;

            // Truncation must be stated, not inferred. `alerts list --limit 2000` used to return
            // exactly 1,000 rows with no signal at all, because the limit was silently clamped —
            // an analyst had no way to know anything had been withheld.
            if page.truncated() {
                eprintln!(
                    "Note: showing {} of {} matching alerts, highest score first — \
                     use --limit N for more, or --limit 0 for all.",
                    page.alerts.len(),
                    page.total_matched
                );
            }
            let records = page.alerts;

            // Collect unique participant slugs across all records for batch enrichment
            let all_slugs: Vec<String> = records
                .iter()
                .flat_map(|r| {
                    serde_json::from_str::<serde_json::Value>(&r.result_json)
                        .ok()
                        .and_then(|v| {
                            serde_json::from_value::<Vec<String>>(v["participants"].clone()).ok()
                        })
                        .unwrap_or_default()
                })
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            let name_map = enrich_alert_participants(&db, &all_slugs)
                .await
                .unwrap_or_default();

            let alerts: Vec<AlertRow> = records
                .into_iter()
                .map(|r| {
                    let participants: Vec<String> =
                        serde_json::from_str::<serde_json::Value>(&r.result_json)
                            .ok()
                            .and_then(|v| {
                                serde_json::from_value::<Vec<String>>(v["participants"].clone())
                                    .ok()
                            })
                            .unwrap_or_default();

                    // D-07 entity column format: first 2 names + "+ N more"
                    let entities: Option<String> = {
                        let names: Vec<&str> = participants
                            .iter()
                            .map(|s| name_map.get(s).map(|n| n.as_str()).unwrap_or(s.as_str()))
                            .collect();
                        if names.is_empty() {
                            None
                        } else if names.len() <= 2 {
                            Some(names.join(", "))
                        } else {
                            Some(format!(
                                "{}, {} + {} more",
                                names[0],
                                names[1],
                                names.len() - 2
                            ))
                        }
                    };

                    AlertRow {
                        id: r.id,
                        motif_type: r.motif_type,
                        score: r.score,
                        ward: r.ward_code,
                        detected_at: r.detected_at,
                        entities,
                    }
                })
                .collect();

            match fmt {
                OutputFormat::Json => print_json(&alerts)?,
                OutputFormat::Table => {
                    let table = build_alert_table(&alerts);
                    println!("{table}");
                }
                OutputFormat::Markdown | OutputFormat::Pdf => {
                    anyhow::bail!("--format md/pdf is only supported by the report command")
                }
            }
        }
        AlertsCommand::Show { id, output } => {
            let fmt = output.unwrap_or_else(OutputFormat::auto);
            let config = AppConfig::load().context("Config load failed")?;
            let db = connect_db(&config).await?;

            let record = get_alert_by_id(&db, &id)
                .await
                .context("SurrealDB get_alert_by_id failed")?;

            match record {
                None => {
                    eprintln!("Alert {} not found", id);
                    std::process::exit(1);
                }
                Some(r) => {
                    // Extract participants and enrich slugs
                    let participants: Vec<String> =
                        serde_json::from_str::<serde_json::Value>(&r.result_json)
                            .ok()
                            .and_then(|v| {
                                serde_json::from_value::<Vec<String>>(v["participants"].clone())
                                    .ok()
                            })
                            .unwrap_or_default();

                    let name_map = enrich_alert_participants(&db, &participants)
                        .await
                        .unwrap_or_default();

                    // Extract evidence_snapshot and score_breakdown from result_json
                    let result_val: serde_json::Value =
                        serde_json::from_str(&r.result_json).unwrap_or(serde_json::Value::Null);
                    let evidence_snapshot = result_val["evidence_snapshot"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    let description = apply_slug_substitution(&evidence_snapshot, &name_map);

                    let alert = AlertRow {
                        id: r.id,
                        motif_type: r.motif_type,
                        score: r.score,
                        ward: r.ward_code,
                        detected_at: r.detected_at,
                        entities: None,
                    };

                    match fmt {
                        OutputFormat::Json => {
                            // D-09: JSON output includes description_resolved alongside alert fields
                            let json_out = serde_json::json!({
                                "id": alert.id,
                                "motif_type": alert.motif_type,
                                "score": alert.score,
                                "ward": alert.ward,
                                "detected_at": alert.detected_at,
                                "result_json": r.result_json,
                                "description_resolved": description,
                            });
                            print_json(&json_out)?;
                        }
                        OutputFormat::Table => {
                            // D-08: four-block rendering
                            // Block 1 — Header
                            println!("ID:       {}", alert.id);
                            println!("Motif:    {}", alert.motif_type);
                            println!("Score:    {:.3}", alert.score);
                            println!("Ward:     {}", alert.ward.as_deref().unwrap_or("\u{2014}"));
                            println!("Detected: {}", alert.detected_at);

                            // Block 2 — Entities
                            println!("\nEntities:");
                            let resolved_names: Vec<&str> = participants
                                .iter()
                                .map(|s| name_map.get(s).map(|n| n.as_str()).unwrap_or(s.as_str()))
                                .collect();
                            for name in &resolved_names {
                                println!("  - {name}");
                            }

                            // Block 3 — Description
                            println!("\nDescription:\n  {description}");

                            // Block 4 — Score Breakdown
                            println!("\nScore Breakdown:");
                            let sb = &result_val["score_breakdown"];
                            println!(
                                "  s_phoenix:    {:.3}",
                                sb["s_phoenix"].as_f64().unwrap_or(0.0)
                            );
                            println!(
                                "  s_proximity:  {:.3}",
                                sb["s_proximity"].as_f64().unwrap_or(0.0)
                            );
                            println!(
                                "  s_clustering: {:.3}",
                                sb["s_clustering"].as_f64().unwrap_or(0.0)
                            );
                            println!(
                                "  p_base:       {:.3}",
                                sb["p_base"].as_f64().unwrap_or(0.0)
                            );
                            println!("  m_env:        {:.3}", sb["m_env"].as_f64().unwrap_or(0.0));
                            println!(
                                "  p_final:      {:.3}",
                                sb["p_final"].as_f64().unwrap_or(0.0)
                            );
                            let imd = match &sb["imd_rank"] {
                                serde_json::Value::Null => "\u{2014}".to_owned(),
                                v => v.to_string(),
                            };
                            println!("  imd_rank:     {imd}");
                            println!(
                                "  ward_code:    {}",
                                sb["ward_code"].as_str().unwrap_or("\u{2014}")
                            );
                        }
                        OutputFormat::Markdown | OutputFormat::Pdf => {
                            anyhow::bail!("--format md/pdf is only supported by the report command")
                        }
                    }
                }
            }
        }

        AlertsCommand::Withdraw { id, reason } => {
            let config = AppConfig::load().context("Config load failed")?;
            let db = connect_db(&config).await?;

            // The already-withdrawn guard now lives inside `withdraw_alert` itself (46-02,
            // D-08/D-09), so both the CLI and the future `/api/alert/:id/withdraw` handler
            // enforce the identical guard — a caller cannot bypass it by skipping a separate
            // pre-check, which is exactly how the old two-step pattern here could have
            // diverged from a second caller.
            let outcome =
                urban_integrity_core::withdraw_alert(&db, &id, &reason, chrono::Utc::now())
                    .await
                    .context("SurrealDB withdraw_alert failed")?;
            match outcome {
                urban_integrity_core::WithdrawOutcome::NotFound => {
                    eprintln!("Alert {} not found", id);
                    std::process::exit(1);
                }
                urban_integrity_core::WithdrawOutcome::AlreadyWithdrawn { at, reason } => {
                    // Re-withdrawing would silently overwrite the original reason and date,
                    // losing the very audit trail that marking rather than deleting exists
                    // to keep.
                    eprintln!("Alert {} was already withdrawn at {} ({})", id, at, reason);
                    std::process::exit(1);
                }
                urban_integrity_core::WithdrawOutcome::Withdrawn => {
                    println!("Alert {id} withdrawn: {reason}");
                    println!(
                        "It is hidden from `alerts list`, reports and the API; \
                              `alerts list --include-withdrawn` still shows it."
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IN-01 (phase 46 code review): the drift guard. 46-03 widened `MotifType` 3 -> 8 and this table
    /// was not widened with it, so five detectors had no `--motif` shortcut at all. A ninth
    /// variant added without an alias fails here rather than shipping half-reachable.
    ///
    /// PROVE THIS CAN FAIL: delete the `("donation", ...)` row and this test fails naming
    /// `donation_contract_correlation`.
    #[test]
    fn every_motif_type_has_an_alias() {
        for motif in MotifType::ALL {
            assert!(
                MOTIF_ALIASES.iter().any(|(_, m)| *m == motif),
                "{} has no --motif short alias — add one to MOTIF_ALIASES",
                motif.wire_str()
            );
        }
        assert_eq!(
            MOTIF_ALIASES.len(),
            MotifType::ALL.len(),
            "no alias may name a motif type twice, and none may be left over"
        );
    }

    /// The five post-adapter detectors 46-03 added were previously unreachable by ANY spelling:
    /// no alias, and no canonical-name handling either, so `--motif offshore_shadow` fell through
    /// the old `other => other.to_string()` arm unchanged. That happened to work only because the
    /// wire name is already canonical — the alias is the part that was missing.
    #[test]
    fn the_five_post_adapter_detectors_are_reachable_by_alias() {
        assert_eq!(
            normalize_motif_type("evasion"),
            "disqualified_director_evasion"
        );
        assert_eq!(
            normalize_motif_type("property"),
            "property_accumulation_anomaly"
        );
        assert_eq!(normalize_motif_type("offshore"), "offshore_shadow");
        assert_eq!(
            normalize_motif_type("charity"),
            "charity_commercial_crossover"
        );
        assert_eq!(
            normalize_motif_type("donation"),
            "donation_contract_correlation"
        );
    }

    #[test]
    fn the_three_original_aliases_still_resolve() {
        assert_eq!(normalize_motif_type("phoenix"), "PhoenixLoop");
        assert_eq!(normalize_motif_type("cluster"), "AddressCluster");
        assert_eq!(normalize_motif_type("nepotism"), "NepotismBridge");
    }

    /// A canonical name typed in any case resolves to the exact stored spelling — an operator
    /// copying `disqualified_director_evasion` out of a report must not have to know it is
    /// already canonical, and `AddressCluster` must survive being lowercased.
    #[test]
    fn a_canonical_name_resolves_to_its_exact_stored_spelling() {
        assert_eq!(normalize_motif_type("AddressCluster"), "AddressCluster");
        assert_eq!(normalize_motif_type("addresscluster"), "AddressCluster");
        assert_eq!(
            normalize_motif_type("DISQUALIFIED_DIRECTOR_EVASION"),
            "disqualified_director_evasion"
        );
    }

    /// Unknown input passes through unchanged rather than being coerced to some nearest motif —
    /// a typo must produce no matches, never the wrong matches.
    #[test]
    fn an_unknown_motif_is_passed_through_not_guessed() {
        assert_eq!(normalize_motif_type("clustr"), "clustr");
        assert_eq!(normalize_motif_type(""), "");
    }

    #[test]
    fn test_parse_since_hours() {
        let dt = parse_since("24h").unwrap();
        let expected = chrono::Utc::now() - chrono::Duration::hours(24);
        assert!(
            (dt - expected).num_seconds().abs() < 5,
            "parse_since('24h') should be within 5s of Utc::now() - 24h"
        );
    }

    #[test]
    fn test_parse_since_days() {
        let dt = parse_since("7d").unwrap();
        let expected = chrono::Utc::now() - chrono::Duration::days(7);
        assert!(
            (dt - expected).num_seconds().abs() < 5,
            "parse_since('7d') should be within 5s of Utc::now() - 7d"
        );
    }

    #[test]
    fn test_parse_since_invalid() {
        assert!(
            parse_since("invalid").is_err(),
            "parse_since('invalid') must return Err"
        );
        assert!(parse_since("").is_err(), "parse_since('') must return Err");
        assert!(
            parse_since("5m").is_err(),
            "parse_since('5m') must return Err"
        );
        assert!(
            parse_since("24H").is_err(),
            "parse_since('24H') must return Err (case-sensitive)"
        );
    }

    #[test]
    fn test_normalize_motif_type_short_names_map_to_internal_variants() {
        assert_eq!(normalize_motif_type("cluster"), "AddressCluster");
        assert_eq!(normalize_motif_type("Cluster"), "AddressCluster");
        assert_eq!(normalize_motif_type("phoenix"), "PhoenixLoop");
        assert_eq!(normalize_motif_type("nepotism"), "NepotismBridge");
    }

    #[test]
    fn test_normalize_motif_type_full_variant_names_pass_through_matching() {
        // Also accept the internal variant name directly (case-insensitive), so a caller who
        // already knows the internal name (e.g. scripting against result_json) isn't broken.
        assert_eq!(normalize_motif_type("AddressCluster"), "AddressCluster");
        assert_eq!(normalize_motif_type("addresscluster"), "AddressCluster");
    }

    #[test]
    fn test_normalize_motif_type_unknown_input_passes_through_unchanged() {
        // An operator typo must not silently coerce to some other motif's alerts.
        assert_eq!(normalize_motif_type("clustre"), "clustre");
    }

    #[test]
    fn test_alerts_since_conflicts_with_from() {
        use clap::Parser;

        // Wrap AlertsCommand in a thin Parser to test argument conflicts.
        #[derive(Parser)]
        struct TestCli {
            #[command(subcommand)]
            command: AlertsCommand,
        }

        let result =
            TestCli::try_parse_from(["test-cli", "list", "--since", "24h", "--from", "2026-01-01"]);
        assert!(
            result.is_err(),
            "--since and --from should conflict; expected clap error but got success"
        );
    }

    #[test]
    fn test_slug_substitution_replaces_known_slugs() {
        let mut map = std::collections::HashMap::new();
        map.insert("director:john-smith".to_owned(), "John Smith".to_owned());
        map.insert("GB-COH:87654321".to_owned(), "Beta Build Ltd".to_owned());
        let text = "officer director:john-smith with failed entity GB-COH:87654321";
        let result = apply_slug_substitution(text, &map);
        assert_eq!(
            result,
            "officer John Smith with failed entity Beta Build Ltd"
        );
    }

    #[test]
    fn test_slug_substitution_passthrough_on_no_match() {
        let map = std::collections::HashMap::new();
        let text = "no slugs here";
        assert_eq!(apply_slug_substitution(text, &map), text);
    }

    // ── WR-01: negative --since guard ─────────────────────────────────────────

    #[test]
    fn test_parse_since_negative_hours_is_err() {
        let err = parse_since("-5h");
        assert!(
            err.is_err(),
            "parse_since('-5h') must return Err, got Ok({:?})",
            err.ok()
        );
        assert!(
            err.unwrap_err().to_string().contains("positive"),
            "error message should mention 'positive'"
        );
    }

    #[test]
    fn test_parse_since_negative_days_is_err() {
        let err = parse_since("-3d");
        assert!(err.is_err(), "parse_since('-3d') must return Err");
        assert!(err.unwrap_err().to_string().contains("positive"));
    }

    #[test]
    fn test_parse_since_zero_hours_is_ok() {
        // 0h is technically non-negative — should be accepted (returns Utc::now())
        assert!(parse_since("0h").is_ok(), "parse_since('0h') must be Ok");
    }

    // ── CR-03: apply_slug_substitution prefix-collision fix ───────────────────

    #[test]
    fn test_slug_substitution_longest_key_wins_on_prefix_overlap() {
        let mut map = std::collections::HashMap::new();
        // Shorter prefix that would corrupt the longer key if applied first
        map.insert("director:john".to_owned(), "John Doe".to_owned());
        map.insert("director:john-smith".to_owned(), "John Smith".to_owned());
        let text = "director:john-smith and director:john filed applications";
        let result = apply_slug_substitution(text, &map);
        assert_eq!(
            result, "John Smith and John Doe filed applications",
            "Longer slug must be substituted before its prefix: got '{result}'"
        );
    }

    #[test]
    fn test_slug_substitution_ch_number_prefix_overlap() {
        let mut map = std::collections::HashMap::new();
        map.insert("GB-COH:1234567".to_owned(), "Short Co".to_owned());
        map.insert("GB-COH:12345678".to_owned(), "Long Co".to_owned());
        let text = "GB-COH:12345678 and GB-COH:1234567 are related";
        let result = apply_slug_substitution(text, &map);
        assert_eq!(
            result, "Long Co and Short Co are related",
            "Longer CH number slug must replace before shorter prefix: got '{result}'"
        );
    }
}
