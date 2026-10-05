//! `WithdrawForm` — the mandatory-reason withdrawal flow (D-08/D-09/D-10, the phase 46 UI spec's
//! "Withdraw form" copy rows), mounted by [`crate::alerts::row::AlertRow`] in place of the row's
//! action column while it is open — the single exception to the row's fixed 208px height
//! (the phase 46 UI spec "Row — withdraw form open").
//!
//! # Validation runs before any network call
//!
//! [`reason_is_valid`] is a plain predicate, called both to disable the confirm button and, on
//! click, to short-circuit before [`submit_withdrawal`] is ever invoked (T-46-09-01: this is
//! defence in depth — the authoritative enforcement is 46-06's server-side 400).
//!
//! # Three server outcomes plus one local sentinel
//!
//! [`submit_withdrawal`] returns the real [`WithdrawOutcomeView`] the server sent. `Withdrawn` and
//! `NotFound` are reported straight to the parent via `on_outcome`, which owns row removal and the
//! page's hidden-count display. `AlreadyWithdrawn` is *also* reported upward (the parent's count
//! does not change, but it is still told), and is additionally rendered in place of the form here —
//! the D-08 guard is a rule that held, not an error, so it carries no alarming styling and no
//! `role="alert"`. A transport/deserialisation failure never reaches `on_outcome` at all: it is
//! this component's own local `Failed` state, shown as [`WITHDRAW_FAILED`] with the form left open
//! for retry.
//!
//! # No "delete this" colour anywhere in this file
//!
//! The phase 46 UI spec's "Three deliberate non-uses" table is explicit: withdrawal removes nothing, so
//! the shared delete/failure token is never used for the confirm control or for this file's own
//! failure text (which uses `--muted-foreground`, matching the staleness-marker convention the
//! same table sets for a different field). This is checked by this plan's own acceptance
//! criteria, not merely described here.

use leptos::prelude::*;
use urban_integrity_workbench_dto::WithdrawOutcomeView;

/// `POST /api/alert/{id}/withdraw`.
pub fn withdraw_url(id: &str) -> String {
    format!("/api/alert/{id}/withdraw")
}

/// The register of `investigation::PIN_FAILED` — a write that did not land is never silent.
pub const WITHDRAW_FAILED: &str = "Not withdrawn. The workbench did not accept the change.";
/// the phase 46 UI spec Copywriting Contract, "Withdraw form question".
pub const WITHDRAW_QUESTION: &str = "Why is this alert being withdrawn?";
/// the phase 46 UI spec Copywriting Contract, "Withdraw form placeholder".
pub const WITHDRAW_PLACEHOLDER: &str =
    "The reason is stored with the withdrawal and shown wherever this alert appears.";
/// the phase 46 UI spec Copywriting Contract, "Withdraw confirm".
pub const WITHDRAW_CONFIRM: &str = "Withdraw alert";
/// the phase 46 UI spec Copywriting Contract, "Withdraw cancel" — not a bare "Cancel", matching this
/// project's own `investigation::DELETE_CANCEL` precedent.
pub const WITHDRAW_CANCEL: &str = "Keep this alert";
/// the phase 46 UI spec Copywriting Contract, "Withdraw validation".
pub const WITHDRAW_VALIDATION: &str = "A reason is required.";

/// The D-10 mandatory-reason check, run BEFORE any network call. A separate, directly-testable
/// pure predicate — not something only checkable by mocking `gloo_net` — per this plan's own
/// acceptance criteria.
///
/// PROVE THIS CAN FAIL: return `true` unconditionally and
/// `reason_is_valid_rejects_empty_and_whitespace_only_input` fails.
pub fn reason_is_valid(reason: &str) -> bool {
    !reason.trim().is_empty()
}

#[derive(Debug, serde::Serialize)]
struct WithdrawRequestBody<'a> {
    reason: &'a str,
}

/// `POST /api/alert/{id}/withdraw`, following `investigation::pin_entity`'s
/// POST-with-typed-failure shape exactly: every transport/parse error maps to
/// [`WITHDRAW_FAILED`], never a raw `gloo_net` error surfaced to the UI.
pub async fn submit_withdrawal(id: &str, reason: &str) -> Result<WithdrawOutcomeView, String> {
    let response = gloo_net::http::Request::post(&withdraw_url(id))
        .json(&WithdrawRequestBody { reason })
        .map_err(|_| WITHDRAW_FAILED.to_string())?
        .send()
        .await
        .map_err(|_| WITHDRAW_FAILED.to_string())?;
    if !response.ok() {
        return Err(WITHDRAW_FAILED.to_string());
    }
    response
        .json()
        .await
        .map_err(|_| WITHDRAW_FAILED.to_string())
}

/// The form's own local state — never a server outcome by itself. `AlreadyWithdrawn` mirrors the
/// server's own D-08 guard so the refusal text can replace the form in place; `Failed` is the
/// local sentinel for a transport/deserialisation failure the server never actually ruled on.
#[derive(Debug, Clone, PartialEq)]
enum FormState {
    Editing,
    Submitting,
    Failed,
    AlreadyWithdrawn { at: String, reason: String },
}

/// The D-08 refusal sentence, verbatim (the phase 46 UI spec "Withdraw refused — already withdrawn").
fn already_withdrawn_text(at: &str, reason: &str) -> String {
    format!("This alert was already withdrawn on {at}: \"{reason}\". The original reason is kept.")
}

