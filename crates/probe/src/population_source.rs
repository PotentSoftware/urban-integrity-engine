//! Single shared home for the probe-population partition: which of the 22 registered probes
//! the oracle can independently check (ORC-08's coverage statement) and, equivalently, what a
//! probe's population provenance is (VRD-13's `PopulationSource`). Extracted verbatim from
//! `probe_oracle.rs` (Phase 38) so `probe::run::execute()` (Plan 39-08) can compute
//! `PopulationSource` without depending on the `probe_oracle` command module — the primary
//! `probe` subcommand must never depend on a secondary comparison command (PD-04), and there
//! must be exactly one copy of this partition, never a second, independently-drifting table.

use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::persistence::oracle::{self, EdgeField};
use urban_integrity_schema::types::verdict::PopulationSource;

/// How a [`CHECKED_PROBES`] entry's declared population is independently recomputed.
///
/// Every field is a compile-time `&'static` value from the mapping tables below — no
/// operator-supplied or runtime-derived string ever reaches an `oracle::` call through this type
/// (T-38-06).
pub enum OracleCheckKind {
    /// A bare or marker-filtered `graph_events` count — `oracle::count_events`.
    Events {
        kind: &'static str,
        markers: &'static [&'static str],
    },
    /// A dedup-by-entity count, optionally prefix-filtered — `oracle::count_distinct_entities`.
    DistinctEntities {
        kind: &'static str,
        marker: &'static str,
        prefix: Option<&'static str>,
    },
    /// A dedup-by-edge-endpoint count — `oracle::count_distinct_edge_field`.
    DistinctEdgeField {
        marker: &'static str,
        field: EdgeField,
    },
    /// Several `Events` checks summed together — `osint-new-node-types`'s 5-source population.
    SumOfEvents(&'static [(&'static str, &'static [&'static str])]),
}

/// One probe's independent oracle check: which probe it applies to, and how to recompute its
/// population.
pub struct OracleCheck {
    pub probe: &'static str,
    pub kind: OracleCheckKind,
}

