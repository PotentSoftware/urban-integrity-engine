use anyhow::{bail, Context as _};
use serde::Serialize;

use crate::output::{
    build_entity_addresses_table, build_entity_alerts_table, build_entity_contracts_table,
    build_entity_directors_table, build_entity_disambiguation_table, build_entity_header_block,
    build_entity_lpa_presence_table, build_entity_osint_block, build_entity_planning_table,
    build_entity_related_table, print_json, OutputFormat,
};
use urban_integrity_core::{
    config::AppConfig, is_slug_shape, is_valid_ch_number, query_entity_by_ch_number,
    query_entity_by_id, query_entity_by_name, query_entity_lpa_presence, resolve_entity,
    EntityProfile, EntityProfileSummary, LpaPresence,
};

/// Entity profile extended with LPA presence data for JSON serialisation.
///
/// Returned by `--output json` so analysts can pipe the full cross-LPA picture
/// through `jq`. Uses `serde(flatten)` to keep the base profile fields at the
/// top level and append `lpaPresence` as a sibling key.
#[derive(Debug, Serialize)]
struct EntityProfileWithLpa<'a> {
    #[serde(flatten)]
    profile: &'a EntityProfile,
    lpa_presence: &'a [LpaPresence],
}

/// Arguments for the `entity` subcommand (flat clap::Args, no inner Subcommand).
///
/// Mutually exclusive: provide `query` (positional) or `--ch-number`, but not both.
#[derive(Debug, clap::Args)]
#[command(
    long_about = "Look up a company by name or Companies House number.\n\n\
                  EXAMPLES:\n\n  \
                  # Look up by name (table output, default)\n  \
                  urban-integrity entity \"Apex Build Ltd\"\n\n  \
                  # Look up by CH number\n  \
                  urban-integrity entity --ch-number 12345678\n\n  \
                  # Get JSON and filter with jq\n  \
                  urban-integrity entity \"Apex Build Ltd\" --output json | jq '.directors[].name'\n\n  \
                  # Extract incorporation date for age check\n  \
                  urban-integrity entity --ch-number 12345678 --output json | jq '{name, incorporationDate, contracts: [.contracts[] | {name, published}]}'\n\n  \
                  # Use a director slug seen on another company profile\n  \
                  urban-integrity entity apex-build-ltd\n\n  \
                  # Check related entities via piped jq\n  \
                  urban-integrity entity \"Apex Build Ltd\" --output json | jq '.relatedEntities[] | select(.sharedConnection.type == \"director\")'"
)]
pub struct EntityArgs {
    /// Company name (fuzzy match) or slug (exact ID lookup).
    /// Mutually exclusive with --ch-number.
    #[arg(conflicts_with = "ch_number")]
    pub query: Option<String>,

    /// Companies House registration number (exact match).
    #[arg(long = "ch-number", conflicts_with = "query")]
    pub ch_number: Option<String>,

    /// Output format
    #[arg(long, value_enum)]
    pub output: Option<OutputFormat>,

    /// Include sub-candidate OSINT links (0.50–0.69, investigative only) in output.
    /// Read-only — these links cannot be confirmed via `links` CLI (D-13).
    #[arg(long, default_value_t = false)]
    pub deep: bool,
}

