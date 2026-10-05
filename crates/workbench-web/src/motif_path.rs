//! `motif_path.rs` — the general induced-subgraph motif renderer (D-01/D-02/D-03, the phase 46 UI spec
//! "Motif-path renderer"). `MotifStar` (`crate::motif_star`) is hard-coded to exactly one
//! centre-plus-leaves shape and `MotifTypedEdge` hard-codes exactly two roles; neither can draw an
//! arbitrary participant set with arbitrary typed edges (phase 46 research Q3). This is genuinely
//! new layout, following `motif_star.rs`'s own **pure-fn-then-thin-wrapper** split (see that
//! file's top-of-file doc comment, itself following `verdict_badge.rs`'s rationale): every piece
//! of geometry that decides WHAT gets drawn is a plain function taking owned/borrowed data and
//! returning owned values, fully host-testable without a DOM. The `#[component]` itself does
//! nothing but call those functions and interpolate their output into `view!`.
//!
//! **Reuse, not reimplementation.** `motif_star.rs`'s geometry PRIMITIVES —
//! [`crate::motif_star::star_leaf_positions`], [`crate::motif_star::truncate_label`],
//! [`crate::motif_star::node_label_anchor`], [`crate::motif_star::LABEL_MAX_CHARS`],
//! [`crate::motif_star::LABELLED_STAR_RADIUS`], [`crate::motif_star::LABEL_FONT`] — are imported
//! and called directly below. This file writes new LAYOUT (node selection order, per-`n`
//! placement including the hub case, parallel-edge families) on top of those primitives; it does
//! not redefine any of them.
//!
//! Mounted by the alert row (`46-08`, `size = 160, labelled = false`,
//! `caps::ALERT_PATH_NODE_BUDGET_ROW = 8`) at `crate::alerts::row`, and by the `/alert/:id` detail
//! route (`46-10`, `size = DETAIL_MOTIF_SIZE = 800, labelled = true`,
//! `caps::ALERT_PATH_NODE_BUDGET_DETAIL = 20`) at `crate::alert_detail_page`. Both of those plans
//! have since run, so this module's earlier `#![allow(dead_code)]` — and its note that `MotifPath`
//! had "zero call sites right now, by design" — were both retired once the mounts landed
//! (phase 46 code review finding WR-02). The reachability sweeps in `tests/abbr_reachable.rs` and
//! `tests/pivot_reachable.rs` are what keep that claim honest rather than this comment.

use std::collections::HashMap;

use leptos::prelude::*;
use urban_integrity_workbench_dto::{InducedSubgraphView, PathEdgeView, PathNodeView};

use crate::edge_palette::edge_type_color;
use crate::entity_ref_view::EntityRefView;
use crate::motif_star::{
    node_label_anchor, star_leaf_positions, truncate_label, LABELLED_STAR_RADIUS, LABEL_FONT,
    LABEL_MAX_CHARS,
};
use crate::node_palette::node_kind_color;

/// Perpendicular displacement between adjacent parallel-edge families, in the 200-unit design
/// space (the phase 46 UI spec's SVG geometry table — the one **new** constant in that table, more
/// than twice the 1.5 stroke width so two families of the same pair never touch at either mount).
pub const PARALLEL_EDGE_OFFSET: f64 = 4.0;

/// The maximum number of distinct edge-type families drawn as separate parallel lines between one
/// node pair. [`edge_families`] itself never drops anything past this — it returns every distinct
/// family it finds — so the true count is always available to a caller's "N more" line; this
/// constant only bounds how many of them [`parallel_edge_offset`] assigns a drawable slot.
pub const MAX_DRAWN_PARALLEL_FAMILIES: usize = 3;

/// **Node selection and ordering — total and deterministic, so two renders of the same induced
/// subgraph draw the same picture** (the phase 46 UI spec's motif-path renderer contract). Returns
/// INDICES into `nodes`, sorted by `degree_in_subgraph` descending, ties broken by `entity.id`
/// ascending. Degree-descending alone is not a total order (two nodes can tie); the id tie-break
/// is what makes the golden test's enumerated `(cx, cy)` pairs assertable at all.
pub fn node_selection_order(nodes: &[PathNodeView]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|&a, &b| {
        nodes[b]
            .degree_in_subgraph
            .cmp(&nodes[a].degree_in_subgraph)
            .then_with(|| nodes[a].entity.id.cmp(&nodes[b].entity.id))
    });
    order
}