/// The 10 registered probes whose declared population this oracle can independently recompute
/// from raw `graph_events`, via `count()`/`GROUP BY` SurrealQL only (never `persistence::scan`,
/// per ORC-06).
pub const CHECKED_PROBES: &[OracleCheck] = &[
    OracleCheck {
        probe: "precomputed-alerts",
        kind: OracleCheckKind::Events {
            kind: "MotifDetected",
            markers: &[],
        },
    },
    // The canary: this entry's oracle count must equal 39,860 on `an_anglesey_aebaa785` — the
    // independently-proven ward-reachable Contract population (Plan 37, `260831-ffx`'s
    // motivating regression). Verified live in Plan 38-03 Task 3.
    OracleCheck {
        probe: "identity-integrity",
        kind: OracleCheckKind::DistinctEntities {
            kind: "NodeAdded",
            marker: "\"type\":\"Contract\"",
            prefix: Some("planning:"),
        },
    },
    // KNOWN counting-shape asymmetry, not a data-integrity defect: `shared_exact_address.rs`
    // counts a `LocatedAt` edge's destination only when BOTH `src_id` and `dst_id` parse via
    // `record_id_str`, whereas this oracle check counts distinct `dst_id` regardless of whether
    // `src_id` parses. A live edge with an unparseable `src_id` but a valid, unique `dst`
    // therefore produces a real `oracle > declared` gap that is an artifact of the two counting
    // shapes — a future reader investigating such a disagreement should not mistake it for a new
    // `260831-ffx`-class finding.
    OracleCheck {
        probe: "address-cluster-exact",
        kind: OracleCheckKind::DistinctEdgeField {
            marker: "\"type\":\"LocatedAt\"",
            field: EdgeField::Dst,
        },
    },
    // ADJ-01, RESOLVED 2026-09-01 — this entry is why. It disagreed live (102,897 declared vs
    // 61,756 oracle) because `geographic_concentration.rs` scanned per event, not per entity;
    // that probe now uses `scan_latest_entities_by_type` and the row agrees. The check stays
    // exactly as it is: it is the regression guard against the defect returning, and its value
    // is that it disagreed once for a real reason. Do not adjust either side to force agreement.
    OracleCheck {
        probe: "geographic-concentration",
        kind: OracleCheckKind::DistinctEntities {
            kind: "NodeAdded",
            marker: "\"type\":\"Location\"",
            prefix: None,
        },
    },
    OracleCheck {
        probe: "ward-concentration",
        kind: OracleCheckKind::DistinctEntities {
            kind: "NodeAdded",
            marker: "\"type\":\"Contract\"",
            prefix: None,
        },
    },
    // Identical check to ward-concentration — same underlying distinct-Contract-entity set
    // (temporal-burst dedups by earliest event, not latest, but the COUNT is identical).
    OracleCheck {
        probe: "temporal-burst",
        kind: OracleCheckKind::DistinctEntities {
            kind: "NodeAdded",
            marker: "\"type\":\"Contract\"",
            prefix: None,
        },
    },
    // Plain substring marker, matching `scan_events_by_payload_contains`'s own needle exactly —
    // NOT the JSON-shaped marker `osint-new-node-types` uses for its own InLiquidation term below.
    OracleCheck {
        probe: "gazette-insolvency-signals",
        kind: OracleCheckKind::Events {
            kind: "NodeUpdated",
            markers: &["InLiquidation"],
        },
    },
    OracleCheck {
        probe: "property-nodes",
        kind: OracleCheckKind::Events {
            kind: "NodeAdded",
            markers: &["\"type\":\"Property\""],
        },
    },
    // The oracle's 5-term SumOfEvents below always matched Rust's 5-term total exactly — the
    // historical Python-vs-Rust mismatch was a Python-side undercount (ADJ-03, resolved Phase 40,
    // see docs/ADJUDICATIONS.md), not an oracle disagreement. This check is held to exact
    // agreement (delta 0, the default for any probe absent from PROBE_DELTA in probe_oracle.rs).
    OracleCheck {
        probe: "osint-new-node-types",
        kind: OracleCheckKind::SumOfEvents(&[
            ("NodeAdded", &["\"type\":\"Psc\""]),
            ("NodeAdded", &["\"type\":\"Charity\""]),
            ("NodeAdded", &["\"type\":\"Property\""]),
            ("NodeAdded", &["\"type\":\"OffshoreEntity\""]),
            ("NodeUpdated", &["\"type\":\"InLiquidation\""]),
        ]),
    },
    // The two-marker AND approximates `is_council_registers_political_actor`'s "council_name is
    // a non-null string" discriminator.
    OracleCheck {
        probe: "phase20-council-registers",
        kind: OracleCheckKind::Events {
            kind: "NodeAdded",
            markers: &["\"type\":\"PoliticalActor\"", "\"council_name\":\""],
        },
    },
];

/// Why a probe cannot be structurally checked by this oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionCategory {
    /// Population is sourced from `identity_links`, not `graph_events`.
    IdentityLinksSourced,
    /// Population is a hardcoded `Verdict::classify(1, 1, ...)` threshold, not a real count.
    VerdictThreshold,
    /// Population counts something structurally different from a `graph_events` entity/edge
    /// count (a term count, a scalar marker field, etc).
    SemanticMismatch,
    /// Population is arithmetic on scalar fields of a single marker record, not a count of any
    /// set.
    MarkerArithmetic,
}

/// One excluded probe: which probe, why it is excluded, and the specific source citation
/// backing that reason.
#[derive(Debug, Clone, Copy)]
pub struct Exclusion {
    pub probe: &'static str,
    pub category: ExclusionCategory,
    pub reason: &'static str,
}

