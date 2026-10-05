//! `StatusLine` — the Graph page's mandatory status line (the phase 44 UI spec "Status line," D-02, D-08,
//! D-09, D-11, D-14): a grammar composer built entirely from `AlgorithmResult`'s structured fields
//! (never a server-sent prose string — plan 44-01's own design), plus the Re-layout/Fit/Reset
//! controls.
//!
//! # Testing approach (plain-function-first, per `tool_rail.rs`'s documented convention)
//!
//! Every grammar clause is its own independently-testable pure function
//! (`edge_exclusion_clause`, `ward_subgraph_edge_clause`, `seed_clause`, `path_noun`, one
//! `*_summary` function per algorithm), composed by [`status_line_text`] — every worked example in
//! the phase 44 UI spec's Status line table is reproduced VERBATIM by a dedicated test (T-44-13-01's
//! mitigation: a transcription error fails the test, not a later analyst reading the wrong thing).
//!
//! Module-wide `#![allow(dead_code)]`: this file lands before `graph_page.rs` mounts
//! `<StatusLine>` in this plan's Task 3 — mirrors `graph_results_list.rs`'s own documented
//! convention.
#![allow(dead_code)]

use leptos::prelude::*;
use urban_integrity_workbench_dto::canvas_filter::CanvasFilterView;
use urban_integrity_workbench_dto::neighbourhood::NeighbourhoodView;
use urban_integrity_workbench_dto::{AlgorithmOutcome, AlgorithmResult, EntityRef, PathResult};

use crate::graph_results_list::RESULTS_LIST_EMPTY_COPY;

/// D-08's hard wall-clock ceiling (30s), enforced server-side for every algorithm's outer
/// `spawn_blocking` timeout regardless of whether that algorithm exposes an adjustable "budget"
/// field of its own (community/k-core have no budget input in their rail parameter form — see
/// `tool_rail.rs`'s `parameter_form` — so `AlgorithmCaps.budget_secs` is `None` for those two
/// per `dispatch.rs::caps_used_for`; this constant is the honest fallback number to display in
/// their truncated-result wording, since 30s IS the real ceiling that would have applied even
/// though no per-request value flows through the cap struct for them today).
pub const HARD_BUDGET_CEILING_SECS: u32 = 30;

// ─── Automatic canvas effect classification (plan Task 3 wiring) ──────────────────────────────

/// Which CATEGORY of automatic, run-completion canvas effect applies to an outcome
/// (the phase 44 UI spec's "Automatic (run-completion) canvas effects" table) — kept co-located with
/// [`betweenness_resize_scale`] below since both feed `graph_page.rs`'s same wiring `Effect`
/// (this plan's Task 3 action text explicitly permits placing both here rather than in
/// `graph_page.rs` itself). This enum carries no payload: `graph_page.rs`'s wiring extracts the
/// actual ids/hues/scores directly from the outcome once it knows which category applies, since
/// `KCore` and `WardSubgraph` both classify as [`CanvasEffect::Hide`] but compute their hidden-id
/// sets differently (`KCore.hidden_node_ids` directly vs. population-minus-`WardSubgraph.node_ids`
/// — the latter needs the full population id list, which this outcome-only function does not
/// have access to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasEffect {
    /// Community detection: recolour by community, hide below-minimum communities.
    Recolor,
    /// k-core / ward-reachable subgraph: hide every node outside the surviving set.
    Hide,
    /// Betweenness centrality: resize every node by its score.
    Resize,
    /// All simple paths / shortest / k-shortest paths: no automatic effect.
    None,
}

/// Maps each of the six `AlgorithmOutcome` variants to its documented automatic canvas effect
/// category — the transcription-accuracy proof for the phase 44 UI spec's automatic-effects table
/// (T-44-13-01's mitigation).
pub fn automatic_effect_for(outcome: &AlgorithmOutcome) -> CanvasEffect {
    match outcome {
        AlgorithmOutcome::Community { .. } => CanvasEffect::Recolor,
        AlgorithmOutcome::KCore { .. } => CanvasEffect::Hide,
        AlgorithmOutcome::Betweenness { .. } => CanvasEffect::Resize,
        AlgorithmOutcome::WardSubgraph { .. } => CanvasEffect::Hide,
        AlgorithmOutcome::HighlightSet { .. } => CanvasEffect::Hide,
        AlgorithmOutcome::Paths { .. } | AlgorithmOutcome::ShortestPaths { .. } => {
            CanvasEffect::None
        }
    }
}

/// Betweenness's automatic resize scale (the phase 44 UI spec: "linear scale, 1×–3× the base radius; a
/// score of 0 stays at 1× — never invisible"). `score <= 0.0` or a non-positive `max_score`
/// (defensive — every real score is a non-negative betweenness value) both return exactly `1.0`;
/// otherwise linear from `1.0` (at `score == 0`) to `3.0` (at `score == max_score`), clamped so a
/// score that (defensively) exceeds `max_score` never scales past `3.0`.
pub fn betweenness_resize_scale(score: f64, max_score: f64) -> f64 {
    if score <= 0.0 || max_score <= 0.0 {
        return 1.0;
    }
    1.0 + 2.0 * (score / max_score).min(1.0)
}

// ─── Number formatting ──────────────────────────────────────────────────────────────────────────

/// Formats `n` with comma thousands separators (`41022` -> `"41,022"`) — every worked example in
/// the phase 44 UI spec's Status line table uses this formatting for counts, so the grammar composer
/// must too (unlike the results list, which never claimed comma formatting).
///
/// `pub(crate)` since plan 45-13: the neighbourhood expansion's status line is the same status
/// line, and a second grouping function beside this one is how the same count would come to be
/// punctuated two ways on one strip of screen.
pub(crate) fn format_thousands(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, byte) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
}

fn entity_display(entity: &EntityRef) -> String {
    format!("{} ({})", entity.name, entity.id)
}

// ─── Shared grammar clauses ─────────────────────────────────────────────────────────────────────

/// The edge-exclusion clause (D-02), always present: exactly `"Using all edge types."` when
/// `excluded` is empty, else `"Ignoring {comma-separated list}."`.
pub fn edge_exclusion_clause(excluded: &[String]) -> String {
    if excluded.is_empty() {
        "Using all edge types.".to_string()
    } else {
        format!("Ignoring {}.", excluded.join(", "))
    }
}

