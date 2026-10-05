//! Source-structural regression test for 46-13 (46-UAT gap 3 / test 8): the `/alert/:id`
//! tool-rail box (`GraphMotifDetail`'s `motif_preview`) must draw the alert's own motif via
//! [`crate::motif_path::MotifPath`], not the participant-chip fallback (`NamedEntityChips`) that
//! read as an empty 800px box before Run / after Reset.
//!
//! Host-side, source-reading only (this crate has no DOM runner) — the same style as
//! `alert_filters_rail_stable.rs` and `abbr_reachable.rs`.
//!
//! Non-vacuity: before the fix, `motif_preview`'s bound span contains `NamedEntityChips` and not
//! `<MotifPath`, so all four tests below fail against the pre-46-13 source.

const ALERT_DETAIL_PAGE_RS: &str = include_str!("../src/alert_detail_page.rs");

/// Strips `//` line comments (never mid-string, this file has no `//` inside string literals in
/// the swept region) so a comment mentioning `NamedEntityChips` or `<MotifPath` can't skew the
/// bound-span assertions.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `let motif_preview = { ... };` binding's text, from the `let motif_preview =` token up to
/// and including the FIRST `};` that closes it. Bounded deliberately short of `<GraphMotifDetail`
/// — that span would also include the retained standalone step-3 `<MotifPath ... />` box around
/// line 266, which would make the positive assertions below pass even before this plan's change.
fn motif_preview_binding_span(stripped: &str) -> &str {
    let start = stripped
        .find("let motif_preview =")
        .expect("alert_detail_page.rs must still declare `let motif_preview =`");
    let after_start = &stripped[start..];
    let end = after_start
        .find("};")
        .expect("the motif_preview binding must close with `};`");
    &after_start[..end + 2]
}

#[test]
fn motif_preview_binding_is_bounded_and_terminated() {
    let stripped = strip_line_comments(ALERT_DETAIL_PAGE_RS);
    let span = motif_preview_binding_span(&stripped);
    assert!(
        !span.is_empty(),
        "motif_preview binding span must not be empty"
    );
    assert!(
        span.ends_with("};"),
        "motif_preview binding span must end with `}};`, got: {span:?}"
    );
    assert!(
        span.len() < 1_500,
        "motif_preview binding span is {} chars — a missing `}};` terminator may have \
         silently widened the match to the rest of the file: {span:?}",
        span.len()
    );
}

#[test]
fn alert_motif_preview_draws_the_motif_not_chips() {
    let stripped = strip_line_comments(ALERT_DETAIL_PAGE_RS);
    let span = motif_preview_binding_span(&stripped);
    assert!(
        span.contains("<MotifPath"),
        "motif_preview must render <MotifPath ... /> inside its bound span, got: {span:?}"
    );
    assert!(
        span.contains("size=DETAIL_MOTIF_SIZE"),
        "motif_preview's MotifPath must use size=DETAIL_MOTIF_SIZE, got: {span:?}"
    );
    assert!(
        span.contains("labelled=true"),
        "motif_preview's MotifPath must render labelled=true, got: {span:?}"
    );
    assert!(
        !span.contains("NamedEntityChips"),
        "motif_preview must no longer render the NamedEntityChips fallback, got: {span:?}"
    );
}

#[test]
fn motif_preview_is_still_wired_to_graph_motif_detail() {
    let stripped = strip_line_comments(ALERT_DETAIL_PAGE_RS);
    assert!(
        stripped.contains("<GraphMotifDetail") && {
            let gmd_start = stripped.find("<GraphMotifDetail").unwrap();
            let gmd_end = (gmd_start + 400).min(stripped.len());
            stripped[gmd_start..gmd_end].contains("motif_preview=motif_preview")
        },
        "GraphMotifDetail must still be wired with motif_preview=motif_preview"
    );
}

/// Pins the plan's "keep both" decision: the standalone step-3 box (46-UI-SPEC detail-route,
/// accepted at UAT test 8) stays, in addition to the new preview inside `motif_preview` — so
/// `<MotifPath` must appear at least twice in the file's code.
#[test]
fn motif_path_renders_in_two_places_standalone_box_and_preview() {
    let stripped = strip_line_comments(ALERT_DETAIL_PAGE_RS);
    let count = stripped.matches("<MotifPath").count();
    assert!(
        count >= 2,
        "expected <MotifPath at least twice (standalone step-3 box + motif_preview), found {count}"
    );
}
