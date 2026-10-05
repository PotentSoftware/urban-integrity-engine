//! `ResultsList` — the Graph page rail's results list (the phase 44 UI spec "Results list (per
//! algorithm)"): one row per algorithm output, per-algorithm content rules, keyboard-navigable
//! (`role="listbox"`/`role="option"`, roving `tabindex`, `Home`/`End`, `Enter`/`Space`), with
//! independent FOCUS (`focused_row`, moves with Arrow keys) and SELECTION (`selected_row`, the
//! prop `graph_page.rs` reads to drive the canvas dim/hide/fit wiring, plan 44-13 Task 3) state.
//!
//! # Testing approach (plain-function-first, per `tool_rail.rs`'s documented convention)
//!
//! Every piece of transcription-sensitive content-building logic — per-outcome row content, the
//! community-swatch-hue passthrough (never a locally-recomputed hue), the "+N more" truncation
//! text, the roving-focus index transition — is factored into plain functions taking owned/
//! borrowed DTO values and returning small local content types, fully host-testable without a
//! DOM/browser (no headless-browser runner exists in this workspace, phase 42 research Pitfall 10).
//!
//! Module-wide `#![allow(dead_code)]`: this file lands before `graph_page.rs` mounts
//! `<ResultsList>` in this same plan's Task 3 — mirrors `tool_rail.rs`/`entity_picker.rs`'s own
//! documented convention for a component committed ahead of its mount site.
#![allow(dead_code)]

use leptos::prelude::*;
use urban_integrity_workbench_dto::{
    AlgorithmOutcome, AlgorithmResult, CommunityResult, EntityRef, PathResult, RankedEntity,
};

use crate::entity_ref_view::EntityRefView;

/// The phase 44 UI spec Copywriting Contract, "Empty state — rail, before any run" — verbatim, shown in
/// place of the list when no result is active yet.
pub const RESULTS_LIST_EMPTY_COPY: &str = "Pick an algorithm and Run to simplify the view.";

// ─── Pure content-building functions (per-outcome) ─────────────────────────────────────────────

/// One path result row's content: the hop sequence as `(entity, edge type walked to the NEXT
/// hop)` pairs — the last hop's edge type is always `None` (mirrors `PathHop.edge_type_to_next`'s
/// own `required_option` shape).
#[derive(Debug, Clone, PartialEq)]
pub struct PathRowContent {
    pub hops: Vec<(EntityRef, Option<String>)>,
}

/// Builds one row per path — `AlgorithmOutcome::Paths`/`ShortestPaths` both render as this same
/// row shape (the phase 44 UI spec: "one row per path... each hop rendered as `EntityRefView`... with the
/// edge type named between consecutive hops").
pub fn paths_rows(paths: &[PathResult]) -> Vec<PathRowContent> {
    paths
        .iter()
        .map(|path| PathRowContent {
            hops: path
                .hops
                .iter()
                .map(|hop| (hop.entity.clone(), hop.edge_type_to_next.clone()))
                .collect(),
        })
        .collect()
}

/// The "+N more" truncation text (the phase 44 UI spec: "a truncated '+N more' Label if the community
/// exceeds the row's comfortable height") — `None` when every member is already shown (`size <=
/// shown`), `Some("+{size - shown} more")` otherwise. `shown` is always the actual number of
/// entities the row rendered (never a hardcoded 5), so this stays correct even if a future
/// change alters how many entities a row shows before truncating.
pub fn community_overflow_text(size: u64, shown: usize) -> Option<String> {
    let shown_u64 = shown as u64;
    if size > shown_u64 {
        Some(format!("+{} more", size - shown_u64))
    } else {
        None
    }
}

/// The maximum number of top entities a community row renders before truncating — matches
/// the phase 44 UI spec's "top five entities by degree" and 44-09's server-side
/// `CommunityResult.top_entities` cap (already <= 5 on the wire; this row builder never assumes
/// that and re-truncates defensively).
const COMMUNITY_ROW_MAX_ENTITIES: usize = 5;

