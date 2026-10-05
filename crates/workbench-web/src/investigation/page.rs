//! The investigation list (`/investigations`) and one reopened investigation
//! (`/investigations/:id`) — PIV-05's "reachable from the shell navigation" and "reopening restores
//! the pinned set and every note".
//!
//! # Reopening is one request, and it is the requirement's real test
//!
//! `GET /api/investigation?id=…` returns the record, its counts, every member and every note. The
//! page holds none of it between visits: a reload re-reads it, and a workbench restart changes
//! nothing, because the investigation is in the **analysis database**. That is not an
//! implementation preference — phases 47 and 48 consume this primitive server-side, where a
//! browser's local storage is not merely inconvenient but invisible.
//! `crates/workbench/tests/investigation_api.rs` is where the claim is actually proved, across two
//! separately-built routers over one store.
//!
//! # Reopening also OPENS it
//!
//! Landing on this page sets the investigation as the session's open one, so the next pin taken
//! from a profile lands here rather than prompting again. That is the loop PIV-05 describes —
//! reopen, keep reading, keep pinning — and without it "reopen" would mean "look at".

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use urban_integrity_workbench_dto::investigation::{
    InvestigationDetailView, InvestigationSummaryView,
};

use crate::breadcrumb::Breadcrumb;
use crate::timestamp::Timestamp;

use super::notes::{NoteField, NoteTarget};
use super::{
    create_investigation, delete_confirm, delete_investigation, entity_pin_target,
    fetch_investigation, fetch_investigations, investigation_href, member_count_clause,
    origin_clause, pin_entity_id, pin_no_profile_reason, pinned_entities, unpin_entity,
    use_active_investigation, DELETE_ACTION, DELETE_CANCEL, DELETE_OPEN, INVESTIGATIONS_LOADING,
    INVESTIGATION_LOADING, LIST_HEADING, MEMBERS_HEADING, NOTES_LABEL, NOTES_PLACEHOLDER,
    NO_INVESTIGATIONS_NEXT_STEP, NO_SAVED_INVESTIGATIONS, PIN_NAME_LABEL, PIN_NAME_PLACEHOLDER,
    PIN_NOTE_LABEL, PIN_NOTE_PLACEHOLDER,
};
use crate::profile::pivot::pivot_href_by_id;

/// The list's own create control says what it makes, not "Create".
const LIST_CREATE: &str = "Create an investigation";
/// What a reopened investigation's member list says when the set is empty — a saved investigation
/// with no members is a real state (it was named before anything was pinned into it), and it is
/// not the same statement as "no saved investigations".
const NO_MEMBERS: &str = "Nothing is pinned to this investigation yet.";
/// See [`NO_MEMBERS`].
const NO_MEMBERS_NEXT_STEP: &str =
    "Open an entity and use the pin control in its header; it will land here.";
/// The reopened page's unpin affordance. Not destructive and takes no confirmation
/// (the phase 45 UI spec), so it is named plainly rather than warned about.
const REMOVE_MEMBER: &str = "Remove from this investigation";

