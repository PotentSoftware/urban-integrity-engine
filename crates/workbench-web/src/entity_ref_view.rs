//! `EntityRefView` — the one way an `EntityRef` reaches the screen outside the profile's own
//! tables, and, since plan 45-11 Task 2, **a click-through to that entity's profile wherever the
//! target's class has one**.
//!
//! # The contract this supersedes, and why
//!
//! The phase 43 UI spec's "Copyable identifier" section said: never an `<a>` tag, never `cursor:
//! pointer` on the name/id text. That was written in a milestone with **no profile page to link
//! to** — this module's own doc comment has said "Phase 45 turns these into links" since it was
//! written, and `entity_ref_display` was left as "the single seam Phase 45 will change". PIV-03
//! ("every … report evidence row and graph result on screen is a click-through to a profile") is
//! that change. The copyable half is untouched: the id still renders in `.font-mono` beside its
//! `<CopyButton>`, and it is the **name** that becomes the anchor.
//!
//! # Which references become links
//!
//! Not this module's decision — [`crate::profile::pivot::pivot_href`] owns it, identically here and
//! on the profile screen, and returns `None` for a class with no profile route so the name renders
//! as plain text rather than as a dead link. Every call site is swept by
//! `tests/pivot_reachable.rs`.
//!
//! # No `stop_propagation` on the anchor
//!
//! `graph_results_list.rs` wraps each result row in an `on:click` that selects it, and the obvious
//! tidy-up — stopping the anchor's click from bubbling to it — would **break client-side
//! routing**. `leptos_router` intercepts anchor clicks with a listener on `window`
//! (`location::handle_anchor_click`), i.e. at the very end of the bubble path, so a
//! `stop_propagation` anywhere below it turns every pivot into a full document navigation: a wasm
//! reload per hop, and the pivot history 45-11 Task 3 builds reduced to page loads. The row
//! selecting itself on the way out is harmless — the page is leaving.
#![allow(dead_code)]

use crate::copy_button::CopyButton;
use crate::profile::pivot::pivot_href;
use leptos::prelude::*;
use urban_integrity_workbench_dto::EntityRef;

/// Splits an `EntityRef` into its `(name, id)` display fragments. The two fragments are returned
/// separately (never concatenated into one un-splittable string) because the component styles them
/// differently (`name` in `.text-body` or as the anchor, `(id)` in `.font-mono`/
/// `--muted-foreground`).
pub fn entity_ref_display(r: &EntityRef) -> (String, String) {
    (r.name.clone(), r.id.clone())
}

/// Renders `{name} ({id})` followed by a `<CopyButton value=id />`, with `{name}` as a link to the
/// entity's profile when its class has one.
///
/// `as_at` carries a date window across the pivot where the surface has one; the Analysis and Graph
/// pages do not, and pass nothing. `no_link` suppresses the anchor — see [`EntityRefView`]'s one
/// opt-out below, which `tests/pivot_reachable.rs` pins by name so a second one cannot appear
/// quietly.
#[component]
#[allow(non_snake_case, dead_code)]
pub fn EntityRefView(
    entity: EntityRef,
    /// The as-at date to carry onto the profile link, for a surface that has one. The profile
    /// screen's own rows do not use this component; this exists so that a future dated surface
    /// does not have to reach past `pivot_href` to build its own href.
    #[prop(optional, into)]
    as_at: Option<String>,
    /// Render the name as text even where it would open. **One call site sets this**: the entity
    /// picker's resolved chip (`entity_picker.rs`), where the reference is a form control's current
    /// value sitting immediately beside its "Clear" button — a mis-click there would navigate out
    /// of a half-composed graph query, and the analyst reached that chip by searching for the
    /// entity in the first place.
    #[prop(optional)]
    no_link: bool,
) -> impl IntoView {
    let (name, id) = entity_ref_display(&entity);
    let id_for_copy = id.clone();
    let href = (!no_link)
        .then(|| pivot_href(Some(&entity), as_at.as_deref()))
        .flatten();

    view! {
        <span style="display: inline-flex; flex-wrap: wrap; align-items: baseline; gap: var(--space-xs); min-width: 0; max-width: 100%; overflow-wrap: anywhere;">
            {match href {
                Some(href) => view! {
                    <a class="text-body" href=href style="color: var(--primary); text-decoration: underline; text-underline-offset: var(--space-xs); min-height: 44px; display: inline-flex; align-items: center;">{name}</a>
                }.into_any(),
                None => view! { <span class="text-body">{name}</span> }.into_any(),
            }}
            <span class="font-mono" style="color: var(--muted-foreground);">"(" {id} ")"</span>
            <CopyButton value=id_for_copy />
        </span>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EntityRef {
        EntityRef {
            id: "director:jane-smith".to_string(),
            name: "Jane Smith".to_string(),
            kind: "Entity".to_string(),
        }
    }

    #[test]
    fn entity_ref_display_returns_name_and_id_as_separate_fragments() {
        let (name, id) = entity_ref_display(&sample());
        assert_eq!(name, "Jane Smith");
        assert_eq!(id, "director:jane-smith");
    }

    /// The two answers the shared policy gives this component's own shapes.
    ///
    /// **This test cannot prove the component calls `pivot_href`, and it was written believing it
    /// could.** Replacing the call with `profile_href(&entity.id, …)` — the mutation that would ship
    /// a link on every ward, contract and address in the report — leaves it green, because it asks
    /// the function rather than the render tree. That is Phase 43's zero-call-site `Abbr` in
    /// miniature. `tests/pivot_reachable.rs`'s
    /// `the_shared_pivot_policy_is_the_only_href_builder_a_surface_calls` is what actually reddens
    /// under it; this stays as a statement about the policy's answers, honestly labelled.
    #[test]
    fn the_shared_policy_answers_this_components_two_shapes() {
        assert_eq!(
            pivot_href(Some(&sample()), None).as_deref(),
            Some("/entity/director%3Ajane-smith")
        );
        let ward = EntityRef {
            id: "ward:X05999001".to_string(),
            name: "Example Ward".to_string(),
            kind: "Ward".to_string(),
        };
        assert_eq!(pivot_href(Some(&ward), None), None);
    }
}
