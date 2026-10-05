//! `urban-integrity-workbench` — the native (server-side) half of the fraud analyst workbench.
//!
//! This crate holds the server-only logic that produces the wire-format types
//! `crates/workbench-dto` defines: the exhaustive `Verdict -> VerdictView` conversion
//! (this plan, 42-03), and — in later plans of this phase — the replay-only boot sequence,
//! embedded static-asset serving, and the workbench status resolver.
//!
//! Never depended on by the wasm-facing frontend crate: this crate freely pulls in
//! `urban-integrity-core`/`urban-integrity-schema`/`surrealdb`/`tokio`, all of which are
//! forbidden from the browser's dependency graph (D-02, WBX-03). The frontend only ever
//! depends on `urban-integrity-workbench-dto`.

pub mod alerts;
pub mod analysis;
pub mod assets;
pub mod boot;
pub mod entity;
pub mod graph;
pub mod home;
pub mod investigation;
pub mod last_event;
pub mod population;
pub mod report_persistence;
pub mod report_view;
pub mod status;
pub mod verdict_conversion;
