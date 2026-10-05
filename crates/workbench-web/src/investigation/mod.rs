//! Saved investigations in the browser (PIV-05) — the pin control, the notes editors, the
//! investigation list and the reopen page.
//!
//! # What this module is, and what it is not
//!
//! It is the **web surface** over two things that already exist and are not rebuilt here: plan
//! 45-07's persistence primitive (`crates/core::persistence::saved_investigations`) and plan
//! 45-08's HTTP routes (`crates/workbench::investigation`). Every write this module makes is one
//! of those routes; there is no second idea of what an investigation is anywhere in this crate.
//!
//! # Where the state lives, and why none of it is in the browser
//!
//! The investigation itself — its name, its notes, its members and their notes — lives in the
//! analysis database and is read back over `GET /api/investigation?id=`. `localStorage` is not an
//! option and not a shortcut declined for tidiness: phases 47 and 48 consume this primitive
//! **server-side**, and a dossier export cannot read a browser's local storage. The restart
//! survival test (`crates/workbench/tests/investigation_api.rs`) is the requirement's real test
//! and it would fail immediately against any browser-local store.
//!
//! One thing is session state and is held in memory: **which investigation is currently open**.
//! That is a choice the analyst makes, not work product — it is re-made in one click, it is
//! visible on the control at all times, and persisting it would make a stale pick invisible after
//! a reload. It is a Leptos context ([`ActiveInvestigation`]) provided once by the app shell, so
//! the profile header and the canvas inspection panel are looking at one value rather than two
//! copies that can disagree.
//!
//! # The as-at is deliberately not part of a pin
//!
//! 45-12 §8 recommended storing the as-at window alongside a pin and 45-14 §5 endorsed it on
//! evidence. It is **declined here**, with the reasoning and the condition that would reverse it
//! recorded in the plan 45-15 summary §5. What this module does instead is state the absence where the
//! analyst can see it: [`as_at_pin_notice`] renders beside the control whenever a window is set, so
//! a pin taken under a filter cannot quietly look like a pin of the filtered view.
//!
//! Deduplication is therefore on the entity id **alone**, which is 45-07's behaviour and not a new
//! one: `PinTarget::member_key` is `entity:{id}`, the record id derives from it and a `UNIQUE`
//! index is the second witness. [`entity_pin_target`] is the one place this crate builds that
//! target, and `a_pin_target_is_the_entity_id_and_nothing_else` fails the day anything else joins
//! it.
//!
//! # Testing approach
//!
//! Plain-function-first, the convention this crate documents in `profile/mod.rs` and `tool_rail.rs`:
//! every decision and every string is a pure function over owned DTO values, host-testable with no
//! DOM. The components are thin, and their reachability is proved by source-reading sweeps in
//! `tests/investigation_reachable.rs` — Phase 43 shipped a fully unit-tested component with zero
//! call sites, which is the failure that style of test exists to catch.

pub mod notes;
pub mod page;
pub mod pin;

use leptos::prelude::*;
use urban_integrity_workbench_dto::investigation::{
    CreateInvestigationRequest, DeleteResultView, InvestigationDetailView,
    InvestigationSummaryView, PinOriginView, PinRequest, PinTargetView, SavedInvestigationView,
    SavedPinView, UnpinRequest, UnpinResultView,
};

use crate::profile::pivot::{encode_segment, namespace_class};
use crate::profile::Density;

// ── Routes (one definition each, shared with `canvas_filters.rs`) ──────────────────────────────

/// `GET`/`POST /api/investigations`.
pub const INVESTIGATIONS_URL: &str = "/api/investigations";
/// `GET`/`PATCH`/`DELETE /api/investigation?id=…`.
pub const INVESTIGATION_URL: &str = "/api/investigation";
/// `POST /api/investigation/pin?id=…` (pin) and `PATCH` (the member's note).
pub const PIN_URL: &str = "/api/investigation/pin";
/// `POST /api/investigation/unpin?id=…`.
pub const UNPIN_URL: &str = "/api/investigation/unpin";
/// `POST /api/investigation/subgraph?id=…` — CNV-03's save, called from `canvas_filters.rs`.
pub const SAVE_SUBGRAPH_URL: &str = "/api/investigation/subgraph";

