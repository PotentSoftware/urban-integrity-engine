pub mod ch_disqualified;
pub mod ch_psc;
pub mod charity_commission;
pub mod council_registers;
pub mod download;
pub mod electoral_commission;
pub mod icij;
pub mod land_registry_ccod;
pub mod land_registry_ocod;
pub mod land_registry_ppd;
pub mod london_gazette;
pub mod mutuals_register;
pub mod opencorporates;
pub mod opensanctions;
pub mod planning_identity;

use async_trait::async_trait;
use surrealdb::{engine::any::Any, Surreal};
use tokio::sync::mpsc::Sender;
use urban_integrity_core::graph::types::GraphMutation;
use urban_integrity_schema::nation::Jurisdiction;

/// Statistics returned by each adapter run.
#[derive(Debug, Clone, Default)]
pub struct EnrichmentStats {
    /// Number of candidate links discovered before threshold filtering.
    pub candidates_found: usize,
    /// Links promoted to auto-linked (score >= AUTO_LINK_THRESHOLD).
    pub auto_linked: usize,
    /// Links queued for analyst review (CANDIDATE_THRESHOLD <= score < AUTO_LINK_THRESHOLD).
    pub pending_review: usize,
    /// Sub-candidate links written (SUB_CANDIDATE_THRESHOLD <= score < CANDIDATE_THRESHOLD).
    /// These are written to identity_links but not promoted to analyst review.
    pub sub_candidate: usize,
    /// Links discarded (score < SUB_CANDIDATE_THRESHOLD or duplicate).
    pub discarded: usize,
    /// Adapters not run because their source has no data for this analysis's nation.
    ///
    /// Counted separately from a zero result on purpose: "this registry does not cover
    /// this country" and "this registry covers it and found nothing" are different
    /// findings, and only one of them is about the entities under analysis.
    pub skipped_out_of_jurisdiction: usize,
}

/// Extension point for every Phase 17–20 OSINT adapter.
///
/// Each adapter is responsible for a single data source (e.g. CH PSC, Land Registry).
/// `is_enabled()` gates execution — adapters with missing credentials skip silently.
#[async_trait]
pub trait EnricherAdapter: Send + Sync {
    /// Human-readable adapter identifier (e.g. `"ch_psc"`).
    fn name(&self) -> &'static str;

    /// Returns true when credentials are present and `enabled = true` in config.
    fn is_enabled(&self) -> bool;

    /// Which UK nations this adapter's source has data for.
    ///
    /// Defaults to UK-wide, which is right for Companies House, The Gazette (the
    /// `all-notices` endpoint spans the London, Edinburgh and Belfast editions) and the
    /// Electoral Commission. HM Land Registry and the Charity Commission override it.
    ///
    /// The engine refuses to run an adapter against a nation it does not cover. Without
    /// that gate the failures are silent and one of them is actively wrong rather than
    /// merely empty: CCOD filters rows on `company_number` alone, so a Scottish analysis
    /// gains real English property for its Scottish companies and gate-3 scores it as
    /// local corroboration (see the jurisdiction-gate spec).
    fn coverage(&self) -> Jurisdiction {
        Jurisdiction::UK_WIDE
    }

    /// Run a single enrichment pass against `db`, emitting `GraphMutation`s via `mutation_tx`.
    async fn run(
        &self,
        db: &Surreal<Any>,
        mutation_tx: &Sender<GraphMutation>,
    ) -> anyhow::Result<EnrichmentStats>;
}
