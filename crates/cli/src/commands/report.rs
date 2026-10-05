use crate::commands::alerts::AlertRow;
use crate::output::{build_alert_table, print_json, OutputFormat};
use anyhow::Context as _;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, clap::Args)]
pub struct ReportArgs {
    /// Ward code to focus report on (e.g. X05999001)
    #[arg(long)]
    pub ward: Option<String>,
    /// Minimum P_c score to include (0.0–1.0)
    #[arg(long, default_value_t = 0.5)]
    pub min_score: f64,
    /// Motif type filter: phoenix, cluster, nepotism
    #[arg(long)]
    pub motif: Option<String>,
    /// Start date (YYYY-MM-DD)
    #[arg(long)]
    pub from: Option<String>,
    /// End date (YYYY-MM-DD)
    #[arg(long)]
    pub to: Option<String>,
    /// Output format: table, json, md, pdf  (--output is a hidden alias for REP-06)
    #[arg(long, alias = "output", value_enum)]
    pub format: Option<OutputFormat>,
    /// Write report to file (only valid with --format md or --format pdf)
    #[arg(long)]
    pub output_file: Option<PathBuf>,
    /// Generate entity-scoped report across all wards/LPAs
    #[arg(long)]
    pub entity: Option<String>,
    /// Cross-LPA correlated scope for --entity reports (e.g. all-lpa)
    #[arg(long)]
    pub scope: Option<String>,
    /// Prepend LLM narrative summary via Ollama (graceful WARN if unavailable)
    #[arg(long)]
    pub summarize: bool,
    /// Maximum alerts to include (0 = all, default 100)
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct ReportFilters {
    pub ward: Option<String>,
    pub min_score: f64,
    pub motif: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// `IntegrityReport` extended with a cross-LPA presence summary.
///
/// Only emitted when `--scope all-lpa` is set; the base `IntegrityReport` JSON
/// (no scope) is byte-for-byte unchanged (REP-06 non-breaking constraint).
#[derive(Debug, Serialize)]
struct IntegrityReportWithLpa<'a> {
    #[serde(flatten)]
    report: &'a IntegrityReport,
    lpa_presence: Vec<urban_integrity_core::LpaPresence>,
}

#[derive(Debug, Serialize)]
pub struct IntegrityReport {
    pub generated_at: String,
    pub filters_applied: ReportFilters,
    pub total_alerts: usize,
    pub high_risk_count: usize,
    pub alerts: Vec<crate::commands::alerts::AlertRow>,
}

/// Generate a default report filename from the scope (ward or entity) and file extension.
///
/// Slug computation: prefer entity over ward over literal `"all"`.
/// Lowercase, ASCII whitespace → hyphen, retain only `a-z`, `0-9`, `-`, drop others.
/// Result: `integrity-report-<slug>-YYYY-MM-DD.<ext>`
pub(crate) fn default_report_filename(
    ward: Option<&str>,
    entity: Option<&str>,
    ext: &str,
) -> String {
    let scope_raw = entity.or(ward).unwrap_or("all");
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let slug: String = scope_raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    format!("integrity-report-{}-{}.{}", slug, date, ext)
}

/// Validate that `--output-file` is only used with `--format md` or `--format pdf`.
///
/// Returns `Err` if `output_file` is `Some` and `fmt` is not `Markdown` or `Pdf`.
pub(crate) fn validate_output_file_format(
    fmt: &OutputFormat,
    output_file: &Option<PathBuf>,
) -> anyhow::Result<()> {
    if output_file.is_some() && !matches!(fmt, OutputFormat::Markdown | OutputFormat::Pdf) {
        anyhow::bail!("--output-file is only valid with --format md or --format pdf");
    }
    Ok(())
}

// ── Markdown rendering constants ─────────────────────────────────────────────

const CH_BASE_URL: &str = "https://find-and-update.company-information.service.gov.uk/company";

// ── Markdown rendering helpers ───────────────────────────────────────────────

/// Render a GFM pipe-syntax alert table from a slice of `AlertRow`.
///
/// Header: `| ID | Motif | Score | Ward | Detected | Entities |`
/// Separator: `|---|---|---|---|---|---|`
/// Each data row: score formatted to 3 decimals; None fields use em-dash U+2014.
fn render_alert_table_md(alerts: &[AlertRow]) -> String {
    let mut out = String::new();
    out.push_str("| ID | Motif | Score | Ward | Detected | Entities |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for a in alerts {
        let ward = a.ward.as_deref().unwrap_or("\u{2014}");
        let entities = a.entities.as_deref().unwrap_or("\u{2014}");
        out.push_str(&format!(
            "| {} | {} | {:.3} | {} | {} | {} |\n",
            a.id, a.motif_type, a.score, ward, a.detected_at, entities
        ));
    }
    out
}

/// Render a GFM level-3 heading + table snapshot for a single `EntityProfile`.
///
/// Per D-07: compact form — CH Number (hyperlink if Some), Status, Incorporated,
/// Highest Alert (motif type + score). No directors/contracts/addresses tables.
fn render_entity_snapshot_md(p: &urban_integrity_core::EntityProfile) -> String {
    let em = "\u{2014}";

    // CH Number cell — hyperlink when Some
    let ch_cell = match &p.ch_number {
        Some(ch) => format!("[{}]({}/{})", ch, CH_BASE_URL, ch),
        None => em.to_owned(),
    };

    // Status — render as human-readable string, not Rust debug format (WR-07).
    // Mirrors the private format_entity_status helper in crate::output.
    use urban_integrity_schema::types::node::EntityStatus;
    let status_cell = match &p.status {
        EntityStatus::Active => "Active".to_owned(),
        EntityStatus::Dissolved(dt) => format!("Dissolved {}", dt.format("%Y-%m-%d")),
        EntityStatus::InLiquidation(dt) => format!("In Liquidation {}", dt.format("%Y-%m-%d")),
    };

    // Incorporation date
    let inc_cell = p
        .incorporation_date
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| em.to_owned());

    // Highest alert: sort alerts by score descending, take first
    let highest_alert_cell = {
        let mut alerts: Vec<&urban_integrity_core::AlertRecord> = p.alerts.iter().collect();
        alerts.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        match alerts.first() {
            Some(a) => format!("{} \u{2014} {:.3}", a.motif_type, a.score),
            None => em.to_owned(),
        }
    };