pub async fn run(args: EntityArgs) -> anyhow::Result<()> {
    let fmt = args.output.unwrap_or_else(OutputFormat::auto); // SP-6
    let deep = args.deep; // SURF-02: propagate --deep flag through render chain

    match (args.query, args.ch_number) {
        // ── Both absent ──────────────────────────────────────────────────────────
        (None, None) => {
            bail!("Specify a company name, slug, or --ch-number");
        }

        // ── CH number lookup (exact match) ────────────────────────────────────
        (None, Some(ch)) | (Some(_), Some(ch)) => {
            // T-10-03-01: validate before any DB call
            if !is_valid_ch_number(&ch) {
                bail!("Invalid CH number format — expected up to 10 alphanumeric characters");
            }
            let config = AppConfig::load().context("Config load failed")?;
            let db = crate::db::connect_db(&config).await?;
            let result = query_entity_by_ch_number(&db, &ch)
                .await
                .context("SurrealDB entity lookup failed")?;
            match result {
                None => {
                    eprintln!("No entity found for CH number {ch}");
                }
                Some(p) => {
                    // Resolve entity_id via D-09 seam for LPA presence lookup.
                    let entity_id = resolve_entity(&db, &ch)
                        .await
                        .ok()
                        .and_then(|ids| ids.into_iter().next())
                        .unwrap_or_else(|| format!("GB-COH:{ch}"));
                    let presence = query_entity_lpa_presence(&db, &entity_id, &config)
                        .await
                        .unwrap_or_default();
                    render_single_profile(&p, fmt, &presence, deep)?;
                }
            }
        }

        // ── Name / slug query ─────────────────────────────────────────────────
        (Some(q), None) => {
            if q.trim().is_empty() {
                bail!("Specify a company name, slug, or --ch-number");
            }

            let config = AppConfig::load().context("Config load failed")?;
            let db = crate::db::connect_db(&config).await?;

            if is_slug_shape(&q) {
                // Try exact slug/ID lookup first (D-01 dual-mode)
                let exact = query_entity_by_id(&db, &q)
                    .await
                    .context("SurrealDB entity lookup failed")?;
                if let Some(p) = exact {
                    let presence = query_entity_lpa_presence(&db, &q, &config)
                        .await
                        .unwrap_or_default();
                    return render_single_profile(&p, fmt, &presence, deep);
                }
                // Slug returned 0 hits — fall back to name search per D-01
            }

            let results = query_entity_by_name(&db, &q)
                .await
                .context("SurrealDB entity search failed")?;

            // For single-match result, resolve entity_id for LPA presence lookup.
            let presence: Vec<LpaPresence> = if results.len() == 1 {
                let ids = resolve_entity(&db, &q).await.unwrap_or_default();
                let entity_id = ids.into_iter().next().unwrap_or_else(|| q.clone());
                query_entity_lpa_presence(&db, &entity_id, &config)
                    .await
                    .unwrap_or_default()
            } else {
                vec![]
            };
            render_results(results, fmt, &q, &presence, deep)?;
        }
    }

    Ok(())
}

fn render_single_profile(
    p: &EntityProfile,
    fmt: OutputFormat,
    lpa_presence: &[LpaPresence],
    deep: bool,
) -> anyhow::Result<()> {
    match fmt {
        OutputFormat::Json => {
            let payload = EntityProfileWithLpa {
                profile: p,
                lpa_presence,
            };
            print_json(&payload)?;
        }
        OutputFormat::Table => {
            print!("{}", build_entity_header_block(p));
            println!();
            // Always render section header even if empty (D-03, D-12)
            println!("Directors");
            println!("{}", build_entity_directors_table(&p.directors));
            println!();
            println!("Contracts");
            println!("{}", build_entity_contracts_table(&p.contracts));
            println!();
            // D-03: planning applications are their own section. Rendered unconditionally, like
            // every other section here (D-03/D-12), so an entity with no planning applications
            // shows an empty table rather than the section vanishing — the distinction between
            // "none" and "not looked for" is the one this screen exists to keep.
            println!("Planning Applications");
            println!("{}", build_entity_planning_table(&p.planning_applications));
            println!();
            // CRS-01: LPA presence table (Phase 13 — replaces the deferred stub)
            println!("LPA Presence");
            println!("{}", build_entity_lpa_presence_table(lpa_presence));
            println!();
            println!("Addresses");
            println!("{}", build_entity_addresses_table(&p.addresses));
            println!();
            println!("Active Alerts");
            println!("{}", build_entity_alerts_table(&p.alerts));
            println!();
            println!("Related Entities");
            println!("{}", build_entity_related_table(&p.related_entities));
            println!();
            // SURF-01 / D-12: OSINT Enrichment section — always rendered even if empty.
            // In Phase 17: Beneficial Owners + Disqualifications populated from live data;
            // remaining 4 subsections render "(none)" with headings intact for future phases.
            // SURF-02 (Phase 19): When --deep is passed, the sub-candidate section is also
            // rendered. D-13: sub_candidate links never shown without --deep.
            println!("OSINT Enrichment");
            print!("{}", build_entity_osint_block(p, deep));
        }
        OutputFormat::Markdown | OutputFormat::Pdf => {
            anyhow::bail!("--format md/pdf is only supported by the report command")
        }
    }
    Ok(())
}

