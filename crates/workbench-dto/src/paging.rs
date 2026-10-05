//! `Page<T>` — the paging envelope every profile section is returned in (PIV-06, 45-CONTEXT D-07).
//!
//! # Why a section is a `Page`, not a `Vec`
//!
//! A bare `Vec` cannot distinguish "this entity has 3 contracts" from "3 of 1,900 are being
//! shown". `crates/core::persistence::queries::AlertPage` exists because that exact ambiguity hid
//! this project's only live Phoenix alert: `alerts list` returned 1,000 rows with no indication
//! that anything had been cut, so an analyst had no reason to look further. The profile read path
//! is the same shape of risk at larger scale — `queries.rs` carries `LIMIT 20000` on AwardedTo
//! edges and `LIMIT 100000` on five more, two of them with comments admitting they were already
//! raised once when live data outgrew them.
//!
//! So every section carries its rows, a cursor for the next window, and the total that window is
//! a window onto. The frontend's "Showing {n} of {total}." caption and its **Show more** control
//! (the phase 45 UI spec) are both composed from these three fields, which is why the total is sent
//! rather than inferred: a browser cannot know what it was not given.
//!
//! # What the fields mean
//!
//! - `rows` — this window, in the read path's deterministic order (plan 45-02 pins the order
//!   server-side; this type carries no ordering invariant of its own).
//! - `next_cursor` — an **opaque** token to pass back for the next window. `None` means this is
//!   the last window, and that is the only thing a consumer may infer from it: the browser must
//!   never parse, increment or construct one.
//! - `total` — how many rows the request matched in full, not how many are in `rows`.
//!
//! A section that needs a *second* count — 45-06's "unfiltered total retained in the section
//! header while an as-at filter is active" — puts it on that section, not here. `Page` answers one
//! question, and answering two from one field is how the figure gets misread.

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

/// One window onto a profile section, plus the truth about how many rows exist beyond it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    /// Opaque cursor for the next window; `None` iff this is the last window. Always a present key
    /// (`required_option`) — an omitted cursor would be indistinguishable from "no more rows",
    /// which is the one thing this field exists to state.
    #[serde(deserialize_with = "required_option")]
    pub next_cursor: Option<String>,
    /// How many rows the request matched in full. `total > rows.len()` is the normal paged case,
    /// and the caption the frontend renders from it.
    pub total: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Page<String> {
        Page {
            rows: vec!["first".to_string(), "second".to_string()],
            next_cursor: Some("cursor-2".to_string()),
            total: 1_900,
        }
    }

    /// A page round-trips with its cursor and its total intact. The total is the field this test
    /// exists for: a page that loses it degrades to the bare `Vec` this type replaced.
    #[test]
    fn a_page_round_trips_with_its_cursor_and_total() {
        let page = sample();
        let json = serde_json::to_value(&page).expect("Page must serialize");
        assert_eq!(json["total"], 1_900);
        assert_eq!(json["next_cursor"], "cursor-2");

        let back: Page<String> = serde_json::from_value(json).expect("Page must deserialize");
        assert_eq!(back, page);
    }

    /// The last window states its lastness explicitly, as a present `null`.
    #[test]
    fn the_last_page_carries_an_explicit_null_cursor() {
        let last = Page::<String> {
            rows: vec!["only".to_string()],
            next_cursor: None,
            total: 1,
        };
        let json = serde_json::to_string(&last).expect("serialize");
        assert!(
            json.contains("\"next_cursor\":null"),
            "the cursor key must be present and null on the last page, not omitted: {json}"
        );
    }

    /// `next_cursor` is `required_option`: an omitted key is an error, not a silent `None`.
    ///
    /// PROVE THIS CAN FAIL: with the `deserialize_with = "required_option"` attribute removed,
    /// serde's default `Option` handling accepts the omitted key and this assertion fails. That
    /// mutation was performed before this test was committed.
    #[test]
    fn an_omitted_cursor_key_is_an_error() {
        let omitted = r#"{"rows":["first"],"total":1}"#;
        assert!(
            serde_json::from_str::<Page<String>>(omitted).is_err(),
            "an omitted `next_cursor` key must be a deserialization error, not a silent None"
        );
    }

    /// `total` has no `#[serde(default)]`, so a page cannot arrive without the figure that makes
    /// it a page — a defaulted `0` alongside two rows would be a lie about the population.
    #[test]
    fn an_omitted_total_is_an_error_not_a_zero() {
        let omitted = r#"{"rows":["first"],"next_cursor":null}"#;
        assert!(
            serde_json::from_str::<Page<String>>(omitted).is_err(),
            "an omitted `total` must be a deserialization error, not a defaulted 0"
        );
    }
}
