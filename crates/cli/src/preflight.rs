//! Startup jurisdiction gate.
//!
//! Establishes which UK nation an analysis covers, and refuses to start when the
//! configuration cannot be right for it: wards spanning two nations, or a deprivation
//! index belonging to a different nation from the wards. Sources that have no data for
//! the nation are reported here and refused at run time by the enricher engine.
//!
//! Runs once, before any subcommand dispatch. See the jurisdiction-gate spec.

use anyhow::bail;
use urban_integrity_core::config::AppConfig;
use urban_integrity_detector::scoring::imd::ImdIndex;
use urban_integrity_schema::nation::Nation;

/// What the preflight established about this analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JurisdictionReport {
    /// The analysis nation, or `None` when no wards are configured (nothing is gated).
    pub nation: Option<Nation>,
    /// The nation of the loaded deprivation index, if one is configured and readable.
    pub imd_nation: Option<Nation>,
    /// Registered sources that have no data for this nation. They will be refused at run
    /// time and counted as `skipped_out_of_jurisdiction`, not reported as zero results.
    pub out_of_jurisdiction_sources: Vec<&'static str>,
}

/// Establish the analysis nation and check the configuration is coherent for it.
///
/// Refuses (rather than warns) on two conditions, because both silently produce a
/// plausible-looking analysis that is wrong:
///
/// - **wards spanning nations** — no deprivation index covers two nations, and HM Land
///   Registry covers England and Wales only, so the same run would be scored on two
///   incomparable scales and enriched from a registry that is right for only part of it.
/// - **an index from another nation** — it loads cleanly, matches no wards, and leaves
///   every alert at `m_env = 1.0`, which is indistinguishable in the payload from "these
///   wards are not deprived".
///
/// A missing or unreadable index is NOT refused: the detector already degrades to
/// unweighted scoring and logs it, and turning that into a hard stop would break every
/// analysis that has no index yet.
pub fn jurisdiction_preflight(config: &AppConfig) -> anyhow::Result<JurisdictionReport> {
    // No `.context()` here: JurisdictionError already says which nations were found and
    // what to do about it, and anyhow's context would replace that in the Display output.
    let nation = config.analysis_nation()?;

    let imd_nation = config.detector.imd_csv_path.as_deref().and_then(|path| {
        let mut index = ImdIndex::new();
        index.load(path).ok()?;
        index.nation()
    });

    if let (Some(analysis), Some(index)) = (nation, imd_nation) {
        if analysis != index {
            let path = config.detector.imd_csv_path.as_deref().unwrap_or("");
            bail!(
                "[detector] imd_csv_path = '{path}' is the {index} deprivation index but this \
                 analysis covers {analysis}. It would load cleanly, match none of the \
                 configured wards, and leave every alert at m_env = 1.0 — which is NOT a \
                 finding that those wards are un-deprived. The four UK indices are separate \
                 and their ranks share no scale; supply the {analysis} index, or unset \
                 imd_csv_path to score without deprivation weighting."
            );
        }
    }

    let out_of_jurisdiction_sources = match nation {
        Some(n) => urban_integrity_enricher::engine::source_coverage()
            .into_iter()
            .filter(|(_, coverage)| !coverage.covers(n))
            .map(|(name, _)| name)
            .collect(),
        None => Vec::new(),
    };

    Ok(JurisdictionReport {
        nation,
        imd_nation,
        out_of_jurisdiction_sources,
    })
}

