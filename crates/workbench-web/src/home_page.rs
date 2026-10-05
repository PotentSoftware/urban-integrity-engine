//! `HomePage` — the workbench's launcher (NAV-01, D-10, D-11, D-12): a CSS grid of four
//! non-hierarchical cards (Analysis, Graph, Ingestors, Help), each independently populated from
//! [`HomeView`]. Home has no breadcrumb (it is the root) and no sidebar — this is the ONLY
//! navigation surface besides the breadcrumb per the phase 43 UI spec's "Home page" section.
//!
//! # Testing approach (plain-function-first, per `verdict_badge.rs`/`status_page.rs`'s convention)
//!
//! No headless-browser runner is wired into this workspace's `cargo test` (the phase 42 research notes
//! Pitfall 10), so every "what text renders for this data" decision is pulled out as a plain,
//! host-testable function (`analysis_card_headline`, `graph_card_headline`, etc.) and tested
//! directly, mirroring `status_page.rs::population_headline`. The `#[component] HomePage` itself
//! only interpolates these functions' output into `view!`.

use std::time::Duration;

use leptos::prelude::*;
use leptos_router::components::A;
use urban_integrity_workbench_dto::{AnalysisCardView, GraphCardView, HomeView, IngestorsCardView};

use crate::abbr::Abbr;
use crate::help::CLI_ABBR;
use crate::timestamp::Timestamp;

/// The honest headline while a large replay's CSR bake has not yet landed (checkpoint fix item
/// 4) — never a fabricated `0 nodes · 0 edges` in that window. Mirrors `graph_page.rs`'s
/// `SNAPSHOT_BAKING_HEADING` exactly (kept as a separate constant rather than shared across the
/// wasm-only crate boundary the two page modules already live inside independently, per this
/// crate's existing one-page-one-module convention).
pub const GRAPH_CARD_SNAPSHOT_BAKING_HEADLINE: &str = "Graph snapshot baking\u{2026}";

/// `GET /api/home` — the native endpoint plan 43-05 built (`home_handler`).
pub const HOME_URL: &str = "/api/home";

/// The exact D-23 error copy, with `/api/home`'s literal endpoint path substituted — shown
/// verbatim on a fetch/deserialization failure, never a generic/paraphrased message and never
/// partial data.
pub const HOME_FETCH_ERROR_COPY: &str =
    "The workbench returned data this page cannot read (/api/home).";

/// Shared card chrome: 160px minimum height (the phase 43 UI spec "Home card minimum height"), `--card`
/// background, 1px `--border`, 8px radius, `lg` internal padding, `xs` gap between the headline
/// and the support lines beneath it. Hover/focus states for the three clickable cards live in the
/// `.home-card` class defined in `HomePage`'s own `<style>` block below (kept local to this file
/// rather than added to `style/tokens.css`, since inline styles alone cannot express `:hover`).
const CARD_STYLE: &str = "display: flex; flex-direction: column; gap: var(--space-xs); min-height: 160px; background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-lg); text-decoration: none; color: inherit; cursor: pointer;";

/// The Ingestors card's chrome — identical box model to [`CARD_STYLE`] but `cursor: default` and
/// no `.home-card` class, since Ingestors is the one non-interactive card (the phase 43 UI spec: "no
/// hover state — it is not interactive").
const INGESTORS_CARD_STYLE: &str = "display: flex; flex-direction: column; gap: var(--space-xs); min-height: 160px; background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-lg); cursor: default;";

const CAPTION_STYLE: &str = "color: var(--muted-foreground);";
/// Card title: the dominant element of each card (Display size, `--success` green). Was a 12px
/// muted Label, then a 16px muted Heading, until 2026-09-06 — the operator found both too small
/// beside the headline directly under it, so the headline is now the Heading-size line instead,
/// and asked for the titles in green. This is a deliberate operator exception to D-12's
/// "green only for the Clear verdict" rule, recorded in the phase 43 UI spec's card anatomy.
const TITLE_STYLE: &str = "color: var(--success);";
const OPEN_AFFORDANCE_STYLE: &str =
    "color: var(--primary); align-self: flex-end; margin-top: auto;";

