//! CNV-03's canvas filters — five structured pickers, and the one definition of what is visible.
//!
//! # Structured pickers, and the thing that is not here
//!
//! There is no text input in this module. Two checklists, two `<input type="date">`, three
//! `<select>`s. Nothing typed by an analyst is parsed into a predicate by anything, because the
//! predicate is the shape of [`CanvasFilterRequest`] and every control writes one field of it.
//!
//! That is CNV-03's own parenthesis — "structured pickers (no query DSL)" — read as the product
//! decision it is: the audience is an analyst, not a query author. A free-text box that gets
//! parsed is a query language whatever its author calls it, and the moment one exists the copy
//! beside it has to explain a syntax.
//!
//! The controls **extend plan 44-12's tool rail** rather than starting a second vocabulary on the
//! same page: both checklists render through `tool_rail::checklist_row`, the `P_c` field through
//! `tool_rail::number_field_f64`, the two date bounds through `tool_rail::date_field` — the fourth
//! member of that module's own typed-field family, added there rather than here for exactly this
//! reason.
//!
//! # Where the options come from
//!
//! **Measured, on the server, over whatever is currently in view** — [`FilterFacets`], recomputed
//! on every request. Not a list kept by hand in the browser, because the one thing a hand-kept
//! list cannot do is tell an analyst that an option is empty: eleven edge-type boxes over a
//! neighbourhood that contains two of them are nine clicks that change nothing and two that change
//! everything, with nothing on screen to say which is which.
//!
//! The single exception is the `P_c` threshold's bands, which are the project's own risk
//! thresholds and are enumerated in `workbench-dto::canvas_filter` — see [`P_C_BAND_NOTE`].
//!
//! # The one definition of "visible"
//!
//! [`visible_subgraph_ids`]. The canvas effect reads it and so does the save, and there is exactly
//! one of it — a second definition is precisely how a saved subgraph comes to differ from the
//! screen it was saved from, which is the defect CNV-03 exists to prevent.

use std::collections::HashSet;

use leptos::prelude::*;
use urban_integrity_workbench_dto::canvas_filter::{
    CanvasFilterRequest, CanvasFilterView, FilterFacets, MissingEvidence, P_C_THRESHOLD_BANDS,
};
use urban_integrity_workbench_dto::caps::SAVE_SUBGRAPH_NODE_CAP;
use urban_integrity_workbench_dto::investigation::{
    CreateInvestigationRequest, ExpansionCutView, SaveSubgraphRequest, SaveSubgraphResultView,
    SavedInvestigationView,
};
use urban_integrity_workbench_dto::neighbourhood::NeighbourhoodView;

use crate::neighbourhood;
use crate::tool_rail;

/// `POST /api/graph/canvas-filter` — plan 45-14's filter and facet endpoint.
pub const CANVAS_FILTER_URL: &str = "/api/graph/canvas-filter";

/// Why the `P_c` bands are the one option list not measured from the data.
///
/// They are not a property of this population — they are this project's own risk thresholds
/// (`queries::classify_risk`: `>= 0.9` critical, `>= 0.7` high, `>= 0.5` medium), the same
/// vocabulary the entity profile's alert rows already carry as a severity band. Measuring bands
/// off the data would produce a different set of numbers on every analysis and make two screens
/// disagree about what "high" means.
pub const P_C_BAND_NOTE: &str = "Bands are this project's own risk thresholds, not this graph's.";

pub const FILTER_FETCH_ERROR_COPY: &str =
    "The canvas filter could not be applied. The workbench server may be unavailable.";

/// The caption over the filter block, beneath the tool rail.
pub const FILTERS_CAPTION: &str = "Canvas filters";

const SELECT_STYLE: &str = "min-height: 44px; padding: 0 var(--space-sm); background: var(--card); color: var(--foreground); border: 1px solid var(--border); border-radius: 6px; width: 100%;";

/// Every reactive signal the filter block reads and writes — one `Copy` struct, created by
/// `graph_page.rs`, exactly as `ToolRailSignals` is.
#[derive(Clone, Copy)]
pub struct CanvasFilterSignals {
    pub node_kinds: RwSignal<Vec<String>>,
    pub edge_types: RwSignal<Vec<String>>,
    pub from_date: RwSignal<Option<String>>,
    pub to_date: RwSignal<Option<String>>,
    pub undated_edges: RwSignal<MissingEvidence>,
    pub ward_id: RwSignal<Option<String>>,
    pub min_p_c: RwSignal<Option<f64>>,
    pub unscored_nodes: RwSignal<MissingEvidence>,
    /// The options every picker draws from, as last measured by the server.
    pub facets: RwSignal<Option<FilterFacets>>,
    /// The last answer. `None` before the first request; `Some` with `filter_active: false` when
    /// the pickers are back at "everything".
    pub result: RwSignal<Option<CanvasFilterView>>,
    pub running: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    /// Whether the checklists have been seeded from the first facets answer. The checklists open
    /// fully ticked — a filter must start by removing nothing — and that seeding happens once,
    /// never again, or an analyst's unticking would be undone by the next response.
    pub seeded: RwSignal<bool>,
}

/// Build the request from the current picker state and the canvas's own focus set.
///
/// Pure in everything that matters: it takes values, not signals, so the transcription from
/// control to wire is testable without a DOM.
#[allow(clippy::too_many_arguments)]
pub fn build_request(
    node_kinds: Vec<String>,
    edge_types: Vec<String>,
    from_date: Option<String>,
    to_date: Option<String>,
    undated_edges: MissingEvidence,
    ward_id: Option<String>,
    min_p_c: Option<f64>,
    unscored_nodes: MissingEvidence,
    scope_ids: Vec<String>,
) -> CanvasFilterRequest {
    CanvasFilterRequest {
        node_kinds,
        edge_types,
        from_date,
        to_date,
        undated_edges,
        ward_id,
        min_p_c,
        unscored_nodes,
        scope_ids,
    }
}

/// Read the whole picker state off `state` and compose the request for `scope_ids`.
///
/// **Tracked reads.** Both callers want to be re-run when a picker moves: the request effect,
/// which then issues a new request, and the save payload, which would otherwise go on describing
/// the filter as it was two clicks ago while the canvas showed something else.
pub fn request_from(state: CanvasFilterSignals, scope_ids: Vec<String>) -> CanvasFilterRequest {
    build_request(
        state.node_kinds.get(),
        state.edge_types.get(),
        state.from_date.get(),
        state.to_date.get(),
        state.undated_edges.get(),
        state.ward_id.get(),
        state.min_p_c.get(),
        state.unscored_nodes.get(),
        scope_ids,
    )
}

/// **The canvas's focus set, before any filter runs.**
///
/// 45-13's expansion when there is one, the whole mounted population when there is not — and in
/// either case narrowed to ids the canvas actually has a point for.
///
/// That last clause is not defensive padding. `neighbourhood::visible_ids` comes from
/// `query_entity_neighbourhood`, which walks `graph_events` **plus `identity_links`** (45-08's own
/// decision, taken because the CSR snapshot would silently omit exactly those neighbours). The
/// mounted population is the ward-reachable CSR scope. The two are not the same set, so an
/// expansion can name a node the canvas has no point for. Nothing on screen changes either way —
/// a point that does not exist cannot be shown — but what "visible" *claims* does, and a save that
/// claimed one would be saving something the analyst never saw.
pub fn canvas_focus_ids(
    expansion: Option<&NeighbourhoodView>,
    collapsed: &[String],
    population_ids: &[String],
) -> Vec<String> {
    let mounted: HashSet<&str> = population_ids.iter().map(String::as_str).collect();
    match expansion {
        Some(view) => neighbourhood::visible_ids_with_collapsed(view, collapsed)
            .into_iter()
            .filter(|id| mounted.contains(id.as_str()))
            .collect(),
        None => population_ids.to_vec(),
    }
}

