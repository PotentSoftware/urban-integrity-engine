//! `motif_star.rs` — the `address-cluster-exact` motif thumbnail (D-09): a location pinned at the
//! exact centre of the thumbnail with up to 8 leaf company nodes fanned evenly around it.
//!
//! Follows `verdict_badge.rs`'s pure-fn-then-thin-wrapper split exactly (see that file's own
//! top-of-file doc comment): every piece of geometry/truncation logic that decides WHAT gets drawn
//! is factored into a plain function taking owned/borrowed data and returning owned values, fully
//! host-testable without a DOM. The `#[component]` itself does nothing but call those functions
//! and interpolate their output into `view!`.
//!
//! Data shape this file consumes, established by `crates/probe/src/shared_exact_address.rs`:
//! `evidence.refs[0]` is always the location (centre), `evidence.refs[1..]` are the applicant
//! companies (leaves), already sorted deterministically (biggest cluster first, ties broken by
//! id) — this file never re-sorts, it only caps and positions.
//!
//! Mounted by `finding_card.rs`'s `Star`-kind thumbnail dispatch (plan 50-06 Task 2), itself
//! reached from `finding_board.rs` -> `analysis_page.rs` -> `app.rs` (plan 50-06 Task 3).

use leptos::prelude::*;
use urban_integrity_workbench_dto::EvidenceRowView;

use crate::edge_palette::edge_type_color;
use crate::node_palette::node_kind_color;

/// `leaf_count` points evenly spaced on a circle of `radius` around `(cx, cy)`, starting at 12
/// o'clock (`i = 0`) and proceeding clockwise. The phase 50 UI spec's exact formula: `angle_deg = i *
/// (360 / leaf_count) - 90`, converted to radians, `(cx + radius * angle.cos(), cy + radius *
/// angle.sin())`. Never divides by zero — `leaf_count = 0` returns an empty vec before the
/// division would occur.
pub fn star_leaf_positions(leaf_count: usize, radius: f64, cx: f64, cy: f64) -> Vec<(f64, f64)> {
    if leaf_count == 0 {
        return Vec::new();
    }
    (0..leaf_count)
        .map(|i| {
            let angle_deg = i as f64 * (360.0 / leaf_count as f64) - 90.0;
            let angle_rad = angle_deg.to_radians();
            (cx + radius * angle_rad.cos(), cy + radius * angle_rad.sin())
        })
        .collect()
}

/// Longest node label a labelled motif draws before truncating, in characters. Sized so a label
/// anchored just outside a leaf at [`LABELLED_STAR_RADIUS`] still ends inside the square at
/// `size = 400` (the detail route's [`crate::finding_detail_page`] mount) — the SVG is
/// `overflow: hidden`, so a longer label would be cut mid-word rather than ellipsised.
pub const LABEL_MAX_CHARS: usize = 15;

/// Star radius in the 200-unit design space when labels are drawn. Pulled in from the unlabelled
/// 70 so that the 3 and 9 o'clock leaf labels have room to run outward. The derivation is in
/// design-space units and is therefore scale-invariant (phase 46 assumption A-02a) — only the px
/// figures below, quoted against the detail route's CURRENT 800px mount
/// (`DETAIL_MOTIF_SIZE`, `finding_detail_page.rs:72`), need to track that mount if it changes
/// again: a 15-char label in a 22px font is ~190px wide, so the label must start no later than
/// ~600px — `200 + 40×2 + 6×2 + 3×2 = 298` design-space units. At 48 the first/last character of
/// a horizontal label was clipped by the square's `overflow: hidden` (seen on the toy's
/// `GB-COH:PL000207`, 2026-09-18, when the detail mount was still 400px).
pub const LABELLED_STAR_RADIUS: f64 = 40.0;

/// Label font size in the 200-unit design space — 5.5 × scale, so 22px at the detail route's
/// current 800px mount (`DETAIL_MOTIF_SIZE`) and ~5.5px (deliberately unreadable, which is why
/// thumbnails stay unlabelled) at the 200px card.
pub const LABEL_FONT: f64 = 5.5;