/// One community detection row's content: size, up to
/// [`COMMUNITY_ROW_MAX_ENTITIES`] top entities, the "+N more" text (if any), and the community's
/// own canvas hue — copied VERBATIM from `CommunityResult.color_hue_deg` (plan 44-09's own
/// computed value). This row builder NEVER computes a hue formula itself.
#[derive(Debug, Clone, PartialEq)]
pub struct CommunityRowContent {
    pub size: u64,
    pub top_entities: Vec<EntityRef>,
    pub overflow_text: Option<String>,
    pub swatch_hue_deg: u32,
}

/// Builds one row per community, sorted in the order `communities` already arrives in
/// (44-09's `communities_to_results` already sorts size-descending before assigning hue —
/// this function performs no re-sort of its own).
pub fn community_rows(communities: &[CommunityResult]) -> Vec<CommunityRowContent> {
    communities
        .iter()
        .map(|community| {
            let top_entities: Vec<EntityRef> = community
                .top_entities
                .iter()
                .take(COMMUNITY_ROW_MAX_ENTITIES)
                .cloned()
                .collect();
            let overflow_text = community_overflow_text(community.size, top_entities.len());
            CommunityRowContent {
                size: community.size,
                top_entities,
                overflow_text,
                swatch_hue_deg: community.color_hue_deg,
            }
        })
        .collect()
}

/// k-core's single summary row's text (the phase 44 UI spec: "a single summary row (node/edge count in
/// the core)").
pub fn k_core_summary(node_count: u64, edge_count: u64) -> String {
    format!("{node_count} nodes, {edge_count} edges")
}

/// Ward-reachable subgraph's single summary row's text (the phase 44 UI spec: "a single summary row
/// (node and edge count) — no per-node rows").
pub fn ward_subgraph_summary(node_count: u64, edge_count: u64) -> String {
    format!("{node_count} nodes, {edge_count} edges")
}

/// Builds one `(entity, score)` row per ranked entity — shared by betweenness centrality's
/// ranked-node rows AND k-core's top-nodes-by-degree rows (both `RankedEntity`, `score` means
/// "betweenness value" for one and "plain degree" for the other; the row shape is identical
/// either way, per `AlgorithmResult`'s own doc comment on `RankedEntity`).
pub fn betweenness_rows(top_nodes: &[RankedEntity]) -> Vec<(EntityRef, f64)> {
    top_nodes
        .iter()
        .map(|ranked| (ranked.entity.clone(), ranked.score))
        .collect()
}

/// k-core's row content, in render order: exactly one [`KCoreRow::Summary`] followed by one
/// [`KCoreRow::Node`] per top node (the phase 44 UI spec: "a single summary row... followed by
/// top-nodes-by-degree rows") — a pure, directly-testable proof of that ordering, independent of
/// the `view!` macro.
#[derive(Debug, Clone, PartialEq)]
pub enum KCoreRow {
    Summary(String),
    Node(EntityRef, f64),
}

pub fn k_core_rows(node_count: u64, edge_count: u64, top_nodes: &[RankedEntity]) -> Vec<KCoreRow> {
    let mut rows = vec![KCoreRow::Summary(k_core_summary(node_count, edge_count))];
    rows.extend(
        betweenness_rows(top_nodes)
            .into_iter()
            .map(|(entity, score)| KCoreRow::Node(entity, score)),
    );
    rows
}

/// The total number of interactive rows [`ResultsList`] renders for `outcome` — the single source
/// of truth for BOTH the roving-focus/keyboard-nav bound (this module) and the row-selection ->
/// canvas-index wiring (`graph_page.rs`, this plan's Task 3), so the two can never disagree about
/// how many rows exist. `KCore`/`WardSubgraph` count their one summary row; `Paths`/
/// `ShortestPaths`/`Community`/`Betweenness` count one row per element.
pub fn row_count_for_outcome(outcome: &AlgorithmOutcome) -> usize {
    match outcome {
        AlgorithmOutcome::Paths { paths } => paths.len(),
        AlgorithmOutcome::ShortestPaths { paths, .. } => paths.len(),
        AlgorithmOutcome::Community { communities, .. } => communities.len(),
        AlgorithmOutcome::KCore { top_nodes, .. } => 1 + top_nodes.len(),
        AlgorithmOutcome::Betweenness { top_nodes, .. } => top_nodes.len(),
        AlgorithmOutcome::WardSubgraph { .. } => 1,
        AlgorithmOutcome::HighlightSet { .. } => 1,
    }
}