/// What travels to the server as `scope_ids`.
///
/// The expansion's focus set when there is one, and **empty** when there is not — an empty
/// `scope_ids` is the server's own reading of "the whole population", and sending 358,484 ids to
/// say "all of it" would be a payload whose only content is its length.
///
/// One definition, called by both the filter request and the save, because a save whose scope
/// differed from the filter's would be answered with a mismatch the analyst could not act on.
pub fn scope_ids_for(
    expansion: Option<&NeighbourhoodView>,
    collapsed: &[String],
    population_ids: &[String],
) -> Vec<String> {
    match expansion {
        Some(_) => canvas_focus_ids(expansion, collapsed, population_ids),
        None => Vec::new(),
    }
}

/// **The one definition of what is on the canvas**: the focus set, narrowed by an active filter.
///
/// Both the canvas effect and the save read this function. A second definition of "visible" is how
/// a saved subgraph comes to differ from the screen it was saved from, and
/// `saved_is_exactly_what_was_visible` in `crates/workbench-web/tests/canvas_filter_reachable.rs`
/// is the proof that the save has not grown one.
///
/// The intersection is taken **into the focus set**, not out of the server's list: the server's
/// answer can only ever narrow what is on screen, never widen it.
pub fn visible_subgraph_ids(
    expansion: Option<&NeighbourhoodView>,
    collapsed: &[String],
    filter: Option<&CanvasFilterView>,
    population_ids: &[String],
) -> Vec<String> {
    let focus = canvas_focus_ids(expansion, collapsed, population_ids);
    match filter {
        Some(view) if view.filter_active => {
            let passed: HashSet<&str> = view.node_ids.iter().map(String::as_str).collect();
            focus
                .into_iter()
                .filter(|id| passed.contains(id.as_str()))
                .collect()
        }
        _ => focus,
    }
}

/// The filter's own clause under the canvas: what it ran over, what it left, and the cap when the
/// cap bit.
///
/// `None` when no filter is active — a screen nobody filtered states no filter, for 45-12's
/// reason: a permanent notice would be a claim about a control that is not running.
pub fn filter_status_clause(filter: Option<&CanvasFilterView>) -> Option<String> {
    let view = filter?;
    if !view.filter_active {
        return None;
    }
    let kept = view.passed_before_cap;
    let mut line = format!(
        "Filters keep {} of {} {}",
        format_thousands(kept),
        format_thousands(view.candidate_nodes),
        if view.candidate_nodes == 1 {
            "node"
        } else {
            "nodes"
        },
    );
    if view.truncation.truncated {
        let cut = kept.saturating_sub(view.node_ids.len() as u64);
        line.push_str(&format!(
            " \u{2014} {} not shown (filter cap {})",
            format_thousands(cut),
            format_thousands(u64::from(
                urban_integrity_workbench_dto::caps::CANVAS_FILTER_NODE_CAP
            )),
        ));
    }
    line.push('.');
    Some(line)
}

/// `1234` -> `"1,234"`. Same shape as `neighbourhood`'s own, which is private to that module.
fn format_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// One filter request. Same fetch/error shape as every other request in this crate.
pub async fn fetch_canvas_filter(request: CanvasFilterRequest) -> Result<CanvasFilterView, String> {
    let response = gloo_net::http::Request::post(CANVAS_FILTER_URL)
        .json(&request)
        .map_err(|_| FILTER_FETCH_ERROR_COPY.to_string())?
        .send()
        .await
        .map_err(|_| FILTER_FETCH_ERROR_COPY.to_string())?;
    if !response.ok() {
        return Err(FILTER_FETCH_ERROR_COPY.to_string());
    }
    response
        .json::<CanvasFilterView>()
        .await
        .map_err(|_| FILTER_FETCH_ERROR_COPY.to_string())
}

/// Seed both checklists from the first facets answer, ticked.
///
/// Once. A filter opens removing nothing, and a second seeding would silently undo an analyst's
/// unticking every time the server answered.
pub fn seed_checklists(state: CanvasFilterSignals, facets: &FilterFacets) {
    if state.seeded.get_untracked() {
        return;
    }
    state
        .node_kinds
        .set(facets.node_kinds.iter().map(|k| k.value.clone()).collect());
    state
        .edge_types
        .set(facets.edge_types.iter().map(|e| e.value.clone()).collect());
    state.seeded.set(true);
}

/// A checklist label: the option and how many candidates carry it.
pub fn facet_label(value: &str, count: u64) -> String {
    format!("{value} ({})", format_thousands(count))
}

/// The two positions of a missing-evidence control, as a `<select>`'s two options.
fn missing_evidence_select(
    id: &'static str,
    label: &'static str,
    keep_text: &'static str,
    exclude_text: &'static str,
    value: RwSignal<MissingEvidence>,
) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <label class="text-label" for=id>{label}</label>
            <select
                id=id
                style=SELECT_STYLE
                prop:value=move || match value.get() {
                    MissingEvidence::Keep => "keep",
                    MissingEvidence::Exclude => "exclude",
                }
                on:change=move |ev| {
                    value.set(if event_target_value(&ev) == "exclude" {
                        MissingEvidence::Exclude
                    } else {
                        MissingEvidence::Keep
                    });
                }
            >
                <option value="keep">{keep_text}</option>
                <option value="exclude">{exclude_text}</option>
            </select>
        </div>
    }
}

