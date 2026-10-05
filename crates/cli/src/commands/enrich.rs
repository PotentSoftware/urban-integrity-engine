use anyhow::Context as _;
use std::sync::Arc;

/// Arguments for the `enrich` subcommand.
#[derive(Debug, clap::Args)]
pub struct EnrichArgs {
    /// Adapter to run: specific adapter name or 'all' for all enabled adapters (D-05)
    #[arg(long, default_value = "all")]
    pub source: String,
}

pub async fn run(args: EnrichArgs) -> anyhow::Result<()> {
    use crate::db::connect_db;
    use urban_integrity_core::config::AppConfig;
    use urban_integrity_enricher::engine::{build_default_adapters, EnricherEngine};

    let config = AppConfig::load().context("Config load failed")?;
    let db = connect_db(&config).await?;

    // Re-auth (engine + drain) must re-select the RESOLVED analysis database,
    // not the config default — otherwise an `--analysis` run silently switches
    // back to `main` at the first hourly refresh.
    let mut surreal_config = config.surreal.clone();
    surreal_config.database = crate::db::resolve_analysis_db(&config).await?;

    // Phase 21: channel receiver is kept alive in a drain task (ACT-01, ACT-02).
    // The drain task reads GraphMutation events and writes graph_events WAL rows directly.
    // No GraphCoordinator or hot graph in the CLI path (D-01).
    //
    // Note: if `serve` is running concurrently, the hot graph will be stale until the
    // next `project` run — this is accepted and expected behaviour (D-05).
    let (mutation_tx, mutation_rx) =
        tokio::sync::mpsc::channel::<urban_integrity_core::graph::types::GraphMutation>(1024);

    // Clone db for the drain task; the engine keeps the original db. The clone
    // is a NEW session (independent auth token), so it gets its own periodic
    // re-auth below — Arc-shared so the refresh hits the very session the drain
    // task writes with. Without this, a run longer than the token TTL loses
    // every mutation drained after expiry (bit the 10h ICIJ run: EC's
    // political_actor/donates_to events all died with 401 at the end).
    let drain_db = Arc::new(db.clone());
    let drain_handle = tokio::spawn(urban_integrity_enricher::wal_writer::drain_to_wal(
        mutation_rx,
        Arc::clone(&drain_db),
    ));

    // Clone sender for the engine; we retain mutation_tx to drop after the run.
    let engine_tx = mutation_tx.clone();

    let adapters = build_default_adapters(&config.enricher, std::sync::Arc::new(db.clone()));
    // Sources that have no data for this analysis's nation are refused rather than run —
    // HMLR CCOD in particular would otherwise return English property for a Scottish
    // analysis's companies (see the jurisdiction-gate spec).
    let analysis_nation = config.analysis_nation()?;
    let engine = Arc::new(EnricherEngine::new(
        db,
        engine_tx,
        config.enricher.clone(),
        surreal_config.clone(),
        adapters,
        analysis_nation,
    ));

    // Periodic re-auth on the engine's own session — Surreal::clone() creates an
    // independent session (new UUID), so reauthing a clone does NOT refresh the
    // engine's self.db. engine.reauth() operates on self.db directly.
    // Keep the handle so we can abort the task after the run completes,
    // allowing the engine Arc to drop and the drain channel to close cleanly.
    let reauth_handle = {
        let engine_for_reauth = Arc::clone(&engine);
        let drain_db_for_reauth = Arc::clone(&drain_db);
        let drain_cfg = surreal_config.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(50 * 60));
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                match engine_for_reauth.reauth().await {
                    Ok(()) => {
                        tracing::info!("SurrealDB session refreshed (enricher periodic re-auth)")
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "SurrealDB enricher periodic re-auth failed")
                    }
                }
                match reauth_session(&drain_db_for_reauth, &drain_cfg).await {
                    Ok(()) => {
                        tracing::info!("SurrealDB session refreshed (drain periodic re-auth)")
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "SurrealDB drain periodic re-auth failed")
                    }
                }
            }
        })
    };

    // FLIP-02: the land-flip detector is a post-adapter detector, not an adapter — it
    // writes land_flip_candidates directly and emits no graph mutations.
    let land_flip = args.source == "land-flip";
    let stats = if args.source == "all" {
        engine.run_all_enabled().await?
    } else if land_flip {
        let lf = engine.detect_land_flips().await?;
        println!(
            "Land-flip detection complete: {} applications scanned, {} candidates written to review queue.",
            lf.applications_scanned, lf.candidates_written
        );
        // Open Item 16: always print WHERE the applications went. A bare "0 candidates" reads
        // identically whether gate-1 matched nothing, the anchor date was missing, the window
        // rejected every transfer, or corroboration was absent — four problems, four fixes.
        println!(
            "  gate-1 no PPD match      : {}{}\n  \
               gate-3 no corroboration  : {}\n  \
               gate-2 no anchor date    : {}\n  \
               gate-2 outside window    : {}\n  \
               produced candidates      : {} application(s)",
            lf.gate1_no_ppd_match,
            if lf.postcode_only_matches > 0 {
                format!(
                    " (of which {} matched on postcode alone)",
                    lf.postcode_only_matches
                )
            } else {
                String::new()
            },
            lf.gate3_no_corroboration,
            lf.gate2_no_anchor_date,
            lf.gate2_outside_window,
            lf.candidate_apps,
        );
        if lf.gate3_query_failed > 0 {
            println!(
                "  WARNING: {} application(s) had a FAILED gate-3 query — results are incomplete, \
                 not merely negative.",
                lf.gate3_query_failed
            );
        }
        urban_integrity_enricher::adapters::EnrichmentStats::default()
    } else {
        engine.run_source(&args.source).await?
    };

    // All adapters have finished sending mutations. Signal the drain channel to close:
    // 1. Abort the reauth background task (it holds an Arc<EnricherEngine> clone).
    reauth_handle.abort();
    // 2. Drop our local guard sender (mutation_tx).
    drop(mutation_tx);
    // 3. Drop the engine Arc — releases the engine_tx sender held by EnricherEngine.
    //    After this, all senders are dropped and the drain task will see channel close.
    drop(engine);

    // Await the drain task. Double-? unwraps JoinError (outer) and anyhow::Result (inner).
    // Fail-fast: any WAL write failure propagates as a non-zero CLI exit (D-04).
    drain_handle.await.context("drain task panicked")??;

    if !land_flip {
        println!(
            "Enrichment complete: {} candidates, {} auto-linked, {} pending review.",
            stats.candidates_found, stats.auto_linked, stats.pending_review
        );
    }

    Ok(())
}

