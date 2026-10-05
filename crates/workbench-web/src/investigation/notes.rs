//! Free-text notes (PIV-05) — **one editor**, used for the investigation's own notes and for every
//! pinned member's note.
//!
//! # Why one component and not two
//!
//! They are the same object: a text area, a save on blur, and a save state the analyst can see.
//! The only difference is which route the save goes to, and that is a two-variant enum
//! ([`NoteTarget`]) rather than a second component. Two editors would be two save-state machines,
//! and the one that was used less would be the one that lost work.
//!
//! # The rule this module exists to keep
//!
//! The phase 45 UI spec: *"Notes are analyst work product — losing them silently is not acceptable."* So:
//!
//! * **"Saved" is only ever shown over a write that returned a row.** 45-07's write layer makes a
//!   note that matched no row an `Err`, 45-08's handler turns that into a 404, and
//!   [`NoteSave::Failed`] is what this module does with it. A cheerful state over a failed write is
//!   the shape of the two incidents this project has already shipped (the ICIJ adapter's 814k
//!   phantom rows, the PPD enrichment's "2795151 written" against 0 staged).
//! * **A failed save keeps the text on screen**, and says so in the copy. The field is bound to a
//!   signal the failure path does not touch, so the analyst's words are still there to copy out.
//! * **A blur that changed nothing sends nothing.** `PATCH /api/investigation` with neither field
//!   set is a 400 by design — "saved" over a request that changed nothing is exactly the false
//!   reassurance this surface must not give — and [`note_save_request`] is where that is decided,
//!   before a request exists.

use leptos::prelude::*;
use urban_integrity_workbench_dto::investigation::{
    PinNoteRequest, PinTargetView, UpdateInvestigationRequest,
};

use super::{patch_json, with_id};

/// Which note is being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteTarget {
    /// The investigation's own notes — `PATCH /api/investigation?id=…`.
    Investigation { investigation_id: String },
    /// One pinned member's note — `PATCH /api/investigation/pin?id=…`.
    Member {
        investigation_id: String,
        target: PinTargetView,
    },
}

/// The save state, as the analyst sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteSave {
    /// Nothing has been typed since the last agreement with the server.
    Rest,
    /// Edited, not yet saved. Blur saves it.
    Edited,
    Saving,
    Saved,
    /// The server refused or was unreachable. The text is still in the field.
    Failed(String),
}

/// The phase 45 UI spec's checker recommendation 7: *"the notes save-state has no copy — the one thing
/// this document calls analyst work product requires 'the save state visible' but declares no
/// strings."* Authored here.
pub const NOTE_EDITED: &str = "Not saved yet \u{2014} leave the field to save it.";
/// See [`NOTE_EDITED`].
pub const NOTE_SAVING: &str = "Saving\u{2026}";
/// See [`NOTE_EDITED`].
pub const NOTE_SAVED: &str = "Saved";

/// The failure sentence. It says the write did not land, gives the server's own reason, and tells
/// the analyst their text is still on screen — because at that moment the only copy of it is.
pub fn note_save_failed(reason: &str) -> String {
    format!(
        "Not saved. {reason} Your text is still in the field \u{2014} copy it somewhere before \
         leaving this page."
    )
}

/// What the save state renders as. `Rest` renders nothing: a permanent "Saved" badge over a field
/// nobody has touched is a claim about a write that did not happen this session.
pub fn note_save_label(state: &NoteSave) -> Option<String> {
    match state {
        NoteSave::Rest => None,
        NoteSave::Edited => Some(NOTE_EDITED.to_string()),
        NoteSave::Saving => Some(NOTE_SAVING.to_string()),
        NoteSave::Saved => Some(NOTE_SAVED.to_string()),
        NoteSave::Failed(reason) => Some(note_save_failed(reason)),
    }
}

/// Whether the save state is one the analyst must act on. Used for the colour, and asserted rather
/// than inferred from the string.
pub fn note_save_is_failure(state: &NoteSave) -> bool {
    matches!(state, NoteSave::Failed(_))
}

/// **What a blur sends, decided before a request exists.**
///
/// `None` when the text is unchanged: the update route rejects a request that changes nothing, and
/// it is right to — but a UI that sent one anyway would render that 400 as a failure over work the
/// analyst never did.
///
/// Comparison is exact, not trimmed. A trailing newline the analyst added is a change they made,
/// and a save that silently discarded it would be a save that did not store what is on screen.
pub fn note_save_request(original: &str, current: &str) -> Option<String> {
    (original != current).then(|| current.to_string())
}

