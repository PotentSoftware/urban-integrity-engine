//! The 22-probe battery's registration site and gated/ungated dispatch entry point.
//!
//! This module holds the battery-internal logic only — building the ordered
//! `Vec<Box<dyn Probe>>` matching Python's `GRAPH_PROBES`/`OSINT_PROBES`
//! (`scripts/analyst_probe.py:2511`/`:2521`) and dispatching it, gated or ungated. CLI-only
//! concerns (`ProbeArgs`, `AppConfig::load()`, connecting to SurrealDB, table rendering, artifact
//! writing) live in `crates/cli/src/commands/probe.rs`, which calls [`execute`] with
//! already-connected dependencies.
//!
//! # Single-config-load invariant (T-32-01)
//!
//! Both database connections the CLI caller opens — the analysis DB and the `_catalog` DB, the
//! second needed only by `IdentityIntegrity` — must be derived from the SAME `AppConfig::load()`
//! result inside one call. `OpencorporatesCrossJurisdiction`'s three boolean config gates
//! ([`OcProbeGates`]) are read from that same config value, never a second independent load. None
//! of the 12 Tier-2 probes or `LandFlipCandidates`/`DonationContractCorrelation` need a second
//! connection — all three are zero-field unit structs.

use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use urban_integrity_schema::types::verdict::{Finding, PopulationSource, Verdict};

use crate::analyst_review_queue::AnalystReviewQueue;
use crate::council_registers_political_actors::CouncilRegistersPoliticalActors;
use crate::detector_readiness::DetectorReadiness;
use crate::disqualified_active_in_procurement::DisqualifiedActiveInProcurement;
use crate::disqualified_director_edges::DisqualifiedDirectorEdges;
use crate::donation_contract_correlation::DonationContractCorrelation;
use crate::electoral_commission_donations::ElectoralCommissionDonations;
use crate::evasion_alerts::EvasionAlerts;
use crate::gazette_insolvency_signals::GazetteInsolvencySignals;
use crate::geographic_concentration::GeographicConcentration;
use crate::identity_integrity::IdentityIntegrity;
use crate::identity_link_registry::IdentityLinkRegistry;
use crate::land_flip_candidates::LandFlipCandidates;
use crate::opencorporates_cross_jurisdiction::OpencorporatesCrossJurisdiction;
use crate::osint_confirmed_intel::OsintConfirmedIntel;
use crate::osint_new_node_types::OsintNewNodeTypes;
use crate::precomputed_alerts::PrecomputedAlerts;
use crate::property_nodes::PropertyNodes;
use crate::psc_beneficial_ownership::PscBeneficialOwnership;
use crate::report::ReportEntry;
use crate::shared_exact_address::SharedExactAddress;
use crate::temporal_burst::TemporalBurst;
use crate::ward_concentration::WardConcentration;
use crate::{Probe, Strategy};

/// Runtime options for [`execute`], replacing the CLI's `&ProbeArgs` so this function needs no
/// `clap` dependency.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    /// Run the complete, ungated 22-probe battery — gating is the default (D-17); this flag opts
    /// out so a gated run can always be compared against the complete one (GAT-04). Note the
    /// dependency graph is still validated either way (GAT-03) -- this flag only chooses which
    /// dispatch function runs AFTER validation passes.
    pub full_battery: bool,
}

/// The result of running the full probe battery: every [`ReportEntry`] (finding paired with its
/// producing probe's tier/hypothesis) plus the [`Strategy`] the battery ran with — both are
/// needed by [`report::render_evidence_report`].
pub struct BatteryResult {
    pub entries: Vec<ReportEntry>,
    pub strategy: Strategy,
}

/// Build the ordered Tier-1 probe battery and run it, returning the raw findings.
///
/// The three `OpencorporatesCrossJurisdiction` config gates, lifted out of `AppConfig` so the
/// battery can be built without one.
#[derive(Debug, Clone, Copy, Default)]
pub struct OcProbeGates {
    pub enabled: bool,
    pub oc_open_data_confirmed: bool,
    pub has_api_key: bool,
}