/// `{route}?id={id}`, with the id percent-encoded exactly as every other query value in this
/// crate is. An investigation id is a SurrealDB record key and carries no colon today, but the
/// encoder is not conditional on that continuing to be true.
pub fn with_id(route: &str, id: &str) -> String {
    format!("{route}?id={}", encode_segment(id))
}

/// The client-side route for the investigation list.
pub const INVESTIGATIONS_PATH: &str = "/investigations";

/// The client-side route for one reopened investigation.
pub fn investigation_href(id: &str) -> String {
    format!("{INVESTIGATIONS_PATH}/{}", encode_segment(id))
}

// ── Copy ───────────────────────────────────────────────────────────────────────────────────────

/// The phase 45 UI spec Copy Deck, "Page header pin control (unpinned)".
pub const PIN_TO_INVESTIGATION: &str = "Pin to investigation";

/// The phase 45 UI spec Copy Deck, "Page header pin control (pinned)". The control becomes the unpin
/// affordance; unpinning is not destructive and takes no confirmation.
pub fn pinned_to(name: &str) -> String {
    format!("Pinned to {name}")
}

/// The phase 45 UI spec's checker recommendation 6: *"the pin-naming prompt has no copy — field label,
/// placeholder and confirm-button label are all unspecified for a new flow."* Authored here, and
/// recorded as a deviation in the plan 45-15 summary rather than left to drift.
pub const PIN_NAME_LABEL: &str = "Name a new investigation";
/// See [`PIN_NAME_LABEL`]. Matches the canvas save control's placeholder word for word, because
/// they name the same thing and two wordings would read as two concepts.
pub const PIN_NAME_PLACEHOLDER: &str = "Name this investigation\u{2026}";
/// See [`PIN_NAME_LABEL`]. Names the action and its subject, never a bare "Create".
pub const PIN_NAME_CONFIRM: &str = "Create and pin";
/// The heading over the existing investigations offered in the inline prompt.
pub const PIN_PICK_EXISTING: &str = "Pin to one you already have";

/// The phase 45 UI spec Copy Deck, "Investigation list empty". The checker's recommendation 9 (*"two
/// dead-end states offer no path"*) is answered by [`NO_INVESTIGATIONS_NEXT_STEP`] beneath it.
pub const NO_SAVED_INVESTIGATIONS: &str = "No saved investigations yet.";
/// The affordance the empty state would otherwise lack.
pub const NO_INVESTIGATIONS_NEXT_STEP: &str =
    "Name one below, or pin an entity from its profile and name it there.";

/// The phase 45 UI spec Copy Deck, "Investigation notes placeholder".
pub const NOTES_LABEL: &str = "Notes";
/// The per-member note's own label, distinguished from the investigation's.
pub const PIN_NOTE_LABEL: &str = "Note on this entity";
/// Both note fields say what they are for, because an empty box with a one-word label does not.
pub const NOTES_PLACEHOLDER: &str = "What this set is for, and what you are looking for\u{2026}";
/// See [`NOTES_PLACEHOLDER`].
pub const PIN_NOTE_PLACEHOLDER: &str = "Why this entity is in the set\u{2026}";

/// The list's column headings.
pub const LIST_HEADING: &str = "Saved investigations";
/// The reopened investigation's member heading.
pub const MEMBERS_HEADING: &str = "Pinned";

