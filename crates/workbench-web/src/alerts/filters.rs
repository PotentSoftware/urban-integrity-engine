//! `AlertFiltersRail` — the `/alerts` filter rail (the phase 46 UI spec "Filter rail (D-13)"), the
//! second of the two per-row building blocks 46-08 shipped. Mounted by 46-09's `/alerts` list page.
//!
//! Structured pickers only, no free-text box — the SAME governing rule `canvas_filters.rs`'s own
//! module doc states and this file quotes rather than re-derives: "a free-text box that gets
//! parsed is a query language whatever its author calls it, and the moment one exists the copy
//! beside it has to explain a syntax." Every control here writes one field of the eventual
//! `/api/alerts` query, reusing `tool_rail`'s typed-field family (`checklist_row`, `date_field`)
//! where it fits, and both `P_c` band constants from their own crates (`P_C_THRESHOLD_BANDS` from
//! `workbench-dto::canvas_filter`, `P_C_BAND_NOTE` from this crate's own `canvas_filters`).
//!
//! # The `P_c` fields are local and bounded, not `tool_rail::number_field_f64` (46-12)
//!
//! The phase 46 UI spec's table names `tool_rail::number_field_f64` for this row, but that helper is
//! shared with the Graph page's community "Resolution" field, which has no `0..1` bound. A `P_c`
//! bound is a `0.0..=1.0` range that needs a `step`, a `min`/`max` and 44x44px steppers the shared
//! unbounded helper does not have — 46-UAT test 3 found decimals like 0.85 unenterable (the rail
//! was remounting on every keystroke, per the 46-UAT P_c-range investigation notes) and the
//! native spinner too small (the phase 46 UI spec). [`pc_bound_field`] is the local, bounded
//! replacement for both the min and max `P_c` fields; it does not touch `tool_rail.rs`.
//!
//! # Detector and ward are single-select, not a multi-checklist, despite the checklist widget
//!
//! The phase 46 UI spec's table names `tool_rail::checklist_row` for both groups, but the wire contract
//! underneath (`crates/workbench::alerts::AlertListQuery.{motif, ward}`, both `Option<String>`,
//! mirroring `core::persistence::queries::AlertFilter.{motif_type, ward_code}`) accepts at most
//! ONE value per field, not a set — unlike `canvas_filters.rs`'s `node_kinds`/`edge_types`, which
//! really are `Vec<String>` on the wire. So each row's checkbox here behaves as an exclusive
//! toggle: checking one clears whatever was previously selected in the same group, and unchecking
//! the active row clears the field to "every {detector,ward}". This is the single-select shape
//! `RwSignal<Option<String>>` already implies (per this plan's own `<interfaces>` prop table),
//! not a design invented in this file.

use leptos::prelude::*;
use urban_integrity_workbench_dto::alerts::{AlertFacetOptionView, AlertFacetsView};
use urban_integrity_workbench_dto::canvas_filter::P_C_THRESHOLD_BANDS;

use crate::canvas_filters::{facet_label, P_C_BAND_NOTE};
use crate::tool_rail::{checklist_row, date_field};

/// The filter rail's own caption, matching `canvas_filters.rs`'s `FILTERS_CAPTION` treatment
/// exactly (Label size, not Heading).
pub const FILTERS_CAPTION: &str = "Filters";

/// The D-13 ward footnote — how many matching alerts carry no ward code and are therefore never
/// matched by a ward filter. Rendered unconditionally, even at `n == 0` (D-10's "a control that
/// vanishes at zero is the same defect one step further on").
pub fn no_ward_code_footnote(n: usize) -> String {
    format!("{n} alerts carry no ward code and are never matched by a ward filter.")
}

/// One exclusive-toggle checklist row: checking it sets `signal` to `Some(value)`; unchecking the
/// currently-active row clears it to `None`. Checking a different row in the same group therefore
/// clears the old selection implicitly, since only one row at a time can read `checked = true`.
fn exclusive_facet_row(
    option: &AlertFacetOptionView,
    signal: RwSignal<Option<String>>,
) -> impl IntoView {
    let value = option.value.clone();
    let label = facet_label(&option.label, option.count as u64);
    let disabled = option.count == 0;
    let for_checked = value.clone();
    let for_toggle = value.clone();
    let checked = Signal::derive(move || signal.get().as_deref() == Some(for_checked.as_str()));
    checklist_row(
        label,
        checked,
        Signal::derive(move || disabled),
        Signal::derive(|| None),
        move |on| {
            if on {
                signal.set(Some(for_toggle.clone()));
            } else {
                signal.set(None);
            }
        },
    )
}