/// Layout, by node count `n` (after budgeting) in the 200-unit design space, centred on `(100,
/// 100)`. Returns one position per node index `0..n`, in the SAME order the caller's node slice
/// is in (the caller is expected to have already applied [`node_selection_order`]).
///
/// - `n == 0` — empty (nothing to draw).
/// - `n == 1` — a single centred position; a 1-node case is a chip, not a star, so no
///   radius/angle computation happens at all.
/// - `n == 2` — the exact literal pair from the phase 46 UI spec's geometry table, NOT a
///   radius-derived formula: unlabelled `(30, 100)`/`(170, 100)` (±70, the unlabelled ring
///   radius), labelled `(60, 100)`/`(140, 100)` (±[`LABELLED_STAR_RADIUS`]). The radius is
///   conditional on `labelled` exactly as `motif_star.rs:145` makes it — hardcoding ±70 at a
///   labelled mount clips the right-hand label (see this plan's own `<interfaces>` derivation).
/// - `n >= 3`, `hub_index == Some(h)` (`h < n`) — `nodes[h]` at the centre; the remaining `n - 1`
///   positions come from [`star_leaf_positions`]`(n - 1, radius, 100.0, 100.0)`, assigned to the
///   non-hub indices in ascending order — reusing the SAME primitive the ring case below uses,
///   just reserving a centre slot first.
/// - `n >= 3`, `hub_index == None` (or an out-of-range `Some`, defensively) — all `n` positions
///   come directly from [`star_leaf_positions`]`(n, radius, 100.0, 100.0)`.
pub fn layout_positions(n: usize, labelled: bool, hub_index: Option<usize>) -> Vec<(f64, f64)> {
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![(100.0, 100.0)];
    }
    if n == 2 {
        return if labelled {
            vec![(60.0, 100.0), (140.0, 100.0)]
        } else {
            vec![(30.0, 100.0), (170.0, 100.0)]
        };
    }
    let radius = if labelled { LABELLED_STAR_RADIUS } else { 70.0 };
    match hub_index {
        Some(h) if h < n => {
            let leaves = star_leaf_positions(n - 1, radius, 100.0, 100.0);
            let mut leaf_iter = leaves.into_iter();
            (0..n)
                .map(|i| {
                    if i == h {
                        (100.0, 100.0)
                    } else {
                        leaf_iter
                            .next()
                            .expect("leaf positions count must equal n - 1 non-hub nodes")
                    }
                })
                .collect()
        }
        _ => star_leaf_positions(n, radius, 100.0, 100.0),
    }
}

/// A node index is a hub iff it has an edge (in either direction) to every OTHER node in the
/// slice. `nodes` and `edges` are the ALREADY-budgeted, ALREADY-ordered slices (the caller has
/// applied [`node_selection_order`] first) — returns the FIRST such index, by that already-
/// established order, or `None` when no node qualifies (including `n < 3`, where "hub versus
/// ring" is not a meaningful distinction).
pub fn find_hub_index(nodes: &[PathNodeView], edges: &[PathEdgeView]) -> Option<usize> {
    let n = nodes.len();
    if n < 3 {
        return None;
    }
    (0..n).find(|&i| {
        let id = &nodes[i].entity.id;
        (0..n).filter(|&j| j != i).all(|j| {
            let other = &nodes[j].entity.id;
            edges.iter().any(|e| {
                (&e.from_id == id && &e.to_id == other) || (&e.to_id == id && &e.from_id == other)
            })
        })
    })
}

