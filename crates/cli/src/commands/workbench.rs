//! `workbench` subcommand: boots the replay-only fraud analyst workbench engine and serves its
//! UI (WBX-01, WBX-02, WBX-06, WBX-08, D-06, D-07).
//!
//! This is the operator-facing entry point WBX-01/WBX-02 actually verify against — this is where
//! `connect_db`/`resolve_analysis_db`/`list_analyses` legitimately live (per plan 42-05's
//! circular-dependency note: `crates/workbench` cannot depend on `crates/cli`, so it takes an
//! already-connected `db`/`db_name`/`analyses` and never calls any of these itself). This
//! command resolves them exactly as `Commands::Serve` does, computes the canonical population
//! via `export_graphml::canonical_population_summary` (D-19), fetches the `_catalog` analysis
//! list, and hands everything to `urban_integrity_workbench::boot::boot`.

use anyhow::Result;
use urban_integrity_core::config::AppConfig;
use urban_integrity_workbench::boot::{
    bind_listener, boot, AnalysisCatalogEntry, BootParams, PopulationSummary,
};

use crate::commands::export_graphml::{canonical_population_summary, CanonicalPopulation};
use crate::db::{connect_db, resolve_analysis_db};
use crate::registry::{connect_catalog, list_analyses};

#[derive(Debug, clap::Args)]
pub struct WorkbenchArgs {
    /// Override the [workbench] bind address (config.toml / URBAN__WORKBENCH__BIND_ADDR),
    /// e.g. "127.0.0.1:8090". CLI flag wins over config/env, standard clap precedence.
    #[arg(long)]
    pub bind: Option<String>,

    /// Open the workbench URL in the operator's default browser once the listener is bound.
    /// Off by default (D-07): the workbench never auto-opens a browser unless this is passed.
    #[arg(long)]
    pub open: bool,
}

