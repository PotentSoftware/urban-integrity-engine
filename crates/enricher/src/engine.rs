use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use tokio::sync::mpsc::Sender;
use tracing::warn;
use urban_integrity_core::config::{EnricherConfig, SurrealConfig};
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_schema::nation::{Jurisdiction, Nation};

use crate::adapters::ch_disqualified::DisqualifiedAdapter;
use crate::adapters::ch_psc::PscAdapter;
use crate::adapters::charity_commission::CharityCommissionAdapter;
use crate::adapters::council_registers::CouncilRegistersAdapter;
use crate::adapters::electoral_commission::ElectoralCommissionAdapter;
use crate::adapters::icij::IcijAdapter;
use crate::adapters::land_registry_ccod::LandRegistryCcodAdapter;
use crate::adapters::land_registry_ocod::LandRegistryOcodAdapter;
use crate::adapters::land_registry_ppd::LandRegistryPpdAdapter;
use crate::adapters::london_gazette::LondonGazetteAdapter;
use crate::adapters::mutuals_register::MutualsRegisterAdapter;
use crate::adapters::opencorporates::OpenCorporatesAdapter;
use crate::adapters::opensanctions::OpenSanctionsAdapter;
use crate::adapters::planning_identity::PlanningIdentityAdapter;
use crate::adapters::{EnricherAdapter, EnrichmentStats};
use crate::charity_crossover::run_charity_crossover_detector;
use crate::donation_correlation::{run_donation_correlation_detector, PartyAuthorityMap};
use crate::evasion::run_evasion_detector;
use crate::offshore_shadow::run_offshore_shadow_detector;
use crate::property_accumulation::run_property_accumulation_detector;

/// Every registered source and the nations it covers, for the startup preflight.
///
/// Derived from `build_default_adapters` rather than a parallel table, so a new adapter
/// cannot declare coverage the preflight does not know about. Adapter constructors do no
/// I/O, so the uninitialised handle here is never used.
pub fn source_coverage() -> Vec<(&'static str, Jurisdiction)> {
    let db = Arc::new(surrealdb::Surreal::<surrealdb::engine::any::Any>::init());
    build_default_adapters(&EnricherConfig::default(), db)
        .iter()
        .map(|a| (a.name(), a.coverage()))
        .collect()
}

/// Whether an adapter may run against this analysis.
///
/// An analysis whose nation is unknown (no wards configured) admits everything: refusing
/// every regional source there would break single-source runs that never had a geography.
/// Telling the operator the nation is unknown is the preflight's job, not this gate's.
pub fn adapter_admitted(coverage: Jurisdiction, analysis_nation: Option<Nation>) -> bool {
    match analysis_nation {
        Some(nation) => coverage.covers(nation),
        None => true,
    }
}