/// Re-sign-in + re-select ns/db on the drain task's session. Mirrors
/// `EnricherEngine::reauth` but targets the drain's independent session
/// (`Surreal::clone()` sessions do not share auth state).
async fn reauth_session(
    db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    cfg: &urban_integrity_core::config::SurrealConfig,
) -> anyhow::Result<()> {
    if let (Some(user), Some(pass)) = (cfg.username.as_deref(), cfg.password.as_deref()) {
        db.signin(surrealdb::opt::auth::Root {
            username: user.to_string(),
            password: pass.to_string(),
        })
        .await
        .context("drain DB re-signin failed")?;
        db.use_ns(&cfg.namespace)
            .use_db(&cfg.database)
            .await
            .context("drain DB ns/db re-selection failed")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Regression test: the channel receiver is now held alive by the drain task (Phase 21 fix).
    ///
    /// Previously, an anonymous binding `_` dropped the receiver immediately, causing
    /// all adapter sends to return `Err(SendError)` instantly (not a deadlock, but silently
    /// discarding all mutations). The fix (Phase 21, ACT-01) keeps the receiver alive
    /// in `drain_to_wal` — channel sends succeed for the lifetime of the drain task.
    ///
    /// This test verifies the NEW behaviour: with a live receiver, sends succeed up to
    /// the buffer capacity and beyond (via async back-pressure). We verify that a send
    /// on a channel with a live receiver returns `Ok(())`.
    #[tokio::test]
    async fn test_mutation_channel_drain_task_keeps_receiver_alive() {
        // Channel capacity = 2
        let (tx, rx) =
            tokio::sync::mpsc::channel::<urban_integrity_core::graph::types::GraphMutation>(2);

        // Spawn a drain task that reads and discards all mutations (no DB needed for this test).
        let drain_handle = tokio::spawn(async move {
            let mut _rx = rx;
            while let Some(_msg) = _rx.recv().await {
                // discard — just drain the channel
            }
        });

        // With a live receiver, sends should succeed (Ok) not immediately error.
        let send_result = tx
            .send(
                urban_integrity_core::graph::types::GraphMutation::RemoveNode {
                    id: urban_integrity_schema::types::id::EntityId("test".to_string()),
                },
            )
            .await;

        assert!(
            send_result.is_ok(),
            "send should succeed when drain task holds the receiver"
        );

        // Clean up: drop sender and await drain task.
        drop(tx);
        let _ = drain_handle.await;
    }
}
