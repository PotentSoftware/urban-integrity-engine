//! `AnalysisPage` — the Analysis page's interactive BLUF report (D-06, D-15, D-17, D-18, D-19,
//! TRI-03/04/05): the BLUF box, the Run button's full state machine, six always-rendered
//! verdict-group rows, a two-level accordion (group -> probe, single-probe-open enforced), and
//! URL-hash-driven deep linking to one expanded probe.
//!
//! # Testing approach (plain-function-first, per `verdict_badge.rs`'s documented rationale)
//!
//! Every piece of copy/formatting/routing logic this page needs — the four BLUF sentences, the
//! contradiction summary, the group badge styling, the hash-to-probe resolver, and the
//! status-code-to-error-string mapping for a failed Run — is factored into plain functions that
//! take owned/borrowed DTO values and return `String`/`Option<...>`, fully host-testable without
//! a DOM. `trigger_run`/`fetch_report` themselves call `gloo-net`'s Fetch-API transport, which
//! panics outside an actual browser runtime (same rationale as `status_page.rs`/
//! `analysis_picker.rs`), so those two async functions are not unit-tested directly — only the
//! pure `run_error_for_status` helper they delegate their status-code mapping to.

use std::collections::HashSet;

use leptos::prelude::*;
use leptos_router::hooks::use_location;
use urban_integrity_workbench_dto::{
    BlufView, ContradictionCheckView, ProbeGroupView, ProbeView, ReportView,
};

use crate::breadcrumb::Breadcrumb;
use crate::entity_ref_view::EntityRefView;
use crate::finding_board::FindingBoard;
use crate::timestamp::Timestamp;
use crate::verdict_badge::VerdictBadge;

/// `GET /api/analysis/report` — the native endpoint Plan 05 built (`report_handler`).
pub const REPORT_URL: &str = "/api/analysis/report";
/// `POST /api/analysis/run` — the native endpoint Plan 05 built (`run_handler`).
pub const RUN_URL: &str = "/api/analysis/run";
/// `GET /api/analysis/report.md` — the Markdown dossier export; a real navigation target, never
/// fetched from Rust.
pub const REPORT_MD_URL: &str = "/api/analysis/report.md";

/// D-23's exact "unreadable endpoint shape" copy, with `/api/analysis/report` substituted for
/// `{endpoint path}` — shown on any non-404 fetch failure against [`REPORT_URL`] (transport error,
/// non-2xx response, or a payload this page cannot deserialize).
pub const ANALYSIS_FETCH_ERROR_COPY: &str =
    "The workbench returned data this page cannot read (/api/analysis/report).";

/// Shown in the headline box while the first `/api/analysis/report` fetch is still in flight,
/// so the D-19 "No probe run yet" empty state is never fabricated for a report that simply has
/// not arrived yet.
pub const ANALYSIS_LOADING_COPY: &str = "Loading the last probe run\u{2026}";

/// What the headline box renders before any report is in hand — resolved by
/// [`headline_state`] from the current report signal and the initial fetch's resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadlineState {
    /// The initial fetch has not resolved yet.
    Loading,
    /// The initial fetch failed (any non-404 outcome); carries the copy to show.
    Failed(String),
    /// The initial fetch resolved to "no report persisted" and no Run has produced one since.
    Empty,
    /// A report is in hand (persisted or from a Run) — the BLUF lines render instead.
    Report,
}

/// Resolves the headline box's state. A report in hand always wins (a successful Run replaces
/// the initial fetch's outcome wholesale); otherwise the initial fetch's resolution decides:
/// unresolved is `Loading`, `Err` is `Failed`, and only `Ok(None)` is the genuine `Empty` state.
pub fn headline_state(
    report: Option<&ReportView>,
    initial_fetch: Option<&Result<Option<ReportView>, String>>,
) -> HeadlineState {
    if report.is_some() {
        return HeadlineState::Report;
    }
    match initial_fetch {
        None => HeadlineState::Loading,
        Some(Err(msg)) => HeadlineState::Failed(msg.clone()),
        Some(Ok(_)) => HeadlineState::Empty,
    }
}

/// The six fixed group names, in report order, matching `crates/probe::report::DETAIL_GROUPS`
/// plus the trailing `Skipped` group `crates/workbench::report_view::from_battery` appends —
/// used only to build the D-19 empty-state's zero-count placeholder rows, never to relabel a
/// real, server-sent `ProbeGroupView.name` (which is always rendered verbatim).
const FIXED_GROUP_NAMES: [&str; 6] = [
    "Confirmed Findings",
    "Suggestive Patterns",
    "Data-Integrity Gaps",
    "Not Applicable",
    "Verified Negative Findings",
    "Skipped",
];

/// The Run button's local UI state — never a DTO, since nothing on `ReportView` needs to know
/// whether a run is currently in flight from this browser tab.
#[derive(Debug, Clone, PartialEq)]
pub enum RunState {
    Idle,
    Running,
    Failed(String),
}

/// Reimplements the trivial singular/plural choice `urban_integrity_probe::plural` makes,
/// verbatim, since this crate cannot depend on `urban_integrity_probe` (D-01's wasm-boundary
/// rule).
fn plural_word<'a>(n: u64, singular: &'a str, plural: &'a str) -> &'a str {
    if n == 1 {
        singular
    } else {
        plural
    }
}

