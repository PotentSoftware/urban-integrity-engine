pub mod alert;
pub mod motif;
pub mod scoring;

use std::collections::HashMap;

use crate::alert::buffer::PendingEvidenceBuffer;
use crate::alert::cache::AlertCache;
use crate::motif::cluster::ClusterDetector;
use crate::motif::nepotism::NepotismDetector;
use crate::motif::phoenix::PhoenixDetector;
use crate::scoring::registry::ScoreRegistry;

use crate::alert::result::MotifResult;
use crate::scoring::imd::ImdIndex;
use serde::{Deserialize, Serialize};
use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_schema::types::node::NodeWeight;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DetectorConfig {
    pub phoenix_window_days: i64,
    pub phoenix_min_value: f64,
    pub imd_csv_path: Option<String>,
}

pub struct DetectorEngine {
    pub cache: AlertCache,
    pub buffer: PendingEvidenceBuffer,
    pub registry: ScoreRegistry,

    pub phoenix: PhoenixDetector,
    pub cluster: ClusterDetector,
    pub nepotism: NepotismDetector,

    /// Deprivation index for IMD-weighting alerts. `None` when `imd_csv_path` is
    /// unset or the CSV failed to load — in that case alerts still get a ward but
    /// no environmental weighting (graceful degradation).
    imd: Option<ImdIndex>,

    /// The configured IMD CSV path, retained separately from `imd` (Phase 46, D-07) so
    /// `TierBDetector::imd_state()` can report it even when the load failed (`imd: None`) —
    /// "no path configured" and "path configured but load failed" must stay distinguishable.
    configured_imd_csv_path: Option<String>,
}

impl DetectorEngine {
    pub fn new(config: DetectorConfig) -> Self {
        // Retained separately from the `imd` load result below (Phase 46, D-07) — captured
        // before the load closure so `imd_state()` can report the configured path even when
        // the load itself failed.
        let configured_imd_csv_path = config.imd_csv_path.clone();

        // Load the IMD index up front (once) from the configured CSV. A missing or
        // malformed CSV degrades gracefully to no weighting rather than failing.
        let imd = config.imd_csv_path.as_deref().and_then(|path| {
            let mut index = ImdIndex::new();
            match index.load(path) {
                Ok(()) => {
                    if !index.denominator_declared {
                        // `max_rank` was inferred from the largest rank in the file, which
                        // is only correct if the file IS the whole index. A regional
                        // extract normalises against its own worst ward and reports
                        // subset-relative weights as deprivation weighting.
                        tracing::warn!(
                            path,
                            wards = index.ranks.len(),
                            inferred_max_rank = index.max_rank,
                            "IMD index declares no `IMD Denominator` column — m_env is scaled to \
                             this file's own worst ward, which is wrong unless the file is the \
                             complete index"
                        );
                    }
                    tracing::info!(
                        path,
                        wards = index.ranks.len(),
                        max_rank = index.max_rank,
                        denominator_declared = index.denominator_declared,
                        "IMD index loaded"
                    );
                    Some(index)
                }
                Err(e) => {
                    tracing::warn!(
                        path,
                        error = %e,
                        "IMD index load failed — alerts will carry a ward but no deprivation weighting"
                    );
                    None
                }
            }
        });

        Self {
            cache: AlertCache::new(1000, 3600),      // 1 hour TTL for top 1000
            buffer: PendingEvidenceBuffer::new(60),  // 60s TTL for incomplete evidence
            registry: ScoreRegistry::new(200, 95.0), // Top 5% threshold

            phoenix: PhoenixDetector::new(crate::motif::phoenix::PhoenixConfig {
                detection_window_days: config.phoenix_window_days,
                min_value_threshold: config.phoenix_min_value,
            }),
            cluster: ClusterDetector::new(Default::default()),
            nepotism: NepotismDetector::new(Default::default()),
            imd,
            configured_imd_csv_path,
        }
    }

