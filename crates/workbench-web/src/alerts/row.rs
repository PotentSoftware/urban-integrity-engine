//! `AlertRow` — the fixed-208px alert card (the phase 46 UI spec "Alert row card"), the first of the two
//! per-row building blocks 46-08 shipped. Mounted by 46-09's `/alerts` list page.
//!
//! Composes entirely shipped chrome rather than inventing a second copy of anything: the motif
//! drawing is `crate::motif_path::MotifPath` (46-07), every participant pivot goes through
//! `crate::profile::pivot::pivot_href` (never a hand-built `/entity/{id}` string — Phase 45's own
//! rule, T-46-08-01), and "Pin to investigation" is the shared `crate::investigation::pin::PinControl`
//! under the new `PinOriginView::Alert` 46-08 added.
//!
//! # The withdraw form is the one exception to the fixed row height (46-09)
//!
//! `withdraw_open` is owned by the CALLER (one fresh `RwSignal<bool>` per row, e.g. from a keyed
//! `<For>` child scope) rather than local state here, because 46-09's `AlertsPage` also needs to
//! know when a withdrawal actually happened (to remove or re-render the row and update the page's
//! hidden-count display) — a signal AlertRow owned privately could not be read from outside it.
//! When `withdraw_open` is `true`, column 3 (actions) is replaced by
//! `crate::alerts::withdraw::WithdrawForm`, and the row's own container style switches from a
//! fixed `height: 208px` to `min-height: 208px` so the mandatory-reason field is never clamped
//! (the phase 46 UI spec "Row — withdraw form open").
//!
//! # The five `ImdStateView` sentences are a pure function, deliberately
//!
//! [`imd_state_copy`] takes no `RwSignal`, no component context — just the DTO variant — so the
//! exact Copywriting Contract sentence for each of the five states is asserted by a host-side
//! `#[test]` rather than trusted to a visual read. [`imd_state_view`] renders the same content for
//! real, with "IMD" (in the two states that name it) as a genuine `<Abbr>` element built from the
//! shared `IMD_ABBR` constant — a pure `String` has no markup to carry the glossary tooltip, so
//! the component and the tested dispatch are deliberately two functions sharing one source of
//! truth for the word itself, not one function serving both jobs.
//!
//! # Detector display names through the one glossary lookup, not per-literal wrapping
//!
//! The phase 46 UI spec's Glossary section requires **Phoenix** / **AddressCluster** / **NepotismBridge**
//! to render through `<Abbr>`. `MotifType::display_name()` (`crates/detector`) does not spell two
//! of those the same way the glossary token does — `"Address Cluster"` and `"Nepotism Bridge"`
//! carry a space the glossary token (`"AddressCluster"`, `"NepotismBridge"`) does not — so
//! [`detector_display_name_view`] maps the server-sent display name to its glossary token
//! explicitly, rather than passing the display name straight into `<Abbr>` and letting two of the
//! three silently fail their lookup (they contain a space, which `glossary::parse_row`'s
//! alphanumeric-only `abbr` shape rejects) and degrade to unlinked plain text with nobody the
//! wiser. The five detector names with no glossary entry render as plain text, honestly.
//!
//! # `P_c` does not currently resolve through the glossary — recorded, not silently patched
//!
//! Phase 46 assumption A-04 asked this task to confirm `glossary::lookup("P_c")` and, if it
//! returned `None`, add a Notes column to the `P_c` row in `docs/abbreviations.md` so it becomes
//! 3-column. It returns `None` — but not for the reason A-04 assumed. `glossary::parse_row`
//! rejects `abbr` unless every character is `is_ascii_alphanumeric()`, and `_` in `P_c` fails that
//! check regardless of the row's column count; a 3-column edit changes nothing.
//! Per `abbr.rs`'s own contract ("never fabricate an expansion"), the fact line still routes the
//! literal `"P_c"` through `<Abbr>` — it degrades to plain text automatically (the same fallback
//! every other unresolved token uses) rather than skipping the lookup by hand, and will start
//! resolving on its own if a later change relaxes the shape rule. That is a parser-wide change
//! affecting every glossary row, not a one-row copy fix, so it is left as a follow-up rather than
//! silently widened inside this plan. See the addendum this task added to A-04.