/// Build the default adapter list for a given config.
///
/// This is the single registration site for all Phase 17+ adapters.
/// Adapters whose credentials are absent will be registered but return
/// `is_enabled() == false` — the engine's `run_all_enabled()` skips them
/// (graceful degradation, CHA-03).
///
/// Phase 17 adapters: PscAdapter (CHA-01), DisqualifiedAdapter (CHA-02).
/// Additional adapters (Land Registry, etc.) are added in later phases.
pub fn build_default_adapters(
    config: &EnricherConfig,
    db: std::sync::Arc<surrealdb::Surreal<surrealdb::engine::any::Any>>,
) -> Vec<Box<dyn EnricherAdapter>> {
    vec![
        // CHA-01 / CHA-03: PSC beneficial-ownership adapter.
        Box::new(PscAdapter::new(config.ch.api_key.clone())),
        // CHA-02 / CHA-03: Disqualified Directors adapter.
        // Registered after PscAdapter per D-10; evasion post-step runs after this.
        Box::new(DisqualifiedAdapter::new(config.ch.api_key.clone())),
        // PROP-01 / D-03: Land Registry CCOD adapter.
        // Registered always; is_enabled() returns false until api_key is set.
        Box::new(LandRegistryCcodAdapter::new(
            config.land_registry.api_key.clone(),
            config
                .land_registry
                .cache_dir
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("data/ccod")),
            config.land_registry.ccod_refresh_interval_days,
        )),
        // PROP-05 / PROP-06 / OPS-01: Land Registry OCOD overseas-ownership adapter.
        // Registered always; is_enabled() returns false until an HMLR key is present.
        // Effective key: ocod.api_key, falling back to the shared land_registry key
        // when blank (one HMLR account licenses both ccod + ocod — Plan 22-02 RESEARCH Q3).
        Box::new(LandRegistryOcodAdapter::new(
            config
                .ocod
                .api_key
                .clone()
                .filter(|k| !k.trim().is_empty())
                .or_else(|| config.land_registry.api_key.clone()),
            config
                .ocod
                .cache_dir
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("data/ocod")),
            config.ocod.ocod_refresh_interval_days,
        )),
        // PROP-02 / D-06: London Gazette insolvency adapter.
        // Always enabled — uses the public Gazette REST API, no key needed.
        Box::new(LondonGazetteAdapter::new(
            std::sync::Arc::clone(&db),
            config.london_gazette.clone(),
        )),
        // FUZ-02: Charity Commission bulk-download adapter (Wave 2, Plan 19-02).
        // OGL v3 open data — no API key required. Enabled by default.
        Box::new(CharityCommissionAdapter::new(
            config
                .charity_commission
                .cache_dir
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("data/charity")),
            config.charity_commission.refresh_interval_days,
            config.charity_commission.enabled,
        )) as Box<dyn EnricherAdapter>,
        // FUZ-03: OpenSanctions/yente PEP and sanctions screening adapter (Wave 2, Plan 19-03).
        // Requires non_commercial_confirmed = true (D-06 CC-BY-NC compliance gate).
        Box::new(OpenSanctionsAdapter::new(config.opensanctions.clone()))
            as Box<dyn EnricherAdapter>,
        // FUZ-04: ICIJ Offshore Leaks adapter (Wave 2, Plan 19-04).
        // ODbL open data — icij_* table isolation for ODbL/OGL separation (D-09).
        // One-shot idempotent load; skips if icij_offshore_entity already populated (D-10).
        Box::new(IcijAdapter::new(
            config
                .icij
                .cache_dir
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("data/icij")),
            config.icij.enabled,
        )) as Box<dyn EnricherAdapter>,
        // POL-01: Electoral Commission donations adapter (Phase 20).
        // OGL v3 open data — no API key required. Enabled by default.
        Box::new(ElectoralCommissionAdapter::new(
            config.electoral_commission.clone(),
            config
                .electoral_commission
                .cache_dir
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("data/electoral")),
        )) as Box<dyn EnricherAdapter>,
        // POL-02: Council Registers of Interest adapter (Phase 20).
        // Scrapling sidecar required. Disabled by default (no lpa_codes configured).
        Box::new(CouncilRegistersAdapter::new(
            config.council_registers.clone(),
        )) as Box<dyn EnricherAdapter>,
        // POL-03: OpenCorporates cross-jurisdiction adapter (Phase 20).
        // ODbL licence gate (oc_open_data_confirmed) + monthly quota counter.
        // Disabled by default.
        Box::new(OpenCorporatesAdapter::new(config.opencorporates.clone()))
            as Box<dyn EnricherAdapter>,
        // FLIP-01: Land Registry PPD (Price Paid Data) staging adapter (Phase 23).
        // OGL v3 open data — no API key required. Disabled by default (enabled = false).
        // Raw PPD rows are staged into ppd_transactions ONLY — no GraphMutation emitted.
        Box::new(LandRegistryPpdAdapter::new(
            config.ppd.clone(),
            config
                .ppd
                .cache_dir
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("data/ppd")),
        )) as Box<dyn EnricherAdapter>,
        // IDN-05: Planning Identity exact-match join (Phase 24).
        // No external credentials — purely an internal graph join. Always enabled.
        // Open Item 24: FCA Mutuals Public Register. Registered societies are corporate
        // bodies that are NOT companies and NOT on the Charity Commission register, so
        // without this the whole class — housing associations, co-ops, community benefit
        // societies — is invisible to every other adapter. Public bulk CSV, no credential.
        // Registered BEFORE PlanningIdentityAdapter so societies are in the graph when the
        // exact-name join runs in the same pass.
        Box::new(MutualsRegisterAdapter::new(config.mutuals.clone())) as Box<dyn EnricherAdapter>,
        Box::new(PlanningIdentityAdapter::new()) as Box<dyn EnricherAdapter>,
    ]
}