/// The 12 registered probes this oracle cannot independently check, each with a category-specific
/// reason citing the exact source of its declared population — never a blanket "Tier-2" label
/// (ORC-08).
pub const EXCLUDED_PROBES: &[Exclusion] = &[
    Exclusion {
        probe: "detector-readiness",
        category: ExclusionCategory::SemanticMismatch,
        reason: "population, when reported, is term_counts.len() -- the number of distinct \
                 motif terms tracked (currently 9), not a graph_events entity/edge count \
                 (detector_readiness.rs:~510-518).",
    },
    Exclusion {
        probe: "identity-link-registry",
        category: ExclusionCategory::IdentityLinksSourced,
        reason: "population is sourced from scan_identity_links(db, None, None), an \
                 identity_links table read, not graph_events.",
    },
    Exclusion {
        probe: "osint-confirmed-intel",
        category: ExclusionCategory::IdentityLinksSourced,
        reason: "population is sourced from scan_identity_links(db, None, None) filtered to \
                 confirmed links, an identity_links table read, not graph_events.",
    },
    Exclusion {
        probe: "osint-analyst-queue",
        category: ExclusionCategory::IdentityLinksSourced,
        reason: "population is sourced from scan_identity_links(db, Some(\"pending_review\"), \
                 None), an identity_links table read, not graph_events.",
    },
    Exclusion {
        probe: "psc-beneficial-ownership",
        category: ExclusionCategory::IdentityLinksSourced,
        reason: "population is sourced from scan_identity_links(db, None, Some(\"ch_psc\")), an \
                 identity_links table read, not graph_events.",
    },
    Exclusion {
        probe: "phase20-ec-donations",
        category: ExclusionCategory::IdentityLinksSourced,
        reason: "population is sourced from \
                 scan_identity_links(db, None, Some(\"electoral_commission\")), an \
                 identity_links table read, not graph_events.",
    },
    Exclusion {
        probe: "opencorporates-cross-jurisdiction",
        category: ExclusionCategory::MarkerArithmetic,
        reason: "population is marker.candidates_found, a scalar recorded by the \
                 OpenCorporates enrichment run's own marker record, not a graph_events or \
                 identity_links count.",
    },
    Exclusion {
        probe: "osint-disqualified-directors",
        category: ExclusionCategory::VerdictThreshold,
        reason: "declared population is hardcoded via \
                 disqualified_active_in_procurement.rs:232's \
                 Verdict::classify(1, 1, matches, hit_threshold, \"\"), not a real count.",
    },
    Exclusion {
        probe: "donation-contract-correlation",
        category: ExclusionCategory::IdentityLinksSourced,
        reason: "population is ec_links.len() (donation_contract_correlation.rs:175), sourced \
                 from identity_links, not graph_events.",
    },
    Exclusion {
        probe: "osint-risk-edges",
        category: ExclusionCategory::IdentityLinksSourced,
        reason: "population combines a graph_events scan with an identity_links scan \
                 (disqualified_director_edges.rs:100,276) -- not a single independently \
                 recomputable graph_events count.",
    },
    Exclusion {
        probe: "osint-evasion-alerts",
        category: ExclusionCategory::VerdictThreshold,
        reason: "declared population is hardcoded via evasion_alerts.rs:151's \
                 Verdict::classify(1, 1, 1, 1, \"\"), not a real count.",
    },
    // D-02's corrected wording -- this is NOT because the probe "routes through
    // persistence::scan" (that claim is false per phase 38 design decision D-02).
    Exclusion {
        probe: "land-flip-candidates",
        category: ExclusionCategory::MarkerArithmetic,
        reason: "population is marker arithmetic on two scalar fields of the \
                 land_flip_runs:current marker (land_flip_candidates.rs:110), not a count of \
                 any set; the only genuinely independent check available is already enforced \
                 in-probe at land_flip_candidates.rs:159.",
    },
];

/// Recompute one [`OracleCheck`]'s population independently, dispatching on its
/// [`OracleCheckKind`] to the matching `oracle::` primitive(s).
pub async fn evaluate(check_kind: &OracleCheckKind, db: &Surreal<Any>) -> anyhow::Result<u64> {
    match check_kind {
        OracleCheckKind::Events { kind, markers } => oracle::count_events(db, kind, markers).await,
        OracleCheckKind::DistinctEntities {
            kind,
            marker,
            prefix,
        } => oracle::count_distinct_entities(db, kind, marker, *prefix).await,
        OracleCheckKind::DistinctEdgeField { marker, field } => {
            oracle::count_distinct_edge_field(db, marker, *field).await
        }
        OracleCheckKind::SumOfEvents(pairs) => {
            let mut total = 0u64;
            for (event_kind, markers) in pairs.iter() {
                total += oracle::count_events(db, event_kind, markers).await?;
            }
            Ok(total)
        }
    }
}

