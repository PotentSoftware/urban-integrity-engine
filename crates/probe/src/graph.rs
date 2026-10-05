//! Probe dependency graph: declaration, startup validation, and stable topological ordering
//! (D-12..D-16, GAT-03). The gate predicate and gated dispatch loop are a later, separate addition
//! to this file (see this module's doc comment once that lands) -- this plan ships graph
//! STRUCTURE only: which probe depends on which, is it acyclic, and what order do dependencies
//! imply.
//!
//! Deliberately does NOT encode D-08's Tier-1 -> Tier-2 OR rule as graph edges -- that rule lives
//! in the gate predicate (a later addition), evaluated against `Probe::tier()`, not against this
//! graph. Modelling it as 84 synthetic edges here would be exactly the "invented edges" the design notes
//! rejected for the full-22-probe-graph option (D-14).

use std::collections::HashMap;

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_schema::types::verdict::{Finding, Severity, Verdict, VerdictKind};

use super::{Probe, Strategy};

/// The declared-edge dependency graph (D-14: today exactly one real edge,
/// "address-cluster-exact" depends on "identity-integrity") plus the stable topological dispatch
/// order it implies (D-15/D-16).
#[derive(Debug)]
pub struct DependencyGraph {
    edges: HashMap<&'static str, &'static [&'static str]>,
    /// Indices into the `probes` slice this graph was built from, in dispatch order.
    order: Vec<usize>,
    /// Names of the `probes` slice this graph was built from, in that slice's own order (NOT
    /// dispatch order). The identity `ensure_built_for` checks against -- see WR-02 there.
    names: Vec<&'static str>,
}

impl DependencyGraph {
    /// `probe_name`'s declared dependencies, or `&[]` if it declares none (or is unknown to this
    /// graph). `validate_dependency_graph` inserts an entry for EVERY probe it saw, so the
    /// unknown-name fallback is unreachable for any slice `ensure_built_for` has accepted --
    /// which `run_gated` calls before dispatching.
    pub fn depends_on(&self, probe_name: &str) -> &'static [&'static str] {
        self.edges.get(probe_name).copied().unwrap_or(&[])
    }

    /// Fail loud unless `probes` is the exact slice this graph was built from (WR-02).
    ///
    /// `run_gated` indexes `probes` with `dispatch_order()` indices computed against the slice
    /// `validate_dependency_graph` saw, and `depends_on()` answers `&[]` for any name this graph
    /// does not know. Pairing a stale or cached graph with a different slice therefore fails one
    /// of two ways: an out-of-bounds index panic if the slice is shorter, or -- silently, with no
    /// error and no log line -- a probe absent from `edges` appearing to declare no dependencies
    /// at all, so it runs unconditionally. That second mode is the mirror image of WR-01:
    /// evidence included that gating should have excluded.
    ///
    /// Compares names positionally rather than just lengths, so a reordered or substituted slice
    /// of the same size is caught too. `anyhow::bail!` rather than `debug_assert!` because this
    /// workspace's implicit release profile has `debug-assertions` off (the same reasoning
    /// `probe_oracle.rs` records for its own checks), which would leave the check absent exactly
    /// where it matters.
    pub fn ensure_built_for(&self, probes: &[Box<dyn Probe>]) -> anyhow::Result<()> {
        if probes.len() != self.names.len() {
            anyhow::bail!(
                "dependency graph was built for {} probes but run against {} -- the graph and \
                 the probe slice have drifted apart; rebuild it with \
                 validate_dependency_graph() against the slice actually being dispatched",
                self.names.len(),
                probes.len()
            );
        }
        for (position, probe) in probes.iter().enumerate() {
            if probe.name() != self.names[position] {
                anyhow::bail!(
                    "dependency graph was built for a different probe slice: position \
                     {position} is '{}' now but was '{}' when the graph was built -- rebuild it \
                     with validate_dependency_graph() against the slice actually being dispatched",
                    probe.name(),
                    self.names[position]
                );
            }
        }
        Ok(())
    }

    /// Dispatch order as indices into the `probes` slice this graph was built from.
    pub fn dispatch_order(&self) -> &[usize] {
        &self.order
    }
}