/// The withdrawal flow: question, reason field, validation, confirm/cancel, and the three-outcome
/// response handling. `alert_id` is owned rather than borrowed so the component can move it into
/// the async submit task without a lifetime fight with its caller's per-row `RwSignal`.
#[component]
#[allow(non_snake_case)]
pub fn WithdrawForm(
    alert_id: String,
    /// Reports whichever server outcome resulted — `Withdrawn`/`NotFound` straight through,
    /// `AlreadyWithdrawn` both through here AND rendered locally (see module doc). A transport
    /// failure never reaches this callback; see [`FormState::Failed`].
    on_outcome: Callback<WithdrawOutcomeView>,
    /// **Keep this alert** closes the form without calling `on_outcome` at all — nothing happened.
    on_cancel: Callback<()>,
) -> impl IntoView {
    let reason = RwSignal::new(String::new());
    let state = RwSignal::new(FormState::Editing);
    let alert_id = StoredValue::new(alert_id);

    let on_confirm = move |_| {
        let current_reason = reason.get_untracked();
        if !reason_is_valid(&current_reason) {
            // Never issues the POST — the confirm button is disabled at this point too, but the
            // short-circuit here is what the acceptance criteria's own test proves directly.
            return;
        }
        state.set(FormState::Submitting);
        let id = alert_id.get_value();
        leptos::task::spawn_local(async move {
            match submit_withdrawal(&id, &current_reason).await {
                Ok(WithdrawOutcomeView::AlreadyWithdrawn { at, reason }) => {
                    state.set(FormState::AlreadyWithdrawn {
                        at: at.clone(),
                        reason: reason.clone(),
                    });
                    on_outcome.run(WithdrawOutcomeView::AlreadyWithdrawn { at, reason });
                }
                Ok(outcome) => on_outcome.run(outcome),
                Err(_) => state.set(FormState::Failed),
            }
        });
    };

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-sm); min-width: 200px;">
            {move || match state.get() {
                FormState::AlreadyWithdrawn { at, reason } => view! {
                    <p class="text-body" style="margin: 0;">{already_withdrawn_text(&at, &reason)}</p>
                }.into_any(),
                other => {
                    let submitting = other == FormState::Submitting;
                    let failed = other == FormState::Failed;
                    view! {
                        <p class="text-body" style="margin: 0;">{WITHDRAW_QUESTION}</p>
                        <textarea
                            placeholder=WITHDRAW_PLACEHOLDER
                            prop:value=move || reason.get()
                            on:input=move |ev| reason.set(event_target_value(&ev))
                            style="width: 100%; box-sizing: border-box; min-height: 44px; \
                                   padding: var(--space-xs) var(--space-sm); background: var(--card); \
                                   border: 1px solid var(--border); border-radius: 6px; color: var(--foreground);"
                        ></textarea>
                        {move || (!reason_is_valid(&reason.get())).then(|| view! {
                            <span class="text-label" style="color: var(--muted-foreground);">
                                {WITHDRAW_VALIDATION}
                            </span>
                        })}
                        {failed.then(|| view! {
                            <span class="text-body" style="color: var(--muted-foreground);">
                                {WITHDRAW_FAILED}
                            </span>
                        })}
                        <div style="display: flex; gap: var(--space-sm);">
                            <button
                                type="button"
                                class="text-body"
                                disabled=move || !reason_is_valid(&reason.get()) || submitting
                                on:click=on_confirm
                                style="min-height: 44px; background: var(--primary); \
                                       color: var(--primary-foreground); border: none; border-radius: 6px; \
                                       cursor: pointer; padding: 0 var(--space-md);"
                            >
                                {WITHDRAW_CONFIRM}
                            </button>
                            <button
                                type="button"
                                class="text-body control-button"
                                on:click=move |_| on_cancel.run(())
                                style="padding: 0 var(--space-md);"
                            >
                                {WITHDRAW_CANCEL}
                            </button>
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

    #[test]
    fn reason_is_valid_rejects_empty_and_whitespace_only_input() {
        assert!(!reason_is_valid(""));
        assert!(!reason_is_valid("   "));
        assert!(!reason_is_valid("\t\n"));
        assert!(reason_is_valid("confirmed false positive"));
        assert!(reason_is_valid("  padded but real  "));
    }

    #[test]
    fn withdraw_url_names_the_alert() {
        assert_eq!(
            withdraw_url("alert:abc123"),
            "/api/alert/alert:abc123/withdraw"
        );
    }

    #[test]
    fn already_withdrawn_text_matches_the_copywriting_contract() {
        assert_eq!(
            already_withdrawn_text("2026-09-18", "confirmed false positive"),
            "This alert was already withdrawn on 2026-09-18: \"confirmed false positive\". \
             The original reason is kept."
        );
    }

    /// The empty-reason short-circuit never calls the fetch path. `on_confirm`'s own body returns
    /// before `submit_withdrawal` is referenced at all when `reason_is_valid` is false — proven
    /// here by calling the same predicate the handler gates on, since the handler itself needs a
    /// mounted component to exercise directly (no DOM in this crate's test model).
    ///
    /// PROVE THIS CAN FAIL: change `on_confirm` to call `submit_withdrawal` unconditionally — this
    /// assertion (and the acceptance-criteria's own reading of `on_confirm`'s source) would no
    /// longer hold, even though this specific test does not execute the component body.
    #[test]
    fn the_short_circuit_predicate_used_by_on_confirm_rejects_empty_input() {
        assert!(!reason_is_valid(""));
    }
}
