//! `Probe`/`Strategy`/`run_probes` — the ordered, unconditional dispatch scaffolding PRB-06/PRB-08
//! require before Phase 32–34 can port any real probes.
//!
//! This module is pure scaffolding: no real probes are registered here. What matters is the
//! *shape* — an ordered slice of `Box<dyn Probe>`, a single `&mut Strategy` threaded through every
//! call in one flat loop, and zero verdict-conditioned branching inside that loop.
//! `run_probes` (below) remains the deliberately ungated primitive every existing call site (19 of
//! them, 15 in test files) continues to use unchanged. Verdict-gated orchestration is a separate,
//! additive mechanism — `graph::validate_dependency_graph` (this phase, D-12/D-13) plus a gated
//! dispatch function added to `graph.rs` in a later plan of this phase — that only `execute()`'s
//! default path invokes; `run_probes` itself is never modified to half-implement it.
//!
//! Phase 43 plan 01 extracted this module, the report writer, the dependency graph and the
//! population-source partition out of `crates/cli` into this standalone, native workspace member
//! so `crates/workbench` can invoke the battery in-process without creating a
//! `workbench -> cli` dependency cycle. `crates/cli/src/commands/probe.rs` is now a thin caller.

pub mod analyst_review_queue;
pub mod attributed;
pub mod council_registers_political_actors;
pub mod detector_readiness;
pub mod disqualification_chain;
pub mod disqualified_active_in_procurement;
pub mod disqualified_director_edges;
pub mod donation_contract_correlation;
pub mod electoral_commission_donations;
pub mod evasion_alerts;
pub mod gazette_insolvency_signals;
pub mod geographic_concentration;
pub mod graph;
pub mod identity_integrity;
pub mod identity_link_registry;
pub mod land_flip_candidates;
pub mod opencorporates_cross_jurisdiction;
pub mod osint_confirmed_intel;
pub mod osint_new_node_types;
pub mod population_source;
pub mod precomputed_alerts;
pub mod property_nodes;
pub mod psc_beneficial_ownership;
pub mod registry;
pub mod report;
pub mod run;
pub mod shared_exact_address;
pub mod temporal_burst;
pub mod ward_concentration;

use std::collections::HashMap;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::scan::{self, RunMarker};
use urban_integrity_schema::types::verdict::Finding;

/// The findings-by-entity coverage vocabulary (PIV-02, plan 45-05), re-exported so a probe
/// author reaches one spelling of it.
///
/// It is defined in `crates/core` beside the index that consumes it rather than here, because
/// `crates/core` cannot depend on this crate (the edge runs `probe -> core`) and the alternative
/// is two spellings of one contract — the thing plan 45-01 exists to prevent.
pub use urban_integrity_core::persistence::queries::{EvidenceScope, ProbeCoverage};

/// Mutable state shared across every probe in a single dispatch run.
///
/// Deliberately a plain mutable bag, not an enum or a builder — Python's original
/// (`scripts/analyst_probe.py:227-234`) is a dataclass mutated in place by whichever probe runs
/// first (e.g. `OSINT_PROBES[0]` sets `osint_active`), and later probes read it. The Rust port
/// keeps that shape exactly rather than inventing something more structured, because PRB-06
/// requires the port to be faithful, not improved.
#[derive(Debug, Default)]
pub struct Strategy {
    pub identity_blocked: bool,
    pub osint_active: bool,
    pub confirmed_link_count: u64,
    pub pending_review_count: u64,
    pub notes: Vec<String>,
    /// D6's lazy `enrichment_runs` memo. `None` until the first probe asks; every later probe in
    /// the same battery sees the cached result regardless of registration order. See
    /// [`Strategy::enrichment_runs`] for the fill/degrade semantics.
    pub enrichment_runs: Option<HashMap<String, RunMarker>>,
}

