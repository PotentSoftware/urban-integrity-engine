//! The pin control (PIV-05) — one component, mounted in the profile header at both densities.
//!
//! # Two surfaces, one control
//!
//! The phase 45 UI spec asks for a pin control "in the profile header **and** on the canvas inspection
//! panel". Those are not two controls: D-11 fixed that there is exactly one profile renderer, the
//! panel mounts it at `Density::Compact`, and the only difference a pin taken there has is its
//! [`PinOriginView`] — which [`super::pin_origin_for`] derives from the density. A second control
//! for the narrow case is the failure D-11 exists to prevent, and this project has its own worked
//! example of what two copies of one thing cost (the Cosmograph source/bundle drift).
//!
//! The panel has a **second** mount site, and it is not a duplicate: 45-11 measured that 71.3% of
//! the nodes on this canvas carry a class with no profile route, so the panel's no-profile branch
//! is the *common* case there. A pin is a reference to an id and needs no profile, so that branch
//! mounts the same control — otherwise "pin an entity from the canvas" would be unavailable for
//! seven canvas clicks in ten.
//!
//! # Pinning with nothing open prompts inline
//!
//! Never a navigation. The spec is explicit and the reason is the analyst's place in the work: the
//! pin is taken *while reading a profile*, and a control that answered it by leaving the page would
//! lose the reading position to save the subject. The prompt lists what already exists (so a second
//! investigation is not created by accident when the analyst meant the one from ten minutes ago)
//! and offers a name field beside it.

use leptos::prelude::*;
use urban_integrity_workbench_dto::investigation::{
    InvestigationDetailView, InvestigationSummaryView, PinOriginView,
};

use super::{
    as_at_pin_notice, entity_pin_target, fetch_investigation, fetch_investigations,
    pin_control_label, pin_entity, pin_press, unpin_entity, use_active_investigation,
    ActiveInvestigation, PinPress, INVESTIGATIONS_LOADING, NO_SAVED_INVESTIGATIONS,
    PIN_NAME_CONFIRM, PIN_NAME_LABEL, PIN_NAME_PLACEHOLDER, PIN_PICK_EXISTING,
};

/// Pin, then re-read the investigation. The re-read is deliberate: the control's label and the
/// list's counts are then the **server's** answer rather than a local guess about what the write
/// did, so a write that did not land cannot leave a control claiming it did.
async fn pin_and_reopen(
    investigation_id: &str,
    entity_id: &str,
    origin: PinOriginView,
) -> Result<InvestigationDetailView, String> {
    pin_entity(investigation_id, entity_pin_target(entity_id), origin).await?;
    fetch_investigation(investigation_id).await
}

/// Unpin, then re-read, for the same reason.
async fn unpin_and_reopen(
    investigation_id: &str,
    entity_id: &str,
) -> Result<InvestigationDetailView, String> {
    unpin_entity(investigation_id, entity_pin_target(entity_id)).await?;
    fetch_investigation(investigation_id).await
}

