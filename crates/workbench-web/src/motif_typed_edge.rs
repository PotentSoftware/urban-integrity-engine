//! `motif_typed_edge.rs` — the `osint-risk-edges` motif thumbnail (D-09): an officer at
//! left-centre fanning out to up to 8 target entities, `DisqualifiedFrom`-sourced edges always
//! drawn and visually emphasised over `Controls`-sourced edges.
//!
//! Data shape this file consumes, widened by plan 50-02's `disqualified_director_edges.rs`: one
//! `EvidenceRow` per officer, `refs[0]` = the officer, `refs[1..]` = that officer's real edge
//! targets, keyed by the stable text prefixes `"DisqualifiedFrom officer "` / `"Controls officer
//! "`. Plan 50-02 never mixes edge types within a single row — a `DisqualifiedFrom`-prefixed row's
//! `refs[1..]` contains ONLY `DisqualifiedFrom`-sourced targets, one row per edge-type block. This
//! is what makes the "DisqualifiedFrom is never truncated out" rule trivially true here: this file
//! always prefers a `DisqualifiedFrom` row wholesale over a `Controls` row (never mixes the two
//! within one drawn thumbnail), so there is no per-target sort to perform — see the
//! `disqualified_row_own_cap_never_admits_a_controls_target` test below, which documents and pins
//! this case rather than assuming it.
//!
//! Follows `verdict_badge.rs`'s pure-fn-then-thin-wrapper split — see that file's own top-of-file
//! doc comment for the rationale (no headless-browser test runner in this workspace).
//!
//! Mounted by `finding_card.rs`'s `TypedEdge`-kind thumbnail dispatch (plan 50-06 Task 2), itself
//! reached from `finding_board.rs` -> `analysis_page.rs` -> `app.rs` (plan 50-06 Task 3).

use leptos::prelude::*;
use urban_integrity_workbench_dto::EvidenceRowView;

use crate::edge_palette::edge_type_color;
use crate::motif_star::{
    edge_label_transform, star_truncation_note, truncate_label, LABEL_FONT, LABEL_MAX_CHARS,
};
use crate::node_palette::node_kind_color;

/// The first `DisqualifiedFrom`-prefixed row, paired with `"DisqualifiedFrom"`, if any exists;
/// else the first `Controls`-prefixed row, paired with `"Controls"`; else `None`. Prefers
/// `DisqualifiedFrom` over `Controls` whenever both exist in the same evidence set (D-09's "never
/// the one that gets cut" rule, satisfied here by drawing an entire `DisqualifiedFrom` row rather
/// than a mix of the two edge types).
pub fn typed_edge_source_row(
    evidence: &[EvidenceRowView],
) -> Option<(&EvidenceRowView, &'static str)> {
    if let Some(row) = evidence
        .iter()
        .find(|r| r.text.starts_with("DisqualifiedFrom officer "))
    {
        return Some((row, "DisqualifiedFrom"));
    }
    if let Some(row) = evidence
        .iter()
        .find(|r| r.text.starts_with("Controls officer "))
    {
        return Some((row, "Controls"));
    }
    None
}

/// Evenly spaces `target_count` points at fixed `cx` between `top_cy` and `bottom_cy` inclusive.
/// A single target lands at the vertical midpoint (handled explicitly to avoid a divide-by-zero
/// when `target_count == 1`).
pub fn typed_edge_target_positions(
    target_count: usize,
    cx: f64,
    top_cy: f64,
    bottom_cy: f64,
) -> Vec<(f64, f64)> {
    if target_count == 0 {
        return Vec::new();
    }
    if target_count == 1 {
        return vec![(cx, (top_cy + bottom_cy) / 2.0)];
    }
    (0..target_count)
        .map(|i| {
            let t = i as f64 / (target_count - 1) as f64;
            (cx, top_cy + t * (bottom_cy - top_cy))
        })
        .collect()
}

