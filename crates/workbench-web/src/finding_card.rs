//! `finding_card.rs` — the finding board's card anatomy (D-04, criterion 1): a `VerdictBadge`,
//! a claim line, a thumbnail slot that dispatches to one of plan 50-05's three motif renderers or
//! this plan's own chips/absence fallbacks, a disconfirming check, and a standalone "View finding
//! →" link present on every card.
//!
//! Follows `verdict_badge.rs`'s pure-fn-then-thin-wrapper split — see that file's own top-of-file
//! doc comment for the rationale (no headless-browser test runner in this workspace): every piece
//! of dispatch/truncation/copy logic lives in a plain function taking `&ProbeView`/`&[EvidenceRowView]`
//! and returning an owned value, fully host-testable without a DOM.
//!
//! # Which evidence row a `Star`/`RankedBars` card actually draws
//!
//! `crates/probe::shared_exact_address` (the one `Star`-kind probe live today) emits an
//! aggregate row FIRST (`refs: vec![]`, the population/summary counts) followed by one row per
//! cluster, biggest first — the plan 50-08 summary's own DB-gated golden measured this directly
//! (`evidence[0]` is always the aggregate row, `evidence[1]` is the real star cluster). Blindly
//! taking `probe.evidence.first()` would therefore hand `MotifStar` the aggregate row's empty
//! `refs` and draw a blank thumbnail for the one live example criterion 2 names. [`motif_evidence_row`]
//! instead picks the first row that actually carries refs — the biggest cluster, per
//! `shared_exact_address.rs`'s own deterministic sort — falling back to the first row (or `None`)
//! only when nothing carries any. `crates/probe::ward_concentration` (the `RankedBars` kind) has
//! no such aggregate prefix row — its ranked-pairs row IS `evidence[0]` and already carries refs —
//! so the same helper is a no-op there and is reused rather than duplicated.
//!
//! # Card interaction model (avoiding nested anchors)
//!
//! Named-entity chips and the "View finding →" link both render real `<a>` tags. Per
//! the phase 50 UI spec's "Card interaction model": the card is never itself wrapped in an outer `<a>`
//! (HTML forbids nesting them); instead the outer `<div>` carries an `on:click` that navigates to
//! the same href, guarded by walking `event.target()` up to its nearest `<a>` ancestor via
//! `Element::closest` — if the click originated inside a chip's own anchor, this handler is a
//! no-op and the chip's native navigation (plus `leptos_router`'s own window-level anchor
//! interception, `entity_ref_view.rs`'s doc comment) is what fires. No `stop_propagation`
//! anywhere in this file, for the same reason that doc comment gives.
//!
//! Mounted by `finding_board.rs` (plan 50-06 Task 3), itself reached from `analysis_page.rs` ->
//! `app.rs`.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use urban_integrity_workbench_dto::{EntityRef, EvidenceRowView, ProbeView};
use wasm_bindgen::JsCast;

use crate::motif_bars::{parse_ranked_pairs, MotifBars};
use crate::motif_star::MotifStar;
use crate::motif_typed_edge::{typed_edge_source_row, MotifTypedEdge};
use crate::named_entity_chips::NamedEntityChips;
use crate::verdict_badge::VerdictBadge;

/// The claim line's source text — `measurement_summary` verbatim, no new field
/// (the phase 50 UI spec's "Claim line source" note).
pub fn claim_line_text(probe: &ProbeView) -> String {
    probe.measurement_summary.clone()
}

/// The disconfirming check's body text — `hypothesis` verbatim, per the phase 50 UI spec's "Data
/// contract note": the falsifiable question a probe already carries doubles as the thing that
/// would make the finding go away if it stopped holding, so no new per-probe field is needed.
pub fn disconfirming_check_text(probe: &ProbeView) -> String {
    probe.hypothesis.clone()
}

/// The thumbnail slot's dispatch target — mirrors `MotifKind`'s five wire values plus an
/// `Unknown` fallback for a value this build does not recognise (T-50-04: a stale client build
/// must degrade safely, never panic, on a `motif_kind` string it does not know).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailKind {
    Star,
    TypedEdge,
    RankedBars,
    Chips,
    Absence,
    Unknown,
}

