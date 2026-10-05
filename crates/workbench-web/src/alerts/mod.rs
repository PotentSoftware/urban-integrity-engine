//! The alert / motif triage inbox (Phase 46, TRI-01/TRI-02) — `AlertsPage`, mounted at `/alerts`.
//!
//! This is the first fully-reachable screen this phase ships: it assembles 46-08's `AlertRow` and
//! `AlertFiltersRail`, 46-09's own `WithdrawForm` (`super::withdraw`), and wires `GET /api/alerts`
//! end to end. See each submodule's own doc comment for its part of the contract.
//!
//! # The inbox opens on everything (D-11)
//!
//! [`build_query_string`] is the one place the `/api/alerts` query is built, and it is written so
//! that every filter signal at its unset default produces a URL with **no** `ward`/`motif`/
//! `min_score`/`max_score`/`from`/`to` query parameter at all — an absent parameter, never one set
//! to a permissive default value, which is what keeps the wire contract itself from smuggling in a
//! hidden floor. `include_withdrawn` is the one exception: it defaults to `false` (D-10) and is
//! sent explicitly only when the analyst has toggled it on.
//!
//! # The noise-floor line is server-measured, never a literal (D-11, A-01)
//!
//! `population_note` arrives from the server on every response and is rendered verbatim through
//! [`noise_floor_line_text`] — no field in that sentence is a compile-time constant. See
//! phase 46 assumption A-01 for why the line names a measured minimum-participant floor rather
//! than a hardcoded "two-participant" figure.
//!
//! # Paging: reset vs. append, never confused (D-12)
//!
//! [`next_request_start`] is the one function that decides the next request's `start` offset —
//! `0` whenever a filter or the sort changes, `alerts.len()` (the count already loaded) when
//! **Show more** is clicked. The component below calls it rather than re-deriving the same
//! decision inline, so the reset behaviour this plan's acceptance criteria asks for is provable
//! without an HTTP round trip.

pub mod filters;
pub mod row;
pub mod withdraw;

use leptos::prelude::*;
use leptos_router::components::A;
use urban_integrity_workbench_dto::{
    AlertFacetsView, AlertPageView, AlertSummaryView, PopulationNoiseFloorView, WithdrawOutcomeView,
};

use crate::alerts::filters::AlertFiltersRail;
use crate::alerts::row::AlertRow;
use crate::profile::pivot::encode_segment;

/// `GET /api/alerts`.
pub const ALERTS_URL: &str = "/api/alerts";
/// The client-side route this page mounts at.
pub const ALERTS_PATH: &str = "/alerts";
/// The shell header nav entry's label (the phase 46 UI spec Copywriting Contract, "Header nav entry").
pub const ALERTS_NAV_LABEL: &str = "Alerts";

/// This crate's established fetch-failure shape (`analysis_page::ANALYSIS_FETCH_ERROR_COPY`,
/// `investigation::INVESTIGATION_FETCH_ERROR`), the endpoint named because an analyst reporting
/// this is reporting a specific route.
pub const ALERTS_FETCH_ERROR: &str =
    "The workbench returned data this page cannot read (/api/alerts).";

pub const ALERTS_LOADING: &str = "Loading alerts\u{2026}";
pub const ALERTS_PAGE_SUBTITLE: &str = "Motif alerts raised by the detectors in this analysis.";
pub const RETRY: &str = "Retry";

pub const EMPTY_NO_ALERTS: &str = "No motif alerts exist in this analysis.";
pub const EMPTY_NO_RUN_ADDENDUM: &str =
    "No detector run is recorded for this analysis, so detection may never have run here.";
pub const EMPTY_FILTERED: &str = "No alert matches these filters.";
pub const CLEAR_FILTERS: &str = "Clear filters";
pub const FILTER_TO_THESE: &str = "Filter to these";
pub const SORT_LABEL: &str = "Sort:";
pub const SORT_SCORE: &str = "Highest P_c";
pub const SORT_RECENCY: &str = "Most recent";
pub const SHOW_MORE: &str = "Show more";
pub const SHOW_THEM: &str = "Show them";
pub const HIDE_THEM: &str = "Hide them";
pub const NO_WITHDRAWN_MATCH: &str = "No withdrawn alerts match these filters.";
pub const OPEN_LEDGER: &str = "Open the Analysis ledger \u{2192}";
pub const DISTINCTION_LINE: &str = "These are detector motif alerts. The Analysis page has the \
                                     22-probe audit battery — its verdicts, coverage, and the \
                                     probes it skipped.";