/// PIV-05's pin control.
#[component]
#[allow(non_snake_case)]
pub fn PinControl(
    entity_id: String,
    origin: PinOriginView,
    /// The date window on screen, if one is set. Carried **only** so the control can say the
    /// window is not saved with the pin — it never reaches the pin's identity. Passed explicitly
    /// at both mount sites (the canvas panel has no window at all: 45-13 runs no as-at filter
    /// there), so a site that acquires one later cannot forget to say so.
    as_at: Option<String>,
) -> impl IntoView {
    let active: ActiveInvestigation = use_active_investigation();
    // `StoredValue` rather than a cloned `String` per closure: every handler below then captures
    // only `Copy` values, which is what lets them be reused inside a reactive block instead of
    // being moved into the first one that mentions them.
    let entity = StoredValue::new(entity_id);
    let prompting = RwSignal::new(false);
    let existing: RwSignal<Option<Vec<InvestigationSummaryView>>> = RwSignal::new(None);
    let new_name = RwSignal::new(String::new());
    let failure: RwSignal<Option<String>> = RwSignal::new(None);
    let busy = RwSignal::new(false);

    let label = move || entity.with_value(|id| pin_control_label(active.detail.get().as_ref(), id));

    let on_press = move |_| {
        let entity_id = entity.get_value();
        failure.set(None);
        match pin_press(active.detail.get_untracked().as_ref(), &entity_id) {
            PinPress::Prompt => {
                prompting.set(true);
                leptos::task::spawn_local(async move {
                    match fetch_investigations().await {
                        Ok(rows) => existing.set(Some(rows)),
                        Err(message) => {
                            existing.set(Some(Vec::new()));
                            failure.set(Some(message));
                        }
                    }
                });
            }
            PinPress::Pin { investigation_id } => {
                busy.set(true);
                leptos::task::spawn_local(async move {
                    match pin_and_reopen(&investigation_id, &entity_id, origin).await {
                        Ok(detail) => active.detail.set(Some(detail)),
                        Err(message) => failure.set(Some(message)),
                    }
                    busy.set(false);
                });
            }
            PinPress::Unpin { investigation_id } => {
                busy.set(true);
                leptos::task::spawn_local(async move {
                    match unpin_and_reopen(&investigation_id, &entity_id).await {
                        Ok(detail) => active.detail.set(Some(detail)),
                        Err(message) => failure.set(Some(message)),
                    }
                    busy.set(false);
                });
            }
        }
    };

    // Picking an investigation out of the prompt, or naming a new one, both end in the same place:
    // the set is opened and this entity is pinned into it, without leaving the page.
    let choose = move |investigation_id: String| {
        let entity_id = entity.get_value();
        busy.set(true);
        failure.set(None);
        leptos::task::spawn_local(async move {
            match pin_and_reopen(&investigation_id, &entity_id, origin).await {
                Ok(detail) => {
                    active.detail.set(Some(detail));
                    prompting.set(false);
                }
                Err(message) => failure.set(Some(message)),
            }
            busy.set(false);
        });
    };

    let on_create = move |_| {
        let name = new_name.get_untracked().trim().to_string();
        if name.is_empty() {
            return;
        }
        let entity_id = entity.get_value();
        busy.set(true);
        failure.set(None);
        leptos::task::spawn_local(async move {
            match super::create_investigation(name, String::new()).await {
                Ok(created) => match pin_and_reopen(&created.id, &entity_id, origin).await {
                    Ok(detail) => {
                        active.detail.set(Some(detail));
                        new_name.set(String::new());
                        prompting.set(false);
                    }
                    Err(message) => failure.set(Some(message)),
                },
                Err(message) => failure.set(Some(message)),
            }
            busy.set(false);
        });
    };

    let notice = as_at_pin_notice(as_at.as_deref());
    let open_href = move || {
        active
            .detail
            .get()
            .map(|detail| super::investigation_href(&detail.investigation.id))
    };
    let open_label = move || {
        active
            .detail
            .get()
            .map(|detail| format!("Open {}", detail.investigation.name))
    };

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs); align-items: flex-start;">
            <div style="display: flex; align-items: center; gap: var(--space-sm); flex-wrap: wrap;">
                <button
                    type="button"
                    id="pin-to-investigation"
                    class="text-body control-button"
                    on:click=on_press
                    disabled=move || busy.get()
                    style="display: inline-flex; align-items: center; gap: var(--space-xs); padding: 0 var(--space-md);"
                >
                    <svg width="16" height="16" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
                        <path d="M12.5 2.5 17.5 7.5 14.5 8.5 11 12 10 17 3 10l5-1 3.5-3.5Z"></path>
                    </svg>
                    {label}
                </button>
                {move || open_href().map(|href| view! {
                    <leptos_router::components::A
                        href=href
                        attr:class="text-body"
                        attr:style="color: var(--primary); min-height: 44px; display: inline-flex; align-items: center;"
                    >
                        {open_label()}
                    </leptos_router::components::A>
                })}
            </div>
            {notice.map(|text| view! {
                <p class="text-label" role="note" style="margin: 0; color: var(--muted-foreground); max-width: 46ch;">
                    {text}
                </p>
            })}
            {move || failure.get().map(|message| view! {
                <p class="text-body" role="alert" style="margin: 0; color: var(--destructive);">{message}</p>
            })}
            {move || prompting.get().then(|| {
                view! {
                    <div style="display: flex; flex-direction: column; gap: var(--space-sm); padding: var(--space-md); background: var(--card); border: 1px solid var(--border); border-radius: 8px; min-width: 260px;">
                        {move || match existing.get() {
                            None => view! {
                                <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                                    {INVESTIGATIONS_LOADING}
                                </p>
                            }.into_any(),
                            Some(rows) if rows.is_empty() => view! {
                                <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                                    {NO_SAVED_INVESTIGATIONS}
                                </p>
                            }.into_any(),
                            Some(rows) => {
                                view! {
                                    <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                        <span class="text-label" style="color: var(--muted-foreground);">{PIN_PICK_EXISTING}</span>
                                        {rows.into_iter().map(|row| {
                                            let id = row.investigation.id.clone();
                                            let name = row.investigation.name.clone();
                                            let counts = super::member_count_clause(row.entity_count, row.edge_count);
                                            view! {
                                                <button
                                                    type="button"
                                                    class="text-body control-button"
                                                    on:click=move |_| choose(id.clone())
                                                    style="text-align: left;"
                                                >
                                                    {name}
                                                    <span style="color: var(--muted-foreground);">" \u{2014} "{counts}</span>
                                                </button>
                                            }
                                        }).collect_view()}
                                    </div>
                                }.into_any()
                            }
                        }}
                        <label class="text-label" for="pin-new-investigation-name" style="color: var(--muted-foreground);">
                            {PIN_NAME_LABEL}
                        </label>
                        <input
                            id="pin-new-investigation-name"
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
                            on:click=on_create
                            disabled=move || busy.get() || new_name.get().trim().is_empty()
                            style="min-height: 44px; border: none; border-radius: 6px; background: var(--primary); color: var(--primary-foreground); cursor: pointer;"
                        >
                            {PIN_NAME_CONFIRM}
                        </button>
                    </div>
                }
            })}
        </div>
    }
}
