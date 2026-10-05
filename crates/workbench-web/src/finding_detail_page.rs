//! `FindingDetailPage` — the `/finding/:probe` route (D-03, D-12–D-16): a real, deep-linkable
//! route mirroring `crate::profile::mod::EntityProfilePage`'s own `use_params_map` pattern,
//! seeded from the already-fetched `ReportView` (no new fetch for the finding itself, per
//! the phase 50 UI spec's "detail route" section) rather than a re-query.
//!
//! # What this page fetches, and what it does not
//!
//! The finding's own data — hypothesis, verdict, evidence — comes from `/api/analysis/report`,
//! reusing `analysis_page::fetch_report` directly rather than a second copy of the same call. The
//! ONE genuinely new fetch this page performs is the finding-seeded population expansion for a
//! graph-shaped finding (`Star`/`TypedEdge`): `GET /api/entity/neighbourhood?ids=...&hops=...`,
//! plan 50-04's multi-seed extension.
//!
//! # Which evidence row seeds the neighbourhood fetch (a Rule 1 deviation from this plan's own
//! literal action text)
//!
//! This plan's own `<action>` text names `probe.evidence.first()` as the source of the seeded
//! ids. That is wrong for a `Star`-kind probe: `finding_card.rs`'s own top-of-file doc comment
//! (and this plan's `<prior_work>` note, carried forward verbatim) documents that
//! `address-cluster-exact`'s `evidence[0]` is a ref-less AGGREGATE row, and `evidence[1]` is the
//! real star cluster the card's own thumbnail draws from. Seeding the neighbourhood fetch from a
//! blind `.first()` would send an empty `ids=` for the one live `Star` example this phase names —
//! a `RecordRef`-bearing endpoint call with no `RecordRef`s in it. [`seed_refs_for_graph_motif`]
//! instead reuses `finding_card::motif_evidence_row` (`Star`) / `motif_typed_edge::
//! typed_edge_source_row` (`TypedEdge`) — the SAME row each kind's own thumbnail already drew
//! from, which is the plan's own stated intent ("the same evidence row the thumbnail drew from"),
//! just not the literal one-line snippet that would have broken it.
//!
//! Reached from `finding_card.rs`'s "View finding →" link / card-body click, itself reached from
//! `finding_board.rs` -> `analysis_page.rs` -> `app.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use urban_integrity_workbench_dto::canvas_filter::MissingEvidence;
use urban_integrity_workbench_dto::caps;
use urban_integrity_workbench_dto::neighbourhood::NeighbourhoodView;
use urban_integrity_workbench_dto::{
    AlgorithmResult, EntityRef, EvidenceRowView, ProbeView, ReportView,
};

use crate::analysis_page::fetch_report;
use crate::breadcrumb::Breadcrumb;
use crate::canvas_filters::{self, CanvasFilterSignals, CanvasFilters};
use crate::cosmograph_trait::{CanvasLinkRow, CanvasPointRow, CosmographHandle, RowMountParams};
use crate::edge_palette;
use crate::entity_ref_view::EntityRefView;
use crate::finding_card::{motif_evidence_row, thumbnail_dispatch, ThumbnailKind};
use crate::graph_page;
use crate::graph_status_line::StatusLine;
use crate::motif_star::MotifStar;
use crate::motif_typed_edge::{typed_edge_source_row, MotifTypedEdge};
use crate::neighbourhood as neighbourhood_mod;
use crate::node_palette;
use crate::profile::pivot::encode_segment;
use crate::tool_rail::{self, ToolRail, ToolRailSignals};
use crate::verdict_badge::VerdictBadge;

/// D-19/§10's exact "unreadable endpoint shape" copy, extended to this page's own
/// finding-seeded population fetch (the phase 50 UI spec's "Error — finding-seeded population
/// failure" row, verbatim).
pub const POPULATION_FETCH_ERROR_COPY: &str =
    "The workbench returned data this page cannot read (/api/entity/neighbourhood).";

/// The detail route's larger motif rendering size. The phase 50 UI spec said "400×400 viewBox"; the
/// operator's second 50-11 verify (2026-09-18) asked for it to "double in terms of width and
/// height" — at 400px a nine-node labelled neighbourhood was "still too small". The box that
/// holds it is `min(800px, 100%)` wide with `aspect-ratio: 1`, so on a narrow viewport it shrinks
/// back rather than overflowing; the SVG and the canvas both fill whatever square results.
const DETAIL_MOTIF_SIZE: f64 = 800.0;
/// The detail route's higher leaf/target budget — 20, not the thumbnail's 8.
const DETAIL_MOTIF_BUDGET: usize = 20;
/// T-50-01's URL-safety cap: how many ids this page ever places into the neighbourhood fetch's
/// query string, independent of the motif renderer's own 8/20-item DISPLAY budget above — a
/// display cap is not a URL-safety cap, and `address-cluster-exact` has named a single finding
/// with 1,813 distinct ids on the live corpus.
///
/// `pub(crate)` (was private): `crate::alert_detail_page::alert_seed_refs` (46-10) reuses this
/// SAME cap for its own seed-id list (threat T-46-02) rather than redefining it — the
/// plan 46-10 interface note's own instruction ("use whichever constant is already in scope
/// … rather than importing one into the other's module" applies to the DISPLAY-budget constants
/// below; this URL-safety cap is the one constant that genuinely is shared, since both pages feed
/// the same `GET /api/entity/neighbourhood?ids=…` endpoint through the same generalised
/// `GraphMotifDetail`).
///
/// Defined as the server's own ceiling, [`caps::CEILING_NEIGHBOURHOOD_SEEDS`] (WR-04): the
/// handler truncates to the same figure, so the two sides cannot drift apart.
pub(crate) const MAX_SEED_IDS_IN_URL: usize = caps::CEILING_NEIGHBOURHOOD_SEEDS;

/// The exact "no finding named" copy (`<action>`'s own wording) — never a bare 404, since the
/// analyst reached this page from a link and deserves to know what was actually looked up.
pub fn not_found_copy(probe_name: &str) -> String {
    format!("No finding named '{probe_name}' in this report.")
}