/// Fetches and deserialises [`HomeView`] from [`HOME_URL`]. Mirrors `status_page.rs::fetch_status`
/// exactly: returns `Err` with a short, loggable reason on any transport/deserialisation failure
/// or non-2xx response — the caller never surfaces this raw string, only [`HOME_FETCH_ERROR_COPY`].
async fn fetch_home() -> Result<HomeView, String> {
    let response = gloo_net::http::Request::get(HOME_URL)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.ok() {
        return Err(format!("home responded with status {}", response.status()));
    }
    response
        .json::<HomeView>()
        .await
        .map_err(|err| err.to_string())
}

/// The Analysis card's headline: "No probe run yet" when no report exists yet
/// (`last_run_at: None`), else "{N} findings confirmed" (the phase 43 UI spec Copywriting Contract).
/// Driven by `last_run_at`'s `Some`/`None` state, per `HomeView`'s own documented convention —
/// never inferred from `findings_confirmed` alone (a report with zero findings still "ran").
pub fn analysis_card_headline(analysis: &AnalysisCardView) -> String {
    match &analysis.last_run_at {
        Some(_) => format!("{} findings confirmed", analysis.findings_confirmed),
        None => "No probe run yet".to_string(),
    }
}

/// The Analysis card's second support line: "{X} of {total} probes could not evaluate, {Y}
/// skipped" — `total` comes from `AnalysisCardView.total_probes`, never a hardcoded `22` (the
/// UI-SPEC's example text uses the literal count of today's battery, but this function reads the
/// server-supplied field so a future battery size change needs no frontend edit).
pub fn analysis_card_support_line_2(analysis: &AnalysisCardView) -> String {
    format!(
        "{} of {} probes could not evaluate, {} skipped",
        analysis.probes_could_not_evaluate, analysis.total_probes, analysis.probes_skipped
    )
}

/// The Graph card's headline (D-11): [`GRAPH_CARD_SNAPSHOT_BAKING_HEADLINE`] while the CSR
/// snapshot is still baking after a historical replay (checkpoint fix item 4 — takes priority
/// over both other states, since `node_count`/`edge_count` are the empty initial snapshot's `0`s
/// in that window, not real data), else "Population unavailable" when the canonical population
/// could not be computed, else the node/edge counts (Display-size, per the phase 43 UI spec's
/// Typography table listing "node/edge counts" as a Home card headline example) — never a
/// fabricated `0` and never the population count itself (that lives in
/// [`graph_card_support_line_1`]).
pub fn graph_card_headline(graph: &GraphCardView) -> String {
    if graph.snapshot_pending {
        return GRAPH_CARD_SNAPSHOT_BAKING_HEADLINE.to_string();
    }
    match graph.population.count {
        Some(_) => format!(
            "{} nodes \u{b7} {} edges",
            graph.node_count, graph.edge_count
        ),
        None => "Population unavailable".to_string(),
    }
}

/// The Graph card's first support line: "population {N}" when available, else the server's typed
/// `unavailable_reason` verbatim (D-11) — never a fabricated `0`, never paraphrased.
pub fn graph_card_support_line_1(graph: &GraphCardView) -> String {
    match graph.population.count {
        Some(count) => format!("population {count}"),
        None => graph
            .population
            .unavailable_reason
            .clone()
            .unwrap_or_default(),
    }
}

/// The Ingestors card's dynamic timestamp-line content: the most recent cross-source event
/// (`overall_last_event`) when at least one source has ever produced one, else `None` — the view
/// renders `None` as the grey-dot "Not running" state (mirrors `status_page.rs::INGESTORS_LINE_TEXT`'s
/// convention: never `--success`/`--destructive` styling, always `--muted-foreground`, regardless
/// of whether individual sources have historical events from a prior live session).
pub fn ingestors_card_line(ingestors: &IngestorsCardView) -> Option<String> {
    ingestors.overall_last_event.clone()
}