/// The `P_c` fields' single numeric input step attribute — see [`pc_bound_field`].
pub(crate) const PC_INPUT_STEP: &str = "0.01";
/// How far the `pc_bound_field` −/+ steppers move a bound per click.
pub(crate) const PC_STEPPER_DELTA: f64 = 0.05;

/// Parses a `P_c` bound from raw input text: trims, then parses as `f64`. Empty, unparseable or
/// non-finite (`NaN`/`inf`) input is `None` — not a bound of `0.0`, which would silently narrow
/// the filter. A finite value is clamped to `[0.0, 1.0]` but never rounded: `0.855` is a
/// legitimate typed bound, and rounding it here would fight the operator's own keystrokes.
pub(crate) fn parse_pc_bound(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = trimmed.parse::<f64>().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    Some(parsed.clamp(0.0, 1.0))
}

/// Steps a `P_c` bound by `delta`, clamped to `[0.0, 1.0]` and rounded to 2 decimal places (so
/// `0.85 + 0.05` reads `0.9`, not `0.9000000000000001`).
pub(crate) fn step_pc(current: f64, delta: f64) -> f64 {
    let stepped = (current + delta).clamp(0.0, 1.0);
    (stepped * 100.0).round() / 100.0
}

/// The stepper buttons' only arithmetic: an absent bound (`None`) steps from `empty_means`
/// (`0.0` for Minimum, `1.0` for Maximum) rather than from some other implicit default, so this
/// "empty field" behaviour is unit-tested here instead of living untested inside a view closure.
pub(crate) fn step_bound(current: Option<f64>, empty_means: f64, delta: f64) -> f64 {
    step_pc(current.unwrap_or(empty_means), delta)
}

/// The hint rendered under the `P_c range` group when the two bounds are inverted (46-REVIEW-GAPS
/// WR-02). Without it an inverted range reads exactly like "no alerts match your filters", which
/// is a materially different situation from "your minimum and maximum are swapped".
pub const PC_RANGE_INVERTED_HINT: &str =
    "Minimum P_c is greater than Maximum P_c \u{2014} no alerts can match this range.";

/// Whether a `P_c` range is empty by definition: `min > max`. An absent maximum (`None`) means
/// "no upper bound" and can never invert the range. Equal bounds are NOT inverted — `min == max`
/// still matches alerts scored exactly at that value.
pub(crate) fn pc_range_inverted(min: f64, max: Option<f64>) -> bool {
    max.is_some_and(|max| min > max)
}

/// A bounded `P_c` field: `0.0..=1.0`, `step="0.01"`, commit-on-change (not per keystroke), and
/// 44x44px −/+ steppers that move the bound by [`PC_STEPPER_DELTA`]. Serves both the Minimum and
/// Maximum `P_c` fields — see the module doc's "`P_c` fields are local and bounded" section for
/// why this replaces `tool_rail::number_field_f64` here rather than reusing it.
///
/// `value` is what `prop:value` renders and what the steppers read as `current`. `commit` is the
/// ONLY write path: `on:change` calls `commit(parse_pc_bound(&raw))`, and each stepper button
/// calls `commit(Some(step_bound(...)))`. `empty_means` is the bound an absent value steps from
/// (`0.0` for Minimum, `1.0` for Maximum).
fn pc_bound_field(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    value: Signal<Option<f64>>,
    commit: impl Fn(Option<f64>) + Copy + Send + Sync + 'static,
    empty_means: f64,
) -> impl IntoView {
    let describedby = format!("{id}-help");
    let describedby_for_span = describedby.clone();
    let dec_label = format!("Decrease {label} by {PC_STEPPER_DELTA}");
    let inc_label = format!("Increase {label} by {PC_STEPPER_DELTA}");
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <label class="text-label" for=id>{label}</label>
            <div style="display: flex; gap: var(--space-xs); align-items: center;">
                <button
                    type="button"
                    class="text-body control-button"
                    style="width: 44px; height: 44px; padding: 0; flex: none;"
                    aria-label=dec_label
                    on:click=move |_| {
                        let current = value.get_untracked();
                        commit(Some(step_bound(current, empty_means, -PC_STEPPER_DELTA)));
                    }
                >
                    "\u{2212}"
                </button>
                <input
                    id=id
                    type="number"
                    inputmode="decimal"
                    min="0"
                    max="1"
                    step=PC_INPUT_STEP
                    class="pc-bound-input"
                    aria-describedby=describedby
                    prop:value=move || value.get().map(|v| v.to_string()).unwrap_or_default()
                    on:change=move |ev| {
                        let raw = event_target_value(&ev);
                        commit(parse_pc_bound(&raw));
                    }
                    style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); \
                           background: var(--card); border: 1px solid var(--border); border-radius: 6px; \
                           color: var(--foreground); min-height: 44px;"
                />
                <button
                    type="button"
                    class="text-body control-button"
                    style="width: 44px; height: 44px; padding: 0; flex: none;"
                    aria-label=inc_label
                    on:click=move |_| {
                        let current = value.get_untracked();
                        commit(Some(step_bound(current, empty_means, PC_STEPPER_DELTA)));
                    }
                >
                    "+"
                </button>
            </div>
            <span id=describedby_for_span class="text-label" style="color: var(--muted-foreground);">{help}</span>
        </div>
    }
}