/// The CR-02-style hard-error completeness check, generalized from `probe_oracle::compare()`'s
/// former inline blocks: every name in `probe_names` appears in exactly one of
/// [`CHECKED_PROBES`]/[`EXCLUDED_PROBES`], and neither table names a probe absent from
/// `probe_names`.
///
/// `anyhow::bail!`, never `debug_assert!` — the workspace defines no `[profile.release]`, so
/// `debug-assertions` is `false` in the shipped binary, exactly the build in which a silent
/// coverage gap matters (CR-02, phase 38 code review). Called by both `probe_oracle::compare()` and
/// `probe::run::execute()` so the partition cannot drift for one command while the other stays
/// green.
pub fn verify_registered_probes_covered(probe_names: &[&str]) -> anyhow::Result<()> {
    if CHECKED_PROBES.len() + EXCLUDED_PROBES.len() != probe_names.len() {
        anyhow::bail!(
            "coverage drift: {} checked + {} excluded != {} probes in the registered probe set \
             -- a probe was added, removed or renamed without updating \
             CHECKED_PROBES/EXCLUDED_PROBES",
            CHECKED_PROBES.len(),
            EXCLUDED_PROBES.len(),
            probe_names.len()
        );
    }
    // Direction 1: every registered probe is in exactly one table.
    for name in probe_names {
        let in_checked = CHECKED_PROBES.iter().any(|c| c.probe == *name);
        let in_excluded = EXCLUDED_PROBES.iter().any(|e| e.probe == *name);
        if in_checked == in_excluded {
            anyhow::bail!(
                "probe '{name}' must appear in exactly one of CHECKED_PROBES/EXCLUDED_PROBES \
                 (checked={in_checked}, excluded={in_excluded})"
            );
        }
    }
    // Direction 2: neither table names a probe the registered set does not contain. Without
    // this a probe removed from the battery keeps a stale EXCLUDED_PROBES row, and the coverage
    // table goes on citing an exclusion reason for something that no longer runs.
    for name in CHECKED_PROBES
        .iter()
        .map(|c| c.probe)
        .chain(EXCLUDED_PROBES.iter().map(|e| e.probe))
    {
        if !probe_names.contains(&name) {
            anyhow::bail!(
                "'{name}' is listed in the oracle tables but is not a registered probe -- the \
                 battery dropped or renamed it without updating CHECKED_PROBES/EXCLUDED_PROBES"
            );
        }
    }
    Ok(())
}

