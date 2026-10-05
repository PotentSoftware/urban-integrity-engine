//! `neighbourhood.rs` — CNV-01's neighbourhood expansion on the canvas: the hop control, the
//! expansion state it produces, and the reported node cap (CNV-02, D-12).
//!
//! # What an expansion *is*, on this canvas
//!
//! The Graph page mounts the **whole population** as one Arrow buffer (44-10/44-16) and never
//! adds or removes a point after that: `CosmographHandle` exposes opacity, labels, colour, size
//! and fit, and no method that grows the mounted set. So "expand a neighbourhood on the canvas"
//! cannot mean "draw these nodes"; it means **narrow the canvas to these nodes**, which is the
//! same channel plan 44-13's row selection already uses (`set_point_opacity(&[], &hidden)` plus
//! `set_visible_labels`). An expansion is therefore a *focus set*, and [`visible_ids`] is the
//! whole of it.
//!
//! That is not a workaround: it is why the seed has to be added back by hand. `GET
//! /api/entity/neighbourhood` returns the neighbourhood **excluding the seed** — the read path's
//! own documented rule, "the seed is not a member of its own neighbourhood" — so a focus set built
//! from `nodes` alone hides the very node the analyst clicked.
//! `the_seed_is_visible_in_its_own_neighbourhood` pins that.
//!
//! # The caps are the server's, and the control offers exactly what the server will honour
//!
//! Both constants come from `workbench-dto::caps` (D-13), which is also what
//! `entity/neighbourhood.rs`'s clamp reads. [`hop_choices`] is derived from
//! [`CEILING_NEIGHBOURHOOD_HOPS`] rather than written out, so a control offering a hop count the
//! server would silently clamp is not something this file can express.
//!
//! The *applied* values travel back on the response and everything on screen is composed from
//! those (see `expansion_status_line` in plan 45-13 Task 2), never from what was requested.

use std::collections::{BTreeMap, HashSet};

use leptos::prelude::*;
use urban_integrity_workbench_dto::caps::{
    CEILING_NEIGHBOURHOOD_HOPS, CLUSTER_COLLAPSE_THRESHOLD, DEFAULT_NEIGHBOURHOOD_HOPS,
    DEFAULT_NEIGHBOURHOOD_NODE_CAP,
};
use urban_integrity_workbench_dto::neighbourhood::{
    NeighbourEdgeView, NeighbourhoodNodeView, NeighbourhoodView,
};

use crate::graph_status_line::format_thousands;
use crate::profile::pivot::encode_segment;
use crate::profile::sections::{range_basis, range_text};

/// `GET /api/entity/neighbourhood?id=…&hops=…&node_cap=…` — plan 45-08's handler.
///
/// **Not** `/api/graph/neighbourhood`. The route lives under `/api/entity/` because it takes an
/// entity id and answers about that subject, alongside `/api/entity/profile` and
/// `/api/entity/disambiguate`; `crates/workbench/src/boot.rs` registers it there.
pub const NEIGHBOURHOOD_URL_PATH: &str = "/api/entity/neighbourhood";

/// The D-23 "unreadable endpoint shape" copy for [`NEIGHBOURHOOD_URL_PATH`], in this crate's
/// established one-sentence-naming-the-endpoint form.
pub const NEIGHBOURHOOD_FETCH_ERROR_COPY: &str =
    "The workbench returned data this page cannot read (/api/entity/neighbourhood).";

/// Shown in place of the hop control's hint when no node has been clicked yet. The control itself
/// stays on screen and disabled rather than appearing on selection: a control that materialises
/// under the pointer is harder to find again than one that was always there.
pub const EXPANSION_NEEDS_A_SEED: &str = "Click a node on the canvas to expand its neighbourhood.";

/// The label on the control that clears an expansion and returns the canvas to the whole
/// population. "Clear", not "Collapse": collapsing is what a cluster glyph does to part of an
/// expansion (Task 5), and one word for two actions would make the status line ambiguous.
pub const CLEAR_EXPANSION: &str = "Clear expansion";

/// The hop counts the control offers — every value the server will actually honour, and no other.
///
/// Derived from [`CEILING_NEIGHBOURHOOD_HOPS`] rather than listed, because a listed `[1, 2, 3]`
/// would go on offering 3 the day the ceiling dropped to 2 and the server began clamping it
/// silently.
pub fn hop_choices() -> Vec<u32> {
    (1..=CEILING_NEIGHBOURHOOD_HOPS).collect()
}

/// The button's own label, agreeing with its number. English does not make "1 hops" acceptable and
/// this project has already corrected the same class of defect twice (45-10 §7.2, 45-12).
pub fn expand_button_label(hops: u32) -> String {
    if hops == 1 {
        "Expand 1 hop".to_string()
    } else {
        format!("Expand {hops} hops")
    }
}

/// `GET /api/entity/neighbourhood` for one expansion.
///
/// `hops` is clamped here as well as server-side, against the **same constant** — so the two
/// cannot disagree, and the URL never carries a value the handler would silently rewrite. The node
/// cap is sent explicitly rather than left to the handler's default for the same reason the
/// response reports it back: "the cap was 250 and 40 nodes came back" is a different statement
/// from "40 nodes is all there is", and both ends should be naming the same number.
pub fn neighbourhood_url(id: &str, hops: u32, node_cap: u32) -> String {
    let hops = hops.clamp(1, CEILING_NEIGHBOURHOOD_HOPS);
    format!(
        "{NEIGHBOURHOOD_URL_PATH}?id={}&hops={hops}&node_cap={node_cap}",
        encode_segment(id)
    )
}

/// Fetches one expansion. Same fetch/error-string shape as every other fetch in this crate.
pub async fn fetch_neighbourhood(
    id: String,
    hops: u32,
    node_cap: u32,
) -> Result<NeighbourhoodView, String> {
    let response = gloo_net::http::Request::get(&neighbourhood_url(&id, hops, node_cap))
        .send()
        .await
        .map_err(|_| NEIGHBOURHOOD_FETCH_ERROR_COPY.to_string())?;
    if !response.ok() {
        return Err(NEIGHBOURHOOD_FETCH_ERROR_COPY.to_string());
    }
    response
        .json::<NeighbourhoodView>()
        .await
        .map_err(|_| NEIGHBOURHOOD_FETCH_ERROR_COPY.to_string())
}