impl OcProbeGates {
    /// Read the gates from a loaded config. `.trim()` mirrors the real adapter's `is_enabled()`
    /// (`adapters/opencorporates.rs:69`) exactly. Without it a whitespace-only key makes the
    /// probe believe the adapter is live and report a verdict for a source that never runs.
    pub fn from_config(config: &urban_integrity_core::config::AppConfig) -> Self {
        Self {
            enabled: config.enricher.opencorporates.enabled,
            oc_open_data_confirmed: config.enricher.opencorporates.oc_open_data_confirmed,
            has_api_key: config
                .enricher
                .opencorporates
                .api_key
                .as_deref()
                .is_some_and(|k| !k.trim().is_empty()),
        }
    }
}

/// **The single registration site for the probe battery.**
///
/// `execute()` below and every test that needs the real battery call this — nothing rebuilds the
/// list. That is what makes `probe_oracle::compare`'s coverage check meaningful (CR-02,
/// the phase 38 code review): a probe added here is immediately visible to the oracle's
/// `CHECKED_PROBES`/`EXCLUDED_PROBES` reconciliation, instead of the tables being reconciled
/// against a hardcoded count or a hand-copied subset that drifts silently.
///
/// Registration order is load-bearing (PRB-06/success criterion 5): `IdentityIntegrity`
/// (position 2) must precede `SharedExactAddress` (position 4) so the `Strategy.identity_blocked`
/// write is visible before the read.
pub fn build_battery(
    catalog: Surreal<Any>,
    db_name: String,
    oc: OcProbeGates,
) -> Vec<Box<dyn Probe>> {
    vec![
        Box::new(PrecomputedAlerts),
        Box::new(IdentityIntegrity { catalog, db_name }),
        Box::new(DetectorReadiness),
        Box::new(SharedExactAddress),
        Box::new(GeographicConcentration),
        Box::new(WardConcentration),
        Box::new(TemporalBurst),
        // -- 12 Tier-2 probes, in exact OSINT_PROBES order (analyst_probe.py:2521) --
        Box::new(IdentityLinkRegistry),
        Box::new(EvasionAlerts),
        Box::new(DisqualifiedActiveInProcurement),
        Box::new(PscBeneficialOwnership),
        Box::new(GazetteInsolvencySignals),
        Box::new(PropertyNodes),
        Box::new(ElectoralCommissionDonations),
        Box::new(CouncilRegistersPoliticalActors),
        Box::new(OsintConfirmedIntel),
        Box::new(AnalystReviewQueue),
        Box::new(OsintNewNodeTypes),
        Box::new(DisqualifiedDirectorEdges),
        // -- 3 Cluster-A anchor probes, in exact OSINT_PROBES order (analyst_probe.py:2521) --
        Box::new(LandFlipCandidates),
        Box::new(DonationContractCorrelation),
        Box::new(OpencorporatesCrossJurisdiction {
            enabled: oc.enabled,
            oc_open_data_confirmed: oc.oc_open_data_confirmed,
            // `.trim()` is the caller's responsibility (mirrors the real adapter's
            // `is_enabled()`, `adapters/opencorporates.rs:69`, exactly). Without it a
            // whitespace-only key makes the probe believe the adapter is live and report a
            // verdict for a source that never actually runs.
            has_api_key: oc.has_api_key,
        }),
    ]
}

/// Every probe name the battery registers, in registration order. Lets a non-DB unit test
/// reconcile the oracle's coverage tables against the real registry.
#[cfg(test)]
pub fn registered_probe_names() -> Vec<&'static str> {
    build_battery(
        Surreal::init(),
        String::from("test"),
        OcProbeGates::default(),
    )
    .iter()
    .map(|p| p.name())
    .collect()
}