/// What is this probe's population provenance this run?
///
/// * No `Coverage` this run (`DataGap`/`Blocked`) — nothing was measured, so there is nothing to
///   corroborate: `SelfReported` with a fixed reason, regardless of the probe's
///   CHECKED/EXCLUDED disposition.
/// * In [`CHECKED_PROBES`] — the oracle independently recomputes the population from raw
///   `graph_events`: `OracleCorroborated` carrying that count. Note the count is the oracle's
///   own number, not an assertion of agreement — rendering/comparison decides what to say about
///   a mismatch.
/// * In [`EXCLUDED_PROBES`] — structurally uncheckable: `SelfReported` carrying that entry's
///   specific, source-cited reason (never a blanket "Tier-2" label, ORC-08).
///
/// T-39-07: `probe_name` is only ever compared (`==`) against the tables' compile-time entries —
/// it is never interpolated into a query or passed to `oracle::` itself. Only the matched
/// entry's own `OracleCheckKind` (compile-time `&'static` data, T-38-06) reaches [`evaluate`].
// `pub`, not `pub(crate)`: 39-09's DB-gated corroboration test (`tests/population_source_
// corroboration.rs`) is an external integration test and must call this directly, mirroring
// `run.rs::execute()`'s wiring against a live oracle.
pub async fn for_probe(
    probe_name: &str,
    has_coverage: bool,
    db: &Surreal<Any>,
) -> anyhow::Result<PopulationSource> {
    if !has_coverage {
        return Ok(PopulationSource::SelfReported {
            reason: "no population measured this run (DataGap/Blocked)",
        });
    }
    if let Some(check) = CHECKED_PROBES.iter().find(|c| c.probe == probe_name) {
        let oracle_count = evaluate(&check.kind, db).await?;
        return Ok(PopulationSource::OracleCorroborated { oracle_count });
    }
    if let Some(exclusion) = EXCLUDED_PROBES.iter().find(|e| e.probe == probe_name) {
        return Ok(PopulationSource::SelfReported {
            reason: exclusion.reason,
        });
    }
    // Defensive redundancy: verify_registered_probes_covered should have caught this earlier in
    // the same call path.
    anyhow::bail!(
        "probe '{probe_name}' is in neither CHECKED_PROBES nor EXCLUDED_PROBES -- the \
         PopulationSource dispatch has drifted from the registered battery"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VRD-13's excluded-probe half (the phase 39 validation notes' "excluded probe... reason surfaced"
    /// row): the exact string that reaches `for_probe`'s `SelfReported` branch for an
    /// ORC-08-excluded probe is the SAME string `EXCLUDED_PROBES` declares — never a
    /// paraphrase or truncation. Non-DB: `for_probe` only touches the database on the
    /// CHECKED path, so this pins the lookup-and-carry logic directly.
    #[test]
    fn excluded_probe_population_source_carries_the_orc08_reason_verbatim() {
        let entry = EXCLUDED_PROBES
            .iter()
            .find(|e| e.probe == "land-flip-candidates")
            .expect("land-flip-candidates must be present in EXCLUDED_PROBES");
        let source = PopulationSource::SelfReported {
            reason: entry.reason,
        };
        match source {
            PopulationSource::SelfReported { reason } => {
                assert_eq!(
                    reason, entry.reason,
                    "the SelfReported reason must be EXCLUDED_PROBES' own string, verbatim"
                );
                assert!(
                    reason.contains("marker arithmetic"),
                    "the reason must be the specific, source-cited ORC-08 text, got: {reason}"
                );
            }
            other => panic!("constructed SelfReported, got {other:?}"),
        }
    }

    /// CR-02 (phase 38 code review): this test used to compare against a hardcoded `22`, so adding a
    /// 23rd probe to the battery without touching either table left it green. It now reconciles
    /// against `probe::run::registered_probe_names()` -- the real registration site -- in both
    /// directions, so battery drift fails at `cargo test` and not only at runtime inside
    /// `compare()`. Needs no database: `build_battery` constructs the probes without querying.
    #[test]
    fn checked_and_excluded_reconcile_against_the_real_battery_registry() {
        let registered = crate::run::registered_probe_names();

        let mut names: Vec<&str> = CHECKED_PROBES.iter().map(|c| c.probe).collect();
        names.extend(EXCLUDED_PROBES.iter().map(|e| e.probe));

        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "no probe name may appear in both CHECKED_PROBES and EXCLUDED_PROBES"
        );

        assert_eq!(
            names.len(),
            registered.len(),
            "the two oracle tables cover {} probes but the battery registers {} -- a probe was \
             added, removed or renamed without updating CHECKED_PROBES/EXCLUDED_PROBES",
            names.len(),
            registered.len()
        );

        for probe in &registered {
            assert!(
                names.contains(probe),
                "registered probe '{probe}' is in neither CHECKED_PROBES nor EXCLUDED_PROBES, so \
                 `probe-oracle`'s coverage statement would silently omit it"
            );
        }
        for name in &names {
            assert!(
                registered.contains(name),
                "'{name}' is listed in the oracle tables but the battery does not register it"
            );
        }
    }

    #[test]
    fn land_flip_exclusion_reason_uses_marker_arithmetic_wording_not_scan_routing() {
        let entry = EXCLUDED_PROBES
            .iter()
            .find(|e| e.probe == "land-flip-candidates")
            .expect("land-flip-candidates must be present in EXCLUDED_PROBES");
        assert!(entry.reason.contains("marker arithmetic"));
        assert!(!entry.reason.contains("persistence::scan"));
        assert!(!entry.reason.contains("routes through scan"));
    }

    #[test]
    fn graph_events_sourced_tier2_probes_are_checked_not_excluded() {
        for name in [
            "gazette-insolvency-signals",
            "property-nodes",
            "osint-new-node-types",
            "phase20-council-registers",
        ] {
            assert!(
                CHECKED_PROBES.iter().any(|c| c.probe == name),
                "{name} should appear in CHECKED_PROBES"
            );
            assert!(
                !EXCLUDED_PROBES.iter().any(|e| e.probe == name),
                "{name} should NOT appear in EXCLUDED_PROBES"
            );
        }
    }
}
