use anyhow::Result;
use clap::Args;
use std::sync::Arc;
use tracing::info;
use urban_integrity_core::config::AppConfig;
use urban_integrity_ingestor::backfill::{
    ch::ChBackfiller, planning::PlanningBackfiller, ward::WardBackfiller, DbSession, SessionCreds,
};
use urban_integrity_ingestor::planning::sidecar::ScrapingSidecar;

use crate::db::{connect_db, resolve_analysis_db};

#[derive(Debug, Args)]
pub struct BackfillArgs {
    #[command(subcommand)]
    pub command: BackfillCommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum BackfillCommand {
    /// Link planning applicants to Companies House numbers via CH REST API
    Ch(BackfillChArgs),
    /// Resolve ward codes for Contracts Finder contracts via postcodes.io
    Ward(BackfillWardArgs),
    /// Repair planning Contracts: re-scrape the wards whose applications are still attributed to
    /// the unknown-applicant placeholder, and stamp explicit applicant provenance across the
    /// planning corpus
    Planning(BackfillPlanningArgs),
    /// Fetch real `date_of_cessation` from Companies House so Phoenix's window is anchored
    /// where the company actually failed, not where the graph first observed it
    Cessation(BackfillCessationArgs),
    /// Fetch officers for Companies House entities that have none, keyed on the identity
    /// evidence CH publishes rather than on the name alone
    Officers(BackfillOfficersArgs),
    /// Re-run the PSC↔officer identity join over data already in the graph — no CH API calls
    ///
    /// The join normally runs inside the `ch_psc` adapter's fetch loop, so correcting the
    /// matcher would otherwise mean re-fetching every PSC record to re-derive links from data
    /// already staged. Idempotent: existing `ch_psc` links are skipped, not duplicated.
    PscLinks(BackfillPscLinksArgs),
}

#[derive(Debug, Args)]
pub struct BackfillPscLinksArgs {
    /// Count the links that would be written without writing them
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct BackfillChArgs {
    /// Show what would be done without writing to the DB
    #[arg(long)]
    pub dry_run: bool,
    /// CH REST API requests per second. Companies House allows 600 requests per
    /// 5 minutes — 2/s — and throttles above that. The default was 5, which
    /// produced `CH API 429 rate limit` errors within the first minute of a live
    /// run on 2026-07-28; the same run at 2/s logged zero 429s over an hour.
    #[arg(long, default_value_t = 2)]
    pub rate_limit: u32,
    /// Discard any saved resume cursor and walk every candidate again.
    ///
    /// A full pass is ~20,000 Companies House lookups at 2/s — several hours — so an
    /// interrupted run resumes by default. Use this when the candidate set has changed
    /// underneath a stale cursor, or to re-test names that previously found no match.
    #[arg(long)]
    pub restart: bool,
}

#[derive(Debug, Args)]
pub struct BackfillCessationArgs {
    /// Show what would be done without writing to the DB, and without spending CH quota
    ///
    /// A dry run answers only from the existing profile cache; uncached targets are reported as
    /// skipped rather than fetched. It also neither reads nor writes the resume cursor, since it
    /// changes nothing and must not claim progress.
    #[arg(long)]
    pub dry_run: bool,
    /// CH REST API requests per second. Companies House allows 600 per 5 minutes — 2/s.
    #[arg(long, default_value_t = 2)]
    pub rate_limit: u32,
    /// Discard any saved resume cursor and walk every candidate again
    #[arg(long)]
    pub restart: bool,
    /// Stop after this many network fetches, so a first live pass can be bounded
    #[arg(long)]
    pub max_fetches: Option<usize>,
}

#[derive(Debug, Args)]
pub struct BackfillOfficersArgs {
    /// Show what would be done without writing to the DB, and without spending CH quota
    #[arg(long)]
    pub dry_run: bool,
    /// CH REST API requests per second. Companies House allows 600 per 5 minutes — 2/s.
    #[arg(long, default_value_t = 2)]
    pub rate_limit: u32,
    /// Discard any saved resume cursor and walk every candidate again
    #[arg(long)]
    pub restart: bool,
    /// Stop after this many network fetches, so a first live pass can be bounded
    #[arg(long)]
    pub max_fetches: Option<usize>,
    /// Audit the officer keys ALREADY in the graph instead of ingesting new ones
    ///
    /// The write guard cannot see what earlier tools wrote, and `scripts/ch_backfill.py`'s
    /// name-only output is still in the graph. This checks existing `director:` nodes against
    /// Companies House evidence and records an `officer_key_collision` anomaly for each key that
    /// conflates people CH says are different — flagging separately the ones already fused, which
    /// are asserting a false identity today.
    #[arg(long)]
    pub audit: bool,
    /// Move officer events off the uncorroborated colliding keys `--audit` reports
    ///
    /// Each event is re-keyed on the Companies House officer id that its company — or its
    /// appointment date and role — attributes it to. Events no CH evidence attributes are left
    /// exactly where they are and reported, never guessed at. Rewrites `graph_events`, so this is
    /// a data correction: run it with `--dry-run` first, and `project` afterwards.
    #[arg(long, conflicts_with = "audit")]
    pub split_collisions: bool,
}

#[derive(Debug, Args)]
pub struct BackfillWardArgs {
    /// Show what would be done without writing to the DB
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct BackfillPlanningArgs {
    /// Show what would be done without writing to the DB
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: BackfillArgs) -> Result<()> {
    let config = AppConfig::load()?;
    let db = Arc::new(connect_db(&config).await?);

    // Every backfill is a multi-hour, single-connection job, and SurrealDB's HTTP session token
    // expires after ~1 hour: a `backfill ch` run died at that mark on 2026-07-28 with
    // `401 Unauthorized for url (http://localhost:8000/rpc)`. `DbSession` re-signs in and retries
    // the query; the guard below refreshes the token proactively so that path stays a backstop.
    //
    // The database recorded here is the one `--analysis` RESOLVED to, not `config.surreal.database`
    // — signing in again resets the connection's namespace/database selection, so reading the raw
    // config field would move a mid-flight run onto a different analysis (the `project` defect from
    // PR #31, in a place where it would be far harder to notice).
    let db_name = resolve_analysis_db(&config).await?;
    let session = match (
        config.surreal.username.as_deref(),
        config.surreal.password.as_deref(),
    ) {
        (Some(user), Some(pass)) => DbSession::new(
            db.clone(),
            SessionCreds {
                username: user.to_owned(),
                password: pass.to_owned(),
                namespace: config.surreal.namespace.clone(),
                database: db_name.clone(),
            },
        ),
        // No credentials configured — SurrealDB is running without auth, so there is no session
        // to expire and nothing to sign back in with.
        _ => DbSession::without_reauth(db.clone()),
    };
    let _reauth_guard = session.spawn_periodic_reauth();

    match args.command {
        BackfillCommand::Ch(ch_args) => {
            // Companies House issues two DIFFERENT key classes, and they are not
            // interchangeable: the streaming key authenticates
            // `stream.companieshouse.gov.uk`, the REST key authenticates
            // `api.company-information.service.gov.uk`. This backfill calls the REST
            // search endpoint, so it needs the REST key.
            //
            // It previously read `ingest.ch_api_key` — the STREAMING key — so every
            // lookup 401'd and the command could never attach a company number to
            // anything. Verified live 2026-07-28 against /search/companies:
            // ingest key -> HTTP 401, enricher key -> HTTP 200. That is why the live
            // analysis had 0 of 22,847 `director:` entities carrying a company number.
            let api_key = config
                .enricher
                .ch
                .api_key
                .as_deref()
                .filter(|k| !k.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Companies House REST key not configured — set \
                         URBAN__ENRICHER__CH__API_KEY. Note this is the REST key, a \
                         different credential from URBAN__INGEST__CH_API_KEY (streaming), \
                         which 401s against the REST search endpoint."
                    )
                })?
                .to_owned();
            let backfiller =
                ChBackfiller::new(session, api_key, ch_args.rate_limit, ch_args.dry_run)
                    .restart(ch_args.restart);
            let stats = backfiller.run().await?;
            info!(
                scanned = stats.scanned,
                matched = stats.matched,
                skipped = stats.skipped,
                errors = stats.errors,
                dry_run = ch_args.dry_run,
                "backfill ch complete"
            );
            println!(
                "CH backfill: {} scanned, {} matched, {} skipped, {} errors{}",
                stats.scanned,
                stats.matched,
                stats.skipped,
                stats.errors,
                if ch_args.dry_run { " [DRY RUN]" } else { "" }
            );
        }
        BackfillCommand::Cessation(args) => {
            // The REST key, not the streaming key — see the note on `backfill ch` above.
            let api_key = config
                .enricher
                .ch
                .api_key
                .as_deref()
                .filter(|k| !k.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Companies House REST key not configured — set \
                         URBAN__ENRICHER__CH__API_KEY (the REST key, not \
                         URBAN__INGEST__CH_API_KEY, which is for streaming and 401s here)."
                    )
                })?
                .to_owned();
            let backfiller =
                urban_integrity_ingestor::backfill::cessation::CessationBackfiller::new(
                    session,
                    api_key,
                    args.rate_limit,
                    args.dry_run,
                )
                .restart(args.restart)
                .max_fetches(args.max_fetches);
            let stats = backfiller.run().await?;
            info!(
                scanned = stats.scanned,
                matched = stats.matched,
                skipped = stats.skipped,
                errors = stats.errors,
                dry_run = args.dry_run,
                "backfill cessation complete"
            );
            println!(
                "Cessation backfill: {} targets, {} dated, {} skipped, {} errors{}",
                stats.scanned,
                stats.matched,
                stats.skipped,
                stats.errors,
                if args.dry_run { " [DRY RUN]" } else { "" }
            );
        }
        BackfillCommand::Officers(args) => {
            // `--audit` and `--split-collisions` read the fetch cache and the graph; neither makes
            // a network call, so demanding a REST key would block two offline data corrections.
            let offline = args.audit || args.split_collisions;
            let api_key = config
                .enricher
                .ch
                .api_key
                .as_deref()
                .filter(|k| !k.is_empty())
                .map(str::to_owned)
                .or_else(|| offline.then(String::new))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Companies House REST key not configured — set \
                         URBAN__ENRICHER__CH__API_KEY (the REST key, not \
                         URBAN__INGEST__CH_API_KEY, which is for streaming and 401s here)."
                    )
                })?;
            let backfiller = urban_integrity_ingestor::backfill::officers::OfficerBackfiller::new(
                session,
                api_key,
                args.rate_limit,
                args.dry_run,
            )
            .restart(args.restart)
            .max_fetches(args.max_fetches)
            .audit_only(args.audit);
            let stats = if args.split_collisions {
                let stats = backfiller.run_split_collisions().await?;
                println!(
                    "Officer collision split: {} event(s) on colliding keys, {} re-keyed, {} left \
                     in place as unattributable{}",
                    stats.scanned,
                    stats.matched,
                    stats.skipped,
                    if args.dry_run {
                        " [DRY RUN — nothing written]"
                    } else {
                        " — run `project` to rebuild the native tables"
                    }
                );
                return Ok(());
            } else {
                backfiller.run().await?
            };
            info!(
                scanned = stats.scanned,
                matched = stats.matched,
                skipped = stats.skipped,
                errors = stats.errors,
                dry_run = args.dry_run,
                "backfill officers complete"
            );
            if args.audit {
                println!(
                    "Officer key audit: {} appointments checked, {} keys ALREADY FUSED, {} colliding but not yet fused{}",
                    stats.scanned,
                    stats.matched,
                    stats.skipped,
                    if args.dry_run { " [DRY RUN — nothing recorded]" } else { " — see `anomalies list`" }
                );
                return Ok(());
            }
            println!(
                "Officer backfill: {} uncovered companies, {} appointments written, {} skipped, {} errors{}",
                stats.scanned,
                stats.matched,
                stats.skipped,
                stats.errors,
                if args.dry_run { " [DRY RUN]" } else { "" }
            );
        }
        BackfillCommand::Ward(ward_args) => {
            let backfiller = WardBackfiller::new(session, ward_args.dry_run);
            let stats = backfiller.run().await?;
            info!(
                scanned = stats.scanned,
                matched = stats.matched,
                skipped = stats.skipped,
                errors = stats.errors,
                dry_run = ward_args.dry_run,
                "backfill ward complete"
            );
            println!(
                "Ward backfill: {} scanned, {} matched, {} skipped, {} errors{}",
                stats.scanned,
                stats.matched,
                stats.skipped,
                stats.errors,
                if ward_args.dry_run { " [DRY RUN]" } else { "" }
            );
        }
        BackfillCommand::Planning(planning_args) => {
            let sidecar = ScrapingSidecar::with_timeout(
                &config.ingest.scrapling_url,
                config.ingest.scrapling_port,
                config.ingest.scrapling_timeout_secs,
            );
            let backfiller = PlanningBackfiller::new(
                session,
                sidecar,
                config.ingest.planning_portals.clone(),
                planning_args.dry_run,
            );
            let stats = backfiller.run().await?;
            info!(
                scanned = stats.scanned,
                matched = stats.matched,
                skipped = stats.skipped,
                bulk_stamped = stats.bulk_stamped,
                already_resolved = stats.already_resolved,
                errors = stats.errors,
                dry_run = planning_args.dry_run,
                "backfill planning complete"
            );
            println!(
                "Planning backfill: {} scanned, {} matched, {} skipped, {} bulk-stamped, \
                 {} already resolved, {} errors{}",
                stats.scanned,
                stats.matched,
                stats.skipped,
                stats.bulk_stamped,
                stats.already_resolved,
                stats.errors,
                if planning_args.dry_run {
                    " [DRY RUN]"
                } else {
                    ""
                }
            );
        }
        BackfillCommand::PscLinks(psc_args) => {
            // Reads and writes only what is already in the graph — no HTTP client, no API key,
            // so none of the CH key plumbing above applies.
            let stats = urban_integrity_enricher::adapters::ch_psc::backfill_psc_officer_links(
                session.handle(),
                psc_args.dry_run,
            )
            .await?;
            info!(
                pscs_scanned = stats.pscs_scanned,
                matched = stats.matched,
                already_linked = stats.already_linked,
                written = stats.written,
                invalid_company_number = stats.invalid_company_number,
                dry_run = psc_args.dry_run,
                "backfill psc-links complete"
            );
            println!(
                "PSC-link backfill: {} PSCs scanned, {} matched an officer, \
                 {} already linked, {} written, {} rejected on company number{}",
                stats.pscs_scanned,
                stats.matched,
                stats.already_linked,
                stats.written,
                stats.invalid_company_number,
                if psc_args.dry_run { " [DRY RUN]" } else { "" }
            );
        }
    }
    Ok(())
}