/// The phase 45 UI spec Copy Deck, "Destructive confirmation" — verbatim, with the name interpolated.
pub fn delete_confirm(name: &str) -> String {
    format!("Delete \"{name}\"? This cannot be undone.")
}
/// The confirm step's action.
pub const DELETE_ACTION: &str = "Delete";
/// The confirm step's escape. **Not** a bare "Cancel": the phase 45 UI spec's checker recommendation 8
/// flags the unqualified label against Phase 44's own "Cancel run" precedent.
pub const DELETE_CANCEL: &str = "Keep this investigation";
/// The control that opens the confirm step.
pub const DELETE_OPEN: &str = "Delete this investigation\u{2026}";

/// Fetch-failure copy, in this crate's established shape (the endpoint is named because an analyst
/// reporting this is reporting a specific route).
pub const INVESTIGATION_FETCH_ERROR: &str =
    "The workbench returned data this page cannot read (/api/investigation).";
/// See [`INVESTIGATION_FETCH_ERROR`].
pub const INVESTIGATIONS_FETCH_ERROR: &str =
    "The workbench returned data this page cannot read (/api/investigations).";
/// A pin that did not land. Never silent: a control that flipped its own label over a failed write
/// would tell the analyst their set contains something it does not.
pub const PIN_FAILED: &str = "Not pinned. The workbench did not accept the pin.";
/// See [`PIN_FAILED`].
pub const UNPIN_FAILED: &str = "Not removed. The workbench did not accept the change.";
/// Loading copy, mirroring "Loading the profile…" / "Loading the graph…".
pub const INVESTIGATIONS_LOADING: &str = "Loading saved investigations\u{2026}";
/// See [`INVESTIGATIONS_LOADING`].
pub const INVESTIGATION_LOADING: &str = "Loading the investigation\u{2026}";

// ── What a pin is ──────────────────────────────────────────────────────────────────────────────

/// **The one place this crate builds an entity pin target.**
///
/// The entity id and nothing else. Not the id and the as-at window, not the id and the surface it
/// was pinned from: the origin travels beside the target, in its own field, precisely so it cannot
/// become part of the identity. `PinTarget::member_key` is `entity:{id}` and the record id derives
/// from it, so an identity with a second component in it would produce two rows for one subject —
/// which is exactly what PIV-05's "pinning the same entity twice does not create two entries"
/// forbids.
pub fn entity_pin_target(entity_id: &str) -> PinTargetView {
    PinTargetView::Entity {
        entity_id: entity_id.to_string(),
    }
}

/// Which surface a pin came from, derived from the density the renderer is mounted at.
///
/// `Density::Full` is `/entity/:id`; `Density::Compact` is the canvas inspection panel. There is
/// one profile renderer (D-11) and therefore one pin control, and this is the whole difference
/// between its two mount sites.
pub fn pin_origin_for(density: Density) -> PinOriginView {
    match density {
        Density::Full => PinOriginView::Profile,
        Density::Compact => PinOriginView::Canvas,
    }
}

/// Whether `entity_id` is already a member of the open investigation.
///
/// Compared against the **target**, not against a rendered string, so a change to how a pin is
/// displayed cannot change what counts as pinned.
pub fn is_pinned(open: Option<&InvestigationDetailView>, entity_id: &str) -> bool {
    let target = entity_pin_target(entity_id);
    open.is_some_and(|detail| detail.pins.iter().any(|pin| pin.target == target))
}

/// What pressing the pin control does, decided before anything is rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinPress {
    /// No investigation is open: prompt for a name inline, and do not navigate away.
    Prompt,
    /// Pin this entity into the open investigation.
    Pin { investigation_id: String },
    /// The entity is already a member: the control is the unpin affordance.
    Unpin { investigation_id: String },
}

/// See [`PinPress`]. A pure decision so the three arms are testable without a DOM.
pub fn pin_press(open: Option<&InvestigationDetailView>, entity_id: &str) -> PinPress {
    match open {
        None => PinPress::Prompt,
        Some(detail) => {
            let investigation_id = detail.investigation.id.clone();
            if is_pinned(open, entity_id) {
                PinPress::Unpin { investigation_id }
            } else {
                PinPress::Pin { investigation_id }
            }
        }
    }
}

