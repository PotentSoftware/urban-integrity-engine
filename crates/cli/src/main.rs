use anyhow::Result;
use clap::{Parser, Subcommand};
use urban_integrity_core::telemetry;

use urban_integrity_cli::commands;

#[derive(Debug, Parser)]
#[command(
    name = "urban-integrity",
    about = "Real-time procurement corruption detection engine",
    version,
    propagate_version = true
)]
struct Cli {
    /// Operate on a named analysis (registry db_name). Default: main.
    /// Bridged to URBAN__ANALYSIS so every command's AppConfig::load() sees it.
    #[arg(long, global = true)]
    analysis: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Generate an integrity report for a ward or time range
    Report(commands::report::ReportArgs),
    /// List and inspect flagged motif alerts
    Alerts(commands::alerts::AlertsArgs),
    /// List and adjudicate data anomalies — inputs the pipeline refused to guess at
    Anomalies(commands::anomalies::AnomaliesArgs),
    /// Show graph engine status and statistics
    Status(commands::status::StatusArgs),
    /// Query entity profile by name or Companies House number
    ///
    /// EXAMPLES:
    ///
    ///   # Look up by name (table output, default)
    ///   urban-integrity entity "Apex Build Ltd"
    ///
    ///   # Look up by CH number
    ///   urban-integrity entity --ch-number 12345678
    ///
    ///   # Get JSON and filter with jq
    ///   urban-integrity entity "Apex Build Ltd" --output json | jq '.directors[].name'
    ///
    ///   # Extract incorporation date for age check
    ///   urban-integrity entity --ch-number 12345678 --output json | jq '{name, incorporationDate}'
    ///
    ///   # Use a director slug seen on another company profile
    ///   urban-integrity entity apex-build-ltd
    ///
    ///   # Check related entities via piped jq
    ///   urban-integrity entity "Apex Build Ltd" --output json | jq '.relatedEntities[] | select(.sharedConnection.type == "director")'
    Entity(commands::entity::EntityArgs),
    /// Find the shortest path between two entities in the HIN graph
    GraphPath(commands::graph_path::GraphPathArgs),
    /// Start the graph engine (API server + ingestors)
    Serve,
    /// Scrape planning portal data on demand (--ward WARD_CODE or --all)
    IngestPlanning(commands::ingest_planning::IngestPlanningArgs),
    /// Backfill enrichment data (CH numbers, ward codes) into the live graph
    Backfill(commands::backfill::BackfillArgs),
    /// Project graph_events WAL into native SurrealDB tables for visual exploration
    Project(commands::project::ProjectArgs),
    /// Export the projected graph to GraphML for Gephi ingestion
    ExportGraphml(commands::export_graphml::ExportGraphmlArgs),
    /// Run a pre-signature risk audit for a Companies House entity
    Verify(commands::verify::VerifyArgs),
    /// Run OSINT enrichment against configured sources (ENR-07)
    Enrich(commands::enrich::EnrichArgs),
    /// Review and manage OSINT identity resolution links (REV-01..REV-05)
    Links(commands::links::LinksArgs),
    /// Review pre-permission land-flip candidates (FLIP-04)
    Flips(commands::flips::FlipsArgs),
    /// Apply IN/OUT schema constraints to all 11 relation tables and re-project (SCH-01..SCH-04)
    ///
    /// PREREQUISITE: Run 'urban-integrity enrich' first (see Pitfall 3 in the phase 21 research notes)
    Migrate(commands::migrate::MigrateArgs),
    /// Purge orphan Property nodes + identity_links left by the pre-fix CCOD ingest
    ///
    /// Dry-run by default (counts only); pass `--apply` to delete. An orphan is a
    /// land_registry/CCOD record whose owner GB-COH company is absent from graph_events.
    PurgeOrphans(commands::purge::PurgeArgs),
    /// Retire superseded GB-OCOD: owner nodes + ownership edges from graph_events (INT-03/D-14)
    ///
    /// Dry-run by default (counts only); pass `--apply` to delete, which requires typing back
    /// the run-derived confirmation digest. graph_events is append-only, so these superseded
    /// events must be retired BEFORE `enrich --source land_registry_ocod` re-emits owners under
    /// the corrected key — otherwise the re-run adds correctly-keyed nodes beside the old
    /// fragmented ones instead of merging them. GB-ROE: owners are never in scope.
    PurgeOcodOwners(commands::purge::PurgeOcodOwnersArgs),
    /// Provision and inspect analyses (create, list) — the analysis lifecycle
    Analysis(commands::analysis::AnalysisArgs),
    /// Seed fixture graphs into a --fixture analysis (never a live corpus)
    Fixture(commands::fixture::FixtureArgs),