/// The canvas focus set for one expansion: **the seed and every node the walk returned**.
///
/// The seed is first and is added here rather than assumed present. `NeighbourhoodView::nodes`
/// excludes it by the read path's definition, so a focus set of `nodes` alone would hide the node
/// the analyst clicked to start the expansion — an expansion that loses its own subject.
///
/// Order is the response's (hop ascending, then id ascending), which is also the order a cut keeps,
/// so what is on screen is the nearest window of the neighbourhood rather than an arbitrary one.
pub fn visible_ids(view: &NeighbourhoodView) -> Vec<String> {
    let mut ids = Vec::with_capacity(view.nodes.len() + view.seeds.len());
    ids.extend(view.seeds.iter().map(|s| s.id.clone()));
    ids.extend(view.nodes.iter().map(|n| n.entity.id.clone()));
    ids
}

/// The default node cap one expansion asks for. A named re-export of the D-13 constant so the
/// page's signal initialiser cannot drift from the handler's default.
pub const EXPANSION_DEFAULT_NODE_CAP: u32 = DEFAULT_NEIGHBOURHOOD_NODE_CAP;

/// The hop count the control opens on (CNV-01's "expand N hops at a time" — the operator arrives
/// at the immediate neighbourhood and chooses to go further).
pub const EXPANSION_DEFAULT_HOPS: u32 = DEFAULT_NEIGHBOURHOOD_HOPS;

/// The status line under the canvas, for one expansion (CNV-02, D-12).
///
/// # The defect this exists to prevent
///
/// A silently shorter graph. A neighbourhood cut at the node cap looks **exactly** like a small
/// neighbourhood — same points, same shape, no error — so the only thing that can tell them apart
/// is a sentence, and the sentence has to be there every time the cut happens and absent every time
/// it does not. A line that always mentioned the cap would be as useless as one that never did:
/// nothing is learned from a warning that is always on.
///
/// # What it is composed from
///
/// [`NeighbourhoodView`]'s own fields, and nothing else. `Truncation::detail` is a **server-side
/// statement of what was cut, never the pre-rendered UI line** — that rule is phase 44's and the
/// neighbourhood DTO restates it — so the number here is derived from `total_discovered` and
/// `nodes.len()`, the same two numbers the server derived its own report from. The *gate* is the
/// server's `truncation.truncated`, not this side's arithmetic: if the two could ever disagree the
/// screen should follow the producer, and `the_cap_clause_and_the_servers_own_report_never_disagree`
/// pins that they cannot.
///
/// Both counts are stated on **both sides** of the cut, because "showing 40 of 40" and "showing 250
/// of 1,842" are the two answers this line exists to distinguish, and dropping the total from the
/// uncut case would make the uncut case unreadable as an answer at all.
pub fn expansion_status_line(view: &NeighbourhoodView) -> String {
    let shown = view.nodes.len() as u64;
    let total = view.total_discovered;
    let hops = hop_phrase(view.hops);
    let cap = format_thousands(u64::from(view.node_cap));

    // An empty neighbourhood is a different statement from a windowed one, and "Showing 0 of 0
    // nodes" reads as a rendering fault rather than as an answer. The handler returns 200 with an
    // empty neighbourhood for an isolated node AND for an id that is not in the graph at all, so
    // this sentence deliberately claims only what both cases share.
    if total == 0 {
        let subjects: Vec<&str> = view.seeds.iter().map(|s| s.id.as_str()).collect();
        return format!(
            "No node lies within {hops} of {} (node cap {cap}).",
            subjects.join(", ")
        );
    }

    let head = format!(
        "Showing {} of {} {} within {hops}",
        format_thousands(shown),
        format_thousands(total),
        node_noun(total),
    );
    if view.truncation.truncated {
        let cut = total.saturating_sub(shown);
        format!(
            "{head} \u{2014} {} not shown (node cap {cap}).",
            format_thousands(cut)
        )
    } else {
        format!("{head} (node cap {cap}).")
    }
}

/// `"1 hop"` / `"2 hops"`. English does not make "within 1 hops" acceptable, and this project has
/// corrected the same class of defect in its own copy twice already (45-10, 45-12).
fn hop_phrase(hops: u32) -> String {
    if hops == 1 {
        "1 hop".to_string()
    } else {
        format!("{hops} hops")
    }
}

/// The noun agrees with the **total**, which is the number it sits beside in the sentence — not
/// with the window, which is the agreement error 45-10 shipped one sentence over and corrected.
fn node_noun(total: u64) -> &'static str {
    if total == 1 {
        "node"
    } else {
        "nodes"
    }
}

/// The node in this expansion with id `id`, if the walk reached it.
///
/// The seed is deliberately not matched: it was not *reached* by anything, so there is no edge to
/// describe, and returning the seed here would make the panel invent a "reached from" for the node
/// the analyst started at.
pub fn reached_node<'a>(
    view: &'a NeighbourhoodView,
    id: &str,
) -> Option<&'a NeighbourhoodNodeView> {
    view.nodes.iter().find(|n| n.entity.id == id)
}

/// How one edge of an expansion reads: **its type and its date range** (CNV-02).
///
/// The range is rendered by `profile::sections::range_text` — 45-12's function, not a second one.
/// That matters more here than it looks: `range_text` is where `Unrecorded` became the word
/// **"Undated"**, where an open-ended range renders `"2019-04-01 →"` rather than the
/// `"→ present"` this store cannot support (`effective_to` is null on every edge in it), and where
/// the date is cut to its ten-character head. A canvas that spelled any of those differently from
/// the profile panel directly above it would be two vocabularies on one screen.
pub fn edge_line(edge: &NeighbourEdgeView) -> String {
    format!("{} \u{2014} {}", edge.edge_type, range_text(&edge.temporal))
}

/// How the walk got to this node: `"Reached from {id} at hop {n}"`.
pub fn reached_from_line(node: &NeighbourhoodNodeView) -> String {
    format!("Reached from {} at hop {}.", node.reached_from, node.hop)
}