/// **Where a note saves to, and what it carries — the whole difference between the two note
/// surfaces, as one pure function.**
///
/// It returns the url and the body together because they are one decision. A `match` inside the
/// transport would be the same code with nothing able to look at it, and the failure it hides is
/// specific: a member's note sent to `/api/investigation` would overwrite the whole
/// investigation's notes with one member's, and every test of the editor would stay green.
///
/// The bodies are serialised from the typed requests rather than hand-built, so the wire shape is
/// still gated by `workbench-dto` — including `UpdateInvestigationRequest`'s explicit `name: null`,
/// which is what tells this route to leave the name alone rather than that a client forgot it.
pub fn note_patch(target: &NoteTarget, notes: String) -> (String, serde_json::Value) {
    match target {
        NoteTarget::Investigation { investigation_id } => (
            with_id(super::INVESTIGATION_URL, investigation_id),
            serde_json::to_value(UpdateInvestigationRequest {
                name: None,
                notes: Some(notes),
            })
            .expect("an update request always serialises"),
        ),
        NoteTarget::Member {
            investigation_id,
            target,
        } => (
            with_id(super::PIN_URL, investigation_id),
            serde_json::to_value(PinNoteRequest {
                target: target.clone(),
                note: notes,
            })
            .expect("a note request always serialises"),
        ),
    }
}

/// Perform the save for either target.
async fn save_note(target: NoteTarget, notes: String) -> Result<(), String> {
    let (url, body) = note_patch(&target, notes);
    patch_json(&url, &body).await.map(|_| ())
}