    /// Run the Tier-1 forensic probe battery against graph_events (PRB-02, Tier-1 slice)
    Probe(commands::probe::ProbeArgs),
    /// Compare every probe's declared population against an independent graph_events count
    /// (ORC-05), and state precisely why the rest cannot be checked (ORC-08)
    ProbeOracle(commands::probe_oracle::ProbeOracleArgs),
    /// Boot a read-only replay snapshot and serve the analyst workbench UI (WBX-01)
    Workbench(commands::workbench::WorkbenchArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialise structured logging (JSON + RUST_LOG filter) once for all commands.
    telemetry::init();
    let cli = Cli::parse();

    // Bridge the global flag to the env layer so every command's AppConfig::load()
    // resolves the same analysis. Edition 2021 → set_var is safe.
    if let Some(ref id) = cli.analysis {
        std::env::set_var("URBAN__ANALYSIS", id);
    }

    // Jurisdiction preflight — once, before dispatch (see the jurisdiction-gate spec).
    // A config whose wards span two nations, or whose deprivation index belongs to a
    // different nation from its wards, produces a plausible-looking analysis that is
    // wrong, so it is refused here rather than warned about mid-run. A config that will
    // not load at all is left to the subcommand, which reports it with its own context.
    if let Ok(config) = urban_integrity_core::config::AppConfig::load() {
        urban_integrity_cli::preflight::jurisdiction_preflight(&config)?.log();
    }

    match cli.command {
        Commands::Report(args) => commands::report::run(args).await?,
        Commands::Alerts(args) => commands::alerts::run(args).await?,
        Commands::Anomalies(args) => commands::anomalies::run(args).await?,
        Commands::Status(args) => commands::status::run(args).await?,
        Commands::Entity(args) => commands::entity::run(args).await?,
        Commands::GraphPath(args) => commands::graph_path::run(args).await?,
        Commands::IngestPlanning(args) => commands::ingest_planning::run(args).await?,
        Commands::Backfill(args) => commands::backfill::run(args).await?,
        Commands::Project(args) => commands::project::run(args).await?,
        Commands::ExportGraphml(args) => commands::export_graphml::run(args).await?,
        Commands::Verify(args) => commands::verify::run(args).await?,
        Commands::Enrich(args) => commands::enrich::run(args).await?,
        Commands::Links(args) => commands::links::run(args).await?,
        Commands::Flips(args) => commands::flips::run(args).await?,
        Commands::Migrate(args) => commands::migrate::run(args).await?,
        Commands::PurgeOrphans(args) => commands::purge::run(args).await?,
        Commands::PurgeOcodOwners(args) => commands::purge::run_ocod_owners(args).await?,
        Commands::Analysis(args) => commands::analysis::run(args).await?,
        Commands::Fixture(args) => commands::fixture::run(args).await?,
        Commands::Probe(args) => commands::probe::run(args).await?,
        Commands::ProbeOracle(args) => commands::probe_oracle::run(args).await?,
        Commands::Workbench(args) => commands::workbench::run(args).await?,
        Commands::Serve => {
            use std::sync::Arc;
            use urban_integrity_api::{start_server, ServerConfig};
            use urban_integrity_cli::db::connect_db;
            use urban_integrity_core::{
                config::AppConfig,
                graph::coordinator::{CoordinatorConfig, GraphCoordinator},
                persistence::wal::wal_channel,
                persistence::{
                    count_graph_events, replay_from_surreal, surreal_writer_loop,
                    SurrealWriterConfig,
                },
            };
            use urban_integrity_detector::DetectorEngine;
            use urban_integrity_ingestor::SurrealCursorStore;

            let config = AppConfig::load().expect("Config load failed — ensure config.toml exists");
            let (wal_tx, wal_rx) = wal_channel(config.graph.wal_channel_capacity);
            let coord_cfg = CoordinatorConfig {
                snapshot_every_n_events: config.graph.snapshot_every_n_events,
                snapshot_interval_secs: config.graph.snapshot_interval_secs,
            };

            // Connect to SurrealDB once — shared by engine, API, and CH cursor
            let db = connect_db(&config)
                .await
                .expect("SurrealDB connection failed — check config.toml [surreal] section");
            let db = Arc::new(db);
            // Resolved ONCE, and handed to every re-auth below. Re-signing in resets the
            // connection's database selection, so the refresh has to restore the analysis this
            // process actually connected to rather than whatever config.toml names.
            let serve_db_name = urban_integrity_cli::db::resolve_analysis_db(&config)
                .await
                .expect("analysis resolution failed");

            // Read the detector settings from config rather than hardcoding them.
            //
            // These three were literals — `phoenix_window_days: 730`, `imd_csv_path: None` —
            // so `serve`, the ONLY process that runs detection, ignored the entire
            // `[detector]` block of `config.toml`. Two consequences, both live:
            //
            //  * Phoenix ran a **730-day** window while config, `PhoenixConfig::default()`
            //    and every doc said 180. On 2026-08-11 that emitted an alert against a pair
            //    190 days apart (planning open item 26) — inside 730, four times outside the
            //    configured policy. Every Phoenix alert this project has ever produced used
            //    730.
            //  * `imd_csv_path: None` disabled IMD weighting outright, so `m_env` was pinned
            //    to 1.0 and `imd_rank` to null on every alert — contradicting the
            //    load-bearing rule that `P_c` is IMD-weighted (docs/ARCHITECTURE.md, Deprivation Weighting).
            //
            // Defaults here match `DetectorConfigRaw::default()`, so an absent block behaves
            // as documented instead of silently widening the window.
            let detector =
                DetectorEngine::new(urban_integrity_cli::detector_config_from(&config.detector));

            let (coordinator, handle) = GraphCoordinator::new_with_db(
                coord_cfg,
                wal_tx,
                Some(Arc::new(detector)),
                Some(db.clone()),
            );
            let handle = Arc::new(handle);

            // Replay existing graph_events into the in-memory coordinator so the detector
            // sees historical planning and CF data, not just new mutations from this session.
            let replayed = match replay_from_surreal(&db, &handle.storage).await {
                Ok(n) => n,
                Err(e) => {
                    // A replay failure must never silently masquerade as an empty graph —
                    // that disables ALL historical detection while the engine looks healthy.
                    // Only a genuinely empty graph_events table may start empty; if events
                    // exist, fail loudly rather than run blind.
                    // If the count itself fails we CANNOT prove the table is empty — and a
                    // replay failure plus a count failure is the correlated case (DB wedged).
                    // `unwrap_or(0)` here vouched for emptiness on exactly the evidence that
                    // could not support it, silently starting a blind engine. Refuse instead.
                    let existing = count_graph_events(&db).await.unwrap_or_else(|count_err| {
                        panic!(
                            "Graph replay failed AND the graph_events count failed — cannot \
                             prove the table is empty, refusing to start with an empty graph. \
                             Replay error: {e}. Count error: {count_err}"
                        )
                    });
                    if existing > 0 {
                        panic!(
                            "Graph replay failed with {existing} graph_events present — refusing \
                             to start with an empty graph (historical detection would be silently \
                             disabled). Error: {e}"
                        );
                    }
                    tracing::warn!(
                        "Graph replay failed on an empty graph_events table — starting empty: {e}"
                    );
                    0
                }
            };
            tracing::info!(
                replayed,
                "Historical graph_events replayed into coordinator"
            );

            tokio::spawn(coordinator.run());

            // WAL → SurrealDB writer: persists every NodeAdded/EdgeAdded/MotifDetected
            // event from the coordinator to graph_events so CLI queries and analyst_probe
            // see all data, not just what was written by ingest-planning.
            {
                let writer_db = db.clone();
                let writer_config = config.clone();
                let writer_db_name = serve_db_name.clone();
                tokio::spawn(surreal_writer_loop(
                    wal_rx,
                    SurrealWriterConfig::default(),
                    move |mut record| {
                        let db = writer_db.clone();
                        let cfg = writer_config.clone();
                        let reauth_target = writer_db_name.clone();
                        async move {
                            // D-08: Build deterministic record ID from event kind + src/dst.
                            // Shared ID builder from wal_writer.rs ensures no cross-path duplicates
                            // between serve-path and CLI enrich WAL writes.
                            let (src_id, dst_id) = match record.kind.as_str() {
                                "NodeAdded" | "NodeRemoved" | "NodeUpdated" => {
                                    let id = record.payload["entity_id"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    (id.clone(), id)
                                }
                                "EdgeAdded" => {
                                    let src =
                                        record.payload["src_id"].as_str().unwrap_or("").to_string();
                                    let dst =
                                        record.payload["dst_id"].as_str().unwrap_or("").to_string();
                                    (src, dst)
                                }
                                "MotifDetected" => {
                                    // Open Item 6d: key on `unique_hash`, NOT the whole
                                    // `result_json`.
                                    //
                                    // `result_json` carries `last_seen_at` and
                                    // `detected_at_event`, which change on every snapshot bake, so
                                    // using it as the id component made a fresh record id — and
                                    // therefore a fresh row — every time a still-matching cluster
                                    // was re-detected. The deterministic-id collision that gives
                                    // the other event kinds their D-07 immutability skip never
                                    // fired for alerts. Measured 2026-07-28: 17,549 rows for 1,374
                                    // distinct clusters (92.2% duplicates), one cluster written 14
                                    // times — exactly the number of bakes. Every raw alert count
                                    // this project has reported was inflated by `serve` runtime.
                                    //
                                    // Falls back to the old behaviour if the hash is missing:
                                    // duplicate rows are recoverable, whereas collapsing every
                                    // alert onto one constant id would silently keep only one.
                                    let rj = record.payload["result_json"].as_str();
                                    let src = rj
                                        .and_then(|s| {
                                            serde_json::from_str::<serde_json::Value>(s).ok()
                                        })
                                        .and_then(|v| {
                                            v.get("unique_hash")
                                                .map(std::string::ToString::to_string)
                                        })
                                        .unwrap_or_else(|| {
                                            tracing::warn!(
                                                "MotifDetected payload has no unique_hash — \
                                                 falling back to result_json-keyed id, which \
                                                 duplicates per bake"
                                            );
                                            rj.unwrap_or("motif").to_string()
                                        });
                                    (src, String::new())
                                }
                                _ => (String::new(), String::new()),
                            };
                            let record_id =
                                urban_integrity_enricher::wal_writer::build_wal_record_id(
                                    &record.kind,
                                    &src_id,
                                    &dst_id,
                                );

                            // Open Item 6d: alerts UPSERT, every other kind CREATEs.
                            //
                            // Keying the id on `unique_hash` alone is not sufficient, because
                            // `AddressCluster` hashes only the location
                            // (`cluster.rs`: `motif_hash(&[loc_id])`), not its participants. Under
                            // `CREATE` the first version of a cluster would win permanently, so an
                            // address that later gained a fourth applicant would keep a stale
                            // three-participant snapshot and a stale risk score — silently wrong,
                            // which is a worse failure than the duplication being fixed here.
                            //
                            // `MotifResult` carries `unique_hash`, `version` and `last_seen_at`:
                            // it was designed to be updated in place. Immutability (D-07) still
                            // holds for the actual event kinds — `NodeAdded`/`EdgeAdded` remain
                            // CREATE-with-AlreadyExists-skip. A motif is a derived assertion about
                            // current state, not a historical fact.

                            // Phase 46 (D-06): first_seen_run_id / last_seen_run_id merge — a
                            // read-before-write, only for MotifDetected (every other kind's write
                            // path is untouched). See `motif_run_attribution.rs` for the full
                            // rationale; extracted there (rather than inlined here) so the merge
                            // decision itself is directly testable against a real SurrealDB.
                            if record.kind == "MotifDetected" {
                                if let Some(result_json_str) = record
                                    .payload
                                    .get("result_json")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string)
                                {
                                    let merged =
                                        urban_integrity_cli::motif_run_attribution::merge_first_seen_last_seen_run_id(
                                            &db,
                                            &record_id,
                                            &result_json_str,
                                        )
                                        .await;
                                    record.payload["result_json"] = serde_json::json!(merged);
                                }
                            }

                            let write_query = if record.kind == "MotifDetected" {
                                "UPSERT type::record('graph_events', $id) CONTENT $r"
                            } else {
                                "CREATE type::record('graph_events', $id) CONTENT $r"
                            };
                            let r = serde_json::to_value(&record).unwrap_or_default();

                            let result = db
                                .query(write_query)
                                .bind(("id", record_id.clone()))
                                .bind(("r", r.clone()))
                                .await
                                // T-24-07: consume the Response. `query().await` returns
                                // `Ok(Response)` whenever the transport succeeded; statement-level
                                // failures (schema rejection, the 3.x datetime-coercion drop) live
                                // inside it and surface only via `.check()`/`.take()`. Mapping it
                                // away with `.map(|_| ())` turned every such failure into success —
                                // the ICIJ shape, on this same CREATE ... CONTENT construction.
                                .and_then(|mut resp| {
                                    resp.take::<Vec<serde_json::Value>>(0).map(|_| ())
                                })
                                .map_err(|e| {
                                    let s = e.to_string();
                                    // D-07: AlreadyExists is success — WAL is immutable, skip.
                                    if s.contains("already exists") || s.contains("AlreadyExists") {
                                        String::new() // sentinel for Ok
                                    } else {
                                        s
                                    }
                                });

                            // Re-auth on 401: SurrealDB HTTP session tokens expire after
                            // ~1 hour. Detect the 401 by string, re-signin, then retry once.
                            //
                            // reauth_db returns `Ok(Some(new_db))` when `db`'s token had
                            // genuinely, fully expired — in that case `db` itself is
                            // unrecoverable (no request through it can succeed, see
                            // reauth_db's doc comment) and the retry MUST go through the
                            // fresh connection instead. `Ok(None)` means the ordinary
                            // in-place re-signin worked, so `db` (shared via the same `Arc`
                            // as the periodic refresh task and every other WAL write) is
                            // fine to retry against directly, exactly as before.
                            if let Err(ref e) = result {
                                if e.is_empty() {
                                    // AlreadyExists sentinel — treat as Ok
                                    return Ok(());
                                }
                                if e.contains("401") || e.contains("Unauthorized") {
                                    tracing::warn!("SurrealDB 401 — re-authenticating session");
                                    if let Ok(reauth_result) = urban_integrity_cli::db::reauth_db(
                                        &db,
                                        &cfg,
                                        &reauth_target,
                                    )
                                    .await
                                    {
                                        let retry = match &reauth_result {
                                            Some(new_db) => {
                                                new_db
                                                    .query(write_query)
                                                    .bind(("id", record_id))
                                                    .bind(("r", r))
                                                    .await
                                            }
                                            None => {
                                                db.query(write_query)
                                                    .bind(("id", record_id))
                                                    .bind(("r", r))
                                                    .await
                                            }
                                        };
                                        return retry
                                            // T-24-07: same Response consumption as the first
                                            // attempt — otherwise a re-authenticated retry
                                            // silently reinstates the swallow-failures path.
                                            .and_then(|mut resp| {
                                                resp.take::<Vec<serde_json::Value>>(0).map(|_| ())
                                            })
                                            .map_err(|e| {
                                                let s = e.to_string();
                                                if s.contains("already exists")
                                                    || s.contains("AlreadyExists")
                                                {
                                                    String::new()
                                                } else {
                                                    s
                                                }
                                            });
                                    }
                                }
                            }
                            result
                        }
                    },
                ));
            }

            // Periodic re-auth: belt-and-suspenders against the ~1-hour SurrealDB HTTP
            // session expiry. Re-signs in every 50 minutes so the token is always fresh
            // before the write_fn's inline re-auth would ever be needed.
            {
                let mut reauth_db = db.clone();
                let reauth_config = config.clone();
                let reauth_target = serve_db_name.clone();
                tokio::spawn(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(50 * 60));
                    interval.tick().await; // skip the immediate first tick
                    loop {
                        interval.tick().await;
                        match urban_integrity_cli::db::reauth_db(
                            &reauth_db,
                            &reauth_config,
                            &reauth_target,
                        )
                        .await
                        {
                            Ok(None) => {
                                tracing::info!("SurrealDB session refreshed (periodic re-auth)")
                            }
                            Ok(Some(new_db)) => {
                                // The token had already fully expired by this tick — no
                                // request through the old `reauth_db` handle could recover it
                                // (see reauth_db's doc comment). Swap this task's own clone to
                                // the fresh connection so its NEXT tick refreshes successfully
                                // instead of repeating the same failure every 50 minutes.
                                //
                                // This does not repair the shared `db` Arc that the WAL writer
                                // and CH/enricher clones hold — a fresh connection here cannot
                                // be transplanted into their handles. Each of those self-heals
                                // independently the next time it hits a 401 and calls
                                // reauth_db itself (see the WAL writer's inline retry above).
                                tracing::warn!(
                                    "SurrealDB periodic re-auth: token had fully expired — \
                                     reconnected this task's own session; other long-lived \
                                     handles will self-heal on their next 401"
                                );
                                reauth_db = std::sync::Arc::new(new_db);
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "SurrealDB periodic re-auth failed")
                            }
                        }
                    }
                });
            }