/// Core enrichment engine — orchestrates all OSINT adapters.
///
/// Supports two execution modes (D-01):
/// - Background: `start_background()` runs on a sleep-first interval (D-04)
/// - On-demand: `run_source()` runs a single named adapter synchronously
pub struct EnricherEngine {
    db: surrealdb::Surreal<surrealdb::engine::any::Any>,
    mutation_tx: Sender<GraphMutation>,
    adapters: Vec<Box<dyn EnricherAdapter>>,
    config: EnricherConfig,
    surreal_config: SurrealConfig,
    /// The nation this analysis covers, derived from the configured ward codes.
    /// `None` when no wards are configured — every adapter is then admitted.
    analysis_nation: Option<Nation>,
}

impl EnricherEngine {
    /// Construct a new EnricherEngine.
    pub fn new(
        db: surrealdb::Surreal<surrealdb::engine::any::Any>,
        mutation_tx: Sender<GraphMutation>,
        config: EnricherConfig,
        surreal_config: SurrealConfig,
        adapters: Vec<Box<dyn EnricherAdapter>>,
        analysis_nation: Option<Nation>,
    ) -> Self {
        Self {
            db,
            mutation_tx,
            adapters,
            config,
            surreal_config,
            analysis_nation,
        }
    }

    /// Log and count an adapter refused admission on jurisdiction grounds.
    ///
    /// WARN rather than INFO: a skipped source is a gap in the analysis's evidence base,
    /// and the operator has to know which questions this run cannot answer.
    fn refuse_out_of_jurisdiction(&self, adapter: &dyn EnricherAdapter) {
        warn!(
            adapter = adapter.name(),
            source_coverage = %adapter.coverage(),
            analysis_nation = %self
                .analysis_nation
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            "Adapter SKIPPED — its source has no data for this analysis's nation. This is \
             not a zero result: the source was never asked."
        );
    }

    /// Re-authenticate the engine's own SurrealDB session.
    ///
    /// Must operate on `self.db` directly: a clone's auth lifecycle does not propagate
    /// back to this handle, so reauthenticating a clone would not refresh this instance.
    /// (Verified live by `engine_coverage::test_surreal_clone_invalidate_leaves_original_usable`
    /// — invalidating a clone leaves the original usable, so they do not share auth state.)
    pub async fn reauth(&self) -> anyhow::Result<()> {
        use surrealdb::opt::auth::Root;
        if let (Some(user), Some(pass)) = (
            self.surreal_config.username.as_deref(),
            self.surreal_config.password.as_deref(),
        ) {
            self.db
                .signin(Root {
                    username: user.to_string(),
                    password: pass.to_string(),
                })
                .await
                .map_err(|e| anyhow::anyhow!("enricher DB re-signin failed: {}", e))?;

            self.db
                .use_ns(&self.surreal_config.namespace)
                .use_db(&self.surreal_config.database)
                .await
                .map_err(|e| anyhow::anyhow!("enricher DB ns/db re-selection failed: {}", e))?;
        }
        Ok(())
    }

