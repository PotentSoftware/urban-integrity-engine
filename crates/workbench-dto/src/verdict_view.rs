//! `VerdictView` — the wire-format mirror of `crates/schema::types::verdict::VerdictKind`.
//!
//! This is the DTO-crate inverse of `verdict.rs`'s Deserialize-avoidance rule: `VerdictView`
//! legitimately derives `Deserialize` because it carries no invariant to bypass — it is a
//! display-only DTO, never round-tripped back to the server as authoritative (see
//! `crates/workbench`'s `From<&Verdict>`-style conversion in a later plan, which is the ONLY
//! producer of a real `VerdictView`). It must never be named bare `Verdict`/`Coverage`/
//! `PopulationSource`/`SkipCause` — always the `*View` suffix — so the CI Deserialize-guard grep
//! (added in plan 42-02) can tell a legitimate DTO mirror from the real type reappearing with
//! `Deserialize`.

use serde::{Deserialize, Serialize};

/// Wire-format mirror of `VerdictKind`'s seven variants, tagged so each produces a structurally
/// distinct `"kind"` value in its JSON representation.
///
/// `Hit`/`Weak`/`Clear`/`NotApplicable` each require both `coverage: CoverageView` and
/// `population_source: PopulationSourceView` at construction — omitting either is a compile
/// error, not a runtime default. This is WBX-05's "same visual weight" rule enforced at the type
/// level, not just in CSS: a renderer cannot show one of these four verdict kinds without also
/// having the coverage numbers and population source in hand.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum VerdictView {
    Hit {
        coverage: CoverageView,
        matches: u64,
        population_source: PopulationSourceView,
    },
    Weak {
        coverage: CoverageView,
        matches: u64,
        population_source: PopulationSourceView,
    },
    Clear {
        coverage: CoverageView,
        population_source: PopulationSourceView,
    },
    NotApplicable {
        coverage: CoverageView,
        reason: String,
        population_source: PopulationSourceView,
    },
    DataGap {
        missing_input: String,
    },
    Blocked {
        prerequisite: String,
    },
    /// `SkipCause`'s inner enum is deliberately unnameable outside `crates/schema` (see
    /// `verdict.rs`'s doc comment on `SkipCause`), so this variant only ever carries the
    /// pre-rendered `SkipCause::describe()` string plus the two optional structured fields the
    /// public accessors (`.upstream()`, `.tier()`) already expose safely — never a nameable
    /// mirror of the private `SkipCauseInner`.
    Skipped {
        cause_description: String,
        upstream: Option<String>,
        tier: Option<String>,
    },
}

/// All three counts carried explicitly — `excluded` is never derived client-side from
/// `population - capable`. The browser never recomputes it, so a rendering bug cannot silently
/// miscompute the exclusion count.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoverageView {
    pub population: u64,
    pub capable: u64,
    pub excluded: u64,
}