/// `/investigations` — every saved investigation in this analysis, most recently modified first.
#[component]
#[allow(non_snake_case)]
pub fn InvestigationsPage() -> impl IntoView {
    let rows: RwSignal<Option<Result<Vec<InvestigationSummaryView>, String>>> = RwSignal::new(None);
    let reload = RwSignal::new(0u32);
    let new_name = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    // Which investigation the confirm step is open for. One at a time, and named: a bare
    // "are you sure" over an unnamed row is how the wrong file gets deleted.
    let confirming: RwSignal<Option<String>> = RwSignal::new(None);

    Effect::new(move |_| {
        reload.get();
        leptos::task::spawn_local(async move {
            rows.set(Some(fetch_investigations().await));
        });
    });

    let on_create = move |_| {
        let name = new_name.get_untracked().trim().to_string();
        if name.is_empty() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            if create_investigation(name, String::new()).await.is_ok() {
                new_name.set(String::new());
            }
            busy.set(false);
            reload.update(|n| *n += 1);
        });
    };

    let on_delete = move |id: String| {
        busy.set(true);
        leptos::task::spawn_local(async move {
            let _outcome = delete_investigation(&id).await;
            confirming.set(None);
            busy.set(false);
            reload.update(|n| *n += 1);
        });
    };

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); max-width: 960px;">
            <Breadcrumb />
            <h1 class="text-display" style="margin: 0;">{LIST_HEADING}</h1>
            {move || match rows.get() {
                None => view! {
                    <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                        {INVESTIGATIONS_LOADING}
                    </p>
                }.into_any(),
                Some(Err(message)) => view! {
                    <p class="text-body" role="alert" style="margin: 0; color: var(--destructive);">
                        {message}
                    </p>
                }.into_any(),
                Some(Ok(list)) if list.is_empty() => view! {
                    <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                        <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                            {NO_SAVED_INVESTIGATIONS}
                        </p>
                        <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                            {NO_INVESTIGATIONS_NEXT_STEP}
                        </p>
                    </div>
                }.into_any(),
                Some(Ok(list)) => view! {
                    <ul style="list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: var(--space-sm);">
                        {list.into_iter().map(|row| {
                            let id = row.investigation.id.clone();
                            let id_for_confirm = id.clone();
                            let id_for_delete = id.clone();
                            let name = row.investigation.name.clone();
                            let name_for_confirm = name.clone();
                            let counts = member_count_clause(row.entity_count, row.edge_count);
                            let updated = row.investigation.updated_at.clone();
                            view! {
                                <li style="display: flex; flex-direction: column; gap: var(--space-xs); background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-md);">
                                    <leptos_router::components::A
                                        href=investigation_href(&id)
                                        attr:class="text-heading"
                                        attr:style="color: var(--primary); text-decoration: none;"
                                    >
                                        {name}
                                    </leptos_router::components::A>
                                    <span class="text-body" style="color: var(--muted-foreground);">{counts}</span>
                                    <Timestamp at=updated />
                                    {move || {
                                        let open_for = confirming.get();
                                        if open_for.as_deref() == Some(id_for_confirm.as_str()) {
                                            let id_for_delete = id_for_delete.clone();
                                            view! {
                                                <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                                    <p class="text-body" role="alert" style="margin: 0; color: var(--destructive);">
                                                        {delete_confirm(&name_for_confirm)}
                                                    </p>
                                                    <div style="display: flex; gap: var(--space-sm);">
                                                        <button
                                                            type="button"
                                                            class="text-body"
                                                            disabled=move || busy.get()
                                                            on:click=move |_| on_delete(id_for_delete.clone())
                                                            style="min-height: 44px; padding: 0 var(--space-md); border: none; border-radius: 6px; background: var(--destructive); color: var(--background); cursor: pointer;"
                                                        >
                                                            {DELETE_ACTION}
                                                        </button>
                                                        <button
                                                            type="button"
                                                            class="text-body"
                                                            on:click=move |_| confirming.set(None)
                                                            style="min-height: 44px; padding: 0 var(--space-md); border: 1px solid var(--border); border-radius: 6px; background: transparent; color: var(--foreground); cursor: pointer;"
                                                        >
                                                            {DELETE_CANCEL}
                                                        </button>
                                                    </div>
                                                </div>
                                            }.into_any()
                                        } else {
                                            let id_for_open = id_for_confirm.clone();
                                            view! {
                                                <button
                                                    type="button"
                                                    class="text-label"
                                                    on:click=move |_| confirming.set(Some(id_for_open.clone()))
                                                    style="min-height: 44px; align-self: flex-start; background: transparent; border: none; color: var(--muted-foreground); cursor: pointer; padding: 0;"
                                                >
                                                    {DELETE_OPEN}
                                                </button>
                                            }.into_any()
                                        }
                                    }}
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                }.into_any(),
            }}
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); max-width: 420px;">
                <label class="text-label" for="new-investigation-name" style="color: var(--muted-foreground);">
                    {PIN_NAME_LABEL}
                </label>
                <input
                    id="new-investigation-name"
                    type="text"
                    aria-label=PIN_NAME_LABEL
                    placeholder=PIN_NAME_PLACEHOLDER
                    prop:value=move || new_name.get()
                    on:input=move |ev| new_name.set(event_target_value(&ev))
                    style="width: 100%; box-sizing: border-box; min-height: 44px; padding: var(--space-xs) var(--space-sm); background: var(--background); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground);"
                />
                <button
                    type="button"
                    class="text-body"
                    disabled=move || busy.get() || new_name.get().trim().is_empty()
                    on:click=on_create
                    style="min-height: 44px; align-self: flex-start; padding: 0 var(--space-md); border: none; border-radius: 6px; background: var(--primary); color: var(--primary-foreground); cursor: pointer;"
                >
                    {LIST_CREATE}
                </button>
            </div>
        </div>
    }
}

/// `/investigations/:id` — one investigation, reopened whole.
#[component]
#[allow(non_snake_case)]
pub fn InvestigationPage() -> impl IntoView {
    let params = use_params_map();
    let active = use_active_investigation();
    let state: RwSignal<Option<Result<InvestigationDetailView, String>>> = RwSignal::new(None);
    let reload = RwSignal::new(0u32);
    let id = move || params.read().get("id").unwrap_or_default();

    Effect::new(move |_| {
        reload.get();
        let id = id();
        state.set(None);
        leptos::task::spawn_local(async move {
            let loaded = fetch_investigation(&id).await;
            // Reopening also OPENS it: the next pin from a profile lands here rather than
            // prompting for a name the analyst already chose.
            if let Ok(detail) = &loaded {
                active.detail.set(Some(detail.clone()));
            }
            state.set(Some(loaded));
        });
    });

    let on_remove = move |entity_id: String| {
        let investigation_id = id();
        leptos::task::spawn_local(async move {
            let _outcome = unpin_entity(&investigation_id, entity_pin_target(&entity_id)).await;
            reload.update(|n| *n += 1);
        });
    };

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); max-width: 960px;">
            {move || {
                let trailing = match state.get() {
                    Some(Ok(detail)) => detail.investigation.name.clone(),
                    _ => String::new(),
                };
                view! { <Breadcrumb trailing=trailing /> }
            }}
            {move || match state.get() {
                None => view! {
                    <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                        {INVESTIGATION_LOADING}
                    </p>
                }.into_any(),
                Some(Err(message)) => view! {
                    <p class="text-body" role="alert" style="margin: 0; color: var(--destructive);">
                        {message}
                    </p>
                }.into_any(),
                Some(Ok(detail)) => {
                    let investigation_id = detail.investigation.id.clone();
                    let counts = member_count_clause(detail.entity_count, detail.edge_count);
                    let members = pinned_entities(&detail)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    view! {
                        <div style="display: flex; flex-direction: column; gap: var(--space-lg);">
                            <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                <h1 class="text-display" style="margin: 0;">{detail.investigation.name.clone()}</h1>
                                <span class="text-body" style="color: var(--muted-foreground);">{counts}</span>
                                <Timestamp at=detail.investigation.updated_at.clone() />
                            </div>
                            <NoteField
                                target=NoteTarget::Investigation { investigation_id: investigation_id.clone() }
                                label=NOTES_LABEL
                                placeholder=NOTES_PLACEHOLDER
                                dom_id="investigation-notes".to_string()
                                initial=detail.investigation.notes.clone()
                            />
                            <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
                                <h2 class="text-heading" style="margin: 0;">{MEMBERS_HEADING}</h2>
                                {if members.is_empty() {
                                    view! {
                                        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                            <p class="text-body" style="margin: 0; color: var(--muted-foreground);">{NO_MEMBERS}</p>
                                            <p class="text-body" style="margin: 0; color: var(--muted-foreground);">{NO_MEMBERS_NEXT_STEP}</p>
                                        </div>
                                    }.into_any()
                                } else {
                                    let investigation_id = investigation_id.clone();
                                    view! {
                                        <ul style="list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: var(--space-md);">
                                            {members.into_iter().map(|pin| {
                                                // `pinned_entities` filtered to entity pins, so
                                                // the `None` arm is unreachable — defaulted rather
                                                // than unwrapped so a future edge row cannot panic
                                                // the page.
                                                let entity_id = pin_entity_id(&pin)
                                                    .unwrap_or_default()
                                                    .to_string();
                                                let href = pivot_href_by_id(&entity_id);
                                                let reason = pin_no_profile_reason(&entity_id);
                                                let id_for_remove = entity_id.clone();
                                                view! {
                                                    <li style="display: flex; flex-direction: column; gap: var(--space-xs); background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-md);">
                                                        {match href {
                                                            Some(href) => view! {
                                                                <leptos_router::components::A
                                                                    href=href
                                                                    attr:class="font-mono text-body"
                                                                    attr:style="color: var(--primary); overflow-wrap: anywhere;"
                                                                >
                                                                    {entity_id.clone()}
                                                                </leptos_router::components::A>
                                                            }.into_any(),
                                                            None => view! {
                                                                <span class="font-mono text-body" style="overflow-wrap: anywhere;">{entity_id.clone()}</span>
                                                            }.into_any(),
                                                        }}
                                                        <span class="text-label" style="color: var(--muted-foreground);">
                                                            {reason.unwrap_or_else(|| origin_clause(pin.origin).to_string())}
                                                        </span>
                                                        <NoteField
                                                            target=NoteTarget::Member {
                                                                investigation_id: investigation_id.clone(),
                                                                target: pin.target.clone(),
                                                            }
                                                            label=PIN_NOTE_LABEL
                                                            placeholder=PIN_NOTE_PLACEHOLDER
                                                            dom_id=format!("pin-note-{}", pin.id)
                                                            initial=pin.note.clone()
                                                        />
                                                        <button
                                                            type="button"
                                                            class="text-label"
                                                            on:click=move |_| on_remove(id_for_remove.clone())
                                                            style="min-height: 44px; align-self: flex-start; background: transparent; border: none; color: var(--muted-foreground); cursor: pointer; padding: 0;"
                                                        >
                                                            {REMOVE_MEMBER}
                                                        </button>
                                                    </li>
                                                }
                                            }).collect_view()}
                                        </ul>
                                    }.into_any()
                                }}
                            </div>
                        </div>
                    }.into_any()
                }
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::investigation::PIN_NAME_CONFIRM;

    /// The four empty statements on this surface are four different facts, and none of them is a
    /// blank rectangle. The phase 45 UI spec's three-way empty-state rule, applied to this page.
    ///
    /// PROVE THIS CAN FAIL: set `NO_MEMBERS` to `NO_SAVED_INVESTIGATIONS` — "you have no
    /// investigations" and "this investigation has nothing in it" are completely different facts
    /// and the assertion fails.
    #[test]
    fn an_empty_list_and_an_empty_investigation_say_different_things() {
        assert_ne!(NO_MEMBERS, NO_SAVED_INVESTIGATIONS);
        assert_ne!(NO_MEMBERS_NEXT_STEP, NO_INVESTIGATIONS_NEXT_STEP);
        // Neither dead end is left without a path (the phase 45 UI spec checker finding 9).
        assert!(NO_INVESTIGATIONS_NEXT_STEP.contains("pin"));
        assert!(NO_MEMBERS_NEXT_STEP.contains("pin control"));
        // And neither is a generic call to action.
        assert!(!LIST_CREATE.eq_ignore_ascii_case("create"));
        assert!(!PIN_NAME_CONFIRM.eq_ignore_ascii_case("create"));
    }
}