/// Looks `probe_name` up across every group's probes in the already-fetched `report` — the T-50-03
/// mitigation: the untrusted `:probe` URL param is validated against the report's own known probe
/// names before anything is rendered, never indexed unchecked. `None` for a typo, a stale link, or
/// a probe renamed since the link was generated; the caller renders the not-found state, never a
/// panic and never an `.unwrap()` on this lookup.
pub fn find_probe<'a>(report: &'a ReportView, probe_name: &str) -> Option<&'a ProbeView> {
    report
        .groups
        .iter()
        .flat_map(|group| &group.probes)
        .find(|probe| probe.name == probe_name)
}

/// This graph motif's declared default `hops` (D-13) — `None` for a non-graph kind, since
/// [`GraphMotifDetail`] is never mounted for one.
fn default_hops_for(kind: ThumbnailKind) -> Option<u32> {
    match kind {
        ThumbnailKind::Star => Some(caps::DEFAULT_HOPS_STAR),
        ThumbnailKind::TypedEdge => Some(caps::DEFAULT_HOPS_TYPED_EDGE),
        _ => None,
    }
}

/// The refs a graph-motif finding's neighbourhood fetch seeds from — see this file's top-of-file
/// doc comment for why this is not `probe.evidence.first()`. `TypedEdge` reuses
/// `typed_edge_source_row` (prefers a `DisqualifiedFrom` row over a `Controls` row, exactly as its
/// own thumbnail does); every other graph kind (`Star`) reuses `motif_evidence_row` (skips a
/// ref-less leading aggregate row).
pub fn seed_refs_for_graph_motif(
    kind: ThumbnailKind,
    evidence: &[EvidenceRowView],
) -> Vec<EntityRef> {
    match kind {
        ThumbnailKind::TypedEdge => typed_edge_source_row(evidence)
            .map(|(row, _)| row.refs.clone())
            .unwrap_or_default(),
        _ => motif_evidence_row(evidence)
            .map(|row| row.refs)
            .unwrap_or_default(),
    }
}

/// `refs`, percent-encoded one at a time (`encode_segment`, so a comma inside an id can never be
/// mistaken for the join separator) and capped at [`MAX_SEED_IDS_IN_URL`] — the T-50-01
/// mitigation.
pub fn seed_ids_csv(refs: &[EntityRef]) -> String {
    refs.iter()
        .take(MAX_SEED_IDS_IN_URL)
        .map(|r| encode_segment(&r.id))
        .collect::<Vec<_>>()
        .join(",")
}

/// `GET /api/entity/neighbourhood?ids=...&hops=...&node_cap=...` — plan 50-04's multi-seed
/// extension, built from this finding's own seed refs rather than a single `id=`.
pub fn multi_seed_neighbourhood_url(refs: &[EntityRef], hops: u32) -> String {
    format!(
        "{}?ids={}&hops={hops}&node_cap={}",
        neighbourhood_mod::NEIGHBOURHOOD_URL_PATH,
        seed_ids_csv(refs),
        caps::DEFAULT_NEIGHBOURHOOD_NODE_CAP,
    )
}

/// Fetches the finding-seeded neighbourhood. Same fetch/error-string shape as every other fetch
/// in this crate; not unit-tested directly (`gloo-net`'s Fetch-API transport panics outside a
/// browser runtime, same rationale as `analysis_page.rs::fetch_report`).
pub async fn fetch_finding_neighbourhood(
    refs: &[EntityRef],
    hops: u32,
) -> Result<NeighbourhoodView, String> {
    let response = gloo_net::http::Request::get(&multi_seed_neighbourhood_url(refs, hops))
        .send()
        .await
        .map_err(|_| POPULATION_FETCH_ERROR_COPY.to_string())?;
    if !response.ok() {
        return Err(POPULATION_FETCH_ERROR_COPY.to_string());
    }
    response
        .json::<NeighbourhoodView>()
        .await
        .map_err(|_| POPULATION_FETCH_ERROR_COPY.to_string())
}

/// A fresh, this-page-only `ToolRailSignals` — same defaults `graph_page.rs` seeds its own rail
/// with, `population_node_ids` starting empty and filled in once the finding-seeded neighbourhood
/// fetch resolves (never the full population — D-16's "the rail must accept a finding-seeded
/// scope").
fn build_tool_rail_signals() -> ToolRailSignals {
    ToolRailSignals {
        algorithm: RwSignal::new("community".to_string()),
        edge_types: RwSignal::new(tool_rail::default_edge_types_included()),
        from_ref: RwSignal::new(None),
        to_ref: RwSignal::new(None),
        ward_ref: RwSignal::new(None),
        seed: RwSignal::new(caps::DEFAULT_SEED),
        hop_cap: RwSignal::new(caps::DEFAULT_HOP_CAP),
        result_cap: RwSignal::new(caps::DEFAULT_RESULT_CAP),
        budget_secs: RwSignal::new(caps::DEFAULT_BUDGET_SECS),
        k: RwSignal::new(caps::DEFAULT_K_SHORTEST),
        top_n: RwSignal::new(caps::DEFAULT_TOP_N),
        min_community_size: RwSignal::new(caps::DEFAULT_MIN_COMMUNITY_SIZE),
        resolution: RwSignal::new(caps::DEFAULT_RESOLUTION),
        session_id: StoredValue::new_local(graph_page::mint_session_id()),
        algorithm_result: RwSignal::new(None),
        algorithm_running: RwSignal::new(false),
        algorithm_error: RwSignal::new(None),
        population_node_ids: StoredValue::new_local(Vec::new()),
        // Starts empty (page just loaded, neighbourhood not resolved yet); the neighbourhood-
        // resolved effect below sets it to the finding's own expansion ids the moment that fetch
        // resolves, so every Run this route posts is scoped to exactly the neighbourhood the
        // canvas draws (plan 50-11's gap-closure fix for the operator's scope-mismatch report).
        scope_ids: RwSignal::new(Vec::new()),
    }
}

