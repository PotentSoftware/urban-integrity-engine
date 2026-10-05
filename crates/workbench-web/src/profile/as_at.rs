//! The **"as at" date control** — PIV-04's UI half (the phase 45 UI spec, "As-at control (PIV-04)").
//!
//! # The control owns no state
//!
//! Plan 45-10 put the as-at date in the query string (`/entity/{id}?as_at=…`) so that a pivot
//! carries it, and plan 45-11 made the profile screen a pure function of
//! [`ProfileRoute`](crate::profile::pivot::ProfileRoute) so that Back and Forward restore the id
//! and the date together. Both hand this plan the same instruction, and it is the whole design of
//! this module: **the control sets one query parameter and stops.**
//!
//! So there is no signal here. Setting a date is [`use_navigate`] to a URL that states the new
//! window; clearing it is a navigation to the same entity with the parameter gone. The input's
//! displayed value is read from the route on every render, not from a copy of it. A date stored in
//! an `RwSignal` would be a second source of truth the Back button cannot reach — the screen would
//! show one window and the address bar would claim another — and
//! `the_as_at_control_stores_no_date_of_its_own` in `tests/pivot_reachable.rs` fails if one appears.
//!
//! # Why the inactive label does not say "today"
//!
//! The phase 45 UI spec's Copy Deck gives the inactive control as **"As at: today"**, defaulting the input
//! to today's date. That sentence describes a screen this workbench never renders. An unfiltered
//! profile is not filtered to today: every fact is listed, tested against nothing, and the wire
//! type says so in its own vocabulary — [`AsAtStatus::NotFiltered`] exists as a variant distinct
//! from `InForce` precisely because "no filter was applied" and "this was in force" are different
//! answers (45-06). A control reading "As at: today" beside a dissolved company's 2009 contracts
//! asserts a filter that is not running, which is the same class of defect as a filter that changes
//! the population silently — one direction over.
//!
//! It would also be a destructive default if it were ever honoured: most subjects in this corpus
//! are historical, so "as at today" would mark almost everything out of force before the analyst
//! had asked a question. The control therefore starts empty and says what the screen is showing —
//! [`AS_AT_ANY_DATE`] — and the deviation is recorded in the plan 45-12 summary rather than taken
//! silently.
//!
//! [`AsAtStatus::NotFiltered`]: urban_integrity_workbench_dto::AsAtStatus::NotFiltered

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

use crate::profile::pivot::profile_href;

/// The control's label stem (the phase 45 UI spec Copy Deck: "As at: …").
pub const AS_AT_LABEL: &str = "As at";

/// What the label reads when no date is set. **Not "today"** — see the module doc.
///
/// It names the *population on screen* rather than the control's own state, because that is what
/// an analyst needs to know before reading a row: every fact is listed, whenever it happened.
pub const AS_AT_ANY_DATE: &str = "any date";

/// The affordance beside an active date (the phase 45 UI spec Copy Deck: "plus a **Clear** affordance").
pub const AS_AT_CLEAR: &str = "Clear";

/// The control's own accessible name, which the visible label's colon form is not.
pub const AS_AT_INPUT_LABEL: &str = "Show this entity as at a date";

/// The label beside the control: `As at: 2021-06-30`, or `As at: any date` when none is set.
pub fn as_at_control_label(as_at: Option<&str>) -> String {
    match as_at {
        Some(date) if !date.is_empty() => format!("{AS_AT_LABEL}: {date}"),
        _ => format!("{AS_AT_LABEL}: {AS_AT_ANY_DATE}"),
    }
}

/// **The whole of what the control does**: the URL for this same entity under a new date window.
///
/// One function for both directions. `chosen` is what an `<input type="date">` hands back — either
/// an ISO `YYYY-MM-DD` string or `""` when the operator cleared it — and
/// [`profile_href`] already treats an empty date as no date, so clearing needs no branch of its
/// own and cannot drift out of agreement with setting.
///
/// It goes through `profile_href` rather than interpolating a query string so that the id is
/// percent-encoded by the same function every pivot anchor uses. `src/profile/as_at.rs` is
/// therefore listed in `MAY_OPEN_DIRECTLY` in `tests/pivot_reachable.rs`: this is not a pivot —
/// there is no `EntityRef` and no question of whether the target opens, because the target is the
/// page the operator is already on.
pub fn as_at_target(id: &str, chosen: &str) -> String {
    profile_href(id, Some(chosen))
}