/// The one notes editor. Saves on blur, shows its state, and never clears the field.
#[component]
#[allow(non_snake_case)]
pub fn NoteField(
    target: NoteTarget,
    /// The field's own label — the investigation's notes and a member's note are different things
    /// and say so.
    label: &'static str,
    placeholder: &'static str,
    /// A stable id for the label's `for` attribute and for the sweeps.
    dom_id: String,
    /// What the server currently holds.
    initial: String,
) -> impl IntoView {
    let target = StoredValue::new(target);
    let text = RwSignal::new(initial.clone());
    // What the server is known to hold. Advanced only by a save that returned, so a failed save
    // leaves the next blur still seeing a change to send.
    let stored = RwSignal::new(initial);
    let state = RwSignal::new(NoteSave::Rest);

    let on_blur = move |_| {
        let current = text.get_untracked();
        let Some(to_save) = note_save_request(&stored.get_untracked(), &current) else {
            // Nothing changed. Say nothing: a "Saved" here would be a claim about a write that
            // never happened.
            if state.get_untracked() == NoteSave::Edited {
                state.set(NoteSave::Rest);
            }
            return;
        };
        state.set(NoteSave::Saving);
        let target = target.get_value();
        leptos::task::spawn_local(async move {
            match save_note(target, to_save.clone()).await {
                Ok(()) => {
                    stored.set(to_save);
                    state.set(NoteSave::Saved);
                }
                Err(reason) => state.set(NoteSave::Failed(reason)),
            }
        });
    };

    let label_id = format!("{dom_id}-state");
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <label class="text-label" for=dom_id.clone() style="color: var(--muted-foreground);">
                {label}
            </label>
            <textarea
                id=dom_id
                rows="3"
                placeholder=placeholder
                prop:value=move || text.get()
                on:input=move |ev| {
                    text.set(event_target_value(&ev));
                    state.set(NoteSave::Edited);
                }
                on:blur=on_blur
                style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); background: var(--background); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); font: inherit; resize: vertical;"
            ></textarea>
            // `aria-live="polite"`, the pattern the phase 44 UI spec established for exactly this: the
            // save state is the one thing on screen that changes without the analyst looking.
            <span
                id=label_id
                class="text-label"
                aria-live="polite"
                style=move || {
                    let colour = if note_save_is_failure(&state.get()) {
                        "var(--destructive)"
                    } else {
                        "var(--muted-foreground)"
                    };
                    format!("min-height: 1.2em; color: {colour};")
                }
            >
                {move || note_save_label(&state.get()).unwrap_or_default()}
            </span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A blur that changed nothing sends nothing, and a blur that changed something sends
    /// exactly what is on screen.**
    ///
    /// PROVE THIS CAN FAIL: return `Some(current.to_string())` unconditionally — the unchanged
    /// case fails, and the editor starts sending a request the route answers 400.
    #[test]
    fn a_blur_that_changed_nothing_sends_nothing() {
        assert_eq!(note_save_request("same", "same"), None);
        assert_eq!(note_save_request("", ""), None);
        assert_eq!(
            note_save_request("", "the applicant"),
            Some("the applicant".to_string())
        );
        assert_eq!(
            note_save_request("the applicant", ""),
            Some(String::new()),
            "clearing a note is a change, and must be saved as one"
        );
        assert_eq!(
            note_save_request("a note", "a note\n"),
            Some("a note\n".to_string()),
            "comparison is exact: a trailing newline the analyst typed is a change they made"
        );
    }

    /// **"Saved" is never shown over a write that did not land, and a failure says the text is
    /// still there.**
    ///
    /// PROVE THIS CAN FAIL: map `NoteSave::Failed` to `Some(NOTE_SAVED.to_string())` — the failure
    /// assertions fail. Map `NoteSave::Rest` to `Some(NOTE_SAVED…)` and the resting assertion
    /// fails, which is the permanent badge over a write nobody made.
    #[test]
    fn the_save_state_says_saved_only_for_a_write_that_returned() {
        assert_eq!(note_save_label(&NoteSave::Rest), None);
        assert_eq!(
            note_save_label(&NoteSave::Edited).as_deref(),
            Some(NOTE_EDITED)
        );
        assert_eq!(
            note_save_label(&NoteSave::Saving).as_deref(),
            Some(NOTE_SAVING)
        );
        assert_eq!(
            note_save_label(&NoteSave::Saved).as_deref(),
            Some(NOTE_SAVED)
        );

        let failed = NoteSave::Failed(
            "the note was not saved — that member is not pinned to inv0001".to_string(),
        );
        let text = note_save_label(&failed).expect("a failure is always stated");
        assert!(
            !text.contains(NOTE_SAVED),
            "a failure must not read as a save: {text}"
        );
        assert!(
            text.contains("that member is not pinned"),
            "the server's own reason must reach the analyst: {text}"
        );
        assert!(
            text.contains("still in the field"),
            "and the analyst must be told their words are still recoverable: {text}"
        );
        assert!(note_save_is_failure(&failed));
        assert!(!note_save_is_failure(&NoteSave::Saved));
    }

    /// **The two note surfaces are one editor pointed at two routes, and a member's note never
    /// reaches the investigation's.**
    ///
    /// PROVE THIS CAN FAIL: point the `Member` arm at `INVESTIGATION_URL` — a member's note would
    /// overwrite the whole investigation's notes on every blur, and the url assertions fail. Drop
    /// the `target` from the member body and the body assertion fails: the note would then be
    /// applied to whichever member the server guessed.
    #[test]
    fn one_editor_saves_to_the_route_its_target_names() {
        let investigation = NoteTarget::Investigation {
            investigation_id: "inv0001".to_string(),
        };
        let member = NoteTarget::Member {
            investigation_id: "inv0001".to_string(),
            target: super::super::entity_pin_target("GB-COH:X06999001"),
        };

        let (inv_url, inv_body) = note_patch(&investigation, "what this set is for".to_string());
        let (mem_url, mem_body) = note_patch(&member, "the applicant".to_string());

        assert_eq!(inv_url, "/api/investigation?id=inv0001");
        assert_eq!(mem_url, "/api/investigation/pin?id=inv0001");
        assert_ne!(inv_url, mem_url);

        // A notes-only update carries the name key as an explicit null: an omitted key and a null
        // mean different things to this route (`required_option`), and only the null says
        // "leave the name alone".
        assert_eq!(inv_body["notes"], "what this set is for");
        assert!(
            inv_body["name"].is_null() && inv_body.get("name").is_some(),
            "the name key must be PRESENT and null: {inv_body}"
        );

        // A member's note names the member it is about.
        assert_eq!(mem_body["note"], "the applicant");
        assert_eq!(mem_body["target"]["entity_id"], "GB-COH:X06999001");
        assert_eq!(mem_body["target"]["kind"], "entity");
    }
}