use leptos::prelude::*;
use urban_integrity_workbench_dto::{
    caps, AlertSummaryView, ImdStateView, InducedSubgraphView, RunAttributionView, ScoreBandView,
    WithdrawOutcomeView,
};

use crate::abbr::Abbr;
use crate::alerts::withdraw::WithdrawForm;
use crate::investigation::pin::PinControl;
use crate::motif_path::MotifPath;
use crate::profile::pivot::pivot_href;
use crate::timestamp::Timestamp;
use urban_integrity_workbench_dto::investigation::PinOriginView;

/// The three `MotifType::display_name()` strings that have a glossary entry, paired with the
/// EXACT token `glossary::lookup` expects for each — which differs in spelling from the display
/// name for two of the three (see this module's doc comment). Checked, not assumed: confirmed by
/// running `cargo test -p urban-integrity-workbench-web --bin urban-integrity-workbench-web
/// glossary::` against a temporary probe before this table was written.
const GLOSSARY_LINKED_DETECTOR_NAMES: &[(&str, &str)] = &[
    ("Phoenix", "Phoenix"),
    ("Address Cluster", "AddressCluster"),
    ("Nepotism Bridge", "NepotismBridge"),
];

/// The detector display name, routed through the one glossary lookup this screen's Copywriting
/// Contract requires — `<Abbr>`-wrapped for the 3 tokenised names, plain text for the other 5.
/// PROVE THIS CAN FAIL: pass `display_name` straight into `<Abbr>` without the token table above
/// and "Address Cluster"/"Nepotism Bridge" silently stop resolving (they contain a space).
///
/// `pub(crate)` (was private): `crate::alert_detail_page` (46-10) reuses this SAME lookup for the
/// detail route's `<h1>`, per this module's own "one glossary lookup, not per-literal wrapping"
/// contract — a second copy on the detail route would be exactly the drift that contract exists
/// to prevent.
pub(crate) fn detector_display_name_view(display_name: &str) -> impl IntoView {
    let token = GLOSSARY_LINKED_DETECTOR_NAMES
        .iter()
        .find(|(name, _)| *name == display_name)
        .map(|(_, token)| *token);
    match token {
        Some(token) => view! { <Abbr token=token.to_string() /> }.into_any(),
        None => view! { <span>{display_name.to_string()}</span> }.into_any(),
    }
}

/// `{band}` word for the `P_c` fact — `ScoreBandView` is already computed server-side, so this is
/// a spelling table, not a re-derivation of `classify_risk`'s thresholds.
///
/// `pub(crate)` (was private): reused by `crate::alert_detail_page` (46-10) for the detail route's
/// unclamped fact block, rather than re-deriving the same spelling a second time.
pub(crate) fn score_band_word(band: &ScoreBandView) -> &'static str {
    match band {
        ScoreBandView::Critical => "Critical",
        ScoreBandView::High => "High",
        ScoreBandView::Medium => "Medium",
        ScoreBandView::BelowMedium => "Below medium",
    }
}

/// The glossary token the fact line's "IMD" word wraps in `<Abbr>` (the phase 46 UI spec's Glossary
/// section names IMD as required `<Abbr>` markup on this screen, D-03/NAV-04) — a bare
/// `pub const`, matching `tool_rail.rs`'s `OSINT_ABBR`/`PSC_ABBR` convention exactly, so this
/// declaration line is itself exempt from `abbr_reachable.rs`'s bare-literal sweep (`_ABBR: &str`
/// is the shape that sweep already recognises as "the `<Abbr>` input itself, not copy"). Both
/// [`imd_state_copy`] and [`imd_state_view`] below build their "IMD" text from this ONE constant
/// rather than spelling the word inline a second time, so a rename cannot drift the two apart.
const IMD_ABBR: &str = "IMD";