/// BLUF line 1: probes that could not evaluate (data gap or blocked). Zero is its own sentence
/// ("All N probes could evaluate."), never "0 of N probes could not evaluate" — that phrasing
/// reads as a near-miss rather than a clean result (the phase 43 UI spec Copywriting Contract).
pub fn bluf_line_1(bluf: &BlufView) -> String {
    if bluf.could_not_evaluate == 0 {
        format!("All {} probes could evaluate.", bluf.total_probes)
    } else {
        format!(
            "{} of {} {} could not evaluate (data gap or blocked).",
            bluf.could_not_evaluate,
            bluf.total_probes,
            plural_word(bluf.could_not_evaluate, "probe", "probes")
        )
    }
}

/// BLUF line 2: probes skipped by gating. Always rendered, including the zero case (TRI-04).
pub fn bluf_line_2(bluf: &BlufView) -> String {
    format!(
        "{} {} skipped by gating.",
        bluf.skipped,
        plural_word(bluf.skipped, "probe", "probes")
    )
}

/// BLUF line 3: records evaluated and how many were structurally capable of a finding — the two
/// counts travel exactly as the server sent them, never re-derived client-side.
pub fn bluf_line_3(bluf: &BlufView) -> String {
    format!(
        "Of {} records evaluated, {} were structurally capable of producing a finding.",
        bluf.population, bluf.capable
    )
}

/// BLUF line 4: findings confirmed.
pub fn bluf_line_4(bluf: &BlufView) -> String {
    format!(
        "{} {} confirmed.",
        bluf.hit_count,
        plural_word(bluf.hit_count, "finding", "findings")
    )
}

/// The contradiction-check summary shown beneath the BLUF lines: `"{N} checks, no contradiction
/// found"` when every check is clean, otherwise the first non-clean check's own `text` verbatim
/// (never paraphrased) — `"contradiction"` and `"could_not_evaluate"` are both surfaced this way,
/// since either one means the summary cannot honestly say "no contradiction found".
pub fn contradiction_summary(checks: &[ContradictionCheckView]) -> String {
    match checks.iter().find(|check| check.status != "clean") {
        Some(check) => check.text.clone(),
        None => format!("{} checks, no contradiction found", checks.len()),
    }
}

/// The group's badge visual treatment, keyed on the 6 fixed group name strings via a
/// case-insensitive prefix match (rather than an exact-string `match`) — this page's own
/// server-sent `ProbeGroupView.name` values (`"Confirmed Findings"`, ..., `"Skipped"`, Title
/// Case per `crates/probe::report::DETAIL_GROUPS`/`crates/workbench::report_view::from_battery`)
/// and the phase 43 UI spec's prose display labels (`"Confirmed findings"`, ..., `"Skipped by
/// gating"`, sentence case with an extra qualifier) differ in casing and wording despite naming
/// the same six groups — a prefix match keyed on each group's distinctive first word(s) is the
/// one mapping that produces the correct, distinct style for both spellings without maintaining
/// two synchronised literal lists. Mirrors `verdict_badge.rs::badge_style`'s per-family colour
/// choices: Confirmed ~ Hit's red, Suggestive ~ Weak's amber, Data-Integrity ~ DataGap/Blocked's
/// purple stripe, Not Applicable ~ grey outline, Verified Negative ~ a neutral grey (never green
/// — no single `PopulationSource` represents a whole group, so this is a deliberate
/// simplification, never the oracle-verified green), Skipped ~ Skipped's hatched treatment.
pub fn group_badge_style(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.starts_with("confirmed") {
        "background: color-mix(in srgb, var(--destructive) 15%, transparent); border: 1px solid color-mix(in srgb, var(--destructive) 30%, transparent);".to_string()
    } else if lower.starts_with("suggestive") {
        "background: color-mix(in srgb, var(--warning) 15%, transparent); border: 1px solid color-mix(in srgb, var(--warning) 30%, transparent);".to_string()
    } else if lower.starts_with("data-integrity") || lower.starts_with("data integrity") {
        "background: repeating-linear-gradient(45deg, color-mix(in srgb, var(--accent) 12%, transparent), color-mix(in srgb, var(--accent) 12%, transparent) 4px, transparent 4px, transparent 8px); border: 1px dashed var(--accent);".to_string()
    } else if lower.starts_with("not applicable") {
        "background: transparent; border: 1px solid var(--muted-foreground);".to_string()
    } else if lower.starts_with("verified negative") {
        "background: color-mix(in srgb, var(--muted-foreground) 12%, transparent); border: 1px solid var(--border);".to_string()
    } else if lower.starts_with("skipped") {
        "background: repeating-linear-gradient(45deg, color-mix(in srgb, var(--border) 40%, transparent), color-mix(in srgb, var(--border) 40%, transparent) 4px, transparent 4px, transparent 8px); border: 1px solid var(--border);".to_string()
    } else {
        "background: transparent; border: 1px solid var(--border);".to_string()
    }
}