            // CH streaming — started when a non-empty API key is present.
            // config.toml may carry ch_api_key = "" as a placeholder; treat that
            // the same as absent so URBAN__INGEST__CH_API_KEY env override works cleanly.
            let ch_api_key = config
                .ingest
                .ch_api_key
                .as_deref()
                .filter(|k| !k.is_empty())
                .map(|k| k.to_string());
            if let Some(api_key) = ch_api_key {
                use urban_integrity_ingestor::ch::client::ChStreamClient;
                let ch_cursor = SurrealCursorStore::new(db.clone());
                let ch_client = ChStreamClient::new(api_key.clone(), (*handle).clone(), ch_cursor);
                tokio::spawn(async move { ch_client.run().await });

                // Officer appointments are a SEPARATE CH stream. Subscribing to `/companies`
                // alone produced a graph with zero Officer nodes, which silently disabled
                // Phoenix motif detection, `ch_disqualified` (candidates_found: 0 on every
                // analysis), and `ch_psc`'s officer matching. Its own cursor record — sharing
                // one would make each stream resume from the other's timepoint.
                let officer_cursor = SurrealCursorStore::new_officers(db.clone());
                let officer_client =
                    ChStreamClient::new_officers(api_key, (*handle).clone(), officer_cursor);
                tokio::spawn(async move { officer_client.run().await });

                tracing::info!("CH streaming started — company profiles and officer appointments");
            } else {
                tracing::warn!(
                    "URBAN__INGEST__CH_API_KEY not set or empty — CH streaming disabled"
                );
            }