/// Loads config, binds the HTTP listener FIRST (D-05: a taken port is a fail-loud error — and
/// it must fail in milliseconds, not after a multi-minute corpus replay), prints the workbench
/// URL (D-07: URL only — no other startup noise), then resolves the analysis database exactly
/// as `Commands::Serve` does, computes the canonical population (D-19), fetches the `_catalog`
/// analysis list, and boots the replay-only engine on the already-bound listener.
pub async fn run(args: WorkbenchArgs) -> Result<()> {
    let mut config = AppConfig::load()?;
    if let Some(bind) = args.bind {
        config.workbench.bind_addr = bind;
    }
    let bind_addr = config.workbench.bind_addr.clone();

    // Bind before ANY SurrealDB round-trip. Everything below this line — connect, the
    // canonical-population load, the `_catalog` fetch, and boot()'s own replay — costs minutes
    // on a live analysis; a port conflict discovered only after all of that is the D-05
    // failure mode this ordering exists to prevent. Once bound, connections to the printed URL
    // queue in the kernel backlog until boot() reaches `axum::serve`, so an early browser tab
    // waits rather than getting connection-refused for the whole replay.
    let listener = bind_listener(&bind_addr).await?;

    // D-07: print the URL only — and only now that the socket it names actually exists.
    println!("Workbench: http://{bind_addr}");

    if args.open {
        let url = format!("http://{bind_addr}");
        open_browser(&url);
    }

    // Connect to the resolved analysis database — identical call sites to Commands::Serve
    // (crates/cli/src/main.rs), so a `workbench --analysis <slug>` targets the same database
    // `serve --analysis <slug>` would.
    let db = connect_db(&config).await?;
    let db = std::sync::Arc::new(db);
    let db_name = resolve_analysis_db(&config).await?;

    // `_catalog` analysis list — fetched BEFORE the population so the target analysis's own
    // registry row can seed the reachability walk below.
    let catalog = connect_catalog(&config).await?;
    let rows = list_analyses(&catalog).await?;

    // The ward codes that seed the canonical population are the TARGET analysis's own (its
    // `_catalog` row's `ward_codes`, recorded at provisioning) — not config.toml's, which
    // describe whatever analysis the operator last pointed the ingestors at. Only the `main` /
    // uncatalogued path has no row, and falls back to config wards inside
    // `canonical_population_summary`. Seeding from config unconditionally made
    // `workbench --analysis <other>` (the exact command the picker discloses) walk the wrong
    // wards and refuse to boot whenever none of them existed in the target graph.
    let analysis_ward_codes: Option<Vec<String>> = rows
        .iter()
        .find(|row| row.db_name == db_name)
        .map(|row| row.ward_codes.clone());

    // Canonical population (D-19, WBX-06): the same load_from_events -> ward seeds ->
    // filter_reachable pipeline export-graphml itself uses — never a fabricated or
    // independently re-derived number. Read from the RESOLVED `db_name`, not
    // `config.surreal.database` (which under `--analysis` still names the default database).
    // Zero resolvable seeds is a typed "population unavailable" state the status page renders
    // honestly, not a boot failure — the graph is still served.
    //
    // KNOWN TWO-READ SHAPE (Phase 42 code review, IN-09): this is the FIRST of two full reads
    // of `graph_events` at boot. `canonical_population_summary` loads the whole table into its
    // own petgraph (over raw HTTP `/sql`) to walk ward reachability; `boot()` then replays the
    // same table a second time (`replay_from_surreal`, over the SDK) into the coordinator's
    // storage. The two reads are therefore stamped separately — `population.generated_at`
    // below, `replayed_at` further down — and can differ by the duration of the second read
    // (minutes on a live analysis). Both timestamps travel to the status page individually
    // ("graph as of" vs "population generated") precisely so that gap is visible, not hidden.
    // Collapsing this into one read means deriving the population from the coordinator's
    // replayed `handle.storage` inside `crates/workbench` — which needs the ward-reachability
    // pipeline extracted out of `crates/cli` first (crates/workbench cannot depend on cli; see
    // docs/CANONICAL-POPULATION.md's "Scope note", the Phase 46+ refactor target). Not done here.
    let population =
        match canonical_population_summary(&config, &db_name, analysis_ward_codes.as_deref())
            .await?
        {
            CanonicalPopulation::Available {
                count,
                generated_at,
                ward_seed_count,
            } => PopulationSummary::Available {
                count,
                generated_at,
                ward_seed_count,
            },
            CanonicalPopulation::Unavailable {
                reason,
                generated_at,
                ward_seed_count,
            } => {
                tracing::warn!(
                    %reason,
                    "workbench: canonical population unavailable for this analysis — the \
                     status page will say so rather than show a fabricated count"
                );
                PopulationSummary::Unavailable {
                    reason,
                    generated_at,
                    ward_seed_count,
                }
            }
        };

    // Mapped into the workbench crate's own (non-cli) entry type — crates/workbench cannot
    // depend on crates/cli::registry::AnalysisRow (circular dependency). Retired/retiring rows
    // are dropped: `resolve_analysis_db` refuses to operate on them, so the picker must not
    // advertise a `workbench --analysis <retired>` restart command that cannot work. (The bound
    // analysis itself can never be one of these — resolution already rejected it.)
    let analyses = rows
        .into_iter()
        .filter(|row| row.status != "retired" && row.status != "retiring")
        .map(|row| AnalysisCatalogEntry {
            db_name: row.db_name,
            label: row.label,
        })
        .collect();

    // Captured just before boot() performs the actual replay+bake — the CLI-side "graph as of"
    // timestamp the status handler reports (D-06). This is the SECOND read's stamp (see the
    // two-read note above): it is taken at the START of the replay, and the interval from here
    // to the bake completing is the replay itself — sub-second on a test fixture, minutes on a
    // live analysis — during which the bound socket is already accepting (queued) connections.
    let replayed_at = chrono::Utc::now();

    // `catalog`/`oc_gates` — the same two inputs `crates/cli/src/commands/probe.rs::execute`
    // threads into `urban_integrity_probe::run::execute` — so the workbench's in-process battery
    // (Phase 43 plan 05, D-15) runs the identical path the CLI `probe` subcommand uses. `catalog`
    // was already connected above (for the `_catalog` analysis list) and is not consumed by
    // anything else at this point.
    let oc_gates = urban_integrity_probe::run::OcProbeGates::from_config(&config);

    boot(
        BootParams {
            config,
            db,
            db_name,
            population,
            replayed_at,
            analyses,
            catalog,
            oc_gates,
            // The same variable already computed above (line ~82) to seed the canonical
            // population summary — threaded through unchanged into the new BootParams field
            // (plan 44-02) so crate::graph::population_scope's own BFS walk starts from the
            // SAME resolved ward codes, never re-resolving config.toml independently.
            analysis_ward_codes: analysis_ward_codes.clone().unwrap_or_default(),
        },
        listener,
    )
    .await
}

/// Best-effort browser launch for `--open` (D-07: developer convenience only, never
/// correctness-critical — a failure here is logged, not fatal). Called only after
/// `bind_listener` has succeeded, so the browser connects to a bound socket and waits for
/// `boot()` to start serving rather than racing the bind.
fn open_browser(url: &str) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, url, "workbench: --open failed to launch a browser");
    }
}
