# Bucket-(c) Differential Adjudications

This document records the adjudication reasoning for the three "bucket-(c)" differential findings
raised by the v7.0 Python-vs-Rust probe differential — findings where the two implementations
disagreed and neither side's own tests caught it. The origin of all three is
the phase 36 differential-results notes (v7.0, differential verification / Python retirement
decision), §6. The purpose of this record is ADJ-04: so the next time a probe disagrees with the oracle, that
disagreement is checked against this record before re-investigating from zero.

Each entry below carries a fixed set of fields — finding name, date, sources compared,
authoritative source, cause, evidence refs, status — followed by a narrative note that handles
whatever the fields alone cannot capture.

## ADJ-01 — geographic-concentration

- **Date:** 2026-09-01
- **Sources compared:** Rust probe (`geographic_concentration.rs`), Python
  (`analyst_probe.py`), the population oracle (`persistence::oracle`, Phase 38)
- **Authoritative source:** the oracle — a third, independently derived count that settled which
  of the two disagreeing implementations was correct
- **Cause:** `geographic_concentration.rs` counted `NodeAdded` events one-per-event via
  `scan::scan_entities_by_type`, double-counting any re-ingested Location; fixed by switching to
  `scan_latest_entities_by_type` (per-entity dedup, matching Python's `fetch_nodes`)
- **Evidence refs:** phase 38 plan 03 summary (population-oracle comparison),
  commit `c8fb0c7`
- **Status:** Resolved 2026-09-01

**Narrative:** The 23,322-vs-1,697 pair originally recorded in the v7.0 differential are `matches`
(streets carrying ≥2 distinct entities), not populations — a future reader citing this pair must
read them as such. The live oracle run surfaced the real, larger disagreement (102,897 declared vs
61,756 oracle) that led to the correct diagnosis, which the plan's own expected figures had not
anticipated. After the fix, live population dropped 102,897 → 61,756 and the derived `matches`
figure dropped 23,322 → exactly 1,697, matching Python's own figure with no residual gap.

## ADJ-02 — precomputed-alerts

- **Date:** 2026-09-03
- **Sources compared:** the `detector_runs:current` marker (written only by
  `GraphCoordinator::bake_snapshot` when a Tier-B motif detector is configured and runs) vs the
  live `MotifDetected` graph_events population (1,947 rows at time of finding)
- **Authoritative source:** neither source alone is authoritative in the usual sense —
  `precomputed-alerts` counts alerts from BOTH producer families (Tier-B's own detector, and five
  enricher post-adapter detectors: evasion, donation_correlation, offshore_shadow,
  charity_crossover, property_accumulation); the marker only ever covered the first
- **Cause:** the probe's verdict logic treated marker-absence as proof no alerts could exist,
  collapsing two independent producers into one signal — fixed by removing the marker-absence
  early return in `precomputed_alerts.rs` (Phase 40, Plan 40-01) so `MotifDetected` rows are always
  counted regardless of Tier-B's own run state; the "did Tier-B run" signal relocated to
  `detector-readiness` as a non-gating evidence row
- **Evidence refs:** `crates/cli/src/commands/probe/precomputed_alerts.rs`,
  `crates/cli/src/commands/probe/detector_readiness.rs`,
  `crates/core/src/graph/coordinator.rs` (`bake_snapshot`, the marker's sole writer)
- **Status:** Resolved 2026-09-03 (Phase 40)

**Narrative:** "Marker absent" and "1,947 alerts present" were never a contradiction — they are two
facts about two different subsystems, and neither implementation was wrong about what it measured,
only about what the capability is (it has two halves, and each side modelled one).

## ADJ-03 — osint-new-node-types

- **Date:** 2026-09-03
- **Sources compared:** Rust (`osint_new_node_types.rs`, 5-term total), Python
  (`scripts/analyst_probe.py:1396-1469`, 4-term total), the population oracle's independent
  5-term `SumOfEvents` check (`population_source.rs`)
- **Authoritative source:** Rust, corroborated independently by the oracle
- **Cause:** `scripts/analyst_probe.py:1414-1421` computes `inliq_count`, displays it in its
  evidence block, then sums `total_osint_nodes` over the four `NodeAdded` types only —
  excluding InLiquidation from the headline with nothing marking the exclusion; Rust always
  summed all five terms
- **Evidence refs:** `crates/cli/src/commands/probe/osint_new_node_types.rs:121`,
  `crates/cli/src/commands/probe/population_source.rs:130-139`,
  phase 36 differential captures `rust-run-1.json` and `python-run-1.json`
- **Status:** Resolved 2026-09-03 (Phase 40) — display-only fix (D-05), the 5-term total was
  never wrong and is unchanged

**Narrative:** The arithmetic, side by side:

    Rust    17,867 + 70 + 105,809 + 29,927 + 1  = 153,674   (5 terms)
    Python  17,867 + 70 + 105,809 + 29,927      = 153,673   (4 terms)
                                              InLiquidation = 1

The generalisation (D-06) matters more than the single-record delta observed here: the delta
always equals the InLiquidation count and scales with it — the `1` observed on the Anglesey
corpus was incidental, not the finding itself. A future run with, say, 500 Gazette InLiquidation
updates would show a 500-record delta against Python — recording the generalisation here is what
stops that reopening this finding.