impl Strategy {
    /// D6's lazy `enrichment_runs` memo: on first call, reads every `enrichment_runs` row once
    /// via `scan::scan_enrichment_runs` and caches the result; every later call in the same
    /// dispatch run -- regardless of which probe asks, or in what order -- returns the same
    /// cached map instead of re-reading.
    ///
    /// This is D6's stated, narrow exception to Phase 32's D3 no-cache rule (phase 32 design decision D3
    /// refused a shared `Contract` population cache across 3 probes; this memo is a capability
    /// marker read by most of the 12 Tier-2 probes, not a population scan read by three -- the
    /// two decisions are not in tension, they draw the line in different places).
    ///
    /// On a scan failure, degrades to an empty `HashMap` rather than propagating the error -- a
    /// probe reading "no marker for source X" from an empty map degrades safely to that source's
    /// own `DataGap`/never-ran branch, and must never be silently read as a measured "definitely
    /// ran" claim.
    pub async fn enrichment_runs(&mut self, db: &Surreal<Any>) -> &HashMap<String, RunMarker> {
        if self.enrichment_runs.is_none() {
            let map = scan::scan_enrichment_runs(db).await.unwrap_or_default();
            self.enrichment_runs = Some(map);
        }
        self.enrichment_runs
            .as_ref()
            .expect("just filled above if it was None")
    }
}

/// The battery's findings-by-entity coverage (PIV-02, plan 45-05) — built from the real
/// `Vec<Box<dyn Probe>>`, never from a number typed into a document.
///
/// This is what makes the profile's coverage statement trustworthy. The figure an analyst reads
/// beside an empty findings list has to come from the probes that actually exist, because the
/// whole point of stating it is that an empty list is otherwise unreadable — and a stale figure
/// would make it unreadable in a way that looks measured.
///
/// Names arrive in registration order and are not sorted here: the order the battery runs in is
/// the order the report already presents, and re-sorting would give a reader two orders for one
/// list.
pub fn battery_evidence_coverage(probes: &[Box<dyn Probe>]) -> ProbeCoverage {
    let mut coverage = ProbeCoverage::default();
    for p in probes {
        let bucket = match p.evidence_scope() {
            EvidenceScope::NamesNodes => &mut coverage.names_nodes,
            EvidenceScope::NamesLinksOnly => &mut coverage.names_links_only,
            EvidenceScope::AggregateOnly => &mut coverage.aggregate_only,
        };
        bucket.push(p.name().to_owned());
    }
    coverage
}

/// What shape a probe's own finding is best drawn in on the finding board (D-06, phase 50).
///
/// Mirrors [`urban_integrity_schema::types::verdict::RecordKind`]'s "typed enum internally, plain
/// wire `String` at the boundary" split: this enum is the native, exhaustively-matched vocabulary
/// a probe answers `Probe::motif_kind()` with; [`workbench_dto::ProbeView::motif_kind`] (a later
/// task in this plan) carries [`MotifKind::name`]'s output across the wire as a plain `String`, so
/// the browser never constructs or infers one of these five values, only matches on the string.
///
/// Never derives `Deserialize` — same rationale as `RecordKind` and every other type in that
/// module's family: no consumer constructs a `MotifKind` from JSON, so the derive is withheld
/// pre-emptively rather than added back in later under time pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotifKind {
    /// A hub-and-spoke cluster fanning out from one shared record — e.g. the addresses
    /// `address-cluster-exact` groups entities under.
    Star,
    /// A single typed edge between two entities — e.g. the officer-to-company edges
    /// `osint-risk-edges` reports.
    TypedEdge,
    /// A ranked bar chart over a bucketed dimension — e.g. `ward-concentration`'s per-ward counts.
    RankedBars,
    /// A bag of named-entity chips with no further structure — the default shape for a probe that
    /// names nodes or identity links but has no dedicated motif renderer.
    Chips,
    /// No motif to draw — the probe's evidence carries no `RecordRef` at all (aggregate-only), so
    /// the card states absence honestly rather than rendering an empty shape.
    Absence,
}

impl MotifKind {
    /// The one spelling of this kind that crosses a wire — see [`RecordKind::name`] for the
    /// identical rationale (a consumer reading `ProbeView.motif_kind` and a consumer matching on
    /// this enum directly must never see two different names for one variant).
    pub const fn name(self) -> &'static str {
        match self {
            MotifKind::Star => "Star",
            MotifKind::TypedEdge => "TypedEdge",
            MotifKind::RankedBars => "RankedBars",
            MotifKind::Chips => "Chips",
            MotifKind::Absence => "Absence",
        }
    }
}