/// Groups edges between the same UNDIRECTED node pair by distinct `edge_type`, in first-seen
/// order. Returns the FULL family list per pair — never capped here — so the true family count
/// is always available to a caller's "N more" line; [`parallel_edge_offset`] is what bounds how
/// many of them are actually assigned a drawable offset.
pub fn edge_families(edges: &[PathEdgeView]) -> HashMap<(String, String), Vec<String>> {
    let mut map: HashMap<(String, String), Vec<String>> = HashMap::new();
    for edge in edges {
        let key = if edge.from_id <= edge.to_id {
            (edge.from_id.clone(), edge.to_id.clone())
        } else {
            (edge.to_id.clone(), edge.from_id.clone())
        };
        let members = map.entry(key).or_default();
        if !members.contains(&edge.edge_type) {
            members.push(edge.edge_type.clone());
        }
    }
    map
}

/// Perpendicular displacement, in design-space units, for the `family_index`-th (0-based) of
/// `family_count` distinct edge-type families drawn between the same node pair. `0.0` when
/// `family_count <= 1` (nothing parallel to offset). Otherwise spaces
/// `min(family_count, `[`MAX_DRAWN_PARALLEL_FAMILIES`]`)` families symmetrically around the direct
/// line at multiples of [`PARALLEL_EDGE_OFFSET`] — 2 families: `-4.0, +4.0`; 3 (or more, capped):
/// `-4.0, 0.0, +4.0`. A `family_index` past the cap is clamped to the last drawable slot, so a
/// caller that (incorrectly) asked for an offset beyond the cap never panics — the cap itself is
/// enforced by the caller choosing not to draw families past index
/// [`MAX_DRAWN_PARALLEL_FAMILIES`]`- 1` in the first place, per the caps-reported discipline: the
/// true count comes from [`edge_families`]'s uncapped `Vec` length, never from this function.
pub fn parallel_edge_offset(family_index: usize, family_count: usize) -> f64 {
    let drawn = family_count.min(MAX_DRAWN_PARALLEL_FAMILIES);
    if drawn <= 1 {
        return 0.0;
    }
    let i = family_index.min(drawn - 1);
    match drawn {
        2 => {
            if i == 0 {
                -PARALLEL_EDGE_OFFSET
            } else {
                PARALLEL_EDGE_OFFSET
            }
        }
        _ => (i as f64 - 1.0) * PARALLEL_EDGE_OFFSET,
    }
}

/// Shifts the line `(x1, y1)-(x2, y2)` sideways by `offset` design-space units, perpendicular to
/// its own direction — used to draw one member of a parallel-edge family without moving the
/// other. Both endpoints shift by the SAME amount, so the drawn segment stays parallel to (and
/// the same length as) the direct line between the two nodes. `offset == 0.0` (or a
/// zero-length line, defensively) returns the input unchanged.
fn perpendicular_offset(x1: f64, y1: f64, x2: f64, y2: f64, offset: f64) -> (f64, f64, f64, f64) {
    if offset == 0.0 {
        return (x1, y1, x2, y2);
    }
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < f64::EPSILON {
        return (x1, y1, x2, y2);
    }
    let (px, py) = (-dy / len, dx / len);
    (
        x1 + px * offset,
        y1 + py * offset,
        x2 + px * offset,
        y2 + py * offset,
    )
}