/// Renders the officer at `(40, 100)` (scaled) and up to `target_budget` targets fanning out at
/// `(160, ...)` (scaled), edges coloured by the row's own returned edge-type label
/// (`"DisqualifiedFrom"` -> `--edge-risk` 3px, `"Controls"` -> `--edge-control` 1.5px) plus the
/// same "+{n} more" truncation note shape `motif_star.rs` uses. `evidence` is the probe's full
/// evidence row list (`typed_edge_source_row` picks the one row this thumbnail draws).
#[component]
#[allow(non_snake_case)]
pub fn MotifTypedEdge(
    evidence: Vec<EvidenceRowView>,
    #[prop(default = 200.0)] size: f64,
    #[prop(default = 8)] target_budget: usize,
    /// Draw the officer's and each target's name plus the edge type — see `MotifStar`'s
    /// `labelled` prop for the rationale. When on, the officer and target columns move left
    /// (30 / 120 in the 200-unit space instead of 40 / 160) so target labels have ~75 units to
    /// run rightward before the square's edge.
    #[prop(default = false)]
    labelled: bool,
) -> impl IntoView {
    let scale = size / 200.0;
    let (officer_cx, target_cx) = if labelled {
        (30.0 * scale, 120.0 * scale)
    } else {
        (40.0 * scale, 160.0 * scale)
    };
    let officer_cy = 100.0 * scale;
    let top_cy = 30.0 * scale;
    let bottom_cy = 170.0 * scale;
    let officer_r = 10.0 * scale;
    let target_r = 6.0 * scale;
    let label_font = LABEL_FONT * scale;
    let label_style = format!(
        "font-size: {label_font}px; fill: var(--foreground); paint-order: stroke; \
         stroke: var(--card-hover); stroke-width: {}px; stroke-linejoin: round;",
        3.0 * scale
    );

    let source = typed_edge_source_row(&evidence);

    let (has_officer, targets, real_count, edge_label) = match source {
        Some((row, label)) => {
            let real = row.refs.get(1..).unwrap_or(&[]).to_vec();
            let drawn: Vec<_> = real.iter().take(target_budget).cloned().collect();
            (true, drawn, real.len(), label)
        }
        None => (false, Vec::new(), 0, ""),
    };

    let positions = typed_edge_target_positions(targets.len(), target_cx, top_cy, bottom_cy);
    let note = star_truncation_note(real_count, targets.len());

    // Label data, computed up front so the `view!` below stays a thin interpolation layer. The
    // officer's name hangs under the officer node, left-aligned to its edge; each target's name
    // runs rightward from just past its dot; the edge type is drawn ONCE along the first edge —
    // every edge in this motif shares the one type the source row named.
    let officer_name = source.map(|(row, _)| row.refs.first().map(|r| r.name.clone()));
    let officer_label: Option<(f64, f64, String, String)> = if labelled {
        officer_name.flatten().map(|name| {
            (
                officer_cx - officer_r,
                officer_cy + officer_r + 3.0 * scale,
                truncate_label(&name, LABEL_MAX_CHARS),
                name,
            )
        })
    } else {
        None
    };
    let target_labels: Vec<(f64, f64, String, String)> = if labelled {
        targets
            .iter()
            .zip(positions.iter())
            .map(|(target, (x, y))| {
                (
                    x + target_r + 3.0 * scale,
                    *y,
                    truncate_label(&target.name, LABEL_MAX_CHARS),
                    target.name.clone(),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    let edge_type_label = if labelled {
        positions
            .first()
            .map(|(x, y)| edge_label_transform(officer_cx, officer_cy, *x, *y))
            .map(|(mx, my, angle)| (mx, my, angle, edge_label.to_string()))
    } else {
        None
    };

    let (edge_token, edge_width) = if edge_label == "DisqualifiedFrom" {
        (edge_type_color("DisqualifiedFrom"), 3.0 * scale)
    } else {
        (edge_type_color("Controls"), 1.5 * scale)
    };
    let officer_color = node_kind_color("Officer");
    let entity_color = node_kind_color("Entity");

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
                        x1=officer_cx.to_string()
                        y1=officer_cy.to_string()
                        x2=x.to_string()
                        y2=y.to_string()
                        stroke=format!("var({edge_token})")
                        stroke-width=edge_width.to_string()
                    />
                }).collect_view()}
                {has_officer.then(|| view! {
                    <circle
                        cx=officer_cx.to_string()
                        cy=officer_cy.to_string()
                        r=officer_r.to_string()
                        fill=format!("var({officer_color})")
                    />
                })}
                {positions.iter().map(|(x, y)| view! {
                    <circle
                        cx=x.to_string()
                        cy=y.to_string()
                        r=target_r.to_string()
                        fill=format!("var({entity_color})")
                    />
                }).collect_view()}
                {edge_type_label.map(|(mx, my, angle, kind)| view! {
                    <text
                        x=mx.to_string()
                        y=my.to_string()
                        dy=(-label_font * 0.9).to_string()
                        text-anchor="middle"
                        dominant-baseline="middle"
                        transform=format!("rotate({angle} {mx} {my})")
                        style=label_style.clone()
                    >
                        {kind}
                    </text>
                })}
                {officer_label.map(|(lx, ly, short, full)| view! {
                    <text
                        x=lx.to_string()
                        y=ly.to_string()
                        text-anchor="start"
                        dominant-baseline="hanging"
                        style=label_style.clone()
                    >
                        <title>{full}</title>
                        {short}
                    </text>
                })}
                {target_labels.into_iter().map(|(lx, ly, short, full)| view! {
                    <text
                        x=lx.to_string()
                        y=ly.to_string()
                        text-anchor="start"
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
    use urban_integrity_workbench_dto::EntityRef;

    fn entity_ref(id: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: id.to_string(),
            kind: "Entity".to_string(),
        }
    }

    fn disq_row(officer: &str, targets: &[&str]) -> EvidenceRowView {
        let mut refs = vec![entity_ref(officer)];
        refs.extend(targets.iter().map(|t| entity_ref(t)));
        EvidenceRowView {
            text: format!(
                "DisqualifiedFrom officer {officer} directs {} entities (showing up to 8): {}",
                targets.len(),
                targets.join(", ")
            ),
            refs,
        }
    }

    fn controls_row(officer: &str, targets: &[&str]) -> EvidenceRowView {
        let mut refs = vec![entity_ref(officer)];
        refs.extend(targets.iter().map(|t| entity_ref(t)));
        EvidenceRowView {
            text: format!(
                "Controls officer {officer} controls {} entities (showing up to 8): {}",
                targets.len(),
                targets.join(", ")
            ),
            refs,
        }
    }

    #[test]
    fn prefers_disqualified_from_when_both_exist() {
        let evidence = vec![
            controls_row("director:od", &["entity:a"]),
            disq_row("director:od", &["entity:b"]),
        ];
        let (row, label) = typed_edge_source_row(&evidence).expect("a source row");
        assert_eq!(label, "DisqualifiedFrom");
        assert!(row.text.starts_with("DisqualifiedFrom officer "));
    }

    #[test]
    fn falls_back_to_controls_when_no_disqualified_from_row_exists() {
        let evidence = vec![controls_row("director:od", &["entity:a"])];
        let (row, label) = typed_edge_source_row(&evidence).expect("a source row");
        assert_eq!(label, "Controls");
        assert!(row.text.starts_with("Controls officer "));
    }

    #[test]
    fn returns_none_on_unrelated_text() {
        let evidence = vec![EvidenceRowView {
            text: "Top wards: A: 12.".to_string(),
            refs: vec![],
        }];
        assert!(typed_edge_source_row(&evidence).is_none());
        assert!(typed_edge_source_row(&[]).is_none());
    }

    #[test]
    fn target_positions_are_evenly_spaced_for_one_two_and_eight() {
        let one = typed_edge_target_positions(1, 160.0, 30.0, 170.0);
        assert_eq!(one, vec![(160.0, 100.0)]);

        let two = typed_edge_target_positions(2, 160.0, 30.0, 170.0);
        assert_eq!(two, vec![(160.0, 30.0), (160.0, 170.0)]);

        let eight = typed_edge_target_positions(8, 160.0, 30.0, 170.0);
        assert_eq!(eight.len(), 8);
        assert_eq!(eight[0], (160.0, 30.0));
        assert_eq!(eight[7], (160.0, 170.0));
        // Monotonically increasing cy, evenly spaced.
        let step = (170.0 - 30.0) / 7.0;
        for (i, (_, y)) in eight.iter().enumerate() {
            let expected = 30.0 + step * i as f64;
            assert!((y - expected).abs() < 1e-9, "point {i}: {y} vs {expected}");
        }
    }

    /// PROVE THIS CAN FAIL: mix a `Controls`-sourced target into `disq_row`'s own ref list — the
    /// `!target.id.starts_with("entity:controls")` assertion then fails, showing that a mixed row
    /// WOULD let a non-DisqualifiedFrom target slip into the drawn set.
    ///
    /// This is the "always-included, never truncated out" rule from the phase 50 UI spec, pinned as
    /// the SECOND of the plan's two possible cases: plan 50-02's rows are always single-typed per
    /// row (one row per officer per edge-type block), so a `DisqualifiedFrom` row's own 8-cap
    /// already only ever contains `DisqualifiedFrom`-sourced targets — the always-included rule is
    /// trivially satisfied by `typed_edge_source_row`'s "prefer DisqualifiedFrom wholesale" choice,
    /// with no per-target sort needed.
    #[test]
    fn disqualified_row_own_cap_never_admits_a_controls_target() {
        let disq_targets: Vec<String> = (0..10).map(|i| format!("entity:disq-{i}")).collect();
        let disq_target_refs: Vec<&str> = disq_targets.iter().map(String::as_str).collect();
        let evidence = vec![
            controls_row("director:od", &["entity:controls-0", "entity:controls-1"]),
            disq_row("director:od", &disq_target_refs),
        ];

        let (row, label) = typed_edge_source_row(&evidence).expect("a source row");
        assert_eq!(label, "DisqualifiedFrom");

        let drawn: Vec<&EntityRef> = row.refs.get(1..).unwrap_or(&[]).iter().take(8).collect();
        assert_eq!(drawn.len(), 8, "8-node budget caps the drawn targets");
        for target in &drawn {
            assert!(
                disq_targets.contains(&target.id),
                "{} must come from the DisqualifiedFrom row's own targets",
                target.id
            );
            assert!(
                !target.id.starts_with("entity:controls"),
                "a Controls-sourced target must never appear in the DisqualifiedFrom draw: {}",
                target.id
            );
        }
    }
}
