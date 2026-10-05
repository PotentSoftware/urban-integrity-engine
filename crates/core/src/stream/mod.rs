//! Shared CH streaming primitives for both `crates/ingestor` and `crates/enricher`.
//!
//! Extracting the reconnect loop, cursor store, and name normalisation here
//! satisfies ENR-01: `crates/enricher` must not import `crates/ingestor`.
//! Both crates import from `urban_integrity_core::stream` instead.

pub mod cursor;
pub mod normalise;
pub mod reconnect;

pub use cursor::{CursorStore, InMemoryCursorStore, SurrealStreamCursor};
pub use normalise::normalise_name;
pub use reconnect::{run_ch_stream, StreamEventHandler};