/// One page's worth of query parameters (D-11/D-12/D-13). Plain owned values, never `RwSignal`s,
/// so [`build_query_string`] is testable with no component mounted.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertListParams {
    pub ward: Option<String>,
    pub motif: Option<String>,
    pub min_score: f64,
    pub max_score: Option<f64>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub sort_recency: bool,
    pub include_withdrawn: bool,
    pub start: usize,
}

impl Default for AlertListParams {
    fn default() -> Self {
        Self {
            ward: None,
            motif: None,
            min_score: 0.0,
            max_score: None,
            from: None,
            to: None,
            sort_recency: false,
            include_withdrawn: false,
            start: 0,
        }
    }
}

/// The server's own list page size default (`workbench::alerts::DEFAULT_ALERT_LIST_LIMIT`) —
/// independently declared here since this wasm-only crate cannot depend on that native crate.
pub const ALERT_LIST_PAGE_LIMIT: usize = 50;

/// Builds the `/api/alerts` query string. D-11: a filter at its unset default is OMITTED, never
/// sent as an explicit permissive value — `ward`/`motif`/`min_score`/`max_score`/`from`/`to` are
/// all absent when [`AlertListParams::default`]'s values are passed through unchanged.
///
/// PROVE THIS CAN FAIL: change the `min_score` branch to `parts.push(format!("min_score={}",
/// params.min_score))` unconditionally — the default (0.0) then rides along on every request as
/// an explicit, if harmless-looking, floor, and `build_query_string_omits_every_filter_at_its_unset_default`
/// fails.
pub fn build_query_string(params: &AlertListParams) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ward) = params.ward.as_deref().filter(|s| !s.is_empty()) {
        parts.push(format!("ward={}", encode_segment(ward)));
    }
    if let Some(motif) = params.motif.as_deref().filter(|s| !s.is_empty()) {
        parts.push(format!("motif={}", encode_segment(motif)));
    }
    if params.min_score > 0.0 {
        parts.push(format!("min_score={}", params.min_score));
    }
    if let Some(max_score) = params.max_score {
        parts.push(format!("max_score={max_score}"));
    }
    if let Some(from) = params.from.as_deref().filter(|s| !s.is_empty()) {
        parts.push(format!("from={}", encode_segment(from)));
    }
    if let Some(to) = params.to.as_deref().filter(|s| !s.is_empty()) {
        parts.push(format!("to={}", encode_segment(to)));
    }
    if params.sort_recency {
        parts.push("sort=recency".to_string());
    }
    if params.start > 0 {
        parts.push(format!("start={}", params.start));
    }
    parts.push(format!("limit={ALERT_LIST_PAGE_LIMIT}"));
    if params.include_withdrawn {
        parts.push("include_withdrawn=true".to_string());
    }
    format!("{ALERTS_URL}?{}", parts.join("&"))
}

/// `GET /api/alerts?{qs}`, in this crate's established typed-failure shape.
async fn fetch_alerts(qs: &str) -> Result<AlertPageView, String> {
    let response = gloo_net::http::Request::get(qs)
        .send()
        .await
        .map_err(|_| ALERTS_FETCH_ERROR.to_string())?;
    if !response.ok() {
        return Err(ALERTS_FETCH_ERROR.to_string());
    }
    response
        .json()
        .await
        .map_err(|_| ALERTS_FETCH_ERROR.to_string())
}

/// Why the page is about to re-fetch — the ONE thing [`next_request_start`] needs to decide the
/// next request's offset (D-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFetchTrigger {
    /// Any filter field or the sort control changed. Always resets to the first page.
    FilterOrSortChanged,
    /// **Show more** was clicked. Appends the next page after whatever is already loaded.
    ShowMore,
}

