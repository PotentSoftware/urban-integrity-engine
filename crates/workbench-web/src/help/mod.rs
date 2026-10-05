//! `HelpPage` — the in-app guide (`/help`, NAV-05, D-13). Four fixed sections in this exact
//! order: Reading the report, Verdict vocabulary (reuses `VerdictLegend` verbatim), Graph
//! algorithms, CLI pivots. Section headings are Display size and every `<p>`/`<h2>`/`<h3>` zeroes
//! its UA margin (spacing comes from the flex gaps alone) since 2026-09-06, when the operator
//! found the Heading-size headings too small and the default-margin-plus-gap rhythm too airy. See the phase 43 UI spec's "Help page" section for the section order and
//! copy contract this module implements.

pub mod cli_cheatsheet;
pub mod glossary_page;

pub mod algorithms;

use leptos::prelude::*;
use urban_integrity_workbench_dto::AnalysisSummaryView;

use crate::abbr::Abbr;
use crate::analysis_picker::current_entry;
use crate::breadcrumb::Breadcrumb;
use crate::verdict_badge::VerdictLegend;
use cli_cheatsheet::cheat_sheet_commands;

/// `GET /api/analyses` — the native endpoint plan 42-05 built, re-fetched here (a second,
/// independent `LocalResource`) so the CLI pivots section can read the current analysis's
/// `db_name`. Mirrors `AnalysisPicker`'s exact fetch shape; no shared cache exists in this crate
/// today and adding one is out of scope for this plan.
async fn fetch_analyses_for_cheatsheet() -> Result<Vec<AnalysisSummaryView>, String> {
    let response = gloo_net::http::Request::get(crate::analysis_picker::ANALYSES_URL)
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

/// One paragraph of Section 1 ("Reading the report") body prose. Pulled into a plain constant so
/// the exact copy is a single, greppable source of truth rather than embedded inline in the
/// `view!` tree.
const READING_THE_REPORT_PARAGRAPHS: [&str; 5] = [
    "The honesty line comes first because a probe battery is only useful if you can tell what it \
     actually checked. Before any verdict, the report states how many of the 22 probes could \
     evaluate at all — a probe that could not evaluate produced no finding either way, so it \
     cannot be read as evidence of a clean bill of health.",
    "\"Could not evaluate\" means a probe hit a data gap or was blocked: a required input source \
     was missing, or the population it needed could not be computed. This is different from a \
     probe running and finding nothing — it means the probe never ran to completion.",
    "\"Skipped by gating\" means a probe's own preconditions were not met for this analysis (for \
     example, a probe that only applies when a certain entity type or edge exists) — it was \
     deliberately not attempted, not blocked by a missing input.",
    "\"Structurally capable of producing a finding\" describes the subset of the population a \
     probe could actually reason about, once records with missing or unusable fields are \
     excluded. A contradiction check compares two independent readings of the same fact (for \
     example, two different sources describing the same entity) and reports whether they agree; \
     a failing contradiction check is shown verbatim, never paraphrased.",
    "Every verdict carries a population figure — the number of records the probe examined — and \
     a pill saying where that figure came from. \"Self-reported\" means the probe counted its own \
     population and nothing checked the number: the verdict is internally consistent, but a \
     wrong count would not show. \"Oracle-verified\" means an independent path — the population \
     oracle, which recounts the same population from the event log by a different route from \
     the probe — arrived at the same figure; the number in brackets is the oracle's count. Only \
     an oracle-verified Clear is shown in green, because a Clear reached over an unchecked \
     population may simply be a Clear over the wrong population.",
];

/// One paragraph per graph algorithm for Section 3 ("Graph algorithms"), in this fixed order:
/// community detection, k-core, betweenness centrality, all simple paths, shortest/k-shortest
/// paths, ward-reachable subgraph. These algorithms are not built until Phase 44 — this section
/// documents what each one DOES, which is not "coming soon" text (D-01).
const GRAPH_ALGORITHM_PARAGRAPHS: [(&str, &str); 6] = [
    (
        "Community detection",
        "Groups entities into clusters that are more densely connected to each other than to the \
         rest of the graph. A resolution parameter controls cluster granularity — higher values \
         produce more, smaller clusters. A truncated result means the algorithm stopped before \
         convergence on a very large graph; the clusters shown are still valid, just possibly \
         coarser than a full run would produce.",
    ),
    (
        "K-core",
        "Finds the maximal subgraph in which every entity has at least k connections to other \
         entities within that same subgraph — a way of finding a graph's densely-interconnected \
         core, filtering out peripheral, loosely-connected entities. A higher k narrows the core. \
         A truncated result means the core at the requested k was larger than the display limit; \
         only the highest-degree portion is shown.",
    ),
    (
        "Betweenness centrality",
        "Scores each entity by how often it lies on the shortest path between two other entities \
         — a high score marks a bridge or broker position in the network, not necessarily a \
         high-degree hub. A truncated result means only the top-scoring entities are shown, not \
         every entity's score.",
    ),
    (
        "All simple paths",
        "Enumerates every path between two chosen entities that visits no entity twice. This can \
         grow combinatorially on a dense graph, so a maximum path length or path count parameter \
         bounds the search. A truncated result means the search hit that bound before exhausting \
         every possible path — the paths shown are real, but not exhaustive.",
    ),
    (
        "Shortest / k-shortest paths",
        "Finds the single shortest path (or the k shortest, distinct paths) between two chosen \
         entities, by number of edges traversed. A truncated result for k-shortest means fewer \
         than k distinct paths exist between the two entities, not that the search failed.",
    ),
    (
        "Ward-reachable subgraph",
        "Given a starting ward, returns every entity reachable from it within a chosen number of \
         hops — a way of scoping the graph to a local neighbourhood rather than viewing the whole \
         corpus. A truncated result means the reachable set exceeded the display limit at that \
         hop count; narrowing the hop count reduces it.",
    ),
];

/// D-03's first-occurrence rule for headings: the full words come first, then the abbreviation
/// in parentheses, and the abbreviation itself is an [`Abbr`] so hover/focus/tap shows the
/// glossary expansion. The words are the glossary's own meaning for [`CLI_ABBR`]
/// (`docs/abbreviations.md`), not a second, drifting copy.
pub const CLI_PIVOTS_HEADING_PREFIX: &str = "Command-Line Interface";

/// The glossary token the "CLI pivots" heading wraps in an [`Abbr`].
pub const CLI_ABBR: &str = "CLI";

/// D-18's Help-page licence line, appended as the closing paragraph of the "Graph algorithms"
/// section (the phase 44 UI spec "Help page licence line") — the first place this phase states
/// Cosmograph's licence to an analyst, matching `crates/workbench-web/js/README.md`'s "Licence"
/// section's facts (package, pinned version, CC-BY-NC-4.0, non-commercial-acceptable) verbatim
/// where the two overlap, even though the surrounding prose differs (one is developer
/// documentation, this is analyst-facing copy).
///
/// Glossary-token caveat (read the phase 44 UI spec's "Help page licence line" section in full before
/// editing this constant): `CC-BY-NC-4.0` splits into a bare `CC` token under
/// `abbr_reachable.rs`'s `contains_bare_token` non-alphanumeric-boundary split, colliding with the
/// glossary's unrelated `CC` = Charity Commission row — wrapping `CC` in `<Abbr>` here would be
/// WRONG (a "Charity Commission" tooltip on a licence code). This constant is declared as a
/// module-level `pub const` split across two lines by a backslash line-continuation, mirroring
/// `READING_THE_REPORT_PARAGRAPHS`'s existing multi-line-literal shape in this same file: neither
/// line, read in isolation by `abbr_reachable.rs`'s per-line `string_literals` scanner, closes a
/// quote it opens (the first line's string starts but is not terminated on that line; the second
/// line's lone closing quote has no matching opener on that line either), so this definition is
/// never extracted as a scannable copy literal by that sweep — the SAME mechanism that already
/// keeps every other multi-line paragraph constant in this file out of the sweep's reach. This is
/// the fix mechanism this plan chooses (see this plan's Summary for the full "why" and the
/// alternative this file's own `<action>` text left open but did not take).
pub const COSMOGRAPH_LICENCE_LINE: &str =
    "Graph rendering uses @cosmograph/cosmograph 2.5.1, licensed CC-BY-NC-4.0 (non-commercial) — \
     acceptable for this local, single-operator research tool with no commercial use.";

/// `#[component] HelpPage`: renders the breadcrumb and the four fixed Help sections in order
/// (D-13). No separate `<h1>"Help"</h1>` — the breadcrumb's final segment already names the page
/// at Heading size, and the operator asked for the duplicate to go (2026-09-06).
#[component]
#[allow(non_snake_case)]
pub fn HelpPage() -> impl IntoView {
    let analyses = LocalResource::new(fetch_analyses_for_cheatsheet);

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-xl); padding-top: var(--space-md);">
            <Breadcrumb />

            <section style="display: flex; flex-direction: column; gap: var(--space-sm);">
                <h2 class="text-display" style="margin: 0;">"Reading the report"</h2>
                {READING_THE_REPORT_PARAGRAPHS
                    .iter()
                    .map(|paragraph| view! {
                        <p class="text-body" style="margin: 0;">{*paragraph}</p>
                    })
                    .collect_view()}
            </section>

            <section style="display: flex; flex-direction: column; gap: var(--space-sm);">
                <h2 class="text-display" style="margin: 0;">"Verdict vocabulary"</h2>
                <VerdictLegend />
            </section>

            <section style="display: flex; flex-direction: column; gap: var(--space-sm);">
                <h2 class="text-display" style="margin: 0;">"Graph algorithms"</h2>
                {GRAPH_ALGORITHM_PARAGRAPHS
                    .iter()
                    .map(|(name, paragraph)| view! {
                        <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                            <h3 class="text-heading" style="margin: 0;">{*name}</h3>
                            <p class="text-body" style="margin: 0;">{*paragraph}</p>
                        </div>
                    })
                    .collect_view()}
                <p class="text-body" style="margin: 0;">{COSMOGRAPH_LICENCE_LINE}</p>
                <a class="text-body" href="/help/algorithms">"Per-algorithm help with worked examples"</a>
            </section>

            <section style="display: flex; flex-direction: column; gap: var(--space-sm);">
                <h2 class="text-display" style="margin: 0;">
                    {CLI_PIVOTS_HEADING_PREFIX}
                    " ("
                    <Abbr token=CLI_ABBR.to_string() />
                    ") pivots"
                </h2>
                <Suspense fallback=move || view! {
                    <div class="text-body" style="color: var(--muted-foreground);">
                        "Loading analyses…"
                    </div>
                }>
                    {move || {
                        analyses.get().map(|result| match result {
                            Ok(list) => {
                                let db_name = current_entry(&list)
                                    .map(|entry| entry.db_name.clone());
                                match db_name {
                                    Some(db_name) => {
                                        let commands = cheat_sheet_commands(&db_name);
                                        view! {
                                            <div
                                                class="font-mono text-body"
                                                style="display: flex; flex-direction: column; gap: var(--space-xs); background: var(--card); padding: var(--space-sm); border-radius: 8px;"
                                            >
                                                {commands.into_iter().map(|command| view! {
                                                    <div>{command}</div>
                                                }).collect_view()}
                                            </div>
                                        }.into_any()
                                    }
                                    None => view! {
                                        <div class="text-body" style="color: var(--muted-foreground);">
                                            "No current analysis."
                                        </div>
                                    }.into_any(),
                                }
                            }
                            Err(_) => view! {
                                <div class="text-body" style="color: var(--muted-foreground);">
                                    "Could not load analyses."
                                </div>
                            }.into_any(),
                        })
                    }}
                </Suspense>
            </section>
        </div>
    }
}