// ─── Roving-tabindex keyboard helper ────────────────────────────────────────────────────────────

/// The keyboard action moving [`ResultsList`]'s roving FOCUS (distinct from selection) —
/// the phase 44 UI spec's "Keyboard" contract for the results list: `ArrowDown`/`ArrowUp` (wrapping at
/// either end) plus `Home`/`End` (jump to first/last), beyond what `entity_picker.rs`'s
/// `Direction`/`next_highlight` cover (that combobox has no `Home`/`End` requirement).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowNav {
    Up,
    Down,
    Home,
    End,
}

/// Pure, host-testable row-FOCUS-index transition — mirrors `entity_picker.rs::next_highlight`'s
/// exact wraparound shape for `Up`/`Down`, extended with `Home`/`End` (the phase 44 UI spec: "`Home`/`End`
/// jump to the first/last row").
pub fn next_row_index(current: Option<usize>, len: usize, direction: RowNav) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match direction {
        RowNav::Home => Some(0),
        RowNav::End => Some(len - 1),
        RowNav::Up => match current {
            None => Some(len - 1),
            Some(i) => Some(if i == 0 { len - 1 } else { i - 1 }),
        },
        RowNav::Down => match current {
            None => Some(0),
            Some(i) => Some((i + 1) % len),
        },
    }
}

// ─── Rendering ──────────────────────────────────────────────────────────────────────────────────

/// A result row's visual state (the phase 44 UI spec "Results-list row selected state," Color section):
/// default (`--card`/`--border`), hover (`--card-hover`), selected (persistent `--card-hover` +
/// 3px `--foreground` left border, mutually exclusive across the list) — NEVER `--primary`.
fn row_style(selected: bool, hovered: bool) -> String {
    let background = if selected || hovered {
        "var(--card-hover)"
    } else {
        "var(--card)"
    };
    let border_left = if selected {
        "3px solid var(--foreground)"
    } else {
        "3px solid transparent"
    };
    format!(
        "display: flex; flex-direction: column; gap: var(--space-xs); padding: var(--space-sm); \
         border: 1px solid var(--muted-foreground); border-left: {border_left}; \
         border-radius: 6px; \
         background: {background}; cursor: pointer; outline: none; min-height: 44px; \
         min-width: 0; overflow-wrap: anywhere; flex-shrink: 0;"
    )
}

/// One `role="option"` row: roving `tabindex` (`0` only when `focused_row == Some(index)`),
/// `aria-selected` tracking `selected_row`, click sets BOTH focus and selection to `index` (per
/// the phase 44 UI spec: "`Enter`/`Space` selects the focused row" — a mouse click is the same commit
/// action as focus-then-Enter).
fn result_row_wrapper(
    index: usize,
    focused_row: RwSignal<Option<usize>>,
    selected_row: RwSignal<Option<usize>>,
    content: AnyView,
) -> AnyView {
    let is_focused = move || focused_row.get() == Some(index);
    let is_selected = move || selected_row.get() == Some(index);
    let hovered: RwSignal<bool> = RwSignal::new(false);
    view! {
        <div
            id=format!("results-list-option-{index}")
            role="option"
            aria-selected=move || is_selected().to_string()
            tabindex=move || if is_focused() { "0" } else { "-1" }
            style=move || row_style(is_selected(), hovered.get())
            on:mouseenter=move |_| hovered.set(true)
            on:mouseleave=move |_| hovered.set(false)
            on:click=move |ev: web_sys::MouseEvent| {
                // STOP THE BUBBLE. `graph_page.rs` installs a `window`-level click listener for
                // D-12's "clicking empty canvas clears the row selection and closes the side
                // card". It distinguishes a canvas point click via `point_hit_this_click`, which
                // only the canvas handlers set — so a click on THIS row reached it with the flag
                // false, took the empty-canvas branch, and set `selected_row` straight back to
                // `None` within the same event. Every row click self-cancelled, for every
                // algorithm, and the canvas never changed (operator QA 2026-09-12). A row click
                // is not an empty-canvas click.
                ev.stop_propagation();
                focused_row.set(Some(index));
                selected_row.set(Some(index));
            }
        >
            {content}
        </div>
    }
    .into_any()
}