/// The Help card's headline: "{N} terms", where `N` is the client-side parsed glossary term
/// count (`crate::glossary::term_count()`). The glossary lives in this wasm-only crate,
/// unreachable from the native `crates/workbench`, so `HomeView` carries no Help figure at all
/// (phase 43 code review finding WR-01 removed the server's former placeholder `0`).
pub fn help_card_headline() -> String {
    format!("{} terms", crate::glossary::term_count())
}

fn analysis_card_view(analysis: &AnalysisCardView) -> impl IntoView {
    let headline = analysis_card_headline(analysis);
    let support_line_2 = analysis_card_support_line_2(analysis);
    let analysis_name = analysis.analysis_name.clone();
    let last_run_at = analysis.last_run_at.clone();

    view! {
        <A href="/analysis" attr:class="home-card" attr:style=CARD_STYLE>
            <span class="text-display" style=TITLE_STYLE>"Analysis"</span>
            <span class="text-heading">{headline}</span>
            <span class="text-body" style=CAPTION_STYLE>{analysis_name}</span>
            <span class="text-body" style=CAPTION_STYLE>{support_line_2}</span>
            {last_run_at.map(|at| view! { <Timestamp at=at /> })}
            <span class="text-label" style=OPEN_AFFORDANCE_STYLE>"Open \u{2192}"</span>
        </A>
    }
}

fn graph_card_view(graph: &GraphCardView) -> impl IntoView {
    let headline = graph_card_headline(graph);
    let support_line_1 = graph_card_support_line_1(graph);
    let generated_at = graph.population.generated_at.clone();

    view! {
        <A href="/graph" attr:class="home-card" attr:style=CARD_STYLE>
            <span class="text-display" style=TITLE_STYLE>"Graph"</span>
            <span class="text-heading font-mono">{headline}</span>
            <span class="text-body" style=CAPTION_STYLE>
                {support_line_1}
                " "
                <Timestamp at=generated_at />
            </span>
            <span class="text-label" style=OPEN_AFFORDANCE_STYLE>"Open \u{2192}"</span>
        </A>
    }
}

fn ingestors_card_view(ingestors: &IngestorsCardView) -> impl IntoView {
    let last_event = ingestors_card_line(ingestors);

    view! {
        <div style=INGESTORS_CARD_STYLE>
            <span class="text-display" style=TITLE_STYLE>"Ingestors"</span>
            <span class="text-heading">"Not running"</span>
            <span class="text-body" style=CAPTION_STYLE>"replay-only session"</span>
            {match last_event {
                Some(at) => view! { <Timestamp at=at /> }.into_any(),
                None => view! {
                    <span style="display: flex; align-items: center; gap: var(--space-xs);">
                        <span
                            aria-hidden="true"
                            style="width: 8px; height: 8px; border-radius: 9999px; background: var(--muted-foreground); display: inline-block; flex-shrink: 0;"
                        ></span>
                        <span class="text-label" style=CAPTION_STYLE>"Not running"</span>
                    </span>
                }.into_any(),
            }}
        </div>
    }
}

fn help_card_view() -> impl IntoView {
    let headline = help_card_headline();

    view! {
        <A href="/help" attr:class="home-card" attr:style=CARD_STYLE>
            <span class="text-display" style=TITLE_STYLE>"Help"</span>
            <span class="text-heading">{headline}</span>
            <span class="text-body" style=CAPTION_STYLE>"How to read the report"</span>
            <span class="text-body" style=CAPTION_STYLE>
                "Graph algorithms and "
                <Abbr token=CLI_ABBR.to_string() />
                " pivots"
            </span>
            <span class="text-label" style=OPEN_AFFORDANCE_STYLE>"Open \u{2192}"</span>
        </A>
    }
}