/// The next request's `start` offset (D-12's "changing a filter or the sort resets to the first
/// page"). `loaded_count` is how many rows are already in the list.
///
/// PROVE THIS CAN FAIL: return `loaded_count` unconditionally — the reset assertion in
/// `next_request_start_resets_on_filter_change_and_advances_on_show_more` fails, and a filter
/// change would leave a stale "Showing 150 of 12" on screen (the exact defect D-12 rules against).
pub fn next_request_start(loaded_count: usize, trigger: ListFetchTrigger) -> usize {
    match trigger {
        ListFetchTrigger::FilterOrSortChanged => 0,
        ListFetchTrigger::ShowMore => loaded_count,
    }
}

/// Whether any of the six D-11 filter dimensions is active — the line between "population
/// genuinely empty" and "filtered to nothing" (the phase 46 UI spec's empty-state table).
/// `include_withdrawn` is deliberately excluded: it is a visibility toggle, not one of D-11's
/// filters, and its own three-state copy already covers the all-withdrawn case separately.
pub fn any_filter_active(
    ward: &Option<String>,
    motif: &Option<String>,
    min_score: f64,
    max_score: Option<f64>,
    from: &Option<String>,
    to: &Option<String>,
) -> bool {
    ward.is_some()
        || motif.is_some()
        || min_score > 0.0
        || max_score.is_some()
        || from.is_some()
        || to.is_some()
}

/// D-11's population/noise-floor sentence, every figure server-measured (the phase 46 assumptions notes
/// A-01) — no field here is a compile-time literal.
pub fn noise_floor_line_text(note: &PopulationNoiseFloorView) -> String {
    format!(
        "{} alerts. {} ({:.1}%) are {} alerts sitting exactly at that detector's minimum \
         participant count ({}).",
        note.total,
        note.floor_count,
        note.floor_percent,
        note.floor_detector_display_name,
        note.floor_participant_count
    )
}

/// The withdrawn-toggle's one of three UI-SPEC states — rendered unconditionally, in all three
/// cases, never conditionally removed at `hidden_count == 0` (D-10's "hidden-with-no-count is the
/// silent-truncation pattern this project rules against, one step further on").
pub fn withdrawn_toggle_text(include_withdrawn: bool, hidden_count: usize) -> String {
    if include_withdrawn {
        format!("Showing {hidden_count} withdrawn alerts.")
    } else if hidden_count > 0 {
        format!("{hidden_count} withdrawn alerts hidden.")
    } else {
        NO_WITHDRAWN_MATCH.to_string()
    }
}

/// "All {n} alerts matching these filters are withdrawn." (empty-state table).
pub fn empty_all_withdrawn_text(n: usize) -> String {
    format!("All {n} alerts matching these filters are withdrawn.")
}

/// Drops the row a just-confirmed withdrawal applies to, returning whether one was actually
/// removed so the caller can keep `total_matched` in step with the visible list.
///
/// Removal is BY ID and nothing else. The obvious-looking alternative —
/// `list.retain(|a| a.withdrawn.is_none())` — is a permanent no-op here and shipped as one: while
/// `include_withdrawn` is false the server has already excluded every withdrawn alert, so each
/// row in hand reads `withdrawn: None`, the predicate is true for all of them, and nothing is
/// ever removed. The client never learns the new `withdrawn` value either, because
/// [`WithdrawOutcomeView::Withdrawn`] carries only a `hidden_count`. Re-deriving the removal from
/// row state rather than from the id will silently reintroduce that bug.
///
/// PROVE THIS CAN FAIL: swap the body for `list.retain(|a| a.withdrawn.is_none()); false` and
/// `drop_withdrawn_row_removes_by_id_not_by_withdrawn_field` fails.
pub fn drop_withdrawn_row(list: &mut Vec<AlertSummaryView>, withdrawn_id: &str) -> bool {
    let before = list.len();
    list.retain(|a| a.id != withdrawn_id);
    list.len() != before
}