    /// Start the background enrichment loop.
    ///
    /// D-04: sleeps BEFORE the first run — avoids hammering external sources on restart.
    /// Per-run errors are logged as warnings; the loop continues regardless.
    ///
    /// Returns a `JoinHandle` so the caller can abort the task on shutdown or
    /// join it for testing (WR-06: previously the handle was discarded, making
    /// cancellation impossible and silently swallowing task panics).
    pub async fn start_background(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let interval = Duration::from_secs(self.config.enrich_interval_secs);
        tokio::spawn(async move {
            loop {
                // D-04: sleep first — no startup run
                tokio::time::sleep(interval).await;
                if let Err(e) = self.run_all_enabled().await {
                    warn!(?e, "EnricherEngine background run failed");
                }
            }
        })
    }

    /// Run all enabled adapters and return aggregated stats (D-03).
    ///
    /// Per-adapter errors are logged as warnings and the loop continues —
    /// a single broken adapter never prevents others from running.
    ///
    /// Post-adapter step (D-10): after all adapters complete, the Disqualified
    /// Director Evasion detector runs against the freshly-written identity_links.
    pub async fn run_all_enabled(&self) -> anyhow::Result<EnrichmentStats> {
        let mut total = EnrichmentStats::default();
        for adapter in &self.adapters {
            if !adapter.is_enabled() {
                continue;
            }
            if !adapter_admitted(adapter.coverage(), self.analysis_nation) {
                self.refuse_out_of_jurisdiction(adapter.as_ref());
                total.skipped_out_of_jurisdiction += 1;
                continue;
            }
            match adapter.run(&self.db, &self.mutation_tx).await {
                Ok(stats) => {
                    // quick-260609-mhh: best-effort run marker so the analyst probe
                    // can tell "ran, zero overlap" from "never ran". Never fails the run.
                    persist_run_marker(&self.db, adapter.name(), &stats).await;
                    total.candidates_found += stats.candidates_found;
                    total.auto_linked += stats.auto_linked;
                    total.pending_review += stats.pending_review;
                    total.discarded += stats.discarded;
                    total.skipped_out_of_jurisdiction += stats.skipped_out_of_jurisdiction;
                }
                Err(e) => {
                    warn!(
                        adapter = adapter.name(),
                        ?e,
                        "Adapter run failed — skipping"
                    );
                }
            }
        }

        // 46-05 D-05/D-06: one run_id per `run_all_enabled` invocation, shared across all 5
        // post-adapter detectors below and recorded as a single `detector_runs` history row
        // (`pipeline == "enricher"`) once they've all had a chance to run — regardless of
        // whether any individual detector errored, so "this run happened" is never lost to
        // one detector's failure (T-46-05-02).
        let started_at_ms = Utc::now().timestamp_millis();
        let run_id = uuid::Uuid::new_v4().to_string();
        let mut motif_count: i64 = 0;

        // D-10: post-adapter evasion step — runs after DisqualifiedAdapter completes.
        // Errors are non-fatal (logged as warnings); the engine still returns Ok.
        match run_evasion(self, &run_id).await {
            Ok(count) => motif_count += count as i64,
            Err(e) => warn!(?e, "Evasion detector run failed — non-fatal"),
        }

        // PROP-03: Property Accumulation Anomaly detector.
        match run_property_accumulation(self, &run_id).await {
            Ok(count) => motif_count += count as i64,
            Err(e) => warn!(?e, "Property accumulation detector run failed — non-fatal"),
        }

        // FUZ-05: Offshore Shadow detector — fires when a contract-holder has a
        // confirmed/auto_linked ICIJ offshore identity link (ENR-11 gate).
        match run_offshore_shadow(self, &run_id).await {
            Ok(count) => motif_count += count as i64,
            Err(e) => warn!(?e, "Offshore Shadow detector run failed — non-fatal"),
        }

        // FUZ-06: Charity-Commercial Crossover detector — fires when an officer
        // directs a contract-holder AND is TrusteeOf a charity (ENR-11 gate).
        match run_charity_crossover(self, &run_id).await {
            Ok(count) => motif_count += count as i64,
            Err(e) => warn!(?e, "Charity Crossover detector run failed — non-fatal"),
        }

        // POL-04: Political Donation -> Contract Correlation detector (Phase 20).
        // Fires HIGH when a donation precedes or follows a contract from a party-controlled LPA
        // within 730 days (both directions — revolving-door detection).
        match run_donation_correlation(self, &run_id).await {
            Ok(count) => motif_count += count as i64,
            Err(e) => warn!(?e, "Donation correlation detector run failed — non-fatal"),
        }

        // FLIP-02: Pre-Permission Land Flip detector — writes land_flip_candidates
        // (review queue, NOT graph_events). Three-gate AND-enforcement; non-fatal. Not one of
        // the 5 `pipeline == "enricher"` motif types this run's history row tags (it never
        // writes graph_events), so it stays outside the run_id/motif_count accounting above.
        if let Err(e) = run_land_flip(self).await {
            warn!(?e, "Land-flip detector run failed — non-fatal");
        }

        // Best-effort history row — mirrors persist_run_marker's non-fatal posture. Recorded
        // once all 5 detectors have had a chance to run, unconditionally, so a partially
        // failed run still gets a history row (T-46-05-02).
        let history_record = urban_integrity_core::persistence::detector_runs::DetectorRunRecord {
            run_id: run_id.clone(),
            started_at_ms,
            pipeline: "enricher".to_string(),
            motif_types: vec![
                "property_accumulation_anomaly".to_string(),
                "disqualified_director_evasion".to_string(),
                "offshore_shadow".to_string(),
                "charity_commercial_crossover".to_string(),
                "donation_contract_correlation".to_string(),
            ],
            motif_count,
            imd_loaded: None,
            imd_csv_path: None,
            imd_denominator_declared: None,
        };
        if let Err(e) =
            urban_integrity_core::persistence::detector_runs::record_detector_run_history(
                &self.db,
                history_record,
            )
            .await
        {
            warn!(
                ?e,
                run_id, "EnricherEngine: failed to record detector_runs history row (non-fatal)"
            );
        }

        Ok(total)
    }

