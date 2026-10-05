//! `Provenance` — where one fact on the entity profile came from (PIV-03, 45-CONTEXT D-04).
//!
//! # Why this is an enum and not an `Option<Provenance>` on the row
//!
//! PIV-03 requires that *every* fact on the profile carries its provenance. An
//! `Option<Provenance>` field turns that requirement into a convention: the next section added to
//! the profile writes `provenance: None` and nothing fails. So the row types in
//! [`crate::entity_profile`] carry `provenance: Provenance` — a mandatory field — and the case a
//! producer genuinely cannot attribute is represented *inside* the type as
//! [`Provenance::Unattributed`], which carries the reason it could not.
//!
//! The consequence is the one 45-01 wants: a row that cannot say where it came from does not
//! compile, and one that admits it cannot is visibly different on the wire from one that can.
//!
//! # Why `Unattributed` is not "no provenance"
//!
//! The phase 45 UI spec renders `Unattributed` as its own chip in `--muted-foreground` with no border:
//! "an unattributed fact that looks sourced is worse than one that admits it". `reason` exists so
//! a producer has to say *why* rather than reaching for a plausible-looking placeholder — which
//! is the failure mode plan 45-04's "no row type gains provenance by defaulting it to a
//! placeholder" names directly. It is not rendered in the chip.
//!
//! Like every type in this crate, no field is `#[serde(default)]` — an omitted key is a
//! deserialization error, not a silently-defaulted empty string.

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

/// The confidence tier of the `identity_links` row a fact was derived from (PIV-03's "identity-link
/// tier where applicable").
///
/// **Exactly two variants, and `pending_review` is deliberately unrepresentable.** ENR-11 forbids
/// surfacing a candidate link, and the phase 45 UI spec says the same in rendering terms: two tier
/// tokens exist, "`pending_review` has no token by design and a chip must never be authored for
/// it". Leaving the third status out of the wire vocabulary means a handler cannot accidentally
/// ship one through this boundary even if a future query forgets the filter.
///
/// This is a vocabulary limit, not an invariant check — consistent with this crate's "view types
/// carry no domain invariant" rule. The `status IN ('auto_linked','confirmed')` filter that
/// *enforces* ENR-11 stays server-side in `crates/core::persistence` (plan 45-04).
///
/// The wire strings match the `identity_links.status` values exactly (`"confirmed"`,
/// `"auto_linked"`), so a producer never has to translate between two spellings of one tier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkTier {
    /// An analyst confirmed the link.
    Confirmed,
    /// The link was auto-established above the confidence threshold.
    AutoLinked,
}

/// Where one fact came from, tagged so the two cases are structurally distinct on the wire
/// (`"attribution": "attributed"` / `"attribution": "unattributed"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "attribution", rename_all = "snake_case")]
pub enum Provenance {
    /// The three things PIV-03 asks for: the source dataset, the `graph_events` timestamp, and
    /// the identity-link tier where the fact came from a link.
    Attributed {
        /// The dataset the fact came from, as the producer names it — e.g. `"CH"`, `"CCOD"`,
        /// `"OCOD"`, `"PPD"`, `"EC"`, `"ICIJ"`. Rendered through the frontend's `<Abbr>` lookup
        /// (the phase 45 UI spec), so it is a short source name rather than a sentence.
        source_dataset: String,
        /// The `graph_events` timestamp of the event that carried this fact, ISO-8601. Not the
        /// time the profile was assembled — a fact's age is a property of the fact.
        observed_at: String,
        /// `Some` only when the fact was derived from an `identity_links` row; `None` for a fact
        /// read straight from a primary dataset. Always a present key
        /// (`required_option`) — an omitted `tier` is a bug, an explicit `null` is the statement
        /// "this fact is not link-derived".
        #[serde(deserialize_with = "required_option")]
        tier: Option<LinkTier>,
    },
    /// The fact exists but its provenance could not be established. Rendered as its own
    /// `Unattributed` chip, never as a blank and never as a plausible-looking source.
    Unattributed {
        /// Why attribution failed — e.g. `"no source event found for this row"`. For the operator
        /// reading a bug report and for plan 45-04's coverage measurement, not for the chip.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attributed() -> Provenance {
        Provenance::Attributed {
            source_dataset: "CH".to_string(),
            observed_at: "2026-07-31T00:00:00Z".to_string(),
            tier: None,
        }
    }

    /// Both variants survive a JSON round trip, and each carries a structurally distinct
    /// `attribution` tag.
    #[test]
    fn both_provenance_variants_round_trip_under_distinct_tags() {
        for (provenance, tag) in [
            (attributed(), "attributed"),
            (
                Provenance::Unattributed {
                    reason: "no source event found for this row".to_string(),
                },
                "unattributed",
            ),
        ] {
            let json = serde_json::to_value(&provenance).expect("Provenance must serialize");
            assert_eq!(json["attribution"], tag);
            let back: Provenance =
                serde_json::from_value(json).expect("Provenance must deserialize");
            assert_eq!(back, provenance);
        }
    }

    /// A link-derived fact carries its tier, and the tier's wire spelling is the
    /// `identity_links.status` value, not a re-spelling of it.
    #[test]
    fn tier_wire_strings_match_identity_link_status_values() {
        let confirmed = serde_json::to_value(LinkTier::Confirmed).expect("serialize");
        let auto = serde_json::to_value(LinkTier::AutoLinked).expect("serialize");
        assert_eq!(confirmed, "confirmed");
        assert_eq!(auto, "auto_linked");
    }

    /// ENR-11 at the wire boundary: `pending_review` is not in the tier vocabulary, so a payload
    /// carrying one fails to deserialize rather than reaching a renderer.
    ///
    /// PROVE THIS CAN FAIL: run against a `LinkTier` with a third `PendingReview` variant added,
    /// this assertion fails (the payload deserializes). That mutation was performed before this
    /// test was committed.
    #[test]
    fn a_pending_review_tier_is_not_representable() {
        assert!(
            serde_json::from_str::<LinkTier>("\"pending_review\"").is_err(),
            "pending_review must not be a renderable tier (ENR-11)"
        );
    }

    /// `tier` is `required_option`: an explicit `null` is a value, an omitted key is an error.
    ///
    /// PROVE THIS CAN FAIL: with the `deserialize_with = "required_option"` attribute removed,
    /// serde's default `Option` handling accepts the omitted key as `None` and this assertion
    /// fails. That mutation was performed before this test was committed.
    #[test]
    fn an_omitted_tier_key_is_an_error_but_an_explicit_null_is_a_value() {
        let omitted = r#"{"attribution":"attributed","source_dataset":"CH","observed_at":"2026-07-31T00:00:00Z"}"#;
        assert!(
            serde_json::from_str::<Provenance>(omitted).is_err(),
            "an omitted `tier` key must be a deserialization error, not a silent None"
        );

        let explicit_null = r#"{"attribution":"attributed","source_dataset":"CH","observed_at":"2026-07-31T00:00:00Z","tier":null}"#;
        let parsed: Provenance =
            serde_json::from_str(explicit_null).expect("an explicit null tier is a valid value");
        assert_eq!(parsed, attributed());
    }

    /// `reason` on `Unattributed` is mandatory — a producer that cannot attribute a fact still has
    /// to say why, rather than shipping a bare `Unattributed` marker.
    #[test]
    fn unattributed_without_a_reason_fails_to_deserialize() {
        let no_reason = r#"{"attribution":"unattributed"}"#;
        assert!(
            serde_json::from_str::<Provenance>(no_reason).is_err(),
            "`Unattributed` must carry the reason attribution failed"
        );
    }
}
