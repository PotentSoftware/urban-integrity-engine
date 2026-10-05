//! `AlertDetailPage` — the `/alert/:id` route (D-03, the phase 46 UI spec "`/alert/:id` detail
//! route"): the motif at full size, the complete Participants list, the unclamped fact block, the
//! Phase 44 tool rail seeded from the alert's own participants, and the action row.
//!
//! Mirrors `/finding/:probe`'s **shell**, not its renderers (phase 46 research Q3,
//! the phase 46 UI spec's own words) — the motif itself is drawn by [`crate::motif_path::MotifPath`]
//! (46-07's general induced-subgraph renderer, already mounted at row scale by 46-08), not by
//! anything `finding_detail_page.rs` owns. The one piece this route DOES reuse from that file is
//! its generalised `GraphMotifDetail` (46-10 Task 1) for the tool rail beneath the motif — see
//! that component's own doc comment for why its signature no longer takes a `ProbeView`.
//!
//! # Every other action-row / fact-block piece is reused, never re-derived
//!
//! `WithdrawForm` and the pin control mount unchanged from `crate::alerts::{withdraw, row}` —
//! D-08/D-09's whole point is that this surface does not change withdrawal's semantics, so this
//! file supplies only page-specific glue (which entity is pinned, what happens after a successful
//! withdrawal), never a second write path. Likewise the fact block's `P_c`/band, IMD-state and
//! run-attribution rendering reuse `crate::alerts::row::{score_band_word, imd_state_view,
//! run_attribution_view, detector_display_name_view}` verbatim (widened to `pub(crate)` by this
//! plan) rather than re-deriving the same five IMD sentences or the same glossary-lookup table a
//! second time.
//!
//! # Not-found is an answer, not an error
//!
//! `GET /api/alert/{id}` 404s for an id no alert in this analysis has (`crates/workbench`'s
//! `detail_handler`, T-46-01) with the exact sentence [`alert_not_found_message`] also produces
//! client-side — the two agree by construction, the same pattern `profile/mod.rs::entity_not_found`
//! already established for `/entity/:id`. The breadcrumb still renders in this state (never a
//! panic on an untrusted route param).
//!
//! # The seed-id cap (T-46-02)
//!
//! [`alert_seed_refs`] caps the tool rail's population-fetch seed list at the SAME
//! `MAX_SEED_IDS_IN_URL` the finding detail route already enforces (widened to `pub(crate)` by
//! this plan, not redefined) — a display cap (the 20-node drawing budget) is not a URL-safety cap,
//! and the live-measured worst case is 173 participants on one alert (phase 46 research Q2).

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use urban_integrity_workbench_dto::investigation::PinOriginView;
use urban_integrity_workbench_dto::{caps, AlertDetailView, EntityRef, WithdrawOutcomeView};

use crate::abbr::Abbr;
use crate::alerts::row::{
    detector_display_name_view, imd_state_view, run_attribution_view, score_band_word,
};
use crate::alerts::withdraw::WithdrawForm;
use crate::alerts::RETRY;
use crate::breadcrumb::Breadcrumb;
use crate::copy_button::CopyButton;
use crate::entity_ref_view::EntityRefView;
use crate::finding_detail_page::{GraphMotifDetail, MAX_SEED_IDS_IN_URL};
use crate::investigation::pin::PinControl;
use crate::motif_path::MotifPath;
use crate::timestamp::Timestamp;

/// `GET /api/alert/:id`.
pub fn alert_detail_url(id: &str) -> String {
    format!("/api/alert/{id}")
}

/// The detail mount's motif size — the shipped `DETAIL_MOTIF_SIZE` value
/// (`finding_detail_page.rs:72`), declared again here rather than imported: this plan's own
/// interface note says to use whichever constant is already in scope at each call site rather
/// than importing one module's rendering-size literal into the other's (the one genuinely SHARED
/// constant between the two pages is the URL-safety cap, [`MAX_SEED_IDS_IN_URL`], which IS
/// imported above).
const DETAIL_MOTIF_SIZE: f64 = 800.0;

pub const ALERT_DETAIL_LOADING: &str = "Loading the alert\u{2026}";
pub const ALERT_DETAIL_FETCH_ERROR: &str =
    "The workbench returned data this page cannot read (/api/alert).";
pub const BACK_TO_ALERTS: &str = "\u{2190} Back to alerts";
pub const PARTICIPANTS_HEADING: &str = "Participants";
pub const WITHDRAW_TRIGGER: &str = "Withdraw\u{2026}";