    /// Run a single adapter by name.
    ///
    /// Also available as a standalone function for testing.
    ///
    /// If no adapter is registered under `source_name`, logs a warning and returns
    /// empty stats (never errors on missing adapter).
    pub async fn run_source(&self, source_name: &str) -> anyhow::Result<EnrichmentStats> {
        for adapter in &self.adapters {
            if adapter.name() == source_name {
                if !adapter.is_enabled() {
                    warn!(
                        source_name,
                        "Adapter is registered but not enabled — skipping"
                    );
                    return Ok(EnrichmentStats::default());
                }
                if !adapter_admitted(adapter.coverage(), self.analysis_nation) {
                    self.refuse_out_of_jurisdiction(adapter.as_ref());
                    return Ok(EnrichmentStats {
                        skipped_out_of_jurisdiction: 1,
                        ..Default::default()
                    });
                }
                let result = adapter.run(&self.db, &self.mutation_tx).await;
                if let Ok(stats) = &result {
                    // quick-260609-mhh: best-effort run marker on the Ok arm only;
                    // the Err passthrough below is unchanged.
                    persist_run_marker(&self.db, adapter.name(), stats).await;
                }
                return result;
            }
        }
        warn!(source_name, "No adapter registered for source");
        Ok(EnrichmentStats::default())
    }

    /// Run ONLY the Pre-Permission Land Flip detector (FLIP-02), for `enrich --source
    /// land-flip`. The detector is a post-adapter step, not an adapter, so it is dispatched
    /// directly rather than via `run_source`.
    pub async fn detect_land_flips(&self) -> anyhow::Result<crate::land_flip::LandFlipStats> {
        crate::land_flip::run_detection(&self.db).await
    }
}