/// A fresh `CanvasFilterSignals` — same shape `graph_page.rs` builds, `facets`/`result` starting
/// `None` until this page's own facets fetch (below) resolves.
fn build_canvas_filter_signals() -> CanvasFilterSignals {
    CanvasFilterSignals {
        node_kinds: RwSignal::new(Vec::new()),
        edge_types: RwSignal::new(Vec::new()),
        from_date: RwSignal::new(None),
        to_date: RwSignal::new(None),
        undated_edges: RwSignal::new(MissingEvidence::Keep),
        ward_id: RwSignal::new(None),
        min_p_c: RwSignal::new(None),
        unscored_nodes: RwSignal::new(MissingEvidence::Exclude),
        facets: RwSignal::new(None),
        result: RwSignal::new(None),
        running: RwSignal::new(false),
        error: RwSignal::new(None),
        seeded: RwSignal::new(false),
    }
}

/// Every entity the finding-seeded expansion touches, in seeds-then-nodes order — the ONE
/// iteration shape [`expansion_ids`] and [`neighbourhood_canvas_rows`] both build from, so the two
/// can never drift out of index-alignment with each other.
fn expansion_entities(view: &NeighbourhoodView) -> impl Iterator<Item = &EntityRef> {
    view.seeds
        .iter()
        .chain(view.nodes.iter().map(|n| &n.entity))
}

/// Every id the finding-seeded expansion touches — every seed plus every node the walk returned.
/// This IS the detail route's own "population" (there is no separate mounted canvas set here, per
/// the phase 50 UI spec — the motif SVG stands in for it), so [`canvas_filters::canvas_focus_ids`]'s own
/// `mounted.contains` narrowing is a no-op against it by construction.
fn expansion_ids(view: &NeighbourhoodView) -> Vec<String> {
    expansion_entities(view).map(|s| s.id.clone()).collect()
}

/// D-FR-03's Motif/Canvas state machine: the 800×800 box's rendered stage is a PURE function of
/// whether `algorithm_result` is `Some` — no separate "canvas mounted" flag that could fall out of
/// sync with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailStage {
    Motif,
    Canvas,
}

/// `Some(_)` (a completed run) => [`DetailStage::Canvas`]; `None` (page just loaded, or Reset was
/// pressed) => [`DetailStage::Motif`].
pub fn detail_stage(result: Option<&AlgorithmResult>) -> DetailStage {
    match result {
        Some(_) => DetailStage::Canvas,
        None => DetailStage::Motif,
    }
}

/// The finding's own already-fetched neighbourhood, turned into the row shape
/// [`CosmographHandle::mount_rows`] needs: points in [`expansion_entities`]'s exact
/// seeds-then-nodes order (so an `id_to_index` built from [`expansion_ids`]'s output lines up 1:1
/// with these rows by index), one link per edge on each returned node (`source` = that node's
/// `reached_from`, `target` = the node's own id, `edge_type` copied verbatim). Never panics on a
/// view with zero `nodes` (seeds-only, e.g. every real edge got filtered by a hub exclusion) —
/// `links` is simply empty in that case.
pub fn neighbourhood_canvas_rows(
    view: &NeighbourhoodView,
) -> (Vec<CanvasPointRow>, Vec<CanvasLinkRow>) {
    let points = expansion_entities(view)
        .map(|entity| CanvasPointRow {
            id: entity.id.clone(),
            name: entity.name.clone(),
            kind: entity.kind.clone(),
        })
        .collect();
    let links = view
        .nodes
        .iter()
        .flat_map(|node| {
            node.edges.iter().map(move |edge| CanvasLinkRow {
                source: node.reached_from.clone(),
                target: node.entity.id.clone(),
                edge_type: edge.edge_type.clone(),
            })
        })
        .collect();
    (points, links)
}

/// Every CSR-local index into `population_ids` — `0..population_ids.len()`, the exact set
/// [`CosmographHandle::fit_view_by_indices`] needs to fit the whole mounted neighbourhood. Shared
/// by `on_fit`'s click handler and the post-warm-up auto-fit timer below, so the two can never
/// compute a different index set for the same handle.
/// The detail route's pending auto-fit timers (WR-01): at most one post-warm-up fit and one
/// post-result re-fit are ever outstanding. Replacing a slot cancels the timer it held;
/// [`PendingFitTimers::clear_all`] runs from the component's `on_cleanup`. Clearing a timer that
/// has already fired is a browser no-op, so stale handles are harmless.
#[derive(Default)]
struct PendingFitTimers {
    warm_up: Option<TimeoutHandle>,
    refit: Option<TimeoutHandle>,
}

impl PendingFitTimers {
    fn replace_warm_up(&mut self, handle: TimeoutHandle) {
        if let Some(previous) = self.warm_up.replace(handle) {
            previous.clear();
        }
    }

    fn replace_refit(&mut self, handle: TimeoutHandle) {
        if let Some(previous) = self.refit.replace(handle) {
            previous.clear();
        }
    }

    fn clear_all(&mut self) {
        for handle in [self.warm_up.take(), self.refit.take()]
            .into_iter()
            .flatten()
        {
            handle.clear();
        }
    }
}

fn all_population_indices(population_ids: StoredValue<Vec<String>, LocalStorage>) -> Vec<u32> {
    population_ids.with_value(|ids| (0..ids.len() as u32).collect())
}