/// Validates the dependency graph UNCONDITIONALLY (GAT-03: a broken graph is a static defect of
/// the registered battery, independent of dispatch mode -- `--full-battery` must not bypass this
/// any more than the default gated path does), then dispatches either the full ungated battery
/// (`crate::run_probes`, unchanged by this entire phase) or the gated one (`graph::run_gated`), per
/// `full_battery`.
///
/// Extracted out of `execute()` specifically so it needs no `AppConfig::load()`/`connect_db()` of
/// its own -- `probes`/`db`/`st` are already-connected inputs -- which is what lets a non-DB unit
/// test call this directly and prove "an invalid graph fails before any probe runs" without going
/// through `execute()`'s DB-dependent setup. Mirrors `registered_probe_names()`'s existing
/// `#[cfg(test)]`-reachable pattern, except this function is real production code `execute()`
/// itself calls, not test-only.
pub(crate) async fn dispatch_battery(
    probes: &[Box<dyn Probe>],
    full_battery: bool,
    db: &Surreal<Any>,
    st: &mut Strategy,
) -> anyhow::Result<Vec<Finding>> {
    // GAT-03: validated BEFORE the mode branch, unconditionally. This is the fix for the
    // plan-checker's blocker -- the graph is validated on the --full-battery path too, which is
    // what makes the guarantee real for `probe-oracle` (probe_oracle.rs) and any future caller.
    let graph = crate::graph::validate_dependency_graph(probes)?;
    if full_battery {
        Ok(crate::run_probes(probes, db, st).await)
    } else {
        crate::graph::run_gated(probes, &graph, db, st).await
    }
}