/// The note under a set of edges **none** of which records a date range — which on this corpus is
/// the ordinary case, not an edge case.
///
/// Returns `None` as soon as one edge is dated: a mixed set is legible from the lines themselves,
/// where one row says `Undated` and the one beside it states a date.
///
/// # Why this is not [`urban_integrity_workbench_dto::temporal::NULL_RANGE_RULE`]
///
/// The rule sentence is about what an **as-at filter** could not test, and the canvas runs no
/// as-at filter. 45-12 settled the same question in the other direction on the profile screen — an
/// unfiltered profile states no rule, because "a permanent notice would be a claim about a filter
/// nobody ran". Rendering it here would be that defect one surface over, and paraphrasing it is
/// explicitly forbidden (45-06: "render `NULL_RANGE_RULE` and `UNDATED_ROWS_ARE` verbatim; do not
/// paraphrase"). So this says strictly less: a **count** and the **fields consulted**, which is the
/// shape of 45-12's `as_at_untested_notice` with the as-at clause removed rather than reworded.
pub fn undated_edges_note(edges: &[NeighbourEdgeView]) -> Option<String> {
    if edges.is_empty() || edges.iter().any(|e| !e.temporal.range.is_unrecorded()) {
        return None;
    }
    let n = edges.len();
    let noun = if n == 1 { "edge" } else { "edges" };
    let verb = if n == 1 { "records" } else { "record" };
    let mut bases: Vec<&str> = edges
        .iter()
        .map(|e| range_basis(&e.temporal.range))
        .collect();
    bases.sort_unstable();
    bases.dedup();
    Some(format!(
        "{n} {noun} here {verb} no date range. Fields consulted: {}.",
        bases.join("; ")
    ))
}

/// A high-degree cluster inside an expansion: every node one parent pulled in, at or above
/// [`CLUSTER_COLLAPSE_THRESHOLD`].
///
/// A "cluster" here is defined by the expansion's own tree, not by a community algorithm: the
/// members are the nodes whose `reached_from` is this parent. That is the set a glyph can honestly
/// stand for, because it is the set the walk actually hung off that node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    /// The node the members were reached through, which stays on the canvas as the glyph.
    pub parent_id: String,
    /// Every member, in the expansion's own order. **The glyph stands for exactly these**, and
    /// re-expanding restores exactly these.
    pub member_ids: Vec<String>,
}

/// Every collapsible cluster in this expansion, parent id ascending.
///
/// Deterministic order, so the list of controls does not reshuffle between two expansions of the
/// same neighbourhood.
pub fn clusters(view: &NeighbourhoodView) -> Vec<Cluster> {
    let mut by_parent: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for node in &view.nodes {
        by_parent
            .entry(node.reached_from.as_str())
            .or_default()
            .push(node.entity.id.clone());
    }
    by_parent
        .into_iter()
        .filter(|(_, members)| members.len() >= CLUSTER_COLLAPSE_THRESHOLD as usize)
        .map(|(parent_id, member_ids)| Cluster {
            parent_id: parent_id.to_string(),
            member_ids,
        })
        .collect()
}

/// **What a collapsed glyph says it stands for.** The count is the whole point: a glyph that did
/// not name its population would be a silently shorter graph wearing a different hat, which is the
/// same defect the node cap's status line exists to prevent.
///
/// PROVE it agrees with the members it was built from:
/// `a_collapsed_glyph_says_how_many_nodes_it_stands_for`.
pub fn glyph_label(cluster: &Cluster) -> String {
    let n = cluster.member_ids.len();
    let noun = if n == 1 { "node" } else { "nodes" };
    format!(
        "{} {noun} collapsed under {}",
        format_thousands(n as u64),
        cluster.parent_id
    )
}

/// The label on the control that collapses or re-expands one cluster.
pub fn cluster_toggle_label(cluster: &Cluster, collapsed: bool) -> String {
    let n = format_thousands(cluster.member_ids.len() as u64);
    if collapsed {
        format!("Show {n} again")
    } else {
        format!("Collapse {n}")
    }
}

/// The canvas focus set with `collapsed` parents' members folded away.
///
/// The parents themselves **stay visible** — they are the glyphs. The seed stays visible for the
/// reason [`visible_ids`] gives. With nothing collapsed this returns exactly [`visible_ids`], which
/// is what makes collapse and re-expand a round trip rather than two separate renderings.
pub fn visible_ids_with_collapsed(view: &NeighbourhoodView, collapsed: &[String]) -> Vec<String> {
    if collapsed.is_empty() {
        return visible_ids(view);
    }
    // Owned rather than borrowed: `clusters` clones its member ids into a temporary, so a set of
    // `&str` into it would not outlive the statement that built it.
    let folded: HashSet<String> = clusters(view)
        .into_iter()
        .filter(|c| collapsed.contains(&c.parent_id))
        .flat_map(|c| c.member_ids)
        .collect();
    visible_ids(view)
        .into_iter()
        .filter(|id| !folded.contains(id))
        .collect()
}

/// The size score for each collapsed cluster's parent, so a glyph **looks** like the many nodes it
/// stands for rather than like one node beside a gap.
///
/// The scale is the canvas's existing one: `set_point_size_by_score` takes the same 1x-3x range
/// plan 44-13's betweenness effect uses, and a cluster is scaled linearly between them by its
/// member count against [`DEFAULT_NEIGHBOURHOOD_NODE_CAP`] — clamped, so a cluster larger than the
/// cap (reachable only if the cap was raised) never scales past 3x.
///
/// No new shim method: `CosmographHandle` has no way to add a synthetic point, so a glyph is an
/// existing point made large, not a new one. See this plan's summary for what that costs.
pub fn glyph_sizes(view: &NeighbourhoodView, collapsed: &[String]) -> Vec<(String, f64)> {
    clusters(view)
        .into_iter()
        .filter(|c| collapsed.contains(&c.parent_id))
        .map(|c| {
            let share = c.member_ids.len() as f64 / f64::from(DEFAULT_NEIGHBOURHOOD_NODE_CAP);
            (c.parent_id, (1.0 + 2.0 * share).clamp(1.0, 3.0))
        })
        .collect()
}

/// How a cluster reads in the list, collapsed or not.
///
/// The *collapsed* form is [`glyph_label`] and names what the glyph stands for; the expanded form
/// names the same population without claiming anything is hidden. One function so the two cannot
/// come to describe different numbers.
pub fn cluster_line(cluster: &Cluster, collapsed: bool) -> String {
    if collapsed {
        return glyph_label(cluster);
    }
    let n = cluster.member_ids.len();
    let noun = if n == 1 { "node" } else { "nodes" };
    format!(
        "{} {noun} under {}",
        format_thousands(n as u64),
        cluster.parent_id
    )
}

/// The caption over the cluster list beneath the hop control.
pub const CLUSTERS_CAPTION: &str = "High-degree clusters";