/// Step 3/4 of the phase 50 UI spec's detail route (D-16): the motif re-rendered at
/// [`DETAIL_MOTIF_SIZE`]/[`DETAIL_MOTIF_BUDGET`], followed by the finding-seeded neighbourhood
/// fetch and — once it resolves — the Phase 44 tool rail plus canvas filters, scoped to that
/// neighbourhood.
///
/// # 46-10's generalisation, and the third `probe` read the plan text missed
///
/// Phase 46 research Q3 (and plan 46-10's own `<interfaces>` section, quoting it) claimed only
/// two call sites inside this function read `probe`: `thumbnail_dispatch(&probe)` and
/// `seed_refs_for_graph_motif(kind, &probe.evidence)`. Reading the function body before widening
/// it (as plan 46-10's own Task 1 `<action>` explicitly asked the executor to do) found a
/// third, larger one the research missed entirely: the render-motif closure at the bottom of this
/// function (the `DetailStage::Motif` branch) ALSO reads `kind` and `probe.evidence` directly, to
/// choose between `MotifTypedEdge`/`MotifStar` and to build each one's `evidence` prop — data that
/// cannot be reduced to `seed_refs: Vec<EntityRef>` (evidence rows carry claim TEXT, not just
/// entity ids). Passing only `(seed_refs, hops)` and dropping this third dependency would silently
/// delete `/finding/:probe`'s own motif preview — the opposite of "behaviour-preserving".
///
/// The fix follows the SAME pattern the plan already prescribes for the other two: move the
/// probe-dependent computation to the CALLER. `motif_preview` is that computation, supplied as a
/// `Callback<(), AnyView>` (this crate's established callback idiom, 46-09's own decision) so the
/// reactive `stage`-driven closure below can invoke it repeatedly without re-deriving anything.
/// `/finding/:probe`'s own call site (`FindingDetailPage`, below) builds this closure from the
/// EXACT SAME match arms this function used to own, so its rendered output is unchanged byte for
/// byte — proved by `cargo test -p urban-integrity-workbench-web` still passing every pre-existing
/// test in this file with no edits to any of them (Task 1's own acceptance criterion). The alert
/// detail route (`crate::alert_detail_page`) is the second caller, and (as of 46-13) supplies its
/// own induced-subgraph `MotifPath` at `DETAIL_MOTIF_SIZE`, labelled — the same drawing its
/// standalone step-3 box above renders — rather than a participant-chip fallback; see that
/// module for why both boxes draw the motif.
///
/// `pub(crate)` (was private): the second caller lives in a sibling module.
#[component]
#[allow(non_snake_case)]
pub(crate) fn GraphMotifDetail(
    seed_refs: Vec<EntityRef>,
    hops: u32,
    /// The 800×800 box's "before an algorithm has run" content. See this doc comment's "third
    /// `probe` read" section for why this is a caller-supplied closure rather than something this
    /// function derives from a `ProbeView` itself.
    motif_preview: Callback<(), AnyView>,
) -> impl IntoView {
    let neighbourhood: RwSignal<Option<Result<NeighbourhoodView, String>>> = RwSignal::new(None);
    let tool_rail_state = build_tool_rail_signals();
    let filter_state = build_canvas_filter_signals();

    // D-FR-03's Motif/Canvas mount state: `canvas_ref` is the `<div>` the canvas branch below
    // renders once `stage` is `Canvas`; `cosmograph` holds the mounted handle (`None` until the
    // first successful run, and again after Reset — Reset drops it, which invokes its
    // `Drop`-driven `graphDestroy()` before the canvas `<div>` leaves the DOM). `id_to_index`/
    // `population_ids` mirror `graph_page.rs`'s own mount-effect shape, filled once the
    // neighbourhood resolves (below) rather than a second fetch.
    let canvas_ref: NodeRef<leptos::html::Div> = NodeRef::new();
    let cosmograph: StoredValue<Option<CosmographHandle>, LocalStorage> =
        StoredValue::new_local(None);
    let id_to_index: StoredValue<HashMap<String, u32>, LocalStorage> =
        StoredValue::new_local(HashMap::new());
    let population_ids: StoredValue<Vec<String>, LocalStorage> = StoredValue::new_local(Vec::new());
    let canvas_mount_error: RwSignal<Option<String>> = RwSignal::new(None);
    let stage =
        Signal::derive(move || detail_stage(tool_rail_state.algorithm_result.get().as_ref()));

    // WR-01 (phase 50 code review): the two auto-fit timers below are browser `setTimeout`s, which live
    // outside Leptos's reactive ownership tree — unmounting this component does not cancel them.
    // Their handles are kept here (a plain `Arc<Mutex<..>>`, deliberately NOT a reactive arena
    // value, so the cleanup below never reads a slot the owner is in the middle of disposing) and
    // cleared on unmount, mirroring `graph_page.rs`'s `on_cleanup` discipline for its window
    // listeners. A navigate-away therefore cancels any pending fit deterministically instead of
    // relying on the closures' own `with_value`/`if let Some` guards to make a late firing
    // harmless.
    let pending_fits: Arc<Mutex<PendingFitTimers>> = Arc::default();
    {
        let pending_fits = Arc::clone(&pending_fits);
        on_cleanup(move || {
            if let Ok(mut timers) = pending_fits.lock() {
                timers.clear_all();
            }
        });
    }

    // The neighbourhood fetch — runs exactly once, at this component's mount (no tracked signal
    // read, so `Effect::new` fires once and never again). A finding with no seed refs at all
    // (defensive; every real graph-motif probe carries at least a centre/officer ref) reports the
    // same failure copy rather than issuing a request that could only ever come back empty.
    {
        let seed_refs = seed_refs.clone();
        Effect::new(move |_| {
            if seed_refs.is_empty() {
                neighbourhood.set(Some(Err(POPULATION_FETCH_ERROR_COPY.to_string())));
                return;
            }
            let seed_refs = seed_refs.clone();
            leptos::task::spawn_local(async move {
                let result = fetch_finding_neighbourhood(&seed_refs, hops).await;
                neighbourhood.set(Some(result));
            });
        });
    }

    // Once the neighbourhood resolves: scope the rail to it and fire the same canvas-filter
    // facets request `graph_page.rs`'s own population-ready effect issues, so `CanvasFilters`
    // does not sit on "Reading what is in view…" forever.
    Effect::new(move |_| {
        if let Some(Ok(view)) = neighbourhood.get() {
            let ids = expansion_ids(&view);
            tool_rail_state.population_node_ids.set_value(ids.clone());
            // The scope-mismatch fix (operator's point 1): every algorithm this route runs is
            // restricted to exactly the finding's own neighbourhood, so the status line's counts
            // describe what the canvas actually draws, never the whole analysis population.
            tool_rail_state.scope_ids.set(ids.clone());
            id_to_index.set_value(
                ids.iter()
                    .enumerate()
                    .map(|(i, id)| (id.clone(), i as u32))
                    .collect(),
            );
            population_ids.set_value(ids.clone());

            let scope_ids = canvas_filters::scope_ids_for(Some(&view), &[], &ids);
            let request = canvas_filters::request_from(filter_state, scope_ids);
            filter_state.running.set(true);
            filter_state.error.set(None);
            leptos::task::spawn_local(async move {
                match canvas_filters::fetch_canvas_filter(request).await {
                    Ok(answer) => {
                        canvas_filters::seed_checklists(filter_state, &answer.facets);
                        filter_state.facets.set(Some(answer.facets.clone()));
                        filter_state.result.set(Some(answer));
                    }
                    Err(message) => filter_state.error.set(Some(message)),
                }
                filter_state.running.set(false);
            });
        }
    });

    // D-FR-03: mounts the canvas the moment a run first produces a result, and applies every
    // SUBSEQUENT result (same or a different algorithm, no intervening Reset) against the
    // already-mounted handle — mirroring `graph_page.rs`'s own reactive `algorithm_result`
    // `Effect` shape. `None` (page just loaded, or Reset just ran) does nothing here: Reset's own
    // handler owns tearing the canvas down, so this effect never fights it.
    Effect::new(move |_| {
        let pending_fits = Arc::clone(&pending_fits);
        let Some(result) = tool_rail_state.algorithm_result.get() else {
            return;
        };
        let already_mounted = cosmograph.with_value(|handle| handle.is_some());
        if !already_mounted {
            let Some(el) = canvas_ref.get() else {
                return;
            };
            // Safe: `<ToolRail>` (the only source of a Run click) is never rendered before
            // `neighbourhood` resolves `Ok`, so `algorithm_result` cannot become `Some` before it
            // does too.
            let Some(Ok(view)) = neighbourhood.get_untracked() else {
                return;
            };
            let (points, links) = neighbourhood_canvas_rows(&view);
            let kind_colors = node_palette::kind_color_entries();
            let edge_colors = edge_palette::edge_color_entries();
            let handle = CosmographHandle::mount_rows(
                RowMountParams {
                    container: &el,
                    points: &points,
                    links: &links,
                    kind_colors: &kind_colors,
                    edge_colors: &edge_colors,
                    warm_up: graph_page::WARM_UP_DURATION,
                },
                |_index: u32| {
                    // Point-click/side-card interaction is out of this gap-closure plan's scope —
                    // the phase 50 UI spec's detail-route contract names only the canvas, the rail and
                    // Reset, never a side card.
                },
                move |message| canvas_mount_error.set(Some(message)),
            );
            cosmograph.set_value(Some(handle));

            // Operator's point 2 (50-09's human-verify gate): the warm-up expands a small
            // (~9-node) neighbourhood past this box's viewport, and nothing on the Graph
            // page's own pattern fits the view automatically — there, Fit is a deliberate user
            // action. Here it must not be, because a first-time visitor has no reason to know a
            // Fit button exists before they have seen the canvas is too small. Scheduled for
            // WARM_UP_DURATION + 300ms slack past the shim's own freeze point (`armWarmUpFreeze`,
            // cosmograph-shim.js), so the fit lands once the simulation has actually settled, not
            // mid-layout. Guarded by `with_value`/`if let Some` against the handle having been
            // dropped by Reset before the timer fires.
            let warm_up_fit = set_timeout_with_handle(
                move || {
                    cosmograph.with_value(|handle| {
                        if let Some(handle) = handle {
                            handle.fit_view_by_indices(&all_population_indices(population_ids));
                        }
                    });
                },
                graph_page::WARM_UP_DURATION + Duration::from_millis(300),
            );
            if let Ok(handle) = warm_up_fit {
                if let Ok(mut timers) = pending_fits.lock() {
                    timers.replace_warm_up(handle);
                }
            }
        }
        cosmograph.with_value(|handle| {
            if let Some(handle) = handle {
                graph_page::apply_result_state(
                    handle,
                    Some(&result),
                    &id_to_index.get_value(),
                    &population_ids.get_value(),
                );
            }
        });
        // Re-fit after EVERY applied result, not only after the first mount: a second Run on an
        // already-mounted canvas re-applies opacity/colour and Cosmograph re-settles the layout,
        // which on the 50-11 re-verify (2026-09-18) pushed labels back past the box's edge — the
        // operator's "only part of it is visible" a second time. 400ms is past Cosmograph's own
        // re-settle; on the very first Run this fires before the warm-up fit above and is simply
        // superseded by it. The `with_value`/`if let` guards a handle Reset has already dropped.
        // A newer result supersedes any re-fit still pending from the previous one, so the
        // previous timer is cancelled rather than left to fire redundantly.
        let refit = set_timeout_with_handle(
            move || {
                cosmograph.with_value(|handle| {
                    if let Some(handle) = handle {
                        handle.fit_view_by_indices(&all_population_indices(population_ids));
                    }
                });
            },
            Duration::from_millis(400),
        );
        if let Ok(handle) = refit {
            if let Ok(mut timers) = pending_fits.lock() {
                timers.replace_refit(handle);
            }
        }
    });

    // Plan 44-13/StatusLine's Re-layout/Fit/Reset controls — the SAME copy and placement as the
    // Graph page's own (D-FR-03's copywriting requirement), driving THIS route's handle/signals.
    let on_relayout = move || {
        cosmograph.with_value(|handle| {
            if let Some(handle) = handle {
                handle.relayout();
            }
        });
    };
    let on_fit = move || {
        let all_indices = all_population_indices(population_ids);
        cosmograph.with_value(|handle| {
            if let Some(handle) = handle {
                handle.fit_view_by_indices(&all_indices);
            }
        });
    };
    // Reset (D-12's heavier clearing action, reused here): clears the result/error, and drops the
    // held `CosmographHandle` — its `Drop` impl calls the shim's `graphDestroy()` before the
    // canvas `<div>` this handle was mounted into leaves the DOM (`stage` flips back to `Motif`
    // the moment `algorithm_result` goes `None`) — never leaves a WebGL context/simulation running
    // against a detached node.
    let on_reset = move || {
        tool_rail_state.algorithm_result.set(None);
        tool_rail_state.algorithm_error.set(None);
        canvas_mount_error.set(None);
        cosmograph.set_value(None);
    };

    // `AnyView` is not `Clone` in this leptos version, so the Motif branch of the reactive
    // `stage`-driven closure below rebuilds it on each call via this small closure rather than
    // holding a single precomputed value — cheap (these are plain SVG components, no fetch, no
    // canvas). `Callback` is `Copy` and safely re-invocable, matching the reason `motif_preview`
    // is a `Callback` rather than a plain `impl Fn` (see this component's doc comment).
    let render_motif = move || motif_preview.run(());

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg);">
            <div style="width: min(800px, 100%); aspect-ratio: 1 / 1; background: var(--card-hover); border: 1px solid var(--border); border-radius: 6px; display: flex; align-items: center; justify-content: center; overflow: hidden;">
                {move || match stage.get() {
                    // D-FR-03: Motif -> Canvas the moment a run produces a result, Canvas -> Motif
                    // on Reset (`stage` is a pure function of `algorithm_result`, so this closure
                    // never disagrees with `on_reset`/the mount effect above about which is shown).
                    DetailStage::Motif => render_motif(),
                    DetailStage::Canvas => view! {
                        <div node_ref=canvas_ref style="width: 100%; height: 100%;"></div>
                    }.into_any(),
                }}
            </div>
            {move || canvas_mount_error.get().map(|message| view! {
                <p class="text-body" role="alert" style="margin: 0; color: var(--destructive);">
                    {message}
                </p>
            })}
            {move || match neighbourhood.get() {
                None => view! {
                    <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                        "Loading the finding's neighbourhood\u{2026}"
                    </p>
                }.into_any(),
                // The population fetch failed. Per the phase 50 UI spec's own state table, this does
                // NOT invalidate the motif above — it already proved the motif geometry
                // independently of this fetch.
                Some(Err(_)) => view! {
                    <p class="text-body" style="margin: 0; color: var(--destructive);">
                        {POPULATION_FETCH_ERROR_COPY}
                    </p>
                }.into_any(),
                Some(Ok(_)) => view! {
                    <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                        <ToolRail state=tool_rail_state />
                        // Canvas filters and the Reset-bearing status line appear only once
                        // `stage` is `Canvas` — D-FR-03's "not shown until an algorithm has
                        // produced a result" requirement, answering the operator's point 2.
                        {move || (stage.get() == DetailStage::Canvas).then(|| view! {
                            <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                                <CanvasFilters state=filter_state />
                                <StatusLine
                                    result=tool_rail_state.algorithm_result.into()
                                    from_ref=tool_rail_state.from_ref.into()
                                    to_ref=tool_rail_state.to_ref.into()
                                    expansion=Signal::derive(|| None::<NeighbourhoodView>)
                                    filter=filter_state.result.into()
                                    on_relayout=on_relayout
                                    on_fit=on_fit
                                    on_reset=on_reset
                                />
                            </div>
                        })}
                    </div>
                }.into_any(),
            }}
        </div>
    }
}