/// The `/alerts` filter rail: **Detector**, **Ward**, **`P_c` range**, **Detected between**, and
/// **Clear filters** — 320px fixed width (the phase 46 UI spec's "comfortable narrow column"
/// measurement, the same one the Phase 44 tool rail and the Phase 50 card minimum already use).
///
/// `facets` is a [`Signal`], not an owned value (46-12): the rail itself is mounted ONCE, as a
/// static child of the page (see `alerts/mod.rs`'s `AlertsPage`), and only the Detector/Ward
/// checklists and the no-ward-code footnote below react to a facets update by re-reading it in
/// their own `move ||` closures. Everything else in this section — the caption, the `P_c` group,
/// the date group, Clear filters — is static and is never torn down by a facets change. That
/// mounting discipline, not this component's own body, is what keeps a focused `P_c` input alive
/// across the refetch every keystroke triggers; see
/// the 46-UAT P_c-range investigation notes for the remount this replaces.
#[component]
#[allow(non_snake_case)]
pub fn AlertFiltersRail(
    #[prop(into)] facets: Signal<Option<AlertFacetsView>>,
    ward_code: RwSignal<Option<String>>,
    motif: RwSignal<Option<String>>,
    min_score: RwSignal<f64>,
    max_score: RwSignal<Option<f64>>,
    from: RwSignal<Option<String>>,
    to: RwSignal<Option<String>>,
    on_clear: impl Fn() + 'static + Send + Sync,
) -> impl IntoView {
    view! {
        <section style="width: 320px; background: var(--card); border: 1px solid var(--border); \
                         border-radius: 8px; padding: var(--space-md); display: flex; \
                         flex-direction: column; gap: var(--space-md);">
            <h2 class="text-label" style="margin: 0;">{FILTERS_CAPTION}</h2>

            <div>
                <span class="text-label">"Detector"</span>
                {move || facets.with(|f| {
                    f.as_ref().map(|f| {
                        f.detectors.iter().map(|option| exclusive_facet_row(option, motif)).collect_view()
                    })
                })}
            </div>

            <div>
                <span class="text-label">"Ward"</span>
                <div style="max-height: 320px; overflow-y: auto; display: flex; flex-direction: column;">
                    {move || facets.with(|f| {
                        f.as_ref().map(|f| {
                            f.wards.iter().map(|option| exclusive_facet_row(option, ward_code)).collect_view()
                        })
                    })}
                </div>
                {move || facets.with(|f| {
                    f.as_ref().map(|f| view! {
                        <span class="text-label" style="color: var(--muted-foreground);">
                            {no_ward_code_footnote(f.no_ward_code_count)}
                        </span>
                    })
                })}
            </div>

            <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                <span class="text-label">"P_c range"</span>
                {pc_bound_field(
                    "alert-filter-min-score",
                    "Minimum P_c",
                    "0.0 to 1.0",
                    Signal::derive(move || Some(min_score.get())),
                    move |v| min_score.set(v.unwrap_or(0.0)),
                    0.0,
                )}
                {pc_bound_field(
                    "alert-filter-max-score",
                    "Maximum P_c",
                    "Leave empty for no upper bound.",
                    max_score.into(),
                    move |v| max_score.set(v),
                    1.0,
                )}
                {move || pc_range_inverted(min_score.get(), max_score.get()).then(|| view! {
                    <span class="text-label" role="alert" style="color: var(--destructive);">
                        {PC_RANGE_INVERTED_HINT}
                    </span>
                })}
                <div style="display: flex; gap: var(--space-xs); flex-wrap: wrap;">
                    {P_C_THRESHOLD_BANDS.iter().map(|(label, value)| {
                        let value = *value;
                        view! {
                            <button
                                type="button"
                                class="text-label control-button"
                                on:click=move |_| min_score.set(value)
                            >
                                {*label}
                            </button>
                        }
                    }).collect_view()}
                </div>
                <span class="text-label" style="color: var(--muted-foreground);">
                    {P_C_BAND_NOTE}
                </span>
            </div>

            <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                <span class="text-label">"Detected between"</span>
                {date_field("alert-filter-from-date", "From", "Leave empty for no lower bound.", from)}
                {date_field("alert-filter-to-date", "To", "Leave empty for no upper bound.", to)}
            </div>

            <button
                type="button"
                class="text-body control-button"
                on:click=move |_| on_clear()
            >
                "Clear filters"
            </button>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pc_bound_accepts_trims_and_clamps() {
        assert_eq!(parse_pc_bound("0.85"), Some(0.85));
        assert_eq!(parse_pc_bound(" .5 "), Some(0.5));
        assert_eq!(
            parse_pc_bound("1.7"),
            Some(1.0),
            "clamped to the upper bound"
        );
        assert_eq!(
            parse_pc_bound("-0.2"),
            Some(0.0),
            "clamped to the lower bound"
        );
        assert_eq!(
            parse_pc_bound(""),
            None,
            "empty text means no bound, not zero"
        );
        assert_eq!(parse_pc_bound("abc"), None);
        assert_eq!(
            parse_pc_bound("NaN"),
            None,
            "non-finite input is rejected, not clamped"
        );
        assert_eq!(parse_pc_bound("inf"), None);
    }

    #[test]
    fn parse_pc_bound_does_not_round_a_legitimate_fraction() {
        assert_eq!(
            parse_pc_bound("0.855"),
            Some(0.855),
            "0.855 is a legitimate typed bound, not something to round away"
        );
    }

    #[test]
    fn pc_range_inverted_only_when_min_exceeds_a_present_max() {
        assert!(pc_range_inverted(0.7, Some(0.3)), "min > max is inverted");
        assert!(
            !pc_range_inverted(0.3, Some(0.7)),
            "an ordinary range is not inverted"
        );
        assert!(
            !pc_range_inverted(0.5, Some(0.5)),
            "equal bounds still match alerts scored exactly at that value"
        );
        assert!(
            !pc_range_inverted(1.0, None),
            "an absent maximum means no upper bound and can never invert the range"
        );
        assert!(
            pc_range_inverted(0.05, Some(0.0)),
            "a maximum stepped down to zero below a non-zero minimum is inverted"
        );
    }

    #[test]
    fn step_pc_clamps_and_rounds_to_two_decimal_places() {
        assert_eq!(step_pc(0.85, 0.05), 0.9);
        assert_eq!(step_pc(0.98, 0.05), 1.0, "clamped at the upper bound");
        assert_eq!(step_pc(0.02, -0.05), 0.0, "clamped at the lower bound");
        assert_eq!(
            step_pc(0.1, 0.05),
            0.15,
            "no floating-point drift such as 0.15000000000000002"
        );
    }

    #[test]
    fn step_bound_steps_an_absent_value_from_empty_means() {
        assert_eq!(
            step_bound(None, 1.0, -0.05),
            0.95,
            "an empty Maximum steps down from 1.0"
        );
        assert_eq!(step_bound(None, 1.0, 0.05), 1.0, "already at the ceiling");
        assert_eq!(
            step_bound(None, 0.0, 0.05),
            0.05,
            "an empty Minimum steps up from 0.0"
        );
        assert_eq!(
            step_bound(Some(0.85), 1.0, 0.05),
            0.9,
            "a present value ignores empty_means"
        );
    }

    fn option(value: &str, label: &str, count: usize) -> AlertFacetOptionView {
        AlertFacetOptionView {
            value: value.to_string(),
            label: label.to_string(),
            count,
        }
    }

    #[test]
    fn no_ward_code_footnote_renders_at_zero_too() {
        assert_eq!(
            no_ward_code_footnote(0),
            "0 alerts carry no ward code and are never matched by a ward filter."
        );
        assert_eq!(
            no_ward_code_footnote(138),
            "138 alerts carry no ward code and are never matched by a ward filter."
        );
    }

    #[test]
    fn facet_options_round_trip_through_the_dto_fixture() {
        // Non-vacuity for the fixture helper itself, used across this module's own tests.
        let o = option("AddressCluster", "Address Cluster", 1807);
        assert_eq!(o.value, "AddressCluster");
        assert_eq!(o.count, 1807);
    }
}