fn render_results(
    results: Vec<EntityProfile>,
    fmt: OutputFormat,
    query: &str,
    lpa_presence: &[LpaPresence],
    deep: bool,
) -> anyhow::Result<()> {
    match results.len() {
        0 => eprintln!("No entities found matching '{query}'"),
        1 => {
            render_single_profile(&results[0], fmt, lpa_presence, deep)?;
        }
        _ => {
            // D-05: disambiguation list
            let summaries: Vec<EntityProfileSummary> = results
                .iter()
                .map(|p| EntityProfileSummary {
                    name: p.name.clone(),
                    ch_number: p.ch_number.clone(),
                    status: p.status.clone(),
                })
                .collect();
            match fmt {
                OutputFormat::Json => print_json(&summaries)?,
                OutputFormat::Table => {
                    println!("Multiple matches for '{query}' — use --ch-number for exact lookup:");
                    println!("{}", build_entity_disambiguation_table(&summaries));
                }
                OutputFormat::Markdown | OutputFormat::Pdf => {
                    anyhow::bail!("--format md/pdf is only supported by the report command")
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::EntityProfile;
    use urban_integrity_schema::types::node::EntityStatus;

    fn make_profile(name: &str, ch: Option<&str>) -> EntityProfile {
        EntityProfile {
            id: name.to_string(),
            name: name.to_owned(),
            ch_number: ch.map(str::to_owned),
            status: EntityStatus::Active,
            incorporation_date: None,
            addresses: vec![],
            directors: vec![],
            contracts: vec![],
            planning_applications: vec![],
            alerts: vec![],
            related_entities: vec![],
            lpa_presence: vec![],
            beneficial_owners: vec![],
            disqualifications: vec![],
            offshore_links: vec![],
            trustee_of: vec![],
            owns_property: vec![],
            donates_to: vec![],
            osint_links: vec![],
            sanctions_status: None,
            osint_alerts: vec![],
            sub_candidate_links: vec![],
            identity_provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            as_at: None,
        }
    }

    // ── is_valid_ch_number ────────────────────────────────────────────────

    #[test]
    fn test_is_valid_ch_number_accepts_valid() {
        assert!(is_valid_ch_number("12345678"));
        assert!(is_valid_ch_number("SC123456"));
        assert!(is_valid_ch_number("NI123456"));
        assert!(is_valid_ch_number("OC123456"));
        assert!(is_valid_ch_number("ab")); // minimum length = 2
        assert!(is_valid_ch_number("ABCDEFGHIJ")); // maximum length = 10
    }

    #[test]
    fn test_is_valid_ch_number_rejects_invalid() {
        assert!(!is_valid_ch_number("")); // empty
        assert!(!is_valid_ch_number("a")); // too short (len < 2)
        assert!(!is_valid_ch_number("12345678901")); // too long (len > 10)
        assert!(!is_valid_ch_number("abc; DROP")); // injection char
        assert!(!is_valid_ch_number("abc-def")); // hyphen not alphanumeric
        assert!(!is_valid_ch_number("'quoted'")); // quotes
    }

    // ── render_results dispatch ───────────────────────────────────────────────

    #[test]
    fn test_render_results_zero_matches_ok() {
        let result = render_results(vec![], OutputFormat::Json, "NonExistentCo", &[], false);
        assert!(
            result.is_ok(),
            "render_results with 0 matches must return Ok"
        );
    }

    #[test]
    fn test_render_results_single_match_json_ok() {
        let profile = make_profile("Apex Build Ltd", Some("12345678"));
        let result = render_results(vec![profile], OutputFormat::Json, "Apex", &[], false);
        assert!(
            result.is_ok(),
            "render_results with 1 match JSON must return Ok"
        );
    }

    #[test]
    fn test_render_results_multiple_matches_json_ok() {
        let profiles = vec![
            make_profile("Alpha Ltd", Some("11111111")),
            make_profile("Alpha Build Ltd", Some("22222222")),
        ];
        let result = render_results(profiles, OutputFormat::Json, "Alpha", &[], false);
        assert!(
            result.is_ok(),
            "render_results disambiguation JSON must return Ok"
        );
    }

    #[test]
    fn test_render_results_multiple_matches_markdown_errors() {
        let profiles = vec![
            make_profile("Alpha Ltd", Some("11111111")),
            make_profile("Alpha Build Ltd", Some("22222222")),
        ];
        let result = render_results(profiles, OutputFormat::Markdown, "Alpha", &[], false);
        assert!(
            result.is_err(),
            "render_results disambiguation with Markdown must Err"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("report command"),
            "Error must mention 'report command': {msg}"
        );
    }

    // ── render_single_profile error paths ────────────────────────────────────

    #[test]
    fn test_render_single_profile_markdown_errors() {
        let p = make_profile("Apex Build Ltd", Some("12345678"));
        let result = render_single_profile(&p, OutputFormat::Markdown, &[], false);
        assert!(
            result.is_err(),
            "Markdown format must return Err for entity profile"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("report command"),
            "Error must mention 'report command': {msg}"
        );
    }

    #[test]
    fn test_render_single_profile_pdf_errors() {
        let p = make_profile("Apex Build Ltd", Some("12345678"));
        let result = render_single_profile(&p, OutputFormat::Pdf, &[], false);
        assert!(
            result.is_err(),
            "Pdf format must return Err for entity profile"
        );
    }

    #[test]
    fn test_render_single_profile_json_ok() {
        let p = make_profile("Apex Build Ltd", Some("12345678"));
        let result = render_single_profile(&p, OutputFormat::Json, &[], false);
        assert!(
            result.is_ok(),
            "Json format must return Ok for entity profile"
        );
    }
}