/// Kahn's algorithm run to completion by hand (the research notes' Pitfall 8: `petgraph::algo::toposort`'s
/// edgeless-node ordering is not a documented stability guarantee, so it cannot be relied on for
/// D-16's registration-order tiebreak). The ready queue is a `Vec` re-sorted by registration index
/// every pass -- never a `HashSet` -- so two probes with no path between them always emerge in
/// registration order, and a declared edge that violates registration order genuinely reorders
/// them.
///
/// Returns `(dispatch order, indices that never reached in-degree zero)`. The second element is
/// non-empty exactly when a cycle exists among those indices.
fn kahns_topological_order(
    names: &[&'static str],
    edges: &HashMap<&'static str, &'static [&'static str]>,
) -> (Vec<usize>, Vec<usize>) {
    let n = names.len();
    let index_of: HashMap<&'static str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, &name)| (name, i))
        .collect();

    // in_degree[i] = number of `i`'s own declared dependencies not yet resolved.
    let mut in_degree = vec![0usize; n];
    // dependents[i] = indices of probes that declare a dependency on probe i -- resolving i must
    // decrement each of these.
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, &name) in names.iter().enumerate() {
        let deps = edges.get(name).copied().unwrap_or(&[]);
        in_degree[i] = deps.len();
        for dep in deps {
            // Every name here has already been validated as a registered probe by
            // `validate_dependency_graph` before this function is ever called.
            let &dep_idx = index_of
                .get(dep)
                .expect("dependency names are validated as registered before topological sort");
            dependents[dep_idx].push(i);
        }
    }

    let mut ready: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    let mut resolved = vec![false; n];

    while !ready.is_empty() {
        // Re-sorted every pass (not a HashSet) so the smallest registration index always wins
        // among the currently-ready probes -- D-16's stability guarantee.
        ready.sort();
        let next = ready.remove(0);
        order.push(next);
        resolved[next] = true;
        for &dependent in &dependents[next] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                ready.push(dependent);
            }
        }
    }

    let unresolved: Vec<usize> = (0..n).filter(|&i| !resolved[i]).collect();
    (order, unresolved)
}