/// Maps `probe.motif_kind` to its [`ThumbnailKind`] via an exhaustive `match` with a `_ =>
/// Unknown` arm — never inferred from the probe name string client-side (D-06). `Unknown` renders
/// identically to `Absence` (a safe, non-crashing fallback) but stays a distinct variant so a test
/// can prove it never occurs against the pinned 5-value vocabulary.
pub fn thumbnail_dispatch(probe: &ProbeView) -> ThumbnailKind {
    match probe.motif_kind.as_str() {
        "Star" => ThumbnailKind::Star,
        "TypedEdge" => ThumbnailKind::TypedEdge,
        "RankedBars" => ThumbnailKind::RankedBars,
        "Chips" => ThumbnailKind::Chips,
        "Absence" => ThumbnailKind::Absence,
        _ => ThumbnailKind::Unknown,
    }
}

/// The exact "no entities to show" copy (the phase 50 UI spec's Copywriting Contract) — the reason
/// clause is not optional, per D-04's "the absence stated, never papered over".
pub fn absence_reason_text() -> &'static str {
    "No entities to show — this probe reports a count, not a list (aggregate-only)."
}

/// The evidence row a `Star`/`RankedBars` thumbnail draws: the first row that actually carries
/// refs, falling back to the first row of any kind, or `None` for a probe with no evidence at
/// all. See this file's top-of-file doc comment for why this is not simply `evidence.first()`.
pub fn motif_evidence_row(evidence: &[EvidenceRowView]) -> Option<EvidenceRowView> {
    evidence
        .iter()
        .find(|row| !row.refs.is_empty())
        .or_else(|| evidence.first())
        .cloned()
}

/// Every ref named across a probe's evidence rows, in row order — the `Chips` thumbnail's input
/// (the phase 50 UI spec: "flatten every evidence row's refs into one list").
fn flattened_refs(evidence: &[EvidenceRowView]) -> Vec<EntityRef> {
    evidence.iter().flat_map(|row| row.refs.clone()).collect()
}

/// The copy for a real motif kind whose particular RUN produced nothing to draw — distinct from
/// [`absence_reason_text`] (a probe that never names entities) because the reason differs: this
/// probe does name entities, this run just found none.
pub fn nothing_drawn_text() -> &'static str {
    "Nothing to draw — this run named no entities; the claim line above says what was evaluated."
}

/// Whether the thumbnail slot has anything real to draw for `kind` from `evidence`. A motif KIND
/// is a property of the probe; what one RUN produced is not: `address-cluster-exact`'s ordinary
/// `Clear` verdict ("No exact address shared…", `crates/probe/src/shared_exact_address.rs`)
/// carries a single ref-less row, and handing that to `MotifStar` drew a blank 200px SVG — the
/// "papered-over absence" D-04 forbids, reached through a real motif kind rather than `Absence`.
/// Found by the phase 50 code review (the blocker). Each arm asks exactly what
/// its renderer needs to draw at least one node/bar/chip: `Star` a centre ref, `TypedEdge` a
/// source row (officer), `RankedBars` at least one parsed pair, `Chips` any ref anywhere.
/// `Absence`/`Unknown` are `false` — they render their own copy regardless.
pub fn thumbnail_has_content(kind: ThumbnailKind, evidence: &[EvidenceRowView]) -> bool {
    match kind {
        ThumbnailKind::Star => motif_evidence_row(evidence).is_some_and(|row| !row.refs.is_empty()),
        ThumbnailKind::TypedEdge => typed_edge_source_row(evidence).is_some(),
        ThumbnailKind::RankedBars => motif_evidence_row(evidence)
            .is_some_and(|row| !parse_ranked_pairs(&row.text).is_empty()),
        ThumbnailKind::Chips => evidence.iter().any(|row| !row.refs.is_empty()),
        ThumbnailKind::Absence | ThumbnailKind::Unknown => false,
    }
}

/// The thumbnail slot's border — solid for a real motif or chips, dashed for the absence state
/// (the phase 50 UI spec: "so the absence is visually distinct even before the text is read").
fn thumbnail_border(kind: ThumbnailKind) -> &'static str {
    match kind {
        ThumbnailKind::Absence | ThumbnailKind::Unknown => "1px dashed var(--border)",
        _ => "1px solid var(--border)",
    }
}

/// Whether `event.target()` sits inside (or is) an `<a>` element — the nested-anchor guard this
/// file's top-of-file doc comment describes. `false` for any event this crate's host-side unit
/// tests can construct (there is no DOM), so this is exercised for real only in the browser; the
/// guard's LOGIC (never navigating over a descendant anchor) is what the doc comment states as
/// the contract, and there is nothing here to unit test without a DOM per this crate's established
/// testing approach.
fn click_originated_inside_an_anchor(ev: &web_sys::MouseEvent) -> bool {
    ev.target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|element| element.closest("a").ok())
        .flatten()
        .is_some()
}