/// The control's label: the phase 45 UI spec's two copy rows, and nothing between them.
pub fn pin_control_label(open: Option<&InvestigationDetailView>, entity_id: &str) -> String {
    if is_pinned(open, entity_id) {
        // `is_pinned` is only true when `open` is `Some`.
        let name = open.map(|d| d.investigation.name.as_str()).unwrap_or("");
        pinned_to(name)
    } else {
        PIN_TO_INVESTIGATION.to_string()
    }
}

/// **The as-at window is not saved with a pin, and this is where that is said.**
///
/// Returns `None` when no window is set — a sentence about a filter nobody is running is the
/// defect 45-12 §6.1 names, one surface over. When a window *is* set it states two things: what
/// was pinned (the subject) and what reopening will show (its profile, unfiltered), so an analyst
/// who pinned a company while looking at 2021 cannot come back expecting 2021.
pub fn as_at_pin_notice(as_at: Option<&str>) -> Option<String> {
    let date = as_at.map(str::trim).filter(|d| !d.is_empty())?;
    Some(format!(
        "This pins the entity, not the {date} window. Reopening from the investigation opens this \
         profile with no date set, and pinning it again under a different date keeps one entry."
    ))
}

/// The class a member's id names, when that class has no profile route — so the row can say why
/// it is not a link instead of looking broken.
pub fn pin_no_profile_reason(entity_id: &str) -> Option<String> {
    namespace_class(entity_id).map(|class| format!("{class} — no profile page yet"))
}

/// The entity id a pin points at, or `None` for an edge pin.
///
/// The one reader of `PinTargetView`'s shape, for the same reason `entity_pin_target` is its one
/// writer: a second place that knows what a pin's identity looks like is a second place that can
/// come to disagree about it.
pub fn pin_entity_id(pin: &SavedPinView) -> Option<&str> {
    match &pin.target {
        PinTargetView::Entity { entity_id } => Some(entity_id),
        PinTargetView::Edge { .. } => None,
    }
}

/// The entity members of an investigation, with their notes — what the reopen page renders.
pub fn pinned_entities(detail: &InvestigationDetailView) -> Vec<&SavedPinView> {
    detail
        .pins
        .iter()
        .filter(|pin| pin_entity_id(pin).is_some())
        .collect()
}

/// One list row's count clause. Two counts, never one total: an entity is a subject and an edge is
/// a relationship, and a subgraph save that brought 600 edges must not inflate "how many subjects
/// are in this file".
pub fn member_count_clause(entity_count: u64, edge_count: u64) -> String {
    let entities = if entity_count == 1 {
        "1 entity".to_string()
    } else {
        format!("{entity_count} entities")
    };
    if edge_count == 0 {
        return entities;
    }
    let edges = if edge_count == 1 {
        "1 edge".to_string()
    } else {
        format!("{edge_count} edges")
    };
    format!("{entities}, {edges}")
}

/// Which surface a member arrived from, in a sentence. An entity the analyst pinned by hand off a
/// profile is a different claim from one that arrived inside a bulk subgraph save, and a set that
/// cannot tell them apart reads every member as deliberate (45-07).
pub fn origin_clause(origin: PinOriginView) -> &'static str {
    match origin {
        PinOriginView::Profile => "pinned from its profile",
        PinOriginView::Canvas => "pinned from the canvas",
        PinOriginView::Subgraph => "arrived with a saved subgraph",
        PinOriginView::Alert => "pinned from the alert inbox",
    }
}

// ── The open investigation (session state, deliberately not persisted) ─────────────────────────

/// Which investigation the pin control writes into, provided once by the app shell.
///
/// A `Copy` handle around one signal, so every mount site shares one value. The detail — not just
/// the id — because the control has to know whether *this* entity is already a member, and asking
/// the server on every render would be an N+1 over a set that is already in hand.
#[derive(Debug, Clone, Copy)]
pub struct ActiveInvestigation {
    pub detail: RwSignal<Option<InvestigationDetailView>>,
}

