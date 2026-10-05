//! `AnalysisPicker` — the app shell's read-only `_catalog` list (D-10), per the phase 42 UI spec's
//! "App shell layout" Analysis picker paragraph.
//!
//! Per D-10 as amended in the phase 42 design notes: this phase's workbench boots one replay-only engine
//! bound to a single analysis database, so clicking a non-current entry does NOT re-fetch
//! `/api/workbench-status` against a different database — it discloses the exact restart command
//! the operator would run instead, honestly communicating that switching requires a restart.
//! Live re-targeting is a Phase 43+ concern (recorded in this plan's SUMMARY).
//!
//! No hardcoded analysis-specific literal (an analysis name, database id or ward code) appears
//! anywhere in this file — every string rendered comes from the fetched `AnalysisSummaryView`
//! data or generic UI copy, enforced by `scripts/check-no-analysis-literals.sh` (D-10 amended).

use leptos::prelude::*;
use urban_integrity_workbench_dto::AnalysisSummaryView;

/// `GET /api/analyses` — the native endpoint plan 42-05 built (`analyses_handler`).
pub const ANALYSES_URL: &str = "/api/analyses";

/// Fetches and deserialises the read-only `_catalog` listing from [`ANALYSES_URL`].
async fn fetch_analyses() -> Result<Vec<AnalysisSummaryView>, String> {
    let response = gloo_net::http::Request::get(ANALYSES_URL)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.ok() {
        return Err(format!(
            "analyses responded with status {}",
            response.status()
        ));
    }
    response
        .json::<Vec<AnalysisSummaryView>>()
        .await
        .map_err(|err| err.to_string())
}

/// The one entry whose `is_current` is `true`, or `None` if the fetched list is empty or (should
/// never happen per `analyses_handler`'s contract) carries no current entry.
pub fn current_entry(list: &[AnalysisSummaryView]) -> Option<&AnalysisSummaryView> {
    list.iter().find(|entry| entry.is_current)
}

/// The exact restart command disclosed when the operator picks a non-current analysis (D-10
/// amended) — the operator would run this verbatim to re-target the workbench at `db_name`.
/// `db_name` comes only from the already-fetched, server-controlled `/api/analyses` list — never
/// client-editable free text (threat T-42-02's accepted-risk mitigation).
pub fn restart_command(db_name: &str) -> String {
    format!("urban-integrity-cli workbench --analysis {db_name}")
}

/// A picker row's inline style — `--card-hover` background on hover, transparent otherwise.
/// Implemented as a per-row `hovered` signal (rather than a new CSS `:hover` rule) so this
/// component stays self-contained within this plan's `files_modified` list.
fn row_style(hovered: bool) -> String {
    let background = if hovered {
        "var(--card-hover)"
    } else {
        "transparent"
    };
    format!(
        "display: flex; flex-direction: column; gap: 2px; padding: var(--space-sm); border-radius: 6px; cursor: pointer; background: {background};"
    )
}

/// One row in the analysis picker's panel: label + monospace database id, `--card-hover` on
/// hover, clicking discloses the restart command via `restart_target`.
#[component]
#[allow(non_snake_case)]
fn PickerRow(
    entry: AnalysisSummaryView,
    restart_target: RwSignal<Option<String>>,
) -> impl IntoView {
    let hovered = RwSignal::new(false);
    let db_name = entry.db_name.clone();
    let db_name_for_click = db_name.clone();

    view! {
        <div
            class="text-body"
            style=move || row_style(hovered.get())
            on:mouseenter=move |_| hovered.set(true)
            on:mouseleave=move |_| hovered.set(false)
            on:click=move |_| restart_target.set(Some(db_name_for_click.clone()))
        >
            <span class="text-body">{entry.label.clone()}</span>
            <span class="text-label font-mono" style="color: var(--muted-foreground);">{db_name}</span>
        </div>
    }
}

/// The picker button (shows the current entry's `db_name`, Body size, per this plan's
/// `<behavior>` block) plus, when open, the panel listing every fetched entry and — once a
/// non-current row has been clicked — the restart-command disclosure.
fn picker_body(
    list: Vec<AnalysisSummaryView>,
    open: RwSignal<bool>,
    restart_target: RwSignal<Option<String>>,
) -> impl IntoView {
    let current_label = current_entry(&list)
        .map(|entry| entry.db_name.clone())
        .unwrap_or_else(|| "No analysis".to_string());
    let rows = list.clone();

    view! {
        <div>
            <button
                class="text-body"
                style="background: transparent; border: 1px solid var(--border); border-radius: 6px; padding: var(--space-xs) var(--space-sm); color: var(--foreground); cursor: pointer;"
                on:click=move |_| open.update(|value| *value = !*value)
            >
                {current_label}
            </button>
            <Show when=move || open.get()>
                <div style="position: absolute; top: calc(100% + var(--space-xs)); right: 0; background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-sm); min-width: 280px; z-index: 10; display: flex; flex-direction: column; gap: var(--space-xs);">
                    {rows.clone().into_iter().map(|entry| view! {
                        <PickerRow entry=entry restart_target=restart_target />
                    }).collect_view()}
                    {move || restart_target.get().map(|db_name| {
                        let command = restart_command(&db_name);
                        view! {
                            <div
                                class="text-body"
                                style="margin-top: var(--space-sm); padding-top: var(--space-sm); border-top: 1px solid var(--border); display: flex; flex-direction: column; gap: var(--space-xs);"
                            >
                                <span>"Switching analyses requires a restart. Run:"</span>
                                <code class="text-body font-mono" style="user-select: all; word-break: break-all;">{command}</code>
                            </div>
                        }
                    })}
                </div>
            </Show>
        </div>
    }
}