/// CNV-02's cluster glyphs: one row per collapsible cluster, each naming its population and
/// carrying the control that folds or restores it.
///
/// Nothing collapses on its own. A graph that folded part of itself away without being asked would
/// be the node cap's defect with a different mechanism, so the threshold in
/// [`CLUSTER_COLLAPSE_THRESHOLD`] is the point at which a *control* appears.
#[component]
#[allow(non_snake_case)]
pub fn ClusterGlyphs(
    expansion: Signal<Option<NeighbourhoodView>>,
    /// The parent ids whose clusters are currently folded away. The canvas reads this too, so the
    /// list and the points can never disagree about what is hidden.
    collapsed: RwSignal<Vec<String>>,
) -> impl IntoView {
    view! {
        {move || {
            let found = expansion.get().map(|view| clusters(&view)).unwrap_or_default();
            (!found.is_empty()).then(|| {
                view! {
                    <div style="display: flex; flex-direction: column; gap: var(--space-xs); padding-bottom: var(--space-sm);">
                        <span class="text-label" style="color: var(--muted-foreground);">
                            {CLUSTERS_CAPTION}
                        </span>
                        {found.into_iter().map(|cluster| {
                            let parent = cluster.parent_id.clone();
                            let is_collapsed = {
                                let parent = parent.clone();
                                move || collapsed.get().contains(&parent)
                            };
                            let line_cluster = cluster.clone();
                            let button_cluster = cluster.clone();
                            let line_collapsed = is_collapsed.clone();
                            let button_collapsed = is_collapsed.clone();
                            view! {
                                <div style="display: flex; align-items: center; gap: var(--space-sm); flex-wrap: wrap;">
                                    <span class="text-body">
                                        {move || cluster_line(&line_cluster, line_collapsed())}
                                    </span>
                                    <button
                                        type="button"
                                        class="text-label"
                                        style=CONTROL_STYLE
                                        on:click=move |_| {
                                            let parent = parent.clone();
                                            collapsed.update(|open| {
                                                if let Some(at) = open.iter().position(|p| p == &parent) {
                                                    open.remove(at);
                                                } else {
                                                    open.push(parent);
                                                }
                                            });
                                        }
                                    >
                                        {move || cluster_toggle_label(&button_cluster, button_collapsed())}
                                    </button>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                }
            })
        }}
    }
}

/// The caption over the edge list in the inspection panel.
pub const REACHED_BY_CAPTION: &str = "Reached by";

const CONTROL_STYLE: &str = "min-height: 44px; padding: 0 var(--space-md); background: var(--card); color: var(--foreground); border: 1px solid var(--border); border-radius: 6px; cursor: pointer;";
const SELECT_STYLE: &str = "min-height: 44px; padding: 0 var(--space-sm); background: var(--card); color: var(--foreground); border: 1px solid var(--border); border-radius: 6px;";

/// CNV-01's explicit hop control: a hop count, an **Expand** button, and a **Clear expansion**
/// button once there is something to clear.
///
/// `seed_label` is `None` until a node is clicked, and the control is disabled with
/// [`EXPANSION_NEEDS_A_SEED`] beneath it in that state — visible but inert, so the analyst can see
/// what a click will offer before making one.
#[component]
#[allow(non_snake_case)]
pub fn HopControl(
    /// How to name the clicked node, or `None` when nothing is selected.
    ///
    /// This is the node's **id**, not its display name: the canvas resolves a clicked point to an
    /// id out of the mounted Arrow column, and the name only arrives with the inspection panel's
    /// own fetch. Showing the id is the honest option — it is what the expansion will be taken
    /// from, and it is what the analyst can copy out of the panel beside it.
    seed_label: Signal<Option<String>>,
    hops: RwSignal<u32>,
    /// True while a fetch is in flight — the button says so rather than appearing to do nothing.
    running: Signal<bool>,
    /// True when an expansion is on the canvas, which is when **Clear expansion** is offered.
    expanded: Signal<bool>,
    on_expand: impl Fn() + 'static,
    on_clear: impl Fn() + 'static,
) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs); padding: var(--space-sm) 0;">
            <div style="display: flex; align-items: center; gap: var(--space-sm); flex-wrap: wrap;">
                <label class="text-label" for="neighbourhood-hops" style="color: var(--muted-foreground);">
                    "Hops"
                </label>
                <select
                    id="neighbourhood-hops"
                    style=SELECT_STYLE
                    disabled=move || seed_label.get().is_none()
                    on:change=move |ev| {
                        if let Ok(value) = event_target_value(&ev).parse::<u32>() {
                            hops.set(value);
                        }
                    }
                >
                    {hop_choices()
                        .into_iter()
                        .map(|choice| {
                            view! {
                                <option value=choice.to_string() selected=move || hops.get() == choice>
                                    {choice.to_string()}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
                <button
                    type="button"
                    class="text-label"
                    style=CONTROL_STYLE
                    disabled=move || seed_label.get().is_none() || running.get()
                    on:click=move |_| on_expand()
                >
                    {move || if running.get() {
                        "Expanding\u{2026}".to_string()
                    } else {
                        expand_button_label(hops.get())
                    }}
                </button>
                // Rendered always and disabled when there is nothing to clear, in the same shape
                // as the status line's own Reset control (the phase 44 UI spec). A button that appears
                // only once an expansion exists would move the two beside it every time.
                <button
                    type="button"
                    class="text-label"
                    style=move || if expanded.get() {
                        CONTROL_STYLE.to_string()
                    } else {
                        format!("{CONTROL_STYLE} opacity: 0.4; cursor: default;")
                    }
                    disabled=move || !expanded.get()
                    on:click=move |_| on_clear()
                >
                    {CLEAR_EXPANSION}
                </button>
            </div>
            <span class="text-label" style="color: var(--muted-foreground);">
                {move || match seed_label.get() {
                    None => EXPANSION_NEEDS_A_SEED.to_string(),
                    Some(label) => format!("Expanding from {label}."),
                }}
            </span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::temporal::{EffectiveRange, Temporal};
    use urban_integrity_workbench_dto::EntityRef;

    fn seed() -> EntityRef {
        EntityRef {
            id: "GB-COH:X6999001".to_string(),
            name: "Example Holdings Ltd".to_string(),
            kind: "Entity".to_string(),
        }
    }

    fn undated_edge(edge_type: &str) -> NeighbourEdgeView {
        NeighbourEdgeView {
            edge_type: edge_type.to_string(),
            temporal: Temporal::unfiltered(EffectiveRange::Unrecorded {
                basis: format!("{edge_type}.meta.effective_from / {edge_type}.meta.effective_to"),
            }),
        }
    }

    fn dated_edge(edge_type: &str, from: &str) -> NeighbourEdgeView {
        NeighbourEdgeView {
            edge_type: edge_type.to_string(),
            temporal: Temporal::unfiltered(EffectiveRange::Recorded {
                from: Some(from.to_string()),
                to: None,
                basis: format!("{edge_type}.meta.effective_from / {edge_type}.meta.effective_to"),
            }),
        }
    }

    fn node(i: u32) -> NeighbourhoodNodeView {
        NeighbourhoodNodeView {
            entity: EntityRef {
                id: format!("contract:X9999-c{i:04}"),
                name: format!("Contract {i}"),
                kind: "Contract".to_string(),
            },
            hop: 1,
            edges: vec![undated_edge("AwardedTo")],
            reached_from: seed().id,
        }
    }

    fn view(population: u32, cap: u32) -> NeighbourhoodView {
        NeighbourhoodView::cut(vec![seed()], 1, cap, (0..population).map(node).collect())
    }

    /// The control offers exactly the hop counts the server honours. A hard-coded list would go on
    /// offering a value the handler had begun clamping, with nothing on screen to say so.
    ///
    /// PROVE THIS CAN FAIL: return `vec![1, 2, 3, 4]` from `hop_choices` and this fails — both on
    /// the ceiling and on the equality with the clamp's own range.
    #[test]
    fn the_hop_control_offers_every_hop_the_server_will_honour_and_no_more() {
        let choices = hop_choices();
        assert_eq!(
            choices,
            (1..=CEILING_NEIGHBOURHOOD_HOPS).collect::<Vec<u32>>()
        );
        assert_eq!(choices.first(), Some(&1), "a zero-hop expansion is not one");
        assert_eq!(choices.last(), Some(&CEILING_NEIGHBOURHOOD_HOPS));
        // Non-vacuity: a one-element list would make "offers every hop" trivially true while
        // leaving the operator no choice to make — the control would be a label.
        assert!(
            choices.len() >= 2,
            "a hop control with {} choice(s) is not a control",
            choices.len()
        );
        assert!(choices.contains(&DEFAULT_NEIGHBOURHOOD_HOPS));
    }

    /// PROVE THIS CAN FAIL: drop the `if hops == 1` arm and this fails on "1 hops".
    #[test]
    fn the_expand_button_agrees_with_its_own_number() {
        assert_eq!(expand_button_label(1), "Expand 1 hop");
        assert_eq!(expand_button_label(2), "Expand 2 hops");
        assert_eq!(
            expand_button_label(CEILING_NEIGHBOURHOOD_HOPS),
            format!("Expand {CEILING_NEIGHBOURHOOD_HOPS} hops")
        );
    }

    /// The id is encoded and both caps reach the wire, so the response's applied values can be
    /// compared against what was asked for.
    #[test]
    fn the_expansion_url_encodes_its_id_and_states_both_caps() {
        assert_eq!(
            neighbourhood_url("GB-COH:X6999001", 2, 250),
            "/api/entity/neighbourhood?id=GB-COH%3AX6999001&hops=2&node_cap=250"
        );
        // An id with a space and a comma — the `LOC:` namespace's shape — survives intact.
        assert!(
            neighbourhood_url("LOC:AB101JB:7 High Street, Anytown", 1, 10)
                .contains("LOC%3AAB101JB%3A7%20High%20Street%2C%20Anytown")
        );
    }

    /// The client clamps against the same constant the handler clamps against, so the URL never
    /// carries a hop count the server would silently rewrite.
    ///
    /// PROVE THIS CAN FAIL: delete the `.clamp(1, CEILING_NEIGHBOURHOOD_HOPS)` in
    /// `neighbourhood_url` and this fails on both ends of the range.
    #[test]
    fn a_hop_count_outside_the_ceiling_never_reaches_the_wire() {
        assert!(
            neighbourhood_url("x", 40, 250).contains(&format!("hops={CEILING_NEIGHBOURHOOD_HOPS}"))
        );
        assert!(neighbourhood_url("x", 0, 250).contains("hops=1"));
        // Non-vacuity: an in-range value passes through untouched, so this is a statement about
        // clamping rather than about a function that always returns the ceiling.
        assert!(neighbourhood_url("x", 2, 250).contains("hops=2"));
    }

    /// **The seed is on screen.** `NeighbourhoodView::nodes` excludes it by the read path's own
    /// definition, so a focus set built from `nodes` alone hides the node the analyst clicked.
    ///
    /// PROVE THIS CAN FAIL: remove the `ids.push(view.seed.id.clone())` line and this fails.
    #[test]
    fn the_seed_is_visible_in_its_own_neighbourhood() {
        let v = view(4, 250);
        let visible = visible_ids(&v);
        assert_eq!(
            visible.first().map(String::as_str),
            Some(seed().id.as_str())
        );
        assert_eq!(visible.len(), v.nodes.len() + 1);
        // Non-vacuity, both arms: the walk really returned neighbours, and the seed really is not
        // among them — otherwise "the seed is visible" would be true for free.
        assert_eq!(v.nodes.len(), 4, "the fixture must have real neighbours");
        assert!(
            v.nodes.iter().all(|n| n.entity.id != seed().id),
            "the read path does not put the seed in its own neighbourhood"
        );
        for node in &v.nodes {
            assert!(visible.contains(&node.entity.id));
        }
    }

    /// A cut expansion focuses on exactly what came back — never on the total it reports.
    #[test]
    fn the_focus_set_is_the_window_that_arrived_not_the_population_it_was_cut_from() {
        let v = view(25, 10);
        assert_eq!(
            v.total_discovered, 25,
            "Non-vacuity: the cap must have bitten"
        );
        assert_eq!(visible_ids(&v).len(), 11, "ten nodes and the seed");
    }

    /// **The cap is reported when it bites and silent when it does not**, and the number it
    /// reports is the number that was actually cut.
    ///
    /// A test asserting only "a cut is reported" passes for a composer that reports a cut every
    /// time, which is the same defect one bit over: an analyst who is warned about every
    /// neighbourhood learns nothing from being warned about this one. So both sides are asserted,
    /// with the population counted on each.
    ///
    /// PROVE THIS CAN FAIL: three separate mutations redden this, and all three were run —
    /// (a) drop the `if view.truncation.truncated` gate and always append the clause;
    /// (b) invert the gate; (c) print `shown` where `cut` is printed.
    #[test]
    fn the_node_cap_is_reported_when_it_bites_and_stays_quiet_when_it_does_not() {
        let cut = view(1_842, 250);
        // Non-vacuity, the biting arm: the cap really did bite and really did remove 1,592.
        assert_eq!(cut.total_discovered, 1_842);
        assert_eq!(cut.nodes.len(), 250);
        assert!(cut.truncation.truncated);
        let line = expansion_status_line(&cut);
        assert_eq!(
            line,
            "Showing 250 of 1,842 nodes within 1 hop \u{2014} 1,592 not shown (node cap 250)."
        );
        assert!(
            line.contains("1,592"),
            "the number cut must be named: {line}"
        );

        let whole = view(40, 250);
        // Non-vacuity, the quiet arm: this neighbourhood is genuinely populated and genuinely
        // whole, so silence here is a statement rather than an empty case.
        assert_eq!(whole.total_discovered, 40);
        assert_eq!(whole.nodes.len(), 40);
        assert!(!whole.truncation.truncated);
        let line = expansion_status_line(&whole);
        assert_eq!(line, "Showing 40 of 40 nodes within 1 hop (node cap 250).");
        assert!(
            !line.contains("not shown"),
            "an uncut neighbourhood must not claim a cut: {line}"
        );
        // The cap is still stated. "The cap was 250 and 40 came back" is a different statement
        // from "40 is all there is", and the analyst deciding whether to raise it needs both.
        assert!(line.contains("node cap 250"));
    }

    /// The reported figure is the arithmetic of the two counts on screen, checked against the
    /// server's own report of the same cut, across every cap/population pair.
    ///
    /// The UI derives the number (D-12: `Truncation::detail` is the server's statement, never the
    /// rendered line) but the *gate* is the server's flag. This is the test that the two can never
    /// disagree — a composer that decided for itself whether a cut had happened is the failure.
    ///
    /// PROVE THIS CAN FAIL: gate the clause on `shown < total` computed here instead of on
    /// `view.truncation.truncated`, then make `NeighbourhoodView::cut` report `truncated: false`
    /// for a cut list; the two disagree and this fails. Both halves were run.
    #[test]
    fn the_cap_clause_and_the_servers_own_report_never_disagree() {
        let mut biting = 0;
        let mut quiet = 0;
        for population in [0u32, 1, 5, 10, 11, 100, 251] {
            for cap in [1u32, 5, 10, 250] {
                let v = view(population, cap);
                let line = expansion_status_line(&v);
                let cut = v.total_discovered - v.nodes.len() as u64;
                if v.truncation.truncated {
                    biting += 1;
                    assert!(cut > 0, "the server reported a cut of nothing");
                    assert!(
                        line.contains(&format!("{} not shown", format_thousands(cut))),
                        "population {population} cap {cap}: the line must name the {cut} cut: \
                         {line}"
                    );
                    // And the server's own detail names the same number, from the same two counts.
                    assert!(
                        v.truncation.detail.contains(&cut.to_string()),
                        "the server's detail and the rendered line must name one number: {}",
                        v.truncation.detail
                    );
                } else {
                    quiet += 1;
                    assert_eq!(cut, 0, "nothing was reported, so nothing may be missing");
                    assert!(
                        !line.contains("not shown"),
                        "population {population} cap {cap}: {line}"
                    );
                }
            }
        }
        // Non-vacuity on BOTH arms: a sweep that only ever cut, or only ever fitted, would prove
        // half of what this test claims.
        assert!(biting >= 6, "only {biting} of the cases hit the cap");
        assert!(quiet >= 6, "only {quiet} of the cases fitted inside it");
    }

    /// An empty neighbourhood says so, rather than rendering "Showing 0 of 0 nodes" — which reads
    /// as a rendering fault, not as an answer.
    ///
    /// PROVE THIS CAN FAIL: delete the `total == 0` early return and this fails.
    #[test]
    fn an_empty_neighbourhood_is_an_answer_rather_than_two_zeroes() {
        let v = view(0, 250);
        assert_eq!(v.total_discovered, 0, "Non-vacuity: the fixture is empty");
        let line = expansion_status_line(&v);
        assert!(!line.contains("0 of 0"), "{line}");
        assert!(line.contains("No node lies within 1 hop"), "{line}");
        assert!(line.contains("node cap 250"), "{line}");
    }

    /// Every number in the line agrees with the noun beside it, at one and at many.
    ///
    /// PROVE THIS CAN FAIL: make `node_noun` agree with the window instead of the total (the exact
    /// error 45-10 shipped in the provenance marker), or drop `hop_phrase`'s singular arm.
    #[test]
    fn the_status_line_reads_correctly_at_one_and_at_many() {
        let one = NeighbourhoodView::cut(vec![seed()], 1, 250, vec![node(0)]);
        assert_eq!(
            expansion_status_line(&one),
            "Showing 1 of 1 node within 1 hop (node cap 250)."
        );
        let two_hops = NeighbourhoodView::cut(vec![seed()], 2, 250, (0..2).map(node).collect());
        assert_eq!(
            expansion_status_line(&two_hops),
            "Showing 2 of 2 nodes within 2 hops (node cap 250)."
        );
        // The window is 1 while the total is 5: the noun follows the total, which is the number it
        // sits beside.
        let windowed = view(5, 1);
        assert!(
            expansion_status_line(&windowed).contains("1 of 5 nodes"),
            "{}",
            expansion_status_line(&windowed)
        );
    }

    /// **CNV-02: an edge on the canvas states its type and its date range**, in 45-12's words
    /// rather than a second set.
    ///
    /// The words are not paraphrased here — `range_text` is called, so `Undated`, the
    /// ten-character date and the `2019-04-01 →` open-ended form all come from the profile
    /// screen's own function. A canvas that spelled an open-ended range `→ present` while the
    /// panel above it said `2019-04-01 →` would be two vocabularies on one screen, and the first
    /// of those spellings asserts something no dataset in this store said.
    ///
    /// PROVE THIS CAN FAIL: inline `format!("{from} → present")` in `edge_line` instead of calling
    /// `range_text`, and both the open-ended and the undated assertions fail.
    #[test]
    fn an_edge_states_its_type_and_its_range_in_the_profile_screens_own_words() {
        assert_eq!(
            edge_line(&undated_edge("Directs")),
            "Directs \u{2014} Undated"
        );
        assert_eq!(
            edge_line(&dated_edge("OwnsProperty", "2019-04-01T00:00:00Z")),
            "OwnsProperty \u{2014} 2019-04-01 \u{2192}"
        );
        // The same range through `range_text` directly — so this is a statement about REUSE, not
        // about two functions that happen to agree today.
        let edge = dated_edge("Controls", "2021-06-30");
        assert!(edge_line(&edge).ends_with(&range_text(&edge.temporal)));
        // And an open-ended range is never reported as ongoing: `effective_to` is null on every
        // edge in this store, so "present" would be a claim no record makes.
        assert!(!edge_line(&edge).contains("present"));
    }

    /// A set of edges none of which records a range says so, with the fields it consulted — and a
    /// set with even one dated edge does not, because the lines themselves are then legible.
    ///
    /// PROVE THIS CAN FAIL: drop the `.any(|e| !e.temporal.range.is_unrecorded())` guard and the
    /// mixed-set assertion fails; drop the `dedup` and the one-basis assertion fails.
    #[test]
    fn a_set_of_edges_with_no_dates_at_all_says_so_and_names_what_it_read() {
        let all_undated = vec![undated_edge("Directs"), undated_edge("Directs")];
        let note = undated_edges_note(&all_undated).expect("an all-undated set is noted");
        assert!(
            note.starts_with("2 edges here record no date range."),
            "{note}"
        );
        assert!(note.contains("Directs.meta.effective_from"), "{note}");
        // One shared basis is stated once, not twice — "this dataset records no dates" is one fact.
        assert_eq!(
            note.matches("Directs.meta.effective_from").count(),
            1,
            "{note}"
        );

        // n = 1 agrees with itself.
        let one = vec![undated_edge("LocatedAt")];
        assert!(undated_edges_note(&one)
            .expect("one undated edge is still noted")
            .starts_with("1 edge here records no date range."));

        // A mixed set is not noted: the analyst can see which is which from the lines.
        let mixed = vec![
            undated_edge("Directs"),
            dated_edge("Controls", "2021-06-30"),
        ];
        assert_eq!(undated_edges_note(&mixed), None);
        // Non-vacuity: that same set really does contain an undated edge, so the `None` is about
        // the mix rather than about there being nothing undated to report.
        assert!(mixed[0].temporal.range.is_unrecorded());
        assert_eq!(undated_edges_note(&[]), None);

        // The note makes no claim about a filter — the canvas runs none. 45-12 settled that an
        // unfiltered screen states no as-at rule; this is that decision one surface over.
        assert!(!note.contains("filter"), "{note}");
        assert!(
            !note.contains(urban_integrity_workbench_dto::temporal::NULL_RANGE_RULE),
            "{note}"
        );
    }

    /// The panel's "how did the walk get here" line, and the seed's deliberate absence from it.
    ///
    /// PROVE THIS CAN FAIL: make `reached_node` search `nodes` plus the seed and the seed
    /// assertion fails.
    #[test]
    fn only_a_node_the_walk_reached_has_something_to_say_about_how() {
        let v = view(3, 250);
        let found = reached_node(&v, "contract:X9999-c0001").expect("a reached node");
        assert_eq!(
            reached_from_line(found),
            format!("Reached from {} at hop 1.", seed().id)
        );
        assert_eq!(found.edge_types(), vec!["AwardedTo".to_string()]);
        // The seed was not reached by anything, so there is no edge to describe.
        assert!(reached_node(&v, &seed().id).is_none());
        // Nor is a node outside this expansion given one.
        assert!(reached_node(&v, "ward:X05999001").is_none());
        // Non-vacuity: the lookup does find things, so the two `None`s are about the subjects.
        assert_eq!(v.nodes.len(), 3);
    }

    /// A cluster large enough to collapse, hung off one parent, plus a handful hung off another.
    fn clustered_view(big: u32, small: u32) -> NeighbourhoodView {
        clustered_view_capped(big, small, DEFAULT_NEIGHBOURHOOD_NODE_CAP)
    }

    /// The same shape with the node cap raised, for the cases that only exist above the default —
    /// a cluster larger than the default cap is reachable only when the operator raises it.
    fn clustered_view_capped(big: u32, small: u32, cap: u32) -> NeighbourhoodView {
        let mut nodes: Vec<NeighbourhoodNodeView> = (0..big)
            .map(|i| NeighbourhoodNodeView {
                entity: EntityRef {
                    id: format!("contract:X9999-c{i:04}"),
                    name: format!("Contract {i}"),
                    kind: "Contract".to_string(),
                },
                hop: 1,
                edges: vec![undated_edge("AwardedTo")],
                reached_from: seed().id,
            })
            .collect();
        nodes.extend((0..small).map(|i| NeighbourhoodNodeView {
            entity: EntityRef {
                id: format!("OFF:X9999-o{i:04}"),
                name: format!("Officer {i}"),
                kind: "Officer".to_string(),
            },
            hop: 2,
            edges: vec![undated_edge("Directs")],
            reached_from: "contract:X9999-c0000".to_string(),
        }));
        NeighbourhoodView::cut(vec![seed()], 2, cap, nodes)
    }

    /// Only a cluster at or above the threshold is offered for collapse, and nothing collapses
    /// unless it is asked to.
    ///
    /// PROVE THIS CAN FAIL: change `>=` to `>` in `clusters`'s filter and the boundary case fails;
    /// drop the filter entirely and the small cluster is offered.
    #[test]
    fn only_a_cluster_big_enough_to_hide_the_rest_is_offered_for_collapse() {
        let threshold = CLUSTER_COLLAPSE_THRESHOLD as usize;
        let v = clustered_view(CLUSTER_COLLAPSE_THRESHOLD, 3);
        let found = clusters(&v);
        assert_eq!(found.len(), 1, "one cluster reaches the threshold");
        assert_eq!(found[0].parent_id, seed().id);
        assert_eq!(found[0].member_ids.len(), threshold);

        // Non-vacuity: the three-node cluster is genuinely in the expansion and genuinely not
        // offered, so "one cluster" is a statement about the threshold rather than about the
        // fixture having only one parent.
        assert!(v
            .nodes
            .iter()
            .any(|n| n.reached_from == "contract:X9999-c0000"));

        // One below the threshold: nothing is offered at all.
        let smaller = clustered_view(CLUSTER_COLLAPSE_THRESHOLD - 1, 3);
        assert!(clusters(&smaller).is_empty());
        assert!(
            smaller.nodes.len() >= threshold,
            "Non-vacuity: the expansion is still big; it is the CLUSTER that is not"
        );

        // And nothing is folded away until a parent is named.
        assert_eq!(
            visible_ids_with_collapsed(&v, &[]),
            visible_ids(&v),
            "an expansion with a collapsible cluster still shows every node until asked"
        );
    }

    /// **A collapsed glyph says how many nodes it stands for, and re-expanding restores exactly
    /// those.**
    ///
    /// The count is the substance: a glyph that folded nodes away without naming them would be a
    /// silently shorter graph with a different mechanism, which is the defect CNV-02 exists to
    /// prevent. The round trip is the other half — a collapse that restored a *different* set, or
    /// a superset, would be a graph that changed while the analyst was reading it.
    ///
    /// PROVE THIS CAN FAIL: make `glyph_label` print the threshold instead of `member_ids.len()`
    /// and the count assertion fails; make `visible_ids_with_collapsed` also drop the parent and
    /// the glyph disappears with its own cluster; make it fold every cluster rather than the named
    /// ones and the untouched-cluster assertion fails. All three were run.
    #[test]
    fn a_collapsed_glyph_says_how_many_nodes_it_stands_for_and_re_expanding_restores_them() {
        let v = clustered_view(40, 30);
        let found = clusters(&v);
        assert_eq!(
            found.len(),
            2,
            "Non-vacuity: two clusters, so one can be left alone"
        );
        let big = found
            .iter()
            .find(|c| c.parent_id == seed().id)
            .expect("the seed's own cluster");
        let other = found
            .iter()
            .find(|c| c.parent_id == "contract:X9999-c0000")
            .expect("the second cluster");

        // The label names the population it stands for — read from the members, not from a cap.
        assert_eq!(
            glyph_label(big),
            format!("40 nodes collapsed under {}", seed().id)
        );
        assert_eq!(big.member_ids.len(), 40);
        assert_eq!(cluster_toggle_label(big, false), "Collapse 40");
        assert_eq!(cluster_toggle_label(big, true), "Show 40 again");
        assert_eq!(
            cluster_line(big, false),
            format!("40 nodes under {}", seed().id)
        );

        let before = visible_ids(&v);
        let collapsed = vec![seed().id];
        let after = visible_ids_with_collapsed(&v, &collapsed);

        // Exactly the glyph's own members left, and nothing else.
        let gone: Vec<&String> = before.iter().filter(|id| !after.contains(id)).collect();
        assert_eq!(gone.len(), big.member_ids.len());
        for id in &big.member_ids {
            assert!(gone.contains(&id), "{id} must be folded under its glyph");
        }
        // The glyph itself stays — it is a point on the canvas, not a gap.
        assert!(after.contains(&seed().id));
        // And the cluster nobody collapsed is untouched.
        for id in &other.member_ids {
            assert!(
                after.contains(id),
                "{id} belongs to a cluster nobody collapsed"
            );
        }

        // The round trip: re-expanding restores exactly the set that was there before, in order.
        assert_eq!(visible_ids_with_collapsed(&v, &[]), before);

        // Collapsing both folds both, and the seed still stands.
        let both = visible_ids_with_collapsed(&v, &[seed().id, "contract:X9999-c0000".to_string()]);
        assert_eq!(both, vec![seed().id]);
    }

    /// A glyph is drawn large in proportion to what it stands for, on the canvas's existing 1x-3x
    /// scale, and only for the clusters actually collapsed.
    ///
    /// PROVE THIS CAN FAIL: drop the `.clamp(1.0, 3.0)` and a cluster larger than the default cap
    /// scales past 3x; drop the `filter` and an un-collapsed cluster's parent is resized anyway.
    #[test]
    fn a_glyph_is_sized_by_the_population_it_stands_for_and_only_when_collapsed() {
        let v = clustered_view(40, 30);
        assert!(
            glyph_sizes(&v, &[]).is_empty(),
            "nothing collapsed, nothing resized"
        );

        let sizes = glyph_sizes(&v, &[seed().id]);
        assert_eq!(sizes.len(), 1, "only the collapsed cluster's parent");
        assert_eq!(sizes[0].0, seed().id);
        assert!(
            sizes[0].1 > 1.0 && sizes[0].1 <= 3.0,
            "a glyph reads larger than one point and never past the canvas's ceiling: {}",
            sizes[0].1
        );

        // A bigger cluster reads bigger — the scale is about the population, not a flag.
        let bigger = clustered_view(200, 30);
        let bigger_size = glyph_sizes(&bigger, &[seed().id])[0].1;
        assert!(bigger_size > sizes[0].1, "{bigger_size} vs {}", sizes[0].1);
        assert!(bigger_size <= 3.0);

        // **The clamp, exercised where it actually bites.** A first version of this test stopped
        // at 200 members and asserted `<= 3.0`, which `1.0 + 2.0 * share` satisfies on its own for
        // every cluster below the default cap — so removing the clamp left it green. A cluster
        // larger than the default cap is reachable the moment the operator raises the cap toward
        // `CEILING_NEIGHBOURHOOD_NODE_CAP` (2,000), and an unclamped glyph would scale past the
        // canvas's own ceiling.
        let over_cap = clustered_view_capped(
            DEFAULT_NEIGHBOURHOOD_NODE_CAP + 150,
            30,
            urban_integrity_workbench_dto::caps::CEILING_NEIGHBOURHOOD_NODE_CAP,
        );
        assert!(
            over_cap.nodes.len() > DEFAULT_NEIGHBOURHOOD_NODE_CAP as usize,
            "Non-vacuity: this fixture must actually exceed the default cap"
        );
        let over_cap_size = glyph_sizes(&over_cap, &[seed().id])[0].1;
        assert_eq!(
            over_cap_size, 3.0,
            "a cluster larger than the cap tops out at the canvas's own ceiling"
        );
    }

    #[test]
    fn the_fetch_error_copy_names_the_endpoint_it_is_about() {
        assert!(NEIGHBOURHOOD_FETCH_ERROR_COPY.contains(NEIGHBOURHOOD_URL_PATH));
    }
}