/// The exact "no alert named" copy (the phase 46 UI spec Copywriting Contract, "Alert not found") — the
/// server's own 404 body carries the same sentence (`detail_handler`'s `not_found(format!(...))`),
/// so the page and the API agree by construction rather than by review, the same discipline
/// `profile/mod.rs::entity_not_found` already established for `/entity/:id`.
pub fn alert_not_found_message(id: &str) -> String {
    format!("No alert with id {id} exists in this analysis.")
}

/// "Showing {d} of {n} participants." (the phase 46 UI spec Copywriting Contract, "Detail participants
/// truncation") — rendered only when `drawn < total`.
pub fn participants_truncation_text(drawn: usize, total: usize) -> String {
    format!("Showing {drawn} of {total} participants.")
}

/// The tool rail's seed-id list (T-46-02): every participant, capped at
/// [`MAX_SEED_IDS_IN_URL`] — reused from `finding_detail_page`, never redefined — even though the
/// live-measured worst case is 173 participants on a single alert (phase 46 research Q2).
///
/// PROVE THIS CAN FAIL: drop the `.take(MAX_SEED_IDS_IN_URL)` and this returns all 173 refs
/// instead of 50 for the fixture below — the T-46-02 mitigation this test pins.
pub fn alert_seed_refs(participants: &[EntityRef]) -> Vec<EntityRef> {
    participants
        .iter()
        .take(MAX_SEED_IDS_IN_URL)
        .cloned()
        .collect()
}

/// What a fetch of one alert resolved to. `NotFound` is its own variant rather than an `Err`
/// carrying a 404 — the same discipline `profile::ProfileLoad` already established: a 404 is an
/// answer with its own copy and chrome (breadcrumb still renders), not a transport failure.
#[derive(Debug, Clone, PartialEq)]
enum AlertDetailLoad {
    NotFound,
    Failed(String),
    Loaded(Box<AlertDetailView>),
}

/// Fetches one alert's detail view. A 404 is [`AlertDetailLoad::NotFound`]; anything else
/// unreadable is [`AlertDetailLoad::Failed`] carrying [`ALERT_DETAIL_FETCH_ERROR`].
async fn fetch_alert_detail(id: &str) -> AlertDetailLoad {
    let response = match gloo_net::http::Request::get(&alert_detail_url(id))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return AlertDetailLoad::Failed(ALERT_DETAIL_FETCH_ERROR.to_string()),
    };
    if response.status() == 404 {
        return AlertDetailLoad::NotFound;
    }
    if !response.ok() {
        return AlertDetailLoad::Failed(ALERT_DETAIL_FETCH_ERROR.to_string());
    }
    match response.json::<AlertDetailView>().await {
        Ok(detail) => AlertDetailLoad::Loaded(Box::new(detail)),
        Err(_) => AlertDetailLoad::Failed(ALERT_DETAIL_FETCH_ERROR.to_string()),
    }
}

/// The `/alert/:id` detail route. Reads `:id` via `use_params_map()`, matching how
/// `/entity/:id`/`/finding/:probe` already read theirs.
#[component]
#[allow(non_snake_case)]
pub fn AlertDetailPage() -> impl IntoView {
    let params = use_params_map();
    let id = move || params.read().get("id").unwrap_or_default();

    let state: RwSignal<Option<AlertDetailLoad>> = RwSignal::new(None);
    let reload: RwSignal<u32> = RwSignal::new(0);

    Effect::new(move |_| {
        let _ = reload.get();
        let id_value = id();
        state.set(None);
        leptos::task::spawn_local(async move {
            let loaded = fetch_alert_detail(&id_value).await;
            state.set(Some(loaded));
        });
    });

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); padding-top: var(--space-md); max-width: 960px;">
            <div style="display: flex; align-items: center; gap: var(--space-md);">
                {move || {
                    let trailing = match state.get() {
                        Some(AlertDetailLoad::Loaded(detail)) => {
                            detail.summary.detector_display_name.clone()
                        }
                        _ => String::new(),
                    };
                    view! { <Breadcrumb trailing=trailing /> }
                }}
                <a
                    class="text-label"
                    href="/alerts"
                    style="color: var(--primary); min-height: 44px; display: inline-flex; align-items: center;"
                >
                    {BACK_TO_ALERTS}
                </a>
            </div>
            {move || match state.get() {
                None => view! {
                    <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                        {ALERT_DETAIL_LOADING}
                    </p>
                }.into_any(),
                Some(AlertDetailLoad::NotFound) => view! {
                    <p class="text-body" style="margin: 0; color: var(--destructive);">
                        {alert_not_found_message(&id())}
                    </p>
                }.into_any(),
                Some(AlertDetailLoad::Failed(_)) => view! {
                    <div style="display: flex; align-items: center; gap: var(--space-md);">
                        <p class="text-body" style="margin: 0; color: var(--destructive);">
                            {ALERT_DETAIL_FETCH_ERROR}
                        </p>
                        <button
                            type="button"
                            class="text-body control-button"
                            on:click=move |_| reload.update(|n| *n += 1)
                            style="padding: 0 var(--space-md);"
                        >
                            {RETRY}
                        </button>
                    </div>
                }.into_any(),
                Some(AlertDetailLoad::Loaded(detail)) => {
                    let detail = *detail;
                    view! { <AlertDetailBody detail=detail /> }.into_any()
                }
            }}
        </div>
    }
}