/// The stable id/hash-target string for a probe within a group — the D-18 hash target is the
/// PROBE name alone, never a compound `{group}/{probe}` key, so `group_name` is deliberately
/// unused here; it exists on the signature so every call site reads as "this probe, in this
/// group" even though only the probe name round-trips through `location.hash`.
pub fn probe_detail_key(_group_name: &str, probe_name: &str) -> String {
    probe_name.to_string()
}

/// Resolves a `location.hash` value (with or without its leading `#`) against the already-
/// fetched `report`'s own probe names — never used to construct a request or index unchecked
/// (T-43-07-01). Returns `(group_name, probe_name)` for the first matching probe, or `None` if no
/// probe in the current report carries that name (an absent/stale/tampered hash expands nothing,
/// never panics).
pub fn resolve_hash_target(report: &ReportView, hash: &str) -> Option<(String, String)> {
    let hash = hash.trim_start_matches('#');
    if hash.is_empty() {
        return None;
    }
    for group in &report.groups {
        for probe in &group.probes {
            if probe_detail_key(&group.name, &probe.name) == hash {
                return Some((group.name.clone(), probe.name.clone()));
            }
        }
    }
    None
}

/// D-19's exact Run-failure copy: `"The probe run failed: {error text from the server}."`.
pub fn run_failure_copy(server_error: &str) -> String {
    format!("The probe run failed: {server_error}.")
}

/// Maps a non-200 `/api/analysis/run` response to its user-facing error string — factored out of
/// `trigger_run` so the 409/500 distinction (and the exact 409 wording) is host-testable without
/// an actual Fetch-API round trip. `body_error` is the 500 response's parsed `error` field, if the
/// body was readable.
fn run_error_for_status(status: u16, body_error: Option<&str>) -> String {
    match status {
        409 => "a probe run is already in progress".to_string(),
        500 => run_failure_copy(body_error.unwrap_or("unknown error")),
        other => format!("The probe run request returned an unexpected response (status {other})."),
    }
}

/// Fetches and deserialises `ReportView` from [`REPORT_URL`]. A 404 is the D-16 "no run yet"
/// case — a distinct `Ok(None)` outcome, never folded into the generic `Err`. Any other non-2xx
/// response or a transport/deserialization failure maps to `Err(`[`ANALYSIS_FETCH_ERROR_COPY`]`)`.
///
/// `pub(crate)` since plan 50-07: `finding_detail_page.rs` reuses this SAME function rather than
/// re-fetching through a second copy — the finding detail route needs the identical `ReportView`
/// the Analysis page already holds, per D-03 / the phase 50 UI spec's "seeded from the already-fetched
/// `ReportView` (no new fetch for the finding itself)".
pub(crate) async fn fetch_report() -> Result<Option<ReportView>, String> {
    let response = gloo_net::http::Request::get(REPORT_URL)
        .send()
        .await
        .map_err(|_| ANALYSIS_FETCH_ERROR_COPY.to_string())?;
    if response.status() == 404 {
        return Ok(None);
    }
    if !response.ok() {
        return Err(ANALYSIS_FETCH_ERROR_COPY.to_string());
    }
    response
        .json::<ReportView>()
        .await
        .map(Some)
        .map_err(|_| ANALYSIS_FETCH_ERROR_COPY.to_string())
}

/// Posts an empty body to [`RUN_URL`]. 200 -> the new `ReportView`; any other status is mapped by
/// [`run_error_for_status`], reading the 500 response's `error` field where present.
async fn trigger_run() -> Result<ReportView, String> {
    let response = gloo_net::http::Request::post(RUN_URL)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if response.status() == 200 {
        return response
            .json::<ReportView>()
            .await
            .map_err(|err| err.to_string());
    }
    let body_error: Option<String> =
        response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|e| e.as_str())
                    .map(|s| s.to_string())
            });
    Err(run_error_for_status(
        response.status(),
        body_error.as_deref(),
    ))
}

/// The 6 fixed-name, zero-probe groups shown by the D-19 empty state ("no report yet") — never a
/// real `ReportView`'s `groups`, only ever constructed here for the placeholder rows.
fn empty_groups() -> Vec<ProbeGroupView> {
    FIXED_GROUP_NAMES
        .iter()
        .map(|name| ProbeGroupView {
            name: (*name).to_string(),
            probes: vec![],
        })
        .collect()
}

