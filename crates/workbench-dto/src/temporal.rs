//! `EffectiveRange` / `AsAtStatus` — when a fact was in force, and what an "as at" filter made of
//! it (PIV-04, 45-CONTEXT D-04).
//!
//! # The decision this module encodes
//!
//! The project's load-bearing domain rule says "every edge carries `created_at`, `effective_from`,
//! `effective_to`". Every edge carries the *fields*. Almost none of them carry *values*: measured
//! 2026-09-15 against this project's working analysis, **`effective_to` is null on all 511,754
//! edge events in the store**, and `effective_from` is populated on exactly two edge types
//! (`Controls`, `OwnsProperty`) out of the twelve the schema declares. Every `Directs`,
//! `AwardedTo` and `LocatedAt` edge — 248,420 of them — carries
//! `effective_from: null, effective_to: null`. The measurement and the analysis it was taken
//! against are recorded in the plan 45-06 summary and in `crates/core::persistence::queries`, which is
//! where an analysis id is allowed to appear; this crate is analysis-scoped by
//! `scripts/check-no-analysis-literals.sh` and must not name one.
//!
//! So an "as at" filter meets an undated row far more often than a dated one, and the rule for
//! that case is the whole design:
//!
//! > **A row whose range is unrecorded is retained and labelled [`AsAtStatus::Undated`], never
//! > dropped and never silently passed through as though it had been tested.**
//!
//! Absence of a date is not evidence that a fact was not in force. Dropping undated rows would
//! empty the Officers, Contracts, Planning applications and Addresses sections *entirely* on this
//! corpus — turning a filter into a blanker, with nothing on screen to say which of the two had
//! happened. Keeping them unmarked would make the filter a silent no-op on those four sections.
//! Neither failure raises an error; both look like a working feature. The third option — keep the
//! row and say that the filter could not vouch for it — is the only one an analyst can act on.
//!
//! [`NULL_RANGE_RULE`] and [`UNDATED_ROWS_ARE`] exist so the renderer states this rule from the
//! same source the read path applies it from, rather than retyping a word that could drift out of
//! agreement with the code (the phase 45 UI spec's as-at banner, whose bracketed word is fixed here).
//!
//! # Why nothing is removed from the profile
//!
//! An as-at filter **marks** rows; it never deletes them. The read path is shared: the CLI
//! `entity` command, the GraphQL `entity` resolver and `build_verify_verdict` all consume the same
//! `EntityProfile`, and `build_verify_verdict` derives `p_c_score` and `dissolved_relative_count`
//! by folding over those very vectors. A filter that removed rows in the read path would let a
//! *display* control move a *verdict* — the same class of failure 45-03 caught when splitting
//! planning applications out of contracts silently narrowed AC-DET-1's input by 86.9%.
//!
//! Hiding a row is therefore a rendering decision, taken by the renderer, from
//! [`AsAtStatus::OutOfForce`]. Counting one is a single predicate over rows that are all still
//! there, which is also what makes "a row excluded by the filter is not silently dropped from a
//! section's total" (the phase 45 UI spec) true by construction rather than by a parallel tally that
//! could disagree with the rows beside it.

use serde::{Deserialize, Serialize};

/// Copied verbatim from `population_meta::required_option` (see that module's doc comment for the
/// full rationale) — this crate's established convention is each DTO module owns its own copy,
/// never a shared import.
fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// The as-at banner's bracketed word (the phase 45 UI spec: "Facts with no recorded date range are
/// {included|excluded} — see below."), fixed here rather than at the render site so the sentence
/// on screen and the rule in [`crate::temporal`] cannot drift apart.
pub const UNDATED_ROWS_ARE: &str = "included";

/// The null-range rule as one sentence an analyst reads, for the as-at banner and for any other
/// surface that has to state it. Rendered verbatim; do not paraphrase at the call site.
pub const NULL_RANGE_RULE: &str =
    "Facts with no recorded date range are included and marked undated: the filter could not test \
     them, and an absent date is not evidence a fact was not in force.";

/// When a fact was in force, **as the record states it** — never as a reader would infer it.
///
/// The two variants are the same shape as [`crate::provenance::Provenance`]'s, and for the same
/// reason: the case a producer cannot answer is represented inside the type, carrying the reason,
/// rather than as an absent field that reads like an oversight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "range", rename_all = "snake_case")]
pub enum EffectiveRange {
    /// The record states at least one endpoint.
    Recorded {
        /// ISO-8601 start, or `None` when the record states no start. Always a present key.
        #[serde(deserialize_with = "required_option")]
        from: Option<String>,
        /// ISO-8601 end, or `None` when the record states no end. Always a present key.
        ///
        /// **`None` means "this record states no end", not "still in force".** The two are
        /// indistinguishable in this store — `effective_to` is null on every edge in it — so a
        /// renderer that prints "present" or "ongoing" here would be asserting something no
        /// dataset said. [`Self::Recorded::basis`] names the field that was read so the claim
        /// stays auditable.
        #[serde(deserialize_with = "required_option")]
        to: Option<String>,
        /// The record field(s) the endpoints were read from — e.g.
        /// `"officer.appointed_on / officer.resigned_on"`. A profile row can take its range from
        /// the edge that carries it or from the node the edge points at, and which one answered is
        /// part of the answer.
        basis: String,
    },
    /// The record states neither endpoint. Not "forever", not "never" — unknown.
    Unrecorded {
        /// Every field that was consulted and found empty, so a reader can tell "this dataset
        /// records no dates" from "this row lost its dates".
        basis: String,
    },
}