/// The loaded body — the motif, Participants, the fact block, the tool rail and the action row.
/// Split from [`AlertDetailPage`] only so the load-state match above stays a plain `match`, not
/// because this is reused elsewhere.
#[component]
#[allow(non_snake_case)]
fn AlertDetailBody(detail: AlertDetailView) -> impl IntoView {
    let summary = detail.summary.clone();
    let alert_id = summary.id.clone();
    let alert_id_for_withdraw = alert_id.clone();
    let seed_refs = alert_seed_refs(&detail.participants);
    let hub_entity_id = detail.participants.first().map(|e| e.id.clone());
    let withdraw_trigger_visible = summary.withdrawn.is_none();
    let withdraw_open: RwSignal<bool> = RwSignal::new(false);

    // The 800×800 box's "before an algorithm has run" content, for the generalised
    // `GraphMotifDetail` below — see that component's doc comment for why this is a caller-built
    // closure. 46-10 originally supplied `NamedEntityChips` here (a row of participant chips
    // floating in an otherwise-empty 800×800 card), reasoning that this page already draws the
    // alert's own true motif above via `MotifPath` so there was no evidence-row text to redraw a
    // second time. 46-UAT test 8 found that reasoning wrong in practice: the chip row reads as a
    // blank graph, violating the phase 50 UI spec ("the box shows the motif layout before Run
    // and after Reset — never a blank canvas"). This closure now renders the SAME induced-subgraph
    // motif a second time via `MotifPath`, at `DETAIL_MOTIF_SIZE`, labelled. The standalone box
    // above is kept rather than removed (plan 46-13's objective, planner's discretion): once
    // Run mounts the Cosmograph canvas in the box below, the standalone box becomes the only
    // on-screen reference drawing of the alert's own motif, so the two do not stay redundant, and
    // moving `GraphMotifDetail` up in its place would reorder 46-UI-SPEC's detail-route layout —
    // a larger change than this gap needs.
    let motif_preview = {
        let path_detail = detail.path_detail.clone();
        Callback::new(move |()| -> AnyView {
            view! { <MotifPath path=path_detail.clone() size=DETAIL_MOTIF_SIZE labelled=true /> }
                .into_any()
        })
    };

    // `Withdrawn` closes the form and re-fetches so the server's own withdrawn marker (date +
    // reason) lands in `summary.withdrawn` — this component has no other way to learn them, since
    // `WithdrawOutcomeView::Withdrawn` carries only `hidden_count`. `AlreadyWithdrawn`/`NotFound`
    // leave the form open: `WithdrawForm` already renders the D-08 refusal text in its place (see
    // its own module doc), and this page — unlike the list, which must remove or re-render a row —
    // has nothing else to update.
    let on_withdraw_outcome = Callback::new(move |outcome: WithdrawOutcomeView| {
        if let WithdrawOutcomeView::Withdrawn { .. } = outcome {
            withdraw_open.set(false);
        }
    });

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg);">
            <div style="display: flex; align-items: center; gap: var(--space-sm); flex-wrap: wrap;">
                <h1 class="text-heading" style="margin: 0;">
                    {detector_display_name_view(&summary.detector_display_name)}
                </h1>
                <span class="font-mono" style="color: var(--muted-foreground);">{alert_id.clone()}</span>
                <CopyButton value=alert_id.clone() />
            </div>

            <div style="width: min(800px, 100%); aspect-ratio: 1 / 1; background: var(--card-hover); border: 1px solid var(--border); border-radius: 6px; display: flex; align-items: center; justify-content: center; overflow: hidden;">
                <MotifPath path=detail.path_detail.clone() size=DETAIL_MOTIF_SIZE labelled=true />
            </div>

            <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
                <h2 class="text-heading" style="margin: 0;">{PARTICIPANTS_HEADING}</h2>
                <div style="display: flex; flex-wrap: wrap; gap: var(--space-sm);">
                    {detail.participants.iter().cloned().map(|entity| view! {
                        <EntityRefView entity=entity />
                    }).collect_view()}
                </div>
                {detail.participants_truncated.then(|| view! {
                    <p class="text-label" style="margin: 0; color: var(--muted-foreground);">
                        {participants_truncation_text(detail.participants.len(), summary.participant_total)}
                    </p>
                })}
            </div>

            <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
                <p class="text-body" style="margin: 0;">
                    <Abbr token="P_c".to_string() />" "
                    <span class="font-mono">{format!("{:.2}", summary.score)}</span>" · "
                    {score_band_word(&summary.score_band)}" · "
                    {imd_state_view(&summary.imd)}
                </p>
                <p class="text-body" style="margin: 0;">
                    {match summary.ward_code.clone() {
                        Some(code) => view! {
                            "Ward "<span class="font-mono">{code}</span>
                        }.into_any(),
                        None => view! {
                            <span style="color: var(--muted-foreground);">
                                "No ward code recorded for this alert."
                            </span>
                        }.into_any(),
                    }}
                </p>
                <p class="text-body" style="margin: 0;">
                    "Detected "<Timestamp at=summary.detected_at.clone() />
                </p>
                {run_attribution_view(&summary.run)}
                {summary.withdrawn.clone().map(|w| view! {
                    <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                        {format!("Withdrawn {} — {}", w.withdrawn_at, w.withdrawn_reason)}
                    </p>
                })}
            </div>

            <GraphMotifDetail seed_refs=seed_refs hops=caps::DEFAULT_HOPS_STAR motif_preview=motif_preview />

            <div style="display: flex; gap: var(--space-sm); align-items: center; flex-wrap: wrap;">
                {move || if withdraw_open.get() {
                    view! {
                        <WithdrawForm
                            alert_id=alert_id_for_withdraw.clone()
                            on_outcome=on_withdraw_outcome
                            on_cancel=Callback::new(move |()| withdraw_open.set(false))
                        />
                    }.into_any()
                } else {
                    let hub_entity_id = hub_entity_id.clone();
                    view! {
                        <>
                            {hub_entity_id.map(|entity_id| view! {
                                <PinControl entity_id=entity_id origin=PinOriginView::Alert as_at=None />
                            })}
                            {withdraw_trigger_visible.then(|| view! {
                                <button
                                    type="button"
                                    class="text-body control-button"
                                    on:click=move |_| withdraw_open.set(true)
                                    style="padding: 0 var(--space-md);"
                                >
                                    {WITHDRAW_TRIGGER}
                                </button>
                            })}
                        </>
                    }.into_any()
                }}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity_ref(id: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: id.to_string(),
            kind: "Entity".to_string(),
        }
    }

    #[test]
    fn alert_detail_url_names_the_alert() {
        assert_eq!(alert_detail_url("alert:abc123"), "/api/alert/alert:abc123");
    }

    #[test]
    fn alert_not_found_message_names_the_looked_up_id() {
        assert_eq!(
            alert_not_found_message("alert:does-not-exist"),
            "No alert with id alert:does-not-exist exists in this analysis."
        );
    }

    #[test]
    fn participants_truncation_text_names_both_counts() {
        assert_eq!(
            participants_truncation_text(50, 173),
            "Showing 50 of 173 participants."
        );
    }

    /// PROVE THIS CAN FAIL: drop the `.take(MAX_SEED_IDS_IN_URL)` in `alert_seed_refs` and this
    /// returns 173 instead of 50 — the T-46-02 mitigation, checked against the live-measured worst
    /// case (phase 46 research Q2), not a small fixture that would pass either way.
    #[test]
    fn alert_seed_refs_caps_at_the_url_safety_limit_against_the_live_measured_worst_case() {
        let participants: Vec<EntityRef> = (0..173)
            .map(|i| entity_ref(&format!("GB-COH:{i:04}")))
            .collect();
        let refs = alert_seed_refs(&participants);
        assert_eq!(refs.len(), MAX_SEED_IDS_IN_URL);
        // The cap keeps a PREFIX (the same policy `finding_detail_page::seed_ids_csv` follows),
        // not an arbitrary subset.
        assert_eq!(refs[0].id, "GB-COH:0000");
    }

    #[test]
    fn alert_seed_refs_is_unchanged_when_under_the_cap() {
        let participants: Vec<EntityRef> = (0..3).map(|i| entity_ref(&format!("id-{i}"))).collect();
        assert_eq!(alert_seed_refs(&participants).len(), 3);
    }
}
