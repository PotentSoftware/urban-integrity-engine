//! `named_entity_chips.rs` — the compact, thumbnail-scale fallback for the 11 `Chips`-kind probes
//! (D-10): a wrapped row of small pills, one per named entity, up to an 8-item budget, with a
//! "+{n} more" note beneath when the probe names more than that.
//!
//! **This is explicitly not a shape claim** (D-10, the phase 50 UI spec's "Named-entity chips"
//! section): no lines connect the chips, no positional meaning is implied — it is a bag of real,
//! clickable ids, nothing more, nothing less. Each chip is name-only at this scale (no `(id)`
//! fragment, no id-copy affordance) — that full `name (id)` form is `EntityRefView`'s, reused
//! unchanged by the detail route's Evidence section (plan 50-07), not by this thumbnail-scale
//! component. Reuses [`crate::profile::pivot::pivot_href`] directly for the click-through policy
//! rather than re-deriving it — one pivot policy, per that module's own doc comment.
//!
//! Follows `verdict_badge.rs`'s pure-fn-then-thin-wrapper split — see that file's own top-of-file
//! doc comment for the rationale (no headless-browser test runner in this workspace).
//!
//! Mounted by `finding_card.rs`'s `Chips`-kind thumbnail dispatch (plan 50-06 Task 2), itself
//! reached from `finding_board.rs` -> `analysis_page.rs` -> `app.rs` (plan 50-06 Task 3).

use leptos::prelude::*;
use urban_integrity_workbench_dto::EntityRef;

use crate::motif_star::star_truncation_note;
use crate::profile::pivot::pivot_href;

/// How many chips the thumbnail budget draws — the same 8-item budget the graph motifs use
/// (the phase 50 UI spec: "one consistent 'how much fits in a thumbnail' rule across the whole board").
const CHIP_BUDGET: usize = 8;

/// The "+{n} more" truncation note (Copywriting Contract) — `None` when every real ref was drawn.
/// Reuses `motif_star.rs`'s exact `"+{n} more"` shape rather than redefining a third copy of the
/// same one-line function (this task's own instruction).
pub fn chips_truncation_note(real_count: usize, drawn_count: usize) -> Option<String> {
    star_truncation_note(real_count, drawn_count)
}

/// The chip pill's full inline style — `--card-hover` background, 1px `--border`, `9999px`
/// radius, `xs`/`sm` padding, 44×44px minimum hit target on the chip's whole clickable area
/// (the phase 50 UI spec's "Named-entity chips" section).
///
/// `max-width: 100%` + `overflow: hidden` (WR-05, phase 50 code review): a real registered company name
/// can be far wider than the thumbnail, and without a bound the pill grows past the card instead
/// of wrapping onto its own row. The ellipsis itself lives on the inner label
/// ([`chip_label_style`]) — `text-overflow` does not apply to a flex container's own text.
fn chip_style() -> &'static str {
    "display: inline-flex; align-items: center; min-height: 44px; max-width: 100%; \
     box-sizing: border-box; overflow: hidden; background: var(--card-hover); \
     border: 1px solid var(--border); border-radius: 9999px; padding: var(--space-xs) var(--space-sm); \
     white-space: nowrap;"
}

/// The inner label's style: the element that actually clips and ellipsises a long name (WR-05).
/// The full name is never lost — each chip carries it as its `title`, the same way
/// `motif_star.rs`/`motif_typed_edge.rs` keep the untruncated name on their labels.
fn chip_label_style() -> &'static str {
    "min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;"
}

/// Renders up to [`CHIP_BUDGET`] of `refs` as pills — an `<a>` (via [`pivot_href`]) when the
/// target's class opens a profile, else a plain `<span>` — name only, no id fragment, no id-copy
/// affordance. Given zero refs, renders nothing: the caller (`finding_card.rs`) is responsible
/// for choosing the absence state instead of calling this component with an empty list.
#[component]
#[allow(non_snake_case)]
pub fn NamedEntityChips(refs: Vec<EntityRef>) -> impl IntoView {
    let real_count = refs.len();
    let drawn: Vec<EntityRef> = refs.into_iter().take(CHIP_BUDGET).collect();
    let note = chips_truncation_note(real_count, drawn.len());

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs); width: 100%;">
            <div style="display: flex; flex-wrap: wrap; gap: var(--space-sm); width: 100%;">
                {drawn.into_iter().map(|entity| {
                    let href = pivot_href(Some(&entity), None);
                    let name = entity.name.clone();
                    let title = name.clone();
                    match href {
                        Some(href) => view! {
                            <a class="text-body" href=href title=title style=format!("{} color: var(--primary);", chip_style())>
                                <span style=chip_label_style()>{name}</span>
                            </a>
                        }.into_any(),
                        None => view! {
                            <span class="text-body" title=title style=chip_style()>
                                <span style=chip_label_style()>{name}</span>
                            </span>
                        }.into_any(),
                    }
                }).collect_view()}
            </div>
            {note.map(|text| view! {
                <div class="text-label" style="color: var(--muted-foreground);">{text}</div>
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_note_is_none_when_everything_real_was_drawn() {
        assert_eq!(chips_truncation_note(8, 8), None);
        assert_eq!(chips_truncation_note(3, 8), None);
    }

    /// PROVE THIS CAN FAIL: compute `n` as `drawn_count - real_count` (swapped operands) and this
    /// fails with an overflow panic instead of `"+5 more"`.
    #[test]
    fn truncation_note_reports_the_exact_delta() {
        assert_eq!(chips_truncation_note(13, 8), Some("+5 more".to_string()));
    }

    /// WR-05: the pill is bounded by its row and the label inside it clips with an ellipsis, so
    /// one very long real name cannot push the chip row past the card.
    ///
    /// PROVE THIS CAN FAIL: drop `max-width: 100%` from `chip_style` (the pre-WR-05 style) and the
    /// first assertion fails.
    #[test]
    fn a_long_name_is_bounded_by_the_pill_and_ellipsised_by_its_label() {
        let pill = chip_style();
        assert!(pill.contains("max-width: 100%"), "{pill}");
        assert!(pill.contains("overflow: hidden"), "{pill}");
        let label = chip_label_style();
        assert!(label.contains("text-overflow: ellipsis"), "{label}");
        assert!(label.contains("overflow: hidden"), "{label}");
        assert!(label.contains("min-width: 0"), "{label}");
    }

    #[test]
    fn chips_truncation_note_matches_star_truncation_note_exactly() {
        // Same function, not a second copy — pinned so a future edit to one cannot silently
        // diverge from the other.
        for (real, drawn) in [(0usize, 0usize), (5, 5), (9, 8), (100, 8)] {
            assert_eq!(
                chips_truncation_note(real, drawn),
                star_truncation_note(real, drawn)
            );
        }
    }
}