/// The general induced-subgraph motif renderer (D-01/D-02/D-03). Dispatches on
/// [`InducedSubgraphView`]'s three variants:
///
/// - [`InducedSubgraphView::SingleParticipant`] — a single [`EntityRefView`] chip, centred, NO
///   `<svg>` at all (`MotifKind::SingleSubject`, A-05 — no path is ever drawable for these).
/// - [`InducedSubgraphView::NoEdges`] — the dashed-border absence slot with the short "No
///   connecting edges" Label placeholder (D-02; the FULL claim line with the snapshot timestamp
///   is the row/detail caller's job, not this component's).
/// - [`InducedSubgraphView::Drawable`] — the drawn subgraph: node selection order
///   ([`node_selection_order`]), hub detection ([`find_hub_index`]), per-`n` layout
///   ([`layout_positions`]) and parallel-edge families ([`edge_families`],
///   [`parallel_edge_offset`]) from this module, every coordinate scaled by `size / 200.0` —
///   `motif_star.rs:142-150`'s own idiom, never a second scaling convention.
///
/// `labelled` controls node-label drawing independently of `size`, mirroring `MotifStar`'s own
/// prop: the row mount (46-08) passes `labelled = false` (`size = 160`), the detail mount (46-10)
/// passes `labelled = true` (`size = 800`). **No node labels at row scale** — the caller's job to
/// pass the right combination, per the phase 46 UI spec; every `<text>` this component ever emits
/// carries `aria-hidden="true"`, because the accessible pivot surface is the Participants list
/// the caller renders separately, not SVG text.
#[component]
#[allow(non_snake_case)]
pub fn MotifPath(
    path: InducedSubgraphView,
    #[prop(default = 200.0)] size: f64,
    #[prop(default = false)] labelled: bool,
) -> impl IntoView {
    match path {
        InducedSubgraphView::SingleParticipant { entity } => view! {
            <div style="display: flex; align-items: center; justify-content: center; min-height: 44px;">
                <EntityRefView entity=entity />
            </div>
        }
        .into_any(),
        InducedSubgraphView::NoEdges { .. } => view! {
            <div style=format!(
                "display: flex; align-items: center; justify-content: center; width: {size}px; \
                 max-width: 100%; aspect-ratio: 1 / 1; border: 1px dashed var(--border);"
            )>
                <span class="text-label" style="color: var(--muted-foreground);">
                    "No connecting edges"
                </span>
            </div>
        }
        .into_any(),
        InducedSubgraphView::Drawable { nodes, edges, .. } => {
            render_drawable(nodes, edges, size, labelled).into_any()
        }
    }
}

