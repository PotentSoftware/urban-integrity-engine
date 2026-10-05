pub mod adapters;
pub mod charity_crossover;
pub mod confidence;
pub mod country_canonical;
pub mod donation_correlation;
pub mod ec_date;
pub mod engine;
pub mod evasion;
pub mod land_flip;
pub mod node_id;
pub mod node_status;
pub mod offshore_shadow;
pub mod property_accumulation;
pub mod wal_writer;

pub use charity_crossover::run_charity_crossover_detector;
pub use confidence::{
    CandidateLink, ConfidenceScore, ConfirmedLink, LinkStatus, SubCandidateLink,
    SUB_CANDIDATE_THRESHOLD,
};
pub use engine::{build_default_adapters, EnricherEngine};
pub use offshore_shadow::run_offshore_shadow_detector;

/// No-op function used by the hot-path latency test (Plan 04).
///
/// Callable from `crates/enricher/tests/hot_path.rs` without importing
/// anything from `crates/ingestor` — demonstrates ENR-01 isolation.
pub fn handle_event_noop() {}