/// The header's analysis picker (D-10): fetches [`ANALYSES_URL`] on mount via a `LocalResource`,
/// shows "Loading analyses…" while pending, the button+panel on success, a generic failure line
/// on error — no analysis-specific example text anywhere.
#[component]
#[allow(non_snake_case)]
pub fn AnalysisPicker() -> impl IntoView {
    let analyses = LocalResource::new(fetch_analyses);
    let open = RwSignal::new(false);
    let restart_target: RwSignal<Option<String>> = RwSignal::new(None);

    view! {
        <div style="position: relative;">
            <Suspense fallback=move || view! {
                <div class="text-body" style="color: var(--muted-foreground); padding: var(--space-xs) var(--space-sm);">
                    "Loading analyses…"
                </div>
            }>
                {move || {
                    analyses.get().map(|result| match result {
                        Ok(list) => picker_body(list, open, restart_target).into_any(),
                        Err(_) => view! {
                            <div class="text-body" style="color: var(--muted-foreground); padding: var(--space-xs) var(--space-sm);">
                                "Could not load analyses."
                            </div>
                        }.into_any(),
                    })
                }}
            </Suspense>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_list() -> Vec<AnalysisSummaryView> {
        vec![
            AnalysisSummaryView {
                db_name: "an_first_1a2b3c".to_string(),
                label: "First analysis".to_string(),
                is_current: false,
            },
            AnalysisSummaryView {
                db_name: "an_second_4d5e6f".to_string(),
                label: "Second analysis".to_string(),
                is_current: true,
            },
        ]
    }

    /// The fetch URL is exactly the native endpoint plan 42-05 built.
    #[test]
    fn analyses_url_matches_the_native_analyses_endpoint() {
        assert_eq!(ANALYSES_URL, "/api/analyses");
    }

    /// `current_entry` returns exactly the one `is_current == true` entry, never the first entry
    /// in list order.
    ///
    /// PROVE THIS CAN FAIL: this test was first run against a version of `current_entry` that
    /// returned `list.first()` unconditionally — the assertion failed with
    /// `Some("an_first_1a2b3c")` instead of `Some("an_second_4d5e6f")`, since the sample list's
    /// first entry is NOT the current one, before `current_entry` was corrected to filter on
    /// `is_current` as committed here.
    #[test]
    fn current_entry_returns_the_entry_marked_is_current() {
        let list = sample_list();
        let current = current_entry(&list).expect("sample list has a current entry");
        assert_eq!(current.db_name, "an_second_4d5e6f");
    }

    #[test]
    fn current_entry_returns_none_for_an_empty_list() {
        assert!(current_entry(&[]).is_none());
    }

    /// `restart_command` produces the exact CLI invocation the operator would run, with the
    /// clicked entry's `db_name` interpolated — never a client-editable free-text value (threat
    /// T-42-02's accepted-risk mitigation: the value only ever comes from a fetched
    /// `AnalysisSummaryView`, never typed by the user).
    #[test]
    fn restart_command_formats_the_exact_cli_invocation() {
        assert_eq!(
            restart_command("an_second_4d5e6f"),
            "urban-integrity-cli workbench --analysis an_second_4d5e6f"
        );
    }

    /// `row_style` applies `--card-hover` only when hovered, transparent otherwise — the D-11
    /// hover-state contract for the analysis-picker's list rows.
    #[test]
    fn row_style_applies_card_hover_only_when_hovered() {
        assert!(row_style(true).contains("var(--card-hover)"));
        assert!(!row_style(false).contains("var(--card-hover)"));
        assert!(row_style(false).contains("transparent"));
    }

    // Note: no in-crate "fixtures contain no forbidden literal" test lives here — asserting a
    // hardcoded, forbidden fixture string is absent would itself have to spell out that same
    // string in this source file, which is exactly what `scripts/check-no-analysis-literals.sh`'s
    // CI-level, case-insensitive file scan (D-10 amended) flags. That script is the authoritative
    // guard for this file and every other workbench source file (see `cosmograph_spike.rs`'s
    // identical note); duplicating it in-crate would only reintroduce the forbidden literal into
    // the source it exists to keep clean. `sample_list()`'s fixture names above (`an_first_...`,
    // `an_second_...`) are deliberately generic and were written to contain no such literal.
}