/// Wire-format mirror of `PopulationSource`, tagged so `SelfReported`/`OracleCorroborated`
/// produce structurally distinct `"source"` values in their JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "source")]
pub enum PopulationSourceView {
    SelfReported { reason: String },
    OracleCorroborated { oracle_count: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_reported() -> PopulationSourceView {
        PopulationSourceView::SelfReported {
            reason: "test".to_string(),
        }
    }

    fn coverage() -> CoverageView {
        CoverageView {
            population: 10,
            capable: 10,
            excluded: 0,
        }
    }

    /// One hand-constructed instance of each of the 7 `VerdictView` variants, used by both the
    /// distinct-tag test and the round-trip test below.
    fn all_variants() -> Vec<VerdictView> {
        vec![
            VerdictView::Hit {
                coverage: coverage(),
                matches: 3,
                population_source: self_reported(),
            },
            VerdictView::Weak {
                coverage: coverage(),
                matches: 1,
                population_source: self_reported(),
            },
            VerdictView::Clear {
                coverage: coverage(),
                population_source: self_reported(),
            },
            VerdictView::NotApplicable {
                coverage: coverage(),
                reason: "3 structurally excluded".to_string(),
                population_source: self_reported(),
            },
            VerdictView::DataGap {
                missing_input: "missing OSINT source".to_string(),
            },
            VerdictView::Blocked {
                prerequisite: "identity-integrity".to_string(),
            },
            VerdictView::Skipped {
                cause_description: "upstream 'identity-integrity' returned Clear".to_string(),
                upstream: Some("identity-integrity".to_string()),
                tier: None,
            },
        ]
    }

    /// Proves every one of the 7 `VerdictView` variants produces its own, structurally distinct
    /// `"kind"` tag — the same falsifiability guarantee `VerdictKind::of` gives the real
    /// `Verdict` in `crates/schema`, ported to this crate's wire format.
    ///
    /// PROVE THIS CAN FAIL (per `docs/DEVELOPMENT.md`'s "Proving a Test Can Fail"): this test was
    /// run against a version of `VerdictView` with the `#[serde(tag = "kind")]` attribute
    /// removed — every assertion below failed (`value["kind"]` resolved to `Null` instead of the
    /// expected string, because untagged/externally-tagged serde enums do not produce a `"kind"`
    /// key at all) — before the attribute was restored to the version committed here.
    #[test]
    fn each_variant_has_a_distinct_kind_tag() {
        let cases: Vec<(VerdictView, &str)> = vec![
            (
                VerdictView::Hit {
                    coverage: coverage(),
                    matches: 3,
                    population_source: self_reported(),
                },
                "Hit",
            ),
            (
                VerdictView::Weak {
                    coverage: coverage(),
                    matches: 1,
                    population_source: self_reported(),
                },
                "Weak",
            ),
            (
                VerdictView::Clear {
                    coverage: coverage(),
                    population_source: self_reported(),
                },
                "Clear",
            ),
            (
                VerdictView::NotApplicable {
                    coverage: coverage(),
                    reason: "3 structurally excluded".to_string(),
                    population_source: self_reported(),
                },
                "NotApplicable",
            ),
            (
                VerdictView::DataGap {
                    missing_input: "missing OSINT source".to_string(),
                },
                "DataGap",
            ),
            (
                VerdictView::Blocked {
                    prerequisite: "identity-integrity".to_string(),
                },
                "Blocked",
            ),
            (
                VerdictView::Skipped {
                    cause_description: "upstream 'identity-integrity' returned Clear".to_string(),
                    upstream: Some("identity-integrity".to_string()),
                    tier: None,
                },
                "Skipped",
            ),
        ];

        for (view, expected_kind) in cases {
            let value = serde_json::to_value(&view).expect("VerdictView must serialize");
            assert_eq!(
                value["kind"],
                serde_json::json!(expected_kind),
                "unexpected kind tag for {view:?}"
            );
        }
    }

    /// Round-trips at least one hand-constructed instance of each of the 7 variants through
    /// `serde_json`. Legitimate here — `VerdictView` is a display-only DTO with no invariant to
    /// bypass, unlike the real `Verdict` in `crates/schema` (see D-03, and this module's
    /// top-of-file doc comment).
    #[test]
    fn round_trips_every_variant_through_json() {
        for view in all_variants() {
            let json = serde_json::to_string(&view).expect("VerdictView must serialize");
            let back: VerdictView =
                serde_json::from_str(&json).expect("VerdictView must deserialize");
            assert_eq!(back, view, "round-trip mismatch for {json}");
        }
    }

    /// `CoverageView` carries all three counts explicitly — `excluded` is never recomputed
    /// client-side, so a serialized payload always shows the exact number the server sent.
    #[test]
    fn coverage_view_round_trips_all_three_counts_explicitly() {
        let cov = CoverageView {
            population: 148,
            capable: 8,
            excluded: 140,
        };
        let json = serde_json::to_string(&cov).unwrap();
        let back: CoverageView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cov);
        assert_eq!(back.excluded, 140);
    }

    /// `PopulationSourceView`'s two variants produce distinct `"source"` tags, matching the same
    /// tagging discipline `VerdictView` uses for `"kind"`.
    #[test]
    fn population_source_view_variants_have_distinct_source_tags() {
        let self_reported_value = serde_json::to_value(self_reported()).unwrap();
        assert_eq!(
            self_reported_value["source"],
            serde_json::json!("SelfReported")
        );

        let oracle = PopulationSourceView::OracleCorroborated { oracle_count: 5 };
        let oracle_value = serde_json::to_value(&oracle).unwrap();
        assert_eq!(
            oracle_value["source"],
            serde_json::json!("OracleCorroborated")
        );
    }
}