/// The ONE applicable IMD-state sentence, dispatched on `ImdStateView`'s variant
/// (the phase 46 UI spec's Copywriting Contract, five exact strings). A pure function, host-tested
/// independently of the component — see the module doc. [`imd_state_view`] renders the same
/// content with "IMD" as a real `<Abbr>` element rather than as this function's plain text.
///
/// PROVE THIS CAN FAIL: collapse `WardNotCovered` and `NoIndexLoaded` into one sentence and the
/// `imd_state_copy_ward_not_covered_names_the_index_file` test fails, since only `WardNotCovered`
/// carries `imd_csv_path` in its copy.
// Deliberately unreferenced outside `#[cfg(test)]`: this function exists to be host-tested
// against the exact Copywriting Contract sentences, never to be rendered directly (that is
// `imd_state_view`'s job — see the module doc's "pure function, deliberately" section).
#[allow(dead_code)]
fn imd_state_copy(imd: &ImdStateView) -> String {
    match imd {
        ImdStateView::Scored { imd_rank, m_env } => {
            format!("{IMD_ABBR} rank {imd_rank} · m_env {m_env:.2}")
        }
        ImdStateView::WardNotCovered {
            imd_csv_path,
            m_env,
        } => format!(
            "No {IMD_ABBR} rank for this ward — the index loaded for this run ({imd_csv_path}) \
             does not cover it. m_env {m_env:.2}."
        ),
        ImdStateView::NoIndexLoaded { m_env } => {
            format!("No deprivation index was loaded for this run. m_env {m_env:.2}.")
        }
        ImdStateView::NotApplicable => {
            "This detector does not compute a deprivation weighting.".to_string()
        }
        ImdStateView::NotRecorded => {
            "No deprivation index state is recorded for this alert.".to_string()
        }
    }
}

/// Renders [`imd_state_copy`]'s exact sentence, with "IMD" (where the sentence names it) as a
/// real `<Abbr>` element rather than plain text — the Glossary-tokens contract `imd_state_copy`
/// alone cannot satisfy, since a pure `String` has no markup to carry the tooltip.
///
/// `pub(crate)` (was private): reused by `crate::alert_detail_page` (46-10) for the detail route's
/// unclamped fact block — the plan's own text names `imd_state_copy` for this reuse, but the
/// Glossary contract applies to the whole screen (both `/alerts` and `/alert/:id`), so the detail
/// route renders through this `<Abbr>`-wrapped pair rather than the plain-`String` one, which
/// exists to be host-tested, not rendered directly (see this module's own doc comment).
pub(crate) fn imd_state_view(imd: &ImdStateView) -> impl IntoView {
    match imd {
        ImdStateView::Scored { imd_rank, m_env } => view! {
            <Abbr token=IMD_ABBR.to_string() />{format!(" rank {imd_rank} · m_env {m_env:.2}")}
        }
        .into_any(),
        ImdStateView::WardNotCovered {
            imd_csv_path,
            m_env,
        } => view! {
            "No "<Abbr token=IMD_ABBR.to_string() />{format!(
                " rank for this ward — the index loaded for this run ({imd_csv_path}) does not \
                 cover it. m_env {m_env:.2}."
            )}
        }
        .into_any(),
        ImdStateView::NoIndexLoaded { m_env } => {
            format!("No deprivation index was loaded for this run. m_env {m_env:.2}.").into_any()
        }
        ImdStateView::NotApplicable => {
            "This detector does not compute a deprivation weighting.".into_any()
        }
        ImdStateView::NotRecorded => {
            "No deprivation index state is recorded for this alert.".into_any()
        }
    }
}

/// The claim line's sentence, EXCLUDING the trailing `<Timestamp>` the `NoEdges` state also
/// carries (the caller splices that in separately — see the phase 46 UI spec's exact three-state copy).
fn claim_line_text(path: &InducedSubgraphView) -> String {
    match path {
        InducedSubgraphView::Drawable {
            edges,
            participant_total,
            drawn_total,
            ..
        } => {
            let n = *participant_total;
            let d = *drawn_total;
            let m = edges.len();
            if n > d {
                format!("{n} participants ({d} drawn) · {m} connecting edges")
            } else {
                format!("{n} participants · {m} connecting edges")
            }
        }
        InducedSubgraphView::NoEdges {
            participant_total, ..
        } => format!(
            "No connecting edges between these {participant_total} participants in the current \
             population."
        ),
        InducedSubgraphView::SingleParticipant { .. } => {
            "This detector names one entity — there is no path to draw.".to_string()
        }
    }
}