/// A single probe in the fixed battery.
///
/// `run` is infallible by design — a probe that hits an internal error (a failed query, a
/// missing table) must catch it INSIDE its own `run` implementation and return a `Finding` whose
/// `Verdict` is `Verdict::data_gap(...)` or `Verdict::blocked(...)`, mirroring Python's
/// `try/except -> Finding("(probe raised)", "DATA_GAP", ...)` (`analyst_probe.py:2551-2557`).
///
/// This trait method deliberately does NOT return `Result` — a `Result`-returning `run` would let
/// the dispatch loop's own code branch on success/failure, which is exactly the verdict-conditioned
/// branching PRB-08 says the loop itself must never do.
#[async_trait::async_trait]
pub trait Probe: Send + Sync {
    /// Human-readable probe identifier.
    fn name(&self) -> &'static str;

    /// Run this probe against `db`, reading and/or mutating the shared `Strategy`.
    async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding;

    /// What this probe's evidence can name, and therefore whether the findings-by-entity
    /// reverse index (PIV-02, plan 45-05) can search it at all.
    ///
    /// **There is deliberately no default.** A default would make this a convention: a probe
    /// added to the battery would silently inherit `AggregateOnly` and the coverage figure the
    /// profile states would quietly stop counting a probe that exists — which is the exact shape
    /// of the failure that section is built to prevent, one dimension over. Without a default,
    /// adding a probe without answering this does not compile.
    ///
    /// Answer it from what the probe's `EvidenceRow`s actually carry, not from what it is about:
    ///
    /// - [`EvidenceScope::NamesNodes`] — at least one `RecordRef` whose `id` is a graph node id.
    /// - [`EvidenceScope::NamesLinksOnly`] — `RecordRef`s, but only `RecordKind::IdentityLink`,
    ///   whose id is an `identity_links` record rather than a node.
    /// - [`EvidenceScope::AggregateOnly`] — no `RecordRef` at all; the evidence text interpolates
    ///   counts, producer names or adapter names.
    ///
    /// `crates/probe/tests/findings_coverage.rs` reconciles every answer here against the probe's
    /// own source, so a probe that gains a `RecordRef` and forgets to change this fails loudly
    /// rather than under-reporting forever.
    fn evidence_scope(&self) -> EvidenceScope;

    /// What shape this probe's finding is drawn in on the finding board (D-06, phase 50).
    ///
    /// **There is deliberately no default.** A default would make this a convention: a probe
    /// added to the battery would silently inherit whichever variant the default picked, and the
    /// finding board would render every un-classified probe in that one shape regardless of what
    /// it actually detected — which is the exact shape of the failure `evidence_scope`'s own
    /// no-default rule exists to prevent, one dimension over. Without a default, adding a probe
    /// without answering this does not compile.
    ///
    /// `crates/probe/tests/motif_kind_coverage.rs` reconciles every answer here against a pinned
    /// per-probe table, so a probe that should change shape and forgets to fails loudly rather
    /// than silently rendering the wrong motif forever.
    fn motif_kind(&self) -> MotifKind;

    /// This probe's tier: exactly `"Tier-1"` or `"Tier-2"` (PRB-02's own two-tier
    /// classification). A per-probe constant, not per-finding data — kept on `Probe` rather than
    /// `Finding` per D-01's split-by-where-it-varies rule.
    fn tier(&self) -> &'static str;

    /// One sentence stating what question this probe answers. A per-probe constant, not
    /// per-finding data — kept on `Probe` rather than `Finding` per D-01's split-by-where-it-varies
    /// rule.
    fn hypothesis(&self) -> &'static str;

