//! `DisambiguationView` — the list PIV-01 opens **before any profile does** (the phase 45 UI spec's
//! "Disambiguation list" contract).
//!
//! # Why this is its own type rather than `Vec<EntityRef>`
//!
//! `GET /api/entities/search` (NAV-06, plan 44-08) already returns `Vec<EntityRef>` from the
//! boot-time label index, and the entity picker renders `name · kind · id` from it. PIV-01 asks
//! for something the label index cannot answer: **type, status and ward** on each candidate row,
//! *before* the analyst commits to one. A status is a fact about the entity, so it comes from the
//! same `crates/core::persistence` read path the profile header renders (PIV-06/D-01) — which is
//! also what stops the disambiguation row and the profile it opens from disagreeing about whether
//! a company is dissolved.
//!
//! # `more_may_exist`, and the cap it reports
//!
//! `crates/core::persistence::queries::resolve_entity` — the D-07 single exact-match resolution
//! choke-point — ends with `entity_ids.truncate(10)` and returns no count of what it cut. A caller
//! therefore cannot distinguish "10 entities match" from "10 of 84 match", and the phase 45 UI spec's
//! heading (**{n} entities match "{query}".**) would state the first while the second was true.
//!
//! [`DisambiguationView::more_may_exist`] carries that as data rather than leaving it to be
//! inferred: `true` means the resolver's cap was reached and the real total is **unknown and at
//! least** `rows.len()`. The renderer must say so (e.g. "10+ entities match") rather than printing
//! a number it cannot stand behind. Closing the cap properly belongs in `resolve_entity`, where
//! the CLI would gain it too; this field is the honest report until then.

use serde::{Deserialize, Serialize};

use crate::entity_profile::EntityStatusView;
use crate::entity_ref::EntityRef;

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

/// One candidate match: who it is, what state it is in, and where it operates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DisambiguationRowView {
    /// The candidate itself — `id`, `name` and `kind`, the same triple the entity picker renders.
    pub entity: EntityRef,
    /// Status in the profile header's own vocabulary, never a raw enum name (the phase 45 UI spec).
    pub status: EntityStatusView,
    /// The ward this candidate's work sits in, **by name where the graph knows one** — 45-UI-SPEC
    /// requires a ward render its name, not its code, so this is an [`EntityRef`] (whose `name`
    /// the renderer uses) rather than a bare code string.
    ///
    /// `None` is a real answer, not a missing one: a company with no ward-coded contract or
    /// planning application in this analysis has no ward to show, and most Companies Finder
    /// suppliers are in that position. `required_option` — an omitted key is an error.
    #[serde(deserialize_with = "required_option")]
    pub ward: Option<EntityRef>,
}

/// The whole list, with the query it answers and whether the resolver's cap was reached.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DisambiguationView {
    /// The query string as the analyst typed it, echoed so the heading can quote it.
    pub query: String,
    pub rows: Vec<DisambiguationRowView>,
    /// `true` when the read path's own match cap was reached and the true total is unknown. See
    /// this module's doc comment — a renderer that prints `rows.len()` as "the" number of matches
    /// while this is `true` is stating a figure the server did not establish.
    pub more_may_exist: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, name: &str) -> DisambiguationRowView {
        DisambiguationRowView {
            entity: EntityRef {
                id: id.to_string(),
                name: name.to_string(),
                kind: "Entity".to_string(),
            },
            status: EntityStatusView::Active,
            ward: Some(EntityRef {
                id: "ward:X05999001".to_string(),
                name: "Example Ward".to_string(),
                kind: "Ward".to_string(),
            }),
        }
    }

    fn sample() -> DisambiguationView {
        DisambiguationView {
            query: "example".to_string(),
            rows: vec![row("GB-COH:X06999001", "Example Holdings Ltd")],
            more_may_exist: false,
        }
    }

    #[test]
    fn a_disambiguation_list_round_trips() {
        let view = sample();
        let json = serde_json::to_value(&view).expect("serialize");
        assert_eq!(json["rows"][0]["status"]["status"], "active");
        assert_eq!(json["rows"][0]["ward"]["name"], "Example Ward");
        let back: DisambiguationView = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, view);
    }

    /// A row with no ward states it as a present `null`, never an omitted key — the same
    /// absent-vs-omitted discipline the rest of this crate keeps.
    ///
    /// PROVE THIS CAN FAIL: with `deserialize_with = "required_option"` removed from
    /// `DisambiguationRowView::ward`, the omitted-key assertion below succeeds in deserializing
    /// and the test fails. That mutation was run before this test was committed.
    #[test]
    fn an_omitted_ward_key_is_an_error_but_an_explicit_null_is_a_value() {
        let mut view = sample();
        view.rows[0].ward = None;
        let json = serde_json::to_string(&view).expect("serialize");
        assert!(
            json.contains("\"ward\":null"),
            "a ward-less row must carry a present null: {json}"
        );

        let omitted =
            r#"{"entity":{"id":"a","name":"A","kind":"Entity"},"status":{"status":"active"}}"#;
        assert!(
            serde_json::from_str::<DisambiguationRowView>(omitted).is_err(),
            "an omitted `ward` key must be a deserialization error, not a silent None"
        );
    }

    /// `more_may_exist` has no `#[serde(default)]`: a list that arrives without it would be read
    /// as complete, which is the one thing the field exists to deny.
    #[test]
    fn an_omitted_more_may_exist_is_an_error_not_a_false() {
        let omitted = r#"{"query":"example","rows":[]}"#;
        assert!(
            serde_json::from_str::<DisambiguationView>(omitted).is_err(),
            "an omitted `more_may_exist` must be a deserialization error, not a defaulted false"
        );
    }
}