/// The run-attribution line (D-05/D-06): "First seen…/Last seen…" with both run ids in
/// `.font-mono`, a staleness clause when applicable, or "Run not recorded." when neither run id
/// is known.
///
/// `pub(crate)` (was private): reused verbatim by `crate::alert_detail_page` (46-10) for the
/// detail route's "run attribution in full" fact — the same sentence, not a second copy of it.
pub(crate) fn run_attribution_view(run: &RunAttributionView) -> impl IntoView {
    if run.first_seen_run_id.is_none() && run.last_seen_run_id.is_none() {
        return view! {
            <span class="text-label" style="color: var(--muted-foreground);">
                "Run not recorded."
            </span>
        }
        .into_any();
    }
    let first_id = run.first_seen_run_id.clone().unwrap_or_default();
    let last_id = run.last_seen_run_id.clone().unwrap_or_default();
    let first_at = run.first_seen_at.clone();
    let last_at = run.last_seen_at.clone();
    let stale = run.runs_since_last_seen.filter(|&k| k > 0);
    view! {
        <span class="text-label" style="color: var(--muted-foreground);">
            "First seen in run "<span class="font-mono">{first_id}</span>" "
            {first_at.map(|at| view! { <Timestamp at=at /> })}
            " · Last seen in run "<span class="font-mono">{last_id}</span>" "
            {last_at.map(|at| view! { <Timestamp at=at /> })}
            {stale.map(|k| format!(" Last confirmed {k} runs ago."))}
        </span>
    }
    .into_any()
}

/// The row's outer container style — `height: 208px` fixed, unless the withdraw form is open, in
/// which case the row grows to fit it (the phase 46 UI spec "Row — withdraw form open", the single
/// exception to the fixed row height).
fn row_container_style(withdraw_expanded: bool) -> String {
    let height_rule = if withdraw_expanded {
        "min-height: 208px;"
    } else {
        "height: 208px;"
    };
    format!(
        "display: grid; grid-template-columns: 160px 1fr 200px; gap: var(--space-lg); \
         {height_rule} background: var(--card); border: 1px solid var(--border); \
         border-radius: 6px; padding: var(--space-lg); box-sizing: border-box;"
    )
}