    /// D-12's per-probe dependency declaration: the `name()`s of probes this probe depends on.
    /// Defaults to none — **every probe in the production battery declares none today**. Read by
    /// `graph::validate_dependency_graph` to build the startup-validated dependency graph and
    /// derive dispatch order; does NOT affect `run_probes`, which remains unconditional.
    ///
    /// # Two conditions before declaring an edge
    ///
    /// D-07 gates a dependent on its upstream returning **Hit or Weak**. That imposes a meaning on
    /// this method which the battery's only edge failed on both counts, so state it plainly:
    ///
    /// 1. **The upstream must be able to return Hit or Weak.** Several probes here are structurally
    ///    Clear-or-DataGap only — they pass `matches: 0` with a `u64::MAX` threshold to
    ///    `Verdict::classify`. Naming one as an upstream makes the dependent *permanently
    ///    unreachable* under the default dispatch, on every corpus, with no error: the report
    ///    simply records it as `Skipped` forever. This is not statically checkable, so it is on
    ///    the author to verify.
    /// 2. **The dependency must be on upstream SIGNAL, not upstream data quality.** "Run me only
    ///    if that probe found something" is what this method expresses. "Run me only if that
    ///    probe's data is trustworthy" is the opposite polarity — a `Clear` upstream means such a
    ///    precondition is *satisfied*, so gating on Hit/Weak would run the dependent exactly when
    ///    its input is broken. Express data-quality preconditions through [`Strategy`] instead
    ///    (see `Strategy::identity_blocked` and its use in `shared_exact_address.rs`), which is
    ///    read when the probe runs and lets it qualify its own conclusion.
    ///
    /// The removed edge is documented in full at the top of `shared_exact_address.rs`.
    fn depends_on(&self) -> &'static [&'static str] {
        &[]
    }
}

/// D-03's fix for the machine-pluralisation defect: returns `"{n} {singular}"` when `n == 1`,
/// else `"{n} {plural_form}"`.
pub fn plural(n: u64, singular: &str, plural_form: &str) -> String {
    if n == 1 {
        format!("{n} {singular}")
    } else {
        format!("{n} {plural_form}")
    }
}