/// One probe's expanded detail, rendered directly beneath its row (D-17/D-18): hypothesis in a
/// blockquote-style left-border block, the full measurement summary, the `VerdictBadge`'s
/// coverage/population-source fragments at full size (the same component the collapsed row
/// already renders — there is no smaller variant), one `EntityRefView` per evidence row entry
/// preceded by that row's text, the typed cause (if any) in muted text, and the remediation line
/// (if any) prefixed `"Remediation: "`.
fn probe_detail_view(probe: ProbeView) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-sm); padding: var(--space-md); margin-left: var(--space-lg); background: var(--card); border: 1px solid var(--border); border-radius: 8px;">
            <blockquote
                class="text-body"
                style="margin: 0; border-left: 3px solid var(--border); padding-left: var(--space-sm); font-style: italic;"
            >
                {probe.hypothesis.clone()}
            </blockquote>
            <p class="text-body" style="margin: 0;">{probe.measurement_summary.clone()}</p>
            <VerdictBadge verdict=probe.verdict.clone() />
            <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
                {probe.evidence.clone().into_iter().map(|row| {
                    let href = crate::graph_deep_link::graph_href_for_refs(
                        &row.refs.iter().map(|r| (r.kind.clone(), r.id.clone())).collect::<Vec<_>>()
                    );
                    view! {
                        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                            <span class="text-body">{row.text.clone()}</span>
                            <div style="display: flex; flex-direction: column; gap: var(--space-xs); padding-left: var(--space-md);">
                                {row.refs.iter().cloned().map(|entity| view! {
                                    <EntityRefView entity=entity />
                                }).collect_view()}
                            </div>
                            {href.map(|href| view! {
                                <a class="text-label" href=href>"Show on graph"</a>
                            })}
                        </div>
                    }
                }).collect_view()}
            </div>
            {probe.cause.clone().map(|cause| view! {
                <span class="text-body" style="color: var(--muted-foreground);">{cause}</span>
            })}
            {probe.remediation.clone().map(|remediation| view! {
                <span class="text-body">"Remediation: " {remediation}</span>
            })}
        </div>
    }
}

/// One probe row inside an expanded group (D-17/D-18): a `VerdictBadge`, the probe name, a
/// single-line ellipsis-truncated measurement summary. Clicking toggles `open_probe` — the ONE
/// `RwSignal<Option<String>>` governing which probe detail is open across the whole page, so
/// opening a second probe implicitly collapses whichever was open before, regardless of group —
/// and writes/clears `location.hash` via the plain DOM API (never `leptos_router`'s navigation
/// primitives, per the research notes' Pattern 1). The wrapping `<div id=...>` is the scroll-into-view
/// target for D-18's hash-routing on load.
fn probe_row_view(
    group_name: String,
    probe: ProbeView,
    open_probe: RwSignal<Option<String>>,
) -> impl IntoView {
    let key = probe_detail_key(&group_name, &probe.name);
    let key_for_toggle = key.clone();
    let key_for_expanded = key.clone();
    let key_for_show = key.clone();
    let probe_for_detail = probe.clone();

    let toggle = move || {
        let currently_open = open_probe.get().as_deref() == Some(key_for_toggle.as_str());
        if currently_open {
            open_probe.set(None);
            if let Some(window) = web_sys::window() {
                let _ = window.location().set_hash("");
            }
        } else {
            open_probe.set(Some(key_for_toggle.clone()));
            if let Some(window) = web_sys::window() {
                let _ = window.location().set_hash(&key_for_toggle);
            }
        }
    };
    let toggle_click = toggle.clone();
    let onclick = move |_| toggle_click();
    let onkeydown = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Enter" || ev.key() == " " {
            ev.prevent_default();
            toggle();
        }
    };

    view! {
        <div id=key.clone() style="display: flex; flex-direction: column; gap: var(--space-xs);">
            <div
                role="button"
                tabindex="0"
                aria-expanded=move || (open_probe.get().as_deref() == Some(key_for_expanded.as_str())).to_string()
                style="display: flex; align-items: center; gap: var(--space-sm); cursor: pointer; padding: var(--space-sm); border-radius: 6px; min-height: 44px;"
                on:click=onclick
                on:keydown=onkeydown
            >
                <VerdictBadge verdict=probe.verdict.clone() />
                <span class="text-body">{probe.name.clone()}</span>
                <span
                    class="text-body"
                    style="color: var(--muted-foreground); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 360px;"
                >
                    {probe.measurement_summary.clone()}
                </span>
                {{
                    // How many of this probe's evidence rows carry nothing pivotable. Stated
                    // rather than hidden: a probe whose rows are all aggregates offers no route
                    // to the canvas, and the analyst should learn that from the row instead of
                    // discovering it by expanding and finding no links.
                    let without = probe
                        .evidence
                        .iter()
                        .filter(|row| {
                            crate::graph_deep_link::graph_href_for_refs(
                                &row.refs
                                    .iter()
                                    .map(|r| (r.kind.clone(), r.id.clone()))
                                    .collect::<Vec<_>>(),
                            )
                            .is_none()
                        })
                        .count();
                    (without > 0).then(|| view! {
                        <span
                            class="text-label"
                            style="color: var(--muted-foreground); white-space: nowrap;"
                        >
                            {format!(
                                "{without} {} without graph pivots",
                                if without == 1 { "row" } else { "rows" }
                            )}
                        </span>
                    })
                }}
            </div>
            <Show when=move || open_probe.get().as_deref() == Some(key_for_show.as_str())>
                {probe_detail_view(probe_for_detail.clone())}
            </Show>
        </div>
    }
}