            // Contracts Finder polling (public API, no key required)
            {
                use urban_integrity_ingestor::cf::client::{CfPoller, InMemoryCfCursor};
                let cf_poller = CfPoller::new(
                    (*handle).clone(),
                    InMemoryCfCursor::new(),
                    config.ingest.cf_poll_interval_secs,
                );
                tracing::info!(
                    interval_secs = config.ingest.cf_poll_interval_secs,
                    "Contracts Finder poller started"
                );
                tokio::spawn(async move { cf_poller.run().await });
            }

            // Planning portal background poller (INPP-01: periodic scraping of configured wards)
            if !config.ingest.planning_portals.is_empty() {
                use urban_integrity_ingestor::planning::{
                    poller::PlanningPoller, sidecar::ScrapingSidecar,
                };
                let planning_sidecar = ScrapingSidecar::with_timeout(
                    &config.ingest.scrapling_url,
                    config.ingest.scrapling_port,
                    config.ingest.scrapling_timeout_secs,
                );
                let planning_poller = PlanningPoller::new(
                    planning_sidecar,
                    (*handle).clone(),
                    config.ingest.planning_portals.clone(),
                    config.ingest.planning_poll_interval_secs,
                );
                tracing::info!(
                    portals = config.ingest.planning_portals.len(),
                    interval_secs = config.ingest.planning_poll_interval_secs,
                    "Planning portal poller started"
                );
                tokio::spawn(async move { planning_poller.run().await });
            } else {
                tracing::info!("No planning_portals configured — planning poller disabled");
            }