/// Ward-reachable subgraph's own edge-exclusion clause (D-01's forced-`Represents` deviation from
/// the two-form rule above) — `excluded_besides_represents` is `AlgorithmResult.excluded_edge_types`
/// directly: because `Represents` is always forced included for this algorithm, it can never
/// legitimately appear in that list, so no filtering is needed before calling this.
pub fn ward_subgraph_edge_clause(excluded_besides_represents: &[String]) -> String {
    if excluded_besides_represents.is_empty() {
        "Always includes Represents for this algorithm; using all other edge types.".to_string()
    } else {
        format!(
            "Always includes Represents for this algorithm; ignoring {}.",
            excluded_besides_represents.join(", ")
        )
    }
}

/// The seed clause (D-10), only for Louvain (community)/betweenness — `None` when `seed` is
/// `None` (never an empty-string sentence).
pub fn seed_clause(seed: Option<u64>) -> Option<String> {
    seed.map(|s| format!("Seed {s}."))
}

/// The singular/plural noun for a path count, per the phase 44 UI spec's pluralisation rule paragraph.
/// **Governed by `shown` alone** — the UI-SPEC's own worked "k = 1 with no path found" example
/// (`shown = 0`, `requested = Some(1)`) renders **"0 of 1 requested paths"** (plural), which
/// disambiguates the rule's own prose ("...or exactly one requested...") in favour of the
/// concrete worked example: singular only when `shown == 1`, plural in every other case
/// including zero, REGARDLESS of what was requested. `requested` is accepted (matching this
/// task's own specified signature) but does not affect the return value — kept for call-site
/// symmetry with the two paths algorithms' summary functions, both of which pass their own
/// `requested_k`/`None` through here.
pub fn path_noun(shown: u64, requested: Option<u64>) -> &'static str {
    let _ = requested;
    if shown == 1 {
        "path"
    } else {
        "paths"
    }
}

// ─── Per-algorithm result-summary functions ────────────────────────────────────────────────────

/// Community detection's result-summary clause (the phase 44 UI spec worked examples, Normal/Truncated).
pub fn community_summary(
    shown: usize,
    hidden_count: u64,
    truncated: bool,
    budget_secs: u32,
) -> String {
    if truncated {
        format!(
            "Showing {} communities, stopped before full convergence within the {budget_secs} s budget.",
            format_thousands(shown as u64)
        )
    } else {
        format!(
            "Showing {} communities, {} nodes hidden below minimum size.",
            format_thousands(shown as u64),
            format_thousands(hidden_count)
        )
    }
}

/// k-core's result-summary clause.
pub fn k_core_status_summary(
    k: u32,
    node_count: u64,
    edge_count: u64,
    truncated: bool,
    budget_secs: u32,
) -> String {
    if truncated {
        format!("Showing the {k}-core: computation stopped at the {budget_secs} s budget, partial result.")
    } else {
        format!(
            "Showing the {k}-core: {} nodes, {} edges.",
            format_thousands(node_count),
            format_thousands(edge_count)
        )
    }
}

/// Betweenness centrality's result-summary clause — the sampling figure is "the standing
/// truncation" (the phase 44 UI spec's own note): always present, never a separately-omitted clause.
pub fn betweenness_summary(shown: usize, sampled_sources: u64, total_sources: u64) -> String {
    format!(
        "Showing top {} of {} nodes by approximate betweenness — {} of {} sources sampled.",
        format_thousands(shown as u64),
        format_thousands(total_sources),
        format_thousands(sampled_sources),
        format_thousands(total_sources)
    )
}

/// All simple paths' result-summary clause.
#[allow(clippy::too_many_arguments)]
pub fn all_simple_paths_summary(
    shown: usize,
    hop_cap: u32,
    from: &EntityRef,
    to: &EntityRef,
    truncated: bool,
    result_cap: u32,
    wall_clock_ms: u64,
    budget_secs: u32,
) -> String {
    if truncated {
        format!(
            "{} of {} result cap reached, {:.1} s of {budget_secs} s budget used, between {} and {}.",
            format_thousands(shown as u64),
            format_thousands(result_cap as u64),
            wall_clock_ms as f64 / 1000.0,
            entity_display(from),
            entity_display(to)
        )
    } else {
        format!(
            "Showing {} {} up to {hop_cap} hops between {} and {}.",
            format_thousands(shown as u64),
            path_noun(shown as u64, None),
            entity_display(from),
            entity_display(to)
        )
    }
}