/// One of the six always-rendered verdict-group rows (D-17, TRI-04): a `group_badge_style`-tinted
/// dot, the group name, the probe count (`.font-mono`), a chevron. The row wrapper itself carries
/// no `<Show when=...>` that could hide the WHOLE row — only the accordion body's INNER content
/// (the probe list vs. the fixed "No probes were skipped in this run" text) branches on
/// `group.probes.is_empty()`, and only for `is_last` (the Skipped group, always the final entry
/// per `ReportView.groups`'s documented invariant). Clicking toggles membership in `open_groups`
/// (`RwSignal<HashSet<String>>`) — independent per group, unlike probe rows.
fn group_row_view(
    group: ProbeGroupView,
    is_last: bool,
    open_groups: RwSignal<HashSet<String>>,
    open_probe: RwSignal<Option<String>>,
) -> impl IntoView {
    let name = group.name.clone();
    let count = group.probes.len();
    let probes = group.probes.clone();
    let dot_style = group_badge_style(&group.name);

    let name_for_toggle = name.clone();
    let toggle = move || {
        open_groups.update(|set| {
            if set.contains(&name_for_toggle) {
                set.remove(&name_for_toggle);
            } else {
                set.insert(name_for_toggle.clone());
            }
        });
    };
    let toggle_click = toggle.clone();
    let onclick = move |_| toggle_click();
    let onkeydown = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Enter" || ev.key() == " " {
            ev.prevent_default();
            toggle();
        }
    };

    let name_for_expanded = name.clone();
    let name_for_show = name.clone();
    let group_name_for_probes = name.clone();

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
            <div
                role="button"
                tabindex="0"
                aria-expanded=move || open_groups.get().contains(&name_for_expanded).to_string()
                style="display: flex; align-items: center; gap: var(--space-sm); background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-md); cursor: pointer; min-height: 44px;"
                on:click=onclick
                on:keydown=onkeydown
            >
                <span
                    aria-hidden="true"
                    style=format!("width: 12px; height: 12px; border-radius: 9999px; flex-shrink: 0; {dot_style}")
                ></span>
                <span class="text-heading">{name.clone()}</span>
                <span class="text-body font-mono">"(" {count.to_string()} ")"</span>
                <span aria-hidden="true" style="margin-left: auto;">"\u{203a}"</span>
            </div>
            <Show when=move || open_groups.get().contains(&name_for_show)>
                <div style="display: flex; flex-direction: column; gap: var(--space-xs); padding-left: var(--space-lg);">
                    {
                        if is_last && probes.is_empty() {
                            view! {
                                <span class="text-body" style="color: var(--muted-foreground);">
                                    "No probes were skipped in this run"
                                </span>
                            }.into_any()
                        } else {
                            let group_name = group_name_for_probes.clone();
                            probes.clone().into_iter().map(move |probe| {
                                probe_row_view(group_name.clone(), probe, open_probe)
                            }).collect_view().into_any()
                        }
                    }
                </div>
            </Show>
        </div>
    }
}

/// The two independently-collapsible bottom sections ("How the search adapted", "Recommended
/// remediation") — same header-plus-chevron visual pattern as a top-level accordion row but
/// governed by their own, independent `open` signal. `numbered` selects an `<ol>` (adapted notes)
/// vs. a `<ul>` (remediation items); numbering/bullets are applied at RENDER time by the browser,
/// never trusted as a pre-numbered prefix baked into the server's strings.
fn collapsible_section(
    title: &'static str,
    items: Vec<String>,
    open: RwSignal<bool>,
    numbered: bool,
) -> impl IntoView {
    let toggle = move || open.update(|value| *value = !*value);
    let toggle_click = toggle;
    let onclick = move |_| toggle_click();
    let onkeydown = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Enter" || ev.key() == " " {
            ev.prevent_default();
            toggle();
        }
    };

    view! {
        <div style="background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-md);">
            <div
                role="button"
                tabindex="0"
                aria-expanded=move || open.get().to_string()
                style="display: flex; align-items: center; gap: var(--space-sm); cursor: pointer; min-height: 44px;"
                on:click=onclick
                on:keydown=onkeydown
            >
                <span class="text-heading">{title}</span>
                <span aria-hidden="true" style="margin-left: auto;">"\u{203a}"</span>
            </div>
            <Show when=move || open.get()>
                <div style="margin-top: var(--space-sm);">
                    {
                        if numbered {
                            view! {
                                <ol class="text-body">
                                    {items.clone().into_iter().map(|item| view! { <li>{item}</li> }).collect_view()}
                                </ol>
                            }.into_any()
                        } else {
                            view! {
                                <ul class="text-body">
                                    {items.clone().into_iter().map(|item| view! { <li>{item}</li> }).collect_view()}
                                </ul>
                            }.into_any()
                        }
                    }
                </div>
            </Show>
        </div>
    }
}

