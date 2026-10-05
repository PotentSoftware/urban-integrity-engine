//! `to_verdict_view` — the ONLY producer of `VerdictView` in this codebase (Pitfall 2's
//! prescribed fix). Every resolver that needs to send a verdict across the wire must call this
//! function rather than hand-rolling a `serde_json::json!` literal or a second, divergent
//! conversion site — a second producer anywhere else would silently reintroduce Pitfall 2
//! (a display bucket silently swallowing a verdict variant).
//!
//! **Not yet wired (Phase 42):** no non-test caller exists — none of this phase's routes
//! (`/api/workbench-status`, `/api/analyses`) carries a verdict, so nothing produces a
//! `VerdictView` on the wire yet. "ONLY producer" is the contract the first verdict-bearing
//! endpoint (the entity profile / triage screens, Phase 43+) must honour by calling this
//! function; it is not a statement that one already does. The function is `pub` from a library
//! crate, so it raises no dead-code warning; its own tests are what keep the exhaustive match
//! exercised until a route consumes it.
//!
//! `population_source` is a caller-supplied parameter, not read from `Verdict` itself, because
//! `Verdict::classify` is deliberately DB-free and synchronous (see `verdict.rs:409-422`'s
//! documented reason) — `PopulationSource` lives on `ReportEntry`, one level up, and is threaded
//! through by whichever caller has it in hand.

use urban_integrity_schema::types::verdict::{Coverage, PopulationSource, Verdict};
use urban_integrity_workbench_dto::verdict_view::{
    CoverageView, PopulationSourceView, VerdictView,
};

fn coverage_view(c: Coverage) -> CoverageView {
    CoverageView {
        population: c.population(),
        capable: c.capable(),
        excluded: c.excluded(),
    }
}

fn population_source_view(ps: &PopulationSource) -> PopulationSourceView {
    match ps {
        PopulationSource::SelfReported { reason } => PopulationSourceView::SelfReported {
            reason: reason.to_string(),
        },
        PopulationSource::OracleCorroborated { oracle_count } => {
            PopulationSourceView::OracleCorroborated {
                oracle_count: *oracle_count,
            }
        }
    }
}

/// Exhaustive, no wildcard arm — mirrors `VerdictKind::of`'s own discipline exactly
/// (`crates/schema/src/types/verdict.rs:242-256`). An 8th `Verdict` variant fails this match to
/// compile rather than silently defaulting a new verdict kind into an existing display bucket.
///
/// PROVE THIS CAN FAIL (per `docs/DEVELOPMENT.md`'s "Proving a Test Can Fail"): temporarily
/// comment out the `Verdict::Skipped { .. } => ...` arm below while editing this function and
/// try to build — the match becomes non-exhaustive and the crate fails to compile with
/// "non-exhaustive patterns: `Verdict::Skipped { .. }` not covered" (E0004), exactly as removing
/// an arm from `VerdictKind::of` would. Restore the arm before committing.
pub fn to_verdict_view(v: &Verdict, population_source: &PopulationSource) -> VerdictView {
    match v {
        Verdict::Hit { coverage, matches } => VerdictView::Hit {
            coverage: coverage_view(*coverage),
            matches: *matches,
            population_source: population_source_view(population_source),
        },
        Verdict::Weak { coverage, matches } => VerdictView::Weak {
            coverage: coverage_view(*coverage),
            matches: *matches,
            population_source: population_source_view(population_source),
        },
        Verdict::Clear(coverage) => VerdictView::Clear {
            coverage: coverage_view(*coverage),
            population_source: population_source_view(population_source),
        },
        Verdict::NotApplicable { coverage, reason } => VerdictView::NotApplicable {
            coverage: coverage_view(*coverage),
            reason: reason.clone(),
            population_source: population_source_view(population_source),
        },
        Verdict::DataGap { missing_input } => VerdictView::DataGap {
            missing_input: missing_input.clone(),
        },
        Verdict::Blocked { prerequisite } => VerdictView::Blocked {
            prerequisite: prerequisite.clone(),
        },
        Verdict::Skipped { cause } => VerdictView::Skipped {
            cause_description: cause.describe(),
            upstream: cause.upstream().map(str::to_string),
            tier: cause.tier().map(str::to_string),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_reported() -> PopulationSource {
        PopulationSource::SelfReported { reason: "x" }
    }

    /// Asserts one distinct `VerdictView` shape per `VerdictKind` — the same falsifiability
    /// guarantee `VerdictKind::of` gives `Verdict` itself, ported to this crate's boundary.
    ///
    /// PROVE THIS CAN FAIL (per `docs/DEVELOPMENT.md`): this test was first run against a
    /// version of `to_verdict_view` with the `Hit`/`Weak` arms swapped (`Verdict::Hit` mapped to
    /// `VerdictView::Weak` and vice versa) — the `matches!(hit, VerdictView::Hit { .. })`
    /// assertion below failed for real (observed: `hit` was a `VerdictView::Weak`), before the
    /// arms were corrected to the version committed here. Also see `to_verdict_view`'s own
    /// doc comment for the compile-time exhaustiveness proof (commenting out the `Skipped` arm).
    #[test]
    fn every_verdict_kind_maps_to_a_distinct_verdict_view_variant() {
        let hit = to_verdict_view(&Verdict::classify(5, 5, 5, 1, ""), &self_reported());
        assert!(matches!(hit, VerdictView::Hit { .. }));

        let weak = to_verdict_view(&Verdict::classify(5, 5, 3, 5, ""), &self_reported());
        assert!(matches!(weak, VerdictView::Weak { .. }));

        let clear = to_verdict_view(&Verdict::classify(10, 10, 0, 1, ""), &self_reported());
        assert!(matches!(clear, VerdictView::Clear { .. }));

        let not_applicable = to_verdict_view(
            &Verdict::classify(10, 7, 0, 1, "3 structurally excluded"),
            &self_reported(),
        );
        assert!(matches!(not_applicable, VerdictView::NotApplicable { .. }));

        let data_gap = to_verdict_view(&Verdict::data_gap("missing"), &self_reported());
        assert!(matches!(data_gap, VerdictView::DataGap { .. }));

        let blocked = to_verdict_view(&Verdict::blocked("prereq"), &self_reported());
        assert!(matches!(blocked, VerdictView::Blocked { .. }));

        let upstream = Verdict::classify(10, 10, 0, 1, "");
        let skipped = to_verdict_view(
            &Verdict::skipped_dependency("identity-integrity", &upstream),
            &self_reported(),
        );
        assert!(matches!(skipped, VerdictView::Skipped { .. }));

        // All 7 must be DISTINCT variant tags, not just individually matchable:
        let all = [hit, weak, clear, not_applicable, data_gap, blocked, skipped];
        let tags: std::collections::HashSet<_> = all
            .iter()
            .map(|v| {
                serde_json::to_value(v).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            tags.len(),
            7,
            "expected 7 distinct VerdictView kind tags, got: {tags:?}"
        );
    }
}