impl JurisdictionReport {
    /// Log what was established, so a run's evidence gaps are on the record before the
    /// work starts rather than inferred from empty results afterwards.
    pub fn log(&self) {
        match self.nation {
            Some(nation) => tracing::info!(
                %nation,
                imd_index = self
                    .imd_nation
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                "Analysis jurisdiction established from configured ward codes"
            ),
            None => tracing::info!(
                "No ward codes configured — analysis nation unknown, no source is gated on \
                 jurisdiction"
            ),
        }
        if !self.out_of_jurisdiction_sources.is_empty() {
            tracing::warn!(
                sources = %self.out_of_jurisdiction_sources.join(", "),
                nation = %self
                    .nation
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                "These sources have no data for this nation and will NOT be run. Their \
                 absence is a coverage gap, not a zero result."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use urban_integrity_core::config::{AppConfig, PlanningPortalConfig};
    use urban_integrity_schema::nation::Nation;

    fn cfg(wards: &[&str], imd_path: Option<&str>) -> AppConfig {
        let json = serde_json::json!({
            "surreal": { "ws_url": "ws://localhost:8000", "namespace": "urban_integrity", "database": "main" },
            "graph": {
                "checkpoint_events_n": 1000, "checkpoint_interval_secs": 60,
                "checkpoint_path": "data/checkpoints", "wal_channel_capacity": 10000,
                "wal_batch_size": 100, "wal_batch_window_ms": 100, "shard_count": 64,
                "snapshot_every_n_events": 1000, "snapshot_interval_secs": 30
            },
            "ingest": { "cf_poll_interval_secs": 900, "scrapling_url": "http://localhost", "scrapling_port": 8765 },
            "detector": { "phoenix_window_days": 180, "phoenix_min_value": 10000.0, "imd_csv_path": imd_path }
        });
        let mut config: AppConfig = serde_json::from_value(json).expect("config parses");
        config.ingest.planning_portals = wards
            .iter()
            .map(|w| PlanningPortalConfig {
                lpa_code: None,
                ward_code: (*w).to_string(),
                portal_type: "arcus".to_string(),
                url: "https://example.invalid".to_string(),
            })
            .collect();
        config
    }

    fn index_file(wards: &[(&str, u32)], denominator: u32) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "Ward Code,IMD Rank,IMD Denominator").unwrap();
        for (ward, rank) in wards {
            writeln!(f, "{ward},{rank},{denominator}").unwrap();
        }
        f
    }

    #[test]
    fn a_matching_index_passes_and_reports_the_nation() {
        let idx = index_file(&[("W05001492", 1573), ("W05001503", 295)], 1909);
        let config = cfg(&["W05001492"], idx.path().to_str());
        let report = jurisdiction_preflight(&config).expect("Welsh index, Welsh wards");
        assert_eq!(report.nation, Some(Nation::Wales));
        assert_eq!(report.imd_nation, Some(Nation::Wales));
        assert!(report.out_of_jurisdiction_sources.is_empty());
    }

    /// The defect this gate exists for: the English index loads cleanly against Welsh
    /// wards, matches none of them, and every alert carries m_env = 1.0 — which is not a
    /// finding that those wards are un-deprived. Previously a WARN mid-run; now a refusal
    /// before the run starts.
    #[test]
    fn an_index_from_another_nation_is_refused_at_startup() {
        let idx = index_file(&[("E05001234", 900)], 32844);
        let config = cfg(&["W05001492"], idx.path().to_str());
        let err = jurisdiction_preflight(&config).expect_err("English index, Welsh wards");
        let msg = err.to_string();
        assert!(msg.contains("England"), "{msg}");
        assert!(msg.contains("Wales"), "{msg}");
        assert!(msg.contains("imd_csv_path"), "{msg}");
    }

    #[test]
    fn a_scottish_analysis_reports_every_source_it_cannot_use() {
        let config = cfg(&["S13002522"], None);
        let report = jurisdiction_preflight(&config).expect("no index configured");
        assert_eq!(report.nation, Some(Nation::Scotland));
        assert_eq!(report.imd_nation, None);
        for source in [
            "land_registry_ccod",
            "land_registry_ocod",
            "ppd",
            "charity_commission",
        ] {
            assert!(
                report.out_of_jurisdiction_sources.contains(&source),
                "{source} covers England and Wales only — a Scottish analysis must be told"
            );
        }
        assert!(
            !report.out_of_jurisdiction_sources.contains(&"ch_psc"),
            "Companies House is UK-wide"
        );
    }

    #[test]
    fn a_mixed_nation_analysis_is_refused_before_anything_runs() {
        let config = cfg(&["W05001492", "S13002522"], None);
        let err = jurisdiction_preflight(&config).expect_err("mixed wards");
        assert!(err.to_string().contains("more than one UK nation"), "{err}");
    }

    /// An analysis with no wards has no nation, so nothing is gated and nothing is
    /// refused — but the report says so rather than implying coverage was checked.
    #[test]
    fn an_analysis_with_no_wards_passes_ungated() {
        let report = jurisdiction_preflight(&cfg(&[], None)).expect("no wards");
        assert_eq!(report.nation, None);
        assert!(report.out_of_jurisdiction_sources.is_empty());
    }

    /// A missing or unreadable index is not a jurisdiction failure — the detector already
    /// degrades to unweighted scoring and says so. Refusing here would turn a soft
    /// degradation into a hard stop.
    #[test]
    fn an_unreadable_index_is_not_a_jurisdiction_failure() {
        let config = cfg(&["W05001492"], Some("data/does-not-exist.csv"));
        let report = jurisdiction_preflight(&config).expect("missing index must not refuse");
        assert_eq!(report.imd_nation, None);
    }
}
