//! The coverage figure the entity profile states about itself, reconciled against the probes that
//! actually exist (PIV-02, plan 45-05).
//!
//! # Why this file exists
//!
//! `crates/core`'s findings-by-entity section prints a sentence of the form *"Searched 6 of 22
//! probes. 14 report only counts…"*. That sentence is the only thing standing between an analyst
//! and the wrong conclusion when the section comes back empty, so it has to be true — and a figure
//! typed into a document stops being true silently, the moment a probe is added or gains a
//! `RecordRef`.
//!
//! Two independent mechanisms keep it honest, because each catches a case the other cannot:
//!
//! 1. **`Probe::evidence_scope` has no default.** A probe added to the battery without answering
//!    it does not compile. That covers "a probe was added".
//! 2. **This file re-derives the answer from each probe's own source** and compares. That covers
//!    "a probe gained a `RecordRef` and its declaration was not updated" — which compiles fine,
//!    and would otherwise under-report forever.
//!
//! # The figure as it stands, re-verified 2026-09-17 (phase 49 plan 9, re-confirmed plan 11)
//!
//! | bucket | count | probes |
//! |---|---|---|
//! | names graph nodes | 12 | `address-cluster-exact`, `temporal-burst`, `ward-concentration`, `gazette-insolvency-signals`, `osint-disqualified-directors`, `osint-risk-edges`, `psc-beneficial-ownership`, `identity-link-registry`, `precomputed-alerts`, `property-nodes`, `phase20-ec-donations`, `osint-new-node-types` |
//! | names identity links only | 2 | `osint-confirmed-intel`, `osint-analyst-queue` |
//! | aggregate-only | 8 | the rest |
//!
//! Plan 49-11 (the phase's closing plan) re-ran `cargo test -p urban-integrity-probe --test
//! findings_coverage` before touching this file and confirmed all five tests already passed
//! against this exact 12/2/8 figure — the incremental reconciliation waves 49-03 through 49-09
//! performed had already kept the pinned bodies current. What 49-11 changed is the two pinned
//! tests' own function *names* (`the_stated_coverage_is_six_of_twenty_two_…` and
//! `forty_four_twenty_ones_twelve_of_thirteen_…`, both still describing the pre-phase-49 figure
//! long after their bodies stopped checking it) — see each test's own doc comment for the rename
//! record. `identity-integrity`'s classification remains open per the plan 49-10 summary: it is
//! CONTESTED, not re-confirmed as genuinely aggregate-only, but its `evidence_scope()` was left
//! unchanged by that plan, so it still counts in the 8 above.
//!
//! **This is not plan 44-21's "twelve of thirteen".** That measurement was correct and still is,
//! about the thirteen probes 44-21 had in front of it — twelve of those carried no id, and
//! `ward-concentration` was given refs first, then `psc-beneficial-ownership` in phase 49 plan 3,
//! then `identity-link-registry` in phase 49 plan 5, then `precomputed-alerts` in phase 49 plan 6,
//! then `property-nodes` in phase 49 plan 7, then `phase20-ec-donations` in phase 49 plan 8, then
//! `osint-new-node-types` in phase 49 plan 9. But the battery has 22 probes, and probes outside
//! 44-21's candidate list already emitted refs from earlier phases. Read as a battery-wide
//! coverage figure, "twelve of thirteen" understates this section's reach and overstates the gap.
//! The two probes outside 44-21's list that are genuinely aggregate-only are `identity-integrity`
//! and `detector-readiness`.
//!
//! # Reading the source is a deliberate choice, and its own trap
//!
//! `land_flip_candidates.rs` contains the text `RecordRef` — in a **comment** saying it never
//! constructs one. A scan that matched that substring would classify a correctly-declared probe as
//! ref-emitting and fail for no reason, which is exactly the "plan grep criteria self-trip" this
//! repository has recorded four times.
//!
//! `probe_body` strips `//` comments, `use` lines and the `#[cfg(test)]` module before matching,
//! and that stripping — **not** the shape of the pattern — is what defuses the trap. Measured, not
//! assumed: widening the pattern from `RecordRef {` to a bare `RecordRef` changes nothing while
//! the stripping is in place, and removing the stripping breaks the scan whichever pattern is
//! used. `the_source_scan_reads_code_and_not_prose` pins that, so the stripping cannot be deleted
//! on the belief that the `{` was carrying it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use surrealdb::Surreal;
use urban_integrity_probe::run::{build_battery, OcProbeGates};
use urban_integrity_probe::{battery_evidence_coverage, EvidenceScope, Probe};