impl EffectiveRange {
    /// `true` when the record states neither endpoint — the case [`NULL_RANGE_RULE`] governs.
    pub fn is_unrecorded(&self) -> bool {
        matches!(self, EffectiveRange::Unrecorded { .. })
    }
}

/// What an "as at" filter made of one row.
///
/// Four variants because a row can be on the profile for four different reasons, and collapsing
/// any two of them loses the thing an analyst needs. There is no variant for "removed": nothing is
/// removed (see the module doc).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "as_at", rename_all = "snake_case")]
pub enum AsAtStatus {
    /// No as-at date was set, so no claim is made about this row's relation to one. Distinct from
    /// [`Self::InForce`]: an unfiltered profile has not established that anything was in force.
    NotFiltered,
    /// The row's recorded range covers the as-at date.
    InForce,
    /// The row's recorded range positively places it outside the as-at date. **The row is still
    /// here** — the phase 45 UI spec renders it muted with an "outside as at" chip, which is what makes
    /// PIV-04's worked example legible without arithmetic by the reader.
    OutOfForce,
    /// The row could not be tested: it records no usable endpoint. Retained under
    /// [`NULL_RANGE_RULE`] and marked, so the filter is never read as having vouched for it.
    Undated,
}

/// A row's temporal facts: the range the record states, and what the as-at filter made of it.
///
/// One grouped field rather than two loose ones, so a producer cannot update the range and leave
/// the status describing the old one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Temporal {
    pub range: EffectiveRange,
    pub status: AsAtStatus,
}

impl Temporal {
    /// A range as read, with no as-at filter applied yet — what every producer emits.
    pub fn unfiltered(range: EffectiveRange) -> Self {
        Self {
            range,
            status: AsAtStatus::NotFiltered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn both_range_variants_survive_a_json_round_trip_and_are_structurally_distinct() {
        let recorded = EffectiveRange::Recorded {
            from: Some("2015-01-04".to_string()),
            to: None,
            basis: "officer.appointed_on / officer.resigned_on".to_string(),
        };
        let unrecorded = EffectiveRange::Unrecorded {
            basis: "directs.meta.effective_from / directs.meta.effective_to".to_string(),
        };

        for value in [&recorded, &unrecorded] {
            let json = serde_json::to_value(value).expect("serialize");
            let back: EffectiveRange = serde_json::from_value(json).expect("deserialize");
            assert_eq!(&back, value);
        }

        assert_eq!(
            serde_json::to_value(&recorded).unwrap()["range"],
            json!("recorded")
        );
        assert_eq!(
            serde_json::to_value(&unrecorded).unwrap()["range"],
            json!("unrecorded")
        );
    }

    /// The end of a range is a present `null`, never an omitted key — the same rule this crate
    /// applies to `Provenance::tier`. An omitted `to` must be a deserialization error rather than
    /// a silent `None`, because "no end recorded" and "the producer forgot" are different facts.
    #[test]
    fn an_omitted_range_endpoint_fails_to_deserialize() {
        let complete = json!({
            "range": "recorded",
            "from": "2015-01-04",
            "to": null,
            "basis": "officer.appointed_on / officer.resigned_on",
        });
        serde_json::from_value::<EffectiveRange>(complete.clone())
            .expect("an explicit null end deserializes");

        let mut missing = complete;
        missing.as_object_mut().unwrap().remove("to");
        assert!(
            serde_json::from_value::<EffectiveRange>(missing).is_err(),
            "an omitted `to` must be an error, not a silently-defaulted None"
        );
    }

    #[test]
    fn every_as_at_status_survives_a_round_trip_and_has_its_own_tag() {
        let all = [
            AsAtStatus::NotFiltered,
            AsAtStatus::InForce,
            AsAtStatus::OutOfForce,
            AsAtStatus::Undated,
        ];
        let mut tags = std::collections::BTreeSet::new();
        for status in all {
            let json = serde_json::to_value(status).expect("serialize");
            let back: AsAtStatus = serde_json::from_value(json.clone()).expect("deserialize");
            assert_eq!(back, status);
            tags.insert(json["as_at"].as_str().expect("a tag").to_string());
        }
        assert_eq!(
            tags.len(),
            4,
            "four statuses must have four distinct wire tags, found {tags:?}"
        );
    }

    /// A producer that has read a range but not applied a filter says so, rather than claiming the
    /// row was in force. `NotFiltered` and `InForce` are different answers.
    #[test]
    fn an_unfiltered_row_does_not_claim_to_be_in_force() {
        let t = Temporal::unfiltered(EffectiveRange::Unrecorded {
            basis: "directs.meta.effective_from".to_string(),
        });
        assert_eq!(t.status, AsAtStatus::NotFiltered);
        assert_ne!(t.status, AsAtStatus::InForce);
    }

    /// The banner word and the rule sentence agree with each other. They are read from here by the
    /// renderer (45-12) and applied from here by the read path (45-06); if someone flips the rule
    /// and updates only one of them, this fails.
    #[test]
    fn the_stated_rule_and_the_banner_word_agree() {
        assert_eq!(UNDATED_ROWS_ARE, "included");
        assert!(
            NULL_RANGE_RULE.contains(UNDATED_ROWS_ARE),
            "the banner word `{UNDATED_ROWS_ARE}` must appear in the rule sentence it summarises: \
             {NULL_RANGE_RULE}"
        );
        assert!(
            !NULL_RANGE_RULE.contains("excluded"),
            "the rule sentence must not also claim undated facts are excluded"
        );
    }
}