/// Shortest/k-shortest paths' result-summary clause.
///
/// `shown < requested_k` and `Truncation.truncated` are DIFFERENT conditions and both matter. A
/// completed run that simply found fewer than `k` distinct paths is not "truncated" in the DTO
/// sense — but a run whose hop cap fired has not exhausted anything, and must not be worded as
/// though it had.
///
/// Operator QA 2026-09-12 found the original wording asserting universal negatives from a bounded
/// search: `Planted Council` to `Planted Disqualified Officer` renders "no path exists" at hop cap
/// 3 and "only 7 distinct paths exist" at hop cap 4, while the same query finds 7 paths at 4 hops
/// and 98 at 6. The server reported `truncated: true, reason: "hop_cap"` in both cases and this
/// composer discarded it. Every claim of exhaustiveness below is now conditioned on the search
/// actually having been exhaustive.
///
/// `edge_types_excluded` guards the SECOND way this wording overclaimed, found in the same QA
/// session and measured through the HTTP path: `GB-COH:PL000099` to `ward:X05999001` returns **0**
/// paths with the three hub edge types unticked and **3** with all eleven included, and
/// `truncation.truncated` is `false` in both — the search really was exhaustive, so the truncation
/// guard above correctly stays silent, and the analyst still reads "no path exists" about a pair
/// with three paths between them. An exhaustive search over a FILTERED edge set establishes
/// nothing about the unfiltered graph, so neither exhaustive branch may speak of the graph as a
/// whole while any edge type is excluded. The trailing `edge_exclusion_clause` names which ones;
/// this clause's job is not to contradict it.
#[allow(clippy::too_many_arguments)]
pub fn shortest_paths_summary(
    shown: usize,
    requested_k: u32,
    from: &EntityRef,
    to: &EntityRef,
    truncated: bool,
    truncation_reason: Option<&str>,
    hop_cap: u32,
    edge_types_excluded: bool,
) -> String {
    let noun = path_noun(shown as u64, Some(requested_k as u64));
    if shown as u32 >= requested_k {
        format!(
            "Showing {shown} of {requested_k} shortest {noun} between {} and {}.",
            entity_display(from),
            entity_display(to)
        )
    } else if shown == 0 {
        match (truncated, truncation_reason) {
            (true, Some("hop_cap")) => format!(
                "Showing 0 of {requested_k} requested {noun} — no path within {hop_cap} hops between {} and {}; the search stopped at the hop cap, so longer paths may exist.",
                entity_display(from),
                entity_display(to)
            ),
            (true, _) => format!(
                "Showing 0 of {requested_k} requested {noun} — none found between {} and {} before the search stopped at a cap, so paths may exist beyond it.",
                entity_display(from),
                entity_display(to)
            ),
            (false, _) if edge_types_excluded => format!(
                "Showing {shown} of {requested_k} requested {noun} — no path between {} and {} with the current edge-type selection.",
                entity_display(from),
                entity_display(to)
            ),
            (false, _) => format!(
                "Showing {shown} of {requested_k} requested {noun} — no path exists between {} and {}.",
                entity_display(from),
                entity_display(to)
            ),
        }
    } else if truncated {
        match truncation_reason {
            Some("hop_cap") => format!(
                "Showing {shown} of {requested_k} requested {noun} — {shown} found within {hop_cap} hops between {} and {}; the search stopped at the hop cap, so more may exist.",
                entity_display(from),
                entity_display(to)
            ),
            _ => format!(
                "Showing {shown} of {requested_k} requested {noun} between {} and {}; the search stopped at a cap, so more may exist.",
                entity_display(from),
                entity_display(to)
            ),
        }
    } else if edge_types_excluded {
        format!(
            "Showing {shown} of {requested_k} requested {noun} — only {shown} distinct {noun} between {} and {} with the current edge-type selection.",
            entity_display(from),
            entity_display(to)
        )
    } else {
        format!(
            "Showing {shown} of {requested_k} requested {noun} — only {shown} distinct {noun} exist between {} and {}.",
            entity_display(from),
            entity_display(to)
        )
    }
}

/// Ward-reachable subgraph's result-summary clause. The truncated form's "estimated {N}+" figure
/// is `caps_used.result_cap` (the cap that fired) — in a real run this equals `node_count` exactly
/// (the walk stops precisely at the cap), so it degrades to "N of an estimated N+" honestly; this
/// function itself supports two independent numbers for whenever a future backend change can
/// supply a genuine estimate larger than the capped `node_count`.
pub fn ward_subgraph_status_summary(
    node_count: u64,
    edge_count: u64,
    hops: u32,
    ward: &EntityRef,
    truncated: bool,
    result_cap: u32,
) -> String {
    if truncated {
        format!(
            "Showing {} of an estimated {}+ reachable nodes — result cap reached.",
            format_thousands(node_count),
            format_thousands(result_cap as u64)
        )
    } else {
        format!(
            "Showing {} nodes and {} edges reachable from {} within {hops} hops.",
            format_thousands(node_count),
            format_thousands(edge_count),
            entity_display(ward)
        )
    }
}

// ─── Composition ────────────────────────────────────────────────────────────────────────────────

/// A defensive fallback `EntityRef` for the (never-expected-in-practice) case where neither the
/// caller-supplied rail state nor any path result carries a From/To entity — `run_enabled`
/// (`tool_rail.rs`) already gates Run on both being resolved before a paths request can be sent,
/// so this path should be unreachable against a real backend response.
fn unknown_entity() -> EntityRef {
    EntityRef {
        id: String::new(),
        name: "Unknown".to_string(),
        kind: "Entity".to_string(),
    }
}

/// Resolves the From/To entities a paths-family status line names. Prefers the caller-supplied
/// rail state (`from_ref`/`to_ref` — always correct, including for a genuinely empty `paths` list,
/// e.g. the "no path exists" case, where `AlgorithmOutcome::Paths`/`ShortestPaths` itself carries
/// no From/To field at all per `algorithm.rs`'s DTO shape); falls back to the first path's own hop
/// endpoints if the caller did not supply them (defensive only).
fn paths_endpoints(
    paths: &[PathResult],
    from_ref: Option<&EntityRef>,
    to_ref: Option<&EntityRef>,
) -> (EntityRef, EntityRef) {
    let from = from_ref.cloned().unwrap_or_else(|| {
        paths
            .first()
            .and_then(|p| p.hops.first())
            .map(|hop| hop.entity.clone())
            .unwrap_or_else(unknown_entity)
    });
    let to = to_ref.cloned().unwrap_or_else(|| {
        paths
            .first()
            .and_then(|p| p.hops.last())
            .map(|hop| hop.entity.clone())
            .unwrap_or_else(unknown_entity)
    });
    (from, to)
}