/// Step 5 of the phase 50 UI spec's detail route: `RankedBars`/`Chips`/`Absence`/`Unknown` — no canvas,
/// no tool rail, no neighbourhood fetch. Every evidence row's text plus its full `EntityRefView`
/// (`name (id)` form — unlike the board's compact chips, per D-07's "the detail route is where the
/// real thing renders").
fn text_evidence_detail(probe: ProbeView) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-md);">
            <h2 class="text-heading" style="margin: 0;">"Evidence"</h2>
            <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                {probe.evidence.clone().into_iter().map(|row| view! {
                    <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                        <span class="text-body">{row.text.clone()}</span>
                        <div style="display: flex; flex-wrap: wrap; gap: var(--space-sm); padding-left: var(--space-md);">
                            {row.refs.iter().cloned().map(|entity| view! {
                                <EntityRefView entity=entity />
                            }).collect_view()}
                        </div>
                    </div>
                }).collect_view()}
            </div>
        </div>
    }
}

/// The `/finding/:probe` route (D-03): reads `:probe`, resolves it against the already-fetched
/// `ReportView` via [`find_probe`], and dispatches on `probe.motif_kind` exactly as
/// `finding_card.rs`'s own `thumbnail_dispatch` does — reused directly rather than a second
/// dispatch.
#[component]
#[allow(non_snake_case)]
pub fn FindingDetailPage() -> impl IntoView {
    let params = use_params_map();
    let probe_name = move || params.read().get("probe").unwrap_or_default();

    let initial_fetch = LocalResource::new(fetch_report);
    let report_signal: RwSignal<Option<ReportView>> = RwSignal::new(None);

    Effect::new(move |_| {
        if let Some(Ok(maybe_report)) = initial_fetch.get() {
            report_signal.set(maybe_report);
        }
    });

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); padding-top: var(--space-md); max-width: 960px;">
            <div style="display: flex; align-items: center; gap: var(--space-md);">
                {move || {
                    let trailing = report_signal
                        .get()
                        .and_then(|report| find_probe(&report, &probe_name()).map(|p| p.name.clone()))
                        .unwrap_or_default();
                    view! { <Breadcrumb trailing=trailing /> }
                }}
                <a
                    class="text-label"
                    href="/analysis#report-accordion"
                    style="color: var(--primary); min-height: 44px; display: inline-flex; align-items: center;"
                >
                    "\u{2190} Back to board"
                </a>
            </div>
            {move || {
                let name = probe_name();
                match report_signal.get() {
                    None => view! {
                        <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                            "Loading the last probe run\u{2026}"
                        </p>
                    }.into_any(),
                    Some(report) => match find_probe(&report, &name).cloned() {
                        None => view! {
                            <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                                {not_found_copy(&name)}
                            </p>
                        }.into_any(),
                        Some(probe) => {
                            let kind = thumbnail_dispatch(&probe);
                            view! {
                                <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                                    <h1 class="text-heading" style="margin: 0;">{probe.name.clone()}</h1>
                                    <VerdictBadge verdict=probe.verdict.clone() />
                                    {match kind {
                                        ThumbnailKind::Star | ThumbnailKind::TypedEdge => {
                                            // Computed here rather than inside `GraphMotifDetail`
                                            // itself — see that component's doc comment ("46-10's
                                            // generalisation, and the third `probe` read"). This is
                                            // the EXACT match logic that function used to own,
                                            // moved verbatim to the caller so this page's rendered
                                            // output is unchanged.
                                            let seed_refs = seed_refs_for_graph_motif(kind, &probe.evidence);
                                            let hops = default_hops_for(kind).unwrap_or(caps::DEFAULT_HOPS_STAR);
                                            let motif_preview = Callback::new({
                                                let probe = probe.clone();
                                                move |()| -> AnyView {
                                                    match kind {
                                                        ThumbnailKind::TypedEdge => view! {
                                                            <MotifTypedEdge
                                                                evidence=probe.evidence.clone()
                                                                size=DETAIL_MOTIF_SIZE
                                                                target_budget=DETAIL_MOTIF_BUDGET
                                                                labelled=true
                                                            />
                                                        }
                                                        .into_any(),
                                                        _ => {
                                                            let evidence = motif_evidence_row(&probe.evidence)
                                                                .unwrap_or(EvidenceRowView {
                                                                    text: String::new(),
                                                                    refs: vec![],
                                                                });
                                                            view! {
                                                                <MotifStar
                                                                    evidence=evidence
                                                                    size=DETAIL_MOTIF_SIZE
                                                                    leaf_budget=DETAIL_MOTIF_BUDGET
                                                                    labelled=true
                                                                />
                                                            }
                                                            .into_any()
                                                        }
                                                    }
                                                }
                                            });
                                            view! {
                                                <GraphMotifDetail seed_refs=seed_refs hops=hops motif_preview=motif_preview />
                                            }.into_any()
                                        }
                                        _ => text_evidence_detail(probe.clone()).into_any(),
                                    }}
                                </div>
                            }.into_any()
                        }
                    },
                }
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::{ProbeGroupView, VerdictView};

    fn entity_ref(id: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: id.to_string(),
            kind: "Entity".to_string(),
        }
    }

    fn sample_probe(name: &str, motif_kind: &str, evidence: Vec<EvidenceRowView>) -> ProbeView {
        ProbeView {
            name: name.to_string(),
            tier: "tier-1".to_string(),
            hypothesis: "test hypothesis".to_string(),
            verdict: VerdictView::DataGap {
                missing_input: "missing OSINT source".to_string(),
            },
            measurement_summary: "test summary".to_string(),
            evidence,
            motif_kind: motif_kind.to_string(),
            cause: None,
            remediation: None,
        }
    }

    fn sample_report() -> ReportView {
        ReportView {
            bluf: urban_integrity_workbench_dto::BlufView {
                could_not_evaluate: 0,
                total_probes: 1,
                skipped: 0,
                population: 0,
                capable: 0,
                hit_count: 0,
            },
            contradiction_checks: vec![],
            groups: vec![ProbeGroupView {
                name: "Confirmed Findings".to_string(),
                probes: vec![sample_probe("shared-exact-address", "Star", vec![])],
            }],
            adapted_notes: vec![],
            remediation_items: vec![],
            run_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    // ─── find_probe / not_found_copy (T-50-03) ─────────────────────────────────────────────

    #[test]
    fn find_probe_resolves_an_existing_probe_by_name() {
        let report = sample_report();
        assert!(find_probe(&report, "shared-exact-address").is_some());
    }

    /// PROVE THIS CAN FAIL: an unrecognised `:probe` URL param (typo, stale link, tampered value)
    /// must resolve to `None` — never panic, never index unchecked — so the caller can render the
    /// not-found state instead of crashing on a bad lookup.
    #[test]
    fn an_unrecognised_probe_name_resolves_to_none_never_panics() {
        let report = sample_report();
        assert_eq!(find_probe(&report, "does-not-exist"), None);
        assert_eq!(find_probe(&report, ""), None);
    }

    #[test]
    fn not_found_copy_names_the_looked_up_probe() {
        assert_eq!(
            not_found_copy("does-not-exist"),
            "No finding named 'does-not-exist' in this report."
        );
    }

    // ─── seed_refs_for_graph_motif — the Rule 1 fix ────────────────────────────────────────

    /// PROVE THIS CAN FAIL: replace the body's `Star`/default arm with `evidence.first()` and
    /// this returns the empty aggregate row's refs (`vec![]`) instead of the real cluster's.
    #[test]
    fn star_kind_skips_the_ref_less_aggregate_row_when_seeding_the_neighbourhood_fetch() {
        let evidence = vec![
            EvidenceRowView {
                text: "2 addresses shared by 2+ distinct applicants, of 148 evaluated.".to_string(),
                refs: vec![],
            },
            EvidenceRowView {
                text: "8 applicants at location:planted-l2".to_string(),
                refs: vec![
                    entity_ref("location:planted-l2"),
                    entity_ref("GB-COH:PL000201"),
                ],
            },
        ];
        let refs = seed_refs_for_graph_motif(ThumbnailKind::Star, &evidence);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "location:planted-l2");
    }

    #[test]
    fn typed_edge_kind_prefers_the_disqualified_row_over_a_controls_row() {
        let evidence = vec![
            EvidenceRowView {
                text: "Controls officer Jane Smith".to_string(),
                refs: vec![entity_ref("director:jane-smith"), entity_ref("GB-COH:C1")],
            },
            EvidenceRowView {
                text: "DisqualifiedFrom officer Jane Smith".to_string(),
                refs: vec![entity_ref("director:jane-smith"), entity_ref("GB-COH:D1")],
            },
        ];
        let refs = seed_refs_for_graph_motif(ThumbnailKind::TypedEdge, &evidence);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[1].id, "GB-COH:D1");
    }

    #[test]
    fn seed_refs_for_graph_motif_is_empty_for_a_probe_with_no_evidence() {
        assert_eq!(seed_refs_for_graph_motif(ThumbnailKind::Star, &[]), vec![]);
        assert_eq!(
            seed_refs_for_graph_motif(ThumbnailKind::TypedEdge, &[]),
            vec![]
        );
    }

    // ─── seed_ids_csv / multi_seed_neighbourhood_url (T-50-01) ─────────────────────────────

    /// PROVE THIS CAN FAIL: drop the `.take(MAX_SEED_IDS_IN_URL)` and this returns 60 segments
    /// instead of 50 — the T-50-01 mitigation this pins.
    #[test]
    fn seed_ids_csv_caps_at_the_url_safety_limit_independent_of_display_budgets() {
        let refs: Vec<EntityRef> = (0..60)
            .map(|i| entity_ref(&format!("GB-COH:{i:04}")))
            .collect();
        let csv = seed_ids_csv(&refs);
        assert_eq!(csv.split(',').count(), MAX_SEED_IDS_IN_URL);
    }

    #[test]
    fn seed_ids_csv_percent_encodes_each_id_so_a_colon_never_collides_with_the_join_separator() {
        let refs = vec![entity_ref("GB-COH:99000001")];
        let csv = seed_ids_csv(&refs);
        assert!(!csv.contains(':'), "got: {csv}");
    }

    #[test]
    fn multi_seed_neighbourhood_url_names_the_multi_seed_endpoint_and_declared_hops() {
        let refs = vec![entity_ref("location:planted-l2")];
        let url = multi_seed_neighbourhood_url(&refs, caps::DEFAULT_HOPS_STAR);
        assert!(
            url.starts_with("/api/entity/neighbourhood?ids="),
            "got: {url}"
        );
        assert!(
            url.contains(&format!("hops={}", caps::DEFAULT_HOPS_STAR)),
            "got: {url}"
        );
        assert!(
            url.contains(&format!(
                "node_cap={}",
                caps::DEFAULT_NEIGHBOURHOOD_NODE_CAP
            )),
            "got: {url}"
        );
    }

    // ─── default_hops_for ───────────────────────────────────────────────────────────────────

    #[test]
    fn default_hops_for_matches_d13_for_both_graph_kinds() {
        assert_eq!(
            default_hops_for(ThumbnailKind::Star),
            Some(caps::DEFAULT_HOPS_STAR)
        );
        assert_eq!(
            default_hops_for(ThumbnailKind::TypedEdge),
            Some(caps::DEFAULT_HOPS_TYPED_EDGE)
        );
        assert_eq!(default_hops_for(ThumbnailKind::RankedBars), None);
        assert_eq!(default_hops_for(ThumbnailKind::Chips), None);
        assert_eq!(default_hops_for(ThumbnailKind::Absence), None);
        assert_eq!(default_hops_for(ThumbnailKind::Unknown), None);
    }

    // ─── expansion_ids ──────────────────────────────────────────────────────────────────────

    #[test]
    fn expansion_ids_names_every_seed_and_every_returned_node() {
        let view = NeighbourhoodView::cut(
            vec![entity_ref("location:planted-l2")],
            1,
            250,
            vec![urban_integrity_workbench_dto::NeighbourhoodNodeView {
                entity: entity_ref("GB-COH:PL000201"),
                hop: 1,
                edges: vec![],
                reached_from: "location:planted-l2".to_string(),
            }],
        );
        let ids = expansion_ids(&view);
        assert_eq!(ids, vec!["location:planted-l2", "GB-COH:PL000201"]);
    }

    // ─── detail_stage (D-FR-03's Motif/Canvas state machine) ───────────────────────────────

    fn sample_algorithm_result() -> AlgorithmResult {
        AlgorithmResult {
            outcome: urban_integrity_workbench_dto::AlgorithmOutcome::Community {
                communities: vec![],
                hidden_node_ids: vec![],
            },
            population_generated_at: "2026-01-01T00:00:00Z".to_string(),
            excluded_edge_types: vec![],
            forced_included_edge_type: None,
            seed_used: Some(42),
            caps_used: urban_integrity_workbench_dto::AlgorithmCaps {
                hop_cap: None,
                result_cap: None,
                budget_secs: None,
                k: None,
                top_n: None,
                min_community_size: Some(5),
            },
            truncation: urban_integrity_workbench_dto::Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            },
            wall_clock_ms: 12,
        }
    }

    /// PROVE THIS CAN FAIL: swap the match arms (`Some` => `Motif`, `None` => `Canvas`) and this
    /// fails — confirming the assertion is load-bearing on which state maps to which, not merely
    /// "returns a `DetailStage`".
    #[test]
    fn detail_stage_is_a_pure_function_of_whether_a_result_exists() {
        assert_eq!(detail_stage(None), DetailStage::Motif);
        let result = sample_algorithm_result();
        assert_eq!(detail_stage(Some(&result)), DetailStage::Canvas);
    }

    // ─── neighbourhood_canvas_rows ──────────────────────────────────────────────────────────

    fn edge(edge_type: &str) -> urban_integrity_workbench_dto::neighbourhood::NeighbourEdgeView {
        urban_integrity_workbench_dto::neighbourhood::NeighbourEdgeView {
            edge_type: edge_type.to_string(),
            temporal: urban_integrity_workbench_dto::Temporal::unfiltered(
                urban_integrity_workbench_dto::EffectiveRange::Unrecorded {
                    basis: format!("{edge_type}.meta"),
                },
            ),
        }
    }

    #[test]
    fn neighbourhood_canvas_rows_returns_seeds_then_nodes_with_one_link_per_edge() {
        let view = NeighbourhoodView::cut(
            vec![
                entity_ref("location:planted-l2"),
                entity_ref("director:jane-smith"),
            ],
            1,
            250,
            vec![urban_integrity_workbench_dto::NeighbourhoodNodeView {
                entity: entity_ref("GB-COH:PL000201"),
                hop: 1,
                edges: vec![edge("Controls"), edge("RegisteredAt")],
                reached_from: "director:jane-smith".to_string(),
            }],
        );
        let (points, links) = neighbourhood_canvas_rows(&view);

        assert_eq!(points.len(), 3, "2 seeds + 1 node");
        assert_eq!(points[0].id, "location:planted-l2");
        assert_eq!(points[1].id, "director:jane-smith");
        assert_eq!(points[2].id, "GB-COH:PL000201");

        assert_eq!(links.len(), 2, "one link per edge on the one returned node");
        for link in &links {
            assert_eq!(link.source, "director:jane-smith");
            assert_eq!(link.target, "GB-COH:PL000201");
        }
        assert_eq!(links[0].edge_type, "Controls");
        assert_eq!(links[1].edge_type, "RegisteredAt");
    }

    /// PROVE THIS CAN FAIL: a seeds-only view (every real edge filtered by a hub exclusion) must
    /// never panic and must still report every seed as a point.
    #[test]
    fn neighbourhood_canvas_rows_on_a_seeds_only_view_returns_seed_points_and_no_links() {
        let view = NeighbourhoodView::cut(
            vec![
                entity_ref("location:planted-l2"),
                entity_ref("director:jane-smith"),
            ],
            1,
            250,
            vec![],
        );
        let (points, links) = neighbourhood_canvas_rows(&view);
        assert_eq!(points.len(), 2);
        assert!(links.is_empty());
    }
}