impl Default for ActiveInvestigation {
    fn default() -> Self {
        Self {
            detail: RwSignal::new(None),
        }
    }
}

/// The shell's one `provide_context` call site.
pub fn provide_active_investigation() {
    provide_context(ActiveInvestigation::default());
}

/// The open investigation, or an inert handle when no shell provided one (the sweep proves the
/// shell does; this keeps a stray mount from panicking rather than silently losing a pin).
pub fn use_active_investigation() -> ActiveInvestigation {
    use_context::<ActiveInvestigation>().unwrap_or_default()
}

// ── Requests ───────────────────────────────────────────────────────────────────────────────────

/// The server's own `"error"` text, or `fallback` when there is not one.
///
/// Surfacing the server's sentence matters here for the same reason it does on the canvas save:
/// the refusal copy is written on the server precisely so the analyst reads the real reason.
pub async fn server_error_text(response: gloo_net::http::Response, fallback: &str) -> String {
    match response.json::<serde_json::Value>().await {
        Ok(body) => body["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| fallback.to_string()),
        Err(_) => fallback.to_string(),
    }
}

/// `GET /api/investigations`.
pub async fn fetch_investigations() -> Result<Vec<InvestigationSummaryView>, String> {
    let response = gloo_net::http::Request::get(INVESTIGATIONS_URL)
        .send()
        .await
        .map_err(|_| INVESTIGATIONS_FETCH_ERROR.to_string())?;
    if !response.ok() {
        return Err(server_error_text(response, INVESTIGATIONS_FETCH_ERROR).await);
    }
    response
        .json()
        .await
        .map_err(|_| INVESTIGATIONS_FETCH_ERROR.to_string())
}

/// `POST /api/investigations`.
pub async fn create_investigation(
    name: String,
    notes: String,
) -> Result<SavedInvestigationView, String> {
    let response = gloo_net::http::Request::post(INVESTIGATIONS_URL)
        .json(&CreateInvestigationRequest { name, notes })
        .map_err(|_| INVESTIGATIONS_FETCH_ERROR.to_string())?
        .send()
        .await
        .map_err(|_| INVESTIGATIONS_FETCH_ERROR.to_string())?;
    if !response.ok() {
        return Err(server_error_text(response, INVESTIGATIONS_FETCH_ERROR).await);
    }
    response
        .json()
        .await
        .map_err(|_| INVESTIGATIONS_FETCH_ERROR.to_string())
}

/// `GET /api/investigation?id=…` — **the reopen**: the record, its counts, every member and every
/// note, in one request.
pub async fn fetch_investigation(id: &str) -> Result<InvestigationDetailView, String> {
    let response = gloo_net::http::Request::get(&with_id(INVESTIGATION_URL, id))
        .send()
        .await
        .map_err(|_| INVESTIGATION_FETCH_ERROR.to_string())?;
    if !response.ok() {
        return Err(server_error_text(response, INVESTIGATION_FETCH_ERROR).await);
    }
    response
        .json()
        .await
        .map_err(|_| INVESTIGATION_FETCH_ERROR.to_string())
}

/// `POST /api/investigation/pin?id=…`. `note: None` **preserves** an existing note (45-07), which
/// is what makes a re-pin from a second surface safe.
pub async fn pin_entity(
    investigation_id: &str,
    target: PinTargetView,
    origin: PinOriginView,
) -> Result<SavedPinView, String> {
    let response = gloo_net::http::Request::post(&with_id(PIN_URL, investigation_id))
        .json(&PinRequest {
            target,
            origin,
            note: None,
        })
        .map_err(|_| PIN_FAILED.to_string())?
        .send()
        .await
        .map_err(|_| PIN_FAILED.to_string())?;
    if !response.ok() {
        return Err(server_error_text(response, PIN_FAILED).await);
    }
    response.json().await.map_err(|_| PIN_FAILED.to_string())
}

/// `POST /api/investigation/unpin?id=…`.
pub async fn unpin_entity(
    investigation_id: &str,
    target: PinTargetView,
) -> Result<UnpinResultView, String> {
    let response = gloo_net::http::Request::post(&with_id(UNPIN_URL, investigation_id))
        .json(&UnpinRequest { target })
        .map_err(|_| UNPIN_FAILED.to_string())?
        .send()
        .await
        .map_err(|_| UNPIN_FAILED.to_string())?;
    if !response.ok() {
        return Err(server_error_text(response, UNPIN_FAILED).await);
    }
    response.json().await.map_err(|_| UNPIN_FAILED.to_string())
}

/// One `PATCH` with a body built elsewhere.
///
/// The two note routes differ only in where they go and what they carry, and
/// `notes::note_patch` is the one place that decides both — so the decision is a pure function
/// with a test rather than a `match` inside a transport nothing can inspect.
pub async fn patch_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let response = gloo_net::http::Request::patch(url)
        .json(body)
        .map_err(|_| INVESTIGATION_FETCH_ERROR.to_string())?
        .send()
        .await
        .map_err(|_| INVESTIGATION_FETCH_ERROR.to_string())?;
    if !response.ok() {
        return Err(server_error_text(response, INVESTIGATION_FETCH_ERROR).await);
    }
    response
        .json()
        .await
        .map_err(|_| INVESTIGATION_FETCH_ERROR.to_string())
}