    format!(
        "### {}\n\n| Field | Value |\n|---|---|\n| CH Number | {} |\n| Status | {} |\n| Incorporated | {} |\n| Highest Alert | {} |\n\n",
        p.name, ch_cell, status_cell, inc_cell, highest_alert_cell
    )
}

/// Render the `## Source Data` citations section.
///
/// For each profile with a `ch_number`: emit a bullet with a Companies House hyperlink.
/// For each contract on that profile:
///   - If `tender_ref` is Some: emit sub-bullet `  - Contract ref: <tender_ref>`
///   - If `application_ref` is Some: emit sub-bullet `  - Planning application: <application_ref>`
///   - If both are None and `name` is non-empty: emit `  - Contract: <name>` (D-04 fallback)
///
/// For each planning application on that profile:
///   - If `application_ref` is Some: emit sub-bullet `  - Planning application: <application_ref>`
///   - Otherwise, if `name` is non-empty: emit `  - Planning application: <name>`
///
/// The two loops emit the **same** `Planning application: …` line, and that is deliberate: D-03
/// moved planning applications out of `EntityProfile::contracts` into their own section, and the
/// citation list must not lose a source because of where a row now lives. The `application_ref`
/// branch in the contracts loop is therefore kept rather than deleted — after the split it is
/// unreachable on well-formed data, so if it ever fires it means a row was mis-routed, and a
/// citation is a better place to notice that than a silent omission.
///
/// # What the D-03 split did to this report's figures
///
/// Measured against `an_anglesey_aebaa785` on 2026-09-15. Baseline is `crates/` at `d2c995ff`,
/// which is byte-identical to the wave-1 merge `1d0ba8cc` — **not** any figure quoted before it,
/// because plan 45-02's de-duplication had already moved the contracts count by 2.372× and
/// attributing that to this split would be wrong.
///
/// `collect_top_entities` profiles at most five entities. On a default run (`--limit 100`,
/// `min_score 0.500`) those five held **12 contract rows before the split and 0 after**; all
/// twelve are planning applications and now arrive from `planning_applications`. The rendered
/// `## Source Data` section is unchanged — the same twelve `Planning application: …` lines, in the
/// same order.
///
/// Corpus-wide, over all 25,314 entities holding an `AwardedTo` edge, the figure this function
/// draws on goes from **46,553 contract rows to 6,687**, with **39,866** moving to planning
/// applications (85.6% of the rows).
///
/// **Order caveat, since nothing on today's corpus exercises it.** For a subject holding both
/// kinds the *set* of citation lines is preserved but their order is not: before the split the
/// two interleaved in destination-id order, and now every contract precedes every planning
/// application. No entity in this store holds both — every `planning:` destination is reached from
/// a `director:` applicant slug and every `CF:` one from a `GB-COH:`/`CF-PARTY:` supplier, 0 of
/// 25,314 sources overlap — so the reordering is unobservable here. It becomes observable the
/// moment identity resolution joins an applicant slug to a company, which is why it is stated
/// rather than assumed away, and why `render_citations_md_preserves_every_citation_across_the_split`
/// asserts set-equality rather than string-equality.
pub fn render_citations_md(profiles: &[urban_integrity_core::EntityProfile]) -> String {
    let mut out = String::from("## Source Data\n\n");
    for p in profiles {
        if let Some(ch) = &p.ch_number {
            out.push_str(&format!(
                "- **{}**: [Companies House]({}/{})\n",
                p.name, CH_BASE_URL, ch
            ));
        }
        for c in &p.contracts {
            if let Some(tref) = &c.tender_ref {
                out.push_str(&format!("  - Contract ref: {}\n", tref));
            }
            if let Some(aref) = &c.application_ref {
                out.push_str(&format!("  - Planning application: {}\n", aref));
            }
            if c.tender_ref.is_none() && c.application_ref.is_none() && !c.name.is_empty() {
                out.push_str(&format!("  - Contract: {}\n", c.name));
            }
        }
        for pa in &p.planning_applications {
            if let Some(aref) = &pa.application_ref {
                out.push_str(&format!("  - Planning application: {}\n", aref));
            } else if !pa.name.is_empty() {
                out.push_str(&format!("  - Planning application: {}\n", pa.name));
            }
        }
    }
    out
}