/// Truncates a node label to `max_chars`, replacing the tail with a single `…` so the drawn
/// text never exceeds `max_chars` characters in total. The full name still reaches the analyst
/// through the `<title>` child every labelled node carries (a native tooltip on hover).
pub fn truncate_label(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_string();
    }
    let mut short: String = name.chars().take(max_chars.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// Where a node's label sits: pushed `gap` units radially AWAY from the motif centre `(cx, cy)`,
/// with a `text-anchor` chosen so the text runs outward too — `start` on the right half, `end`
/// on the left, `middle` in the near-vertical band (|unit x| ≤ 0.3, i.e. within ~17° of
/// straight up/down). Callers pair this with `dominant-baseline="middle"` so the returned `y` is
/// the text's vertical centre, not its baseline. A node AT the centre gets its label directly
/// below it, anchored `middle`.
pub fn node_label_anchor(x: f64, y: f64, cx: f64, cy: f64, gap: f64) -> (f64, f64, &'static str) {
    let dx = x - cx;
    let dy = y - cy;
    let len = (dx * dx + dy * dy).sqrt();
    if len < f64::EPSILON {
        return (x, y + gap, "middle");
    }
    let (ux, uy) = (dx / len, dy / len);
    let anchor = if ux > 0.3 {
        "start"
    } else if ux < -0.3 {
        "end"
    } else {
        "middle"
    };
    (x + ux * gap, y + uy * gap, anchor)
}

/// Midpoint and upright rotation (degrees) for a label laid ALONG the edge `(x1, y1) → (x2, y2)`.
/// The angle is folded into `(-90, 90]` so the text never reads upside down whichever way the
/// edge was drawn; a perfectly vertical edge reads top-to-bottom.
pub fn edge_label_transform(x1: f64, y1: f64, x2: f64, y2: f64) -> (f64, f64, f64) {
    let mx = (x1 + x2) / 2.0;
    let my = (y1 + y2) / 2.0;
    let mut angle = (y2 - y1).atan2(x2 - x1).to_degrees();
    if angle > 90.0 {
        angle -= 180.0;
    } else if angle <= -90.0 {
        angle += 180.0;
    }
    (mx, my, angle)
}

/// The "+{n} more" truncation note (Copywriting Contract) — `None` when every real leaf was
/// drawn, `Some` with the exact delta otherwise. The delta is against THIS thumbnail's own drawn
/// cluster, never the grand total across all clusters (D-07 — the claim line's job).
pub fn star_truncation_note(real_leaf_count: usize, drawn_count: usize) -> Option<String> {
    if real_leaf_count <= drawn_count {
        None
    } else {
        Some(format!("+{} more", real_leaf_count - drawn_count))
    }
}

/// Renders `evidence`'s star: `refs[0]` is the centre location, `refs[1..]` (capped at
/// `leaf_budget`) are the leaf companies. `size` scales the whole geometry proportionally —
/// `size / 200.0` — so the detail route (plan 50-07) can request 400.0 against the exact same
/// pure functions. If `evidence.refs` carries no leaves (`len() < 2`), draws only the centre
/// circle — never fabricates placeholder leaves.
#[component]
#[allow(non_snake_case)]
pub fn MotifStar(
    evidence: EvidenceRowView,
    #[prop(default = 200.0)] size: f64,
    #[prop(default = 8)] leaf_budget: usize,
    /// Draw node names and the edge type alongside the geometry. Off for the board's thumbnails
    /// (the shape is the point at 200px; 5.5px text would be noise), on for the `/finding/:probe`
    /// detail route, where the operator asked that "clicking on a graph should show the graph with
    /// labels for nodes and edges". The star's edges are all one type (`LocatedAt`), so the edge
    /// label is drawn ONCE, along the first spoke, rather than repeated on every spoke.
    #[prop(default = false)]
    labelled: bool,
) -> impl IntoView {
    let scale = size / 200.0;
    let cx = 100.0 * scale;
    let cy = 100.0 * scale;
    let radius = if labelled { LABELLED_STAR_RADIUS } else { 70.0 } * scale;
    let centre_r = 10.0 * scale;
    let leaf_r = 6.0 * scale;
    let edge_width = 1.5 * scale;
    let label_font = LABEL_FONT * scale;
    let label_gap = leaf_r + 3.0 * scale;

    let centre = evidence.refs.first().cloned();
    let real_leaves = evidence.refs.get(1..).unwrap_or(&[]).to_vec();
    let drawn_leaves: Vec<_> = real_leaves.iter().take(leaf_budget).cloned().collect();
    let positions = star_leaf_positions(drawn_leaves.len(), radius, cx, cy);
    let note = star_truncation_note(real_leaves.len(), drawn_leaves.len());

    let centre_color = node_kind_color("Location");
    let leaf_color = node_kind_color("Entity");
    let edge_color = edge_type_color("LocatedAt");

    // Labels are computed as plain data here and only interpolated below, keeping the
    // pure-fn-then-thin-wrapper split this file's doc comment commits to.
    let leaf_labels: Vec<(f64, f64, &'static str, String, String)> = if labelled {
        drawn_leaves
            .iter()
            .zip(positions.iter())
            .map(|(leaf, (x, y))| {
                let (lx, ly, anchor) = node_label_anchor(*x, *y, cx, cy, label_gap);
                (
                    lx,
                    ly,
                    anchor,
                    truncate_label(&leaf.name, LABEL_MAX_CHARS),
                    leaf.name.clone(),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let centre_label = if labelled {
        centre.as_ref().map(|c| {
            let (lx, ly, anchor) = node_label_anchor(cx, cy, cx, cy, centre_r + 3.0 * scale);
            (
                lx,
                ly,
                anchor,
                truncate_label(&c.name, LABEL_MAX_CHARS),
                c.name.clone(),
            )
        })
    } else {
        None
    };
    let edge_label = if labelled {
        positions
            .first()
            .map(|(x, y)| edge_label_transform(cx, cy, *x, *y))
    } else {
        None
    };
    // A halo in the card's own background colour keeps a label legible where it crosses a spoke.
    let label_style = format!(
        "font-size: {label_font}px; fill: var(--foreground); paint-order: stroke; \
         stroke: var(--card-hover); stroke-width: {}px; stroke-linejoin: round;",
        3.0 * scale
    );

    view! {
        <div style="display: flex; flex-direction: column; align-items: center; gap: var(--space-xs);">
            <svg
                viewBox=format!("0 0 {size} {size}")
                width="100%"
                height=size.to_string()
                // Never taller than the square that holds it: the detail route's box is
                // `min(800px, 100%)` wide with `aspect-ratio: 1`, so on a narrow viewport the
                // fixed `height=size` would otherwise overflow the shrunken square.
                style="max-width: 100%; max-height: 100%;"
            >
                {positions.iter().map(|(x, y)| view! {
                    <line
                        x1=cx.to_string()
                        y1=cy.to_string()
                        x2=x.to_string()
                        y2=y.to_string()
                        stroke=format!("var({edge_color})")
                        stroke-width=edge_width.to_string()
                    />
                }).collect_view()}
                {centre.as_ref().map(|_| view! {
                    <circle
                        cx=cx.to_string()
                        cy=cy.to_string()
                        r=centre_r.to_string()
                        fill=format!("var({centre_color})")
                    />
                })}
                {positions.iter().map(|(x, y)| view! {
                    <circle
                        cx=x.to_string()
                        cy=y.to_string()
                        r=leaf_r.to_string()
                        fill=format!("var({leaf_color})")
                    />
                }).collect_view()}
                {edge_label.map(|(mx, my, angle)| view! {
                    <text
                        x=mx.to_string()
                        y=my.to_string()
                        dy=(-label_font * 0.9).to_string()
                        text-anchor="middle"
                        dominant-baseline="middle"
                        transform=format!("rotate({angle} {mx} {my})")
                        style=label_style.clone()
                    >
                        "LocatedAt"
                    </text>
                })}
                {centre_label.map(|(lx, ly, anchor, short, full)| view! {
                    <text
                        x=lx.to_string()
                        y=ly.to_string()
                        text-anchor=anchor
                        dominant-baseline="hanging"
                        style=label_style.clone()
                    >
                        <title>{full}</title>
                        {short}
                    </text>
                })}
                {leaf_labels.into_iter().map(|(lx, ly, anchor, short, full)| view! {
                    <text
                        x=lx.to_string()
                        y=ly.to_string()
                        text-anchor=anchor
                        dominant-baseline="middle"
                        style=label_style.clone()
                    >
                        <title>{full}</title>
                        {short}
                    </text>
                }).collect_view()}
            </svg>
            {note.map(|text| view! {
                <div class="text-label" style="color: var(--muted-foreground);">{text}</div>
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOLERANCE: f64 = 1e-6;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < TOLERANCE
    }

    /// PROVE THIS CAN FAIL: change `max_chars.saturating_sub(1)` to `max_chars` and the truncated
    /// result becomes 16 characters, failing the length assertion.
    #[test]
    fn truncate_label_keeps_short_names_and_ellipsises_long_ones_within_the_cap() {
        assert_eq!(truncate_label("Planted Beta Ltd", 16), "Planted Beta Ltd");
        let long = truncate_label("Planted Block Two Officer 3", LABEL_MAX_CHARS);
        assert_eq!(long.chars().count(), LABEL_MAX_CHARS);
        assert!(long.ends_with('…'), "got {long:?}");
        assert!(long.starts_with("Planted Block "), "got {long:?}");
        // Multi-byte input is cut on chars, never bytes (the resolver-panic lesson, 2026-06-09).
        assert_eq!(truncate_label("Ünïcödé Nämé Lt d", 5), "Ünïc…");
    }

    /// PROVE THIS CAN FAIL: flip either 0.3 threshold sign and the right-hand leaf's anchor
    /// becomes `end`, so its label would run back INTO the star.
    #[test]
    fn node_label_anchor_runs_outward_on_each_side_of_the_centre() {
        let (cx, cy) = (100.0, 100.0);
        // 3 o'clock: to the right, text starts at the node and runs right.
        let (x, y, anchor) = node_label_anchor(170.0, 100.0, cx, cy, 9.0);
        assert!(approx_eq(x, 179.0) && approx_eq(y, 100.0), "got ({x}, {y})");
        assert_eq!(anchor, "start");
        // 9 o'clock: to the left, text ends at the node and runs left.
        let (x, _, anchor) = node_label_anchor(30.0, 100.0, cx, cy, 9.0);
        assert!(approx_eq(x, 21.0), "got {x}");
        assert_eq!(anchor, "end");
        // 12 o'clock: within the vertical band, centred and pushed upward.
        let (x, y, anchor) = node_label_anchor(100.0, 30.0, cx, cy, 9.0);
        assert!(approx_eq(x, 100.0) && approx_eq(y, 21.0), "got ({x}, {y})");
        assert_eq!(anchor, "middle");
        // The centre node itself: directly below, centred.
        let (x, y, anchor) = node_label_anchor(cx, cy, cx, cy, 13.0);
        assert!(approx_eq(x, 100.0) && approx_eq(y, 113.0), "got ({x}, {y})");
        assert_eq!(anchor, "middle");
    }

    /// PROVE THIS CAN FAIL: remove the `angle > 90.0` fold and a right-to-left edge reads upside
    /// down at 180°.
    #[test]
    fn edge_label_transform_sits_at_the_midpoint_and_never_reads_upside_down() {
        let (mx, my, angle) = edge_label_transform(0.0, 0.0, 100.0, 0.0);
        assert!(approx_eq(mx, 50.0) && approx_eq(my, 0.0) && approx_eq(angle, 0.0));
        // Same edge drawn the other way folds back to upright.
        let (_, _, angle) = edge_label_transform(100.0, 0.0, 0.0, 0.0);
        assert!(approx_eq(angle, 0.0), "got {angle}");
        // Straight up (the star's 12 o'clock spoke) reads top-to-bottom, not bottom-to-top.
        let (_, _, angle) = edge_label_transform(100.0, 100.0, 100.0, 30.0);
        assert!(approx_eq(angle, 90.0), "got {angle}");
        // Down-and-right stays as drawn.
        let (_, _, angle) = edge_label_transform(0.0, 0.0, 100.0, 100.0);
        assert!(approx_eq(angle, 45.0), "got {angle}");
        // Up-and-left folds to down-and-right's angle.
        let (_, _, angle) = edge_label_transform(100.0, 100.0, 0.0, 0.0);
        assert!(approx_eq(angle, 45.0), "got {angle}");
    }

    /// PROVE THIS CAN FAIL: swap the `- 90.0` offset for `+ 90.0` (starting at 6 o'clock instead
    /// of 12) and the first-point assertion fails; drop the offset entirely (starting at 3
    /// o'clock) and it also fails.
    #[test]
    fn eight_leaves_start_at_twelve_oclock_evenly_spaced() {
        let positions = star_leaf_positions(8, 70.0, 100.0, 100.0);
        assert_eq!(positions.len(), 8);
        let (first_x, first_y) = positions[0];
        assert!(approx_eq(first_x, 100.0), "first x: {first_x}");
        assert!(approx_eq(first_y, 30.0), "first y: {first_y}");

        // 3 o'clock (i = 2, 90 degrees clockwise from 12) lands at (cx + radius, cy).
        let (third_x, third_y) = positions[2];
        assert!(approx_eq(third_x, 170.0), "third x: {third_x}");
        assert!(approx_eq(third_y, 100.0), "third y: {third_y}");

        // 6 o'clock (i = 4, 180 degrees) lands at (cx, cy + radius).
        let (fifth_x, fifth_y) = positions[4];
        assert!(approx_eq(fifth_x, 100.0), "fifth x: {fifth_x}");
        assert!(approx_eq(fifth_y, 170.0), "fifth y: {fifth_y}");
    }

    #[test]
    fn zero_leaves_returns_empty_vec_without_dividing_by_zero() {
        let positions = star_leaf_positions(0, 70.0, 100.0, 100.0);
        assert!(positions.is_empty());
    }

    #[test]
    fn truncation_note_is_none_when_everything_real_was_drawn() {
        assert_eq!(star_truncation_note(8, 8), None);
        assert_eq!(star_truncation_note(3, 8), None);
    }

    /// PROVE THIS CAN FAIL: compute `n` as `drawn_count - real_leaf_count` (swapped operands) and
    /// this fails with an overflow panic instead of `"+5 more"`.
    #[test]
    fn truncation_note_reports_the_exact_delta() {
        assert_eq!(star_truncation_note(13, 8), Some("+5 more".to_string()));
    }
}