/// The as-at control: a native `<input type="date">`, its label, and a **Clear** affordance when a
/// date is set.
///
/// No date-picker package (the phase 45 UI spec, "date input"). Mounted at `Density::Full` only — see
/// `ProfileHeader`.
#[component]
#[allow(non_snake_case)]
pub fn AsAtControl(id: String, as_at: Option<String>) -> impl IntoView {
    let navigate = use_navigate();
    let label = as_at_control_label(as_at.as_deref());
    let current = as_at.clone().unwrap_or_default();
    let id_for_input = id.clone();
    let id_for_clear = id;
    let navigate_for_clear = navigate.clone();

    view! {
        <span style="display: inline-flex; align-items: center; gap: var(--space-sm); flex-wrap: wrap;">
            <label class="text-label" style="color: var(--muted-foreground);" for="profile-as-at">
                {label}
            </label>
            <input
                id="profile-as-at"
                type="date"
                aria-label=AS_AT_INPUT_LABEL
                prop:value=current
                on:change=move |ev| {
                    let chosen = event_target_value(&ev);
                    navigate(&as_at_target(&id_for_input, &chosen), Default::default());
                }
                style="padding: var(--space-xs) var(--space-sm); background: var(--card); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); min-height: 44px;"
            />
            {as_at.is_some().then(|| {
                let navigate = navigate_for_clear.clone();
                let id = id_for_clear.clone();
                view! {
                    <button
                        type="button"
                        style="min-height: 44px; padding: 0 var(--space-md); background: transparent; border: 1px solid var(--border); border-radius: 6px; color: var(--primary); cursor: pointer;"
                        on:click=move |_| navigate(&as_at_target(&id, ""), Default::default())
                    >
                        {AS_AT_CLEAR}
                    </button>
                }
            })}
        </span>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three ids whose encodings differ, so a target built by string interpolation rather than
    /// through `profile_href` is visible.
    const IDS: &[&str] = &["GB-COH:X6999001", "director:jane-smith", "GB-MUT:X6999002"];

    /// **Setting a date changes the URL by exactly one query parameter and nothing else.**
    ///
    /// The assertion is not "the href contains the date" — that passes for a control that also
    /// dropped the id, changed its encoding, or navigated to a different entity. It is that the
    /// filtered href is the unfiltered href plus `?as_at={date}`, byte for byte.
    ///
    /// PROVE THIS CAN FAIL: make `as_at_target` return `format!("/entity/{id}?as_at={chosen}")` —
    /// the colon in every id survives unencoded and the suffix check fails on all three.
    #[test]
    fn setting_a_date_adds_one_query_parameter_and_changes_nothing_else() {
        let date = "2021-06-30";
        for id in IDS {
            let bare = profile_href(id, None);
            let filtered = as_at_target(id, date);
            assert!(
                !bare.is_empty() && bare.starts_with("/entity/"),
                "non-vacuity: the unfiltered href for {id} must be a profile path, got {bare}"
            );
            assert!(
                !bare.contains("as_at"),
                "non-vacuity: the unfiltered href must not already carry the parameter: {bare}"
            );
            assert_eq!(
                filtered,
                format!("{bare}?as_at={date}"),
                "the control must add one query parameter to the path it is already on"
            );
        }
    }

    /// **Clearing returns the same entity's unfiltered profile**, not a different entity and not a
    /// URL with an empty parameter on it (`ProfileRoute` treats `?as_at=` as unset, but a URL that
    /// says it is filtered when it is not would still be a lie in the address bar).
    ///
    /// PROVE THIS CAN FAIL: give `as_at_target` a branch that passes `""` straight into the query
    /// string — every id gains a trailing `?as_at=` and the equality fails.
    #[test]
    fn clearing_the_date_returns_the_same_entity_unfiltered() {
        for id in IDS {
            assert_eq!(as_at_target(id, ""), profile_href(id, None));
            assert!(
                !as_at_target(id, "").contains("as_at"),
                "a cleared control must leave no parameter behind: {}",
                as_at_target(id, "")
            );
            // Non-vacuity: the round trip really does change something when a date IS given.
            assert_ne!(as_at_target(id, ""), as_at_target(id, "2021-06-30"));
        }
    }

    /// **The inactive label does not claim a filter is running.**
    ///
    /// The phase 45 UI spec's Copy Deck says "As at: today"; this is the deviation, pinned so that it is a
    /// decision rather than a drift. An unfiltered profile lists facts from every date — the wire
    /// says `AsAtStatus::NotFiltered`, not `InForce` — so a label naming a date would describe a
    /// screen that is not on show.
    ///
    /// PROVE THIS CAN FAIL: set `AS_AT_ANY_DATE` to `"today"` — the first two assertions fail.
    #[test]
    fn the_inactive_label_names_no_date_because_no_filter_is_running() {
        let inactive = as_at_control_label(None);
        assert!(
            !inactive.contains("today"),
            "an unfiltered profile is not filtered to today: {inactive}"
        );
        assert_eq!(inactive, "As at: any date");
        assert_eq!(
            as_at_control_label(Some("")),
            inactive,
            "a bare ?as_at= is unset"
        );

        // Non-vacuity: an active label DOES name its date, so the test above is about the inactive
        // case rather than about a label that never says anything.
        let active = as_at_control_label(Some("2021-06-30"));
        assert_eq!(active, "As at: 2021-06-30");
        assert_ne!(active, inactive);
    }
}