/// The 208px alert row card (the phase 46 UI spec "Alert row card"). Every entity in the motif is a
/// pivot (TRI-02) — participant chips go through `pivot_href` directly, never a second href
/// builder. `PinControl` mounts with `PinOriginView::Alert` on the alert's hub participant (the
/// first `participant_chips` entry, per plan 46-08's `<interfaces>` note); it is omitted
/// entirely when there is no participant to pin (D-02/single-participant with an empty chip list).
#[component]
#[allow(non_snake_case)]
pub fn AlertRow(
    alert: AlertSummaryView,
    /// Owned by the caller (46-09) — one fresh signal per row, so the parent can also react to a
    /// completed withdrawal. See this module's doc comment.
    withdraw_open: RwSignal<bool>,
    /// Reports the server's withdrawal outcome upward, paired with the id of the alert it applies
    /// to; the parent owns row removal/re-render and the page's hidden-count display (46-09's
    /// `AlertsPage`). The id is carried because [`WithdrawOutcomeView`] itself names no alert —
    /// without it the parent cannot tell WHICH row the outcome belongs to, and any removal it
    /// attempts degenerates into a no-op over a list whose rows all still read `withdrawn: None`.
    on_withdraw_outcome: Callback<(String, WithdrawOutcomeView)>,
) -> impl IntoView {
    let withdrawn_marker = alert.withdrawn.clone().map(|w| {
        view! {
            <p class="text-label" style="margin: 0; color: var(--muted-foreground);">
                {format!("Withdrawn {} — {}", w.withdrawn_at, w.withdrawn_reason)}
            </p>
        }
    });

    let claim_text = claim_line_text(&alert.path);
    let claim_timestamp = match &alert.path {
        InducedSubgraphView::NoEdges {
            snapshot_generated_at,
            ..
        } => Some(snapshot_generated_at.clone()),
        _ => None,
    };

    let fact_line = format!("{:.2}", alert.score);
    let band_word = score_band_word(&alert.score_band);
    let imd_view = imd_state_view(&alert.imd);

    let hub_entity_id = alert.participant_chips.first().map(|e| e.id.clone());
    let participant_total = alert.participant_total;
    let chip_count = alert.participant_chips.len();
    let more_count = participant_total.saturating_sub(chip_count);
    debug_assert!(
        chip_count <= caps::ALERT_ROW_PARTICIPANT_CHIPS,
        "the server must already cap participant_chips to ALERT_ROW_PARTICIPANT_CHIPS"
    );

    let open_href = format!("/alert/{}", alert.id);
    let alert_id_for_withdraw = alert.id.clone();
    let withdraw_trigger_visible = alert.withdrawn.is_none();

    view! {
        <div style=move || row_container_style(withdraw_open.get())>
            // Column 1 — thumbnail slot.
            <div style="width: 160px; height: 160px;">
                <MotifPath path=alert.path.clone() size=160.0 labelled=false />
            </div>

            // Column 2 — content, vertical stack, sm gaps.
            <div style="display: flex; flex-direction: column; gap: var(--space-sm); min-width: 0; overflow: hidden;">
                {withdrawn_marker}
                <span class="text-label">{detector_display_name_view(&alert.detector_display_name)}</span>
                <p
                    class="text-body"
                    title=claim_text.clone()
                    style="margin: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;"
                >
                    {claim_text.clone()}
                    {claim_timestamp.map(|at| view! { " "<Timestamp at=at /> })}
                </p>
                <p class="text-body" style="margin: 0;">
                    <Abbr token="P_c".to_string() />" "
                    <span class="font-mono">{fact_line}</span>" · "{band_word}" · "{imd_view}
                </p>
                {run_attribution_view(&alert.run)}
                <div style="display: flex; flex-wrap: nowrap; gap: var(--space-sm); overflow: hidden;">
                    {alert.participant_chips.iter().cloned().map(|entity| {
                        match pivot_href(Some(&entity), None) {
                            Some(href) => view! {
                                <a href=href
                                    class="text-label"
                                    style="min-height: 44px; display: inline-flex; align-items: center; \
                                           color: var(--primary); text-decoration: underline; \
                                           text-underline-offset: var(--space-xs);"
                                >
                                    {entity.name}
                                </a>
                            }.into_any(),
                            None => view! {
                                <span class="text-label" style="min-height: 44px; display: inline-flex; align-items: center;">
                                    {entity.name}
                                </span>
                            }.into_any(),
                        }
                    }).collect_view()}
                    {(more_count > 0).then(|| view! {
                        <span class="text-label" style="min-height: 44px; display: inline-flex; align-items: center; color: var(--muted-foreground);">
                            {format!("+{more_count} more")}
                        </span>
                    })}
                </div>
            </div>

            // Column 3 — actions, vertical stack, sm gaps, right-aligned. Replaced entirely by
            // WithdrawForm while `withdraw_open` is true (the row's one exception to a fixed
            // 208px height — see `row_container_style`).
            <div style="display: flex; flex-direction: column; gap: var(--space-sm); align-items: flex-end;">
                {move || if withdraw_open.get() {
                    let id = alert_id_for_withdraw.clone();
                    let id_for_outcome = id.clone();
                    view! {
                        <WithdrawForm
                            alert_id=id
                            on_outcome=Callback::new(move |outcome: WithdrawOutcomeView| {
                                withdraw_open.set(false);
                                on_withdraw_outcome.run((id_for_outcome.clone(), outcome));
                            })
                            on_cancel=Callback::new(move |()| withdraw_open.set(false))
                        />
                    }.into_any()
                } else {
                    let open_href = open_href.clone();
                    let hub_entity_id = hub_entity_id.clone();
                    view! {
                        <a href=open_href
                            class="text-body"
                            style="min-height: 44px; display: inline-flex; align-items: center; color: var(--primary);"
                        >
                            "Open alert →"
                        </a>
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
                                "Withdraw…"
                            </button>
                        })}
                    }.into_any()
                }}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector_display_name_view_maps_the_three_tokenised_names_to_the_glossary_spelling() {
        assert_eq!(
            GLOSSARY_LINKED_DETECTOR_NAMES
                .iter()
                .find(|(n, _)| *n == "Phoenix")
                .map(|(_, t)| *t),
            Some("Phoenix")
        );
        assert_eq!(
            GLOSSARY_LINKED_DETECTOR_NAMES
                .iter()
                .find(|(n, _)| *n == "Address Cluster")
                .map(|(_, t)| *t),
            Some("AddressCluster")
        );
        assert_eq!(
            GLOSSARY_LINKED_DETECTOR_NAMES
                .iter()
                .find(|(n, _)| *n == "Nepotism Bridge")
                .map(|(_, t)| *t),
            Some("NepotismBridge")
        );
        // A detector with no glossary entry maps to nothing.
        assert_eq!(
            GLOSSARY_LINKED_DETECTOR_NAMES
                .iter()
                .find(|(n, _)| *n == "Property Accumulation")
                .map(|(_, t)| *t),
            None
        );
    }

    #[test]
    fn score_band_word_covers_all_four_bands() {
        assert_eq!(score_band_word(&ScoreBandView::Critical), "Critical");
        assert_eq!(score_band_word(&ScoreBandView::High), "High");
        assert_eq!(score_band_word(&ScoreBandView::Medium), "Medium");
        assert_eq!(score_band_word(&ScoreBandView::BelowMedium), "Below medium");
    }

    #[test]
    fn imd_state_copy_scored_names_the_rank_and_m_env() {
        assert_eq!(
            imd_state_copy(&ImdStateView::Scored {
                imd_rank: 42,
                m_env: 0.73
            }),
            "IMD rank 42 · m_env 0.73"
        );
    }

    #[test]
    fn imd_state_copy_ward_not_covered_names_the_index_file() {
        assert_eq!(
            imd_state_copy(&ImdStateView::WardNotCovered {
                imd_csv_path: "data/wimd_2019_wards.csv".to_string(),
                m_env: 1.0
            }),
            "No IMD rank for this ward — the index loaded for this run \
             (data/wimd_2019_wards.csv) does not cover it. m_env 1.00."
        );
    }

    #[test]
    fn imd_state_copy_no_index_loaded_names_the_run() {
        assert_eq!(
            imd_state_copy(&ImdStateView::NoIndexLoaded { m_env: 1.0 }),
            "No deprivation index was loaded for this run. m_env 1.00."
        );
    }

    #[test]
    fn imd_state_copy_not_applicable_names_the_detector_class() {
        assert_eq!(
            imd_state_copy(&ImdStateView::NotApplicable),
            "This detector does not compute a deprivation weighting."
        );
    }

    #[test]
    fn imd_state_copy_not_recorded_names_the_alert() {
        assert_eq!(
            imd_state_copy(&ImdStateView::NotRecorded),
            "No deprivation index state is recorded for this alert."
        );
    }

    #[test]
    fn claim_line_text_drawable_states_the_drawn_clause_only_when_truncated() {
        let whole = InducedSubgraphView::Drawable {
            nodes: vec![],
            edges: vec![],
            participant_total: 3,
            drawn_total: 3,
        };
        assert_eq!(
            claim_line_text(&whole),
            "3 participants · 0 connecting edges"
        );

        let truncated = InducedSubgraphView::Drawable {
            nodes: vec![],
            edges: vec![],
            participant_total: 12,
            drawn_total: 8,
        };
        assert_eq!(
            claim_line_text(&truncated),
            "12 participants (8 drawn) · 0 connecting edges"
        );
    }

    #[test]
    fn claim_line_text_no_edges_and_single_participant_match_the_contract() {
        assert_eq!(
            claim_line_text(&InducedSubgraphView::NoEdges {
                participant_total: 5,
                snapshot_generated_at: "2026-09-21T00:00:00Z".to_string(),
            }),
            "No connecting edges between these 5 participants in the current population."
        );
        assert_eq!(
            claim_line_text(&InducedSubgraphView::SingleParticipant {
                entity: urban_integrity_workbench_dto::EntityRef {
                    id: "GB-COH:1".to_string(),
                    name: "Example Ltd".to_string(),
                    kind: "Entity".to_string(),
                }
            }),
            "This detector names one entity — there is no path to draw."
        );
    }
}