            // EnricherEngine background task (D-01, D-04: sleep-first scheduling).
            // Phase 17–18: PscAdapter, DisqualifiedAdapter, LandRegistryCcodAdapter, LondonGazetteAdapter.
            // Adapters degrade gracefully when their API key is absent (CHA-03).
            {
                use urban_integrity_enricher::engine::{build_default_adapters, EnricherEngine};
                let adapters = build_default_adapters(&config.enricher, std::sync::Arc::clone(&db));
                let adapter_count = adapters.len();
                let enricher = Arc::new(EnricherEngine::new(
                    (*db).clone(),
                    handle.mutation_tx.clone(),
                    config.enricher.clone(),
                    config.surreal.clone(),
                    adapters,
                    config.analysis_nation()?,
                ));
                tracing::info!(
                    interval_secs = config.enricher.enrich_interval_secs,
                    adapters = adapter_count,
                    "EnricherEngine background task started (Phase 17–18 adapters registered)"
                );
                // WR-06: store the handle so the task can be aborted on shutdown.
                // start_server below runs until SIGINT/SIGTERM; on return we abort
                // the enricher to avoid spurious errors against a closing DB connection.
                let enricher_handle = enricher.start_background().await;

                let server_config = ServerConfig::default();
                start_server(handle, server_config).await?;

                // Shutdown: cancel the background enricher task.
                enricher_handle.abort();
            }
        }
    }

    Ok(())
}