/// `DELETE /api/investigation?id=…` — the one destructive action this phase adds.
pub async fn delete_investigation(investigation_id: &str) -> Result<DeleteResultView, String> {
    let response = gloo_net::http::Request::delete(&with_id(INVESTIGATION_URL, investigation_id))
        .send()
        .await
        .map_err(|_| INVESTIGATION_FETCH_ERROR.to_string())?;
    if !response.ok() {
        return Err(server_error_text(response, INVESTIGATION_FETCH_ERROR).await);
    }
    response
        .json()
        .await
        .map_err(|_| INVESTIGATION_FETCH_ERROR.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::pivot::pivot_href_by_id;

    const COMPANY: &str = "GB-COH:X06999001";
    const DIRECTOR: &str = "director:x9999-example-name";
    const WARD: &str = "ward:X05999001";

    pub(crate) fn investigation(name: &str) -> SavedInvestigationView {
        SavedInvestigationView {
            id: "inv0001".to_string(),
            name: name.to_string(),
            analysis_db: "an_x9999_test".to_string(),
            notes: String::new(),
            schema_version: 1,
            created_at: "2026-09-16T09:00:00Z".to_string(),
            updated_at: "2026-09-16T09:30:00Z".to_string(),
        }
    }

    pub(crate) fn pin(entity_id: &str, note: &str, origin: PinOriginView) -> SavedPinView {
        SavedPinView {
            id: format!("inv0001-entity-{entity_id}"),
            target: entity_pin_target(entity_id),
            note: note.to_string(),
            origin,
            pinned_at: "2026-09-16T09:10:00Z".to_string(),
            updated_at: "2026-09-16T09:10:00Z".to_string(),
        }
    }

    pub(crate) fn detail(name: &str, pins: Vec<SavedPinView>) -> InvestigationDetailView {
        let entity_count = pins
            .iter()
            .filter(|p| matches!(p.target, PinTargetView::Entity { .. }))
            .count() as u64;
        InvestigationDetailView {
            investigation: investigation(name),
            entity_count,
            edge_count: pins.len() as u64 - entity_count,
            pins,
        }
    }

    /// **The identity of a pin is the entity id and nothing else.**
    ///
    /// The third of this plan's three truths, and the one the as-at decision could have broken.
    /// Two pins of the same company taken under two different date windows, from two different
    /// surfaces, must produce the **same** target — because the record id derives from it, and two
    /// different targets would be two rows for one subject.
    ///
    /// PROVE THIS CAN FAIL: make `entity_pin_target` take the as-at and return
    /// `entity_id: format!("{entity_id}@{as_at}")`. The equality fails.
    #[test]
    fn a_pin_target_is_the_entity_id_and_nothing_else() {
        let under_one_window = entity_pin_target(COMPANY);
        let under_another_window = entity_pin_target(COMPANY);
        assert_eq!(under_one_window, under_another_window);
        assert_eq!(
            under_one_window,
            PinTargetView::Entity {
                entity_id: COMPANY.to_string()
            },
            "nothing but the id is in the identity"
        );

        // Non-vacuity: the comparison can tell two subjects apart, so the equality above is not
        // an artefact of a target that ignores its input.
        assert_ne!(entity_pin_target(COMPANY), entity_pin_target(DIRECTOR));

        // And the surface travels beside the target, never inside it.
        assert_eq!(pin_origin_for(Density::Full), PinOriginView::Profile);
        assert_eq!(pin_origin_for(Density::Compact), PinOriginView::Canvas);
    }

    /// The control's three states, decided before anything renders.
    ///
    /// PROVE THIS CAN FAIL: have `pin_press` return `PinPress::Pin` whenever an investigation is
    /// open — the unpin arm fails, and with it the spec's "the control becomes the unpin
    /// affordance".
    #[test]
    fn the_control_prompts_pins_or_unpins_and_says_which_on_its_face() {
        let empty = detail("Ward follow-up", vec![]);
        let holding = detail(
            "Ward follow-up",
            vec![pin(COMPANY, "", PinOriginView::Profile)],
        );

        assert_eq!(pin_press(None, COMPANY), PinPress::Prompt);
        assert_eq!(
            pin_control_label(None, COMPANY),
            "Pin to investigation",
            "the phase 45 UI spec Copy Deck, unpinned"
        );

        assert_eq!(
            pin_press(Some(&empty), COMPANY),
            PinPress::Pin {
                investigation_id: "inv0001".to_string()
            }
        );
        assert_eq!(
            pin_control_label(Some(&empty), COMPANY),
            "Pin to investigation"
        );

        assert_eq!(
            pin_press(Some(&holding), COMPANY),
            PinPress::Unpin {
                investigation_id: "inv0001".to_string()
            }
        );
        assert_eq!(
            pin_control_label(Some(&holding), COMPANY),
            "Pinned to Ward follow-up",
            "the phase 45 UI spec Copy Deck, pinned"
        );

        // Membership is per entity, not per investigation: a set holding one company says nothing
        // about a second one.
        assert!(!is_pinned(Some(&holding), DIRECTOR));
        assert_eq!(
            pin_press(Some(&holding), DIRECTOR),
            PinPress::Pin {
                investigation_id: "inv0001".to_string()
            }
        );
    }

    /// The window is not saved with the pin, and the control says so **only** when a window is set.
    ///
    /// PROVE THIS CAN FAIL: return `Some(...)` unconditionally — the unfiltered assertion fails,
    /// which is 45-12 §6.1's defect (a sentence about a filter nobody is running).
    #[test]
    fn a_pin_taken_under_a_date_window_says_the_window_is_not_saved_with_it() {
        assert_eq!(as_at_pin_notice(None), None);
        assert_eq!(as_at_pin_notice(Some("")), None);
        assert_eq!(as_at_pin_notice(Some("   ")), None);

        let notice = as_at_pin_notice(Some("2021-06-30")).expect("a set window is disclosed");
        assert!(
            notice.contains("2021-06-30"),
            "the window the analyst set must be named: {notice}"
        );
        assert!(
            notice.contains("keeps one entry"),
            "and the dedup rule stated, because it is the reason the window is not in the \
             identity: {notice}"
        );
    }

    /// A member whose id names a class with no profile route renders as text, not a dead link.
    ///
    /// PROVE THIS CAN FAIL: return `Some(profile_href(...))` unconditionally — the ward and the
    /// contract assertions fail.
    #[test]
    fn a_member_with_no_profile_route_is_not_rendered_as_a_link() {
        assert_eq!(
            pivot_href_by_id(COMPANY).as_deref(),
            Some("/entity/GB-COH%3AX06999001")
        );
        assert!(pivot_href_by_id(DIRECTOR).is_some(), "a director opens");
        assert_eq!(pivot_href_by_id(WARD), None);
        assert_eq!(pivot_href_by_id("CF:x9999-cf-00001"), None);
        assert_eq!(
            pin_no_profile_reason(WARD).as_deref(),
            Some("Ward — no profile page yet")
        );
        assert_eq!(pin_no_profile_reason(COMPANY), None);
    }

    /// Entities and edges are counted apart, and the singular reads as English.
    ///
    /// PROVE THIS CAN FAIL: return `format!("{} members", entity_count + edge_count)` — every
    /// assertion below fails, and the list starts claiming a subgraph save put 626 subjects in the
    /// file.
    #[test]
    fn the_list_counts_subjects_apart_from_relationships() {
        assert_eq!(member_count_clause(0, 0), "0 entities");
        assert_eq!(member_count_clause(1, 0), "1 entity");
        assert_eq!(member_count_clause(28, 0), "28 entities");
        assert_eq!(member_count_clause(28, 26), "28 entities, 26 edges");
        assert_eq!(member_count_clause(2, 1), "2 entities, 1 edge");
    }

    /// Only entity members reach the reopen page's member list, and their notes come with them.
    #[test]
    fn reopening_yields_every_entity_member_with_its_note() {
        let mut pins = vec![
            pin(COMPANY, "the applicant", PinOriginView::Profile),
            pin(DIRECTOR, "", PinOriginView::Canvas),
        ];
        pins.push(SavedPinView {
            id: "inv0001-edge".to_string(),
            target: PinTargetView::Edge {
                from: COMPANY.to_string(),
                to: DIRECTOR.to_string(),
                relation: "directs".to_string(),
            },
            note: "visible after filtering".to_string(),
            origin: PinOriginView::Subgraph,
            pinned_at: "2026-09-16T09:20:00Z".to_string(),
            updated_at: "2026-09-16T09:20:00Z".to_string(),
        });
        let detail = detail("Ward follow-up", pins);

        assert_eq!(detail.entity_count, 2);
        assert_eq!(detail.edge_count, 1, "non-vacuity: an edge pin is present");
        let entities = pinned_entities(&detail);
        assert_eq!(
            entities
                .iter()
                .filter_map(|pin| pin_entity_id(pin))
                .collect::<Vec<_>>(),
            vec![COMPANY, DIRECTOR],
            "in stored order, edge pins excluded"
        );
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].note, "the applicant");
        assert_eq!(origin_clause(entities[1].origin), "pinned from the canvas");
        assert_eq!(
            origin_clause(PinOriginView::Subgraph),
            "arrived with a saved subgraph"
        );
    }

    /// The destructive confirmation names the investigation, and the escape is not a bare word.
    #[test]
    fn the_delete_confirmation_names_what_it_would_delete() {
        assert_eq!(
            delete_confirm("Ward follow-up"),
            "Delete \"Ward follow-up\"? This cannot be undone."
        );
        assert_ne!(DELETE_CANCEL, "Cancel", "phase 45 UI checker finding 8");
        assert!(DELETE_CANCEL.contains("investigation"));
    }

    /// Every route this crate calls is built once, from the shared constants.
    #[test]
    fn a_route_names_its_investigation_in_the_query_string() {
        assert_eq!(
            with_id(INVESTIGATION_URL, "inv0001"),
            "/api/investigation?id=inv0001"
        );
        assert_eq!(
            with_id(PIN_URL, "inv0001"),
            "/api/investigation/pin?id=inv0001"
        );
        assert_eq!(investigation_href("inv0001"), "/investigations/inv0001");
    }
}