/// CNV-03's five pickers.
#[component]
#[allow(non_snake_case)]
pub fn CanvasFilters(state: CanvasFilterSignals) -> impl IntoView {
    view! {
        <section style="background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-md); display: flex; flex-direction: column; gap: var(--space-md);">
            <h2 class="text-label" style="margin: 0;">{FILTERS_CAPTION}</h2>
            {move || state.error.get().map(|message| view! {
                <p class="text-body" role="alert" style="margin: 0; color: var(--destructive);">{message}</p>
            })}
            {move || match state.facets.get() {
                None => view! {
                    <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                        "Reading what is in view\u{2026}"
                    </p>
                }.into_any(),
                Some(facets) => {
                    let wards = facets.wards.clone();
                    let score_universe = facets.nodes_with_a_score;
                    let dated = facets.edges_with_a_date;
                    let undated = facets.edges_without_a_date;
                    view! {
                        <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                            <div>
                                <span class="text-label">"Node type"</span>
                                {facets.node_kinds.iter().map(|facet| {
                                    let value = facet.value.clone();
                                    let for_checked = value.clone();
                                    let for_click = value.clone();
                                    let checked = Signal::derive(move || {
                                        state.node_kinds.get().iter().any(|k| k == &for_checked)
                                    });
                                    tool_rail::checklist_row(
                                        facet_label(&value, facet.count),
                                        checked,
                                        Signal::derive(|| false),
                                        Signal::derive(|| None),
                                        move |on| toggle(state.node_kinds, &for_click, on),
                                    )
                                }).collect_view()}
                            </div>
                            <div>
                                <span class="text-label">"Edge type"</span>
                                {facets.edge_types.iter().map(|facet| {
                                    let value = facet.value.clone();
                                    let for_checked = value.clone();
                                    let for_click = value.clone();
                                    let checked = Signal::derive(move || {
                                        state.edge_types.get().iter().any(|e| e == &for_checked)
                                    });
                                    tool_rail::checklist_row(
                                        facet_label(&value, facet.count),
                                        checked,
                                        Signal::derive(|| false),
                                        Signal::derive(|| None),
                                        move |on| toggle(state.edge_types, &for_click, on),
                                    )
                                }).collect_view()}
                            </div>
                            {tool_rail::date_field("filter-from-date", "In force from", "Leave empty for no lower bound.", state.from_date)}
                            {tool_rail::date_field("filter-to-date", "In force to", "Leave empty for no upper bound.", state.to_date)}
                            {missing_evidence_select(
                                "filter-undated-edges",
                                "Edges recording no date",
                                "Keep them",
                                "Exclude them",
                                state.undated_edges,
                            )}
                            <span class="text-label" style="color: var(--muted-foreground);">
                                {undated_edges_note(dated, undated)}
                            </span>
                            <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                <label class="text-label" for="filter-ward">"Ward"</label>
                                <select
                                    id="filter-ward"
                                    style=SELECT_STYLE
                                    prop:value=move || state.ward_id.get().unwrap_or_default()
                                    on:change=move |ev| {
                                        let raw = event_target_value(&ev);
                                        state.ward_id.set((!raw.is_empty()).then_some(raw));
                                    }
                                >
                                    <option value="">"Every ward"</option>
                                    {wards.iter().map(|ward| {
                                        let id = ward.entity.id.clone();
                                        let label = facet_label(&ward.entity.name, ward.member_count);
                                        view! { <option value=id>{label}</option> }
                                    }).collect_view()}
                                </select>
                                <span class="text-label" style="color: var(--muted-foreground);">
                                    {ward_note(facets.wards.len())}
                                </span>
                            </div>
                            <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                <label class="text-label" for="filter-min-score">"Risk score"</label>
                                <select
                                    id="filter-min-score"
                                    style=SELECT_STYLE
                                    prop:value=move || state.min_p_c.get().map(|v| v.to_string()).unwrap_or_default()
                                    on:change=move |ev| {
                                        let raw = event_target_value(&ev);
                                        state.min_p_c.set(raw.parse::<f64>().ok());
                                    }
                                >
                                    <option value="">"No threshold"</option>
                                    {P_C_THRESHOLD_BANDS.iter().map(|(label, value)| {
                                        view! { <option value=value.to_string()>{*label}</option> }
                                    }).collect_view()}
                                </select>
                                <span class="text-label" style="color: var(--muted-foreground);">
                                    {P_C_BAND_NOTE}
                                </span>
                            </div>
                            {missing_evidence_select(
                                "filter-unscored-nodes",
                                "Nodes carrying no score",
                                "Keep them",
                                "Exclude them",
                                state.unscored_nodes,
                            )}
                            <span class="text-label" style="color: var(--muted-foreground);">
                                {score_universe_note(score_universe)}
                            </span>
                        </div>
                    }.into_any()
                }
            }}
        </section>
    }
}

fn toggle(signal: RwSignal<Vec<String>>, value: &str, on: bool) {
    let value = value.to_string();
    signal.update(|selected| {
        if on {
            if !selected.iter().any(|v| v == &value) {
                selected.push(value.clone());
            }
        } else {
            selected.retain(|v| v != &value);
        }
    });
}

/// How many edges in view could be tested against a date at all.
///
/// This number is the difference between "the date range found nothing in this window" and "this
/// dataset does not record dates", and on this corpus it is almost always the second: measured
/// 2026-09-15, `effective_to` is null on every `EdgeAdded` event in the store and `effective_from`
/// on three quarters of them. Stated beside the control rather than discovered afterwards.
pub fn undated_edges_note(with_a_date: u64, without: u64) -> String {
    let total = with_a_date + without;
    if total == 0 {
        return "No edge is in view, so there is nothing for a date range to test.".to_string();
    }
    format!(
        "{} of {} edges in view record no date at all.",
        format_thousands(without),
        format_thousands(total)
    )
}

/// What the ward picker can and cannot offer.
pub fn ward_note(ward_count: usize) -> String {
    if ward_count == 0 {
        return "No ward is in view.".to_string();
    }
    format!(
        "{} in view. A ward keeps what it is joined to by a Represents edge; for everything \
         reachable from one, run the ward-reachable subgraph above.",
        if ward_count == 1 {
            "1 ward".to_string()
        } else {
            format!("{ward_count} wards")
        }
    )
}

/// The `P_c` threshold's whole universe, stated beside it.
pub fn score_universe_note(nodes_with_a_score: u64) -> String {
    if nodes_with_a_score == 0 {
        return "No node in view carries a risk score: no live alert names one.".to_string();
    }
    format!(
        "{} of the nodes in view carry a risk score \u{2014} only an entity a live alert names \
         has one.",
        format_thousands(nodes_with_a_score)
    )
}

// ─── CNV-03: an empty canvas explains itself ───────────────────────────────────────────────────

