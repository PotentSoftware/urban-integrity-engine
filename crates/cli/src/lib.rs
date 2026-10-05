/// Public re-exports for integration tests.
/// The CLI binary uses `mod` directly in main.rs, but tests need a lib target.
pub mod commands;
pub mod db;
pub mod motif_run_attribution;
pub mod ollama;
pub mod output;
pub mod preflight;
pub mod registry;

// Re-export invoke_pdf_tool for integration tests (report_pdf.rs)
pub use commands::report::invoke_pdf_tool;

/// Build the detector's runtime settings from the `[detector]` config block.
///
/// Exists as a named function purely so the wiring can be tested. It used to be three
/// literals inline in `serve` — `phoenix_window_days: 730`, `phoenix_min_value: 10_000.0`,
/// `imd_csv_path: None` — which meant the only process that runs detection ignored the
/// entire `[detector]` block. Phoenix ran a 730-day window while config and every doc said
/// 180, and IMD weighting was disabled outright, pinning `m_env` to 1.0 on every alert
/// (planning open item 26).
///
/// Fallbacks match `DetectorConfigRaw::default()`, so an absent block behaves as documented
/// rather than silently widening the window.
pub fn detector_config_from(
    raw: &urban_integrity_core::config::DetectorConfigRaw,
) -> urban_integrity_detector::DetectorConfig {
    urban_integrity_detector::DetectorConfig {
        phoenix_window_days: raw.phoenix_window_days.unwrap_or(180),
        phoenix_min_value: raw.phoenix_min_value.unwrap_or(10_000.0),
        imd_csv_path: raw.imd_csv_path.clone(),
    }
}

#[cfg(test)]
mod detector_config_tests {
    use super::detector_config_from;
    use urban_integrity_core::config::DetectorConfigRaw;

    #[test]
    fn detector_config_comes_from_config_not_a_literal() {
        let raw = DetectorConfigRaw {
            phoenix_window_days: Some(180),
            phoenix_min_value: Some(25_000.0),
            imd_csv_path: Some("data/imd_2025.csv".to_string()),
        };
        let cfg = detector_config_from(&raw);
        assert_eq!(
            cfg.phoenix_window_days, 180,
            "the configured window must reach the detector — serve hardcoded 730, so every \
             Phoenix alert ever emitted used a window four times wider than policy"
        );
        assert_eq!(cfg.phoenix_min_value, 25_000.0);
        assert_eq!(
            cfg.imd_csv_path.as_deref(),
            Some("data/imd_2025.csv"),
            "imd_csv_path must reach the detector — hardcoding None disabled IMD weighting, \
             contradicting the load-bearing rule that P_c is IMD-weighted"
        );
    }

    #[test]
    fn absent_detector_block_falls_back_to_documented_defaults() {
        let raw = DetectorConfigRaw {
            phoenix_window_days: None,
            phoenix_min_value: None,
            imd_csv_path: None,
        };
        let cfg = detector_config_from(&raw);
        assert_eq!(
            cfg.phoenix_window_days, 180,
            "a missing window must default to the documented 180, never wider"
        );
        assert_eq!(cfg.phoenix_min_value, 10_000.0);
    }
}