/// Build the best-effort `enrichment_runs:<source>` UPSERT SurrealQL (quick-260609-mhh).
///
/// `source` is a trusted `&'static` adapter name (a closed set of valid record-id keys,
/// see `EnricherAdapter::name`), so it is safe to interpolate directly into the record id.
/// `last_run_at` is bound at the call site via `$last_run_at`; the four counters are
/// inlined as `i64` numerics. Mirrors the cursor.rs UPSERT idiom.
///
/// `sub_candidate` is deliberately NOT persisted (locked decision — only the four
/// counters that the analyst probe consumes).
fn build_run_marker_upsert(source: &str, stats: &EnrichmentStats) -> String {
    format!(
        "UPSERT enrichment_runs:{source} SET source = '{source}', \
         last_run_at = $last_run_at, \
         candidates_found = {candidates_found}, \
         auto_linked = {auto_linked}, \
         pending_review = {pending_review}, \
         discarded = {discarded}",
        source = source,
        candidates_found = stats.candidates_found as i64,
        auto_linked = stats.auto_linked as i64,
        pending_review = stats.pending_review as i64,
        discarded = stats.discarded as i64,
    )
}

/// Persist a per-source enrichment run marker best-effort (quick-260609-mhh).
///
/// Failures only WARN-log — they MUST NOT propagate and MUST NOT alter the adapter's
/// returned stats (graceful-degradation invariant, mirrors cursor.rs persist_timepoint).
async fn persist_run_marker(db: &Surreal<Any>, source: &str, stats: &EnrichmentStats) {
    let sql = build_run_marker_upsert(source, stats);
    let last_run_at = Utc::now().timestamp();
    let result: surrealdb::Result<Vec<serde_json::Value>> = db
        .query(sql)
        .bind(("last_run_at", last_run_at))
        .await
        .and_then(|mut r| r.take(0));

    if let Err(e) = result {
        warn!(
            error = %e,
            source = source,
            "EnricherEngine: failed to persist enrichment_runs marker (non-fatal)"
        );
    }
}

/// Run the property accumulation detector as a post-adapter step.
///
/// Returns the number of alerts written, so `run_all_enabled` can fold it into the
/// `motif_count` on this run's `detector_runs` history row (46-05 D-05).
async fn run_property_accumulation(engine: &EnricherEngine, run_id: &str) -> anyhow::Result<usize> {
    let count = run_property_accumulation_detector(&engine.db, run_id).await?;
    if count > 0 {
        tracing::info!(
            alerts_written = count,
            "run_property_accumulation: HIGH alerts written for property_accumulation_anomaly"
        );
    }
    Ok(count)
}

/// Run the evasion detector as a post-adapter step.
///
/// This is a thin wrapper so the engine's `run_all_enabled` can call it without
/// needing to hold a mutable borrow on `self`. The detector is always run
/// regardless of whether any adapter is enabled — it operates on `identity_links`
/// that may have been written in a prior run.
async fn run_evasion(engine: &EnricherEngine, run_id: &str) -> anyhow::Result<usize> {
    let count = run_evasion_detector(&engine.db, run_id).await?;
    if count > 0 {
        tracing::info!(
            alerts_written = count,
            "run_evasion: CRITICAL alerts written for disqualified_director_evasion"
        );
    }
    Ok(count)
}

/// Run the Offshore Shadow detector as a post-adapter step (FUZ-05).
///
/// Fires HIGH alerts when a contract-holding entity has a confirmed/auto_linked
/// ICIJ offshore identity link (ENR-11 gate). Non-fatal — errors are logged as
/// warnings and the engine continues.
async fn run_offshore_shadow(engine: &EnricherEngine, run_id: &str) -> anyhow::Result<usize> {
    let count = run_offshore_shadow_detector(&engine.db, run_id).await?;
    if count > 0 {
        tracing::info!(
            alerts_written = count,
            "run_offshore_shadow: HIGH alerts written for offshore_shadow"
        );
    }
    Ok(count)
}