/// The page's four terminal states once a fetch has resolved at least once.
#[derive(Debug, Clone, PartialEq)]
enum LoadState {
    Loading,
    Loaded,
    Failed(String),
}

/// The `/alerts` triage inbox (the phase 46 UI spec "`/alerts` page layout"). Opens unfiltered (D-11),
/// re-fetches on every filter/sort/withdrawn-toggle change (resetting to the first page), and
/// appends on **Show more** — never replacing, never reloading (the phase 46 UI spec "Sorting and
/// paging").
#[component]
#[allow(non_snake_case)]
pub fn AlertsPage() -> impl IntoView {
    let ward: RwSignal<Option<String>> = RwSignal::new(None);
    let motif: RwSignal<Option<String>> = RwSignal::new(None);
    let min_score: RwSignal<f64> = RwSignal::new(0.0);
    let max_score: RwSignal<Option<f64>> = RwSignal::new(None);
    let from: RwSignal<Option<String>> = RwSignal::new(None);
    let to: RwSignal<Option<String>> = RwSignal::new(None);
    let sort_recency: RwSignal<bool> = RwSignal::new(false);
    let include_withdrawn: RwSignal<bool> = RwSignal::new(false);

    let alerts: RwSignal<Vec<AlertSummaryView>> = RwSignal::new(Vec::new());
    let total_matched: RwSignal<usize> = RwSignal::new(0);
    // How many alerts match the CURRENT filters and are withdrawn — updated only from a fetch
    // made with `include_withdrawn = false` (the server always reports 0 otherwise, since nothing
    // is hidden when the toggle is already showing them). See `withdrawn_toggle_text`'s doc.
    let hidden_count: RwSignal<usize> = RwSignal::new(0);
    let facets: RwSignal<Option<AlertFacetsView>> = RwSignal::new(None);
    let population_note: RwSignal<Option<PopulationNoiseFloorView>> = RwSignal::new(None);
    let detector_run_recorded: RwSignal<bool> = RwSignal::new(true);
    let load_state: RwSignal<LoadState> = RwSignal::new(LoadState::Loading);

    let do_fetch = move |trigger: ListFetchTrigger| {
        let loaded = alerts.with_untracked(|a| a.len());
        let start = next_request_start(loaded, trigger);
        let reset = start == 0 && trigger == ListFetchTrigger::FilterOrSortChanged;
        let request_include_withdrawn = include_withdrawn.get_untracked();
        let params = AlertListParams {
            ward: ward.get_untracked(),
            motif: motif.get_untracked(),
            min_score: min_score.get_untracked(),
            max_score: max_score.get_untracked(),
            from: from.get_untracked(),
            to: to.get_untracked(),
            sort_recency: sort_recency.get_untracked(),
            include_withdrawn: request_include_withdrawn,
            start,
        };
        let qs = build_query_string(&params);
        load_state.set(LoadState::Loading);
        leptos::task::spawn_local(async move {
            match fetch_alerts(&qs).await {
                Ok(page) => {
                    if reset {
                        alerts.set(page.alerts.clone());
                    } else {
                        alerts.update(|list| list.extend(page.alerts.clone()));
                    }
                    total_matched.set(page.total_matched);
                    if !request_include_withdrawn {
                        hidden_count.set(page.withdrawn_hidden_count);
                    }
                    // Only notify `facets` when the value actually changed (46-12): `RwSignal::set`
                    // notifies unconditionally in Leptos 0.8, and every keystroke in a P_c field
                    // triggers a refetch whose Ok arm lands here. The rail is now a static mount
                    // (see the view below), so this is a secondary defence that cuts pointless
                    // Detector/Ward checklist rebuilds rather than the fix for the lost-focus bug.
                    if facets.with_untracked(|f| f.as_ref() != Some(&page.facets)) {
                        facets.set(Some(page.facets.clone()));
                    }
                    population_note.set(Some(page.population_note.clone()));
                    detector_run_recorded.set(page.detector_run_recorded);
                    load_state.set(LoadState::Loaded);
                }
                Err(msg) => load_state.set(LoadState::Failed(msg)),
            }
        });
    };

    // Re-fetches on mount and on every filter/sort/withdrawn-toggle change, always resetting to
    // the first page (D-12). `start`/`alerts` are deliberately NOT tracked here — a fetch this
    // effect itself triggers must not re-trigger itself.
    Effect::new(move |_| {
        let _ = (
            ward.get(),
            motif.get(),
            min_score.get(),
            max_score.get(),
            from.get(),
            to.get(),
            sort_recency.get(),
            include_withdrawn.get(),
        );
        do_fetch(ListFetchTrigger::FilterOrSortChanged);
    });

    let on_show_more = move |_| do_fetch(ListFetchTrigger::ShowMore);
    let on_retry = move |_| do_fetch(ListFetchTrigger::FilterOrSortChanged);
    let on_clear = move || {
        ward.set(None);
        motif.set(None);
        min_score.set(0.0);
        max_score.set(None);
        from.set(None);
        to.set(None);
        sort_recency.set(false);
    };
    let on_filter_to_floor = move |_| {
        // D-11: "Filter to these" is a FILTER shortcut, not a new query param — `AlertListQuery`
        // (46-06) has no participant-count field, so setting `motif` alone satisfies the UI-SPEC's
        // own text (the floor IS "this detector type" for `SingleSubject` kinds, and already the
        // plurality of `AddressCluster` rows at its measured floor for `Structural` kinds).
        if let Some(note) = population_note.get_untracked() {
            motif.set(Some(note.floor_motif_type));
        }
    };

    let on_withdraw_outcome = Callback::new(
        move |(withdrawn_id, outcome): (String, WithdrawOutcomeView)| match outcome {
            WithdrawOutcomeView::Withdrawn {
                hidden_count: new_hidden_count,
            } => {
                hidden_count.set(new_hidden_count);
                if !include_withdrawn.get_untracked() {
                    // Decrement ONLY if a row actually went, so "Showing n of total" can never
                    // drift from the list the user is looking at.
                    let mut removed = false;
                    alerts.update(|list| removed = drop_withdrawn_row(list, &withdrawn_id));
                    if removed {
                        total_matched.update(|n| *n = n.saturating_sub(1));
                    }
                } else {
                    do_fetch(ListFetchTrigger::FilterOrSortChanged);
                }
            }
            WithdrawOutcomeView::AlreadyWithdrawn { .. } | WithdrawOutcomeView::NotFound => {
                // No row removal, no count change (D-08: a rule held, not a write). `WithdrawForm`
                // already renders the refusal text in place for `AlreadyWithdrawn`.
            }
        },
    );

    view! {
        <div>
            <crate::breadcrumb::Breadcrumb />
            <div style="display: flex; gap: var(--space-lg); align-items: flex-start; margin-top: var(--space-lg);">
                <AlertFiltersRail
                    facets=facets
                    ward_code=ward
                    motif=motif
                    min_score=min_score
                    max_score=max_score
                    from=from
                    to=to
                    on_clear=on_clear
                />
                <div style="flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; gap: var(--space-md);">
                    <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                        <h1 class="text-display" style="margin: 0;">{ALERTS_NAV_LABEL}</h1>
                        <p class="text-body" style="margin: 0; color: var(--muted-foreground);">{ALERTS_PAGE_SUBTITLE}</p>
                    </div>

                    {move || match load_state.get() {
                        LoadState::Loading if alerts.with(|a| a.is_empty()) => view! {
                            <p class="text-body" style="margin: 0; color: var(--muted-foreground);">{ALERTS_LOADING}</p>
                        }.into_any(),
                        LoadState::Failed(msg) => view! {
                            <div style="display: flex; align-items: center; gap: var(--space-md);">
                                <p class="text-body" style="margin: 0; color: var(--muted-foreground);">{msg}</p>
                                <button type="button" class="text-body control-button" on:click=on_retry
                                    style="padding: 0 var(--space-md);">
                                    {RETRY}
                                </button>
                            </div>
                        }.into_any(),
                        _ => {
                            let n = alerts.get().len();
                            let total = total_matched.get();
                            if total == 0 {
                                let filtered = any_filter_active(
                                    &ward.get(), &motif.get(), min_score.get(), max_score.get(),
                                    &from.get(), &to.get(),
                                );
                                let hidden = hidden_count.get();
                                if hidden > 0 && !include_withdrawn.get() {
                                    view! {
                                        <div style="display: flex; align-items: center; gap: var(--space-md);">
                                            <p class="text-body" style="margin: 0;">{empty_all_withdrawn_text(hidden)}</p>
                                            <button type="button" class="text-body control-button" on:click=move |_| include_withdrawn.set(true)
                                                style="padding: 0 var(--space-md);">
                                                {SHOW_THEM}
                                            </button>
                                        </div>
                                    }.into_any()
                                } else if filtered {
                                    view! {
                                        <div style="display: flex; align-items: center; gap: var(--space-md);">
                                            <p class="text-body" style="margin: 0;">{EMPTY_FILTERED}</p>
                                            <button type="button" class="text-body control-button" on:click=move |_| on_clear()
                                                style="padding: 0 var(--space-md);">
                                                {CLEAR_FILTERS}
                                            </button>
                                        </div>
                                    }.into_any()
                                } else {
                                    view! {
                                        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                            <p class="text-body" style="margin: 0;">{EMPTY_NO_ALERTS}</p>
                                            {(!detector_run_recorded.get()).then(|| view! {
                                                <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                                                    {EMPTY_NO_RUN_ADDENDUM}
                                                </p>
                                            })}
                                        </div>
                                    }.into_any()
                                }
                            } else {
                                view! {
                                    <>
                                        {move || population_note.get().map(|note| view! {
                                            <div style="display: flex; align-items: center; gap: var(--space-sm); flex-wrap: wrap;">
                                                <p class="text-body" style="margin: 0;">{noise_floor_line_text(&note)}</p>
                                                <button type="button" class="text-label" on:click=on_filter_to_floor
                                                    style="min-height: 44px; background: transparent; border: none; \
                                                           color: var(--primary); cursor: pointer; padding: 0;">
                                                    {FILTER_TO_THESE}
                                                </button>
                                            </div>
                                        })}
                                        <div style="display: flex; align-items: center; gap: var(--space-xs); flex-wrap: wrap;">
                                            <p class="text-body" style="margin: 0;">{DISTINCTION_LINE}</p>
                                            <A href="/analysis" attr:class="text-label" attr:style="color: var(--primary);">
                                                {OPEN_LEDGER}
                                            </A>
                                        </div>
                                        <div style="display: flex; align-items: center; justify-content: space-between; gap: var(--space-md); flex-wrap: wrap;">
                                            <div style="display: flex; align-items: center; gap: var(--space-sm);">
                                                <span class="text-label">{SORT_LABEL}</span>
                                                <div class="segmented" role="group" aria-label="Sort alerts">
                                                    <button type="button" class="text-label control-button"
                                                        aria-pressed=move || (!sort_recency.get()).to_string()
                                                        on:click=move |_| sort_recency.set(false)>
                                                        {SORT_SCORE}
                                                    </button>
                                                    <button type="button" class="text-label control-button"
                                                        aria-pressed=move || sort_recency.get().to_string()
                                                        on:click=move |_| sort_recency.set(true)>
                                                        {SORT_RECENCY}
                                                    </button>
                                                </div>
                                            </div>
                                            <div style="display: flex; align-items: center; gap: var(--space-sm);">
                                                <span class="text-label" style="color: var(--muted-foreground);">
                                                    {move || withdrawn_toggle_text(include_withdrawn.get(), hidden_count.get())}
                                                </span>
                                                {move || {
                                                    let showing = include_withdrawn.get();
                                                    let hidden = hidden_count.get();
                                                    if showing {
                                                        Some(view! {
                                                            <button type="button" class="text-label control-button" on:click=move |_| include_withdrawn.set(false)>
                                                                {HIDE_THEM}
                                                            </button>
                                                        }.into_any())
                                                    } else if hidden > 0 {
                                                        Some(view! {
                                                            <button type="button" class="text-label control-button" on:click=move |_| include_withdrawn.set(true)>
                                                                {SHOW_THEM}
                                                            </button>
                                                        }.into_any())
                                                    } else {
                                                        None
                                                    }
                                                }}
                                            </div>
                                        </div>
                                        <p class="text-label" style="margin: 0;">{format!("Showing {n} of {total}.")}</p>
                                        <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                                            <For
                                                each=move || alerts.get()
                                                key=|a| a.id.clone()
                                                children=move |alert: AlertSummaryView| {
                                                    let withdraw_open = RwSignal::new(false);
                                                    view! {
                                                        <AlertRow
                                                            alert=alert
                                                            withdraw_open=withdraw_open
                                                            on_withdraw_outcome=on_withdraw_outcome
                                                        />
                                                    }
                                                }
                                            />
                                        </div>
                                        {(n < total).then(|| view! {
                                            <button type="button" class="text-body control-button" on:click=on_show_more
                                                style="align-self: flex-start; padding: 0 var(--space-md); margin-top: var(--space-2xl);">
                                                {SHOW_MORE}
                                            </button>
                                        })}
                                    </>
                                }.into_any()
                            }
                        }
                    }}
                </div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::{
        ImdStateView, InducedSubgraphView, RunAttributionView, ScoreBandView,
    };

    /// A row carrying `withdrawn: None` — the shape every row in hand actually has while
    /// `include_withdrawn` is false, which is exactly what made the old field-based predicate
    /// vacuous.
    fn row(id: &str) -> AlertSummaryView {
        AlertSummaryView {
            id: id.to_string(),
            motif_type: "AddressCluster".to_string(),
            detector_display_name: "Address cluster".to_string(),
            score: 0.8,
            score_band: ScoreBandView::High,
            imd: ImdStateView::NoIndexLoaded { m_env: 1.0 },
            ward_code: None,
            detected_at: "2026-09-21T00:00:00Z".to_string(),
            withdrawn: None,
            run: RunAttributionView {
                first_seen_run_id: None,
                first_seen_at: None,
                last_seen_run_id: None,
                last_seen_at: None,
                runs_since_last_seen: None,
            },
            participant_total: 2,
            participant_chips: vec![],
            path: InducedSubgraphView::NoEdges {
                participant_total: 2,
                snapshot_generated_at: "2026-09-21T00:00:00Z".to_string(),
            },
        }
    }

    /// CR-01 (phase 46 code review): the withdrawn row must actually leave the list. The shipped
    /// `retain(|a| a.withdrawn.is_none())` removed nothing, because every row the server hands
    /// back while `include_withdrawn` is false already reads `withdrawn: None` — so the withdrawn
    /// alert stayed on screen with a live Withdraw button while `total_matched` decremented
    /// underneath it.
    #[test]
    fn drop_withdrawn_row_removes_by_id_not_by_withdrawn_field() {
        let mut list = vec![row("alert:a"), row("alert:b"), row("alert:c")];

        assert!(
            list.iter().all(|a| a.withdrawn.is_none()),
            "precondition: the old field-based predicate is vacuous over this list — every row \
             reads withdrawn: None, so retain(withdrawn.is_none()) would keep all three"
        );

        assert!(drop_withdrawn_row(&mut list, "alert:b"));
        assert_eq!(
            list.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["alert:a", "alert:c"],
            "only the withdrawn id is removed, and the surrounding order is preserved"
        );
    }

    /// The count must not drift when nothing was removed — an outcome naming a row this page is
    /// not showing (a stale tab, a second window) must leave `total_matched` alone.
    #[test]
    fn drop_withdrawn_row_reports_false_when_the_id_is_absent() {
        let mut list = vec![row("alert:a"), row("alert:b")];
        assert!(!drop_withdrawn_row(&mut list, "alert:missing"));
        assert_eq!(list.len(), 2, "an unmatched id removes nothing");
    }

    #[test]
    fn build_query_string_omits_every_filter_at_its_unset_default() {
        let params = AlertListParams::default();
        let qs = build_query_string(&params);
        for forbidden in [
            "ward=",
            "motif=",
            "min_score=",
            "max_score=",
            "from=",
            "to=",
        ] {
            assert!(
                !qs.contains(forbidden),
                "{qs} must not carry {forbidden} at defaults"
            );
        }
        // Non-vacuity: the limit is always present, and include_withdrawn is absent at its
        // false default — so the assertions above are not just "the string is empty".
        assert!(qs.contains("limit=50"));
        assert!(!qs.contains("include_withdrawn"));
    }

    #[test]
    fn build_query_string_includes_every_set_filter() {
        let params = AlertListParams {
            ward: Some("X05000001".to_string()),
            motif: Some("AddressCluster".to_string()),
            min_score: 0.7,
            max_score: Some(0.95),
            from: Some("2026-01-01".to_string()),
            to: Some("2026-06-30".to_string()),
            sort_recency: true,
            include_withdrawn: true,
            start: 50,
        };
        let qs = build_query_string(&params);
        assert!(qs.contains("ward=X05000001"));
        assert!(qs.contains("motif=AddressCluster"));
        assert!(qs.contains("min_score=0.7"));
        assert!(qs.contains("max_score=0.95"));
        assert!(qs.contains("from=2026-01-01"));
        assert!(qs.contains("to=2026-06-30"));
        assert!(qs.contains("sort=recency"));
        assert!(qs.contains("start=50"));
        assert!(qs.contains("include_withdrawn=true"));
    }

    #[test]
    fn next_request_start_resets_on_filter_change_and_advances_on_show_more() {
        assert_eq!(
            next_request_start(37, ListFetchTrigger::FilterOrSortChanged),
            0,
            "D-12: a filter or sort change always resets to the first page"
        );
        assert_eq!(
            next_request_start(0, ListFetchTrigger::FilterOrSortChanged),
            0
        );
        assert_eq!(
            next_request_start(37, ListFetchTrigger::ShowMore),
            37,
            "Show more appends after whatever is already loaded"
        );
    }

    #[test]
    fn any_filter_active_is_false_only_at_every_unset_default() {
        assert!(!any_filter_active(&None, &None, 0.0, None, &None, &None));
        assert!(any_filter_active(
            &Some("X05000001".to_string()),
            &None,
            0.0,
            None,
            &None,
            &None
        ));
        assert!(any_filter_active(
            &None,
            &Some("Phoenix".to_string()),
            0.0,
            None,
            &None,
            &None
        ));
        assert!(any_filter_active(&None, &None, 0.5, None, &None, &None));
        assert!(any_filter_active(
            &None,
            &None,
            0.0,
            Some(0.9),
            &None,
            &None
        ));
        assert!(any_filter_active(
            &None,
            &None,
            0.0,
            None,
            &Some("2026-01-01".to_string()),
            &None
        ));
        assert!(any_filter_active(
            &None,
            &None,
            0.0,
            None,
            &None,
            &Some("2026-06-30".to_string())
        ));
    }

    #[test]
    fn noise_floor_line_text_interpolates_every_field_from_the_server() {
        let note = PopulationNoiseFloorView {
            total: 1947,
            floor_motif_type: "AddressCluster".to_string(),
            floor_detector_display_name: "Address Cluster".to_string(),
            floor_count: 1093,
            floor_percent: 60.5,
            floor_participant_count: 3,
        };
        assert_eq!(
            noise_floor_line_text(&note),
            "1947 alerts. 1093 (60.5%) are Address Cluster alerts sitting exactly at that \
             detector's minimum participant count (3)."
        );
    }

    #[test]
    fn withdrawn_toggle_text_covers_all_three_states() {
        assert_eq!(withdrawn_toggle_text(false, 0), NO_WITHDRAWN_MATCH);
        assert_eq!(
            withdrawn_toggle_text(false, 12),
            "12 withdrawn alerts hidden."
        );
        assert_eq!(
            withdrawn_toggle_text(true, 12),
            "Showing 12 withdrawn alerts."
        );
    }

    #[test]
    fn empty_all_withdrawn_text_names_the_count() {
        assert_eq!(
            empty_all_withdrawn_text(4),
            "All 4 alerts matching these filters are withdrawn."
        );
    }
}