/// Run every probe in `probes`, in order, threading a single `&mut Strategy` through each call.
///
/// This loop must never grow a branch conditioned on a `Finding`'s `Verdict` — that is PRB-08's
/// foreclosure constraint and PRB-06's fixed-battery decision (the Rust port and the Python
/// original must run the identical probe set for RPT-01's differential to be meaningful). No
/// `is_enabled()` check, no admission gate, no early return — every registered probe runs on
/// every dispatch, regardless of what any earlier probe returned. `run_probes` itself remains the
/// deliberately ungated primitive every existing call site (19 of them, 15 in test files)
/// continues to use unchanged; verdict-gated orchestration is a separate, additive mechanism
/// (`graph::validate_dependency_graph`, this phase, plus a gated dispatch function added to
/// `graph.rs` in a later plan) that only `execute()`'s default path invokes — this loop is never
/// modified to half-implement it.
pub async fn run_probes(
    probes: &[Box<dyn Probe>],
    db: &Surreal<Any>,
    st: &mut Strategy,
) -> Vec<Finding> {
    let mut findings = Vec::with_capacity(probes.len());
    for probe in probes {
        findings.push(probe.run(db, st).await);
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::verdict::{Severity, Verdict};

    struct StubProbeA;

    #[async_trait::async_trait]
    impl Probe for StubProbeA {
        fn name(&self) -> &'static str {
            "stub_a"
        }

        /// Test stubs are `AggregateOnly` — they emit no evidence at all, so nothing in them
        /// could ever name a node. Declaring it explicitly is the point of the method having no
        /// default (see `Probe::evidence_scope`).
        fn evidence_scope(&self) -> EvidenceScope {
            EvidenceScope::AggregateOnly
        }

        /// These stubs are never exercised by the finding board, only by dispatch-ordering/
        /// strategy tests, so their motif classification is inert — `Absence` requires the least
        /// reasoning to verify.
        fn motif_kind(&self) -> MotifKind {
            MotifKind::Absence
        }

        async fn run(&self, _db: &Surreal<Any>, st: &mut Strategy) -> Finding {
            st.identity_blocked = true;
            Finding {
                probe: self.name().to_string(),
                verdict: Verdict::classify(5, 5, 5, 1, ""),
                summary: "A ran".to_string(),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            }
        }

        fn tier(&self) -> &'static str {
            "Tier-1"
        }

        fn hypothesis(&self) -> &'static str {
            "stub probe A -- synthetic, no real hypothesis"
        }
    }

    struct StubProbeB;

    #[async_trait::async_trait]
    impl Probe for StubProbeB {
        fn name(&self) -> &'static str {
            "stub_b"
        }

        /// Test stubs are `AggregateOnly` — they emit no evidence at all, so nothing in them
        /// could ever name a node. Declaring it explicitly is the point of the method having no
        /// default (see `Probe::evidence_scope`).
        fn evidence_scope(&self) -> EvidenceScope {
            EvidenceScope::AggregateOnly
        }

        /// These stubs are never exercised by the finding board, only by dispatch-ordering/
        /// strategy tests, so their motif classification is inert — `Absence` requires the least
        /// reasoning to verify.
        fn motif_kind(&self) -> MotifKind {
            MotifKind::Absence
        }

        async fn run(&self, _db: &Surreal<Any>, st: &mut Strategy) -> Finding {
            let summary = if st.identity_blocked {
                "saw identity_blocked=true"
            } else {
                "saw identity_blocked=false"
            };
            Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap("stub, no real data"),
                summary: summary.to_string(),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            }
        }

        fn tier(&self) -> &'static str {
            "Tier-1"
        }

        fn hypothesis(&self) -> &'static str {
            "stub probe B -- synthetic, no real hypothesis"
        }
    }

    struct NamedStub(&'static str);

    #[async_trait::async_trait]
    impl Probe for NamedStub {
        fn name(&self) -> &'static str {
            self.0
        }

        /// Test stubs are `AggregateOnly` — they emit no evidence at all, so nothing in them
        /// could ever name a node. Declaring it explicitly is the point of the method having no
        /// default (see `Probe::evidence_scope`).
        fn evidence_scope(&self) -> EvidenceScope {
            EvidenceScope::AggregateOnly
        }

        /// These stubs are never exercised by the finding board, only by dispatch-ordering/
        /// strategy tests, so their motif classification is inert — `Absence` requires the least
        /// reasoning to verify.
        fn motif_kind(&self) -> MotifKind {
            MotifKind::Absence
        }

        async fn run(&self, _db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
            Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap("stub, no real data"),
                summary: format!("{} ran", self.0),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            }
        }

        fn tier(&self) -> &'static str {
            "Tier-1"
        }

        fn hypothesis(&self) -> &'static str {
            "named stub -- synthetic, no real hypothesis"
        }
    }

    #[test]
    fn depends_on_defaults_to_empty_when_not_overridden() {
        // StubProbeA does not override `depends_on()` -- proves the default trait method
        // (D-12) returns `&[]` rather than requiring every probe to implement it.
        assert_eq!(StubProbeA.depends_on(), &[] as &[&str]);
    }

    #[tokio::test]
    async fn two_stub_probes_thread_a_shared_strategy() {
        let db: Surreal<Any> = Surreal::init();
        let mut st = Strategy::default();

        let probes: Vec<Box<dyn Probe>> = vec![Box::new(StubProbeA), Box::new(StubProbeB)];
        let findings = run_probes(&probes, &db, &mut st).await;

        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].summary, "A ran");
        assert_eq!(
            findings[1].summary, "saw identity_blocked=true",
            "second probe must observe the first probe's mutation through the shared &mut Strategy"
        );
    }

    #[tokio::test]
    async fn every_probe_runs_regardless_of_prior_verdict() {
        let db: Surreal<Any> = Surreal::init();
        let mut st = Strategy::default();

        // StubProbeA returns a Hit-shaped Verdict (matches >= hit_threshold).
        let probes: Vec<Box<dyn Probe>> = vec![Box::new(StubProbeA), Box::new(StubProbeB)];
        let findings = run_probes(&probes, &db, &mut st).await;

        assert!(matches!(findings[0].verdict, Verdict::Hit { .. }));
        assert_eq!(
            findings.len(),
            2,
            "StubProbeB must still run and appear in the results even though StubProbeA returned a Hit"
        );
        assert_eq!(findings[1].probe, "stub_b");
    }

    #[tokio::test]
    async fn dispatch_preserves_registration_order() {
        let db: Surreal<Any> = Surreal::init();
        let mut st = Strategy::default();

        let probes: Vec<Box<dyn Probe>> = vec![
            Box::new(NamedStub("first")),
            Box::new(NamedStub("second")),
            Box::new(NamedStub("third")),
        ];
        let findings = run_probes(&probes, &db, &mut st).await;

        let names: Vec<&str> = findings.iter().map(|f| f.probe.as_str()).collect();
        assert_eq!(names, vec!["first", "second", "third"]);
    }

    struct EnrichmentRunsProbeA;

    #[async_trait::async_trait]
    impl Probe for EnrichmentRunsProbeA {
        fn name(&self) -> &'static str {
            "enrichment_runs_probe_a"
        }

        /// Test stubs are `AggregateOnly` — they emit no evidence at all, so nothing in them
        /// could ever name a node. Declaring it explicitly is the point of the method having no
        /// default (see `Probe::evidence_scope`).
        fn evidence_scope(&self) -> EvidenceScope {
            EvidenceScope::AggregateOnly
        }

        /// These stubs are never exercised by the finding board, only by dispatch-ordering/
        /// strategy tests, so their motif classification is inert — `Absence` requires the least
        /// reasoning to verify.
        fn motif_kind(&self) -> MotifKind {
            MotifKind::Absence
        }

        async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
            let len = st.enrichment_runs(db).await.len();
            Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap("stub, no real data"),
                summary: format!("A saw {len} enrichment_runs entries"),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            }
        }

        fn tier(&self) -> &'static str {
            "Tier-2"
        }

        fn hypothesis(&self) -> &'static str {
            "enrichment runs stub A -- synthetic, no real hypothesis"
        }
    }

    struct EnrichmentRunsProbeB;

    #[async_trait::async_trait]
    impl Probe for EnrichmentRunsProbeB {
        fn name(&self) -> &'static str {
            "enrichment_runs_probe_b"
        }

        /// Test stubs are `AggregateOnly` — they emit no evidence at all, so nothing in them
        /// could ever name a node. Declaring it explicitly is the point of the method having no
        /// default (see `Probe::evidence_scope`).
        fn evidence_scope(&self) -> EvidenceScope {
            EvidenceScope::AggregateOnly
        }

        /// These stubs are never exercised by the finding board, only by dispatch-ordering/
        /// strategy tests, so their motif classification is inert — `Absence` requires the least
        /// reasoning to verify.
        fn motif_kind(&self) -> MotifKind {
            MotifKind::Absence
        }

        async fn run(&self, db: &Surreal<Any>, st: &mut Strategy) -> Finding {
            let len = st.enrichment_runs(db).await.len();
            Finding {
                probe: self.name().to_string(),
                verdict: Verdict::data_gap("stub, no real data"),
                summary: format!("B saw {len} enrichment_runs entries"),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            }
        }

        fn tier(&self) -> &'static str {
            "Tier-2"
        }

        fn hypothesis(&self) -> &'static str {
            "enrichment runs stub B -- synthetic, no real hypothesis"
        }
    }

    /// D6: one fill, many reads. `Surreal::init()` has no live tables, so this cannot exercise a
    /// real read -- instead it proves the SECOND probe to call `st.enrichment_runs(db)` sees the
    /// identical cached map the FIRST probe filled, regardless of which of the two asked first
    /// (order independence), rather than each probe triggering its own `scan_enrichment_runs`
    /// call.
    #[tokio::test]
    async fn strategy_enrichment_runs_memo_is_order_independent() {
        let db: Surreal<Any> = Surreal::init();
        let mut st = Strategy::default();
        assert!(
            st.enrichment_runs.is_none(),
            "memo must start unfilled before any probe asks"
        );

        let probes: Vec<Box<dyn Probe>> = vec![
            Box::new(EnrichmentRunsProbeA),
            Box::new(EnrichmentRunsProbeB),
        ];
        let findings = run_probes(&probes, &db, &mut st).await;

        assert_eq!(findings.len(), 2);
        assert!(
            st.enrichment_runs.is_some(),
            "memo must be filled after the first probe asks"
        );
        let filled_len = st.enrichment_runs.as_ref().unwrap().len();

        // The second probe's own report of what it saw must match the first probe's -- proving
        // both read the SAME cached fill, not two independent scans.
        assert!(
            findings[0].summary.contains(&filled_len.to_string()),
            "first probe's reported count must match the memo's final contents, got: {}",
            findings[0].summary
        );
        assert!(
            findings[1].summary.contains(&filled_len.to_string()),
            "second probe must see the identical cached map the first probe filled, got: {}",
            findings[1].summary
        );
    }
}