/// The Analysis page (D-06/D-15/D-17/D-18/D-19, TRI-03/04/05): fetches [`REPORT_URL`] on mount,
/// renders the BLUF box + Run button state machine + six verdict-group rows + accordion, and
/// reactively expands/scrolls to whichever probe `location.hash` names on load or on a later hash
/// change.
#[component]
#[allow(non_snake_case)]
pub fn AnalysisPage() -> impl IntoView {
    let report_signal: RwSignal<Option<ReportView>> = RwSignal::new(None);
    let run_state: RwSignal<RunState> = RwSignal::new(RunState::Idle);
    let open_groups: RwSignal<HashSet<String>> = RwSignal::new(HashSet::new());
    let open_probe: RwSignal<Option<String>> = RwSignal::new(None);
    let adapted_open: RwSignal<bool> = RwSignal::new(false);
    let remediation_open: RwSignal<bool> = RwSignal::new(false);

    let initial_fetch = LocalResource::new(fetch_report);
    let location = use_location();

    // Writes the first `fetch_report()` resolution into `report_signal` — the only place this
    // signal is populated from a GET; every subsequent update comes from a successful Run
    // replacing it wholesale (never merged field-by-field).
    Effect::new(move |_| {
        if let Some(Ok(maybe_report)) = initial_fetch.get() {
            report_signal.set(maybe_report);
        }
    });

    // D-18's READ side: reactively re-runs whenever `location.hash` OR `report_signal` change
    // (e.g. a fresh Run replacing the report while a hash is still present in the address bar).
    Effect::new(move |_| {
        let hash = location.hash.get();
        if let Some(report) = report_signal.get() {
            if let Some((group_name, probe_name)) = resolve_hash_target(&report, &hash) {
                open_groups.update(|set| {
                    set.insert(group_name);
                });
                open_probe.set(Some(probe_name.clone()));
                if let Some(window) = web_sys::window() {
                    if let Some(document) = window.document() {
                        if let Some(element) = document.get_element_by_id(&probe_name) {
                            element.scroll_into_view();
                        }
                    }
                }
            }
        }
    });

    let run_onclick = move |_| {
        if run_state.get_untracked() == RunState::Running {
            return;
        }
        run_state.set(RunState::Running);
        leptos::task::spawn_local(async move {
            match trigger_run().await {
                Ok(new_report) => {
                    report_signal.set(Some(new_report));
                    run_state.set(RunState::Idle);
                }
                Err(msg) => {
                    run_state.set(RunState::Failed(msg));
                }
            }
        });
    };

    view! {
        <div>
            <Breadcrumb />
            <div style="background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-lg); margin-top: var(--space-lg); display: flex; flex-direction: column; gap: var(--space-sm);">
                {move || match report_signal.get() {
                    None => match headline_state(None, initial_fetch.get().as_ref()) {
                        HeadlineState::Loading => view! {
                            <p class="text-body" style="margin: 0; color: var(--muted-foreground);">{ANALYSIS_LOADING_COPY}</p>
                        }.into_any(),
                        HeadlineState::Failed(msg) => view! {
                            <p class="text-body" style="margin: 0; color: var(--destructive);">{msg}</p>
                        }.into_any(),
                        HeadlineState::Empty | HeadlineState::Report => view! {
                            <h2 class="text-heading" style="margin: 0;">"No probe run yet for this analysis"</h2>
                        }.into_any(),
                    },
                    Some(report) => {
                        let run_at = report.run_at.clone();
                        let checks = report.contradiction_checks.clone();
                        view! {
                            <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                <span class="text-body">{bluf_line_1(&report.bluf)}</span>
                                <span class="text-body">{bluf_line_2(&report.bluf)}</span>
                                <span class="text-body">{bluf_line_3(&report.bluf)}</span>
                                <span class="text-body">{bluf_line_4(&report.bluf)}</span>
                            </div>
                            <div style="display: flex; align-items: center; gap: var(--space-md); flex-wrap: wrap; margin-top: var(--space-sm);">
                                <Timestamp at=run_at />
                                <span class="text-body">{contradiction_summary(&checks)}</span>
                            </div>
                        }.into_any()
                    }
                }}
                <div style="display: flex; align-items: center; gap: var(--space-md); margin-top: var(--space-sm);">
                    <button
                        class="text-body"
                        style="background: var(--primary); color: var(--primary-foreground); border: none; border-radius: 6px; padding: var(--space-xs) var(--space-md); cursor: pointer; min-height: 44px;"
                        on:click=run_onclick
                        disabled=move || run_state.get() == RunState::Running
                    >
                        {move || if run_state.get() == RunState::Running { "Running…" } else { "Run" }}
                    </button>
                    {move || report_signal.get().map(|_| view! {
                        // `download` is load-bearing, not decoration: leptos_router intercepts
                        // every same-origin anchor click unless the element carries `download` or
                        // `rel="external"` (leptos_router 0.8 location/mod.rs). Without it this
                        // href is routed client-side, matches none of the app's routes, and the
                        // operator gets app.rs's `Routes fallback` — a bare "Not found." — while
                        // the endpoint itself serves the dossier perfectly well.
                        <a class="text-label" download style="color: var(--primary);" href=REPORT_MD_URL>"Export dossier"</a>
                    })}
                </div>
                <div aria-live="polite" style="display: flex; flex-direction: column; gap: var(--space-xs);">
                    {move || match run_state.get() {
                        RunState::Running => Some(view! {
                            <span class="text-label" style="color: var(--muted-foreground);">"Running probe battery…"</span>
                        }.into_any()),
                        RunState::Failed(msg) => Some(view! {
                            <span class="text-body" style="color: var(--destructive);">{msg}</span>
                        }.into_any()),
                        RunState::Idle => None,
                    }}
                </div>
            </div>
            <a
                href="#report-accordion"
                class="text-label"
                style="position: sticky; top: 0; z-index: 1; align-self: flex-end; display: inline-flex; align-items: center; min-height: 44px; margin-top: var(--space-xl); color: var(--primary); background: var(--background);"
            >
                "Board \u{00b7} Report"
            </a>
            {move || report_signal.get().map(|report| view! {
                <FindingBoard groups=report.groups.clone() />
            })}
            <div id="report-accordion" style="display: flex; flex-direction: column; gap: var(--space-sm); margin-top: var(--space-2xl);">
                {move || {
                    let groups = report_signal.get().map(|r| r.groups.clone()).unwrap_or_else(empty_groups);
                    let last_index = groups.len().saturating_sub(1);
                    groups
                        .into_iter()
                        .enumerate()
                        .map(|(i, group)| group_row_view(group, i == last_index, open_groups, open_probe))
                        .collect_view()
                }}
            </div>
            {move || report_signal.get().map(|report| view! {
                <div style="display: flex; flex-direction: column; gap: var(--space-md); margin-top: var(--space-xl);">
                    {collapsible_section("How the search adapted", report.adapted_notes.clone(), adapted_open, true)}
                    {collapsible_section("Recommended remediation", report.remediation_items.clone(), remediation_open, false)}
                </div>
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::VerdictView;

    fn bluf(overrides: impl FnOnce(&mut BlufView)) -> BlufView {
        let mut bluf = BlufView {
            could_not_evaluate: 0,
            total_probes: 22,
            skipped: 0,
            population: 0,
            capable: 0,
            hit_count: 0,
        };
        overrides(&mut bluf);
        bluf
    }

    #[test]
    fn urls_match_the_native_analysis_endpoints() {
        assert_eq!(REPORT_URL, "/api/analysis/report");
        assert_eq!(RUN_URL, "/api/analysis/run");
        assert_eq!(REPORT_MD_URL, "/api/analysis/report.md");
    }

    #[test]
    fn analysis_fetch_error_copy_names_the_report_endpoint() {
        assert_eq!(
            ANALYSIS_FETCH_ERROR_COPY,
            "The workbench returned data this page cannot read (/api/analysis/report)."
        );
    }

    #[test]
    fn headline_is_loading_until_the_initial_fetch_resolves() {
        assert_eq!(headline_state(None, None), HeadlineState::Loading);
    }

    #[test]
    fn headline_surfaces_a_failed_initial_fetch_instead_of_the_empty_state() {
        let failed: Result<Option<ReportView>, String> = Err(ANALYSIS_FETCH_ERROR_COPY.to_string());
        assert_eq!(
            headline_state(None, Some(&failed)),
            HeadlineState::Failed(ANALYSIS_FETCH_ERROR_COPY.to_string())
        );
    }

    #[test]
    fn headline_is_empty_only_when_the_fetch_resolved_to_no_report() {
        let none: Result<Option<ReportView>, String> = Ok(None);
        assert_eq!(headline_state(None, Some(&none)), HeadlineState::Empty);
    }

    #[test]
    fn headline_shows_the_report_whenever_one_is_in_hand() {
        let report = sample_report_for_hash_tests();
        let failed: Result<Option<ReportView>, String> = Err("stale".to_string());
        assert_eq!(
            headline_state(Some(&report), Some(&failed)),
            HeadlineState::Report
        );
        assert_eq!(headline_state(Some(&report), None), HeadlineState::Report);
    }

    #[test]
    fn loading_copy_is_not_the_empty_state_heading() {
        assert_ne!(ANALYSIS_LOADING_COPY, "No probe run yet for this analysis");
        assert!(ANALYSIS_LOADING_COPY.starts_with("Loading"));
    }

    #[test]
    fn bluf_line_1_reports_all_probes_could_evaluate_at_zero() {
        let view = bluf(|b| {
            b.could_not_evaluate = 0;
            b.total_probes = 22;
        });
        assert_eq!(bluf_line_1(&view), "All 22 probes could evaluate.");
    }

    #[test]
    fn bluf_line_1_reports_the_could_not_evaluate_count() {
        let view = bluf(|b| {
            b.could_not_evaluate = 3;
            b.total_probes = 22;
        });
        assert_eq!(
            bluf_line_1(&view),
            "3 of 22 probes could not evaluate (data gap or blocked)."
        );
    }

    #[test]
    fn bluf_line_2_reports_zero_skipped() {
        let view = bluf(|b| b.skipped = 0);
        assert_eq!(bluf_line_2(&view), "0 probes skipped by gating.");
    }

    #[test]
    fn bluf_line_3_reports_population_and_capable() {
        let view = bluf(|b| {
            b.population = 638_016;
            b.capable = 638_015;
        });
        assert_eq!(
            bluf_line_3(&view),
            "Of 638016 records evaluated, 638015 were structurally capable of producing a finding."
        );
    }

    #[test]
    fn bluf_line_4_reports_hit_count() {
        let view = bluf(|b| b.hit_count = 10);
        assert_eq!(bluf_line_4(&view), "10 findings confirmed.");
    }

    fn checks_all_clean() -> Vec<ContradictionCheckView> {
        vec![
            ContradictionCheckView {
                label: "check-a".to_string(),
                text: "check-a is clean".to_string(),
                status: "clean".to_string(),
            },
            ContradictionCheckView {
                label: "check-b".to_string(),
                text: "check-b is clean".to_string(),
                status: "clean".to_string(),
            },
        ]
    }

    #[test]
    fn contradiction_summary_reports_count_when_all_clean() {
        assert_eq!(
            contradiction_summary(&checks_all_clean()),
            "2 checks, no contradiction found"
        );
    }

    #[test]
    fn contradiction_summary_returns_the_failing_checks_own_text_verbatim() {
        let mut checks = checks_all_clean();
        checks[0].status = "contradiction".to_string();
        checks[0].text = "exact contradiction text, not paraphrased".to_string();
        assert_eq!(
            contradiction_summary(&checks),
            "exact contradiction text, not paraphrased"
        );
    }

    #[test]
    fn group_badge_style_distinguishes_skipped_from_confirmed() {
        let confirmed = group_badge_style("Confirmed findings");
        let skipped = group_badge_style("Skipped by gating");
        assert_ne!(confirmed, skipped);
    }

    #[test]
    fn group_badge_style_is_distinct_for_all_six_server_sent_names() {
        let styles: Vec<String> = FIXED_GROUP_NAMES
            .iter()
            .map(|n| group_badge_style(n))
            .collect();
        let unique: std::collections::HashSet<_> = styles.iter().collect();
        assert_eq!(
            unique.len(),
            6,
            "all 6 fixed group names must produce distinct badge styles"
        );
    }

    #[test]
    fn probe_detail_key_returns_the_bare_probe_name() {
        assert_eq!(
            probe_detail_key("Confirmed findings", "shared-exact-address"),
            "shared-exact-address"
        );
    }

    fn sample_probe(name: &str) -> ProbeView {
        ProbeView {
            name: name.to_string(),
            tier: "tier-1".to_string(),
            hypothesis: "test hypothesis".to_string(),
            verdict: VerdictView::DataGap {
                missing_input: "missing OSINT source".to_string(),
            },
            measurement_summary: "test summary".to_string(),
            evidence: vec![],
            motif_kind: "Absence".to_string(),
            cause: None,
            remediation: None,
        }
    }

    fn sample_report_for_hash_tests() -> ReportView {
        ReportView {
            bluf: bluf(|_| {}),
            contradiction_checks: vec![],
            groups: vec![
                ProbeGroupView {
                    name: "Confirmed Findings".to_string(),
                    probes: vec![sample_probe("shared-exact-address")],
                },
                ProbeGroupView {
                    name: "Skipped".to_string(),
                    probes: vec![],
                },
            ],
            adapted_notes: vec![],
            remediation_items: vec![],
            run_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn resolve_hash_target_finds_the_owning_group_for_an_existing_probe() {
        let report = sample_report_for_hash_tests();
        assert_eq!(
            resolve_hash_target(&report, "shared-exact-address"),
            Some((
                "Confirmed Findings".to_string(),
                "shared-exact-address".to_string()
            ))
        );
    }

    #[test]
    fn resolve_hash_target_strips_a_leading_hash_character() {
        let report = sample_report_for_hash_tests();
        assert_eq!(
            resolve_hash_target(&report, "#shared-exact-address"),
            Some((
                "Confirmed Findings".to_string(),
                "shared-exact-address".to_string()
            ))
        );
    }

    #[test]
    fn resolve_hash_target_returns_none_for_an_absent_probe_name() {
        let report = sample_report_for_hash_tests();
        assert_eq!(resolve_hash_target(&report, "does-not-exist"), None);
    }

    #[test]
    fn resolve_hash_target_returns_none_for_an_empty_hash() {
        let report = sample_report_for_hash_tests();
        assert_eq!(resolve_hash_target(&report, ""), None);
        assert_eq!(resolve_hash_target(&report, "#"), None);
    }

    #[test]
    fn run_failure_copy_matches_the_d19_exact_format() {
        assert_eq!(
            run_failure_copy("timeout waiting for battery"),
            "The probe run failed: timeout waiting for battery."
        );
    }

    #[test]
    fn run_error_for_status_distinguishes_409_from_500() {
        let conflict = run_error_for_status(409, None);
        let failure = run_error_for_status(500, Some("battery run failed: timeout"));
        assert_eq!(conflict, "a probe run is already in progress");
        assert_ne!(conflict, failure);
        assert_eq!(
            failure,
            "The probe run failed: battery run failed: timeout."
        );
    }

    /// The fallback arm is rendered verbatim in the Run-failure line, so it is UI copy: it must
    /// name the status and must not carry a bare glossary abbreviation (D-03 — "HTTP" is a
    /// glossary row and there is no `<Abbr>` on this path).
    #[test]
    fn run_error_for_status_fallback_names_the_status_without_a_bare_abbreviation() {
        let msg = run_error_for_status(502, None);
        assert!(msg.contains("502"), "got: {msg}");
        assert!(!msg.contains("HTTP"), "got: {msg}");
    }

    #[test]
    fn run_error_for_status_500_falls_back_to_unknown_error_when_body_unreadable() {
        assert_eq!(
            run_error_for_status(500, None),
            "The probe run failed: unknown error."
        );
    }
}