/// The testable core of the probe battery — CLI callers (`crates/cli/src/commands/probe.rs`) wrap
/// this in table rendering and artifact writing. Registration order is load-bearing (PRB-06/success
/// criterion 5): `IdentityIntegrity` (position 2) must precede `SharedExactAddress` (position 4)
/// so the `Strategy.identity_blocked` write is visible before the read.
pub async fn execute(
    db: &Surreal<Any>,
    catalog: Surreal<Any>,
    db_name: String,
    oc: OcProbeGates,
    options: RunOptions,
) -> anyhow::Result<BatteryResult> {
    let probes = build_battery(catalog, db_name, oc);

    // Belt-and-braces completeness check (Plan 39-07's shared function): the CHECKED/EXCLUDED
    // partition must cover this battery exactly, and this fires on drift even if the
    // `probe-oracle` command is never invoked.
    crate::population_source::verify_registered_probes_covered(
        &probes.iter().map(|p| p.name()).collect::<Vec<_>>(),
    )?;

    let mut st = Strategy::default();
    // `probes` is passed by reference below — `dispatch_battery` does not consume it — so it
    // remains owned here afterwards for the `zip` that pairs each probe's tier/hypothesis with
    // its finding. Safe because both `run_probes` and `run_gated` iterate `probes` in order and
    // push exactly one `Finding` per probe (the same 1:1 ordering invariant
    // `dispatch_preserves_registration_order`, plan 31-03, already proves at the `run_probes`
    // level, and `run_gated_output_order_matches_input_slice_order...`, plan 41-03, proves at the
    // gated level).
    let findings = dispatch_battery(&probes, options.full_battery, db, &mut st).await?;

    // VRD-11/VRD-13 (PD-03): population provenance is computed unconditionally for every probe,
    // on the SAME connection the battery just used — never a second AppConfig::load()/connect_db()
    // (WR-05's fabricated-disagreement hazard). Order matches `probes`/`findings` 1:1.
    //
    // D-21: a gated-out probe gets no population provenance computed — it never ran, so there is
    // nothing to attribute. Special-cased BEFORE calling `for_probe`, which is otherwise untouched
    // (its own has_coverage short-circuit already returns SelfReported for DataGap/Blocked; this
    // mirrors that shape for Skipped without restructuring for_probe itself).
    let mut population_sources = Vec::with_capacity(probes.len());
    for (p, finding) in probes.iter().zip(findings.iter()) {
        let population_source = if matches!(finding.verdict, Verdict::Skipped { .. }) {
            PopulationSource::SelfReported {
                reason: "probe was gated out and never ran — no population to measure",
            }
        } else {
            crate::population_source::for_probe(p.name(), finding.verdict.coverage().is_some(), db)
                .await?
        };
        population_sources.push(population_source);
    }

    let entries: Vec<ReportEntry> = probes
        .iter()
        .zip(findings)
        .zip(population_sources)
        .map(|((p, finding), population_source)| ReportEntry {
            tier: p.tier(),
            hypothesis: p.hypothesis(),
            motif_kind: p.motif_kind(),
            finding,
            population_source,
        })
        .collect();
    Ok(BatteryResult {
        entries,
        strategy: st,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvidenceScope, MotifKind};
    use urban_integrity_schema::types::verdict::Severity;

    // ── Task 2: dispatch_battery() -- unconditional graph validation, D-21 provenance ────────

    /// `run()` panics unconditionally -- the concrete, non-vacuous proof that `dispatch_battery`
    /// never calls a probe's real `run()` on an invalid graph, under EITHER dispatch mode. A bug
    /// that let validation be skipped (the plan-checker's blocker on `--full-battery`) would make
    /// this test panic and fail loudly.
    struct PanicsIfRun(&'static str, &'static [&'static str]);

    #[async_trait::async_trait]
    impl Probe for PanicsIfRun {
        fn name(&self) -> &'static str {
            self.0
        }

        /// Test stubs are `AggregateOnly` — they emit no evidence at all, so nothing in them
        /// could ever name a node. Declaring it explicitly is the point of the method having no
        /// default (see `Probe::evidence_scope`).
        fn evidence_scope(&self) -> EvidenceScope {
            EvidenceScope::AggregateOnly
        }

        /// This stub is never exercised by the finding board, only by dispatch-ordering/strategy
        /// tests, so its motif classification is inert — `Absence` requires the least reasoning
        /// to verify.
        fn motif_kind(&self) -> MotifKind {
            MotifKind::Absence
        }

        async fn run(&self, _db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
            panic!(
                "{}'s run() was invoked -- dispatch_battery failed to validate the graph first",
                self.0
            );
        }

        fn tier(&self) -> &'static str {
            "Tier-1"
        }

        fn hypothesis(&self) -> &'static str {
            "stub -- must never actually run, the graph is invalid"
        }

        fn depends_on(&self) -> &'static [&'static str] {
            self.1
        }
    }

    #[tokio::test]
    async fn dispatch_battery_rejects_an_invalid_graph_under_both_dispatch_modes() {
        // A two-probe cycle -- invalid regardless of dispatch mode.
        let probes: Vec<Box<dyn Probe>> = vec![
            Box::new(PanicsIfRun("cycle-a", &["cycle-b"])),
            Box::new(PanicsIfRun("cycle-b", &["cycle-a"])),
        ];
        let db: Surreal<Any> = Surreal::init();

        let mut st = Strategy::default();
        // Not panicking here IS the proof: if dispatch_battery let this reach run_gated/gate(),
        // gate() would look up an upstream verdict that was never dispatched and panic on the
        // `.expect(...)` in graph.rs's gate(), or -- if validation were skipped entirely and
        // run_probes were called instead -- PanicsIfRun's own run() would panic directly.
        dispatch_battery(&probes, false, &db, &mut st)
            .await
            .expect_err("a cyclic graph must fail validation before any probe runs (gated mode)");

        let mut st2 = Strategy::default();
        // The blocker fix's core claim: --full-battery must not bypass validation either.
        dispatch_battery(&probes, true, &db, &mut st2)
            .await
            .expect_err(
            "a cyclic graph must fail validation before any probe runs (full-battery mode too -- \
             this is the plan-checker blocker fix)",
        );
    }

    /// An ordinary stub whose `run()` returns a fixed, non-`Skipped` `Verdict` -- used to confirm
    /// the `full_battery: true` pass-through still dispatches every probe for real after the
    /// refactor extracted this branch into `dispatch_battery`.
    struct OkStub(&'static str);

    #[async_trait::async_trait]
    impl Probe for OkStub {
        fn name(&self) -> &'static str {
            self.0
        }

        /// Test stubs are `AggregateOnly` — they emit no evidence at all, so nothing in them
        /// could ever name a node. Declaring it explicitly is the point of the method having no
        /// default (see `Probe::evidence_scope`).
        fn evidence_scope(&self) -> EvidenceScope {
            EvidenceScope::AggregateOnly
        }

        /// This stub is never exercised by the finding board, only by dispatch-ordering/strategy
        /// tests, so its motif classification is inert — `Absence` requires the least reasoning
        /// to verify.
        fn motif_kind(&self) -> MotifKind {
            MotifKind::Absence
        }

        async fn run(&self, _db: &Surreal<Any>, _st: &mut Strategy) -> Finding {
            Finding {
                probe: self.0.to_string(),
                verdict: Verdict::classify(5, 5, 0, 1, ""),
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
            "stub -- ordinary, expected to actually run"
        }
    }

    #[tokio::test]
    async fn dispatch_battery_runs_every_probe_under_full_battery() {
        let probes: Vec<Box<dyn Probe>> =
            vec![Box::new(OkStub("stub-x")), Box::new(OkStub("stub-y"))];
        let db: Surreal<Any> = Surreal::init();
        let mut st = Strategy::default();

        let findings = dispatch_battery(&probes, true, &db, &mut st)
            .await
            .expect("a valid graph with full_battery=true must dispatch via run_probes");

        assert_eq!(findings.len(), 2, "one Finding per registered probe");
        for f in &findings {
            assert!(
                !matches!(f.verdict, Verdict::Skipped { .. }),
                "full-battery dispatch must never gate a probe out, got {:?}",
                f.verdict
            );
        }
        assert_eq!(
            findings
                .iter()
                .map(|f| f.probe.as_str())
                .collect::<Vec<_>>(),
            vec!["stub-x", "stub-y"],
            "full-battery dispatch must preserve registration order"
        );
    }

    /// D-21: a gated-out probe's `Finding` carries `Verdict::Skipped`, and `execute()`'s
    /// population-provenance loop special-cases that BEFORE calling `for_probe` -- this pins the
    /// exact branch logic `execute()` uses directly, with no live `db` required, since the
    /// `Skipped` arm never reaches `for_probe`'s DB-touching path.
    #[test]
    fn skipped_verdict_yields_self_reported_population_source_without_touching_db() {
        let upstream = Verdict::classify(10, 10, 0, 1, "");
        let skipped = Verdict::skipped_dependency("identity-integrity", &upstream);
        let finding = Finding {
            probe: "address-cluster-exact".to_string(),
            summary: "Skipped — gated out".to_string(),
            verdict: skipped,
            cause: None,
            severity: Severity::Info,
            evidence: vec![],
            attributions: vec![],
            remediation_hint: None,
        };

        // Mirrors execute()'s D-21 branch exactly.
        let population_source = if matches!(finding.verdict, Verdict::Skipped { .. }) {
            PopulationSource::SelfReported {
                reason: "probe was gated out and never ran — no population to measure",
            }
        } else {
            unreachable!("this test's finding.verdict is always Verdict::Skipped");
        };

        match population_source {
            PopulationSource::SelfReported { reason } => assert_eq!(
                reason,
                "probe was gated out and never ran — no population to measure"
            ),
            other => panic!("expected SelfReported, got {other:?}"),
        }
    }
}