/// Run the Charity-Commercial Crossover detector as a post-adapter step (FUZ-06).
///
/// Fires MEDIUM alerts when an officer directs a contract-holding entity AND is
/// TrusteeOf a charity (ENR-11 gate). Non-fatal — errors are logged as warnings
/// and the engine continues.
async fn run_charity_crossover(engine: &EnricherEngine, run_id: &str) -> anyhow::Result<usize> {
    let count = run_charity_crossover_detector(&engine.db, run_id).await?;
    if count > 0 {
        tracing::info!(
            alerts_written = count,
            "run_charity_crossover: MEDIUM alerts written for charity_commercial_crossover"
        );
    }
    Ok(count)
}

/// Run the Political Donation → Contract Correlation detector as a post-adapter step (POL-04).
///
/// Loads the party authority control map and fires HIGH alerts when a donation
/// precedes or follows a contract from a party-controlled LPA within 730 days
/// (both directions — revolving-door detection). Non-fatal — if the JSON file is
/// absent or unparseable, logs a warning and returns Ok(()).
/// Run the Pre-Permission Land Flip detector as a post-adapter step (FLIP-02).
///
/// Runs the three mandatory gates (structural address, temporal window, connection
/// corroboration) against staged PPD transactions and writes passing candidates to the
/// `land_flip_candidates` review queue (NEVER `graph_events`). Non-fatal — errors are
/// logged as warnings and the engine continues.
async fn run_land_flip(engine: &EnricherEngine) -> anyhow::Result<()> {
    let stats = crate::land_flip::run_detection(&engine.db).await?;
    if stats.candidates_written > 0 {
        tracing::info!(
            applications_scanned = stats.applications_scanned,
            candidates_written = stats.candidates_written,
            "run_land_flip: land-flip candidates written to review queue"
        );
    }
    Ok(())
}

async fn run_donation_correlation(engine: &EnricherEngine, run_id: &str) -> anyhow::Result<usize> {
    let party_map = match PartyAuthorityMap::load() {
        Ok(m) => m,
        Err(e) => {
            warn!(
                ?e,
                "donation_correlation: failed to load data/party_authority_control.json — skipping detector"
            );
            return Ok(0);
        }
    };
    let count = run_donation_correlation_detector(&engine.db, &party_map, run_id).await?;
    if count > 0 {
        tracing::info!(
            alerts_written = count,
            "run_donation_correlation: HIGH alerts written for donation_contract_correlation"
        );
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure, offline test (no DB, no network, no async): the run-marker UPSERT SQL
    /// targets `enrichment_runs:<source>` and sets exactly the four persisted counters
    /// plus source/last_run_at, never `sub_candidate` (quick-260609-mhh).
    #[test]
    fn build_run_marker_upsert_shape() {
        let stats = EnrichmentStats {
            candidates_found: 7,
            auto_linked: 2,
            pending_review: 1,
            sub_candidate: 3, // deliberately set — must NOT appear in the SQL
            discarded: 4,
            skipped_out_of_jurisdiction: 0,
        };

        let sql = build_run_marker_upsert("ch_psc", &stats);

        // Record id is parameterised by the adapter name.
        assert!(
            sql.contains("enrichment_runs:ch_psc"),
            "SQL should target enrichment_runs:ch_psc, got: {sql}"
        );

        // All persisted fields present.
        for field in [
            "source",
            "last_run_at",
            "candidates_found",
            "auto_linked",
            "pending_review",
            "discarded",
        ] {
            assert!(
                sql.contains(field),
                "SQL should set field `{field}`, got: {sql}"
            );
        }

        // sub_candidate is the locked exclusion.
        assert!(
            !sql.contains("sub_candidate"),
            "SQL must NOT reference sub_candidate, got: {sql}"
        );

        // Distinct source names yield distinct record ids.
        let ec_sql = build_run_marker_upsert("electoral_commission", &EnrichmentStats::default());
        assert!(
            ec_sql.contains("enrichment_runs:electoral_commission"),
            "SQL should target enrichment_runs:electoral_commission, got: {ec_sql}"
        );
        assert!(
            !ec_sql.contains("enrichment_runs:ch_psc"),
            "electoral_commission marker must not collide with ch_psc, got: {ec_sql}"
        );
    }
}