fn path_row_content(row: PathRowContent) -> AnyView {
    view! {
        <div style="display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-xs);">
            {row.hops.into_iter().map(|(entity, edge_type)| {
                view! {
                    <span style="display: inline-flex; align-items: center; gap: var(--space-xs);">
                        <EntityRefView entity=entity />
                        {edge_type.map(|edge_type| view! {
                            <span class="text-label" style="color: var(--muted-foreground);">
                                {format!("\u{2192} {edge_type} \u{2192}")}
                            </span>
                        })}
                    </span>
                }
            }).collect_view()}
        </div>
    }
    .into_any()
}

fn community_row_content(row: CommunityRowContent) -> AnyView {
    let swatch_style = format!(
        "width: 12px; height: 12px; border-radius: 9999px; flex-shrink: 0; \
         background: hsl({}deg 70% 55%);",
        row.swatch_hue_deg
    );
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <div style="display: flex; align-items: center; gap: var(--space-xs);">
                <span aria-hidden="true" style=swatch_style></span>
                <span class="text-body font-mono">{format!("{} entities", row.size)}</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: 2px;">
                {row.top_entities.into_iter().map(|entity| view! { <EntityRefView entity=entity /> }).collect_view()}
            </div>
            {row.overflow_text.map(|text| view! {
                <span class="text-label" style="color: var(--muted-foreground);">{text}</span>
            })}
        </div>
    }
    .into_any()
}

fn ranked_entity_row_content(entity: EntityRef, score: f64) -> AnyView {
    view! {
        <div style="display: flex; align-items: center; justify-content: space-between; gap: var(--space-xs);">
            <EntityRefView entity=entity />
            <span class="text-body font-mono">{score.to_string()}</span>
        </div>
    }
    .into_any()
}

fn summary_row_content(text: String) -> AnyView {
    view! {
        <span class="text-body font-mono">{text}</span>
    }
    .into_any()
}

/// Dispatches on `outcome` and renders the matching row set (per [`row_count_for_outcome`]'s own
/// counting), each wrapped via [`result_row_wrapper`].
/// The one summary row a `highlight_set` result renders — the deep link's own entity count.
/// Singular/plural rather than "1 entities", matching this module's other summary copy.
pub fn highlight_set_summary(count: usize) -> String {
    format!(
        "{count} named {}",
        if count == 1 { "entity" } else { "entities" }
    )
}

fn render_rows(
    outcome: &AlgorithmOutcome,
    focused_row: RwSignal<Option<usize>>,
    selected_row: RwSignal<Option<usize>>,
) -> AnyView {
    match outcome {
        AlgorithmOutcome::Paths { paths } => paths_rows(paths)
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                result_row_wrapper(index, focused_row, selected_row, path_row_content(row))
            })
            .collect_view()
            .into_any(),
        AlgorithmOutcome::ShortestPaths { paths, .. } => paths_rows(paths)
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                result_row_wrapper(index, focused_row, selected_row, path_row_content(row))
            })
            .collect_view()
            .into_any(),
        AlgorithmOutcome::Community { communities, .. } => community_rows(communities)
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                result_row_wrapper(index, focused_row, selected_row, community_row_content(row))
            })
            .collect_view()
            .into_any(),
        AlgorithmOutcome::KCore {
            node_count,
            edge_count,
            top_nodes,
            ..
        } => k_core_rows(*node_count, *edge_count, top_nodes)
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let content = match row {
                    KCoreRow::Summary(text) => summary_row_content(text),
                    KCoreRow::Node(entity, score) => ranked_entity_row_content(entity, score),
                };
                result_row_wrapper(index, focused_row, selected_row, content)
            })
            .collect_view()
            .into_any(),
        AlgorithmOutcome::Betweenness { top_nodes, .. } => betweenness_rows(top_nodes)
            .into_iter()
            .enumerate()
            .map(|(index, (entity, score))| {
                result_row_wrapper(
                    index,
                    focused_row,
                    selected_row,
                    ranked_entity_row_content(entity, score),
                )
            })
            .collect_view()
            .into_any(),
        AlgorithmOutcome::WardSubgraph {
            node_count,
            edge_count,
            ..
        } => {
            let content = summary_row_content(ward_subgraph_summary(*node_count, *edge_count));
            result_row_wrapper(0, focused_row, selected_row, content)
        }
        AlgorithmOutcome::HighlightSet { node_ids, .. } => {
            let content = summary_row_content(highlight_set_summary(node_ids.len()));
            result_row_wrapper(0, focused_row, selected_row, content)
        }
    }
}