fn battery() -> Vec<Box<dyn Probe>> {
    build_battery(
        Surreal::init(),
        String::from("findings_coverage_test"),
        OcProbeGates::default(),
    )
}

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// A probe module's source with `//`-comments and its `#[cfg(test)]` module removed.
///
/// Comments go because a doc comment or an inline note mentioning `RecordRef` is a statement
/// *about* the code, not the code. The test module goes because a stub `RecordRef` in a unit test
/// says nothing about what the probe emits at runtime.
fn probe_body(raw: &str) -> String {
    let code = match raw.find("\n#[cfg(test)]") {
        Some(i) => &raw[..i],
        None => raw,
    };
    code.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .filter(|l| !l.trim_start().starts_with("use "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a probe's source says it emits, derived rather than declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Observed {
    constructs_refs: bool,
    names_a_node_kind: bool,
}

impl Observed {
    fn scope(self) -> EvidenceScope {
        match (self.constructs_refs, self.names_a_node_kind) {
            (false, _) => EvidenceScope::AggregateOnly,
            (true, true) => EvidenceScope::NamesNodes,
            (true, false) => EvidenceScope::NamesLinksOnly,
        }
    }
}

/// Every probe module in `crates/probe/src`, keyed by the probe's own `name()`.
///
/// A module counts as a probe module when it contains an `impl Probe for`; that excludes
/// `lib.rs`, `graph.rs` and `run.rs`, whose `Probe` impls are all inside `#[cfg(test)]` and are
/// therefore removed by [`probe_body`] before the check runs.
fn observed_by_probe_name() -> BTreeMap<String, Observed> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(src_dir()).expect("read crates/probe/src") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).expect("read probe source");
        let body = probe_body(&raw);
        if !body.contains("impl Probe for") {
            continue;
        }
        let name = probe_name(&body)
            .unwrap_or_else(|| panic!("{path:?} impls Probe but declares no name() literal"));
        let constructs_refs = body.contains("RecordRef {");
        let names_a_node_kind =
            body.contains("RecordKind::Entity") || body.contains("RecordKind::CompanyNumber");
        out.insert(
            name,
            Observed {
                constructs_refs,
                names_a_node_kind,
            },
        );
    }
    out
}

/// The string literal returned by `fn name(&self) -> &'static str`.
fn probe_name(body: &str) -> Option<String> {
    let i = body.find("fn name(&self) -> &'static str")?;
    let rest = &body[i..];
    let q1 = rest.find('"')?;
    let q2 = rest[q1 + 1..].find('"')?;
    Some(rest[q1 + 1..q1 + 1 + q2].to_owned())
}

