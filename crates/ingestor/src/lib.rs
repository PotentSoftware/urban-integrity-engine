//! Urban Integrity — Data Ingestor
//!
//! Concurrent ingestion from Companies House (streaming), Contracts Finder (REST polling),
//! and planning portals (Scrapling sidecar). Feeds into the core EventPipeline.

pub mod address;
pub mod backfill;
pub mod cf;
pub mod ch;
pub mod planning;
pub mod projector;

pub use ch::SurrealCursorStore;
