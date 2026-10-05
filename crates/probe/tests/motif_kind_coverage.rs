//! Reconciles every probe's `Probe::motif_kind()` answer against the pinned classification table
//! (D-06, phase 50 plan 1), mirroring `findings_coverage.rs`'s own reconciliation discipline for
//! `evidence_scope`.
//!
//! # Why this file exists
//!
//! `Probe::motif_kind()` has no default impl, so a probe that forgets to answer it fails to
//! compile — that covers "a probe was added and never classified". It does not cover a probe
//! that answers with the *wrong* variant: that compiles fine, and would silently mis-render the
//! finding board (`finding_card.rs`'s thumbnail dispatch, phase 50's later plans) for whichever
//! probe drifted. This file closes that gap by comparing the battery's real `motif_kind()`
//! answers against a table pinned here, so a mis-transcription or an un-reviewed reclassification
//! fails loudly in CI rather than silently reaching the board.
//!
//! # The table, as classified in this plan (phase 50 plan 1, 2026-09-17)
//!
//! | bucket | count | probes |
//! |---|---|---|
//! | `Star` | 1 | `address-cluster-exact` |
//! | `TypedEdge` | 1 | `osint-risk-edges` |
//! | `RankedBars` | 1 | `ward-concentration` |
//! | `Chips` | 11 | the 9 remaining `NamesNodes` probes plus `osint-analyst-queue` and `osint-confirmed-intel` (`NamesLinksOnly`) |
//! | `Absence` | 8 | every `AggregateOnly` probe |
//!
//! `osint-analyst-queue` and `osint-confirmed-intel` are classified `Chips` despite carrying only
//! `NamesLinksOnly` evidence (an `IdentityLink`-kind `RecordRef`, not a node id) -- see this
//! plan's Task 1 for the full reasoning: `named_entity_chips.rs` (plan 50-06) renders
//! whatever `EntityRefView` does with an `IdentityLink`-kind ref honestly, even without a working
//! pivot link, which is strictly more informative than `Absence`'s "no entities to show".

use std::collections::BTreeMap;

use surrealdb::Surreal;
use urban_integrity_probe::run::{build_battery, OcProbeGates};
use urban_integrity_probe::{MotifKind, Probe};

fn battery() -> Vec<Box<dyn Probe>> {
    build_battery(
        Surreal::init(),
        String::from("motif_kind_coverage_test"),
        OcProbeGates::default(),
    )
}

/// The pinned table: probe name -> expected `MotifKind::name()`.
fn pinned_table() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("address-cluster-exact", "Star"),
        ("osint-risk-edges", "TypedEdge"),
        ("ward-concentration", "RankedBars"),
        ("gazette-insolvency-signals", "Chips"),
        ("identity-link-registry", "Chips"),
        ("osint-disqualified-directors", "Chips"),
        ("osint-new-node-types", "Chips"),
        ("phase20-ec-donations", "Chips"),
        ("precomputed-alerts", "Chips"),
        ("property-nodes", "Chips"),
        ("psc-beneficial-ownership", "Chips"),
        ("temporal-burst", "Chips"),
        ("osint-analyst-queue", "Chips"),
        ("osint-confirmed-intel", "Chips"),
        ("donation-contract-correlation", "Absence"),
        ("osint-evasion-alerts", "Absence"),
        ("geographic-concentration", "Absence"),
        ("identity-integrity", "Absence"),
        ("land-flip-candidates", "Absence"),
        ("opencorporates-cross-jurisdiction", "Absence"),
        ("phase20-council-registers", "Absence"),
        ("detector-readiness", "Absence"),
    ])
}

/// **The reconciliation.** Every probe's `motif_kind()` matches the pinned table, and the pinned
/// table's key set matches the battery's probe-name set exactly (non-vacuity: a probe added
/// without a pinned entry fails loudly here, mirroring `findings_coverage.rs`'s own non-vacuity
/// discipline).
///
/// PROVE THIS CAN FAIL: change `ward_concentration.rs`'s `motif_kind()` to return
/// `MotifKind::Chips` and this fails naming `ward-concentration`, expected `RankedBars`, got
/// `Chips`. That mutation was performed; see the plan summary.
#[test]
fn every_probe_declares_a_motif_kind_and_it_matches_the_pinned_table() {
    let battery = battery();
    let pinned = pinned_table();

    let battery_names: std::collections::BTreeSet<&str> =
        battery.iter().map(|p| p.name()).collect();
    let pinned_names: std::collections::BTreeSet<&str> = pinned.keys().copied().collect();
    assert_eq!(
        battery_names, pinned_names,
        "the battery's probe-name set and the pinned table's key set must match exactly; a \
         probe added to the battery without a pinned entry here -- or a pinned entry naming a \
         probe that no longer exists -- must fail loudly rather than being silently skipped"
    );

    let mut disagreements: Vec<String> = Vec::new();
    for p in &battery {
        let expected = pinned[p.name()];
        let got = p.motif_kind().name();
        if got != expected {
            disagreements.push(format!("{}: expected {expected}, got {got}", p.name()));
        }
    }
    assert!(
        disagreements.is_empty(),
        "a probe's motif_kind() disagrees with the pinned classification table. This value \
         drives the finding board's per-card rendering, so a mismatch reaching the browser \
         mis-renders that probe's card:\n  {}",
        disagreements.join("\n  ")
    );
}

/// The pinned table's own shape: 22 probes total, with the exact per-variant counts D-06's
/// classification produced. A bare `.len() == 22` would let two variants swap counts unnoticed,
/// so each is asserted individually.
///
/// PROVE THIS CAN FAIL: change `identity_link_registry.rs`'s `motif_kind()` from `Chips` to
/// `Absence` and the `Chips` count drops to 10 while `Absence` rises to 9, both failing here.
#[test]
fn the_pinned_table_has_twenty_two_probes_with_the_exact_per_variant_split() {
    let battery = battery();
    assert_eq!(battery.len(), 22, "the battery itself must have 22 probes");

    let mut star = 0;
    let mut typed_edge = 0;
    let mut ranked_bars = 0;
    let mut chips = 0;
    let mut absence = 0;
    for p in &battery {
        match p.motif_kind() {
            MotifKind::Star => star += 1,
            MotifKind::TypedEdge => typed_edge += 1,
            MotifKind::RankedBars => ranked_bars += 1,
            MotifKind::Chips => chips += 1,
            MotifKind::Absence => absence += 1,
        }
    }

    assert_eq!(star, 1, "expected exactly 1 Star probe");
    assert_eq!(typed_edge, 1, "expected exactly 1 TypedEdge probe");
    assert_eq!(ranked_bars, 1, "expected exactly 1 RankedBars probe");
    assert_eq!(chips, 11, "expected exactly 11 Chips probes");
    assert_eq!(absence, 8, "expected exactly 8 Absence probes");
    assert_eq!(
        star + typed_edge + ranked_bars + chips + absence,
        22,
        "every probe must land in exactly one motif-kind bucket"
    );
}

/// `MotifKind::name()` round-trips through the exact strings this file's pinned table and
/// `ProbeView.motif_kind` (Task 2) both rely on -- asserted directly so a rename of any variant's
/// wire spelling fails here rather than being discovered downstream.
#[test]
fn motif_kind_name_is_exhaustive_and_stable() {
    assert_eq!(MotifKind::Star.name(), "Star");
    assert_eq!(MotifKind::TypedEdge.name(), "TypedEdge");
    assert_eq!(MotifKind::RankedBars.name(), "RankedBars");
    assert_eq!(MotifKind::Chips.name(), "Chips");
    assert_eq!(MotifKind::Absence.name(), "Absence");
}