/// The pure-data half of the `Drawable` branch: computes every circle/line/label as plain data
/// (never a `view!` fragment) before the final `view!` call at the bottom interpolates them —
/// the same split `motif_star.rs`'s own `MotifStar` body follows.
fn render_drawable(
    nodes: Vec<PathNodeView>,
    edges: Vec<PathEdgeView>,
    size: f64,
    labelled: bool,
) -> impl IntoView {
    let scale = size / 200.0;

    // Node selection order (Task 1) applied to the already-server-budgeted node slice, so two
    // renders of the same data draw the same picture.
    let order = node_selection_order(&nodes);
    let ordered: Vec<PathNodeView> = order.into_iter().map(|i| nodes[i].clone()).collect();
    let n = ordered.len();
    let hub_index = find_hub_index(&ordered, &edges);

    let positions: Vec<(f64, f64)> = layout_positions(n, labelled, hub_index)
        .into_iter()
        .map(|(x, y)| (x * scale, y * scale))
        .collect();

    let index_by_id: HashMap<&str, usize> = ordered
        .iter()
        .enumerate()
        .map(|(i, node)| (node.entity.id.as_str(), i))
        .collect();
    let families = edge_families(&edges);

    let hub_r = 10.0 * scale;
    let ring_r = 6.0 * scale;
    let edge_width = 1.5 * scale;
    let label_font = LABEL_FONT * scale;
    let cx = 100.0 * scale;
    let cy = 100.0 * scale;
    let label_style = format!(
        "font-size: {label_font}px; fill: var(--foreground); paint-order: stroke; \
         stroke: var(--card-hover); stroke-width: {}px; stroke-linejoin: round;",
        3.0 * scale
    );

    // Edge lines: at most MAX_DRAWN_PARALLEL_FAMILIES per pair, parallel-offset apart; the
    // TRUE family count (edge_families' uncapped Vec length) is the caller's business, never
    // this renderer's — this loop only decides what actually gets drawn.
    let edge_lines: Vec<(f64, f64, f64, f64, &'static str)> = edges
        .iter()
        .filter_map(|edge| {
            let &i = index_by_id.get(edge.from_id.as_str())?;
            let &j = index_by_id.get(edge.to_id.as_str())?;
            let key = if edge.from_id <= edge.to_id {
                (edge.from_id.clone(), edge.to_id.clone())
            } else {
                (edge.to_id.clone(), edge.from_id.clone())
            };
            let members = families.get(&key)?;
            let family_index = members.iter().position(|t| t == &edge.edge_type)?;
            if family_index >= MAX_DRAWN_PARALLEL_FAMILIES {
                return None;
            }
            let (x1, y1) = positions[i];
            let (x2, y2) = positions[j];
            let offset = parallel_edge_offset(family_index, members.len()) * scale;
            let (ox1, oy1, ox2, oy2) = perpendicular_offset(x1, y1, x2, y2, offset);
            Some((ox1, oy1, ox2, oy2, edge_type_color(&edge.edge_type)))
        })
        .collect();

    // Node circles: hub radius for the hub (if any), ring radius otherwise.
    let node_circles: Vec<(f64, f64, f64, &'static str)> = ordered
        .iter()
        .enumerate()
        .map(|(i, node_view)| {
            let (x, y) = positions[i];
            let r = if hub_index == Some(i) { hub_r } else { ring_r };
            (x, y, r, node_kind_color(&node_view.entity.kind))
        })
        .collect();

    // Node labels: only when `labelled`, reusing motif_star.rs's own primitives verbatim.
    let node_labels: Vec<(f64, f64, &'static str, String, String)> = if labelled {
        ordered
            .iter()
            .enumerate()
            .map(|(i, node_view)| {
                let (x, y) = positions[i];
                let r = if hub_index == Some(i) { hub_r } else { ring_r };
                let gap = r + 3.0 * scale;
                let (lx, ly, anchor) = node_label_anchor(x, y, cx, cy, gap);
                (
                    lx,
                    ly,
                    anchor,
                    truncate_label(&node_view.entity.name, LABEL_MAX_CHARS),
                    node_view.entity.name.clone(),
                )
            })
            .collect()
    } else {
        Vec::new()
    };

    view! {
        <svg
            viewBox=format!("0 0 {size} {size}")
            width="100%"
            height=size.to_string()
            style="max-width: 100%; max-height: 100%;"
        >
            {edge_lines.into_iter().map(|(x1, y1, x2, y2, color)| view! {
                <line
                    x1=x1.to_string()
                    y1=y1.to_string()
                    x2=x2.to_string()
                    y2=y2.to_string()
                    stroke=format!("var({color})")
                    stroke-width=edge_width.to_string()
                />
            }).collect_view()}
            {node_circles.into_iter().map(|(x, y, r, color)| view! {
                <circle cx=x.to_string() cy=y.to_string() r=r.to_string() fill=format!("var({color})") />
            }).collect_view()}
            {node_labels.into_iter().map(|(lx, ly, anchor, short, full)| view! {
                <text
                    x=lx.to_string()
                    y=ly.to_string()
                    text-anchor=anchor
                    dominant-baseline="middle"
                    aria-hidden="true"
                    style=label_style.clone()
                >
                    <title>{full}</title>
                    {short}
                </text>
            }).collect_view()}
        </svg>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::EntityRef;

    const TOLERANCE: f64 = 1e-6;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < TOLERANCE
    }

    fn node(id: &str, degree: u32) -> PathNodeView {
        PathNodeView {
            entity: EntityRef {
                id: id.to_string(),
                name: format!("Node {id}"),
                kind: "Entity".to_string(),
            },
            degree_in_subgraph: degree,
        }
    }

    fn edge(from: &str, to: &str, edge_type: &str) -> PathEdgeView {
        PathEdgeView {
            from_id: from.to_string(),
            to_id: to.to_string(),
            edge_type: edge_type.to_string(),
        }
    }

    /// PROVE THIS CAN FAIL: sort by `degree_in_subgraph` descending ALONE (drop the id tie-break)
    /// and Rust's stable sort keeps "b" before "c" (their construction order) instead of the
    /// required id-ascending "b" (wait — construction order here IS id-ascending for this
    /// fixture's tie, so the real failure mode this guards is a sort that is NOT stable/total: a
    /// degree-only key run through an unstable comparator can legally emit either tie order,
    /// which would make the golden `(cx, cy)` pairs this renderer depends on non-reproducible).
    #[test]
    fn node_selection_order_is_degree_desc_then_id_asc() {
        // Constructed in id order (b, a, c, d) with degrees b=1, a=3, c=3, d=0: two nodes tie at
        // the top degree (a, c) — id-ascending breaks the tie a-before-c — then degree 1 (b),
        // then degree 0 (d) follow. Total order: a, c, b, d.
        let nodes = vec![node("b", 1), node("a", 3), node("c", 3), node("d", 0)];
        let order = node_selection_order(&nodes);
        let ids: Vec<&str> = order.iter().map(|&i| nodes[i].entity.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c", "b", "d"]);
    }

    #[test]
    fn layout_positions_n1_is_a_single_centred_chip() {
        let positions = layout_positions(1, false, None);
        assert_eq!(positions, vec![(100.0, 100.0)]);
        // Also true when labelled — the 1-node case never touches the radius at all.
        assert_eq!(layout_positions(1, true, None), vec![(100.0, 100.0)]);
    }

    /// PROVE THIS CAN FAIL: derive the n=2 positions from a formula (e.g. always ±70 regardless
    /// of `labelled`) instead of the literal match — the labelled assertion below fails,
    /// reproducing the label-clipping defect the phase 46 UI spec documents (`GB-COH:PL000207`,
    /// 2026-09-18).
    #[test]
    fn layout_positions_n2_radius_is_conditional_on_labelled() {
        let unlabelled = layout_positions(2, false, None);
        assert!(approx_eq(unlabelled[0].0, 30.0) && approx_eq(unlabelled[0].1, 100.0));
        assert!(approx_eq(unlabelled[1].0, 170.0) && approx_eq(unlabelled[1].1, 100.0));

        let labelled = layout_positions(2, true, None);
        assert!(approx_eq(labelled[0].0, 60.0) && approx_eq(labelled[0].1, 100.0));
        assert!(approx_eq(labelled[1].0, 140.0) && approx_eq(labelled[1].1, 100.0));

        // The SAME n=2 case gives DIFFERENT positions depending on `labelled` alone.
        assert_ne!(unlabelled, labelled);
    }

    #[test]
    fn layout_positions_n5_with_a_hub_centres_it_and_rings_the_rest() {
        // hub at index 2; the other 4 (indices 0,1,3,4) go to the ring at star_leaf_positions(4,
        // 70.0, 100.0, 100.0) in ascending non-hub index order.
        let positions = layout_positions(5, false, Some(2));
        assert_eq!(positions.len(), 5);
        assert!(approx_eq(positions[2].0, 100.0) && approx_eq(positions[2].1, 100.0));
        let expected_ring = star_leaf_positions(4, 70.0, 100.0, 100.0);
        let non_hub: Vec<(f64, f64)> = positions
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 2)
            .map(|(_, p)| *p)
            .collect();
        assert_eq!(non_hub, expected_ring);
        // First ring point (12 o'clock) is directly above the centre.
        assert!(approx_eq(non_hub[0].0, 100.0) && approx_eq(non_hub[0].1, 30.0));
    }

    #[test]
    fn layout_positions_n5_with_no_hub_rings_all_five() {
        let positions = layout_positions(5, false, None);
        assert_eq!(positions, star_leaf_positions(5, 70.0, 100.0, 100.0));
        // 12 o'clock start, evenly spaced at 360/5 = 72 degrees.
        assert!(approx_eq(positions[0].0, 100.0) && approx_eq(positions[0].1, 30.0));
    }

    #[test]
    fn find_hub_index_finds_the_node_connected_to_every_other() {
        let nodes = vec![
            node("hub", 4),
            node("a", 1),
            node("b", 1),
            node("c", 1),
            node("d", 1),
        ];
        let edges = vec![
            edge("hub", "a", "Controls"),
            edge("hub", "b", "Controls"),
            edge("c", "hub", "Controls"),
            edge("hub", "d", "Controls"),
        ];
        assert_eq!(find_hub_index(&nodes, &edges), Some(0));

        // No universally-adjacent node -> None.
        let ring_edges = vec![
            edge("a", "b", "Controls"),
            edge("b", "c", "Controls"),
            edge("c", "d", "Controls"),
        ];
        assert_eq!(find_hub_index(&nodes, &ring_edges), None);

        // Fewer than 3 nodes is never a hub case.
        assert_eq!(find_hub_index(&nodes[..2], &edges), None);
    }

    /// PROVE THIS CAN FAIL: compute the offset as `family_index as f64 * PARALLEL_EDGE_OFFSET`
    /// (no centring) and the 2-family case yields `0.0, 4.0` instead of the required
    /// `-4.0, +4.0` — same magnitude, but not "opposite directions" from the direct line.
    #[test]
    fn parallel_edge_offset_spaces_families_symmetrically_and_reports_the_true_count() {
        let edges = vec![
            edge("a", "b", "Controls"),
            edge("a", "b", "Directs"),
            edge("a", "b", "TrusteeOf"),
            edge("a", "b", "AwardedTo"),
        ];
        let families = edge_families(&edges);
        let key = ("a".to_string(), "b".to_string());
        let members = families.get(&key).expect("one pair must be grouped");
        // The true count is 4 — never silently dropped to 3, even though only 3 will be drawn.
        assert_eq!(members.len(), 4, "the true family count must be preserved");

        // 2 families: exactly -4.0 / +4.0, opposite directions, magnitude 4.0.
        let two = ["Controls".to_string(), "Directs".to_string()];
        let offsets: Vec<f64> = (0..2).map(|i| parallel_edge_offset(i, two.len())).collect();
        assert!(approx_eq(offsets[0], -4.0) && approx_eq(offsets[1], 4.0));
        assert!(offsets[0].signum() != offsets[1].signum());

        // 4 families: drawn set caps at 3 (-4.0, 0.0, +4.0); the true count (4) still came from
        // `members.len()` above, never from this function.
        let capped_offsets: Vec<f64> = (0..members.len())
            .map(|i| parallel_edge_offset(i, members.len()))
            .collect();
        assert_eq!(
            capped_offsets.len(),
            4,
            "one offset per family index, including past the cap"
        );
        assert!(approx_eq(capped_offsets[0], -4.0));
        assert!(approx_eq(capped_offsets[1], 0.0));
        assert!(approx_eq(capped_offsets[2], 4.0));
        // The 4th (index 3, past MAX_DRAWN_PARALLEL_FAMILIES) clamps to the last drawable slot
        // rather than panicking or drifting further out.
        assert!(approx_eq(capped_offsets[3], 4.0));
    }

    #[test]
    fn edge_families_groups_by_undirected_pair_in_first_seen_order() {
        let edges = vec![
            edge("a", "b", "Controls"),
            edge("b", "a", "Directs"),
            edge("a", "b", "Controls"), // duplicate type, same pair: not a new family
        ];
        let families = edge_families(&edges);
        assert_eq!(
            families.len(),
            1,
            "a-b and b-a must collapse into one undirected pair"
        );
        let members = &families[&("a".to_string(), "b".to_string())];
        assert_eq!(
            members,
            &vec!["Controls".to_string(), "Directs".to_string()]
        );
    }

    /// PROVE THIS CAN FAIL: shift only one endpoint (a translation bug) and the segment is no
    /// longer parallel to the direct line — the length assertion below would fail.
    #[test]
    fn perpendicular_offset_shifts_both_endpoints_the_same_amount() {
        // A horizontal line: perpendicular is vertical.
        let (x1, y1, x2, y2) = perpendicular_offset(0.0, 100.0, 100.0, 100.0, 4.0);
        assert!(approx_eq(x1, 0.0) && approx_eq(y1, 104.0));
        assert!(approx_eq(x2, 100.0) && approx_eq(y2, 104.0));

        // Zero offset is a no-op.
        let unchanged = perpendicular_offset(10.0, 20.0, 30.0, 40.0, 0.0);
        assert_eq!(unchanged, (10.0, 20.0, 30.0, 40.0));

        // A zero-length line never divides by zero.
        let degenerate = perpendicular_offset(5.0, 5.0, 5.0, 5.0, 4.0);
        assert_eq!(degenerate, (5.0, 5.0, 5.0, 5.0));
    }
}