/// **The reconciliation.** Every probe's declared `evidence_scope` matches what its own source
/// actually constructs.
///
/// PROVE THIS CAN FAIL: change `ward_concentration.rs`'s declaration to
/// `EvidenceScope::AggregateOnly` and this fails naming `ward-concentration`, declared
/// `AggregateOnly`, observed `NamesNodes`. That mutation was performed; see the plan summary.
#[test]
fn every_declared_scope_matches_what_the_probe_actually_emits() {
    let battery = battery();
    let observed = observed_by_probe_name();

    // Non-vacuity, before any comparison: the scan must have found a probe module per battery
    // entry. A scan that found nothing would agree with every declaration vacuously.
    let declared_names: BTreeSet<String> = battery.iter().map(|p| p.name().to_owned()).collect();
    let observed_names: BTreeSet<String> = observed.keys().cloned().collect();
    assert_eq!(
        observed_names, declared_names,
        "the source scan and the battery must see the same probes; a mismatch means either a \
         registered probe has no module here or a module exists that is never registered"
    );
    assert!(
        !observed.is_empty(),
        "the source scan found no probe modules at all, so every assertion below is vacuous"
    );

    let mut disagreements: Vec<String> = Vec::new();
    for p in &battery {
        let obs = observed[p.name()];
        if p.evidence_scope() != obs.scope() {
            disagreements.push(format!(
                "{}: declared {:?}, source says {:?} (constructs_refs={}, node_kind={})",
                p.name(),
                p.evidence_scope(),
                obs.scope(),
                obs.constructs_refs,
                obs.names_a_node_kind,
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "a probe's declared evidence scope disagrees with its own source. The declaration is what \
         the profile's coverage figure is built from, so this is an under- or over-report reaching \
         an analyst:\n  {}",
        disagreements.join("\n  ")
    );
}

/// The coverage is built from the battery, accounts for every probe exactly once, and puts a
/// non-zero population in all three buckets.
///
/// The third clause is the non-vacuity guard that matters: with 14 of 22 probes emitting nothing,
/// a coverage that had silently collapsed to "everything is aggregate-only" would still satisfy
/// the totals.
///
/// PROVE THIS CAN FAIL: drop the `NamesLinksOnly` arm in `battery_evidence_coverage` into
/// `aggregate_only` and the `names_links_only` non-empty assertion fails.
#[test]
fn the_coverage_accounts_for_every_probe_exactly_once_and_all_three_buckets_are_populated() {
    let battery = battery();
    let coverage = battery_evidence_coverage(&battery);

    assert_eq!(
        coverage.probes_total(),
        battery.len(),
        "every probe must land in exactly one bucket"
    );

    let mut all: Vec<&String> = coverage
        .names_nodes
        .iter()
        .chain(coverage.names_links_only.iter())
        .chain(coverage.aggregate_only.iter())
        .collect();
    let before = all.len();
    all.sort();
    all.dedup();
    assert_eq!(before, all.len(), "no probe may appear in two buckets");

    assert!(
        !coverage.names_nodes.is_empty(),
        "no probe can name a node — the findings-by-entity section would be permanently empty and \
         every test of it vacuous"
    );
    assert!(
        !coverage.names_links_only.is_empty(),
        "the link-only bucket is empty; if that is genuinely true the bucket should be retired, \
         and if it is not, the coverage has collapsed two different problems into one"
    );
    assert!(
        !coverage.aggregate_only.is_empty(),
        "the aggregate-only bucket is empty, which would contradict every measurement since 44-21"
    );
}

/// The figure, pinned. A probe added, removed, or changing bucket fails here with the numbers, so
/// the summary and the UI copy go stale loudly rather than quietly.
///
/// The named lists are the point: a bare count would let two probes swap buckets unnoticed.
///
/// **Renamed in plan 49-11** (was `the_stated_coverage_is_six_of_twenty_two_…`): the body's
/// pinned lists and counts were already reconciled to 12/2/8 incrementally across plans 49-03
/// through 49-09 (each wave updated this test the moment its probe flipped bucket — see those
/// plans' SUMMARYs), but the function's own name kept saying "six" long after the body no longer
/// measured six of anything. A stale name asserting a number the body does not check is worse
/// than no test (plan 49-11's own words) — this rename is 49-11's part of closing that gap,
/// re-run against the real `cargo test` output rather than hand-predicted.
///
/// PROVE THIS CAN FAIL: it fails by construction on any battery change. Verified by moving
/// `temporal-burst` to `AggregateOnly`, which fails naming it in the expected-but-missing set.
#[test]
fn the_stated_coverage_is_twelve_of_twenty_two_and_this_is_the_battery_it_was_measured_against() {
    let battery = battery();
    let coverage = battery_evidence_coverage(&battery);

    let sorted = |v: &Vec<String>| {
        let mut v = v.clone();
        v.sort();
        v
    };

    assert_eq!(
        sorted(&coverage.names_nodes),
        vec![
            "address-cluster-exact",
            "gazette-insolvency-signals",
            "identity-link-registry",
            "osint-disqualified-directors",
            "osint-new-node-types",
            "osint-risk-edges",
            "phase20-ec-donations",
            "precomputed-alerts",
            "property-nodes",
            "psc-beneficial-ownership",
            "temporal-burst",
            "ward-concentration",
        ],
        "the searchable population changed; re-measure and update the plan 45-05 summary and the \
         profile copy rather than editing this list to match"
    );
    assert_eq!(
        sorted(&coverage.names_links_only),
        vec!["osint-analyst-queue", "osint-confirmed-intel"],
    );
    assert_eq!(coverage.aggregate_only.len(), 8);
    assert_eq!(coverage.probes_total(), 22);

    // And the sentence an analyst reads, built from those counts.
    let statement = coverage.statement();
    assert!(
        statement.contains("Searched 12 of 22 probes"),
        "got: {statement}"
    );
    assert!(
        statement.contains("8 report only counts"),
        "got: {statement}"
    );
}

/// 44-21's own thirteen candidates, checked as a set rather than as a remembered sentence.
///
/// Twelve of them were aggregate-only when 44-21 measured, and `ward-concentration` was not — so
/// 44-21 was right about its own scope at the time. This test existed so that when a later phase
/// gave one of those twelve a `RecordRef`, the claim "twelve of thirteen" would fail here instead
/// of being repeated from a summary forever. **That moment happened**: phase 49's evidence model
/// gave six of the twelve a `RecordRef` — `psc-beneficial-ownership` (plan 3),
/// `identity-link-registry` (plan 5), `precomputed-alerts` (plan 6), `property-nodes` (plan 7),
/// `phase20-ec-donations` (plan 8), `osint-new-node-types` (plan 9) — and this test's own doc
/// comment predicted exactly this at the time it was written.
///
/// **Renamed in plan 49-11** (was `forty_four_twenty_ones_twelve_of_thirteen_still_holds_for_
/// its_own_thirteen`): "twelve of thirteen" no longer holds, so a name still claiming it would be
/// a wrong headline attached to a correct body. This is the recorded correction, in the file's
/// own established pattern of recording corrections rather than silently editing history (see
/// `land_flip_candidates.rs`'s mutation-14 precedent this file's own top-of-file doc comment
/// cites). The remaining assertions (`aggregate == 6`, `battery_aggregate == 8`) were already
/// re-measured and updated incrementally across plans 49-03 through 49-09, one probe at a time,
/// as each probe's `evidence_scope()` flipped — this rename does not change what the body checks.
///
/// PROVE THIS CAN FAIL: declaring `osint-new-node-types` `NamesNodes` (plan 49-09) dropped the
/// count further, from seven to six — this test caught it, per its own stated design.
#[test]
fn forty_four_twenty_ones_six_of_thirteen_after_phase_49_evidence_model() {
    const CANDIDATES_44_21: &[&str] = &[
        "land-flip-candidates",
        "donation-contract-correlation",
        "osint-evasion-alerts",
        "precomputed-alerts",
        "psc-beneficial-ownership",
        "property-nodes",
        "geographic-concentration",
        "phase20-council-registers",
        "phase20-ec-donations",
        "opencorporates-cross-jurisdiction",
        "identity-link-registry",
        "osint-new-node-types",
        "ward-concentration",
    ];

    let battery = battery();
    let by_name: BTreeMap<&str, EvidenceScope> = battery
        .iter()
        .map(|p| (p.name(), p.evidence_scope()))
        .collect();

    // Non-vacuity: every remembered name must still be a probe, or the count below is measuring a
    // shorter list than it claims.
    for c in CANDIDATES_44_21 {
        assert!(
            by_name.contains_key(c),
            "44-21 named `{c}` as a ref candidate and no probe answers to that name today; the \
             comparison is not measuring what it says it is"
        );
    }
    assert_eq!(CANDIDATES_44_21.len(), 13);

    let aggregate = CANDIDATES_44_21
        .iter()
        .filter(|c| by_name[*c] == EvidenceScope::AggregateOnly)
        .count();
    assert_eq!(
        aggregate, 6,
        "44-21 measured twelve of its thirteen candidates as aggregate-only; phase 49 gave \
         psc-beneficial-ownership (plan 3), identity-link-registry (plan 5), \
         precomputed-alerts (plan 6), property-nodes (plan 7), phase20-ec-donations (plan 8) and \
         osint-new-node-types (plan 9) a RecordRef each, dropping this to six -- a finding to \
         record rather than a number to edit"
    );
    assert_eq!(by_name["ward-concentration"], EvidenceScope::NamesNodes);
    assert_eq!(
        by_name["psc-beneficial-ownership"],
        EvidenceScope::NamesNodes
    );
    assert_eq!(by_name["identity-link-registry"], EvidenceScope::NamesNodes);
    assert_eq!(by_name["precomputed-alerts"], EvidenceScope::NamesNodes);
    assert_eq!(by_name["property-nodes"], EvidenceScope::NamesNodes);
    assert_eq!(by_name["phase20-ec-donations"], EvidenceScope::NamesNodes);
    assert_eq!(by_name["osint-new-node-types"], EvidenceScope::NamesNodes);

    // And the figure that is NOT 44-21's: the battery is bigger than its candidate list, so the
    // battery-wide gap is smaller than "twelve of thirteen" reads.
    let battery_aggregate = battery
        .iter()
        .filter(|p| p.evidence_scope() == EvidenceScope::AggregateOnly)
        .count();
    assert_eq!(
        battery_aggregate, 8,
        "battery-wide aggregate-only count; 44-21's six remaining plus identity-integrity and \
         detector-readiness, which were not among its candidates"
    );
}

/// The source scan reads code, not prose.
///
/// `land_flip_candidates.rs` says "Never a `RecordRef`" in a comment. It is the one probe in the
/// tree that names the type it does not use, so it is the only available witness that this scan is
/// matching code — and without it every other probe would agree with its declaration for the
/// uninteresting reason that it never mentions the type at all.
///
/// PROVE THIS CAN FAIL: make `probe_body` return the source unchanged and this fails on the
/// stripping assertion. That mutation was performed; see the plan summary. The pattern's `{` was
/// mutated away too and changed nothing, which is why the doc above credits the stripping rather
/// than the pattern.
#[test]
fn the_source_scan_reads_code_and_not_prose() {
    let raw = std::fs::read_to_string(src_dir().join("land_flip_candidates.rs"))
        .expect("read land_flip_candidates.rs");
    assert!(
        raw.contains("RecordRef"),
        "this test is only meaningful while land_flip_candidates.rs still mentions RecordRef in a \
         comment; if that comment is gone, pick another probe with the same shape or delete this \
         test — do not weaken the stripping"
    );

    let body = probe_body(&raw);
    assert!(
        !body.contains("RecordRef"),
        "comment stripping failed: the only RecordRef in this file is a comment saying it never \
         constructs one, and a scan that counted it would fail a correctly-declared probe"
    );

    // And the classification that depends on it, end to end.
    assert_eq!(
        observed_by_probe_name()["land-flip-candidates"].scope(),
        EvidenceScope::AggregateOnly
    );
}