    pub fn run_tier_b(
        &self,
        snapshot: &CsrSnapshot,
        delta_nodes: &[u32],
        id_map: &HashMap<u32, String>,
    ) -> Vec<MotifResult> {
        let mut all_results = Vec::new();

        // Rayon par_iter over detectors or delta_nodes
        // Let's run the detectors sequentially but they can par_iter internally if needed
        // For now, just collect from each detector.
        let p_res = self.phoenix.check_tier_b(snapshot, delta_nodes, id_map);
        let c_res = self.cluster.check(snapshot, id_map);
        let n_res = self.nepotism.check(snapshot, id_map);

        all_results.extend(p_res);
        all_results.extend(c_res);
        all_results.extend(n_res);

        // Post-detection enrichment: attach each alert's ward (resolved from a
        // Contract in a participant's neighbourhood) and, when an IMD index is
        // configured, its deprivation weighting. Runs for every detector's output
        // so ward attribution can't drift per-detector.
        let rev: HashMap<&str, u32> = id_map.iter().map(|(idx, id)| (id.as_str(), *idx)).collect();
        for result in &mut all_results {
            self.enrich_alert(result, snapshot, &rev);
        }

        // Coverage, reported once per pass rather than per alert.
        //
        // An alert with a ward the index does not cover scores `m_env = 1.0` — the same
        // number as the least deprived ward in the index. Nothing in the payload says
        // which of the two happened beyond a null `imd_rank`, so without this the
        // difference between "weighted, and this place is fine" and "no deprivation data
        // for this place at all" is invisible in the logs. Measured live on
        // `an_anglesey_aebaa785` (2026-08-16): 1,231 of 1,917 alerts carried a ward,
        // every one of them Welsh (`W05…`), and the configured English index matched
        // none of them.
        if let Some(imd) = &self.imd {
            let with_ward = all_results
                .iter()
                .filter(|r| r.score_breakdown.ward_code.is_some())
                .count();
            let weighted = all_results
                .iter()
                .filter(|r| r.score_breakdown.imd_rank.is_some())
                .count();
            if weighted < with_ward {
                tracing::warn!(
                    alerts = all_results.len(),
                    with_ward,
                    weighted,
                    unweighted = with_ward - weighted,
                    index_wards = imd.ranks.len(),
                    "IMD weighting incomplete — these alerts have a ward the index does not \
                     cover and carry m_env = 1.0, which is NOT a finding that they are \
                     un-deprived"
                );
            }
        }

        all_results
    }

    /// Attach ward + IMD weighting to one detected alert. Without this stage every
    /// alert carries `ward_code: None`, so ward-scoped queries return nothing (see
    /// the `alerts-missing-ward-imd-attribution` debug finding).
    fn enrich_alert(
        &self,
        result: &mut MotifResult,
        snapshot: &CsrSnapshot,
        rev: &HashMap<&str, u32>,
    ) {
        // Ward: first Contract neighbour of any participant that carries a ward_code.
        let ward = result
            .participants
            .iter()
            .filter_map(|p| rev.get(p.as_str()).copied())
            .find_map(|idx| ward_of_contract_neighbour(snapshot, idx));

        let Some(ward) = ward else {
            return;
        };
        result.score_breakdown.ward_code = Some(ward.clone());

        // IMD environmental weighting only applies when an index is loaded AND that
        // index actually covers this ward. `deprivation` returns 0.0 for an unknown ward,
        // so applying it unconditionally would stamp a weighting on an alert the index has
        // nothing to say about — indistinguishable from the least deprived ward. Leaving
        // `m_env` at its default and `imd_rank` at `None` keeps the two apart.
        if let Some(imd) = self.imd.as_ref().filter(|i| i.covers(&ward)) {
            // Deprivation claims a fraction of the score's remaining HEADROOM rather than
            // multiplying it, so it can never reach the 1.0 ceiling and flatten the field —
            // see `formula::IMD_WEIGHT` for the live measurement that ruled the
            // multiplicative forms out.
            let base = result.risk_score;
            let weighted = crate::scoring::formula::apply_imd(base, imd.deprivation(&ward));
            result.score_breakdown.m_env = crate::scoring::formula::effective_m_env(base, weighted);
            result.score_breakdown.imd_rank = imd.ranks.get(&ward).copied();
            result.risk_score = weighted;
            result.score_breakdown.p_final = weighted;
        }
    }
}

/// The ward code of the first `Contract` node directly adjacent to `node_idx`, if any.
fn ward_of_contract_neighbour(snapshot: &CsrSnapshot, node_idx: u32) -> Option<String> {
    for &n in snapshot.graph.neighbors_slice(node_idx) {
        if let NodeWeight::Contract(c) = &snapshot.graph[n] {
            if let Some(ward) = c.ward_code.as_ref() {
                return Some(ward.clone());
            }
        }
    }
    None
}

impl urban_integrity_core::graph::coordinator::TierBDetector for DetectorEngine {
    fn run_tier_b(
        &self,
        snapshot: &CsrSnapshot,
        delta_nodes: &[u32],
        id_map: &HashMap<u32, String>,
    ) -> Vec<urban_integrity_core::graph::types::GraphEventKind> {
        let results = self.run_tier_b(snapshot, delta_nodes, id_map);
        results
            .into_iter()
            .map(
                |r| urban_integrity_core::graph::types::GraphEventKind::MotifDetected {
                    result_json: serde_json::to_string(&r).unwrap_or_default(),
                },
            )
            .collect()
    }

    fn imd_state(&self) -> urban_integrity_core::graph::coordinator::ImdRunState {
        urban_integrity_core::graph::coordinator::ImdRunState {
            loaded: self.imd.is_some(),
            csv_path: self.configured_imd_csv_path.clone(),
            denominator_declared: self
                .imd
                .as_ref()
                .map(|i| i.denominator_declared)
                .unwrap_or(false),
        }
    }
}