/// One probe's card (D-04, criterion 1): 360px MINIMUM height (not fixed — the phase 50 UI spec's
/// "fixed 360px" cannot hold its own contents: `lg` padding ×2 + header + four `md` gaps + the
/// 200px thumbnail + a two-line claim + the 44px link is ~440px, and at a hard 360px with
/// `overflow: hidden` the "View finding →" link was clipped — operator feedback at 50-09's
/// human-verify gate, 2026-09-18; the grid's default `align-items: stretch` still equalises every
/// card in a row, so the "true set of small cards" reading survives), `lg` padding, `--card` background
/// (`--card-hover` on hover of the card body), containing — in order — the existing
/// `VerdictBadge`, the claim line, the 200px thumbnail slot, the disconfirming check, and a real,
/// standalone `<a>` "View finding →" link, present unconditionally regardless of thumbnail kind
/// (D-04's "the card stays clickable", including on every `AggregateOnly` card).
#[component]
#[allow(non_snake_case)]
pub fn FindingCard(probe: ProbeView) -> impl IntoView {
    let navigate = use_navigate();
    let href_for_click = format!("/finding/{}", probe.name);

    let claim = claim_line_text(&probe);
    let disconfirming = disconfirming_check_text(&probe);
    let kind = thumbnail_dispatch(&probe);
    // A real motif kind with nothing to draw this run takes the absence state's dashed border and
    // its own copy — never a blank slot (see `thumbnail_has_content`).
    let drawable = thumbnail_has_content(kind, &probe.evidence);
    let border = if drawable {
        thumbnail_border(kind)
    } else {
        thumbnail_border(ThumbnailKind::Absence)
    };

    let hovered: RwSignal<bool> = RwSignal::new(false);

    let onclick = move |ev: web_sys::MouseEvent| {
        if click_originated_inside_an_anchor(&ev) {
            return;
        }
        navigate(&href_for_click, Default::default());
    };

    let nothing_drawn =
        !drawable && !matches!(kind, ThumbnailKind::Absence | ThumbnailKind::Unknown);
    let thumbnail = if nothing_drawn {
        view! {
            <span
                class="text-body"
                style="color: var(--muted-foreground); text-align: center; padding: var(--space-sm);"
            >
                {nothing_drawn_text()}
            </span>
        }
        .into_any()
    } else {
        match kind {
        ThumbnailKind::Star => {
            let evidence = motif_evidence_row(&probe.evidence).unwrap_or(EvidenceRowView {
                text: String::new(),
                refs: vec![],
            });
            view! { <MotifStar evidence=evidence /> }.into_any()
        }
        ThumbnailKind::TypedEdge => {
            view! { <MotifTypedEdge evidence=probe.evidence.clone() /> }.into_any()
        }
        ThumbnailKind::RankedBars => {
            let evidence = motif_evidence_row(&probe.evidence).unwrap_or(EvidenceRowView {
                text: String::new(),
                refs: vec![],
            });
            view! { <MotifBars evidence=evidence /> }.into_any()
        }
        ThumbnailKind::Chips => {
            let refs = flattened_refs(&probe.evidence);
            view! { <NamedEntityChips refs=refs /> }.into_any()
        }
        ThumbnailKind::Absence | ThumbnailKind::Unknown => view! {
            <span
                class="text-body"
                style="color: var(--muted-foreground); text-align: center; padding: var(--space-sm);"
            >
                {absence_reason_text()}
            </span>
        }
        .into_any(),
        }
    };

    let card_style = move || {
        format!(
            "display: flex; flex-direction: column; gap: var(--space-md); min-height: 360px; \
             padding: var(--space-lg); background: {}; border: 1px solid var(--border); \
             border-radius: 8px; cursor: pointer; overflow: hidden;",
            if hovered.get() {
                "var(--card-hover)"
            } else {
                "var(--card)"
            }
        )
    };

    view! {
        <div
            style=card_style
            on:click=onclick
            on:mouseenter=move |_| hovered.set(true)
            on:mouseleave=move |_| hovered.set(false)
        >
            <VerdictBadge verdict=probe.verdict.clone() />
            <span
                class="text-body"
                title=claim.clone()
                style="width: 100%; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex-shrink: 0;"
            >
                {claim.clone()}
            </span>
            <div style=format!(
                "height: 200px; width: 100%; flex-shrink: 0; background: var(--card-hover); \
                 border: {border}; border-radius: 6px; display: flex; align-items: center; \
                 justify-content: center; overflow: hidden; padding: var(--space-xs);"
            )>
                {thumbnail}
            </div>
            <div style="flex: 1 1 auto; min-height: 0; overflow: hidden;">
                <span class="text-label">"Disconfirming check: "</span>
                <span
                    class="text-body"
                    style="display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden;"
                >
                    {disconfirming}
                </span>
            </div>
            <a
                class="text-label"
                href=format!("/finding/{}", probe.name)
                style="align-self: flex-end; color: var(--primary); min-height: 44px; display: inline-flex; align-items: center; flex-shrink: 0;"
            >
                "View finding \u{2192}"
            </a>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::VerdictView;

    fn sample_probe(motif_kind: &str) -> ProbeView {
        ProbeView {
            name: "shared-exact-address".to_string(),
            tier: "tier-1".to_string(),
            hypothesis: "test hypothesis".to_string(),
            verdict: VerdictView::DataGap {
                missing_input: "missing OSINT source".to_string(),
            },
            measurement_summary: "test summary".to_string(),
            evidence: vec![],
            motif_kind: motif_kind.to_string(),
            cause: None,
            remediation: None,
        }
    }

    fn entity_ref(id: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: id.to_string(),
            kind: "Entity".to_string(),
        }
    }

    #[test]
    fn claim_line_text_returns_measurement_summary_verbatim() {
        let probe = sample_probe("Absence");
        assert_eq!(claim_line_text(&probe), "test summary");
    }

    #[test]
    fn disconfirming_check_text_returns_hypothesis_verbatim() {
        let probe = sample_probe("Absence");
        assert_eq!(disconfirming_check_text(&probe), "test hypothesis");
    }

    /// PROVE THIS CAN FAIL: drop the `_ => Unknown` arm's catch-all (make it exhaustive over only
    /// the 5 known strings) and this fails to compile; keep the arm but map an unrecognised string
    /// to `Absence` directly instead of `Unknown` and the `all_five_...` assertion still passes
    /// but `an_unrecognised_motif_kind_maps_to_unknown_not_a_panic` fails, since it explicitly
    /// checks the distinct variant rather than only the rendering behaviour.
    #[test]
    fn all_five_real_motif_kind_names_map_to_their_matching_variant() {
        assert_eq!(
            thumbnail_dispatch(&sample_probe("Star")),
            ThumbnailKind::Star
        );
        assert_eq!(
            thumbnail_dispatch(&sample_probe("TypedEdge")),
            ThumbnailKind::TypedEdge
        );
        assert_eq!(
            thumbnail_dispatch(&sample_probe("RankedBars")),
            ThumbnailKind::RankedBars
        );
        assert_eq!(
            thumbnail_dispatch(&sample_probe("Chips")),
            ThumbnailKind::Chips
        );
        assert_eq!(
            thumbnail_dispatch(&sample_probe("Absence")),
            ThumbnailKind::Absence
        );
    }

    #[test]
    fn an_unrecognised_motif_kind_maps_to_unknown_not_a_panic() {
        assert_eq!(
            thumbnail_dispatch(&sample_probe("SomeFutureKind")),
            ThumbnailKind::Unknown
        );
        assert_eq!(
            thumbnail_dispatch(&sample_probe("")),
            ThumbnailKind::Unknown
        );
    }

    #[test]
    fn absence_reason_text_is_the_exact_ui_spec_copy() {
        assert_eq!(
            absence_reason_text(),
            "No entities to show — this probe reports a count, not a list (aggregate-only)."
        );
    }

    /// **The bug this file's top-of-file doc comment documents and fixes.** An aggregate row with
    /// no refs, followed by the real cluster row — `motif_evidence_row` must skip the empty one.
    ///
    /// PROVE THIS CAN FAIL: replace the body with `evidence.first().cloned()` and this fails,
    /// returning the empty aggregate row instead of the populated cluster row.
    #[test]
    fn motif_evidence_row_skips_a_leading_ref_less_aggregate_row() {
        let evidence = vec![
            EvidenceRowView {
                text: "2 addresses shared by 2+ distinct applicants, of 148 with \u{2265}1 \
                       applicant evaluated."
                    .to_string(),
                refs: vec![],
            },
            EvidenceRowView {
                text: "8 applicants at location:planted-l2".to_string(),
                refs: vec![
                    entity_ref("location:planted-l2"),
                    entity_ref("GB-COH:PL000201"),
                ],
            },
        ];
        let chosen = motif_evidence_row(&evidence).expect("a row with refs exists");
        assert_eq!(chosen.text, "8 applicants at location:planted-l2");
        assert_eq!(chosen.refs.len(), 2);
    }

    /// `ward-concentration`'s shape has no aggregate prefix row — its one row already carries
    /// refs, so the helper is a no-op and still returns it.
    #[test]
    fn motif_evidence_row_is_a_no_op_when_the_first_row_already_carries_refs() {
        let evidence = vec![EvidenceRowView {
            text: "Top wards: A: 12, B: 9.".to_string(),
            refs: vec![entity_ref("ward:A"), entity_ref("ward:B")],
        }];
        let chosen = motif_evidence_row(&evidence).expect("a row exists");
        assert_eq!(chosen.refs.len(), 2);
    }

    #[test]
    fn motif_evidence_row_falls_back_to_the_first_row_when_none_carry_refs() {
        let evidence = vec![EvidenceRowView {
            text: "No exact address shared by 2+ distinct applicants.".to_string(),
            refs: vec![],
        }];
        let chosen = motif_evidence_row(&evidence).expect("a row exists");
        assert_eq!(chosen.refs.len(), 0);
    }

    #[test]
    fn motif_evidence_row_returns_none_for_a_probe_with_no_evidence_at_all() {
        assert_eq!(motif_evidence_row(&[]), None);
    }

    #[test]
    fn flattened_refs_concatenates_every_rows_refs_in_row_order() {
        let evidence = vec![
            EvidenceRowView {
                text: "row a".to_string(),
                refs: vec![entity_ref("a1"), entity_ref("a2")],
            },
            EvidenceRowView {
                text: "row b".to_string(),
                refs: vec![entity_ref("b1")],
            },
        ];
        let refs = flattened_refs(&evidence);
        assert_eq!(
            refs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["a1", "a2", "b1"]
        );
    }

    /// PROVE THIS CAN FAIL: make the `Star` arm of `thumbnail_has_content` return `true` and the
    /// Clear-run assertion below fails — the exact blank-SVG bug the phase 50 code review found.
    #[test]
    fn a_motif_kind_whose_run_named_no_entities_has_nothing_to_draw() {
        // `shared_exact_address.rs`'s Clear verdict shape: one ref-less row.
        let clear_run = vec![EvidenceRowView {
            text: "3 locations evaluated.".to_string(),
            refs: vec![],
        }];
        assert!(!thumbnail_has_content(ThumbnailKind::Star, &clear_run));
        assert!(!thumbnail_has_content(ThumbnailKind::TypedEdge, &clear_run));
        assert!(!thumbnail_has_content(
            ThumbnailKind::RankedBars,
            &clear_run
        ));
        assert!(!thumbnail_has_content(ThumbnailKind::Chips, &clear_run));
        assert!(!thumbnail_has_content(ThumbnailKind::Star, &[]));

        let hit = vec![EvidenceRowView {
            text: "8 companies at one address".to_string(),
            refs: vec![
                entity_ref("location:planted-l2"),
                entity_ref("GB-COH:PL000201"),
            ],
        }];
        assert!(thumbnail_has_content(ThumbnailKind::Star, &hit));
        assert!(thumbnail_has_content(ThumbnailKind::Chips, &hit));
        let typed = vec![EvidenceRowView {
            text: "DisqualifiedFrom officer director:planted-od".to_string(),
            refs: vec![
                entity_ref("director:planted-od"),
                entity_ref("GB-COH:PL000002"),
            ],
        }];
        assert!(thumbnail_has_content(ThumbnailKind::TypedEdge, &typed));
        // Absence never claims content — it renders its own copy.
        assert!(!thumbnail_has_content(ThumbnailKind::Absence, &hit));
    }

    #[test]
    fn nothing_drawn_copy_is_distinct_from_the_aggregate_only_copy() {
        assert_ne!(nothing_drawn_text(), absence_reason_text());
        assert!(nothing_drawn_text().starts_with("Nothing to draw — "));
    }

    /// The absence/unknown thumbnail slot's border is visually distinct (dashed) from every real
    /// motif/chips slot (solid) — the phase 50 UI spec's "so the absence is visually distinct even
    /// before the text is read".
    #[test]
    fn thumbnail_border_is_dashed_only_for_absence_and_unknown() {
        assert_eq!(
            thumbnail_border(ThumbnailKind::Absence),
            "1px dashed var(--border)"
        );
        assert_eq!(
            thumbnail_border(ThumbnailKind::Unknown),
            "1px dashed var(--border)"
        );
        for kind in [
            ThumbnailKind::Star,
            ThumbnailKind::TypedEdge,
            ThumbnailKind::RankedBars,
            ThumbnailKind::Chips,
        ] {
            assert_eq!(thumbnail_border(kind), "1px solid var(--border)");
        }
    }
}