/// Invoke pandoc (primary) or wkhtmltopdf (fallback) to produce a PDF from a Markdown file.
///
/// Strategy (per D-10, D-11, the research notes' Pitfall 2):
/// 1. If pandoc is on PATH, attempt direct PDF: `pandoc <md> -o <pdf>`.
///    On success, return Ok(()).
/// 2. If pandoc direct PDF failed (non-zero exit — e.g. pdflatex missing), check whether
///    wkhtmltopdf is on PATH. If so, run a two-step fallback:
///    a. `pandoc <md> -t html5 -o <tmp.html>`
///    b. `wkhtmltopdf <tmp.html> <pdf>`
///    On success, return Ok(()).
/// 3. If pandoc is absent, try wkhtmltopdf directly (may fail on Markdown input — that is fine).
/// 4. If all paths fail, return Err with the D-11 install-instructions message.
///
/// CRITICAL (T-12-01 mitigation): Every `Command::new(...)` uses chained `.arg()` calls
/// with `&Path` / `&str` arguments — NO shell expansion, NO `sh -c`, NO format-string
/// interpolation into a single arg. Path args pass through OsStr so shell metacharacters
/// in filenames cannot escape into a shell.
///
/// The `.md` file at `md_path` is NEVER deleted by this function — per D-11 the analyst
/// keeps the Markdown even when PDF generation fails.
pub fn invoke_pdf_tool(md_path: &Path, pdf_path: &Path) -> anyhow::Result<()> {
    // Step 1: Check pandoc availability
    let pandoc_available = std::process::Command::new("pandoc")
        .arg("--version")
        .output()
        .is_ok();

    if pandoc_available {
        // Attempt direct PDF via pandoc
        let status = std::process::Command::new("pandoc")
            .arg(md_path)
            .arg("-o")
            .arg(pdf_path)
            .status();

        match status {
            Ok(s) if s.success() => return Ok(()),
            _ => {
                // pandoc present but direct PDF failed (e.g. pdflatex missing) — try HTML fallback
                // WR-03: unique path per invocation to prevent concurrent-run races.
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                let html_path = std::env::temp_dir().join(format!(
                    "urban-integrity-report-{}-{}.html",
                    std::process::id(),
                    ts
                ));

                // Check wkhtmltopdf availability
                let wkhtml_available = std::process::Command::new("wkhtmltopdf")
                    .arg("--version")
                    .output()
                    .is_ok();

                if wkhtml_available {
                    // Two-step: pandoc → html5, then wkhtmltopdf → pdf
                    let html_status = std::process::Command::new("pandoc")
                        .arg(md_path)
                        .arg("-t")
                        .arg("html5")
                        .arg("-o")
                        .arg(&html_path)
                        .status();

                    if let Ok(hs) = html_status {
                        if hs.success() {
                            let pdf_status = std::process::Command::new("wkhtmltopdf")
                                .arg(&html_path)
                                .arg(pdf_path)
                                .status();
                            // Clean up the intermediate HTML file regardless of outcome.
                            let _ = std::fs::remove_file(&html_path);
                            if let Ok(ps) = pdf_status {
                                if ps.success() {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        // pandoc absent — try wkhtmltopdf directly as a last resort
        let wkhtml_available = std::process::Command::new("wkhtmltopdf")
            .arg("--version")
            .output()
            .is_ok();

        if wkhtml_available {
            let status = std::process::Command::new("wkhtmltopdf")
                .arg(md_path)
                .arg(pdf_path)
                .status();
            if let Ok(s) = status {
                if s.success() {
                    return Ok(());
                }
            }
        }
    }

    // All paths failed — D-11: emit install instructions; md_path remains on disk
    anyhow::bail!(
        "PDF generation requires pandoc. Install: sudo apt install pandoc (Debian/Ubuntu) or brew install pandoc (macOS). Markdown report saved to {}",
        md_path.display()
    )
}

/// Render a complete six-section Markdown report.
///
/// Section order (REP-01 / D-07 / D-08):
///   1. Header (`# UrbanIntegrity Report` + metadata)
///   2. Summary Narrative (only when `summary_narrative` is Some)
///   3. `## Summary` — total alerts, high-risk count
///   4. `## Alerts` — GFM pipe table
///   5. `## Top Entities` — up to 5 entity snapshots
///   6. `## Source Data` — citations (CH hyperlinks, tender_ref, application_ref)
pub fn render_markdown_report(
    report: &IntegrityReport,
    profiles: &[urban_integrity_core::EntityProfile],
    summary_narrative: Option<&str>,
) -> String {
    let mut out = String::new();

    // Section 1: Header
    out.push_str("# UrbanIntegrity Report\n\n");
    out.push_str(&format!("Generated: {}\n\n", report.generated_at));
    out.push_str(&format!(
        "Filters: ward={}, motif={}, min_score={:.3}, from={}, to={}\n\n",
        report.filters_applied.ward.as_deref().unwrap_or("\u{2014}"),
        report
            .filters_applied
            .motif
            .as_deref()
            .unwrap_or("\u{2014}"),
        report.filters_applied.min_score,
        report.filters_applied.from.as_deref().unwrap_or("\u{2014}"),
        report.filters_applied.to.as_deref().unwrap_or("\u{2014}"),
    ));

    // Section 2: Summary Narrative (optional)
    if let Some(narrative) = summary_narrative {
        out.push_str("## Summary Narrative\n\n");
        out.push_str(narrative);
        out.push_str("\n\n");
    }

    // Section 3: Summary stats
    out.push_str("## Summary\n\n");
    out.push_str(&format!("- Total alerts: {}\n", report.total_alerts));
    out.push_str(&format!("- High-risk count: {}\n", report.high_risk_count));
    out.push_str(&format!(
        "- Date range: {}..{}\n\n",
        report.filters_applied.from.as_deref().unwrap_or("\u{2014}"),
        report.filters_applied.to.as_deref().unwrap_or("\u{2014}"),
    ));

    // Section 4: Alert table
    out.push_str("## Alerts\n\n");
    out.push_str(&render_alert_table_md(&report.alerts));
    out.push('\n');

    // Section 5: Top Entities
    out.push_str("## Top Entities\n\n");
    for p in profiles {
        out.push_str(&render_entity_snapshot_md(p));
    }

    // Section 6: Citations
    out.push_str(&render_citations_md(profiles));

    out
}

/// Collect up to 5 top entity profiles for ward-scoped reports.
///
/// Extracts `participants` slugs from each `AlertRecord.result_json`, deduplicates
/// by slug keeping the highest score seen, sorts descending, takes up to 5, and
/// hydrates each via `query_entity_by_id`. Missing entities are silently skipped.
async fn collect_top_entities(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    records: &[urban_integrity_core::AlertRecord],
) -> anyhow::Result<Vec<urban_integrity_core::EntityProfile>> {
    use std::collections::HashMap;

    // Build (slug → max_score) map from result_json participants
    let mut slug_scores: HashMap<String, f64> = HashMap::new();
    for record in records {
        let score = record.score;
        let participants: Vec<String> =
            serde_json::from_str::<serde_json::Value>(&record.result_json)
                .ok()
                .and_then(|v| serde_json::from_value::<Vec<String>>(v["participants"].clone()).ok())
                .unwrap_or_default();
        for slug in participants {
            let entry = slug_scores.entry(slug).or_insert(score);
            if score > *entry {
                *entry = score;
            }
        }
    }

    // Sort by score descending, take up to 5 unique slugs
    let mut ranked: Vec<(String, f64)> = slug_scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(5);

    // Hydrate each slug via query_entity_by_id
    let mut profiles = Vec::new();
    for (slug, _score) in ranked {
        match urban_integrity_core::query_entity_by_id(db, &slug).await {
            Ok(Some(p)) => profiles.push(p),
            Ok(None) => {} // silently skip missing entities
            Err(e) => {
                tracing::warn!(slug = %slug, error = %e, "Failed to hydrate entity for report top-5");
            }
        }
    }
    Ok(profiles)
}

// Ollama summarization is provided by the shared `crate::ollama` module.
// Use `crate::ollama::summarize_via_ollama` at all call sites.

pub async fn run(args: ReportArgs) -> anyhow::Result<()> {
    use crate::db::connect_db;
    use urban_integrity_core::{
        config::AppConfig, is_slug_shape, query_alerts, query_entity_by_id, query_entity_by_name,
        query_entity_lpa_presence, resolve_entity, AlertFilter, LpaPresence,
    };

    let fmt = args.format.clone().unwrap_or_else(OutputFormat::auto);

    // D-03: fail fast on invalid flag combos
    validate_output_file_format(&fmt, &args.output_file)?;

    // WR-02: --scope is only meaningful for entity-scoped reports.
    // Reject it early so the analyst knows their flag was not ignored.
    if args.scope.is_some() && args.entity.is_none() {
        anyhow::bail!("--scope requires --entity; ward-scoped reports do not support --scope");
    }

    // Load config and connect to SurrealDB
    let config = AppConfig::load().context("Config load failed — ensure config.toml exists")?;
    let db = connect_db(&config).await?;

    // REP-05: entity-scoped branch — short-circuits the ward query
    if let Some(ref entity_query) = args.entity {
        let profile_opt: Option<urban_integrity_core::EntityProfile> = if is_slug_shape(
            entity_query,
        ) {
            query_entity_by_id(&db, entity_query)
                .await
                .context("query_entity_by_id failed")?
        } else {
            let results = query_entity_by_name(&db, entity_query)
                .await
                .context("query_entity_by_name failed")?;
            if results.len() > 1 {
                tracing::warn!(
                        "Multiple entities match {}: using first ({}). Use --entity {} for exact lookup.",
                        entity_query,
                        results[0].name,
                        results[0]
                            .ch_number
                            .as_deref()
                            .unwrap_or(&results[0].name)
                    );
            }
            results.into_iter().next()
        };

        let profile = match profile_opt {
            Some(p) => p,
            None => {
                anyhow::bail!("No entity found matching {:?}", entity_query);
            }
        };

        // CRS-02: resolve LPA presence when --scope all-lpa is set (only then).
        // Non-scope JSON output must stay byte-for-byte unchanged (REP-06).
        let lpa_presence: Vec<LpaPresence> = if args.scope.as_deref() == Some("all-lpa") {
            let ids = resolve_entity(&db, entity_query).await.unwrap_or_default();
            match ids.into_iter().next() {
                Some(entity_id) => query_entity_lpa_presence(&db, &entity_id, &config)
                    .await
                    .unwrap_or_default(),
                None => {
                    // CR-03: do not pass the raw user string as a graph entity ID —
                    // it is not a valid graph ID and query_entity_lpa_presence would
                    // silently return empty data.  Warn the analyst instead.
                    tracing::warn!(
                        entity_query,
                        "resolve_entity returned no IDs for --scope all-lpa; \
                         LPA presence will be empty"
                    );
                    vec![]
                }
            }
        } else {
            vec![]
        };

        // Assemble a synthetic IntegrityReport from the entity's alerts
        let alerts: Vec<AlertRow> = profile
            .alerts
            .iter()
            .map(|a| AlertRow {
                id: a.id.clone(),
                motif_type: a.motif_type.clone(),
                score: a.score,
                ward: a.ward_code.clone(),
                detected_at: a.detected_at.clone(),
                entities: None,
            })
            .collect();
        let high_risk_count = alerts.iter().filter(|a| a.score >= 0.8).count();
        let total_alerts = alerts.len();
        let report = IntegrityReport {
            generated_at: chrono::Utc::now().to_rfc3339(),
            filters_applied: ReportFilters {
                ward: None,
                min_score: args.min_score,
                motif: args.motif.clone(),
                from: args.from.clone(),
                to: args.to.clone(),
            },
            total_alerts,
            high_risk_count,
            alerts,
        };
        let profiles = vec![profile];

        // Helper: append LPA presence section to a Markdown string (CRS-02).
        // Only called when scope == "all-lpa".
        let append_lpa_md = |mut md: String, presence: &[LpaPresence]| -> String {
            if !presence.is_empty() {
                md.push_str("\n## LPA Presence\n\n");
                md.push_str("| LPA Code | Contracts | Planning Apps |\n");
                md.push_str("|----------|-----------|---------------|\n");
                for row in presence {
                    md.push_str(&format!(
                        "| {} | {} | {} |\n",
                        row.lpa_code, row.contract_count, row.planning_application_count
                    ));
                }
                md.push('\n');
            }
            md
        };

        match fmt {
            OutputFormat::Markdown => {
                let preliminary_md = render_markdown_report(&report, &profiles, None);
                let narrative = if args.summarize {
                    let truncated: String = preliminary_md.chars().take(4000).collect();
                    crate::ollama::summarize_via_ollama(&truncated).await
                } else {
                    None
                };
                let base_md = render_markdown_report(&report, &profiles, narrative.as_deref());
                let md = if args.scope.as_deref() == Some("all-lpa") {
                    append_lpa_md(base_md, &lpa_presence)
                } else {
                    base_md
                };
                let path = args.output_file.clone().unwrap_or_else(|| {
                    PathBuf::from(default_report_filename(None, args.entity.as_deref(), "md"))
                });
                std::fs::write(&path, &md)
                    .with_context(|| format!("Failed to write report to {}", path.display()))?;
                println!("Report written to {}", path.display());
            }
            OutputFormat::Pdf => {
                // Determine output paths
                let (pdf_path, md_path) = match &args.output_file {
                    Some(p) => {
                        let pdf = if p.extension().and_then(|e| e.to_str()) == Some("pdf") {
                            p.clone()
                        } else {
                            p.with_extension("pdf")
                        };
                        let md = pdf.with_extension("md");
                        (pdf, md)
                    }
                    None => {
                        let pdf = PathBuf::from(default_report_filename(
                            None,
                            args.entity.as_deref(),
                            "pdf",
                        ));
                        let md = PathBuf::from(default_report_filename(
                            None,
                            args.entity.as_deref(),
                            "md",
                        ));
                        (pdf, md)
                    }
                };
                let preliminary_md = render_markdown_report(&report, &profiles, None);
                let narrative = if args.summarize {
                    let truncated: String = preliminary_md.chars().take(4000).collect();
                    crate::ollama::summarize_via_ollama(&truncated).await
                } else {
                    None
                };
                let base_md = render_markdown_report(&report, &profiles, narrative.as_deref());
                let md = if args.scope.as_deref() == Some("all-lpa") {
                    append_lpa_md(base_md, &lpa_presence)
                } else {
                    base_md
                };
                // Write .md FIRST (D-11: .md preserved even if PDF generation fails)
                std::fs::write(&md_path, &md)
                    .with_context(|| format!("Failed to write .md to {}", md_path.display()))?;
                invoke_pdf_tool(&md_path, &pdf_path)?;
                println!("Report written to {}", pdf_path.display());
            }
            OutputFormat::Json => {
                // REP-06: non-scope JSON unchanged; only emit lpa_presence when scope=all-lpa.
                if args.scope.as_deref() == Some("all-lpa") {
                    print_json(&IntegrityReportWithLpa {
                        report: &report,
                        lpa_presence,
                    })?;
                } else {
                    print_json(&report)?;
                }
            }
            OutputFormat::Table => {
                println!("UrbanIntegrity Report \u{2014} {}", report.generated_at);
                println!(
                    "Total alerts: {} | High risk: {}",
                    report.total_alerts, report.high_risk_count
                );
                let table = build_alert_table(&report.alerts);
                println!("{table}");
                // CRS-02: show LPA presence table when scope=all-lpa
                if args.scope.as_deref() == Some("all-lpa") && !lpa_presence.is_empty() {
                    use crate::output::build_entity_lpa_presence_table;
                    println!("\nLPA Presence");
                    println!("{}", build_entity_lpa_presence_table(&lpa_presence));
                }
            }
        }
        return Ok(());
    }

    // Ward-scoped (or global) report path
    let filter = AlertFilter {
        min_score: args.min_score,
        ward_code: args.ward.clone(),
        motif_type: args.motif.clone(),
        from_date: args.from.clone(),
        to_date: args.to.clone(),
        limit: args.limit,
        // A report is a published artefact. Withdrawn alerts must not appear in one.
        include_withdrawn: false,
        ..Default::default()
    };

    let records = query_alerts(&db, &filter)
        .await
        .context("SurrealDB query_alerts failed")?;

    // The page reports how many MATCHED, so the note states the real total instead of inferring
    // truncation from "len == limit" — which was also true when the totals happened to be equal.
    if records.truncated() {
        eprintln!(
            "Note: showing {} of {} matching alerts — use --limit N for more (--limit 0 for all).",
            records.alerts.len(),
            records.total_matched
        );
    }

    let alerts: Vec<AlertRow> = records
        .alerts
        .iter()
        .map(|r| AlertRow {
            id: r.id.clone(),
            motif_type: r.motif_type.clone(),
            score: r.score,
            ward: r.ward_code.clone(),
            detected_at: r.detected_at.clone(),
            entities: None,
        })
        .collect();

    let high_risk_count = alerts.iter().filter(|a| a.score >= 0.8).count();
    let total_alerts = alerts.len();

    let report = IntegrityReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        filters_applied: ReportFilters {
            ward: args.ward.clone(),
            min_score: args.min_score,
            motif: args.motif.clone(),
            from: args.from.clone(),
            to: args.to.clone(),
        },
        total_alerts,
        high_risk_count,
        alerts,
    };

    match fmt {
        OutputFormat::Json => print_json(&report)?,
        OutputFormat::Table => {
            println!("UrbanIntegrity Report \u{2014} {}", report.generated_at);
            println!(
                "Total alerts: {} | High risk: {}",
                report.total_alerts, report.high_risk_count
            );
            let table = build_alert_table(&report.alerts);
            println!("{table}");
        }
        OutputFormat::Markdown => {
            let profiles = collect_top_entities(&db, &records.alerts).await?;
            let preliminary_md = render_markdown_report(&report, &profiles, None);
            let narrative = if args.summarize {
                let truncated: String = preliminary_md.chars().take(4000).collect();
                crate::ollama::summarize_via_ollama(&truncated).await
            } else {
                None
            };
            let md = render_markdown_report(&report, &profiles, narrative.as_deref());
            let path = args.output_file.clone().unwrap_or_else(|| {
                PathBuf::from(default_report_filename(args.ward.as_deref(), None, "md"))
            });
            std::fs::write(&path, &md)
                .with_context(|| format!("Failed to write report to {}", path.display()))?;
            println!("Report written to {}", path.display());
        }
        OutputFormat::Pdf => {
            let profiles = collect_top_entities(&db, &records.alerts).await?;
            // Determine output paths
            let (pdf_path, md_path) = match &args.output_file {
                Some(p) => {
                    let pdf = if p.extension().and_then(|e| e.to_str()) == Some("pdf") {
                        p.clone()
                    } else {
                        p.with_extension("pdf")
                    };
                    let md = pdf.with_extension("md");
                    (pdf, md)
                }
                None => {
                    let pdf =
                        PathBuf::from(default_report_filename(args.ward.as_deref(), None, "pdf"));
                    let md =
                        PathBuf::from(default_report_filename(args.ward.as_deref(), None, "md"));
                    (pdf, md)
                }
            };
            let preliminary_md = render_markdown_report(&report, &profiles, None);
            let narrative = if args.summarize {
                let truncated: String = preliminary_md.chars().take(4000).collect();
                crate::ollama::summarize_via_ollama(&truncated).await
            } else {
                None
            };
            let md = render_markdown_report(&report, &profiles, narrative.as_deref());
            // Write .md FIRST (D-11: .md preserved even if PDF generation fails)
            std::fs::write(&md_path, &md)
                .with_context(|| format!("Failed to write .md to {}", md_path.display()))?;
            invoke_pdf_tool(&md_path, &pdf_path)?;
            println!("Report written to {}", pdf_path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::{AlertRecord, ContractRow, EntityProfile, PlanningApplicationRow};
    use urban_integrity_schema::types::node::EntityStatus;

    fn make_profile(ch_number: Option<&str>) -> EntityProfile {
        EntityProfile {
            id: "test".to_string(),
            name: "Apex Build Ltd".to_owned(),
            ch_number: ch_number.map(str::to_owned),
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

    fn make_alert_row(id: &str, score: f64) -> AlertRow {
        AlertRow {
            id: id.to_owned(),
            motif_type: "phoenix".to_owned(),
            score,
            ward: Some("W05001492".to_owned()),
            detected_at: "2026-05-23T10:00:00Z".to_owned(),
            entities: None,
        }
    }

    fn make_report(alerts: Vec<AlertRow>) -> IntegrityReport {
        let high_risk_count = alerts.iter().filter(|a| a.score >= 0.8).count();
        let total_alerts = alerts.len();
        IntegrityReport {
            generated_at: "2026-05-23T10:00:00Z".to_owned(),
            filters_applied: ReportFilters {
                ward: Some("W05001492".to_owned()),
                min_score: 0.5,
                motif: None,
                from: None,
                to: None,
            },
            total_alerts,
            high_risk_count,
            alerts,
        }
    }

    // ── default_report_filename tests ────────────────────────────────────────

    #[test]
    fn default_filename_ward_scope() {
        let name = default_report_filename(Some("W05001492"), None, "md");
        assert!(
            name.starts_with("integrity-report-w05001492-"),
            "Expected prefix integrity-report-w05001492-, got: {name}"
        );
        assert!(name.ends_with(".md"), "Expected .md extension, got: {name}");
    }

    #[test]
    fn default_filename_entity_scope() {
        let name = default_report_filename(None, Some("Apex Build Ltd"), "pdf");
        assert!(
            name.starts_with("integrity-report-apex-build-ltd-"),
            "Expected prefix integrity-report-apex-build-ltd-, got: {name}"
        );
        assert!(
            name.ends_with(".pdf"),
            "Expected .pdf extension, got: {name}"
        );
    }

    #[test]
    fn default_filename_all_scope() {
        let name = default_report_filename(None, None, "md");
        assert!(
            name.starts_with("integrity-report-all-"),
            "Expected prefix integrity-report-all-, got: {name}"
        );
    }

    // ── validate_output_file_format tests ────────────────────────────────────

    #[test]
    fn validate_output_file_format_rejects_json_with_file() {
        let result =
            validate_output_file_format(&OutputFormat::Json, &Some(PathBuf::from("x.json")));
        assert!(result.is_err(), "Expected Err for Json + output_file");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--output-file is only valid with --format md or --format pdf"),
            "Error message must contain the expected text, got: {msg}"
        );
    }

    #[test]
    fn validate_output_file_format_accepts_markdown_with_file() {
        let result =
            validate_output_file_format(&OutputFormat::Markdown, &Some(PathBuf::from("x.md")));
        assert!(result.is_ok(), "Expected Ok for Markdown + output_file");
    }

    #[test]
    fn validate_output_file_format_accepts_markdown_without_file() {
        let result = validate_output_file_format(&OutputFormat::Markdown, &None);
        assert!(
            result.is_ok(),
            "Expected Ok for Markdown without output_file"
        );
    }

    #[test]
    fn validate_output_file_format_rejects_table_with_file() {
        let result =
            validate_output_file_format(&OutputFormat::Table, &Some(PathBuf::from("x.txt")));
        assert!(result.is_err(), "Expected Err for Table + output_file");
    }

    // ── render_alert_table_md tests ──────────────────────────────────────────

    #[test]
    fn render_alert_table_md_header_and_separator() {
        let result = render_alert_table_md(&[]);
        assert!(
            result.contains("| ID | Motif | Score | Ward | Detected | Entities |"),
            "Header row missing, got:\n{result}"
        );
        assert!(
            result.contains("|---|---|---|---|---|---|"),
            "Separator row missing, got:\n{result}"
        );
    }

    #[test]
    fn render_alert_table_md_formats_score_to_3_decimals() {
        let alert = make_alert_row("alert-001", 0.876543);
        let result = render_alert_table_md(&[alert]);
        assert!(
            result.contains("0.877"),
            "Score should be formatted to 3 decimals (0.877), got:\n{result}"
        );
    }

    #[test]
    fn render_alert_table_md_uses_em_dash_for_none_ward() {
        let alert = AlertRow {
            id: "a-1".to_owned(),
            motif_type: "cluster".to_owned(),
            score: 0.7,
            ward: None,
            detected_at: "2026-05-23T10:00:00Z".to_owned(),
            entities: None,
        };
        let result = render_alert_table_md(&[alert]);
        assert!(
            result.contains('\u{2014}'),
            "None ward should render as em-dash, got:\n{result}"
        );
    }

    // ── render_markdown_report section tests ─────────────────────────────────

    #[test]
    fn render_markdown_report_contains_all_six_sections() {
        let report = make_report(vec![make_alert_row("a-1", 0.9)]);
        let profiles: Vec<EntityProfile> = vec![];
        let md = render_markdown_report(&report, &profiles, None);

        assert!(
            md.contains("# UrbanIntegrity Report"),
            "Section 1 header missing:\n{md}"
        );
        assert!(md.contains("## Summary"), "Section 3 missing:\n{md}");
        assert!(md.contains("## Alerts"), "Section 4 missing:\n{md}");
        assert!(md.contains("## Top Entities"), "Section 5 missing:\n{md}");
        assert!(md.contains("## Source Data"), "Section 6 missing:\n{md}");
    }

    #[test]
    fn render_markdown_report_no_summary_narrative_when_none() {
        let report = make_report(vec![]);
        let md = render_markdown_report(&report, &[], None);
        assert!(
            !md.contains("## Summary Narrative"),
            "Summary Narrative section must NOT appear when summary_narrative is None:\n{md}"
        );
    }

    #[test]
    fn render_markdown_report_includes_summary_narrative_when_some() {
        let report = make_report(vec![]);
        let md = render_markdown_report(&report, &[], Some("This is the narrative."));
        assert!(
            md.contains("## Summary Narrative"),
            "Summary Narrative section must appear:\n{md}"
        );
        assert!(
            md.contains("This is the narrative."),
            "Narrative text must appear:\n{md}"
        );
    }

    // ── render_citations_md tests ────────────────────────────────────────────

    #[test]
    fn render_citations_md_ch_hyperlink() {
        let profile = make_profile(Some("12345678"));
        let result = render_citations_md(&[profile]);
        assert!(
            result.contains("[Companies House](https://find-and-update.company-information.service.gov.uk/company/12345678)"),
            "CH hyperlink missing, got:\n{result}"
        );
    }

    #[test]
    fn render_citations_md_tender_ref_present() {
        let mut profile = make_profile(Some("99999999"));
        profile.contracts.push(ContractRow {
            id: "CF:CF-TEST-001".to_owned(),
            name: "Test Contract".to_owned(),
            value_gbp: None,
            status: None,
            published: None,
            ward_code: None,
            tender_ref: Some("CF-TEST-001".to_owned()),
            application_ref: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            temporal: urban_integrity_core::Temporal::unfiltered(
                urban_integrity_core::EffectiveRange::Unrecorded {
                    basis: "hand-built test fixture".to_owned(),
                },
            ),
        });
        let result = render_citations_md(&[profile]);
        assert!(
            result.contains("Contract ref: CF-TEST-001"),
            "tender_ref not found, got:\n{result}"
        );
    }

    #[test]
    fn render_citations_md_application_ref_present() {
        let mut profile = make_profile(Some("88888888"));
        profile.contracts.push(ContractRow {
            id: "planning:FUL/2024/001234".to_owned(),
            name: "Planning App".to_owned(),
            value_gbp: None,
            status: None,
            published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: Some("FUL/2024/001234".to_owned()),
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            temporal: urban_integrity_core::Temporal::unfiltered(
                urban_integrity_core::EffectiveRange::Unrecorded {
                    basis: "hand-built test fixture".to_owned(),
                },
            ),
        });
        let result = render_citations_md(&[profile]);
        assert!(
            result.contains("Planning application: FUL/2024/001234"),
            "application_ref not found, got:\n{result}"
        );
    }

    /// D-03: the split must move citations between sections without losing or inventing one.
    ///
    /// Renders the same underlying facts twice — once shaped as they were **before** the split
    /// (every row in `contracts`, planning rows carrying an `application_ref`) and once as they are
    /// **after** it — and asserts the two citation line *sets* are equal.
    ///
    /// Sets rather than strings, deliberately: for a subject holding both kinds the split changes
    /// the order (contracts now all precede planning applications, where the two used to interleave
    /// in destination-id order). No entity on `an_anglesey_aebaa785` holds both — 0 of 25,314
    /// `AwardedTo` sources overlap, measured 2026-09-15 — so this fixture is the only place that
    /// ordering difference is currently observable, and asserting string equality would fail for a
    /// reason that has nothing to do with the invariant being guarded.
    ///
    /// NON-VACUITY: both sections are asserted non-empty and the citation list is asserted to
    /// contain lines of both kinds before any comparison. A version of this test that compared two
    /// empty renderings would pass while the split had emptied both sections, which is the exact
    /// failure mode `is_planning_application_id`'s doc comment describes.
    #[test]
    fn render_citations_md_preserves_every_citation_across_the_split() {
        let contract = |name: &str, tender: &str| ContractRow {
            id: format!("CF:{tender}"),
            name: name.to_owned(),
            value_gbp: None,
            status: None,
            published: None,
            ward_code: None,
            tender_ref: Some(tender.to_owned()),
            application_ref: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            temporal: urban_integrity_core::Temporal::unfiltered(
                urban_integrity_core::EffectiveRange::Unrecorded {
                    basis: "hand-built test fixture".to_owned(),
                },
            ),
        };

        // Before the split: one section, planning rows distinguished only by `application_ref`.
        let mut before = make_profile(Some("11111111"));
        before.contracts = vec![
            contract("CF Works", "CF-2024-0001"),
            ContractRow {
                id: "planning:X99C001".to_owned(),
                name: "Change of use".to_owned(),
                value_gbp: None,
                status: None,
                published: None,
                ward_code: None,
                tender_ref: None,
                application_ref: Some("X99C001".to_owned()),
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            },
            contract("CF Resurfacing", "CF-2024-0002"),
            ContractRow {
                id: "planning:X99C002".to_owned(),
                name: "Rear extension".to_owned(),
                value_gbp: None,
                status: None,
                published: None,
                ward_code: None,
                tender_ref: None,
                application_ref: Some("X99C002".to_owned()),
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            },
        ];

        // After the split: the same four facts, routed by destination-id prefix.
        let mut after = make_profile(Some("11111111"));
        after.contracts = vec![
            contract("CF Works", "CF-2024-0001"),
            contract("CF Resurfacing", "CF-2024-0002"),
        ];
        after.planning_applications = vec![
            PlanningApplicationRow {
                id: "planning:X99C001".to_owned(),
                name: "Change of use".to_owned(),
                status: None,
                published: None,
                ward_code: None,
                application_ref: Some("X99C001".to_owned()),
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            },
            PlanningApplicationRow {
                id: "planning:X99C002".to_owned(),
                name: "Rear extension".to_owned(),
                status: None,
                published: None,
                ward_code: None,
                application_ref: Some("X99C002".to_owned()),
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            },
        ];

        assert!(
            !after.contracts.is_empty() && !after.planning_applications.is_empty(),
            "NON-VACUITY: the post-split fixture must populate BOTH sections, or the comparison \
             below can pass by comparing two identical-but-wrong renderings"
        );

        let sorted_lines = |md: &str| {
            let mut v: Vec<String> = md
                .lines()
                .filter(|l| l.starts_with("  - "))
                .map(str::to_owned)
                .collect();
            v.sort();
            v
        };

        let before_lines = sorted_lines(&render_citations_md(&[before]));
        let after_lines = sorted_lines(&render_citations_md(&[after]));

        assert_eq!(
            before_lines.len(),
            4,
            "NON-VACUITY: the pre-split rendering must cite all four facts, got {before_lines:?}"
        );
        assert!(
            after_lines.iter().any(|l| l.contains("Contract ref: "))
                && after_lines
                    .iter()
                    .any(|l| l.contains("Planning application: ")),
            "NON-VACUITY: the post-split rendering must cite BOTH kinds, got {after_lines:?}"
        );
        assert_eq!(
            after_lines, before_lines,
            "the D-03 split must not lose or invent a citation"
        );
    }

    #[test]
    fn render_citations_md_fallback_to_contract_name() {
        let mut profile = make_profile(Some("77777777"));
        profile.contracts.push(ContractRow {
            id: "CF:x-fixture-08".to_owned(),
            name: "Road Repair Contract".to_owned(),
            value_gbp: None,
            status: None,
            published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            temporal: urban_integrity_core::Temporal::unfiltered(
                urban_integrity_core::EffectiveRange::Unrecorded {
                    basis: "hand-built test fixture".to_owned(),
                },
            ),
        });
        let result = render_citations_md(&[profile]);
        assert!(
            result.contains("Contract: Road Repair Contract"),
            "D-04 fallback to contract name not found, got:\n{result}"
        );
    }

    // ── render_entity_snapshot_md tests ─────────────────────────────────────

    #[test]
    fn render_entity_snapshot_md_ch_hyperlink() {
        let profile = make_profile(Some("12345678"));
        let result = render_entity_snapshot_md(&profile);
        assert!(
            result.contains("find-and-update.company-information.service.gov.uk/company/12345678"),
            "CH hyperlink missing in entity snapshot, got:\n{result}"
        );
    }

    // ── invoke_pdf_tool ───────────────────────────────────────────────────────

    #[test]
    fn invoke_pdf_tool_with_nonexistent_md_file_returns_result() {
        use std::path::Path;
        // With a nonexistent input file, invoke_pdf_tool must return without panicking.
        // Whether it returns Ok or Err depends on which PDF tools are installed.
        let md = Path::new("/tmp/urban-integrity-test-nonexistent-12345.md");
        let pdf = Path::new("/tmp/urban-integrity-test-nonexistent-12345.pdf");
        let result = invoke_pdf_tool(md, pdf);
        // The exact Result variant is environment-dependent, but it must not panic.
        // When no tools are installed, it returns Err with the install message.
        if let Err(ref e) = result {
            let msg = e.to_string();
            assert!(
                msg.contains("pandoc") || msg.contains("PDF"),
                "Err must mention pandoc/PDF, got: {msg}"
            );
        }
        // Ok is also acceptable if pandoc happened to succeed (unlikely with no .md file)
    }

    // ── render_entity_snapshot_md — no CH number ──────────────────────────────

    #[test]
    fn render_entity_snapshot_md_no_ch_number_uses_em_dash() {
        let profile = make_profile(None);
        let result = render_entity_snapshot_md(&profile);
        assert!(
            result.contains('\u{2014}'),
            "No CH number must render as em-dash, got:\n{result}"
        );
        assert!(
            !result.contains("find-and-update"),
            "No CH number must NOT produce a hyperlink, got:\n{result}"
        );
    }

    #[test]
    fn render_entity_snapshot_md_no_alerts_shows_em_dash_for_highest() {
        let profile = make_profile(None);
        let result = render_entity_snapshot_md(&profile);
        // With empty alerts vec, Highest Alert cell should be em-dash
        let lines: Vec<&str> = result.lines().collect();
        let highest_line = lines
            .iter()
            .find(|l| l.contains("Highest Alert"))
            .expect("Highest Alert row must exist");
        assert!(
            highest_line.contains('\u{2014}'),
            "Empty alerts must show em-dash in Highest Alert, got: {highest_line}"
        );
    }

    // ── render_markdown_report — with ward filters in header ──────────────────

    #[test]
    fn render_markdown_report_shows_filter_values_in_header() {
        let report = IntegrityReport {
            generated_at: "2026-05-23T10:00:00Z".into(),
            filters_applied: ReportFilters {
                ward: Some("E05001234".into()),
                min_score: 0.75,
                motif: Some("phoenix".into()),
                from: Some("2026-01-01".into()),
                to: Some("2026-12-31".into()),
            },
            total_alerts: 2,
            high_risk_count: 1,
            alerts: vec![make_alert_row("a-1", 0.9), make_alert_row("a-2", 0.6)],
        };
        let md = render_markdown_report(&report, &[], None);
        assert!(
            md.contains("E05001234"),
            "ward filter must appear in header, got:\n{md}"
        );
        assert!(
            md.contains("0.750"),
            "min_score must appear in header, got:\n{md}"
        );
        assert!(
            md.contains("phoenix"),
            "motif must appear in header, got:\n{md}"
        );
        assert!(
            md.contains("2026-01-01"),
            "from date must appear, got:\n{md}"
        );
        assert!(md.contains("2026-12-31"), "to date must appear, got:\n{md}");
    }

    #[test]
    fn render_markdown_report_counts_match_report_fields() {
        // Scores 0.9 and 0.85 are >= 0.8 (high-risk); 0.6 is not → high_risk_count = 2
        let report = make_report(vec![
            make_alert_row("a-1", 0.9),
            make_alert_row("a-2", 0.85),
            make_alert_row("a-3", 0.6),
        ]);
        let md = render_markdown_report(&report, &[], None);
        assert!(
            md.contains("Total alerts: 3"),
            "total_alerts must appear, got:\n{md}"
        );
        assert!(
            md.contains("High-risk count: 2"),
            "high_risk_count must appear, got:\n{md}"
        );
    }

    #[test]
    fn render_entity_snapshot_md_highest_alert() {
        let mut profile = make_profile(None);
        profile.alerts.push(AlertRecord {
            id: "a-1".to_owned(),
            motif_type: "phoenix".to_owned(),
            score: 0.95,
            result_json: "{}".to_owned(),
            detected_at: "2026-05-23T10:00:00Z".to_owned(),
            ward_code: None,
            withdrawn_at: None,
            withdrawn_reason: None,
            participants: vec![],
            first_seen_run_id: None,
            last_seen_run_id: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        });
        profile.alerts.push(AlertRecord {
            id: "a-2".to_owned(),
            motif_type: "cluster".to_owned(),
            score: 0.75,
            result_json: "{}".to_owned(),
            detected_at: "2026-05-23T10:00:00Z".to_owned(),
            ward_code: None,
            withdrawn_at: None,
            withdrawn_reason: None,
            participants: vec![],
            first_seen_run_id: None,
            last_seen_run_id: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        });
        let result = render_entity_snapshot_md(&profile);
        assert!(
            result.contains("phoenix"),
            "Highest alert motif type must be 'phoenix' (score 0.95), got:\n{result}"
        );
        assert!(
            result.contains("0.950"),
            "Highest alert score must be formatted to 3 decimals, got:\n{result}"
        );
    }
}
