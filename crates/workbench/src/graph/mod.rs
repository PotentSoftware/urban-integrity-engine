//! `crates/workbench::graph` — server-side graph population scoping, the CSR traversal adapter,
//! cancellation primitive and algorithm modules the Graph page needs (the phase 44 research notes, GRA-01..06).
//!
//! Plan 44-02 declared [`population_scope`]. Plan 44-15 (this plan) adds the CSR/petgraph
//! adapter ([`csr_view`]) and the cancellation primitive ([`cancel`]) as real, tested
//! implementations, plus the remaining module tree ([`cache`], [`session`], [`arrow_export`],
//! [`label_index`], [`algorithms`]) as genuine-but-minimal stubs — each names, in its own doc
//! comment, the exact wave-3 plan (44-04 through 44-07) that fills it in. No later plan should
//! need to touch this file again.

pub mod algorithms;
pub mod arrow_export;
pub mod cache;
pub mod cancel;
/// CNV-03's five structured canvas filters (plan 45-14).
pub mod canvas_filter;
pub mod csr_view;
pub mod dispatch;
pub mod fixture;
pub mod handlers;
pub mod label_index;
pub mod population_scope;
pub mod session;