/// `Some(order)` on a fully-resolved acyclic graph, `None` if a cycle prevented some probes from
/// ever reaching in-degree zero.
fn stable_topological_order(
    names: &[&'static str],
    edges: &HashMap<&'static str, &'static [&'static str]>,
) -> Option<Vec<usize>> {
    let (order, unresolved) = kahns_topological_order(names, edges);
    if unresolved.is_empty() {
        Some(order)
    } else {
        None
    }
}

/// Build and validate the dependency graph for `probes`. `Err` on a cycle or a dependency naming
/// an unregistered probe -- called from `execute()` before any probe runs (a later plan wires this
/// call site; this plan only ships the function). Mirrors
/// `population_source::verify_registered_probes_covered`'s shape and fail-loud posture exactly.
pub fn validate_dependency_graph(probes: &[Box<dyn Probe>]) -> anyhow::Result<DependencyGraph> {
    let names: Vec<&'static str> = probes.iter().map(|p| p.name()).collect();
    let mut edges: HashMap<&'static str, &'static [&'static str]> = HashMap::new();

    for probe in probes {
        let deps = probe.depends_on();
        for &dep in deps {
            if !names.contains(&dep) {
                anyhow::bail!(
                    "probe '{}' declares depends_on() = \"{dep}\", which is not a registered \
                     probe name -- the dependency was renamed, removed, or never registered",
                    probe.name()
                );
            }
        }
        edges.insert(probe.name(), deps);
    }

    match stable_topological_order(&names, &edges) {
        Some(order) => {
            reject_interleaved_tiers(&names, &order, probes)?;
            Ok(DependencyGraph {
                edges,
                order,
                names,
            })
        }
        None => {
            // Recompute the unresolved set rather than emitting a bare "cycle exists" message
            // (the project's fail-loud-with-actionable-detail convention).
            let (_, unresolved) = kahns_topological_order(&names, &edges);
            let unresolved_names: Vec<&str> = unresolved.iter().map(|&i| names[i]).collect();
            anyhow::bail!(
                "dependency cycle detected among probes: {} -- their depends_on() declarations \
                 never reach a resolvable order",
                unresolved_names.join(", ")
            );
        }
    }
}

/// Reject a dispatch order that interleaves the tiers (GAT-03).
///
/// `gate()`'s D-08 exception reads the accumulator of findings dispatched SO FAR, so it is a
/// faithful reading of "did ANY Tier-1 probe return Hit/Weak" only if every Tier-1 probe has
/// already dispatched by the time the first Tier-2 probe is gated. Nothing in Kahn's algorithm
/// enforces that: two probes with no path between them emerge in registration order (D-16), so a
/// registration order interleaving the tiers would make `gate()` under-count `probes_checked` and,
/// worse, silently skip a Tier-2 probe that a not-yet-dispatched Tier-1 probe would have unlocked
/// -- the "wrong predicate silently suppresses evidence" failure this phase exists to prevent.
///
/// Checked here, alongside the cycle and unregistered-dependency checks, rather than left to
/// registration-order luck backed only by a test over the one production battery.
///
/// Vacuous unless BOTH tiers are present: a battery with no Tier-1 probes gates every Tier-2 probe
/// out on an empty checked-set, which is D-08 working as specified, not an ordering defect.
fn reject_interleaved_tiers(
    names: &[&'static str],
    order: &[usize],
    probes: &[Box<dyn Probe>],
) -> anyhow::Result<()> {
    let tiers: Vec<&'static str> = order.iter().map(|&i| probes[i].tier()).collect();
    let (Some(last_tier1), Some(first_tier2)) = (
        tiers.iter().rposition(|&t| t == "Tier-1"),
        tiers.iter().position(|&t| t == "Tier-2"),
    ) else {
        return Ok(());
    };
    if last_tier1 > first_tier2 {
        anyhow::bail!(
            "dispatch order interleaves the tiers: Tier-2 probe '{}' dispatches at position \
             {first_tier2}, before Tier-1 probe '{}' at position {last_tier1} -- D-08's Tier-1 \
             -> Tier-2 OR rule reads only the probes dispatched so far, so it would gate '{}' \
             out against an incomplete Tier-1 result set. Register every Tier-1 probe before \
             every Tier-2 probe in build_battery()",
            names[order[first_tier2]],
            names[order[last_tier1]],
            names[order[first_tier2]],
        );
    }
    Ok(())
}

/// The gate predicate (D-01..D-08, GAT-01). `completed` is every (probe name, tier, verdict)
/// already dispatched THIS run, in dispatch order -- toposort guarantees every declared dependency
/// of `probe` is already present. Returns `Ok(())` if `probe` should run for real. Returns
/// `Err(Verdict)` -- an ALREADY-CONSTRUCTED `Verdict::Skipped`, built here where the real upstream
/// Finding is in scope -- if gating applies. Constructing the Verdict here (not in the caller, from
/// a weaker signal) is what keeps the "cannot name a cause that did not gate it" guarantee real.
fn gate(
    probe: &dyn Probe,
    graph: &DependencyGraph,
    completed: &[(&'static str, &'static str, Verdict)],
) -> Result<(), Verdict> {
    // 1. Declared deps, D-07's default AND: every named dependency must be Hit/Weak.
    for &dep_name in graph.depends_on(probe.name()) {
        let (_, _, dep_verdict) = completed
            .iter()
            .find(|(name, _, _)| *name == dep_name)
            .expect("toposort guarantees declared deps dispatch before their dependents");
        if !matches!(
            VerdictKind::of(dep_verdict),
            VerdictKind::Hit | VerdictKind::Weak
        ) {
            return Err(Verdict::skipped_dependency(dep_name, dep_verdict));
        }
    }
    // 2. D-08's ONE exception: Tier-2 probes gate on OR across Tier-1, not AND. This is gate-time
    // logic, not a graph edge (D-14) -- read `probe.tier()`/`completed` directly, never `graph`.
    if probe.tier() == "Tier-2" {
        let tier1_checked: Vec<&Verdict> = completed
            .iter()
            .filter(|(_, tier, _)| *tier == "Tier-1")
            .map(|(_, _, v)| v)
            .collect();
        let any_pass = tier1_checked
            .iter()
            .any(|v| matches!(VerdictKind::of(v), VerdictKind::Hit | VerdictKind::Weak));
        if !any_pass {
            return Err(Verdict::skipped_tier_aggregate(
                "Tier-1",
                tier1_checked.len() as u8,
            ));
        }
    }
    Ok(())
}

/// The gated alternative to `run_probes` (`mod.rs`). Dispatches in `graph.dispatch_order()`
/// (topological) order internally, evaluating `gate()` against findings accumulated so far, but
/// returns the resulting `Vec<Finding>` in the SAME order as the input `probes` slice -- so
/// `execute()`'s existing `probes.iter().zip(findings)` zip logic needs no change downstream of
/// this call. `run_probes` itself is not modified anywhere in this phase; only `execute()`'s
/// default path (a later plan) calls this function instead.
pub async fn run_gated(
    probes: &[Box<dyn Probe>],
    graph: &DependencyGraph,
    db: &Surreal<Any>,
    st: &mut Strategy,
) -> anyhow::Result<Vec<Finding>> {
    // WR-02: `graph` and `probes` are independent parameters, so nothing in the signature stops a
    // caller pairing a cached graph with a different slice. Refuse to dispatch on a mismatch
    // rather than indexing out of bounds or silently under-gating.
    graph.ensure_built_for(probes)?;
    // Accumulator in DISPATCH order, for gate()'s completed-lookup. Toposort guarantees every
    // declared dependency of a probe already has an entry here by the time that probe is visited.
    let mut completed: Vec<(&'static str, &'static str, Verdict)> =
        Vec::with_capacity(probes.len());
    // Indexed by ORIGINAL position in `probes` -- filled in dispatch order, read out in input
    // order, so the returned Vec<Finding> matches `probes`' order regardless of internal dispatch
    // order.
    let mut slots: Vec<Option<Finding>> = (0..probes.len()).map(|_| None).collect();

    for &idx in graph.dispatch_order() {
        let probe = &probes[idx];
        let finding = match gate(probe.as_ref(), graph, &completed) {
            Ok(()) => probe.run(db, st).await,
            Err(verdict) => {
                // Destructure the Verdict::Skipped gate() always returns in its Err arm to build
                // the synthesized Finding's summary, per this plan's <interfaces> note.
                let cause_description = match &verdict {
                    Verdict::Skipped { cause } => cause.describe(),
                    _ => unreachable!("gate() only ever returns Verdict::Skipped in its Err arm"),
                };
                Finding {
                    probe: probe.name().to_string(),
                    summary: format!("Skipped — {cause_description}"),
                    verdict,
                    cause: None,
                    severity: Severity::Info,
                    evidence: vec![],
                    attributions: vec![],
                    remediation_hint: None,
                }
            }
        };
        completed.push((probe.name(), probe.tier(), finding.verdict.clone()));
        slots[idx] = Some(finding);
    }

    Ok(slots
        .into_iter()
        .map(|f| f.expect("every dispatch-order index is visited exactly once"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvidenceScope, MotifKind};
    use urban_integrity_schema::types::verdict::{Finding, Severity, Verdict};

    struct StubProbe {
        name: &'static str,
        tier: &'static str,
        deps: &'static [&'static str],
    }

    #[async_trait::async_trait]
    impl Probe for StubProbe {
        fn name(&self) -> &'static str {
            self.name
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

        async fn run(&self, _db: &Surreal<Any>, _st: &mut super::super::Strategy) -> Finding {
            Finding {
                probe: self.name.to_string(),
                verdict: Verdict::data_gap("stub, no real data"),
                summary: format!("{} ran", self.name),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            }
        }

        fn tier(&self) -> &'static str {
            self.tier
        }

        fn hypothesis(&self) -> &'static str {
            "stub -- synthetic, no real hypothesis"
        }

        fn depends_on(&self) -> &'static [&'static str] {
            self.deps
        }
    }

    fn stub(name: &'static str, deps: &'static [&'static str]) -> Box<dyn Probe> {
        Box::new(StubProbe {
            name,
            tier: "Tier-1",
            deps,
        })
    }

    fn stub_tier2(name: &'static str, deps: &'static [&'static str]) -> Box<dyn Probe> {
        Box::new(StubProbe {
            name,
            tier: "Tier-2",
            deps,
        })
    }

    #[test]
    fn dependency_on_an_unregistered_probe_is_rejected() {
        let probes: Vec<Box<dyn Probe>> =
            vec![stub("phantom-dep-declarer", &["nonexistent-probe"])];
        let err = validate_dependency_graph(&probes).expect_err(
            "a depends_on() edge naming an unregistered probe must fail validation, not be \
             silently accepted",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("phantom-dep-declarer"),
            "error must name the declaring probe, got: {msg}"
        );
        assert!(
            msg.contains("nonexistent-probe"),
            "error must name the missing dependency, got: {msg}"
        );
    }

    #[test]
    fn a_two_probe_cycle_is_rejected() {
        let probes: Vec<Box<dyn Probe>> = vec![
            stub("cycle-alpha", &["cycle-beta"]),
            stub("cycle-beta", &["cycle-alpha"]),
        ];
        let err = validate_dependency_graph(&probes)
            .expect_err("a cycle must fail validation, not be silently accepted");
        let msg = err.to_string();
        assert!(
            msg.contains("cycle-alpha") || msg.contains("cycle-beta"),
            "error must name at least one of the cyclic probes, got: {msg}"
        );
    }

    #[test]
    fn real_shipped_battery_validates_ok_with_exactly_one_real_edge() {
        let probes = super::super::run::build_battery(
            Surreal::init(),
            "test".to_string(),
            super::super::run::OcProbeGates::default(),
        );
        let graph = validate_dependency_graph(&probes)
            .expect("the real shipped 22-probe battery must validate successfully");

        let non_empty: Vec<(&str, &[&str])> = probes
            .iter()
            .filter_map(|p| {
                let deps = graph.depends_on(p.name());
                (!deps.is_empty()).then_some((p.name(), deps))
            })
            .collect();

        assert!(
            non_empty.is_empty(),
            "the production battery must declare NO dependency edges. D-14's single edge \
             (address-cluster-exact -> identity-integrity) was removed 2026-09-03: \
             identity-integrity cannot return Hit/Weak, so D-07's gate could never open and the \
             dependent probe was permanently unreachable under the default dispatch. Before \
             adding an edge here, check that the named upstream can actually return Hit or Weak, \
             and that upstream SIGNAL (not upstream data quality) is what makes the dependent \
             meaningful -- see `Probe::depends_on`'s doc comment. Got: {non_empty:?}"
        );
    }

    #[test]
    fn independent_probes_keep_registration_order_when_no_dependency_exists() {
        let probes: Vec<Box<dyn Probe>> = vec![
            stub("indep-a", &[]),
            stub("indep-b", &[]),
            stub("indep-c", &[]),
        ];
        let graph = validate_dependency_graph(&probes).expect("no cycle, no unregistered dep");
        assert_eq!(
            graph.dispatch_order(),
            &[0, 1, 2],
            "probes with no path between them must keep registration order (D-16)"
        );
    }

    #[test]
    fn a_declared_edge_that_violates_registration_order_genuinely_reorders_dispatch() {
        // Downstream (index 0) depends on Upstream (index 1) -- the opposite of registration
        // order. Proves the sort genuinely reorders when an edge requires it, not merely
        // coincidentally matching registration order (the research notes' Open Question 2).
        let probes: Vec<Box<dyn Probe>> =
            vec![stub("downstream", &["upstream"]), stub("upstream", &[])];
        let graph = validate_dependency_graph(&probes).expect("no cycle, no unregistered dep");
        assert_eq!(
            graph.dispatch_order(),
            &[1, 0],
            "upstream (index 1) must dispatch before downstream (index 0), since downstream \
             declares a dependency on it"
        );
    }

    #[tokio::test]
    async fn run_gated_refuses_a_graph_built_for_a_shorter_slice() {
        // WR-02 mode (a): the graph's dispatch_order() indexes past the end of `probes`, which
        // previously panicked with an out-of-bounds index.
        let full: Vec<Box<dyn Probe>> = vec![stub("alpha", &[]), stub("beta", &[])];
        let graph = validate_dependency_graph(&full).expect("two independent stubs must validate");

        let filtered: Vec<Box<dyn Probe>> = vec![stub("alpha", &[])];
        let db: Surreal<Any> = Surreal::init();
        let mut st = super::super::Strategy::default();
        let err = run_gated(&filtered, &graph, &db, &mut st)
            .await
            .expect_err("a graph built for 2 probes must not dispatch against a slice of 1");
        let msg = err.to_string();
        assert!(
            msg.contains('2') && msg.contains('1'),
            "error must report both the built-for and run-against counts, got: {msg}"
        );
    }

    #[tokio::test]
    async fn run_gated_refuses_a_same_length_but_reordered_slice() {
        // WR-02 mode (b), the silent one: a same-length slice whose probes the graph does not
        // know makes depends_on() answer `&[]`, so a probe with real declared dependencies would
        // run unconditionally -- evidence included that gating should have excluded, with no
        // error. A length-only check would miss this entirely.
        let built_for: Vec<Box<dyn Probe>> = vec![stub("alpha", &[]), stub("beta", &["alpha"])];
        let graph =
            validate_dependency_graph(&built_for).expect("a single real edge must validate");

        let substituted: Vec<Box<dyn Probe>> = vec![stub("alpha", &[]), stub("gamma", &["alpha"])];
        let db: Surreal<Any> = Surreal::init();
        let mut st = super::super::Strategy::default();
        let err = run_gated(&substituted, &graph, &db, &mut st)
            .await
            .expect_err("a slice of the same length but different probes must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("gamma") && msg.contains("beta"),
            "error must name both the probe now at the position and the one expected there, \
             got: {msg}"
        );
    }

    #[test]
    fn ensure_built_for_accepts_the_slice_the_graph_was_built_from() {
        // Guards against the check being vacuous or over-eager -- the happy path every real
        // caller takes must still pass.
        let probes: Vec<Box<dyn Probe>> = vec![stub("alpha", &[]), stub("beta", &["alpha"])];
        let graph = validate_dependency_graph(&probes).expect("a single real edge must validate");
        graph
            .ensure_built_for(&probes)
            .expect("the slice the graph was built from must be accepted");
    }

    #[test]
    fn a_battery_interleaving_the_tiers_is_rejected() {
        // WR-01's exact scenario: Tier1-A, Tier2-X, Tier1-B registered in that order with no
        // declared edges. Kahn's dispatches in plain registration order, so Tier2-X would be
        // gated against a Tier-1 set missing Tier1-B entirely.
        let probes: Vec<Box<dyn Probe>> = vec![
            stub("tier1-a", &[]),
            stub_tier2("tier2-x", &[]),
            stub("tier1-b", &[]),
        ];
        let err = validate_dependency_graph(&probes).expect_err(
            "a registration order interleaving Tier-1 and Tier-2 must fail validation -- D-08's \
             OR rule would otherwise read an incomplete Tier-1 result set",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("tier2-x"),
            "error must name the Tier-2 probe that would be gated too early, got: {msg}"
        );
        assert!(
            msg.contains("tier1-b"),
            "error must name the Tier-1 probe that dispatches too late, got: {msg}"
        );
    }

    #[test]
    fn a_battery_with_both_tiers_correctly_ordered_is_accepted() {
        // Guards against the check being vacuous or over-eager: the well-ordered form of the
        // battery above must still validate.
        let probes: Vec<Box<dyn Probe>> = vec![
            stub("tier1-a", &[]),
            stub("tier1-b", &[]),
            stub_tier2("tier2-x", &[]),
        ];
        validate_dependency_graph(&probes)
            .expect("Tier-1 probes registered before Tier-2 probes must validate successfully");
    }

    #[test]
    fn a_tier2_only_battery_is_accepted() {
        // The check is deliberately vacuous with no Tier-1 probes present: gating every Tier-2
        // probe out against an empty checked-set is D-08 as specified, not an ordering defect.
        let probes: Vec<Box<dyn Probe>> =
            vec![stub_tier2("tier2-x", &[]), stub_tier2("tier2-y", &[])];
        validate_dependency_graph(&probes)
            .expect("a battery with no Tier-1 probes has no tier-ordering invariant to violate");
    }

    #[test]
    fn real_battery_every_tier1_dispatch_position_precedes_every_tier2_dispatch_position() {
        let probes = super::super::run::build_battery(
            Surreal::init(),
            "test".to_string(),
            super::super::run::OcProbeGates::default(),
        );
        let graph = validate_dependency_graph(&probes)
            .expect("the real shipped 22-probe battery must validate successfully");

        let last_tier1 = graph
            .dispatch_order()
            .iter()
            .map(|&i| probes[i].tier())
            .rposition(|t| t == "Tier-1")
            .expect("the real battery must contain at least one Tier-1 probe");
        let first_tier2 = graph
            .dispatch_order()
            .iter()
            .map(|&i| probes[i].tier())
            .position(|t| t == "Tier-2")
            .expect("the real battery must contain at least one Tier-2 probe");

        assert!(
            last_tier1 < first_tier2,
            "every Tier-1 probe's dispatch position must precede every Tier-2 probe's dispatch \
             position -- last Tier-1 at {last_tier1}, first Tier-2 at {first_tier2}"
        );
    }

    // ── Task 1: gate() truth table (D-01..D-08) ─────────────────────────────────────────────

    #[test]
    fn gate_returns_ok_when_upstream_hit_or_weak() {
        let probes: Vec<Box<dyn Probe>> =
            vec![stub("upstream", &[]), stub("downstream", &["upstream"])];
        let graph = validate_dependency_graph(&probes).expect("no cycle, no unregistered dep");
        let downstream = probes
            .iter()
            .find(|p| p.name() == "downstream")
            .expect("downstream is registered");

        let hit = Verdict::classify(5, 5, 5, 1, "");
        let completed = vec![("upstream", "Tier-1", hit)];
        assert!(
            gate(downstream.as_ref(), &graph, &completed).is_ok(),
            "upstream Hit must let the dependent run"
        );

        let weak = Verdict::classify(5, 5, 3, 5, "");
        let completed = vec![("upstream", "Tier-1", weak)];
        assert!(
            gate(downstream.as_ref(), &graph, &completed).is_ok(),
            "upstream Weak must let the dependent run"
        );
    }

    #[test]
    fn gate_returns_err_skipped_for_each_non_passing_upstream_verdict() {
        let probes: Vec<Box<dyn Probe>> =
            vec![stub("upstream", &[]), stub("downstream", &["upstream"])];
        let graph = validate_dependency_graph(&probes).expect("no cycle, no unregistered dep");
        let downstream = probes
            .iter()
            .find(|p| p.name() == "downstream")
            .expect("downstream is registered");

        let cases: Vec<(Verdict, VerdictKind)> = vec![
            (Verdict::classify(10, 10, 0, 1, ""), VerdictKind::Clear),
            (
                Verdict::classify(10, 7, 0, 1, "3 structurally excluded"),
                VerdictKind::NotApplicable,
            ),
            (Verdict::data_gap("missing input"), VerdictKind::DataGap),
            (Verdict::blocked("prerequisite"), VerdictKind::Blocked),
        ];

        for (upstream_verdict, expected_kind) in cases {
            let completed = vec![("upstream", "Tier-1", upstream_verdict)];
            let err = gate(downstream.as_ref(), &graph, &completed).expect_err(
                "a non-Hit/Weak upstream verdict must gate the dependent out (D-01..D-07)",
            );
            let Verdict::Skipped { cause } = err else {
                panic!("gate()'s Err arm must always be Verdict::Skipped");
            };
            assert_eq!(
                cause.upstream(),
                Some("upstream"),
                "must name the DIRECT upstream (D-05)"
            );
            assert_eq!(
                cause.upstream_verdict(),
                Some(expected_kind),
                "cause.upstream_verdict() must match the upstream's actual VerdictKind"
            );
        }
    }

    #[test]
    fn tier2_probe_with_no_declared_deps_gates_via_d08_or_exception() {
        let probes: Vec<Box<dyn Probe>> = vec![stub_tier2("tier2-probe", &[])];
        let graph = validate_dependency_graph(&probes).expect("no cycle, no unregistered dep");
        let tier2 = probes
            .iter()
            .find(|p| p.name() == "tier2-probe")
            .expect("tier2-probe is registered");

        // Zero Tier-1 probes checked so far -> gated out.
        let completed: Vec<(&'static str, &'static str, Verdict)> = vec![];
        let err = gate(tier2.as_ref(), &graph, &completed)
            .expect_err("no Tier-1 probe checked yet, must gate out via D-08");
        let Verdict::Skipped { cause } = err else {
            panic!("gate()'s Err arm must always be Verdict::Skipped");
        };
        assert_eq!(cause.tier(), Some("Tier-1"));
        assert_eq!(cause.probes_checked(), Some(0));

        // All Tier-1 probes checked so far failed the gate -> still gated out; probes_checked
        // reflects how many were actually checked.
        let completed = vec![
            ("t1-a", "Tier-1", Verdict::classify(10, 10, 0, 1, "")),
            ("t1-b", "Tier-1", Verdict::data_gap("x")),
        ];
        let err = gate(tier2.as_ref(), &graph, &completed)
            .expect_err("all checked Tier-1 probes failed the gate, must gate out via D-08");
        let Verdict::Skipped { cause } = err else {
            panic!("gate()'s Err arm must always be Verdict::Skipped");
        };
        assert_eq!(cause.probes_checked(), Some(2));

        // At least one Tier-1 probe returned Hit/Weak among others that did not -> runs,
        // regardless of how many other Tier-1 probes returned something else (D-08's OR, not AND).
        let completed = vec![
            ("t1-a", "Tier-1", Verdict::classify(10, 10, 0, 1, "")),
            ("t1-b", "Tier-1", Verdict::classify(5, 5, 5, 1, "")),
        ];
        assert!(
            gate(tier2.as_ref(), &graph, &completed).is_ok(),
            "at least one Tier-1 Hit/Weak must let the Tier-2 probe run"
        );
    }

    #[test]
    fn skips_cascade_transitively_and_each_hop_names_its_direct_upstream() {
        // A (Clear) -> B depends_on ["a"] -> C depends_on ["b"]. Proves D-04 (cascade falls out
        // with no separate logic -- Skipped is not Hit/Weak, so it propagates through gate()'s
        // ordinary AND check) and D-05 (each skip names its DIRECT upstream, not the root cause
        // several hops back) in one test.
        let probes: Vec<Box<dyn Probe>> =
            vec![stub("a", &[]), stub("b", &["a"]), stub("c", &["b"])];
        let graph = validate_dependency_graph(&probes).expect("no cycle, no unregistered dep");
        let b = probes.iter().find(|p| p.name() == "b").expect("b exists");
        let c = probes.iter().find(|p| p.name() == "c").expect("c exists");

        let a_clear = Verdict::classify(10, 10, 0, 1, "");
        let completed_for_b = vec![("a", "Tier-1", a_clear.clone())];
        let b_err = gate(b.as_ref(), &graph, &completed_for_b)
            .expect_err("a returned Clear, b must gate out");
        let Verdict::Skipped { cause: b_cause } = b_err else {
            panic!("gate()'s Err arm must always be Verdict::Skipped");
        };
        assert_eq!(
            b_cause.upstream(),
            Some("a"),
            "b's skip must name its direct upstream 'a'"
        );
        assert_eq!(b_cause.upstream_verdict(), Some(VerdictKind::Clear));

        // b's own real recorded verdict this run is the Skipped just proven above -- feed that
        // real Verdict, not a hand-asserted one, into c's completed list.
        let b_skipped = Verdict::skipped_dependency("a", &a_clear);
        let completed_for_c = vec![("a", "Tier-1", a_clear.clone()), ("b", "Tier-1", b_skipped)];
        let c_err = gate(c.as_ref(), &graph, &completed_for_c)
            .expect_err("b was skipped (not Hit/Weak), c must also gate out");
        let Verdict::Skipped { cause: c_cause } = c_err else {
            panic!("gate()'s Err arm must always be Verdict::Skipped");
        };
        assert_eq!(
            c_cause.upstream(),
            Some("b"),
            "c's skip must name its DIRECT upstream 'b', not the root cause 'a' several hops back"
        );
        assert_eq!(
            c_cause.upstream_verdict(),
            Some(VerdictKind::Skipped),
            "c's skip must report b's actual kind (Skipped), not re-derive a's Clear"
        );
    }

    // ── Task 2: run_gated() dispatch -- non-vacuity proof + 1:1 ordering invariant ──────────

    /// Always returns `Verdict::Clear` when run -- used as the upstream that gates
    /// `PanicIfRunProbe` out.
    struct GatekeeperProbe;

    #[async_trait::async_trait]
    impl Probe for GatekeeperProbe {
        fn name(&self) -> &'static str {
            "gatekeeper"
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

        async fn run(&self, _db: &Surreal<Any>, _st: &mut super::super::Strategy) -> Finding {
            Finding {
                probe: self.name().to_string(),
                verdict: Verdict::classify(10, 10, 0, 1, ""),
                summary: "gatekeeper ran, returned Clear".to_string(),
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
            "stub -- always returns Clear so its dependent must gate out"
        }
    }

    /// `run()` panics unconditionally -- the concrete, non-vacuous proof that `run_gated` never
    /// calls a gated-out probe's real `run()`. A bug that made `run_gated` call `.run()`
    /// unconditionally would make this test panic and fail loudly (this plan's <behavior> note).
    struct PanicIfRunProbe {
        deps: &'static [&'static str],
    }

    #[async_trait::async_trait]
    impl Probe for PanicIfRunProbe {
        fn name(&self) -> &'static str {
            "panics-if-run"
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

        async fn run(&self, _db: &Surreal<Any>, _st: &mut super::super::Strategy) -> Finding {
            panic!("panics-if-run's run() was invoked -- gating failed to prevent it");
        }

        fn tier(&self) -> &'static str {
            "Tier-1"
        }

        fn hypothesis(&self) -> &'static str {
            "stub -- must never actually run"
        }

        fn depends_on(&self) -> &'static [&'static str] {
            self.deps
        }
    }

    #[tokio::test]
    async fn run_gated_never_invokes_run_on_a_gated_out_probe() {
        let probes: Vec<Box<dyn Probe>> = vec![
            Box::new(GatekeeperProbe),
            Box::new(PanicIfRunProbe {
                deps: &["gatekeeper"],
            }),
        ];
        let graph = validate_dependency_graph(&probes).expect("no cycle, no unregistered dep");
        let db: Surreal<Any> = Surreal::init();
        let mut st = super::super::Strategy::default();

        // Must not panic -- if it does, run_gated called panics-if-run's real run().
        let findings = run_gated(&probes, &graph, &db, &mut st)
            .await
            .expect("the graph is built from this exact probes slice");

        assert_eq!(findings.len(), 2);
        let panics_if_run_finding = findings
            .iter()
            .find(|f| f.probe == "panics-if-run")
            .expect("panics-if-run must still produce a Finding, just a synthesized one");
        assert!(
            matches!(panics_if_run_finding.verdict, Verdict::Skipped { .. }),
            "a gated-out probe's Finding must carry Verdict::Skipped, got {:?}",
            panics_if_run_finding.verdict
        );
    }

    #[tokio::test]
    async fn run_gated_output_order_matches_input_slice_order_not_internal_dispatch_order() {
        // Downstream (index 0) depends_on ["upstream"] (index 1) -- forces Upstream to dispatch
        // first internally per Plan 41-02's toposort, the opposite of registration order.
        let probes: Vec<Box<dyn Probe>> =
            vec![stub("downstream", &["upstream"]), stub("upstream", &[])];
        let graph = validate_dependency_graph(&probes).expect("no cycle, no unregistered dep");
        assert_eq!(
            graph.dispatch_order(),
            &[1, 0],
            "sanity check: upstream must dispatch before downstream internally"
        );

        let db: Surreal<Any> = Surreal::init();
        let mut st = super::super::Strategy::default();
        let findings = run_gated(&probes, &graph, &db, &mut st)
            .await
            .expect("the graph is built from this exact probes slice");

        assert_eq!(findings.len(), 2);
        assert_eq!(
            findings[0].probe, "downstream",
            "output order must match the ORIGINAL probes slice order, not internal dispatch order"
        );
        assert_eq!(findings[1].probe, "upstream");
    }
}