/// Composes the full status-line sentence for `result`, per the phase 44 UI spec's grammar template:
/// `{result summary}. {truncation clause, only if truncated}. {edge-exclusion clause, always
/// present}. {seed clause, only for Louvain/betweenness}.` — for every algorithm here, the
/// truncation-dependent wording is folded directly into that algorithm's own `*_summary` function
/// (the worked examples in the table show ONE sentence whose wording changes shape when truncated,
/// never two independently-joined clauses — reproducing the table's exact punctuation, e.g.
/// community's comma-joined "...communities, stopped before..." rather than a second
/// period-separated sentence, is what "reproduce these VERBATIM" requires).
///
/// `from_ref`/`to_ref` are the rail's own resolved picker state (`tool_rail.rs`'s
/// `ToolRailSignals.from_ref`/`to_ref`, read live by the caller) — `AlgorithmOutcome::Paths`/
/// `ShortestPaths` carry no From/To field of their own (`algorithm.rs`'s DTO shape), so this is
/// the only source that can name the queried entities for a genuinely empty (zero-path) result.
/// Every other outcome ignores both parameters.
pub fn status_line_text(
    result: &AlgorithmResult,
    from_ref: Option<&EntityRef>,
    to_ref: Option<&EntityRef>,
) -> String {
    let budget_secs = result
        .caps_used
        .budget_secs
        .unwrap_or(HARD_BUDGET_CEILING_SECS);

    let summary = match &result.outcome {
        AlgorithmOutcome::Community {
            communities,
            hidden_node_ids,
        } => community_summary(
            communities.len(),
            hidden_node_ids.len() as u64,
            result.truncation.truncated,
            budget_secs,
        ),
        AlgorithmOutcome::KCore {
            k,
            node_count,
            edge_count,
            ..
        } => k_core_status_summary(
            *k,
            *node_count,
            *edge_count,
            result.truncation.truncated,
            budget_secs,
        ),
        AlgorithmOutcome::Betweenness {
            sampled_sources,
            total_sources,
            top_nodes,
        } => betweenness_summary(top_nodes.len(), *sampled_sources, *total_sources),
        AlgorithmOutcome::Paths { paths } => {
            let (from, to) = paths_endpoints(paths, from_ref, to_ref);
            all_simple_paths_summary(
                paths.len(),
                result.caps_used.hop_cap.unwrap_or(0),
                &from,
                &to,
                result.truncation.truncated,
                result.caps_used.result_cap.unwrap_or(0),
                result.wall_clock_ms,
                budget_secs,
            )
        }
        AlgorithmOutcome::ShortestPaths { paths, requested_k } => {
            let (from, to) = paths_endpoints(paths, from_ref, to_ref);
            shortest_paths_summary(
                paths.len(),
                *requested_k,
                &from,
                &to,
                result.truncation.truncated,
                result.truncation.reason.as_deref(),
                result.caps_used.hop_cap.unwrap_or(0),
                !result.excluded_edge_types.is_empty(),
            )
        }
        AlgorithmOutcome::WardSubgraph {
            ward,
            hops,
            node_count,
            edge_count,
            ..
        } => ward_subgraph_status_summary(
            *node_count,
            *edge_count,
            *hops,
            ward,
            result.truncation.truncated,
            result.caps_used.result_cap.unwrap_or(*node_count as u32),
        ),
        AlgorithmOutcome::HighlightSet {
            node_ids,
            unresolved_count,
        } => format!(
            "Showing {} named {}; {} outside the population.",
            format_thousands(node_ids.len() as u64),
            if node_ids.len() == 1 {
                "entity"
            } else {
                "entities"
            },
            format_thousands(*unresolved_count)
        ),
    };

    let edge_clause = if matches!(result.outcome, AlgorithmOutcome::WardSubgraph { .. }) {
        ward_subgraph_edge_clause(&result.excluded_edge_types)
    } else {
        edge_exclusion_clause(&result.excluded_edge_types)
    };

    let mut sentences = vec![summary, edge_clause];
    if let Some(seed_sentence) = seed_clause(result.seed_used) {
        sentences.push(seed_sentence);
    }
    sentences.join(" ")
}

// ─── Component ──────────────────────────────────────────────────────────────────────────────────

const CONTROL_STYLE: &str = "background: none; border: none; color: var(--primary); cursor: pointer; min-height: 44px; min-width: 44px; padding: 0 var(--space-sm);";