/// The four-card grid, rendered only once [`HomeView`] has resolved (`HomePage`'s `<Suspense>`
/// covers the pending state). `repeat(auto-fit, minmax(260px, 1fr))`, `md` gaps, `lg` page padding
/// (the phase 43 UI spec "Home page" section) — no card is a focal point (D-10).
fn home_grid(home: HomeView) -> impl IntoView {
    view! {
        <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: var(--space-md); padding: var(--space-lg);">
            {analysis_card_view(&home.analysis)}
            {graph_card_view(&home.graph)}
            {ingestors_card_view(&home.ingestors)}
            {help_card_view()}
        </div>
    }
}

/// The workbench's Home page (NAV-01): fetches [`HOME_URL`] on mount via a `LocalResource`. While
/// pending, `<Suspense>`'s fallback shows a loading line, never a blank page. On success, the
/// four-card grid renders. On failure, the exact [`HOME_FETCH_ERROR_COPY`] renders, never partial
/// data. Not yet wired into `app.rs`'s route table — plan 43-09 owns that.
///
/// While `home.graph.snapshot_pending` is `true` (checkpoint fix item 4), re-fetches
/// [`HOME_URL`] every 5 seconds via `set_timeout`/`LocalResource::refetch` until it flips back to
/// `false` — no new dependency, mirrors `graph_page.rs::GraphPage`'s identical polling shape.
#[component]
#[allow(non_snake_case)]
pub fn HomePage() -> impl IntoView {
    let home = LocalResource::new(fetch_home);

    Effect::new(move |_| {
        if let Some(Ok(home_view)) = home.get() {
            if home_view.graph.snapshot_pending {
                set_timeout(move || home.refetch(), Duration::from_secs(5));
            }
        }
    });

    view! {
        <div>
            <style>".home-card:hover { background: var(--card-hover); } .home-card:focus-visible { outline: 2px solid var(--primary); outline-offset: 2px; }"</style>
            <Suspense fallback=move || view! {
                <p class="text-body" style="color: var(--muted-foreground); padding: var(--space-lg);">"Loading home\u{2026}"</p>
            }>
                {move || {
                    home.get().map(|result| match result {
                        Ok(home_view) => home_grid(home_view).into_any(),
                        Err(_) => view! {
                            <p class="text-body" style="color: var(--muted-foreground); padding: var(--space-lg);">
                                {HOME_FETCH_ERROR_COPY}
                            </p>
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
    use urban_integrity_workbench_dto::PopulationMeta;

    fn population(count: Option<u64>, unavailable_reason: Option<&str>) -> PopulationMeta {
        PopulationMeta {
            count,
            generated_at: "2026-01-01T10:30:00Z".to_string(),
            unavailable_reason: unavailable_reason.map(|s| s.to_string()),
            ward_seed_count: 14,
        }
    }

    fn analysis_view(last_run_at: Option<&str>, findings_confirmed: u64) -> AnalysisCardView {
        AnalysisCardView {
            analysis_name: "First analysis".to_string(),
            database: "an_first_1a2b3c".to_string(),
            findings_confirmed,
            probes_could_not_evaluate: 3,
            probes_skipped: 0,
            total_probes: 22,
            last_run_at: last_run_at.map(|s| s.to_string()),
        }
    }

    #[test]
    fn home_url_matches_the_native_home_endpoint() {
        assert_eq!(HOME_URL, "/api/home");
    }

    #[test]
    fn home_fetch_error_copy_contains_the_endpoint_path() {
        assert!(HOME_FETCH_ERROR_COPY.contains("/api/home"));
    }

    #[test]
    fn analysis_card_headline_no_report_yet() {
        let analysis = analysis_view(None, 0);
        assert_eq!(analysis_card_headline(&analysis), "No probe run yet");
    }

    #[test]
    fn analysis_card_headline_with_report() {
        let analysis = analysis_view(Some("2026-01-01T10:33:00Z"), 3);
        assert_eq!(analysis_card_headline(&analysis), "3 findings confirmed");
    }

    #[test]
    fn analysis_card_support_line_2_reports_could_not_evaluate_and_skipped() {
        let mut analysis = analysis_view(Some("2026-01-01T10:33:00Z"), 3);
        analysis.probes_could_not_evaluate = 3;
        analysis.probes_skipped = 0;
        analysis.total_probes = 22;
        assert_eq!(
            analysis_card_support_line_2(&analysis),
            "3 of 22 probes could not evaluate, 0 skipped"
        );
    }

    #[test]
    fn graph_card_headline_population_unavailable() {
        let graph = GraphCardView {
            node_count: 358_484,
            edge_count: 507_179,
            population: population(None, Some("no ward code resolved")),
            graph_as_of: "2026-01-01T10:33:00Z".to_string(),
            snapshot_pending: false,
        };
        assert_eq!(graph_card_headline(&graph), "Population unavailable");
        assert_ne!(graph_card_headline(&graph), "0");
    }

    /// Checkpoint fix item 4: `snapshot_pending: true` takes priority over both the available and
    /// unavailable population states — the headline reads the honest baking state, never a
    /// fabricated `0 nodes · 0 edges`.
    #[test]
    fn graph_card_headline_snapshot_pending_takes_priority_over_population_state() {
        let mut graph = GraphCardView {
            node_count: 0,
            edge_count: 0,
            population: population(Some(358_484), None),
            graph_as_of: "2026-01-01T10:33:00Z".to_string(),
            snapshot_pending: true,
        };
        assert_eq!(
            graph_card_headline(&graph),
            GRAPH_CARD_SNAPSHOT_BAKING_HEADLINE
        );
        assert_ne!(graph_card_headline(&graph), "0 nodes \u{b7} 0 edges");

        graph.population = population(None, Some("no ward code resolved"));
        assert_eq!(
            graph_card_headline(&graph),
            GRAPH_CARD_SNAPSHOT_BAKING_HEADLINE,
            "snapshot_pending must win even when the population is also unavailable"
        );
    }

    #[test]
    fn graph_card_support_line_1_returns_reason_verbatim_when_unavailable() {
        let graph = GraphCardView {
            node_count: 358_484,
            edge_count: 507_179,
            population: population(None, Some("no ward code resolved")),
            graph_as_of: "2026-01-01T10:33:00Z".to_string(),
            snapshot_pending: false,
        };
        assert_eq!(graph_card_support_line_1(&graph), "no ward code resolved");
    }

    #[test]
    fn graph_card_support_line_1_shows_population_count_when_available() {
        let graph = GraphCardView {
            node_count: 358_484,
            edge_count: 507_179,
            population: population(Some(113_250), None),
            graph_as_of: "2026-01-01T10:33:00Z".to_string(),
            snapshot_pending: false,
        };
        assert_eq!(graph_card_support_line_1(&graph), "population 113250");
    }

    #[test]
    fn ingestors_card_line_is_none_when_no_source_has_ever_run() {
        let ingestors = IngestorsCardView {
            companies_house_last_event: None,
            contracts_finder_last_event: None,
            planning_last_event: None,
            overall_last_event: None,
        };
        assert_eq!(ingestors_card_line(&ingestors), None);
    }

    #[test]
    fn ingestors_card_line_returns_overall_last_event_when_present() {
        let ingestors = IngestorsCardView {
            companies_house_last_event: Some("2026-01-01T09:00:00Z".to_string()),
            contracts_finder_last_event: None,
            planning_last_event: None,
            overall_last_event: Some("2026-01-01T09:00:00Z".to_string()),
        };
        assert_eq!(
            ingestors_card_line(&ingestors),
            Some("2026-01-01T09:00:00Z".to_string())
        );
    }

    /// The headline must come from the crate's own glossary parser — there is no server-side
    /// figure to fall back on, and a fabricated `0 terms` must never render.
    #[test]
    fn help_card_headline_uses_the_real_client_side_glossary_count() {
        let headline = help_card_headline();
        assert!(headline.ends_with(" terms"));
        assert_ne!(headline, "0 terms");
        let real_count = crate::glossary::term_count();
        assert_eq!(headline, format!("{real_count} terms"));
    }
}