/// The Graph page rail's results list (the phase 44 UI spec "Results list (per algorithm)"): renders
/// [`RESULTS_LIST_EMPTY_COPY`] in place of the list when `result.get()` is `None`, otherwise the
/// matching row set for `result.outcome` inside a `role="listbox"` container with roving-tabindex
/// keyboard navigation (`ArrowDown`/`ArrowUp` wrap at either end, `Home`/`End` jump to first/last,
/// `Enter`/`Space` commits the focused row as the SELECTION). FOCUS and SELECTION are two
/// independent signals per the phase 44 UI spec: keyboard focus moves freely; `selected_row` (the prop
/// `graph_page.rs`'s Task 3 wiring reads) only changes on click or `Enter`/`Space`.
#[component]
#[allow(non_snake_case)]
pub fn ResultsList(
    result: Signal<Option<AlgorithmResult>>,
    selected_row: RwSignal<Option<usize>>,
) -> impl IntoView {
    let focused_row: RwSignal<Option<usize>> = RwSignal::new(None);

    let on_keydown = move |ev: web_sys::KeyboardEvent| {
        let total = result
            .get_untracked()
            .as_ref()
            .map(|r| row_count_for_outcome(&r.outcome))
            .unwrap_or(0);
        match ev.key().as_str() {
            "ArrowDown" => {
                ev.prevent_default();
                focused_row.set(next_row_index(
                    focused_row.get_untracked(),
                    total,
                    RowNav::Down,
                ));
            }
            "ArrowUp" => {
                ev.prevent_default();
                focused_row.set(next_row_index(
                    focused_row.get_untracked(),
                    total,
                    RowNav::Up,
                ));
            }
            "Home" => {
                ev.prevent_default();
                focused_row.set(next_row_index(
                    focused_row.get_untracked(),
                    total,
                    RowNav::Home,
                ));
            }
            "End" => {
                ev.prevent_default();
                focused_row.set(next_row_index(
                    focused_row.get_untracked(),
                    total,
                    RowNav::End,
                ));
            }
            "Enter" | " " => {
                ev.prevent_default();
                if let Some(index) = focused_row.get_untracked() {
                    selected_row.set(Some(index));
                }
            }
            _ => {}
        }
    };

    view! {
        <div>
            {move || match result.get() {
                None => view! {
                    <p class="text-body" style="color: var(--muted-foreground);">{RESULTS_LIST_EMPTY_COPY}</p>
                }.into_any(),
                Some(algorithm_result) => view! {
                    <div
                        role="listbox"
                        tabindex="0"
                        style="display: flex; flex-direction: column; gap: var(--space-sm); max-height: max(480px, calc(100vh - 320px)); overflow-y: auto; overflow-x: hidden; min-width: 0;"
                        on:keydown=on_keydown
                    >
                        {render_rows(&algorithm_result.outcome, focused_row, selected_row)}
                    </div>
                }.into_any(),
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::PathHop;

    fn entity(id: &str, name: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Entity".to_string(),
        }
    }

    // ─── paths_rows ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn paths_rows_renders_one_row_per_path_with_edge_type_between_hops() {
        let paths = vec![PathResult {
            hops: vec![
                PathHop {
                    entity: entity("director:jane-smith", "Jane Smith"),
                    edge_type_to_next: Some("AwardedTo".to_string()),
                },
                PathHop {
                    entity: entity("GB-COH:99000001", "Acme Ltd"),
                    edge_type_to_next: None,
                },
            ],
        }];
        let rows = paths_rows(&paths);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hops.len(), 2);
        assert_eq!(
            rows[0].hops[0].0,
            entity("director:jane-smith", "Jane Smith")
        );
        assert_eq!(rows[0].hops[0].1, Some("AwardedTo".to_string()));
        assert_eq!(
            rows[0].hops[1].1, None,
            "the last hop has no edge type to next"
        );
    }

    #[test]
    fn paths_rows_renders_two_hop_path_as_two_paths() {
        let paths = vec![
            PathResult {
                hops: vec![
                    PathHop {
                        entity: entity("a", "A"),
                        edge_type_to_next: Some("Directs".to_string()),
                    },
                    PathHop {
                        entity: entity("b", "B"),
                        edge_type_to_next: None,
                    },
                ],
            },
            PathResult {
                hops: vec![
                    PathHop {
                        entity: entity("a", "A"),
                        edge_type_to_next: Some("Controls".to_string()),
                    },
                    PathHop {
                        entity: entity("c", "C"),
                        edge_type_to_next: None,
                    },
                ],
            },
        ];
        let rows = paths_rows(&paths);
        assert_eq!(
            rows.len(),
            2,
            "one row per path result, not one row for the whole set"
        );
    }

    // ─── community_overflow_text ────────────────────────────────────────────────────────────

    #[test]
    fn community_overflow_text_is_none_when_every_member_is_shown() {
        assert_eq!(community_overflow_text(5, 5), None);
        assert_eq!(
            community_overflow_text(3, 5),
            None,
            "fewer members than shown slots"
        );
    }

    #[test]
    fn community_overflow_text_reports_the_remaining_count() {
        assert_eq!(community_overflow_text(8, 5), Some("+3 more".to_string()));
        assert_eq!(
            community_overflow_text(41_022, 5),
            Some("+41017 more".to_string())
        );
    }

    // ─── community_rows ──────────────────────────────────────────────────────────────────────

    fn sample_community(index: u32, size: u64, hue: u32, entity_count: usize) -> CommunityResult {
        // member_node_ids is deliberately sized to `size` (>= entity_count in every call site
        // below) — a community row must still cap its rendered entity list at
        // COMMUNITY_ROW_MAX_ENTITIES from top_entities alone, never widening to the full
        // membership (44-17: member_node_ids exists for the CANVAS effects, not the row).
        CommunityResult {
            index,
            size,
            top_entities: (0..entity_count)
                .map(|i| entity(&format!("e{i}"), &format!("Entity {i}")))
                .collect(),
            color_hue_deg: hue,
            member_node_ids: (0..size).map(|i| format!("member{i}")).collect(),
        }
    }

    #[test]
    fn community_rows_caps_top_entities_at_five_and_reports_overflow() {
        let communities = vec![sample_community(0, 41_022, 0, 5)];
        let rows = community_rows(&communities);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].top_entities.len(), 5);
        assert_eq!(rows[0].overflow_text, Some("+41017 more".to_string()));
    }

    /// PROVE THIS CAN FAIL: the swatch hue is copied VERBATIM from `CommunityResult.color_hue_deg`
    /// — a version of `community_rows` that recomputed `360 / community_count` instead would give
    /// a different value than the DTO's own non-trivial `90` for this fixture, failing this
    /// assertion.
    #[test]
    fn community_row_swatch_hue_is_copied_verbatim_from_the_dto_never_recomputed() {
        let communities = vec![sample_community(0, 10, 90, 3)];
        let rows = community_rows(&communities);
        assert_eq!(rows[0].swatch_hue_deg, 90);
    }

    /// 44-17: `CommunityResult.member_node_ids` now carries the FULL community membership (used
    /// by the canvas recolour/selection effects, `graph_page.rs`), but the results-list row must
    /// still render at most [`COMMUNITY_ROW_MAX_ENTITIES`] entities from `top_entities` alone —
    /// never widening to `member_node_ids`'s length, which is asserted here to be strictly larger
    /// than the rendered row.
    #[test]
    fn community_row_entity_list_never_exceeds_five_even_when_member_node_ids_has_more() {
        let communities = vec![sample_community(0, 41_022, 0, 5)];
        assert!(
            communities[0].member_node_ids.len() > 5,
            "fixture must actually exercise the gap between full membership and the row cap"
        );
        let rows = community_rows(&communities);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].top_entities.len() <= 5,
            "a row's rendered entity list must never exceed five, even when member_node_ids has more"
        );
    }

    #[test]
    fn community_rows_renders_one_row_per_community_in_input_order() {
        let communities = vec![
            sample_community(0, 100, 0, 2),
            sample_community(1, 50, 90, 2),
            sample_community(2, 10, 180, 1),
        ];
        let rows = community_rows(&communities);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].size, 100);
        assert_eq!(rows[1].size, 50);
        assert_eq!(rows[2].size, 10);
    }

    // ─── k_core_summary / k_core_rows ────────────────────────────────────────────────────────

    #[test]
    fn k_core_summary_states_node_and_edge_count() {
        assert_eq!(k_core_summary(1_204, 3_880), "1204 nodes, 3880 edges");
    }

    #[test]
    fn k_core_renders_exactly_one_summary_row_followed_by_top_node_rows() {
        let top_nodes = vec![
            RankedEntity {
                entity: entity("a", "A"),
                score: 12.0,
            },
            RankedEntity {
                entity: entity("b", "B"),
                score: 9.0,
            },
        ];
        let rows = k_core_rows(1_204, 3_880, &top_nodes);
        assert_eq!(rows.len(), 3, "one summary row + 2 top-node rows");
        assert_eq!(rows[0], KCoreRow::Summary(k_core_summary(1_204, 3_880)));
        assert_eq!(rows[1], KCoreRow::Node(entity("a", "A"), 12.0));
        assert_eq!(rows[2], KCoreRow::Node(entity("b", "B"), 9.0));
    }

    #[test]
    fn k_core_with_no_top_nodes_still_renders_the_single_summary_row() {
        let rows = k_core_rows(0, 0, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], KCoreRow::Summary(k_core_summary(0, 0)));
    }

    // ─── ward_subgraph_summary / row_count_for_outcome (WardSubgraph) ──────────────────────

    #[test]
    fn ward_subgraph_summary_states_node_and_edge_count() {
        assert_eq!(
            ward_subgraph_summary(1_842, 3_110),
            "1842 nodes, 3110 edges"
        );
    }

    #[test]
    fn ward_subgraph_outcome_renders_exactly_one_summary_row_no_per_node_rows() {
        let outcome = AlgorithmOutcome::WardSubgraph {
            ward: entity("ward:X05001499", "Sample Ward"),
            hops: 2,
            node_count: 1_842,
            edge_count: 3_110,
            node_ids: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };
        assert_eq!(
            row_count_for_outcome(&outcome),
            1,
            "exactly one summary row, regardless of node_ids length"
        );
    }

    // ─── betweenness_rows ────────────────────────────────────────────────────────────────────

    #[test]
    fn betweenness_rows_maps_entity_and_score_in_order() {
        let top_nodes = vec![
            RankedEntity {
                entity: entity("a", "A"),
                score: 0.9,
            },
            RankedEntity {
                entity: entity("b", "B"),
                score: 0.4,
            },
        ];
        let rows = betweenness_rows(&top_nodes);
        assert_eq!(rows, vec![(entity("a", "A"), 0.9), (entity("b", "B"), 0.4)]);
    }

    // ─── row_count_for_outcome ───────────────────────────────────────────────────────────────

    #[test]
    fn row_count_for_outcome_matches_every_variant() {
        assert_eq!(
            row_count_for_outcome(&AlgorithmOutcome::Paths {
                paths: vec![PathResult { hops: vec![] }, PathResult { hops: vec![] },],
            }),
            2
        );
        assert_eq!(
            row_count_for_outcome(&AlgorithmOutcome::ShortestPaths {
                paths: vec![PathResult { hops: vec![] }],
                requested_k: 3,
            }),
            1
        );
        assert_eq!(
            row_count_for_outcome(&AlgorithmOutcome::Community {
                communities: vec![sample_community(0, 10, 0, 1), sample_community(1, 5, 90, 1)],
                hidden_node_ids: vec![],
            }),
            2
        );
        assert_eq!(
            row_count_for_outcome(&AlgorithmOutcome::KCore {
                k: 3,
                node_count: 10,
                edge_count: 20,
                top_nodes: vec![RankedEntity {
                    entity: entity("a", "A"),
                    score: 1.0,
                }],
                hidden_node_ids: vec![],
            }),
            2,
            "1 summary row + 1 top-node row"
        );
        assert_eq!(
            row_count_for_outcome(&AlgorithmOutcome::Betweenness {
                sampled_sources: 500,
                total_sources: 113_384,
                top_nodes: vec![
                    RankedEntity {
                        entity: entity("a", "A"),
                        score: 1.0,
                    },
                    RankedEntity {
                        entity: entity("b", "B"),
                        score: 0.5,
                    },
                ],
            }),
            2
        );
    }

    // ─── next_row_index / RowNav ─────────────────────────────────────────────────────────────

    #[test]
    fn next_row_index_returns_none_for_an_empty_list() {
        assert_eq!(next_row_index(None, 0, RowNav::Down), None);
        assert_eq!(next_row_index(Some(0), 0, RowNav::Up), None);
    }

    #[test]
    fn next_row_index_starts_at_first_row_on_first_down_and_last_on_first_up() {
        assert_eq!(next_row_index(None, 5, RowNav::Down), Some(0));
        assert_eq!(next_row_index(None, 5, RowNav::Up), Some(4));
    }

    /// PROVE THIS CAN FAIL: run against a version that saturates instead of wrapping at either
    /// end, and this fails.
    #[test]
    fn next_row_index_wraps_at_both_ends() {
        assert_eq!(next_row_index(Some(4), 5, RowNav::Down), Some(0));
        assert_eq!(next_row_index(Some(0), 5, RowNav::Up), Some(4));
    }

    #[test]
    fn next_row_index_increments_and_decrements_within_bounds() {
        assert_eq!(next_row_index(Some(1), 5, RowNav::Down), Some(2));
        assert_eq!(next_row_index(Some(2), 5, RowNav::Up), Some(1));
    }

    #[test]
    fn next_row_index_home_and_end_jump_regardless_of_current() {
        assert_eq!(next_row_index(Some(3), 5, RowNav::Home), Some(0));
        assert_eq!(next_row_index(Some(1), 5, RowNav::End), Some(4));
        assert_eq!(next_row_index(None, 5, RowNav::Home), Some(0));
        assert_eq!(next_row_index(None, 5, RowNav::End), Some(4));
    }

    // ─── row_style ───────────────────────────────────────────────────────────────────────────

    /// Per the phase 44 UI spec's Color section: selected rows carry `--card-hover` + a `--foreground`
    /// left border, and — critically — NEVER `--primary` (reserved for the Run/Cancel-run button
    /// and focus rings only).
    #[test]
    fn row_style_selected_never_uses_primary() {
        let style = row_style(true, false);
        assert!(style.contains("var(--card-hover)"));
        assert!(style.contains("var(--foreground)"));
        assert!(!style.contains("var(--primary)"));
    }

    #[test]
    fn row_style_border_is_readable_against_the_card_not_the_near_invisible_border_token() {
        // `--border` is #1e293b against a #111827 `--card` in dark theme — so low-contrast that a
        // list of seven distinct paths read as one undifferentiated block (operator QA
        // 2026-09-12: "the borders around these need to be made more visible so it is easy to see
        // there are distinct paths"). PROVE THIS CAN FAIL: swap the token back to `var(--border)`
        // and this test fails on the first assertion.
        for (selected, hovered) in [(false, false), (true, false), (false, true)] {
            let style = row_style(selected, hovered);
            assert!(
                style.contains("border: 1px solid var(--muted-foreground);"),
                "row border must use the readable token, got: {style}"
            );
            assert!(
                !style.contains("border: 1px solid var(--border);"),
                "row border must not fall back to the near-invisible --border token"
            );
        }
    }

    #[test]
    fn row_style_never_shrinks_inside_the_scrolling_listbox() {
        // An explicit `min-height` on a column-flex item overrides `min-height: auto`, so without
        // `flex-shrink: 0` a capped, scrolling listbox squashes every row to 44px and the row
        // content paints over its neighbours (QA gate 2026-09-08: "data overlays itself").
        for (selected, hovered) in [(false, false), (true, false), (false, true)] {
            assert!(row_style(selected, hovered).contains("flex-shrink: 0;"));
        }
    }

    #[test]
    fn row_style_default_has_no_left_border_highlight() {
        let style = row_style(false, false);
        assert!(style.contains("var(--card)"));
        assert!(!style.contains("var(--foreground)"));
    }

    // ─── RESULTS_LIST_EMPTY_COPY ─────────────────────────────────────────────────────────────

    #[test]
    fn results_list_empty_copy_matches_the_copywriting_contract_verbatim() {
        assert_eq!(
            RESULTS_LIST_EMPTY_COPY,
            "Pick an algorithm and Run to simplify the view."
        );
    }
}