/// The Graph page's status line (the phase 44 UI spec "Status line"): 48px, docked beneath the canvas,
/// left-aligned [`status_line_text`] (or [`RESULTS_LIST_EMPTY_COPY`] when no result is active —
/// the status line is always present once the canvas has mounted, never conditionally hidden),
/// right-aligned Re-layout/Fit/Reset controls. Reset is disabled/greyed when no result is active;
/// Re-layout/Fit are always enabled (they act on the canvas regardless of algorithm state).
#[component]
#[allow(non_snake_case)]
pub fn StatusLine(
    result: Signal<Option<AlgorithmResult>>,
    from_ref: Signal<Option<EntityRef>>,
    to_ref: Signal<Option<EntityRef>>,
    /// The active neighbourhood expansion (plan 45-13, CNV-01/CNV-02), or `None`.
    ///
    /// Takes precedence over the algorithm result when present, because it describes what is
    /// actually on the canvas: an expansion narrows the canvas through the same opacity/label
    /// channel a run does, so `graph_page.rs` clears one when the other arrives and the two are
    /// never both live. D-12's line, not a second one — the cap is reported through the same
    /// mandatory `Truncation` block and the same strip of screen.
    expansion: Signal<Option<NeighbourhoodView>>,
    /// The active canvas filter (plan 45-14, CNV-03), or `None`.
    ///
    /// Appended to whichever sentence already describes the canvas rather than replacing it: a
    /// filter and an expansion **compose** — the filter runs over the expansion's focus set — so
    /// the line has to state both or it will describe half of what is on screen.
    /// `canvas_filters::filter_status_clause` returns `None` when no filter is narrowing anything,
    /// for 45-12's reason: an unfiltered screen states no rule about a filter nobody ran.
    filter: Signal<Option<CanvasFilterView>>,
    on_relayout: impl Fn() + 'static,
    on_fit: impl Fn() + 'static,
    on_reset: impl Fn() + 'static,
) -> impl IntoView {
    view! {
        <div style="height: 48px; display: flex; align-items: center; justify-content: space-between; gap: var(--space-md); padding: 0 var(--space-md); border-top: 1px solid var(--border); background: var(--card);">
            <span class="text-body" style="white-space: nowrap; overflow: hidden; text-overflow: ellipsis;">
                {move || {
                    let head = match (expansion.get(), result.get()) {
                        (Some(view), _) => crate::neighbourhood::expansion_status_line(&view),
                        (None, Some(algorithm_result)) => {
                            let from = from_ref.get();
                            let to = to_ref.get();
                            status_line_text(&algorithm_result, from.as_ref(), to.as_ref())
                        }
                        (None, None) => RESULTS_LIST_EMPTY_COPY.to_string(),
                    };
                    match crate::canvas_filters::filter_status_clause(filter.get().as_ref()) {
                        Some(clause) => format!("{head} {clause}"),
                        None => head,
                    }
                }}
            </span>
            <div style="display: flex; gap: var(--space-xs); flex-shrink: 0;">
                <button class="text-label" style=CONTROL_STYLE on:click=move |_| on_relayout()>"Re-layout"</button>
                <button class="text-label" style=CONTROL_STYLE on:click=move |_| on_fit()>"Fit"</button>
                <button
                    class="text-label"
                    style=move || {
                        if result.get().is_none() {
                            format!("{CONTROL_STYLE} opacity: 0.4; cursor: default;")
                        } else {
                            CONTROL_STYLE.to_string()
                        }
                    }
                    disabled=move || result.get().is_none()
                    on:click=move |_| on_reset()
                >
                    "Reset"
                </button>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::{AlgorithmCaps, CommunityResult, RankedEntity, Truncation};

    fn entity(id: &str, name: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Entity".to_string(),
        }
    }

    fn empty_caps() -> AlgorithmCaps {
        AlgorithmCaps {
            hop_cap: None,
            result_cap: None,
            budget_secs: None,
            k: None,
            top_n: None,
            min_community_size: None,
        }
    }

    fn no_truncation() -> Truncation {
        Truncation {
            truncated: false,
            reason: None,
            detail: String::new(),
        }
    }

    fn truncated(reason: &str, detail: &str) -> Truncation {
        Truncation {
            truncated: true,
            reason: Some(reason.to_string()),
            detail: detail.to_string(),
        }
    }

    // ─── format_thousands ────────────────────────────────────────────────────────────────────

    #[test]
    fn format_thousands_matches_every_worked_example_figure() {
        assert_eq!(format_thousands(41_022), "41,022");
        assert_eq!(format_thousands(113_384), "113,384");
        assert_eq!(format_thousands(1_204), "1,204");
        assert_eq!(format_thousands(3_880), "3,880");
        assert_eq!(format_thousands(1_842), "1,842");
        assert_eq!(format_thousands(3_110), "3,110");
        assert_eq!(format_thousands(500), "500");
        assert_eq!(format_thousands(37), "37");
        assert_eq!(format_thousands(0), "0");
    }

    // ─── edge_exclusion_clause / ward_subgraph_edge_clause ─────────────────────────────────

    #[test]
    fn edge_exclusion_clause_both_forms() {
        assert_eq!(edge_exclusion_clause(&[]), "Using all edge types.");
        assert_eq!(
            edge_exclusion_clause(&[
                "Represents".to_string(),
                "IssuedBy".to_string(),
                "LocatedAt".to_string(),
            ]),
            "Ignoring Represents, IssuedBy, LocatedAt."
        );
    }

    #[test]
    fn ward_subgraph_edge_clause_both_forms() {
        assert_eq!(
            ward_subgraph_edge_clause(&[]),
            "Always includes Represents for this algorithm; using all other edge types."
        );
        assert_eq!(
            ward_subgraph_edge_clause(&["IssuedBy".to_string(), "LocatedAt".to_string()]),
            "Always includes Represents for this algorithm; ignoring IssuedBy, LocatedAt."
        );
    }

    // ─── path_noun ───────────────────────────────────────────────────────────────────────────

    #[test]
    fn path_noun_singular_only_when_exactly_one_shown() {
        assert_eq!(path_noun(1, Some(1)), "path");
        assert_eq!(path_noun(1, None), "path");
        assert_eq!(path_noun(1, Some(3)), "path");
    }

    /// PROVE THIS CAN FAIL / reproduces the UI-SPEC's own worked "k = 1 with no path found"
    /// example verbatim: `shown = 0`, `requested = Some(1)` still renders PLURAL ("0 of 1
    /// requested paths"), disambiguating the rule's ambiguous prose in favour of the concrete
    /// worked example.
    #[test]
    fn path_noun_zero_shown_is_plural_even_when_exactly_one_was_requested() {
        assert_eq!(path_noun(0, Some(1)), "paths");
    }

    #[test]
    fn path_noun_plural_for_every_other_case() {
        assert_eq!(path_noun(0, None), "paths");
        assert_eq!(path_noun(2, Some(3)), "paths");
        assert_eq!(path_noun(37, None), "paths");
    }

    // ─── seed_clause ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn seed_clause_present_only_when_some() {
        assert_eq!(seed_clause(Some(42)), Some("Seed 42.".to_string()));
        assert_eq!(seed_clause(None), None);
    }

    // ─── status_line_text — six Normal worked examples, verbatim ───────────────────────────

    fn sample_community(index: u32, size: u64, hue: u32) -> CommunityResult {
        CommunityResult {
            index,
            size,
            top_entities: vec![],
            color_hue_deg: hue,
            member_node_ids: vec![],
        }
    }

    #[test]
    fn community_normal_worked_example_matches_verbatim() {
        let communities: Vec<CommunityResult> =
            (0..7).map(|i| sample_community(i, 10, 0)).collect();
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::Community {
                communities,
                hidden_node_ids: vec!["x".to_string(); 41_022],
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![
                "Represents".to_string(),
                "IssuedBy".to_string(),
                "LocatedAt".to_string(),
            ],
            forced_included_edge_type: None,
            seed_used: Some(42),
            caps_used: AlgorithmCaps {
                min_community_size: Some(5),
                ..empty_caps()
            },
            truncation: no_truncation(),
            wall_clock_ms: 812,
        };
        assert_eq!(
            status_line_text(&result, None, None),
            "Showing 7 communities, 41,022 nodes hidden below minimum size. Ignoring Represents, IssuedBy, LocatedAt. Seed 42."
        );
    }

    #[test]
    fn community_truncated_worked_example_matches_verbatim() {
        let communities: Vec<CommunityResult> =
            (0..9).map(|i| sample_community(i, 10, 0)).collect();
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::Community {
                communities,
                hidden_node_ids: vec![],
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![
                "Represents".to_string(),
                "IssuedBy".to_string(),
                "LocatedAt".to_string(),
            ],
            forced_included_edge_type: None,
            seed_used: Some(42),
            caps_used: empty_caps(),
            truncation: truncated("wall_clock", "stopped at the 30s wall-clock budget"),
            wall_clock_ms: 30_000,
        };
        assert_eq!(
            status_line_text(&result, None, None),
            "Showing 9 communities, stopped before full convergence within the 30 s budget. Ignoring Represents, IssuedBy, LocatedAt. Seed 42."
        );
    }

    #[test]
    fn k_core_normal_worked_example_matches_verbatim() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::KCore {
                k: 3,
                node_count: 1_204,
                edge_count: 3_880,
                top_nodes: vec![],
                hidden_node_ids: vec![],
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                k: Some(3),
                ..empty_caps()
            },
            truncation: no_truncation(),
            wall_clock_ms: 500,
        };
        assert_eq!(
            status_line_text(&result, None, None),
            "Showing the 3-core: 1,204 nodes, 3,880 edges. Using all edge types."
        );
    }

    #[test]
    fn k_core_truncated_worked_example_matches_verbatim() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::KCore {
                k: 3,
                node_count: 900,
                edge_count: 2_000,
                top_nodes: vec![],
                hidden_node_ids: vec![],
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                k: Some(3),
                ..empty_caps()
            },
            truncation: truncated("wall_clock", "stopped at the 30s wall-clock budget"),
            wall_clock_ms: 30_000,
        };
        assert_eq!(
            status_line_text(&result, None, None),
            "Showing the 3-core: computation stopped at the 30 s budget, partial result. Using all edge types."
        );
    }

    #[test]
    fn betweenness_normal_worked_example_matches_verbatim() {
        let top_nodes: Vec<RankedEntity> = (0..50)
            .map(|i| RankedEntity {
                entity: entity(&format!("e{i}"), &format!("Entity {i}")),
                score: 1.0,
            })
            .collect();
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::Betweenness {
                sampled_sources: 500,
                total_sources: 113_384,
                top_nodes,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![
                "Represents".to_string(),
                "IssuedBy".to_string(),
                "LocatedAt".to_string(),
            ],
            forced_included_edge_type: None,
            seed_used: Some(42),
            caps_used: AlgorithmCaps {
                top_n: Some(50),
                ..empty_caps()
            },
            truncation: truncated("sample", "sampled 500 of 113384 source nodes"),
            wall_clock_ms: 2_000,
        };
        assert_eq!(
            status_line_text(&result, None, None),
            "Showing top 50 of 113,384 nodes by approximate betweenness — 500 of 113,384 sources sampled. Ignoring Represents, IssuedBy, LocatedAt. Seed 42."
        );
    }

    #[test]
    fn all_simple_paths_normal_worked_example_matches_verbatim() {
        let paths: Vec<PathResult> = (0..37).map(|_| PathResult { hops: vec![] }).collect();
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::Paths { paths },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![
                "Represents".to_string(),
                "IssuedBy".to_string(),
                "LocatedAt".to_string(),
            ],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(4),
                result_cap: Some(100),
                budget_secs: Some(5),
                ..empty_caps()
            },
            truncation: no_truncation(),
            wall_clock_ms: 300,
        };
        let from = entity("director:jane-smith", "Jane Smith");
        let to = entity("GB-COH:99000001", "Acme Ltd");
        assert_eq!(
            status_line_text(&result, Some(&from), Some(&to)),
            "Showing 37 paths up to 4 hops between Jane Smith (director:jane-smith) and Acme Ltd (GB-COH:99000001). Ignoring Represents, IssuedBy, LocatedAt."
        );
    }

    #[test]
    fn all_simple_paths_truncated_worked_example_matches_verbatim() {
        let paths: Vec<PathResult> = (0..100).map(|_| PathResult { hops: vec![] }).collect();
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::Paths { paths },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![
                "Represents".to_string(),
                "IssuedBy".to_string(),
                "LocatedAt".to_string(),
            ],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(4),
                result_cap: Some(100),
                budget_secs: Some(5),
                ..empty_caps()
            },
            truncation: truncated("result_cap", "stopped at result cap 100"),
            wall_clock_ms: 900,
        };
        let from = entity("director:jane-smith", "Jane Smith");
        let to = entity("GB-COH:99000001", "Acme Ltd");
        assert_eq!(
            status_line_text(&result, Some(&from), Some(&to)),
            "100 of 100 result cap reached, 0.9 s of 5 s budget used, between Jane Smith (director:jane-smith) and Acme Ltd (GB-COH:99000001). Ignoring Represents, IssuedBy, LocatedAt."
        );
    }

    #[test]
    fn shortest_paths_normal_worked_example_matches_verbatim() {
        let paths: Vec<PathResult> = (0..3).map(|_| PathResult { hops: vec![] }).collect();
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths,
                requested_k: 3,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                k: Some(3),
                hop_cap: Some(4),
                ..empty_caps()
            },
            truncation: no_truncation(),
            wall_clock_ms: 100,
        };
        let from = entity("director:jane-smith", "Jane Smith");
        let to = entity("GB-COH:99000001", "Acme Ltd");
        assert_eq!(
            status_line_text(&result, Some(&from), Some(&to)),
            "Showing 3 of 3 shortest paths between Jane Smith (director:jane-smith) and Acme Ltd (GB-COH:99000001). Using all edge types."
        );
    }

    #[test]
    fn shortest_paths_truncated_worked_example_matches_verbatim() {
        let paths: Vec<PathResult> = (0..2).map(|_| PathResult { hops: vec![] }).collect();
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths,
                requested_k: 3,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                k: Some(3),
                hop_cap: Some(4),
                ..empty_caps()
            },
            truncation: no_truncation(),
            wall_clock_ms: 100,
        };
        let from = entity("director:jane-smith", "Jane Smith");
        let to = entity("GB-COH:99000001", "Acme Ltd");
        assert_eq!(
            status_line_text(&result, Some(&from), Some(&to)),
            "Showing 2 of 3 requested paths — only 2 distinct paths exist between Jane Smith (director:jane-smith) and Acme Ltd (GB-COH:99000001). Using all edge types."
        );
    }

    /// The pluralisation rule's own worked example: `k = 1` normal result.
    #[test]
    fn shortest_paths_k_equals_one_normal_worked_example_matches_verbatim() {
        let paths: Vec<PathResult> = vec![PathResult { hops: vec![] }];
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths,
                requested_k: 1,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: empty_caps(),
            truncation: no_truncation(),
            wall_clock_ms: 50,
        };
        let from = entity("director:jane-smith", "Jane Smith");
        let to = entity("GB-COH:99000001", "Acme Ltd");
        assert_eq!(
            status_line_text(&result, Some(&from), Some(&to)),
            "Showing 1 of 1 shortest path between Jane Smith (director:jane-smith) and Acme Ltd (GB-COH:99000001). Using all edge types."
        );
    }

    /// The pluralisation rule's own worked example: `k = 1`, no path found.
    #[test]
    fn shortest_paths_k_equals_one_no_path_worked_example_matches_verbatim() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths: vec![],
                requested_k: 1,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: empty_caps(),
            truncation: no_truncation(),
            wall_clock_ms: 50,
        };
        let from = entity("director:jane-smith", "Jane Smith");
        let to = entity("GB-COH:99000001", "Acme Ltd");
        assert_eq!(
            status_line_text(&result, Some(&from), Some(&to)),
            "Showing 0 of 1 requested paths — no path exists between Jane Smith (director:jane-smith) and Acme Ltd (GB-COH:99000001). Using all edge types."
        );
    }

    /// Operator QA 2026-09-12. `Planted Council` -> `Planted Disqualified Officer` finds 0 paths
    /// at hop cap 3, 7 at hop cap 4 and 98 at hop cap 6. The old wording claimed "no path exists"
    /// for the first and "only 7 distinct paths exist" for the second — both universal negatives
    /// drawn from a search the server had already reported as `truncated: true, reason: hop_cap`.
    /// PROVE THIS CAN FAIL: drop the `truncated` argument's effect and these assertions fail.
    #[test]
    fn shortest_paths_zero_results_at_a_hop_cap_never_claims_no_path_exists() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths: vec![],
                requested_k: 100,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(3),
                result_cap: Some(500),
                budget_secs: Some(10),
                k: Some(100),
                top_n: None,
                min_community_size: None,
            },
            truncation: Truncation {
                truncated: true,
                reason: Some("hop_cap".to_string()),
                detail: "stopped at hop cap 3".to_string(),
            },
            wall_clock_ms: 4,
        };
        let from = entity("GB-COH:PL000099", "Planted Council");
        let to = entity("director:planted-od", "Planted Disqualified Officer");
        let text = status_line_text(&result, Some(&from), Some(&to));
        assert!(
            text.contains("no path within 3 hops"),
            "must name the bound it searched to, got: {text}"
        );
        assert!(
            !text.contains("no path exists"),
            "must never assert a universal negative from a capped search, got: {text}"
        );
    }

    #[test]
    fn shortest_paths_partial_results_at_a_hop_cap_never_claim_that_is_all_that_exist() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths: (0..7).map(|_| PathResult { hops: vec![] }).collect(),
                requested_k: 100,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(4),
                result_cap: Some(500),
                budget_secs: Some(10),
                k: Some(100),
                top_n: None,
                min_community_size: None,
            },
            truncation: Truncation {
                truncated: true,
                reason: Some("hop_cap".to_string()),
                detail: "stopped at hop cap 4".to_string(),
            },
            wall_clock_ms: 6,
        };
        let from = entity("GB-COH:PL000099", "Planted Council");
        let to = entity("director:planted-od", "Planted Disqualified Officer");
        let text = status_line_text(&result, Some(&from), Some(&to));
        assert!(
            text.contains("within 4 hops") && text.contains("more may exist"),
            "must say what bound produced these and that more may lie beyond, got: {text}"
        );
        assert!(
            !text.contains("distinct paths exist"),
            "must never claim exhaustiveness from a capped search, got: {text}"
        );
    }

    /// The SECOND overclaim, measured through the HTTP path at the 2026-09-12 QA session:
    /// `GB-COH:PL000099` to `ward:X05999001` returns 0 paths with the three hub edge types
    /// unticked and 3 with all eleven included, `truncated: false` in both. The search is
    /// genuinely exhaustive, so the hop-cap guard above stays silent — and the analyst reads
    /// "no path exists" about a pair with three paths between them. An exhaustive search over a
    /// filtered edge set says nothing about the unfiltered graph.
    ///
    /// PROVE THIS CAN FAIL: drop the `edge_types_excluded` argument's effect and this fails on
    /// the "no path exists" assertion.
    #[test]
    fn shortest_paths_zero_results_under_edge_exclusion_never_claims_no_path_exists() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths: vec![],
                requested_k: 3,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![
                "Represents".to_string(),
                "IssuedBy".to_string(),
                "LocatedAt".to_string(),
            ],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(4),
                result_cap: Some(500),
                budget_secs: Some(30),
                k: Some(3),
                top_n: None,
                min_community_size: None,
            },
            truncation: Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            wall_clock_ms: 3,
        };
        let from = entity("GB-COH:PL000099", "Planted Council");
        let to = entity("ward:X05999001", "Planted Ward");
        let text = status_line_text(&result, Some(&from), Some(&to));
        assert!(
            !text.contains("no path exists"),
            "an exhaustive search over a FILTERED edge set is not a universal negative, got: {text}"
        );
        assert!(
            text.contains("with the current edge-type selection"),
            "must qualify the claim by the edge-type selection that produced it, got: {text}"
        );
        assert!(
            text.contains("Ignoring Represents, IssuedBy, LocatedAt."),
            "the exclusion clause must still name which types were dropped, got: {text}"
        );
    }

    /// The same qualification on the non-zero exhaustive branch: "only N distinct paths exist"
    /// is a claim about the whole graph, and is equally unearned while any edge type is excluded.
    #[test]
    fn shortest_paths_exhaustive_results_under_edge_exclusion_never_claim_that_is_all_that_exist() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths: (0..2).map(|_| PathResult { hops: vec![] }).collect(),
                requested_k: 10,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec!["LocatedAt".to_string()],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(4),
                result_cap: Some(500),
                budget_secs: Some(30),
                k: Some(10),
                top_n: None,
                min_community_size: None,
            },
            truncation: Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            wall_clock_ms: 5,
        };
        let from = entity("GB-COH:PL000099", "Planted Council");
        let to = entity("ward:X05999001", "Planted Ward");
        let text = status_line_text(&result, Some(&from), Some(&to));
        assert!(
            !text.contains("distinct paths exist"),
            "exhaustive over a filtered edge set is not exhaustive over the graph, got: {text}"
        );
        assert!(
            text.contains("with the current edge-type selection"),
            "must qualify the count by the edge-type selection, got: {text}"
        );
    }

    /// The guard must not fire when nothing is excluded: the UI-SPEC's own worked example keeps
    /// the unqualified wording, and that is correct — an exhaustive search over the FULL edge set
    /// really has established that no path exists.
    #[test]
    fn shortest_paths_zero_results_with_all_edge_types_still_says_no_path_exists() {
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::ShortestPaths {
                paths: vec![],
                requested_k: 1,
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(4),
                result_cap: Some(500),
                budget_secs: Some(30),
                k: Some(1),
                top_n: None,
                min_community_size: None,
            },
            truncation: Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            wall_clock_ms: 2,
        };
        let from = entity("director:jane-smith", "Jane Smith");
        let to = entity("GB-COH:99000001", "Acme Ltd");
        let text = status_line_text(&result, Some(&from), Some(&to));
        assert_eq!(
            text,
            "Showing 0 of 1 requested paths — no path exists between Jane Smith (director:jane-smith) and Acme Ltd (GB-COH:99000001). Using all edge types."
        );
    }

    #[test]
    fn ward_subgraph_normal_worked_example_matches_verbatim() {
        let ward = entity("ward:X05001499", "Sample Ward");
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::WardSubgraph {
                ward,
                hops: 2,
                node_count: 1_842,
                edge_count: 3_110,
                node_ids: vec![],
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec!["IssuedBy".to_string(), "LocatedAt".to_string()],
            forced_included_edge_type: Some("Represents".to_string()),
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(2),
                result_cap: Some(1_000),
                ..empty_caps()
            },
            truncation: no_truncation(),
            wall_clock_ms: 400,
        };
        assert_eq!(
            status_line_text(&result, None, None),
            "Showing 1,842 nodes and 3,110 edges reachable from Sample Ward (ward:X05001499) within 2 hops. Always includes Represents for this algorithm; ignoring IssuedBy, LocatedAt."
        );
    }

    #[test]
    fn ward_subgraph_truncated_worked_example_matches_verbatim() {
        let ward = entity("ward:X05001499", "Sample Ward");
        let result = AlgorithmResult {
            outcome: AlgorithmOutcome::WardSubgraph {
                ward,
                hops: 2,
                node_count: 1_000,
                edge_count: 1_500,
                node_ids: vec![],
            },
            population_generated_at: "2026-01-01T10:30:00Z".to_string(),
            excluded_edge_types: vec!["IssuedBy".to_string(), "LocatedAt".to_string()],
            forced_included_edge_type: Some("Represents".to_string()),
            seed_used: None,
            caps_used: AlgorithmCaps {
                hop_cap: Some(2),
                result_cap: Some(1_842),
                ..empty_caps()
            },
            truncation: truncated("result_cap", "stopped at result cap 1000"),
            wall_clock_ms: 400,
        };
        assert_eq!(
            status_line_text(&result, None, None),
            "Showing 1,000 of an estimated 1,842+ reachable nodes — result cap reached. Always includes Represents for this algorithm; ignoring IssuedBy, LocatedAt."
        );
    }

    // ─── HARD_BUDGET_CEILING_SECS ────────────────────────────────────────────────────────────

    #[test]
    fn hard_budget_ceiling_is_thirty_seconds_matching_d08() {
        assert_eq!(HARD_BUDGET_CEILING_SECS, 30);
    }

    // ─── automatic_effect_for ────────────────────────────────────────────────────────────────

    fn ward_outcome() -> AlgorithmOutcome {
        AlgorithmOutcome::WardSubgraph {
            ward: entity("ward:X05001499", "Sample Ward"),
            hops: 2,
            node_count: 10,
            edge_count: 5,
            node_ids: vec![],
        }
    }

    #[test]
    fn automatic_effect_for_maps_every_outcome_variant_to_the_documented_category() {
        assert_eq!(
            automatic_effect_for(&AlgorithmOutcome::Community {
                communities: vec![],
                hidden_node_ids: vec![],
            }),
            CanvasEffect::Recolor
        );
        assert_eq!(
            automatic_effect_for(&AlgorithmOutcome::KCore {
                k: 3,
                node_count: 0,
                edge_count: 0,
                top_nodes: vec![],
                hidden_node_ids: vec![],
            }),
            CanvasEffect::Hide
        );
        assert_eq!(
            automatic_effect_for(&AlgorithmOutcome::Betweenness {
                sampled_sources: 0,
                total_sources: 0,
                top_nodes: vec![],
            }),
            CanvasEffect::Resize
        );
        assert_eq!(automatic_effect_for(&ward_outcome()), CanvasEffect::Hide);
        assert_eq!(
            automatic_effect_for(&AlgorithmOutcome::Paths { paths: vec![] }),
            CanvasEffect::None
        );
        assert_eq!(
            automatic_effect_for(&AlgorithmOutcome::ShortestPaths {
                paths: vec![],
                requested_k: 1,
            }),
            CanvasEffect::None
        );
    }

    // ─── betweenness_resize_scale ────────────────────────────────────────────────────────────

    /// PROVE THIS CAN FAIL: a naive `score / max_score * 2.0 + 1.0` without the zero-guard would
    /// return `1.0` too for `score == 0.0` coincidentally when `max_score > 0`, but would divide
    /// by zero (NaN) if `max_score == 0.0` — this asserts the explicit guard handles that case.
    #[test]
    fn betweenness_resize_scale_zero_score_always_stays_at_one_x() {
        assert_eq!(betweenness_resize_scale(0.0, 100.0), 1.0);
        assert_eq!(
            betweenness_resize_scale(0.0, 0.0),
            1.0,
            "never NaN/invisible even with a degenerate max_score"
        );
    }

    #[test]
    fn betweenness_resize_scale_max_score_reaches_three_x() {
        assert_eq!(betweenness_resize_scale(100.0, 100.0), 3.0);
    }

    #[test]
    fn betweenness_resize_scale_is_linear_between_the_bounds() {
        assert_eq!(betweenness_resize_scale(50.0, 100.0), 2.0);
        assert_eq!(betweenness_resize_scale(25.0, 100.0), 1.5);
    }
}