/// **Why the canvas is empty** — `None` when it is not.
///
/// # The distinction this exists for
///
/// "The filters excluded everything" and "this entity has no neighbours of any type" look
/// identical on a blank canvas and are completely different facts. One is a statement about a
/// control the analyst is holding; the other is a finding about the subject. An empty canvas that
/// says nothing lets the first be read as the second, which is the most expensive misreading this
/// screen can produce.
///
/// So the cases are kept apart and each names its own evidence:
///
/// | State | What it says |
/// |---|---|
/// | nothing in view, no filter narrowing | this neighbourhood is empty — nothing was removed |
/// | nothing in view, filters running | this neighbourhood is empty **and** the filters removed nothing from it |
/// | an empty node-type checklist | no node type is ticked, so nothing can pass |
/// | everything removed | which filter removed how many, in order |
///
/// # The date caveat
///
/// A date range that empties this canvas is, on this corpus, far more likely to be a filter
/// artefact than a finding: `effective_to` is null on every `EdgeAdded` event in the store and
/// `effective_from` on three quarters of them (45-06, measured). Excluding the untestable is a
/// position the analyst took on a control, and when it is what emptied the screen the sentence
/// says which — naming the count, so the claim is checkable rather than reassuring.
pub fn empty_canvas_sentence(
    expansion: Option<&NeighbourhoodView>,
    filter: Option<&CanvasFilterView>,
    visible_count: usize,
    picker: &CanvasFilterRequest,
) -> Option<String> {
    if visible_count > 0 {
        return None;
    }
    let narrowing = filter.is_some_and(|f| f.filter_active);

    // Is there anything for a filter to have removed? The filter's own candidate count is the
    // honest source — it is what the server actually ran over — and the expansion is the fallback
    // before the first answer has arrived.
    let candidates = match filter {
        Some(view) => view.candidate_nodes,
        None => expansion.map_or(0, |e| e.nodes.len() as u64 + 1),
    };

    // ── Nothing was in view in the first place ──────────────────────────────────────────────
    if candidates == 0 || expansion.is_some_and(|e| e.total_discovered == 0) {
        let subject = expansion
            .and_then(|e| e.seeds.first())
            .map(|s| s.id.clone());
        return Some(match (subject, narrowing) {
            (Some(id), true) => format!(
                "{id} has no neighbours of any type. The filters removed nothing \u{2014} there \
                 was nothing in view for them to remove."
            ),
            (Some(id), false) => format!("{id} has no neighbours of any type."),
            (None, _) => "Nothing is in view on this canvas.".to_string(),
        });
    }

    if !narrowing {
        // Empty, something was in view, and no filter is running: the canvas and the focus set
        // disagree, which is a fault rather than an answer, and saying so is better than a blank.
        return Some(
            "Nothing is in view, and no filter is narrowing anything \u{2014} the canvas and the \
             expansion disagree about what should be on screen."
                .to_string(),
        );
    }

    if picker.node_kinds.is_empty() {
        return Some(format!(
            "No node type is ticked, so none of the {} nodes in view can pass. Tick at least one \
             to see anything.",
            format_thousands(candidates)
        ));
    }

    let removed = filter.map(|f| f.removed).unwrap_or_default();
    let mut causes: Vec<(u64, String)> = vec![
        (removed.by_node_type, "the node type checklist".to_string()),
        (removed.by_ward, "the ward pick".to_string()),
        (removed.by_p_c, "the risk score threshold".to_string()),
        (removed.by_date_range, "the date range".to_string()),
        (
            removed.by_edge_type,
            if picker.edge_types.is_empty() {
                "the edge type checklist, which has nothing ticked".to_string()
            } else {
                "the edge type checklist".to_string()
            },
        ),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .collect();
    causes.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let attribution = if causes.is_empty() {
        // The counts and the id list disagree. Say so rather than composing a cause out of
        // nothing — a fabricated explanation is worse than a blank canvas.
        "The filters report removing none of them, which they cannot both be doing.".to_string()
    } else {
        let phrases: Vec<String> = causes
            .iter()
            .map(|(n, label)| format!("{} by {label}", format_thousands(*n)))
            .collect();
        format!("{}.", join_with_and(&phrases))
    };

    let mut sentence = format!(
        "Every one of the {} nodes in view was excluded by the filters \u{2014} {attribution} \
         Nothing has been removed from the graph itself.",
        format_thousands(candidates)
    );

    if removed.by_date_range > 0 && picker.undated_edges == MissingEvidence::Exclude {
        if let Some(view) = filter {
            sentence.push(' ');
            sentence.push_str(&format!(
                "{} of the {} edges in view record no date at all, and excluding them is a \
                 position you took on a control: on this dataset an undated edge is the ordinary \
                 case, so that is a property of the data rather than a finding about these nodes.",
                format_thousands(view.facets.edges_without_a_date),
                format_thousands(view.facets.edges_without_a_date + view.facets.edges_with_a_date),
            ));
        }
    }

    Some(sentence)
}

/// `["a", "b", "c"]` -> `"a, b and c"`.
fn join_with_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

// ─── CNV-03: saving the visible subgraph ───────────────────────────────────────────────────────

/// The 45-13 cut the canvas was already carrying when this save was taken, read off the
/// expansion's own response rather than recomputed.
pub fn expansion_cut_from(view: &NeighbourhoodView) -> ExpansionCutView {
    ExpansionCutView {
        // Single-seed callers pass a one-element `seeds`; a finding-seeded (multi-seed) canvas
        // save is a later plan's concern — `ExpansionCutView.seed_id` stays singular here and
        // names the first seed, which is the only one any current caller ever populates.
        seed_id: view.seeds.first().map(|s| s.id.clone()).unwrap_or_default(),
        hops: view.hops,
        node_cap: view.node_cap,
        total_discovered: view.total_discovered,
        shown: view.nodes.len() as u64,
        truncated: view.truncation.truncated,
    }
}

/// **Everything the save sends**, composed from the canvas's own state in one place.
///
/// `expected_node_ids` is [`visible_subgraph_ids`] — the same function the canvas effect applies —
/// and `scope_ids` is [`scope_ids_for`], the same one the filter request used. Neither is
/// re-derived here, and that is the whole design: a save that computed its own idea of what was on
/// screen is the defect CNV-03 exists to prevent, and it is invisible after the fact.
///
/// `picker` arrives with whatever `scope_ids` it was built with and leaves with the canvas's; the
/// server compares the request it is given against the ids it is given, so the two must describe
/// one canvas.
pub fn save_payload(
    expansion: Option<&NeighbourhoodView>,
    collapsed: &[String],
    filter: Option<&CanvasFilterView>,
    population_ids: &[String],
    mut picker: CanvasFilterRequest,
) -> SaveSubgraphRequest {
    picker.scope_ids = scope_ids_for(expansion, collapsed, population_ids);
    SaveSubgraphRequest {
        filter: picker,
        expected_node_ids: visible_subgraph_ids(expansion, collapsed, filter, population_ids),
        expansion_cut: expansion.map(expansion_cut_from),
    }
}

/// What the save button says it will do — **the count, on the control, before it is pressed**.
///
/// A button that said only "Save" would leave the analyst to find out afterwards how much of the
/// screen went in. Above the cap the label says the refusal instead, because a control that looks
/// pressable and then explains itself is worse than one that explains itself first.
pub fn save_button_label(visible: usize) -> String {
    let cap = SAVE_SUBGRAPH_NODE_CAP as usize;
    if visible > cap {
        return format!(
            "Too many to save ({} of a maximum {cap})",
            format_thousands(visible as u64)
        );
    }
    if visible == 1 {
        return "Save 1 visible node".to_string();
    }
    format!("Save {} visible nodes", format_thousands(visible as u64))
}

/// Whether the save control can be pressed at all.
pub fn save_enabled(visible: usize, name: &str, saving: bool) -> bool {
    !saving && !name.trim().is_empty() && visible > 0 && visible <= SAVE_SUBGRAPH_NODE_CAP as usize
}

pub const SAVE_CAPTION: &str = "Save the visible subgraph";
pub const SAVE_NAME_PLACEHOLDER: &str = "Name this investigation\u{2026}";
pub const SAVE_FETCH_ERROR_COPY: &str =
    "The visible subgraph was not saved. The workbench server may be unavailable.";
// The two routes this save calls are declared once, in `investigation`, and imported here — a
// second copy of a path string is a second thing to keep in step with the router (plan 45-15).
pub use crate::investigation::{INVESTIGATIONS_URL, SAVE_SUBGRAPH_URL};

/// Create an investigation and write the visible subgraph into it.
///
/// Two requests, and the second carries **both** the ids the canvas has on screen and the filter
/// that produced them, so the server can re-run the filter and refuse to save anything else. An
/// error body from either is surfaced verbatim: the refusal and the mismatch sentences are written
/// on the server precisely so the analyst reads the real reason rather than a generic failure.
pub async fn save_visible_subgraph(
    name: String,
    payload: SaveSubgraphRequest,
) -> Result<SaveSubgraphResultView, String> {
    let created = gloo_net::http::Request::post(INVESTIGATIONS_URL)
        .json(&CreateInvestigationRequest {
            name,
            notes: String::new(),
        })
        .map_err(|_| SAVE_FETCH_ERROR_COPY.to_string())?
        .send()
        .await
        .map_err(|_| SAVE_FETCH_ERROR_COPY.to_string())?;
    if !created.ok() {
        return Err(server_error_text(created).await);
    }
    let investigation: SavedInvestigationView = created
        .json()
        .await
        .map_err(|_| SAVE_FETCH_ERROR_COPY.to_string())?;

    let url = format!(
        "{SAVE_SUBGRAPH_URL}?id={}",
        crate::profile::pivot::encode_segment(&investigation.id)
    );
    let saved = gloo_net::http::Request::post(&url)
        .json(&payload)
        .map_err(|_| SAVE_FETCH_ERROR_COPY.to_string())?
        .send()
        .await
        .map_err(|_| SAVE_FETCH_ERROR_COPY.to_string())?;
    if !saved.ok() {
        return Err(server_error_text(saved).await);
    }
    saved
        .json::<SaveSubgraphResultView>()
        .await
        .map_err(|_| SAVE_FETCH_ERROR_COPY.to_string())
}

/// The server's own `"error"` text, or this crate's fallback when there is not one.
async fn server_error_text(response: gloo_net::http::Response) -> String {
    match response.json::<serde_json::Value>().await {
        Ok(body) => body["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| SAVE_FETCH_ERROR_COPY.to_string()),
        Err(_) => SAVE_FETCH_ERROR_COPY.to_string(),
    }
}

/// CNV-03's save control.
///
/// One prop carries the whole thing: [`save_payload`]'s output, which is composed in `graph_page.rs`
/// from the canvas's own state. The control therefore has no idea what is visible and no way to
/// form an opinion about it — which is exactly the property that makes "saved == visible" a
/// statement about one function rather than about two that happen to agree.
#[component]
#[allow(non_snake_case)]
pub fn SaveVisibleSubgraph(payload: Signal<SaveSubgraphRequest>) -> impl IntoView {
    let name: RwSignal<String> = RwSignal::new(String::new());
    let saving: RwSignal<bool> = RwSignal::new(false);
    let outcome: RwSignal<Option<SaveSubgraphResultView>> = RwSignal::new(None);
    let failure: RwSignal<Option<String>> = RwSignal::new(None);

    let on_save = move |_| {
        let request = payload.get_untracked();
        let investigation_name = name.get_untracked();
        saving.set(true);
        failure.set(None);
        outcome.set(None);
        leptos::task::spawn_local(async move {
            match save_visible_subgraph(investigation_name, request).await {
                Ok(result) => outcome.set(Some(result)),
                Err(message) => failure.set(Some(message)),
            }
            saving.set(false);
        });
    };

    let visible_count = move || payload.get().expected_node_ids.len();

    view! {
        <section style="background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-md); display: flex; flex-direction: column; gap: var(--space-sm);">
            <h2 class="text-label" style="margin: 0;">{SAVE_CAPTION}</h2>
            <input
                id="save-subgraph-name"
                type="text"
                aria-label=SAVE_CAPTION
                placeholder=SAVE_NAME_PLACEHOLDER
                prop:value=move || name.get()
                on:input=move |ev| name.set(event_target_value(&ev))
                style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); background: var(--background); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); min-height: 44px;"
            />
            <button
                class="text-body"
                style=move || {
                    let base = "border: none; border-radius: 6px; padding: var(--space-xs) var(--space-md); min-height: 44px; background: var(--primary); color: var(--primary-foreground);";
                    if save_enabled(visible_count(), &name.get(), saving.get()) {
                        format!("{base} cursor: pointer;")
                    } else {
                        format!("{base} opacity: 0.4; cursor: default;")
                    }
                }
                disabled=move || !save_enabled(visible_count(), &name.get(), saving.get())
                on:click=on_save
            >
                {move || if saving.get() {
                    "Saving\u{2026}".to_string()
                } else {
                    save_button_label(visible_count())
                }}
            </button>
            {move || failure.get().map(|message| view! {
                <p class="text-body" role="alert" style="margin: 0; color: var(--destructive);">{message}</p>
            })}
            {move || outcome.get().map(|result| view! {
                <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                    {result.provenance_note.clone()}
                </p>
            })}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use urban_integrity_workbench_dto::algorithm::Truncation;
    use urban_integrity_workbench_dto::canvas_filter::{FilterFacets, FilterRemovalCounts};
    use urban_integrity_workbench_dto::neighbourhood::{NeighbourEdgeView, NeighbourhoodNodeView};
    use urban_integrity_workbench_dto::temporal::{EffectiveRange, Temporal};
    use urban_integrity_workbench_dto::EntityRef;

    fn entity(id: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: format!("{id} name"),
            kind: "Contract".to_string(),
        }
    }

    fn neighbour(id: &str) -> NeighbourhoodNodeView {
        NeighbourhoodNodeView {
            entity: entity(id),
            hop: 1,
            edges: vec![NeighbourEdgeView {
                edge_type: "AwardedTo".to_string(),
                temporal: Temporal::unfiltered(EffectiveRange::Unrecorded {
                    basis: "AwardedTo.meta.effective_from / AwardedTo.meta.effective_to"
                        .to_string(),
                }),
            }],
            reached_from: "GB-COH:X6999001".to_string(),
        }
    }

    fn expansion(ids: &[&str]) -> NeighbourhoodView {
        NeighbourhoodView::cut(
            vec![entity("GB-COH:X6999001")],
            1,
            250,
            ids.iter().map(|id| neighbour(id)).collect(),
        )
    }

    fn empty_facets() -> FilterFacets {
        FilterFacets {
            node_kinds: vec![],
            edge_types: vec![],
            wards: vec![],
            nodes_with_a_score: 0,
            highest_score: None,
            edges_with_a_date: 0,
            edges_without_a_date: 0,
        }
    }

    fn answer(active: bool, ids: &[&str], candidates: u64, passed: u64) -> CanvasFilterView {
        CanvasFilterView {
            filter_active: active,
            candidate_nodes: candidates,
            candidate_edges: 0,
            node_ids: ids.iter().map(|i| i.to_string()).collect(),
            edges: vec![],
            removed: FilterRemovalCounts::default(),
            facets: empty_facets(),
            truncation: Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            passed_before_cap: passed,
        }
    }

    /// The focus set is narrowed to nodes the canvas actually has a point for.
    ///
    /// An expansion comes from `query_entity_neighbourhood`, which walks `graph_events` **plus
    /// `identity_links`**; the mounted population is the ward-reachable CSR scope. The two are not
    /// the same set, so an expansion can name a node with no point on screen. Nothing visible
    /// changes — a point that does not exist cannot be shown — but what "visible" *claims* does.
    ///
    /// PROVE THIS CAN FAIL: drop the `mounted.contains` filter from `canvas_focus_ids` and the
    /// off-population id reappears in the focus set.
    #[test]
    fn the_focus_set_never_claims_a_node_the_canvas_has_no_point_for() {
        let view = expansion(&["contract:c1", "identity:only-in-the-read-path"]);
        let population = vec![
            "GB-COH:X6999001".to_string(),
            "contract:c1".to_string(),
            "contract:c2".to_string(),
        ];
        let focus = canvas_focus_ids(Some(&view), &[], &population);
        assert_eq!(focus, vec!["GB-COH:X6999001", "contract:c1"]);
        // Non-vacuity: the expansion really did name a node outside the population, or this test
        // would pass over a fixture where the filter could not bite.
        assert!(neighbourhood::visible_ids(&view)
            .iter()
            .any(|id| !population.contains(id)));
    }

    /// With no expansion the focus set is the whole mounted population, in its own order.
    #[test]
    fn with_no_expansion_the_focus_set_is_the_mounted_population() {
        let population = vec!["b".to_string(), "a".to_string(), "c".to_string()];
        assert_eq!(canvas_focus_ids(None, &[], &population), population);
    }

    /// **A filter can only narrow what is on screen, never widen it.**
    ///
    /// The intersection is taken into the focus set, so an answer naming an id the canvas is not
    /// focused on cannot put it back. Without that, a stale answer from before a collapse — or a
    /// server disagreeing about the scope — would silently add points the analyst had folded away.
    ///
    /// PROVE THIS CAN FAIL: intersect out of `view.node_ids` instead of into the focus set (i.e.
    /// return `view.node_ids` filtered by the focus) and the ORDER assertion still passes but the
    /// widening one fails; return `view.node_ids` wholesale and both fail.
    #[test]
    fn a_filter_narrows_the_focus_set_and_can_never_widen_it() {
        let view = expansion(&["contract:c1", "contract:c2"]);
        let population = vec![
            "GB-COH:X6999001".to_string(),
            "contract:c1".to_string(),
            "contract:c2".to_string(),
            "contract:c3".to_string(),
        ];
        let filtered = answer(
            true,
            // `contract:c3` is in the population but NOT in this expansion's focus set.
            &["contract:c1", "contract:c3"],
            3,
            2,
        );
        let visible = visible_subgraph_ids(Some(&view), &[], Some(&filtered), &population);
        assert_eq!(visible, vec!["contract:c1"]);
        assert!(
            !visible.contains(&"contract:c3".to_string()),
            "a filter answer must never add a point the canvas was not focused on"
        );
    }

    /// An inactive filter leaves the focus set exactly as it was — the same set, in the same order.
    ///
    /// PROVE THIS CAN FAIL: drop the `view.filter_active` guard and the empty `node_ids` of an
    /// inactive answer empties the canvas.
    #[test]
    fn an_inactive_filter_leaves_the_focus_set_untouched() {
        let view = expansion(&["contract:c1", "contract:c2"]);
        let population = vec![
            "GB-COH:X6999001".to_string(),
            "contract:c1".to_string(),
            "contract:c2".to_string(),
        ];
        let inactive = answer(false, &[], 3, 3);
        assert_eq!(
            visible_subgraph_ids(Some(&view), &[], Some(&inactive), &population),
            canvas_focus_ids(Some(&view), &[], &population)
        );
        assert_eq!(
            visible_subgraph_ids(None, &[], Some(&inactive), &population),
            population
        );
    }

    /// A collapsed cluster is folded out of the visible set, and re-expanding restores it exactly.
    ///
    /// PROVE THIS CAN FAIL: call `neighbourhood::visible_ids` instead of
    /// `visible_ids_with_collapsed` in `canvas_focus_ids` and the collapsed arm equals the
    /// expanded one.
    #[test]
    fn a_collapse_is_folded_out_of_the_visible_set_and_a_re_expansion_restores_it() {
        let ids: Vec<String> = (0..30).map(|i| format!("contract:c{i:03}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let view = expansion(&refs);
        let mut population = vec!["GB-COH:X6999001".to_string()];
        population.extend(ids.iter().cloned());

        let expanded = visible_subgraph_ids(Some(&view), &[], None, &population);
        let collapsed = visible_subgraph_ids(
            Some(&view),
            &["GB-COH:X6999001".to_string()],
            None,
            &population,
        );
        assert_eq!(expanded.len(), 31);
        assert_eq!(collapsed, vec!["GB-COH:X6999001"]);
        assert_eq!(
            visible_subgraph_ids(Some(&view), &[], None, &population),
            expanded,
            "re-expanding restores exactly the set that was folded"
        );
    }

    /// The filter's clause states the cap only when the cap bit, and is silent when no filter is
    /// narrowing anything.
    ///
    /// PROVE THIS CAN FAIL: append the cap clause unconditionally and the first assertion fails;
    /// drop the `filter_active` guard and the `None` assertion fails.
    #[test]
    fn the_filter_clause_names_the_cap_only_when_the_cap_bit() {
        let quiet = answer(true, &["a", "b"], 10, 2);
        assert_eq!(
            filter_status_clause(Some(&quiet)).as_deref(),
            Some("Filters keep 2 of 10 nodes.")
        );

        let mut cut = answer(true, &["a"], 4_000, 3_500);
        cut.truncation = Truncation {
            truncated: true,
            reason: Some("canvas_filter_node_cap".to_string()),
            detail: "stopped at canvas filter node cap 2000".to_string(),
        };
        cut.node_ids = (0..2_000).map(|i| format!("n{i}")).collect();
        let clause = filter_status_clause(Some(&cut)).expect("a cut filter must state its cap");
        assert!(clause.contains("1,500 not shown"), "{clause}");
        assert!(clause.contains("filter cap 2,000"), "{clause}");

        assert_eq!(filter_status_clause(None), None);
        assert_eq!(
            filter_status_clause(Some(&answer(false, &[], 10, 10))),
            None
        );
    }

    /// The three notes beside the pickers say different things, and each states its own number.
    ///
    /// PROVE THIS CAN FAIL: make `undated_edges_note` print the total instead of the untestable
    /// count and the first assertion fails.
    #[test]
    fn each_picker_states_the_universe_it_can_actually_test() {
        assert_eq!(
            undated_edges_note(1, 6),
            "6 of 7 edges in view record no date at all."
        );
        assert_eq!(
            undated_edges_note(0, 0),
            "No edge is in view, so there is nothing for a date range to test."
        );
        assert!(ward_note(0).contains("No ward is in view"));
        assert!(ward_note(1).starts_with("1 ward in view"));
        assert!(ward_note(4).starts_with("4 wards in view"));
        assert!(score_universe_note(0).contains("no live alert names one"));
        assert!(score_universe_note(2_500).contains("2,500"));
    }

    /// The save button states the count **before** it is pressed, and above the cap it states the
    /// refusal instead of looking pressable.
    ///
    /// PROVE THIS CAN FAIL: drop the `visible > cap` branch from `save_button_label` and the
    /// over-cap assertion fails; drop the same bound from `save_enabled` and the control becomes
    /// pressable for a save the server will refuse.
    #[test]
    fn the_save_control_says_what_it_will_save_and_refuses_above_the_cap() {
        let cap = SAVE_SUBGRAPH_NODE_CAP as usize;
        assert_eq!(save_button_label(1), "Save 1 visible node");
        assert_eq!(save_button_label(cap), format!("Save {cap} visible nodes"));
        let over = save_button_label(cap + 1);
        assert!(over.starts_with("Too many to save"), "{over}");
        assert!(over.contains(&cap.to_string()));

        assert!(save_enabled(cap, "Ward follow-up", false));
        assert!(!save_enabled(cap + 1, "Ward follow-up", false));
        assert!(!save_enabled(0, "Ward follow-up", false));
        assert!(!save_enabled(10, "   ", false));
        assert!(!save_enabled(10, "Ward follow-up", true));
    }

    /// The cut carried into a save is read off the expansion's own response.
    ///
    /// PROVE THIS CAN FAIL: source `truncated` from `total_discovered > shown` computed here
    /// instead of from the server's own flag and the uncut arm still passes while the gate stops
    /// being the producer's — which is the 45-13 rule this inherits.
    #[test]
    fn the_saved_cut_is_the_expansions_own_report() {
        let fitted = expansion(&["contract:c1", "contract:c2"]);
        let cut = expansion_cut_from(&fitted);
        assert_eq!(cut.seed_id, "GB-COH:X6999001");
        assert_eq!(cut.total_discovered, 2);
        assert_eq!(cut.shown, 2);
        assert!(!cut.truncated);

        let ids: Vec<String> = (0..300).map(|i| format!("contract:c{i:03}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let big = NeighbourhoodView::cut(
            vec![entity("GB-COH:X6999001")],
            2,
            250,
            refs.iter().map(|id| neighbour(id)).collect(),
        );
        let big_cut = expansion_cut_from(&big);
        assert!(big_cut.truncated);
        assert_eq!(big_cut.total_discovered, 300);
        assert_eq!(big_cut.shown, 250);
    }

    /// The scope the filter ran over and the scope the save sends are one definition.
    ///
    /// PROVE THIS CAN FAIL: make `scope_ids_for` return the population when there is no expansion
    /// and the second assertion fails — and the server, reading a non-empty scope, would answer
    /// about a different candidate set than the filter did.
    #[test]
    fn the_filter_and_the_save_send_the_same_scope() {
        let view = expansion(&["contract:c1"]);
        let population = vec!["GB-COH:X6999001".to_string(), "contract:c1".to_string()];
        assert_eq!(
            scope_ids_for(Some(&view), &[], &population),
            canvas_focus_ids(Some(&view), &[], &population)
        );
        assert!(
            scope_ids_for(None, &[], &population).is_empty(),
            "no expansion means the whole population, which the server reads as an empty scope"
        );
    }

    fn kinded(id: &str, kind: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: format!("{id} name"),
            kind: kind.to_string(),
        }
    }

    fn kinded_neighbour(
        id: &str,
        kind: &str,
        hop: u32,
        reached_from: &str,
    ) -> NeighbourhoodNodeView {
        NeighbourhoodNodeView {
            entity: kinded(id, kind),
            hop,
            edges: vec![NeighbourEdgeView {
                edge_type: "AwardedTo".to_string(),
                temporal: Temporal::unfiltered(EffectiveRange::Unrecorded {
                    basis: "AwardedTo.meta.effective_from / AwardedTo.meta.effective_to"
                        .to_string(),
                }),
            }],
            reached_from: reached_from.to_string(),
        }
    }

    const SEED2: &str = "GB-COH:X06999001";
    /// Above `CLUSTER_COLLAPSE_THRESHOLD` (25), so the collapse arm below can actually bite —
    /// 45-13 §7.3's lesson: an assertion made where it cannot bite is a vacuous bound dressed up.
    const CONTRACTS: usize = 26;

    /// The mounted population: the seed, four contracts, two wards, two locations, and one node the
    /// canvas is never focused on.
    fn population() -> Vec<String> {
        let mut ids = vec![SEED2.to_string()];
        for i in 1..=CONTRACTS {
            ids.push(format!("CF:x9999-cf-{i:05}"));
        }
        ids.push("ward:X05999001".to_string());
        ids.push("ward:X05999002".to_string());
        ids.push("LOC:X001AA:one".to_string());
        ids.push("LOC:X001AA:two".to_string());
        ids.push("GB-COH:X06999999".to_string());
        ids
    }

    /// One hop: the seed and its four contracts.
    fn one_hop() -> NeighbourhoodView {
        NeighbourhoodView::cut(
            vec![kinded(SEED2, "Entity")],
            1,
            250,
            (1..=CONTRACTS)
                .map(|i| kinded_neighbour(&format!("CF:x9999-cf-{i:05}"), "Contract", 1, SEED2))
                .collect(),
        )
    }

    /// Two hops: everything one hop reached, plus the two wards and the two locations behind them.
    fn two_hops() -> NeighbourhoodView {
        let mut nodes: Vec<NeighbourhoodNodeView> = (1..=CONTRACTS)
            .map(|i| kinded_neighbour(&format!("CF:x9999-cf-{i:05}"), "Contract", 1, SEED2))
            .collect();
        nodes.push(kinded_neighbour(
            "ward:X05999001",
            "Ward",
            2,
            "CF:x9999-cf-00001",
        ));
        nodes.push(kinded_neighbour(
            "ward:X05999002",
            "Ward",
            2,
            "CF:x9999-cf-00003",
        ));
        nodes.push(kinded_neighbour(
            "LOC:X001AA:one",
            "Location",
            2,
            "CF:x9999-cf-00001",
        ));
        nodes.push(kinded_neighbour(
            "LOC:X001AA:two",
            "Location",
            2,
            "CF:x9999-cf-00002",
        ));
        NeighbourhoodView::cut(vec![kinded(SEED2, "Entity")], 2, 250, nodes)
    }

    fn blank_facets() -> FilterFacets {
        FilterFacets {
            node_kinds: vec![],
            edge_types: vec![],
            wards: vec![],
            nodes_with_a_score: 0,
            highest_score: None,
            edges_with_a_date: 0,
            edges_without_a_date: 0,
        }
    }

    /// A filter answer that passes `ids` — the shape the server returns for an active filter.
    fn passing(ids: &[&str], candidates: u64) -> CanvasFilterView {
        CanvasFilterView {
            filter_active: true,
            candidate_nodes: candidates,
            candidate_edges: 0,
            node_ids: ids.iter().map(|i| i.to_string()).collect(),
            edges: vec![],
            removed: FilterRemovalCounts {
                by_node_type: candidates - ids.len() as u64,
                ..FilterRemovalCounts::default()
            },
            facets: blank_facets(),
            truncation: Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            passed_before_cap: ids.len() as u64,
        }
    }

    fn picker() -> CanvasFilterRequest {
        CanvasFilterRequest {
            node_kinds: vec!["Contract".to_string(), "Ward".to_string()],
            edge_types: vec!["AwardedTo".to_string(), "Represents".to_string()],
            from_date: None,
            to_date: None,
            undated_edges: MissingEvidence::Keep,
            ward_id: None,
            min_p_c: None,
            unscored_nodes: MissingEvidence::Exclude,
            scope_ids: vec![],
        }
    }

    fn set(ids: &[String]) -> BTreeSet<&str> {
        ids.iter().map(String::as_str).collect()
    }

    /// **CNV-03's central claim: what a save writes is exactly what was on the canvas.**
    ///
    /// The test is built so it cannot pass vacuously. "The saved set is non-empty" and "the saved set
    /// is a subset of the population" are both true of a save that quietly re-ran the unfiltered
    /// query, so neither is asserted. What is asserted is set equality against the canvas's own
    /// visible set, under **a filter that genuinely removes nodes and an expansion that genuinely adds
    /// them** — with the counts on both sides taken and compared first, so a fixture where either did
    /// nothing fails before the equality is reached.
    ///
    /// PROVE THIS CAN FAIL: in `save_payload`, replace
    /// `visible_subgraph_ids(expansion, collapsed, filter, population_ids)` with
    /// `population_ids.to_vec()` — the save that re-runs the unfiltered query. Every equality below
    /// fails. The mutation was applied, the suite run, and the mutation reverted.
    #[test]
    fn saved_is_exactly_what_was_visible() {
        let population = population();
        // Every contract and both wards; the seed and the two locations are what the node-type
        // checklist removes.
        let mut filtered: Vec<String> = (1..=CONTRACTS)
            .map(|i| format!("CF:x9999-cf-{i:05}"))
            .collect();
        filtered.push("ward:X05999001".to_string());
        filtered.push("ward:X05999002".to_string());
        let filtered_ids: Vec<&str> = filtered.iter().map(String::as_str).collect();

        // ── The expansion genuinely adds nodes ──────────────────────────────────────────────────
        let near = canvas_focus_ids(Some(&one_hop()), &[], &population);
        let far = canvas_focus_ids(Some(&two_hops()), &[], &population);
        assert_eq!(near.len(), CONTRACTS + 1, "the seed and every contract");
        assert_eq!(far.len(), CONTRACTS + 5, "plus two wards and two locations");
        assert!(
            set(&far).is_superset(&set(&near)) && far.len() > near.len(),
            "non-vacuity: the second expansion must actually add nodes, or nothing below is a test \
             of a filter composing with an expansion"
        );

        // ── The filter genuinely removes nodes ──────────────────────────────────────────────────
        let unfiltered = visible_subgraph_ids(Some(&two_hops()), &[], None, &population);
        let filter = passing(&filtered_ids, far.len() as u64);
        let visible = visible_subgraph_ids(Some(&two_hops()), &[], Some(&filter), &population);
        assert_eq!(unfiltered.len(), CONTRACTS + 5);
        assert_eq!(visible.len(), CONTRACTS + 2);
        assert_ne!(
            set(&unfiltered),
            set(&visible),
            "non-vacuity: the filtered and unfiltered populations must differ, or an equality against \
             either proves nothing about which one was saved"
        );

        // ── Saved == visible ────────────────────────────────────────────────────────────────────
        let payload = save_payload(Some(&two_hops()), &[], Some(&filter), &population, picker());
        assert_eq!(
            set(&payload.expected_node_ids),
            set(&visible),
            "the save must send exactly the visible set"
        );
        assert_eq!(payload.expected_node_ids.len(), CONTRACTS + 2);
        assert_ne!(
            set(&payload.expected_node_ids),
            set(&unfiltered),
            "a save that re-ran the unfiltered query would land here"
        );
        assert_ne!(
            set(&payload.expected_node_ids),
            set(&population),
            "and a save that ignored the expansion too would land here"
        );

        // The scope the save declares is the scope the filter ran over, or the server's own
        // recomputation would answer about a different candidate set and refuse the save.
        assert_eq!(
            set(&payload.filter.scope_ids),
            set(&scope_ids_for(Some(&two_hops()), &[], &population))
        );

        // ── A collapse is part of "visible" too ─────────────────────────────────────────────────
        let collapsed = vec![SEED2.to_string()];
        let folded =
            visible_subgraph_ids(Some(&two_hops()), &collapsed, Some(&filter), &population);
        let folded_payload = save_payload(
            Some(&two_hops()),
            &collapsed,
            Some(&filter),
            &population,
            picker(),
        );
        assert!(
            folded.len() < visible.len(),
            "non-vacuity: the collapse must actually fold something away"
        );
        assert_eq!(set(&folded_payload.expected_node_ids), set(&folded));
    }

    /// The save carries the cut the canvas was already under, and says so when there was none.
    ///
    /// PROVE THIS CAN FAIL: return `None` unconditionally from `save_payload`'s `expansion_cut` and
    /// the first assertion fails — the saved set would then look like an unexpanded one.
    #[test]
    fn a_save_carries_the_cut_the_canvas_was_already_under() {
        let population = population();
        let payload = save_payload(Some(&two_hops()), &[], None, &population, picker());
        let cut = payload.expansion_cut.expect("an expansion was running");
        assert_eq!(cut.seed_id, SEED2);
        assert_eq!(cut.hops, 2);
        assert!(!cut.truncated);
        assert_eq!(cut.total_discovered, CONTRACTS as u64 + 4);

        let unexpanded = save_payload(None, &[], None, &population, picker());
        assert!(
            unexpanded.expansion_cut.is_none(),
            "no expansion was running, and claiming one would misdescribe the set"
        );
    }

    /// **An empty canvas distinguishes "the filters excluded everything" from "there are no
    /// neighbours".**
    ///
    /// The two look identical on a blank rectangle and are completely different facts — one is
    /// about a control the analyst is holding, the other is a finding about the subject. Every arm
    /// below is asserted to be a DIFFERENT sentence from every other, because a single sentence
    /// covering all of them would be the blank canvas with extra words.
    ///
    /// PROVE THIS CAN FAIL: return one constant string from `empty_canvas_sentence` for every
    /// empty case — the distinctness assertion fails. Return `None` unconditionally and the first
    /// four fail. Both mutations were applied and reverted.
    #[test]
    fn an_empty_canvas_says_which_kind_of_empty_it_is() {
        let full = passing(&["CF:x9999-cf-00001"], 30);
        // A canvas with something on it explains nothing: an explanation nobody needs is noise.
        assert_eq!(
            empty_canvas_sentence(Some(&two_hops()), Some(&full), 1, &picker()),
            None
        );

        // 1. No neighbours, no filter running.
        let isolated = NeighbourhoodView::cut(vec![kinded(SEED2, "Entity")], 1, 250, vec![]);
        let no_neighbours = empty_canvas_sentence(Some(&isolated), None, 0, &picker())
            .expect("an empty canvas must explain itself");
        assert!(no_neighbours.contains("has no neighbours of any type"));
        assert!(!no_neighbours.contains("filter"));

        // 2. No neighbours AND filters running — the distinction that matters most. The filters
        //    are not the reason, and the sentence has to say so or the analyst will read a
        //    finding as an artefact.
        let over_nothing = CanvasFilterView {
            candidate_nodes: 0,
            ..passing(&[], 0)
        };
        let both = empty_canvas_sentence(Some(&isolated), Some(&over_nothing), 0, &picker())
            .expect("an empty canvas must explain itself");
        assert!(both.contains("has no neighbours of any type"));
        assert!(
            both.contains("filters removed nothing"),
            "it must say the filters are not the reason: {both}"
        );

        // 3. An empty node-type checklist.
        let mut nothing_ticked = picker();
        nothing_ticked.node_kinds = vec![];
        let unticked = empty_canvas_sentence(
            Some(&two_hops()),
            Some(&passing(&[], 31)),
            0,
            &nothing_ticked,
        )
        .expect("an empty canvas must explain itself");
        assert!(unticked.contains("No node type is ticked"));
        assert!(unticked.contains("31"));

        // 4. The filters removed everything — which one, and how many each.
        let mut removed_all = passing(&[], 31);
        removed_all.removed = FilterRemovalCounts {
            by_node_type: 29,
            by_p_c: 2,
            ..FilterRemovalCounts::default()
        };
        let excluded = empty_canvas_sentence(Some(&two_hops()), Some(&removed_all), 0, &picker())
            .expect("an empty canvas must explain itself");
        assert!(
            excluded.contains("29 by the node type checklist and 2 by the risk score threshold"),
            "the causes must be named largest first: {excluded}"
        );
        assert!(
            excluded.contains("Nothing has been removed from the graph itself"),
            "an excluded node is still in the graph, and the copy must not imply otherwise"
        );

        // Every arm is a different sentence.
        let arms = [&no_neighbours, &both, &unticked, &excluded];
        for (i, a) in arms.iter().enumerate() {
            for b in arms.iter().skip(i + 1) {
                assert_ne!(a, b, "two different facts must not share one sentence");
            }
        }
    }

    /// A date range that empties the canvas says it is probably a filter artefact, and names the
    /// count that makes it one.
    ///
    /// On this corpus `effective_to` is null on every `EdgeAdded` event and `effective_from` on
    /// three quarters of them, so excluding the untestable empties almost any view. That is a
    /// property of the dataset and reading it as a finding about the subject is the expensive
    /// mistake.
    ///
    /// PROVE THIS CAN FAIL: drop the `undated_edges == Exclude` guard and the caveat appears when
    /// the analyst KEPT the undated edges — where it would be false — failing the second
    /// assertion. Drop the whole clause and the first fails.
    #[test]
    fn a_date_range_that_empties_the_canvas_says_it_is_probably_an_artefact() {
        let mut view = passing(&[], 31);
        view.removed = FilterRemovalCounts {
            by_date_range: 31,
            ..FilterRemovalCounts::default()
        };
        view.facets = FilterFacets {
            edges_with_a_date: 1,
            edges_without_a_date: 47,
            ..blank_facets()
        };

        let mut excluding = picker();
        excluding.from_date = Some("2019-01-01".to_string());
        excluding.undated_edges = MissingEvidence::Exclude;
        let artefact = empty_canvas_sentence(Some(&two_hops()), Some(&view), 0, &excluding)
            .expect("an empty canvas must explain itself");
        assert!(artefact.contains("31 by the date range"));
        assert!(
            artefact.contains("47 of the 48 edges in view record no date at all"),
            "the caveat must name the count that makes it a caveat: {artefact}"
        );
        assert!(artefact.contains("property of the data rather than a finding"));

        // The same emptiness with the undated edges KEPT is a real date answer, not an artefact,
        // and must not carry the caveat.
        let mut keeping = excluding.clone();
        keeping.undated_edges = MissingEvidence::Keep;
        let genuine = empty_canvas_sentence(Some(&two_hops()), Some(&view), 0, &keeping)
            .expect("an empty canvas must explain itself");
        assert!(genuine.contains("31 by the date range"));
        assert!(
            !genuine.contains("record no date at all"),
            "the artefact caveat must not appear where it would be false: {genuine}"
        );
        assert_ne!(artefact, genuine);
    }

    /// The label on a checklist box carries the option and its count.
    #[test]
    fn a_checklist_label_carries_its_own_count() {
        assert_eq!(facet_label("AwardedTo", 27_341), "AwardedTo (27,341)");
        assert_eq!(facet_label("Controls", 0), "Controls (0)");
    }
}
